//! Bounded KML and KMZ map previews. External resources are never fetched.

use std::collections::HashSet;
use std::io::{Cursor, Read};
use std::path::Path;

use serde_json::{Value, json};
use zip::ZipArchive;

use crate::convert::{ConvertOptions, PageConsumer, read_limited_file};
use crate::error::{Error, Result};
use crate::geospatial::xml_tree::{XmlElement, XmlLimits, parse_xml_tree};

const MAX_KML_INPUT_BYTES: u64 = 32 * 1024 * 1024;
const KML_NAMESPACE: &str = "http://www.opengis.net/kml/2.2";
const GX_NAMESPACE: &str = "http://www.google.com/kml/ext/2.2";
const MAX_KMZ_INPUT_BYTES: u64 = 64 * 1024 * 1024;
const MAX_KMZ_ENTRY_BYTES: u64 = 32 * 1024 * 1024;
const MAX_KMZ_TOTAL_UNCOMPRESSED_BYTES: u64 = 128 * 1024 * 1024;
const MAX_KMZ_ENTRIES: usize = 10_000;
const MAX_KML_XML_NODES: usize = 200_000;
const MAX_KML_XML_EVENTS: usize = 1_000_000;
const MAX_KML_XML_DEPTH: usize = 64;
const MAX_KML_GEOMETRY_DEPTH: usize = 16;
const MAX_KML_TEXT_BYTES: usize = 32 * 1024 * 1024;
const MAX_KML_PLACEMARKS: usize = 100_000;
const MAX_KML_COORDINATES: usize = 500_000;

#[derive(Default)]
struct KmlState {
    placemarks: usize,
    coordinates: usize,
    altitude_ignored: bool,
    unsupported_geometry: bool,
    style_data_ignored: bool,
    external_links: bool,
    track_time_ignored: bool,
    incomplete_track_data: bool,
    unsupported_extensions: bool,
}

pub(crate) fn convert_kml(
    path: &Path,
    options: &ConvertOptions,
    sink: &mut dyn PageConsumer,
) -> Result<Vec<String>> {
    let bytes = read_limited_file(
        path,
        options.max_input_bytes.min(MAX_KML_INPUT_BYTES),
        "KML input",
    )?;
    convert_xml(
        &bytes,
        "kml",
        "KML Map Preview",
        options.max_xml_events.min(MAX_KML_XML_EVENTS),
        sink,
    )
}

pub(crate) fn convert_kmz(
    path: &Path,
    options: &ConvertOptions,
    sink: &mut dyn PageConsumer,
) -> Result<Vec<String>> {
    let bytes = read_limited_file(
        path,
        options.max_input_bytes.min(MAX_KMZ_INPUT_BYTES),
        "KMZ input",
    )?;
    let mut archive = ZipArchive::new(Cursor::new(bytes.as_slice()))
        .map_err(|error| Error::InvalidInput(format!("invalid KMZ archive: {error}")))?;
    if archive.len() > MAX_KMZ_ENTRIES {
        return Err(Error::LimitExceeded(format!(
            "KMZ archive contains {} entries; maximum is {MAX_KMZ_ENTRIES}",
            archive.len()
        )));
    }
    let mut names = HashSet::with_capacity(archive.len());
    let mut total_bytes = 0u64;
    let mut first_kml = None;
    let mut doc_kml = None;
    for index in 0..archive.len() {
        let entry = archive.by_index(index)?;
        let name = entry.name();
        if name.len() > 4096
            || name.starts_with('/')
            || name.contains('\\')
            || name.split('/').any(|part| part == "..")
        {
            return Err(Error::InvalidInput(
                "KMZ archive contains an unsafe part name".into(),
            ));
        }
        if !names.insert(name.to_owned()) {
            return Err(Error::InvalidInput(format!(
                "KMZ archive repeats entry '{name}'"
            )));
        }
        if entry.size() > MAX_KMZ_ENTRY_BYTES {
            return Err(Error::LimitExceeded(format!(
                "KMZ entry '{name}' is {} bytes; maximum is {MAX_KMZ_ENTRY_BYTES}",
                entry.size()
            )));
        }
        total_bytes = total_bytes.saturating_add(entry.size());
        if total_bytes > MAX_KMZ_TOTAL_UNCOMPRESSED_BYTES {
            return Err(Error::LimitExceeded(format!(
                "KMZ expands beyond {MAX_KMZ_TOTAL_UNCOMPRESSED_BYTES} bytes"
            )));
        }
        if !entry.is_dir() && name.eq_ignore_ascii_case("doc.kml") {
            doc_kml = Some(name.to_owned());
        }
        if !entry.is_dir() && first_kml.is_none() && name.to_ascii_lowercase().ends_with(".kml") {
            first_kml = Some(name.to_owned());
        }
    }
    let selected_doc_kml = doc_kml.is_some();
    let kml_name = doc_kml
        .or(first_kml)
        .ok_or_else(|| Error::InvalidInput("KMZ archive contains no KML document".into()))?;
    let kml_entry = archive.by_name(&kml_name)?;
    let expected = kml_entry.size();
    if expected > MAX_KML_INPUT_BYTES {
        return Err(Error::LimitExceeded(format!(
            "KMZ KML document exceeds {MAX_KML_INPUT_BYTES} bytes"
        )));
    }
    let mut xml = Vec::with_capacity(usize::try_from(expected).unwrap_or(0).min(8 * 1024 * 1024));
    kml_entry
        .take(MAX_KML_INPUT_BYTES.saturating_add(1))
        .read_to_end(&mut xml)?;
    if xml.len() as u64 != expected {
        return Err(Error::InvalidInput(
            "KMZ KML entry has an invalid expanded size".into(),
        ));
    }
    let mut warnings = vec![
        "KMZ reads only the selected KML document; embedded images/models and other archive resources are omitted".into(),
    ];
    if !selected_doc_kml {
        warnings.push("KMZ has no root doc.kml; the first KML entry was selected".into());
    }
    convert_xml_with_warnings(
        &xml,
        "kmz",
        "KMZ Map Preview",
        warnings,
        options.max_xml_events.min(MAX_KML_XML_EVENTS),
        sink,
    )
}

