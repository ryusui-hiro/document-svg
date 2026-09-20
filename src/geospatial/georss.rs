//! Bounded RSS/Atom GeoRSS Simple feed previews.

use std::path::Path;

use serde_json::{Value, json};

use crate::convert::{ConvertOptions, PageConsumer, read_limited_file};
use crate::error::{Error, Result};
use crate::geospatial::xml_tree::{XmlElement, XmlLimits, parse_xml_tree};

const GEORSS_NAMESPACE: &str = "http://www.georss.org/georss";
const ATOM_NAMESPACE: &str = "http://www.w3.org/2005/Atom";
const RDF_NAMESPACE: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#";
const MAX_GEORSS_INPUT_BYTES: u64 = 32 * 1024 * 1024;
const MAX_GEORSS_XML_NODES: usize = 200_000;
const MAX_GEORSS_XML_EVENTS: usize = 1_000_000;
const MAX_GEORSS_XML_DEPTH: usize = 64;
const MAX_GEORSS_TEXT_BYTES: usize = 32 * 1024 * 1024;
const MAX_GEORSS_FEATURES: usize = 100_000;
const MAX_GEORSS_POSITIONS: usize = 500_000;

#[derive(Default)]
struct GeoRssState {
    features: usize,
    positions: usize,
    metadata_ignored: bool,
    external_links: bool,
    unsupported_geometry: bool,
    unsupported_extensions: bool,
    extra_dimensions_ignored: bool,
}

pub(crate) fn looks_like_georss_prefix(bytes: &[u8]) -> bool {
    let has_georss_namespace = bytes
        .windows(GEORSS_NAMESPACE.len())
        .any(|window| window.eq_ignore_ascii_case(GEORSS_NAMESPACE.as_bytes()));
    if !has_georss_namespace {
        return false;
    }
    let text = String::from_utf8_lossy(bytes).to_ascii_lowercase();
    let has_geometry = [
        ":point", ":line", ":polygon", ":box", "<point", "<line", "<polygon", "<box",
    ]
    .iter()
    .any(|needle| text.contains(needle));
    if !has_geometry {
        return false;
    }
    [
        b"rss".as_slice(),
        b"feed".as_slice(),
        b"RDF".as_slice(),
        b"point".as_slice(),
        b"line".as_slice(),
        b"polygon".as_slice(),
        b"box".as_slice(),
    ]
    .iter()
    .any(|name| crate::geospatial::xml_tree::looks_like_root(bytes, name, None))
}

pub(crate) fn convert(
    path: &Path,
    options: &ConvertOptions,
    sink: &mut dyn PageConsumer,
) -> Result<Vec<String>> {
    let bytes = read_limited_file(
        path,
        options.max_input_bytes.min(MAX_GEORSS_INPUT_BYTES),
        "GeoRSS input",
    )?;
    let root = parse_xml_tree(
        &bytes,
        &XmlLimits {
            max_events: options.max_xml_events.min(MAX_GEORSS_XML_EVENTS),
            max_nodes: MAX_GEORSS_XML_NODES,
            max_depth: MAX_GEORSS_XML_DEPTH,
            max_text_bytes: MAX_GEORSS_TEXT_BYTES,
        },
        "GeoRSS",
    )?;
    let valid_root = match root.name.as_str() {
        "rss" => root.namespace.is_none(),
        "feed" => root.namespace.as_deref() == Some(ATOM_NAMESPACE),
        "RDF" => root.namespace.as_deref() == Some(RDF_NAMESPACE),
        "point" | "line" | "polygon" | "box" => root.namespace.as_deref() == Some(GEORSS_NAMESPACE),
        _ => false,
    };
    if !valid_root {
        return Err(Error::InvalidInput(
            "GeoRSS root must be RSS, Atom, RDF, or a GeoRSS Simple geometry".into(),
        ));
    }
    let mut state = GeoRssState::default();
    let mut features = Vec::new();
    collect_georss(&root, &mut features, &mut state, 0)?;
    if features.is_empty() {
        return Err(Error::InvalidInput(
            "feed contains no supported GeoRSS Simple geometry".into(),
        ));
    }
    let mut warnings = vec![
        "GeoRSS Simple coordinates are latitude/longitude in WGS 84 and are projected with Web Mercator".into(),
    ];
    if state.metadata_ignored {
        warnings.push("feed titles, descriptions, timestamps, and other non-geographic properties are not displayed".into());
    }
    if state.external_links {
        warnings.push("RSS/Atom linked resources and enclosures are not fetched".into());
    }
    if state.unsupported_geometry {
        warnings.push("GeoRSS curved or unsupported geometries such as circle were omitted".into());
    }
    if state.unsupported_extensions {
        warnings.push("unsupported GeoRSS extension elements were omitted".into());
    }
    if state.extra_dimensions_ignored {
        warnings.push("GeoRSS GML extra coordinate ordinates were validated and omitted".into());
    }
    let root = json!({ "type": "FeatureCollection", "features": features });
    crate::geospatial::geojson::convert_value(
        &root,
        "georss",
        "GeoRSS Feed Map Preview",
        "GeoRSS",
        warnings,
        sink,
    )
}

