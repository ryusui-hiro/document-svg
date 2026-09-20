//! Bounded GML feature geometry previews with explicit CRS-axis handling.

use std::path::Path;

use serde_json::{Value, json};

use crate::convert::{ConvertOptions, PageConsumer, read_limited_file};
use crate::error::{Error, Result};
use crate::geospatial::xml_tree::{XmlElement, XmlLimits, parse_xml_tree};

const GML_NAMESPACES: [&str; 2] = [
    "http://www.opengis.net/gml/3.2",
    "http://www.opengis.net/gml",
];
const MAX_GML_INPUT_BYTES: u64 = 32 * 1024 * 1024;
const MAX_GML_XML_NODES: usize = 200_000;
const MAX_GML_XML_EVENTS: usize = 1_000_000;
const MAX_GML_XML_DEPTH: usize = 64;
const MAX_GML_TEXT_BYTES: usize = 32 * 1024 * 1024;
const MAX_GML_GEOMETRIES: usize = 200_000;
const MAX_GML_POSITIONS: usize = 500_000;
const MAX_GML_GEOMETRY_DEPTH: usize = 32;

#[derive(Clone, Default)]
struct GmlContext {
    srs_name: Option<String>,
    dimension: Option<usize>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum AxisOrder {
    LongitudeLatitude,
    LatitudeLongitude,
}

#[derive(Default)]
struct GmlState {
    geometries: usize,
    positions: usize,
    crs_assumed: bool,
    extra_dimensions_ignored: bool,
    feature_properties_ignored: bool,
    links_present: bool,
    unsupported_geometry: bool,
    unsupported_extensions: bool,
}

pub(crate) struct GeoRssGmlFragment {
    pub geometries: Vec<Value>,
    pub positions: usize,
    pub extra_dimensions_ignored: bool,
    pub unsupported_geometry: bool,
    pub unsupported_extensions: bool,
}

pub(crate) fn looks_like_gml_prefix(bytes: &[u8]) -> bool {
    let is_gml_root = [
        b"FeatureCollection".as_slice(),
        b"Point".as_slice(),
        b"LineString".as_slice(),
        b"LinearRing".as_slice(),
        b"Polygon".as_slice(),
        b"MultiPoint".as_slice(),
        b"MultiLineString".as_slice(),
        b"MultiCurve".as_slice(),
        b"MultiPolygon".as_slice(),
        b"MultiSurface".as_slice(),
        b"MultiGeometry".as_slice(),
    ]
    .iter()
    .any(|name| {
        GML_NAMESPACES.iter().any(|namespace| {
            crate::geospatial::xml_tree::looks_like_root(bytes, name, Some(namespace.as_bytes()))
        })
    });
    if is_gml_root {
        return true;
    }
    let text = String::from_utf8_lossy(bytes).to_ascii_lowercase();
    text.contains("featurecollection")
        && (text.contains(GML_NAMESPACES[0]) || text.contains(GML_NAMESPACES[1]))
        && crate::geospatial::xml_tree::looks_like_root(bytes, b"FeatureCollection", None)
}

pub(crate) fn convert(
    path: &Path,
    options: &ConvertOptions,
    sink: &mut dyn PageConsumer,
) -> Result<Vec<String>> {
    let bytes = read_limited_file(
        path,
        options.max_input_bytes.min(MAX_GML_INPUT_BYTES),
        "GML input",
    )?;
    let root = parse_xml_tree(
        &bytes,
        &XmlLimits {
            max_events: options.max_xml_events.min(MAX_GML_XML_EVENTS),
            max_nodes: MAX_GML_XML_NODES,
            max_depth: MAX_GML_XML_DEPTH,
            max_text_bytes: MAX_GML_TEXT_BYTES,
        },
        "GML",
    )?;
    let mut state = GmlState::default();
    let mut features = Vec::new();
    let context = GmlContext::default();
    collect_features(&root, &context, &mut features, &mut state, 0)?;
    if features.is_empty() {
        return Err(Error::InvalidInput(
            "GML document contains no supported linear geometry".into(),
        ));
    }
    inspect_ignored_data(&root, &mut state);
    let mut warnings = Vec::new();
    warnings.push(
        "GML geometry is projected with Web Mercator; coordinates outside WGS 84 are rejected and no reprojection is attempted".into(),
    );
    if state.crs_assumed {
        warnings.push(
            "GML has no recognized srsName; coordinates are assumed to use CRS84 longitude/latitude".into(),
        );
    }
    if state.extra_dimensions_ignored {
        warnings
            .push("GML third and higher coordinate ordinates were validated and omitted".into());
    }
    if state.feature_properties_ignored {
        warnings.push("GML feature properties and identifiers are not displayed".into());
    }
    if state.links_present {
        warnings.push("GML XLink and other linked resources are not fetched".into());
    }
    if state.unsupported_geometry {
        warnings.push(
            "GML curved, surface, solid, or other unsupported geometry members were omitted".into(),
        );
    }
    if state.unsupported_extensions {
        warnings.push(
            "GML application-schema extension elements without supported geometry were omitted"
                .into(),
        );
    }
    let root = json!({ "type": "FeatureCollection", "features": features });
    crate::geospatial::geojson::convert_value(
        &root,
        "gml",
        "GML Map Preview",
        "GML",
        warnings,
        sink,
    )
}

pub(crate) fn parse_georss_gml_fragment(node: &XmlElement) -> Result<GeoRssGmlFragment> {
    let default = GmlContext {
        // GeoRSS's default CRS is EPSG:4326 with latitude/longitude axis order.
        srs_name: Some("EPSG:4326".into()),
        dimension: None,
    };
    let mut state = GmlState::default();
    let mut geometries = Vec::new();
    collect_georss_gml_members(node, &default, &mut geometries, &mut state, 0)?;
    Ok(GeoRssGmlFragment {
        geometries,
        positions: state.positions,
        extra_dimensions_ignored: state.extra_dimensions_ignored,
        unsupported_geometry: state.unsupported_geometry,
        unsupported_extensions: state.unsupported_extensions,
    })
}

fn collect_georss_gml_members(
    node: &XmlElement,
    inherited: &GmlContext,
    geometries: &mut Vec<Value>,
    state: &mut GmlState,
    depth: usize,
) -> Result<()> {
    if depth > MAX_GML_GEOMETRY_DEPTH {
        return Err(Error::LimitExceeded(
            "GeoRSS GML geometry nesting limit exceeded".into(),
        ));
    }
    let context = context_for(node, inherited)?;
    for child in &node.children {
        if !is_gml(child) {
            state.unsupported_extensions = true;
            continue;
        }
        let child_context = context_for(child, &context)?;
        let parsed = match child.name.as_str() {
            "Envelope" | "Box" => parse_georss_envelope(child, &child_context, state)?,
            _ if geometry_element(child) => parse_geometry(child, &context, state, depth + 1)?,
            _ => {
                collect_georss_gml_members(child, &context, geometries, state, depth + 1)?;
                None
            }
        };
        if let Some(geometry) = parsed {
            geometries.push(geometry);
        }
    }
    Ok(())
}

fn parse_georss_envelope(
    node: &XmlElement,
    context: &GmlContext,
    state: &mut GmlState,
) -> Result<Option<Value>> {
    let positions = collect_positions(node, context, state)?;
    if positions.len() != 2 {
        return Err(Error::InvalidInput(
            "GeoRSS GML Envelope/Box must contain lower and upper corner positions".into(),
        ));
    }
    let lower = positions[0]
        .as_array()
        .filter(|position| position.len() == 2)
        .ok_or_else(|| Error::InvalidInput("GeoRSS GML lower corner is invalid".into()))?;
    let upper = positions[1]
        .as_array()
        .filter(|position| position.len() == 2)
        .ok_or_else(|| Error::InvalidInput("GeoRSS GML upper corner is invalid".into()))?;
    let west = lower[0]
        .as_f64()
        .ok_or_else(|| Error::InvalidInput("GeoRSS GML lower longitude is invalid".into()))?;
    let south = lower[1]
        .as_f64()
        .ok_or_else(|| Error::InvalidInput("GeoRSS GML lower latitude is invalid".into()))?;
    let east = upper[0]
        .as_f64()
        .ok_or_else(|| Error::InvalidInput("GeoRSS GML upper longitude is invalid".into()))?;
    let north = upper[1]
        .as_f64()
        .ok_or_else(|| Error::InvalidInput("GeoRSS GML upper latitude is invalid".into()))?;
    if south > north {
        return Err(Error::InvalidInput(
            "GeoRSS GML envelope lower latitude exceeds upper latitude".into(),
        ));
    }
    state.positions = state.positions.saturating_add(3);
    if state.positions > MAX_GML_POSITIONS {
        return Err(Error::LimitExceeded(format!(
            "GML exceeds {MAX_GML_POSITIONS} positions"
        )));
    }
    let ring = vec![
        json!([west, south]),
        json!([east, south]),
        json!([east, north]),
        json!([west, north]),
        json!([west, south]),
    ];
    Ok(Some(json!({ "type": "Polygon", "coordinates": [ring] })))
}

fn is_gml(node: &XmlElement) -> bool {
    node.namespace
        .as_deref()
        .is_some_and(|namespace| GML_NAMESPACES.contains(&namespace))
}

fn context_for(node: &XmlElement, inherited: &GmlContext) -> Result<GmlContext> {
    let mut context = inherited.clone();
    if let Some(srs_name) = node.attribute("srsName") {
        context.srs_name = Some(srs_name.trim().to_owned());
    }
    if let Some(dimension) = node.attribute("srsDimension") {
        let dimension = dimension.parse::<usize>().map_err(|_| {
            Error::InvalidInput("GML srsDimension must be a positive integer".into())
        })?;
        if !(2..=4).contains(&dimension) {
            return Err(Error::Unsupported(format!(
                "GML coordinate dimension {dimension} is unsupported"
            )));
        }
        context.dimension = Some(dimension);
    }
    if node.attribute("axisLabels").is_some() || node.attribute("uomLabels").is_some() {
        return Err(Error::Unsupported(
            "GML axisLabels/uomLabels are not supported; use a recognized srsName alone".into(),
        ));
    }
    Ok(context)
}

fn axis_order(context: &GmlContext, state: &mut GmlState) -> Result<AxisOrder> {
    let Some(srs_name) = context.srs_name.as_deref() else {
        state.crs_assumed = true;
        return Ok(AxisOrder::LongitudeLatitude);
    };
    let normalized = srs_name.trim().to_ascii_lowercase();
    if normalized.contains("crs84") || normalized == "crs:84" {
        return Ok(AxisOrder::LongitudeLatitude);
    }
    if normalized == "epsg:4326"
        || normalized.ends_with("/4326") && normalized.contains("epsg")
        || normalized.ends_with("#4326") && normalized.contains("epsg")
        || normalized.ends_with("::4326") && normalized.contains("epsg")
        || normalized.ends_with(":4326") && normalized.contains("epsg")
    {
        return Ok(AxisOrder::LatitudeLongitude);
    }
    if normalized == "epsg:4979"
        || normalized.ends_with("/4979") && normalized.contains("epsg")
        || normalized.ends_with("#4979") && normalized.contains("epsg")
        || normalized.ends_with("::4979") && normalized.contains("epsg")
        || normalized.ends_with(":4979") && normalized.contains("epsg")
    {
        return Ok(AxisOrder::LatitudeLongitude);
    }
    Err(Error::Unsupported(format!(
        "GML CRS '{srs_name}' cannot be projected without a coordinate transformation"
    )))
}

fn geometry_element(node: &XmlElement) -> bool {
    is_gml(node)
        && matches!(
            node.name.as_str(),
            "Point"
                | "LineString"
                | "LinearRing"
                | "Polygon"
                | "MultiPoint"
                | "MultiLineString"
                | "MultiCurve"
                | "MultiPolygon"
                | "MultiSurface"
                | "MultiGeometry"
                | "Curve"
                | "Surface"
                | "Solid"
                | "Tin"
                | "TriangulatedSurface"
        )
}

fn collect_features(
    node: &XmlElement,
    inherited: &GmlContext,
    features: &mut Vec<Value>,
    state: &mut GmlState,
    depth: usize,
) -> Result<()> {
    if depth > MAX_GML_XML_DEPTH {
        return Err(Error::LimitExceeded(
            "GML feature nesting limit exceeded".into(),
        ));
    }
    let context = context_for(node, inherited)?;
    if geometry_element(node) {
        if let Some(geometry) = parse_geometry(node, &context, state, 0)? {
            features.push(json!({
                "type": "Feature",
                "geometry": geometry,
                "properties": null
            }));
            if features.len() > MAX_GML_GEOMETRIES {
                return Err(Error::LimitExceeded(format!(
                    "GML contains more than {MAX_GML_GEOMETRIES} top-level geometries"
                )));
            }
        }
        return Ok(());
    }
    for child in &node.children {
        collect_features(child, &context, features, state, depth + 1)?;
    }
    Ok(())
}

fn parse_geometry(
    node: &XmlElement,
    inherited: &GmlContext,
    state: &mut GmlState,
    depth: usize,
) -> Result<Option<Value>> {
    if depth > MAX_GML_GEOMETRY_DEPTH {
        return Err(Error::LimitExceeded(
            "GML geometry nesting limit exceeded".into(),
        ));
    }
    state.geometries = state.geometries.saturating_add(1);
    if state.geometries > MAX_GML_GEOMETRIES {
        return Err(Error::LimitExceeded(format!(
            "GML contains more than {MAX_GML_GEOMETRIES} geometries"
        )));
    }
    let context = context_for(node, inherited)?;
    match node.name.as_str() {
        "Point" => {
            let positions = collect_positions(node, &context, state)?;
            if positions.len() != 1 {
                return Err(Error::InvalidInput(
                    "GML Point must contain exactly one position".into(),
                ));
            }
            Ok(Some(
                json!({ "type": "Point", "coordinates": positions[0] }),
            ))
        }
        "LineString" | "LinearRing" => {
            let positions = collect_positions(node, &context, state)?;
            if positions.len() < 2 {
                return Err(Error::InvalidInput(
                    "GML line geometry must contain at least two positions".into(),
                ));
            }
            Ok(Some(
                json!({ "type": "LineString", "coordinates": positions }),
            ))
        }
        "Polygon" => {
            let mut rings = Vec::new();
            collect_rings(node, &context, &mut rings, state, depth + 1)?;
            if rings.is_empty() {
                state.unsupported_geometry = true;
                Ok(None)
            } else {
                Ok(Some(json!({ "type": "Polygon", "coordinates": rings })))
            }
        }
        "MultiPoint" | "MultiLineString" | "MultiCurve" | "MultiPolygon" | "MultiSurface"
        | "MultiGeometry" => {
            let mut geometries = Vec::new();
            collect_nested_geometries(node, &context, &mut geometries, state, depth + 1)?;
            if geometries.is_empty() {
                Ok(None)
            } else {
                Ok(Some(
                    json!({ "type": "GeometryCollection", "geometries": geometries }),
                ))
            }
        }
        "Curve" | "Surface" | "Solid" | "Tin" | "TriangulatedSurface" => {
            state.unsupported_geometry = true;
            Ok(None)
        }
        _ => Ok(None),
    }
}

fn collect_nested_geometries(
    node: &XmlElement,
    inherited: &GmlContext,
    geometries: &mut Vec<Value>,
    state: &mut GmlState,
    depth: usize,
) -> Result<()> {
    let context = context_for(node, inherited)?;
    for child in &node.children {
        if geometry_element(child) {
            if let Some(geometry) = parse_geometry(child, &context, state, depth + 1)? {
                geometries.push(geometry);
            }
        } else if is_gml(child) {
            collect_nested_geometries(child, &context, geometries, state, depth + 1)?;
        } else {
            state.unsupported_extensions = true;
        }
    }
    Ok(())
}

fn collect_rings(
    node: &XmlElement,
    inherited: &GmlContext,
    rings: &mut Vec<Vec<Value>>,
    state: &mut GmlState,
    depth: usize,
) -> Result<()> {
    if depth > MAX_GML_GEOMETRY_DEPTH {
        return Err(Error::LimitExceeded(
            "GML polygon boundary nesting limit exceeded".into(),
        ));
    }
    let context = context_for(node, inherited)?;
    for child in &node.children {
        if !is_gml(child) {
            state.unsupported_extensions = true;
            continue;
        }
        if geometry_element(child) && child.name != "LinearRing" {
            state.unsupported_geometry = true;
            continue;
        }
        if child.name == "LinearRing" {
            let context = context_for(child, &context)?;
            let positions = collect_positions(child, &context, state)?;
            if positions.len() >= 3 {
                rings.push(positions);
            } else {
                state.unsupported_geometry = true;
            }
        } else {
            collect_rings(child, &context, rings, state, depth + 1)?;
        }
    }
    Ok(())
}

fn collect_positions(
    node: &XmlElement,
    inherited: &GmlContext,
    state: &mut GmlState,
) -> Result<Vec<Value>> {
    let mut sources = Vec::new();
    collect_position_sources(node, inherited, &mut sources, state)?;
    if sources.is_empty() {
        return Err(Error::InvalidInput(format!(
            "GML {} has no supported pos/posList/coordinates element",
            node.name
        )));
    }
    let mut positions = Vec::new();
    for (source, context) in sources {
        let order = axis_order(&context, state)?;
        let dimension = context.dimension.unwrap_or_else(|| {
            context
                .srs_name
                .as_deref()
                .map(default_dimension)
                .unwrap_or(2)
        });
        if !(2..=4).contains(&dimension) {
            return Err(Error::Unsupported(format!(
                "GML coordinate dimension {dimension} is unsupported"
            )));
        }
        state.extra_dimensions_ignored |= dimension > 2;
        let parsed = match source.name.as_str() {
            "pos" | "lowerCorner" | "upperCorner" => {
                let parsed = parse_pos_list(&source.text, dimension, order, state)?;
                if parsed.len() != 1 {
                    return Err(Error::InvalidInput(format!(
                        "GML pos contains {} positions; expected exactly one",
                        parsed.len()
                    )));
                }
                parsed
            }
            "posList" => {
                let parsed = parse_pos_list(&source.text, dimension, order, state)?;
                if let Some(expected) = source.attribute("count") {
                    let expected = expected.parse::<usize>().map_err(|_| {
                        Error::InvalidInput("GML posList count must be an integer".into())
                    })?;
                    if expected != parsed.len() {
                        return Err(Error::InvalidInput(
                            "GML posList count does not match its coordinate data".into(),
                        ));
                    }
                }
                parsed
            }
            "coordinates" => {
                check_coordinate_separators(source)?;
                let mut parsed = Vec::new();
                for tuple in source.text.split_whitespace() {
                    let mut ordinates = [0.0; 4];
                    let mut ordinate_count = 0usize;
                    for value in tuple.split(',') {
                        if ordinate_count >= dimension {
                            return Err(Error::InvalidInput(format!(
                                "GML coordinates tuple has more than srsDimension {dimension} ordinates"
                            )));
                        }
                        ordinates[ordinate_count] = parse_number(value.trim())?;
                        ordinate_count += 1;
                    }
                    if ordinate_count != dimension {
                        return Err(Error::InvalidInput(format!(
                            "GML coordinates tuple has {} ordinates but srsDimension is {dimension}",
                            ordinate_count
                        )));
                    }
                    parsed.push(make_position(&ordinates[..ordinate_count], order, state)?);
                }
                parsed
            }
            _ => continue,
        };
        positions.extend(parsed);
    }
    Ok(positions)
}

fn collect_position_sources<'a>(
    node: &'a XmlElement,
    inherited: &GmlContext,
    sources: &mut Vec<(&'a XmlElement, GmlContext)>,
    state: &mut GmlState,
) -> Result<()> {
    let context = context_for(node, inherited)?;
    for child in &node.children {
        if is_gml(child)
            && matches!(
                child.name.as_str(),
                "pos" | "posList" | "coordinates" | "lowerCorner" | "upperCorner"
            )
        {
            sources.push((child, context_for(child, &context)?));
        } else if is_gml(child) && !geometry_element(child) {
            collect_position_sources(child, &context, sources, state)?;
        } else if !is_gml(child) {
            state.unsupported_extensions = true;
        }
    }
    Ok(())
}

fn parse_pos_list(
    text: &str,
    dimension: usize,
    order: AxisOrder,
    state: &mut GmlState,
) -> Result<Vec<Value>> {
    let mut positions = Vec::new();
    let mut tuple = Vec::with_capacity(dimension);
    for ordinate in text.split_whitespace() {
        tuple.push(parse_number(ordinate)?);
        if tuple.len() == dimension {
            positions.push(make_position(&tuple, order, state)?);
            tuple.clear();
        }
    }
    if !tuple.is_empty() {
        return Err(Error::InvalidInput(format!(
            "GML posList ordinate count is not divisible by srsDimension {dimension}"
        )));
    }
    Ok(positions)
}

fn parse_number(text: &str) -> Result<f64> {
    let value = text
        .parse::<f64>()
        .map_err(|_| Error::InvalidInput("GML coordinate ordinate is not numeric".into()))?;
    if value.is_finite() {
        Ok(value)
    } else {
        Err(Error::InvalidInput(
            "GML coordinate ordinate is not finite".into(),
        ))
    }
}

fn make_position(values: &[f64], order: AxisOrder, state: &mut GmlState) -> Result<Value> {
    let (longitude, latitude) = match order {
        AxisOrder::LongitudeLatitude => (values[0], values[1]),
        AxisOrder::LatitudeLongitude => (values[1], values[0]),
    };
    state.positions = state.positions.saturating_add(1);
    if state.positions > MAX_GML_POSITIONS {
        return Err(Error::LimitExceeded(format!(
            "GML exceeds {MAX_GML_POSITIONS} positions"
        )));
    }
    Ok(json!([longitude, latitude]))
}

fn default_dimension(srs_name: &str) -> usize {
    let normalized = srs_name.to_ascii_lowercase();
    if normalized.contains("4979") || normalized.contains("crs84h") {
        3
    } else {
        2
    }
}

fn check_coordinate_separators(node: &XmlElement) -> Result<()> {
    if node.attribute("decimal").is_some_and(|value| value != ".")
        || node.attribute("cs").is_some_and(|value| value != ",")
        || node
            .attribute("ts")
            .is_some_and(|value| !value.chars().all(char::is_whitespace))
    {
        return Err(Error::Unsupported(
            "GML coordinates with custom decimal/tuple/list separators are unsupported".into(),
        ));
    }
    Ok(())
}

fn inspect_ignored_data(node: &XmlElement, state: &mut GmlState) {
    if is_gml(node) {
        if matches!(
            node.name.as_str(),
            "name" | "description" | "identifier" | "metaDataProperty"
        ) {
            state.feature_properties_ignored = true;
        }
    } else {
        state.feature_properties_ignored = true;
    }
    if node
        .attributes
        .keys()
        .any(|name| name.ends_with(":href") || name == "href")
    {
        state.links_present = true;
    }
    for child in &node.children {
        inspect_ignored_data(child, state);
    }
}