fn convert_xml(
    xml: &[u8],
    source_format: &str,
    title: &str,
    max_xml_events: usize,
    sink: &mut dyn PageConsumer,
) -> Result<Vec<String>> {
    convert_xml_with_warnings(xml, source_format, title, Vec::new(), max_xml_events, sink)
}

fn convert_xml_with_warnings(
    xml: &[u8],
    source_format: &str,
    title: &str,
    mut warnings: Vec<String>,
    max_xml_events: usize,
    sink: &mut dyn PageConsumer,
) -> Result<Vec<String>> {
    if xml.len() as u64 > MAX_KML_INPUT_BYTES {
        return Err(Error::LimitExceeded(format!(
            "KML document exceeds {MAX_KML_INPUT_BYTES} bytes"
        )));
    }
    let root = parse_xml_tree(
        xml,
        &XmlLimits {
            max_events: max_xml_events,
            max_nodes: MAX_KML_XML_NODES,
            max_depth: MAX_KML_XML_DEPTH,
            max_text_bytes: MAX_KML_TEXT_BYTES,
        },
        "KML",
    )?;
    if root.name != "kml" || root.namespace.as_deref() != Some(KML_NAMESPACE) {
        return Err(Error::InvalidInput(
            "XML root is not in the KML 2.2 namespace".into(),
        ));
    }
    let mut state = KmlState::default();
    inspect_metadata_and_links(&root, &mut state);
    let mut features = Vec::new();
    collect_placemarks(&root, &mut features, &mut state, 0)?;
    if features.is_empty() {
        return Err(Error::InvalidInput(
            "KML document contains no supported Placemark geometry".into(),
        ));
    }
    if state.style_data_ignored {
        warnings.push("KML placemark names, styles, ExtendedData, overlays, and model styling are not displayed".into());
    }
    if state.external_links {
        warnings
            .push("KML linked resources, including NetworkLink targets, are not fetched".into());
    }
    if state.unsupported_geometry {
        warnings.push("unsupported KML geometry such as Model or overlays was omitted".into());
    }
    if state.unsupported_extensions {
        warnings.push(
            "KML extension elements outside the supported core and gx:Track subset were omitted"
                .into(),
        );
    }
    if state.track_time_ignored {
        warnings.push("KML gx:Track timestamps and interpolation are not preserved".into());
    }
    if state.incomplete_track_data {
        warnings.push(
            "KML tracks with empty or unmatched position tuples were partially omitted".into(),
        );
    }
    if state.altitude_ignored {
        warnings.push("KML altitude and elevation modes are not applied".into());
    }
    let root = json!({ "type": "FeatureCollection", "features": features });
    crate::geospatial::geojson::convert_value(
        &root,
        source_format,
        title,
        "KML",
        std::mem::take(&mut warnings),
        sink,
    )
}