fn collect_georss(
    node: &XmlElement,
    features: &mut Vec<Value>,
    state: &mut GeoRssState,
    depth: usize,
) -> Result<()> {
    if depth > MAX_GEORSS_XML_DEPTH {
        return Err(Error::LimitExceeded(
            "GeoRSS feed nesting limit exceeded".into(),
        ));
    }
    if node.namespace.as_deref() == Some(GEORSS_NAMESPACE) {
        match node.name.as_str() {
            "point" => {
                let values = parse_pair_list(&node.text, state)?;
                if values.len() != 1 {
                    return Err(Error::InvalidInput(
                        "GeoRSS point must contain one latitude/longitude pair".into(),
                    ));
                }
                push_feature(
                    json!({ "type": "Point", "coordinates": values[0] }),
                    features,
                    state,
                )?;
                return Ok(());
            }
            "line" => {
                let positions = parse_pair_list(&node.text, state)?;
                if positions.len() < 2 {
                    return Err(Error::InvalidInput(
                        "GeoRSS line requires at least two positions".into(),
                    ));
                }
                push_feature(
                    json!({ "type": "LineString", "coordinates": positions }),
                    features,
                    state,
                )?;
                return Ok(());
            }
            "polygon" => {
                let ring = parse_pair_list(&node.text, state)?;
                if ring.len() < 3 {
                    return Err(Error::InvalidInput(
                        "GeoRSS polygon requires at least three positions".into(),
                    ));
                }
                push_feature(
                    json!({ "type": "Polygon", "coordinates": [ring] }),
                    features,
                    state,
                )?;
                return Ok(());
            }
            "box" => {
                let mut values = coordinate_tokens(&node.text);
                let south = values.next().map(parse_coordinate).transpose()?;
                let west = values.next().map(parse_coordinate).transpose()?;
                let north = values.next().map(parse_coordinate).transpose()?;
                let east = values.next().map(parse_coordinate).transpose()?;
                if south.is_none()
                    || west.is_none()
                    || north.is_none()
                    || east.is_none()
                    || values.next().is_some()
                {
                    return Err(Error::InvalidInput(
                        "GeoRSS box must contain south, west, north, and east".into(),
                    ));
                }
                let south = south.expect("validated GeoRSS box south");
                let west = west.expect("validated GeoRSS box west");
                let north = north.expect("validated GeoRSS box north");
                let east = east.expect("validated GeoRSS box east");
                validate_coordinate(south, west)?;
                validate_coordinate(north, east)?;
                if south > north {
                    return Err(Error::InvalidInput(
                        "GeoRSS box south latitude exceeds north latitude".into(),
                    ));
                }
                for _ in 0..5 {
                    increment_position(state)?;
                }
                let ring = vec![
                    json!([west, south]),
                    json!([east, south]),
                    json!([east, north]),
                    json!([west, north]),
                    json!([west, south]),
                ];
                push_feature(
                    json!({ "type": "Polygon", "coordinates": [ring] }),
                    features,
                    state,
                )?;
                return Ok(());
            }
            "where" => {
                let fragment = crate::geospatial::gml::parse_georss_gml_fragment(node)?;
                state.positions = state.positions.saturating_add(fragment.positions);
                if state.positions > MAX_GEORSS_POSITIONS {
                    return Err(Error::LimitExceeded(format!(
                        "GeoRSS exceeds {MAX_GEORSS_POSITIONS} positions"
                    )));
                }
                state.extra_dimensions_ignored |= fragment.extra_dimensions_ignored;
                state.unsupported_geometry |= fragment.unsupported_geometry;
                state.unsupported_extensions |= fragment.unsupported_extensions;
                if fragment.geometries.is_empty() {
                    state.unsupported_geometry = true;
                }
                for geometry in fragment.geometries {
                    push_feature(geometry, features, state)?;
                }
                return Ok(());
            }
            "circle" => {
                state.unsupported_geometry = true;
                return Ok(());
            }
            "elev" | "floor" | "featuretypetag" | "relationshiptag" => {
                state.metadata_ignored = true;
                return Ok(());
            }
            _ => state.metadata_ignored = true,
        }
    } else if matches!(
        node.name.as_str(),
        "title" | "description" | "updated" | "published" | "author"
    ) {
        state.metadata_ignored = true;
    }
    if node.name == "link" || node.name == "enclosure" || node.attributes.contains_key("href") {
        state.external_links = true;
    }
    for child in &node.children {
        collect_georss(child, features, state, depth + 1)?;
    }
    Ok(())
}