pub(crate) fn looks_like_kml_prefix(bytes: &[u8]) -> bool {
    crate::geospatial::xml_tree::looks_like_root(bytes, b"kml", Some(KML_NAMESPACE.as_bytes()))
}

fn inspect_metadata_and_links(node: &XmlElement, state: &mut KmlState) {
    let is_core = in_namespace(node, KML_NAMESPACE);
    let is_gx = in_namespace(node, GX_NAMESPACE);
    if !is_core && !is_gx {
        state.unsupported_extensions = true;
        for child in &node.children {
            inspect_metadata_and_links(child, state);
        }
        return;
    }
    if is_core {
        if matches!(
            node.name.as_str(),
            "name"
                | "description"
                | "ExtendedData"
                | "Style"
                | "StyleMap"
                | "StyleUrl"
                | "when"
                | "angles"
        ) {
            state.style_data_ignored = true;
        }
        if matches!(node.name.as_str(), "altitude" | "altitudeMode") {
            state.altitude_ignored = true;
        }
    } else if is_gx && node.name == "altitudeMode" {
        state.altitude_ignored = true;
    }
    if node.name == "href" && !node.text.trim().is_empty() {
        state.external_links = true;
    }
    if matches!(
        node.name.as_str(),
        "NetworkLink" | "GroundOverlay" | "ScreenOverlay" | "PhotoOverlay"
    ) {
        state.external_links = true;
    }
    for child in &node.children {
        inspect_metadata_and_links(child, state);
    }
}

fn in_namespace(node: &XmlElement, namespace: &str) -> bool {
    node.namespace.as_deref() == Some(namespace)
}

fn collect_placemarks(
    node: &XmlElement,
    features: &mut Vec<Value>,
    state: &mut KmlState,
    depth: usize,
) -> Result<()> {
    if depth > MAX_KML_XML_DEPTH {
        return Err(Error::LimitExceeded(
            "KML container nesting limit exceeded".into(),
        ));
    }
    if !in_namespace(node, KML_NAMESPACE) {
        if node.namespace.as_deref() != Some(GX_NAMESPACE) {
            state.unsupported_extensions = true;
        }
        return Ok(());
    }
    match node.name.as_str() {
        "Placemark" => {
            state.placemarks += 1;
            if state.placemarks > MAX_KML_PLACEMARKS {
                return Err(Error::LimitExceeded(format!(
                    "KML contains more than {MAX_KML_PLACEMARKS} Placemarks"
                )));
            }
            let mut geometries = Vec::new();
            for child in &node.children {
                collect_geometry_nodes(child, &mut geometries, state, 0)?;
            }
            if geometries.is_empty() {
                state.unsupported_geometry = true;
                return Ok(());
            }
            state.style_data_ignored = true;
            let geometry = if geometries.len() == 1 {
                geometries.pop().unwrap()
            } else {
                json!({ "type": "GeometryCollection", "geometries": geometries })
            };
            features.push(json!({ "type": "Feature", "geometry": geometry, "properties": null }));
            if features.len() > MAX_KML_PLACEMARKS {
                return Err(Error::LimitExceeded(format!(
                    "KML contains more than {MAX_KML_PLACEMARKS} renderable Placemarks"
                )));
            }
            Ok(())
        }
        "NetworkLink" => {
            state.external_links = true;
            Ok(())
        }
        "GroundOverlay" | "ScreenOverlay" | "PhotoOverlay" | "Style" | "StyleMap" => {
            state.style_data_ignored = true;
            if matches!(
                node.name.as_str(),
                "GroundOverlay" | "ScreenOverlay" | "PhotoOverlay"
            ) {
                state.external_links = true;
            }
            Ok(())
        }
        _ => {
            for child in &node.children {
                collect_placemarks(child, features, state, depth + 1)?;
            }
            Ok(())
        }
    }
}

fn collect_geometry_nodes(
    node: &XmlElement,
    geometries: &mut Vec<Value>,
    state: &mut KmlState,
    depth: usize,
) -> Result<()> {
    if depth > MAX_KML_GEOMETRY_DEPTH {
        return Err(Error::LimitExceeded(
            "KML geometry nesting limit exceeded".into(),
        ));
    }
    if is_geometry_element(node) {
        if let Some(geometry) = parse_kml_geometry(node, state, depth)? {
            geometries.push(geometry);
        }
        return Ok(());
    }
    if !in_namespace(node, KML_NAMESPACE) {
        state.unsupported_extensions = true;
        return Ok(());
    }
    if matches!(
        node.name.as_str(),
        "name" | "description" | "ExtendedData" | "StyleUrl"
    ) {
        state.style_data_ignored = true;
    }
    if node.name == "Model" {
        state.unsupported_geometry = true;
        return Ok(());
    }
    for child in &node.children {
        collect_geometry_nodes(child, geometries, state, depth + 1)?;
    }
    Ok(())
}

fn is_geometry_element(node: &XmlElement) -> bool {
    (in_namespace(node, KML_NAMESPACE)
        && matches!(
            node.name.as_str(),
            "Point" | "LineString" | "Polygon" | "MultiGeometry"
        ))
        || (in_namespace(node, GX_NAMESPACE)
            && matches!(node.name.as_str(), "Track" | "MultiTrack"))
}

fn parse_kml_geometry(
    node: &XmlElement,
    state: &mut KmlState,
    depth: usize,
) -> Result<Option<Value>> {
    match node.name.as_str() {
        "Point" => {
            let positions = parse_coordinates(
                find_descendant_in_namespace(node, "coordinates", KML_NAMESPACE)
                    .map(|n| n.text.as_str())
                    .unwrap_or(""),
                state,
            )?;
            match positions.len() {
                0 => Ok(None),
                1 => Ok(Some(
                    json!({ "type": "Point", "coordinates": positions[0] }),
                )),
                _ => Ok(Some(
                    json!({ "type": "MultiPoint", "coordinates": positions }),
                )),
            }
        }
        "LineString" => {
            let positions = parse_coordinates(
                find_descendant_in_namespace(node, "coordinates", KML_NAMESPACE)
                    .map(|n| n.text.as_str())
                    .unwrap_or(""),
                state,
            )?;
            if positions.len() < 2 {
                state.unsupported_geometry = true;
                Ok(None)
            } else {
                Ok(Some(
                    json!({ "type": "LineString", "coordinates": positions }),
                ))
            }
        }
        "Polygon" => {
            let mut rings = Vec::new();
            collect_rings(node, &mut rings, state)?;
            if rings.is_empty() {
                state.unsupported_geometry = true;
                Ok(None)
            } else {
                Ok(Some(json!({ "type": "Polygon", "coordinates": rings })))
            }
        }
        "MultiGeometry" => {
            let mut geometries = Vec::new();
            for child in &node.children {
                collect_geometry_nodes(child, &mut geometries, state, depth + 1)?;
            }
            if geometries.is_empty() {
                Ok(None)
            } else {
                Ok(Some(
                    json!({ "type": "GeometryCollection", "geometries": geometries }),
                ))
            }
        }
        "Track" => {
            let mut positions = Vec::new();
            let when_count = count_descendants(node, "when", KML_NAMESPACE);
            let coordinate_count = count_descendants(node, "coord", GX_NAMESPACE);
            state.track_time_ignored |= when_count > 0;
            state.incomplete_track_data |=
                when_count != coordinate_count || has_empty_descendant(node, "coord", GX_NAMESPACE);
            collect_track_coordinates(node, &mut positions, state)?;
            if positions.len() < 2 {
                state.unsupported_geometry = true;
                Ok(None)
            } else {
                Ok(Some(
                    json!({ "type": "LineString", "coordinates": positions }),
                ))
            }
        }
        "MultiTrack" => {
            let mut geometries = Vec::new();
            for child in &node.children {
                if in_namespace(child, GX_NAMESPACE)
                    && child.name == "Track"
                    && let Some(geometry) = parse_kml_geometry(child, state, depth + 1)?
                {
                    geometries.push(geometry);
                }
            }
            if geometries.is_empty() {
                Ok(None)
            } else {
                Ok(Some(
                    json!({ "type": "GeometryCollection", "geometries": geometries }),
                ))
            }
        }
        _ => {
            state.unsupported_geometry = true;
            Ok(None)
        }
    }
}