fn parse_pair_list(text: &str, state: &mut GeoRssState) -> Result<Vec<Value>> {
    let mut positions = Vec::new();
    let mut values = coordinate_tokens(text);
    while let Some(latitude_text) = values.next() {
        let longitude_text = values.next().ok_or_else(|| {
            Error::InvalidInput(
                "GeoRSS Simple geometry must contain latitude/longitude pairs".into(),
            )
        })?;
        let latitude = parse_coordinate(latitude_text)?;
        let longitude = parse_coordinate(longitude_text)?;
        validate_coordinate(latitude, longitude)?;
        increment_position(state)?;
        positions.push(json!([longitude, latitude]));
    }
    Ok(positions)
}

fn coordinate_tokens(text: &str) -> impl Iterator<Item = &str> {
    text.split(|character: char| character.is_whitespace() || character == ',')
        .filter(|token| !token.is_empty())
}

fn parse_coordinate(text: &str) -> Result<f64> {
    let value = text
        .parse::<f64>()
        .map_err(|_| Error::InvalidInput("GeoRSS coordinate is not numeric".into()))?;
    if !value.is_finite() {
        return Err(Error::InvalidInput(
            "GeoRSS coordinate is not finite".into(),
        ));
    }
    Ok(value)
}

fn validate_coordinate(latitude: f64, longitude: f64) -> Result<()> {
    if !(-90.0..=90.0).contains(&latitude) || !(-180.0..=180.0).contains(&longitude) {
        return Err(Error::InvalidInput(
            "GeoRSS coordinate is outside WGS 84 bounds".into(),
        ));
    }
    Ok(())
}

fn increment_position(state: &mut GeoRssState) -> Result<()> {
    state.positions = state.positions.saturating_add(1);
    if state.positions > MAX_GEORSS_POSITIONS {
        return Err(Error::LimitExceeded(format!(
            "GeoRSS exceeds {MAX_GEORSS_POSITIONS} positions"
        )));
    }
    Ok(())
}

fn push_feature(geometry: Value, features: &mut Vec<Value>, state: &mut GeoRssState) -> Result<()> {
    state.features = state.features.saturating_add(1);
    if state.features > MAX_GEORSS_FEATURES {
        return Err(Error::LimitExceeded(format!(
            "GeoRSS exceeds {MAX_GEORSS_FEATURES} geometries"
        )));
    }
    features.push(json!({ "type": "Feature", "geometry": geometry, "properties": null }));
    Ok(())
}