fn collect_rings(
    node: &XmlElement,
    rings: &mut Vec<Vec<Value>>,
    state: &mut KmlState,
) -> Result<()> {
    if !in_namespace(node, KML_NAMESPACE) {
        state.unsupported_extensions = true;
        return Ok(());
    }
    if node.name == "LinearRing" {
        let positions = parse_coordinates(
            find_descendant_in_namespace(node, "coordinates", KML_NAMESPACE)
                .map(|n| n.text.as_str())
                .unwrap_or(""),
            state,
        )?;
        if positions.len() >= 3 {
            rings.push(positions);
        } else {
            state.unsupported_geometry = true;
        }
        return Ok(());
    }
    for child in &node.children {
        collect_rings(child, rings, state)?;
    }
    Ok(())
}

fn collect_track_coordinates(
    node: &XmlElement,
    positions: &mut Vec<Value>,
    state: &mut KmlState,
) -> Result<()> {
    for child in &node.children {
        if child.name == "coord" && in_namespace(child, GX_NAMESPACE) {
            let fields = child.text.split_whitespace().collect::<Vec<_>>();
            if fields.len() < 2 {
                continue;
            }
            let lon = parse_coordinate(fields[0])?;
            let lat = parse_coordinate(fields[1])?;
            let coordinate = vec![json!(lon), json!(lat)];
            if let Some(altitude) = fields.get(2) {
                let _ = parse_coordinate(altitude)?;
                state.altitude_ignored = true;
            }
            push_position(json!(coordinate), positions, state)?;
        } else if in_namespace(child, GX_NAMESPACE) || in_namespace(child, KML_NAMESPACE) {
            collect_track_coordinates(child, positions, state)?;
        }
    }
    Ok(())
}

fn parse_coordinates(text: &str, state: &mut KmlState) -> Result<Vec<Value>> {
    let mut positions = Vec::new();
    for tuple in text.split_whitespace() {
        let parts = tuple.split(',').collect::<Vec<_>>();
        if parts.len() < 2 {
            return Err(Error::InvalidInput(
                "KML coordinate tuple needs longitude,latitude".into(),
            ));
        }
        let longitude = parse_coordinate(parts[0])?;
        let latitude = parse_coordinate(parts[1])?;
        let position = vec![json!(longitude), json!(latitude)];
        if let Some(altitude) = parts.get(2) {
            let _ = parse_coordinate(altitude)?;
            state.altitude_ignored = true;
        }
        if parts.len() > 3 {
            state.altitude_ignored = true;
        }
        push_position(json!(position), &mut positions, state)?;
    }
    Ok(positions)
}

fn push_position(position: Value, positions: &mut Vec<Value>, state: &mut KmlState) -> Result<()> {
    state.coordinates = state.coordinates.saturating_add(1);
    if state.coordinates > MAX_KML_COORDINATES {
        return Err(Error::LimitExceeded(format!(
            "KML exceeds {MAX_KML_COORDINATES} coordinate tuples"
        )));
    }
    positions.push(position);
    Ok(())
}

fn parse_coordinate(text: &str) -> Result<f64> {
    let value = text
        .parse::<f64>()
        .map_err(|_| Error::InvalidInput("KML coordinate is not numeric".into()))?;
    if !value.is_finite() {
        return Err(Error::InvalidInput("KML coordinate is not finite".into()));
    }
    Ok(value)
}

fn find_descendant_in_namespace<'a>(
    node: &'a XmlElement,
    name: &str,
    namespace: &str,
) -> Option<&'a XmlElement> {
    if node.name == name && in_namespace(node, namespace) {
        return Some(node);
    }
    node.children
        .iter()
        .find_map(|child| find_descendant_in_namespace(child, name, namespace))
}

fn count_descendants(node: &XmlElement, name: &str, namespace: &str) -> usize {
    node.children
        .iter()
        .map(|child| {
            usize::from(child.name == name && in_namespace(child, namespace))
                + count_descendants(child, name, namespace)
        })
        .sum()
}

fn has_empty_descendant(node: &XmlElement, name: &str, namespace: &str) -> bool {
    node.children.iter().any(|child| {
        (child.name == name && in_namespace(child, namespace) && child.text.trim().is_empty())
            || has_empty_descendant(child, name, namespace)
    })
}
