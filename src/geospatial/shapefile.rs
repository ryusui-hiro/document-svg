//! Bounded ESRI Shapefile geometry previews.

use std::fs;
use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use crate::convert::{ConvertOptions, PageConsumer, read_limited_file};
use crate::error::{Error, Result};

const MAX_SHP_BYTES: u64 = 64 * 1024 * 1024;
const MAX_PRJ_BYTES: u64 = 1024 * 1024;
const MAX_SHX_BYTES: u64 = 1024 * 1024;
const MAX_RECORDS: usize = 100_000;
const MAX_POSITIONS: usize = 500_000;
const MAX_PARTS: usize = 100_000;
const MAX_RING_TESTS: usize = 2_000_000;

#[derive(Debug)]
struct Ring {
    positions: Vec<[f64; 2]>,
    bbox: [f64; 4],
    signed_area: f64,
}

#[derive(Default)]
struct ParseState {
    positions: usize,
    parts: usize,
    has_null_shapes: bool,
    has_z_or_m: bool,
    closed_rings: bool,
    degenerate_parts: bool,
    orphan_holes: bool,
}

pub(crate) fn looks_like_shapefile_prefix(bytes: &[u8]) -> bool {
    bytes.len() >= 36
        && u32::from_be_bytes(bytes[0..4].try_into().unwrap()) == 9994
        && u32::from_le_bytes(bytes[28..32].try_into().unwrap()) == 1000
        && matches!(
            u32::from_le_bytes(bytes[32..36].try_into().unwrap()),
            0 | 1 | 3 | 5 | 8 | 11 | 13 | 15 | 18 | 21 | 23 | 25 | 28
        )
}

pub(crate) fn convert(
    path: &Path,
    options: &ConvertOptions,
    sink: &mut dyn PageConsumer,
) -> Result<Vec<String>> {
    let bytes = read_limited_file(
        path,
        options.max_input_bytes.min(MAX_SHP_BYTES),
        "Shapefile input",
    )?;
    let (root, mut warnings) = parse_shapefile(&bytes)?;
    match read_sidecar(path, "prj", MAX_PRJ_BYTES, "Shapefile projection")? {
        Some((_, projection)) => validate_wgs84_projection(&projection)?,
        None => warnings.push(
            "Shapefile .prj file is missing; coordinates are assumed to be WGS 84 longitude/latitude".into(),
        ),
    }
    if read_sidecar(path, "dbf", 0, "Shapefile attributes")?.is_some() {
        warnings.push("Shapefile DBF attribute fields are not displayed".into());
    }

    let index = read_sidecar(path, "shx", MAX_SHX_BYTES, "Shapefile index")?;
    if let Some((_, index)) = index {
        validate_index(&index, &bytes)?;
    }

    crate::geospatial::geojson::convert_value(
        &root,
        "shapefile",
        "Shapefile Map Preview",
        "Shapefile",
        warnings,
        sink,
    )
}

fn parse_shapefile(bytes: &[u8]) -> Result<(Value, Vec<String>)> {
    if bytes.len() < 100 {
        return Err(invalid("file is shorter than its 100-byte header"));
    }
    if read_u32_be(bytes, 0)? != 9994 {
        return Err(invalid("file code is not 9994"));
    }
    if read_u32_le(bytes, 28)? != 1000 {
        return Err(Error::Unsupported(
            "only Shapefile version 1000 is supported".into(),
        ));
    }
    let declared_length = usize::try_from(read_u32_be(bytes, 24)?)
        .ok()
        .and_then(|words| words.checked_mul(2))
        .ok_or_else(|| invalid("declared file length overflows"))?;
    if declared_length != bytes.len() {
        return Err(invalid(format!(
            "header length is {declared_length} bytes but input has {}",
            bytes.len()
        )));
    }
    let shape_type = read_u32_le(bytes, 32)?;
    if !is_supported_type(shape_type) {
        return Err(Error::Unsupported(format!(
            "Shapefile shape type {shape_type} is unsupported"
        )));
    }

    let mut offset = 100usize;
    let mut expected_record_number = 1u32;
    let mut record_count = 0usize;
    let mut features = Vec::new();
    let mut state = ParseState::default();
    while offset < bytes.len() {
        if record_count >= MAX_RECORDS {
            return Err(Error::LimitExceeded(format!(
                "Shapefile exceeds {MAX_RECORDS} records"
            )));
        }
        if bytes.len().saturating_sub(offset) < 8 {
            return Err(invalid("truncated record header"));
        }
        let record_number = read_u32_be(bytes, offset)?;
        let content_words = read_u32_be(bytes, offset + 4)?;
        if record_number != expected_record_number {
            return Err(invalid(format!(
                "record number {record_number} is not the expected {expected_record_number}"
            )));
        }
        expected_record_number = expected_record_number.saturating_add(1);
        let content_bytes = usize::try_from(content_words)
            .ok()
            .and_then(|words| words.checked_mul(2))
            .ok_or_else(|| invalid("record content length overflows"))?;
        let content_start = offset + 8;
        let content_end = content_start
            .checked_add(content_bytes)
            .filter(|end| *end <= bytes.len())
            .ok_or_else(|| invalid("record content exceeds the file"))?;
        if content_bytes < 4 {
            return Err(invalid("record content is shorter than the shape type"));
        }
        record_count += 1;
        let record = &bytes[content_start..content_end];
        let actual_type = read_u32_le(record, 0)?;
        if actual_type == 0 {
            state.has_null_shapes = true;
            if content_bytes != 4 {
                return Err(invalid(
                    "Null Shape record must contain only its shape type",
                ));
            }
        } else {
            if actual_type != shape_type {
                return Err(invalid(format!(
                    "record shape type {actual_type} differs from header type {shape_type}"
                )));
            }
            let geometry = parse_record_geometry(record, actual_type, &mut state)?;
            if !geometry.is_null() {
                features
                    .push(json!({ "type": "Feature", "geometry": geometry, "properties": null }));
            }
        }
        offset = content_end;
    }
    if offset != bytes.len() {
        return Err(invalid(
            "record sequence does not end at the declared file length",
        ));
    }

    let mut warnings = Vec::new();
    if state.has_null_shapes {
        warnings.push("null Shapefile records were omitted".into());
    }
    if state.has_z_or_m {
        warnings.push(
            "Shapefile Z and measure ordinates were validated and omitted from the 2D preview"
                .into(),
        );
    }
    if state.closed_rings {
        warnings.push("open Shapefile polygon rings were closed for preview".into());
    }
    if state.degenerate_parts {
        warnings.push("empty or degenerate Shapefile parts were omitted".into());
    }
    if state.orphan_holes {
        warnings.push("Shapefile polygon rings without a containing outer ring were rendered as separate polygons".into());
    }
    if shape_type == 0 && record_count == 0 {
        return Err(Error::InvalidInput(
            "Shapefile has no records or declared geometry type".into(),
        ));
    }
    Ok((
        json!({ "type": "FeatureCollection", "features": features }),
        warnings,
    ))
}

fn parse_record_geometry(record: &[u8], shape_type: u32, state: &mut ParseState) -> Result<Value> {
    state.has_z_or_m |= matches!(shape_type, 11 | 13 | 15 | 18 | 21 | 23 | 25 | 28);
    match shape_type {
        1 | 11 | 21 => {
            let expected_min = match shape_type {
                1 => 20,
                11 | 21 => 28,
                _ => unreachable!(),
            };
            if record.len() < expected_min {
                return Err(invalid("truncated Point record"));
            }
            let x = read_coordinate(record, 4)?;
            let y = read_coordinate(record, 12)?;
            validate_xy(x, y)?;
            state.positions = checked_add_limit(state.positions, 1, MAX_POSITIONS, "positions")?;
            match shape_type {
                11 => {
                    if !read_f64_le(record, 20)?.is_finite() {
                        return Err(invalid("Z ordinate is not finite"));
                    }
                    validate_optional_measure(record, 28)?;
                    if record.len() != 28 && record.len() != 36 {
                        return Err(invalid("PointZ record has an invalid length"));
                    }
                }
                21 => {
                    if record.len() != 28 {
                        return Err(invalid("PointM record has an invalid length"));
                    }
                    validate_measure(read_f64_le(record, 20)?)?;
                }
                1 if record.len() != 20 => return Err(invalid("Point record has trailing bytes")),
                _ => {}
            }
            Ok(json!({ "type": "Point", "coordinates": [x, y] }))
        }
        8 | 18 | 28 => {
            let (points, end) = parse_points(record, 40, state)?;
            let end = validate_dimension_arrays(record, end, points.len(), shape_type)?;
            if end != record.len() {
                return Err(invalid("MultiPoint record has trailing bytes"));
            }
            Ok(json!({ "type": "MultiPoint", "coordinates": points }))
        }
        3 | 5 | 13 | 15 | 23 | 25 => {
            let (parts, points, end) = parse_parts_and_points(record, state)?;
            let end = validate_dimension_arrays(record, end, points.len(), shape_type)?;
            if end != record.len() {
                return Err(invalid("PolyLine/Polygon record has trailing bytes"));
            }
            if matches!(shape_type, 3 | 13 | 23) {
                Ok(lines_geometry(parts, points, state))
            } else {
                polygon_geometry(parts, points, state)
            }
        }
        _ => Err(Error::Unsupported(format!(
            "Shapefile shape type {shape_type} is not supported"
        ))),
    }
}

fn parse_points(
    record: &[u8],
    points_offset: usize,
    state: &mut ParseState,
) -> Result<(Vec<[f64; 2]>, usize)> {
    if record.len() < points_offset {
        return Err(invalid("truncated MultiPoint header"));
    }
    let count = checked_count(read_i32_le(record, 36)?, MAX_POSITIONS, "point")?;
    if count == 0 {
        state.degenerate_parts = true;
    }
    let point_bytes = count
        .checked_mul(16)
        .and_then(|size| points_offset.checked_add(size))
        .filter(|end| *end <= record.len())
        .ok_or_else(|| invalid("MultiPoint coordinate array is truncated"))?;
    state.positions = checked_add_limit(state.positions, count, MAX_POSITIONS, "positions")?;
    let mut points = Vec::with_capacity(count);
    for index in 0..count {
        let offset = points_offset + index * 16;
        let x = read_coordinate(record, offset)?;
        let y = read_coordinate(record, offset + 8)?;
        validate_xy(x, y)?;
        points.push([x, y]);
    }
    Ok((points, point_bytes))
}

fn parse_parts_and_points(
    record: &[u8],
    state: &mut ParseState,
) -> Result<(Vec<usize>, Vec<[f64; 2]>, usize)> {
    if record.len() < 44 {
        return Err(invalid("truncated PolyLine/Polygon header"));
    }
    let part_count = checked_count(read_i32_le(record, 36)?, MAX_PARTS, "part")?;
    let point_count = checked_count(read_i32_le(record, 40)?, MAX_POSITIONS, "point")?;
    if part_count == 0 || point_count == 0 {
        if part_count != 0 || point_count != 0 {
            return Err(invalid(
                "part and point counts must both be zero or both be nonzero",
            ));
        }
        state.degenerate_parts = true;
    }
    state.parts = checked_add_limit(state.parts, part_count, MAX_PARTS, "parts")?;
    state.positions = checked_add_limit(state.positions, point_count, MAX_POSITIONS, "positions")?;
    let parts_end = part_count
        .checked_mul(4)
        .and_then(|size| 44usize.checked_add(size))
        .filter(|end| *end <= record.len())
        .ok_or_else(|| invalid("part index array is truncated"))?;
    let points_end = point_count
        .checked_mul(16)
        .and_then(|size| parts_end.checked_add(size))
        .filter(|end| *end <= record.len())
        .ok_or_else(|| invalid("coordinate array is truncated"))?;
    let mut parts = Vec::with_capacity(part_count);
    for index in 0..part_count {
        let start = checked_count(
            read_i32_le(record, 44 + index * 4)?,
            point_count,
            "part index",
        )?;
        if (index == 0 && start != 0)
            || start >= point_count
            || parts.last().is_some_and(|previous| *previous >= start)
        {
            return Err(invalid(
                "part indexes must start at zero and increase within the point array",
            ));
        }
        parts.push(start);
    }
    let mut points = Vec::with_capacity(point_count);
    for index in 0..point_count {
        let offset = parts_end + index * 16;
        let x = read_coordinate(record, offset)?;
        let y = read_coordinate(record, offset + 8)?;
        validate_xy(x, y)?;
        points.push([x, y]);
    }
    Ok((parts, points, points_end))
}

fn validate_dimension_arrays(
    record: &[u8],
    offset: usize,
    point_count: usize,
    shape_type: u32,
) -> Result<usize> {
    let has_z = matches!(shape_type, 11 | 13 | 15 | 18);
    let has_m = matches!(shape_type, 21 | 23 | 25 | 28);
    let mut next = offset;
    if has_z {
        next = validate_double_array(record, next, point_count, true, "Z")?;
        // Z records may omit M entirely; if present, the complete M block is required.
        if next < record.len() {
            next = validate_double_array(record, next, point_count, false, "measure")?;
        }
    } else if has_m {
        next = validate_double_array(record, next, point_count, false, "measure")?;
    }
    Ok(next)
}

fn validate_double_array(
    record: &[u8],
    offset: usize,
    point_count: usize,
    finite_values: bool,
    label: &str,
) -> Result<usize> {
    let required = 16usize
        .checked_add(
            point_count
                .checked_mul(8)
                .ok_or_else(|| invalid("ordinate array length overflows"))?,
        )
        .and_then(|size| offset.checked_add(size))
        .filter(|end| *end <= record.len())
        .ok_or_else(|| invalid(format!("{label} range/array is truncated")))?;
    if finite_values {
        for index in 0..point_count {
            let value = read_f64_le(record, offset + 16 + index * 8)?;
            if !value.is_finite() {
                return Err(invalid(format!("{label} ordinate is not finite")));
            }
        }
    } else {
        for index in 0..point_count {
            validate_measure(read_f64_le(record, offset + 16 + index * 8)?)?;
        }
    }
    Ok(required)
}

fn validate_optional_measure(record: &[u8], offset: usize) -> Result<()> {
    if record.len() >= offset + 8 {
        validate_measure(read_f64_le(record, offset)?)?;
    }
    Ok(())
}

fn validate_measure(value: f64) -> Result<()> {
    // The Shapefile specification reserves values below -1e38 as "no data".
    if value.is_finite() {
        Ok(())
    } else {
        Err(invalid("measure ordinate is infinite"))
    }
}

fn lines_geometry(parts: Vec<usize>, points: Vec<[f64; 2]>, state: &mut ParseState) -> Value {
    let mut lines = Vec::<Vec<[f64; 2]>>::new();
    for (index, start) in parts.iter().copied().enumerate() {
        let end = parts.get(index + 1).copied().unwrap_or(points.len());
        if end.saturating_sub(start) < 2 {
            state.degenerate_parts = true;
            continue;
        }
        lines.push(points[start..end].to_vec());
    }
    match lines.len() {
        0 => Value::Null,
        1 => json!({ "type": "LineString", "coordinates": lines.remove(0) }),
        _ => json!({ "type": "MultiLineString", "coordinates": lines }),
    }
}

fn polygon_geometry(
    parts: Vec<usize>,
    points: Vec<[f64; 2]>,
    state: &mut ParseState,
) -> Result<Value> {
    let mut rings = Vec::new();
    for (index, start) in parts.iter().copied().enumerate() {
        let end = parts.get(index + 1).copied().unwrap_or(points.len());
        if end.saturating_sub(start) < 3 {
            state.degenerate_parts = true;
            continue;
        }
        let mut positions = points[start..end].to_vec();
        if positions.first() != positions.last() {
            positions.push(positions[0]);
            state.positions = checked_add_limit(state.positions, 1, MAX_POSITIONS, "positions")?;
            state.closed_rings = true;
        }
        if positions.len() < 4 {
            state.degenerate_parts = true;
            continue;
        }
        let signed_area = ring_signed_area(&positions);
        if !signed_area.is_finite() {
            return Err(invalid("polygon ring area is not finite"));
        }
        if signed_area == 0.0 {
            state.degenerate_parts = true;
            continue;
        }
        rings.push(make_ring(positions, signed_area));
    }
    if rings.is_empty() {
        state.degenerate_parts = true;
        return Ok(Value::Null);
    }
    let shells: Vec<usize> = rings
        .iter()
        .enumerate()
        .filter_map(|(index, ring)| (ring.signed_area <= 0.0).then_some(index))
        .collect();
    let mut polygons: Vec<Vec<Vec<[f64; 2]>>> = shells
        .iter()
        .map(|index| vec![rings[*index].positions.clone()])
        .collect();
    let mut ring_tests = 0usize;
    for (ring_index, ring) in rings.iter().enumerate() {
        if ring.signed_area <= 0.0 {
            continue;
        }
        let sample = ring.positions[0];
        let mut containing: Option<(usize, f64)> = None;
        for (polygon_index, shell_index) in shells.iter().copied().enumerate() {
            let shell = &rings[shell_index];
            if sample[0] < shell.bbox[0]
                || sample[0] > shell.bbox[2]
                || sample[1] < shell.bbox[1]
                || sample[1] > shell.bbox[3]
            {
                continue;
            }
            ring_tests = ring_tests.saturating_add(shell.positions.len());
            if ring_tests > MAX_RING_TESTS {
                return Err(Error::LimitExceeded(format!(
                    "Shapefile polygon ring nesting exceeds {MAX_RING_TESTS} segment checks"
                )));
            }
            if point_in_ring(sample, &shell.positions) {
                let area = shell.signed_area.abs();
                if containing.is_none_or(|(_, previous_area)| area < previous_area) {
                    containing = Some((polygon_index, area));
                }
            }
        }
        if let Some((polygon_index, _)) = containing {
            polygons[polygon_index].push(ring.positions.clone());
        } else {
            state.orphan_holes = true;
            polygons.push(vec![ring.positions.clone()]);
        }
        let _ = ring_index;
    }
    if polygons.len() == 1 {
        Ok(json!({ "type": "Polygon", "coordinates": polygons.remove(0) }))
    } else {
        Ok(json!({ "type": "MultiPolygon", "coordinates": polygons }))
    }
}

fn make_ring(positions: Vec<[f64; 2]>, signed_area: f64) -> Ring {
    let mut bbox = [
        f64::INFINITY,
        f64::INFINITY,
        f64::NEG_INFINITY,
        f64::NEG_INFINITY,
    ];
    for [x, y] in &positions {
        bbox[0] = bbox[0].min(*x);
        bbox[1] = bbox[1].min(*y);
        bbox[2] = bbox[2].max(*x);
        bbox[3] = bbox[3].max(*y);
    }
    Ring {
        positions,
        bbox,
        signed_area,
    }
}

fn ring_signed_area(ring: &[[f64; 2]]) -> f64 {
    let mut twice_area = 0.0;
    for pair in ring.windows(2) {
        twice_area += pair[0][0] * pair[1][1] - pair[1][0] * pair[0][1];
    }
    twice_area / 2.0
}

fn point_in_ring([x, y]: [f64; 2], ring: &[[f64; 2]]) -> bool {
    let mut inside = false;
    for pair in ring.windows(2) {
        let [x1, y1] = pair[0];
        let [x2, y2] = pair[1];
        if (y1 > y) != (y2 > y) && x < (x2 - x1) * (y - y1) / (y2 - y1) + x1 {
            inside = !inside;
        }
    }
    inside
}

fn validate_wgs84_projection(bytes: &[u8]) -> Result<()> {
    let projection = std::str::from_utf8(bytes)
        .map_err(|_| Error::InvalidInput("Shapefile .prj is not UTF-8/ASCII WKT".into()))?;
    let lower = projection.to_ascii_lowercase();
    let is_projected = ["projcs[", "projectedcrs[", "projcrs["]
        .iter()
        .any(|marker| lower.contains(marker));
    let is_geographic = lower.contains("geogcs[") || lower.contains("geogcrs[");
    let is_degree_based = lower.contains("degree");
    let compact = lower
        .chars()
        .filter(|character| !character.is_whitespace())
        .collect::<String>();
    let has_epsg_4326 = compact.contains("\"epsg\",\"4326\"")
        || compact.contains("\"epsg\",4326]")
        || compact.contains("epsg:4326");
    let has_wgs84_datum =
        lower.contains("wgs_1984") || lower.contains("wgs 84") || lower.contains("wgs84");
    if is_projected || !is_geographic || !is_degree_based || !(has_epsg_4326 || has_wgs84_datum) {
        return Err(Error::Unsupported(
            "Shapefile projection is not a recognized degree-based WGS 84 geographic CRS; reproject to longitude/latitude first".into(),
        ));
    }
    Ok(())
}

fn read_sidecar(
    path: &Path,
    extension: &str,
    max_bytes: u64,
    context: &str,
) -> Result<Option<(PathBuf, Vec<u8>)>> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let Some(stem) = path.file_stem() else {
        return Ok(None);
    };
    let stem = stem.to_string_lossy();
    let mut candidates = Vec::new();
    let lowercase = extension.to_ascii_lowercase();
    let titlecase = format!(
        "{}{}",
        &lowercase[..1].to_ascii_uppercase(),
        &lowercase[1..]
    );
    for ext in [lowercase, extension.to_ascii_uppercase(), titlecase] {
        candidates.push(parent.join(format!("{stem}.{ext}")));
    }
    for candidate in candidates {
        match fs::metadata(&candidate) {
            Ok(metadata) if metadata.is_file() => {
                if max_bytes == 0 {
                    return Ok(Some((candidate, Vec::new())));
                }
                return Ok(Some((
                    candidate.clone(),
                    read_limited_file(&candidate, max_bytes, context)?,
                )));
            }
            Ok(_) => {
                return Err(Error::InvalidInput(format!(
                    "{} sidecar is not a regular file",
                    candidate.display()
                )));
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(None)
}

fn validate_index(bytes: &[u8], shp: &[u8]) -> Result<()> {
    if bytes.len() < 100 || read_u32_be(bytes, 0)? != 9994 || read_u32_le(bytes, 28)? != 1000 {
        return Err(invalid("SHX index header is malformed"));
    }
    if read_u32_le(bytes, 32)? != read_u32_le(shp, 32)? {
        return Err(invalid("SHX shape type differs from the SHP header"));
    }
    let declared = usize::try_from(read_u32_be(bytes, 24)?)
        .ok()
        .and_then(|words| words.checked_mul(2))
        .ok_or_else(|| invalid("SHX declared length overflows"))?;
    if declared != bytes.len() || !(bytes.len() - 100).is_multiple_of(8) {
        return Err(invalid("SHX length does not match its record index"));
    }
    let expected_count = (bytes.len() - 100) / 8;
    let shp_count = count_records(shp)?;
    if expected_count != shp_count {
        return Err(invalid("SHX entry count differs from SHP record count"));
    }
    let mut shp_offset = 100usize;
    for index in 0..expected_count {
        let entry = 100 + index * 8;
        let offset_words = read_u32_be(bytes, entry)?;
        let content_words = read_u32_be(bytes, entry + 4)?;
        if usize::try_from(offset_words)
            .ok()
            .and_then(|value| value.checked_mul(2))
            != Some(shp_offset)
            || content_words != read_u32_be(shp, shp_offset + 4)?
        {
            return Err(invalid("SHX entry does not match its SHP record"));
        }
        let bytes = usize::try_from(content_words)
            .ok()
            .and_then(|value| value.checked_mul(2))
            .ok_or_else(|| invalid("SHX content length overflows"))?;
        shp_offset = shp_offset
            .checked_add(8 + bytes)
            .ok_or_else(|| invalid("SHX record offset overflows"))?;
    }
    if shp_offset != shp.len() {
        return Err(invalid("SHX record offsets do not cover the SHP file"));
    }
    Ok(())
}

fn count_records(shp: &[u8]) -> Result<usize> {
    let mut offset = 100usize;
    let mut count = 0usize;
    while offset < shp.len() {
        let content = usize::try_from(read_u32_be(shp, offset + 4)?)
            .ok()
            .and_then(|words| words.checked_mul(2))
            .ok_or_else(|| invalid("SHP record length overflows"))?;
        offset = offset
            .checked_add(8 + content)
            .filter(|end| *end <= shp.len())
            .ok_or_else(|| invalid("SHP record length exceeds the file"))?;
        count += 1;
    }
    Ok(count)
}

fn validate_xy(x: f64, y: f64) -> Result<()> {
    if !x.is_finite()
        || !y.is_finite()
        || !(-180.0..=180.0).contains(&x)
        || !(-90.0..=90.0).contains(&y)
    {
        return Err(Error::InvalidInput(
            "Shapefile coordinate is outside WGS 84 longitude/latitude bounds".into(),
        ));
    }
    Ok(())
}

fn checked_count(value: i32, maximum: usize, label: &str) -> Result<usize> {
    let value =
        usize::try_from(value).map_err(|_| invalid(format!("{label} count is negative")))?;
    if value > maximum {
        return Err(Error::LimitExceeded(format!(
            "Shapefile {label} count {value} exceeds {maximum}"
        )));
    }
    Ok(value)
}

fn checked_add_limit(current: usize, amount: usize, maximum: usize, label: &str) -> Result<usize> {
    let total = current.saturating_add(amount);
    if total > maximum {
        return Err(Error::LimitExceeded(format!(
            "Shapefile {label} exceed {maximum}"
        )));
    }
    Ok(total)
}

fn is_supported_type(shape_type: u32) -> bool {
    matches!(
        shape_type,
        0 | 1 | 3 | 5 | 8 | 11 | 13 | 15 | 18 | 21 | 23 | 25 | 28
    )
}

fn read_u32_be(bytes: &[u8], offset: usize) -> Result<u32> {
    let value = bytes
        .get(offset..offset + 4)
        .ok_or_else(|| invalid("truncated 32-bit integer"))?;
    Ok(u32::from_be_bytes(value.try_into().unwrap()))
}

fn read_u32_le(bytes: &[u8], offset: usize) -> Result<u32> {
    let value = bytes
        .get(offset..offset + 4)
        .ok_or_else(|| invalid("truncated 32-bit integer"))?;
    Ok(u32::from_le_bytes(value.try_into().unwrap()))
}

fn read_i32_le(bytes: &[u8], offset: usize) -> Result<i32> {
    let value = bytes
        .get(offset..offset + 4)
        .ok_or_else(|| invalid("truncated 32-bit integer"))?;
    Ok(i32::from_le_bytes(value.try_into().unwrap()))
}

fn read_f64_le(bytes: &[u8], offset: usize) -> Result<f64> {
    let value = bytes
        .get(offset..offset + 8)
        .ok_or_else(|| invalid("truncated 64-bit float"))?;
    Ok(f64::from_le_bytes(value.try_into().unwrap()))
}

fn read_coordinate(bytes: &[u8], offset: usize) -> Result<f64> {
    let value = read_f64_le(bytes, offset)?;
    if !value.is_finite() {
        return Err(invalid("coordinate is not finite"));
    }
    Ok(value)
}

fn invalid(message: impl Into<String>) -> Error {
    Error::InvalidInput(format!("invalid Shapefile: {}", message.into()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn point_shape(x: f64, y: f64) -> Vec<u8> {
        let mut record = 1u32.to_le_bytes().to_vec();
        record.extend_from_slice(&x.to_le_bytes());
        record.extend_from_slice(&y.to_le_bytes());
        record
    }

    fn shapefile(shape_type: u32, records: &[Vec<u8>]) -> Vec<u8> {
        let mut bytes = vec![0; 100];
        bytes[0..4].copy_from_slice(&9994u32.to_be_bytes());
        bytes[24..28].copy_from_slice(&0u32.to_be_bytes());
        bytes[28..32].copy_from_slice(&1000u32.to_le_bytes());
        bytes[32..36].copy_from_slice(&shape_type.to_le_bytes());
        for (index, record) in records.iter().enumerate() {
            bytes.extend_from_slice(&u32::try_from(index + 1).unwrap().to_be_bytes());
            bytes.extend_from_slice(&u32::try_from(record.len() / 2).unwrap().to_be_bytes());
            bytes.extend_from_slice(record);
        }
        let words = u32::try_from(bytes.len() / 2).unwrap();
        bytes[24..28].copy_from_slice(&words.to_be_bytes());
        bytes
    }

    #[test]
    fn recognizes_shapefile_header_and_mixed_endian_record() {
        let bytes = shapefile(1, &[point_shape(-122.4, 37.8)]);
        assert!(looks_like_shapefile_prefix(&bytes[..36]));
        let (root, warnings) = parse_shapefile(&bytes).unwrap();
        assert!(warnings.is_empty());
        assert_eq!(root["features"][0]["geometry"]["coordinates"][0], -122.4);
        assert_eq!(root["features"][0]["geometry"]["coordinates"][1], 37.8);
    }

    #[test]
    fn polygon_ring_order_does_not_change_shell_and_hole_assignment() {
        let bytes = include_bytes!("../../tests/fixtures/sample_polygon.shp");
        let (root, warnings) = parse_shapefile(bytes).unwrap();
        let geometry = &root["features"][0]["geometry"];
        assert_eq!(geometry["type"], "MultiPolygon");
        let polygons = geometry["coordinates"].as_array().unwrap();
        assert_eq!(polygons.len(), 2);
        assert_eq!(polygons[0].as_array().unwrap().len(), 2);
        assert_eq!(polygons[1].as_array().unwrap().len(), 1);
        assert!(warnings.is_empty());
    }

    #[test]
    fn parses_multipoint_and_measured_polyline_record_layouts() {
        let mut multipoint = 8u32.to_le_bytes().to_vec();
        for value in [-122.5f64, 37.0, -121.5, 38.0] {
            multipoint.extend_from_slice(&value.to_le_bytes());
        }
        multipoint.extend_from_slice(&2i32.to_le_bytes());
        for [x, y] in [[-122.4f64, 37.2], [-121.8, 37.7]] {
            multipoint.extend_from_slice(&x.to_le_bytes());
            multipoint.extend_from_slice(&y.to_le_bytes());
        }
        let mut state = ParseState::default();
        let geometry = parse_record_geometry(&multipoint, 8, &mut state).unwrap();
        assert_eq!(geometry["type"], "MultiPoint");
        assert_eq!(geometry["coordinates"].as_array().unwrap().len(), 2);

        let mut measured_line = 23u32.to_le_bytes().to_vec();
        for value in [-122.5f64, 37.0, -121.5, 38.0] {
            measured_line.extend_from_slice(&value.to_le_bytes());
        }
        measured_line.extend_from_slice(&2i32.to_le_bytes());
        measured_line.extend_from_slice(&4i32.to_le_bytes());
        measured_line.extend_from_slice(&0i32.to_le_bytes());
        measured_line.extend_from_slice(&2i32.to_le_bytes());
        for [x, y] in [
            [-122.4f64, 37.2],
            [-122.1, 37.3],
            [-121.9, 37.6],
            [-121.7, 37.7],
        ] {
            measured_line.extend_from_slice(&x.to_le_bytes());
            measured_line.extend_from_slice(&y.to_le_bytes());
        }
        for value in [0.0f64, 1.0, 0.0, 0.5, 0.5, 1.0] {
            measured_line.extend_from_slice(&value.to_le_bytes());
        }
        let mut state = ParseState::default();
        let geometry = parse_record_geometry(&measured_line, 23, &mut state).unwrap();
        assert_eq!(geometry["type"], "MultiLineString");
        assert_eq!(geometry["coordinates"].as_array().unwrap().len(), 2);
        assert!(state.has_z_or_m);
    }

    #[test]
    fn parses_point_z_and_m_with_optional_z_measure() {
        let mut point_z = 11u32.to_le_bytes().to_vec();
        for value in [-122.4f64, 37.8, 14.5, -1.0e39] {
            point_z.extend_from_slice(&value.to_le_bytes());
        }
        let mut state = ParseState::default();
        let geometry = parse_record_geometry(&point_z, 11, &mut state).unwrap();
        assert_eq!(geometry["type"], "Point");
        assert_eq!(geometry["coordinates"][0], -122.4);

        let mut point_m = 21u32.to_le_bytes().to_vec();
        for value in [-122.4f64, 37.8, -1.0e39] {
            point_m.extend_from_slice(&value.to_le_bytes());
        }
        let mut state = ParseState::default();
        let geometry = parse_record_geometry(&point_m, 21, &mut state).unwrap();
        assert_eq!(geometry["coordinates"][1], 37.8);
        assert!(state.has_z_or_m);
    }

    #[test]
    fn rejects_bad_header_length_and_unsupported_crs() {
        let mut bytes = shapefile(1, &[point_shape(0.0, 0.0)]);
        bytes[24..28].copy_from_slice(&51u32.to_be_bytes());
        assert!(parse_shapefile(&bytes).is_err());
        assert!(
            validate_wgs84_projection(br#"PROJCS["WGS_1984_Web_Mercator_Auxiliary_Sphere"]"#)
                .is_err()
        );
        assert!(validate_wgs84_projection(br#"GEOCCS["WGS 84 geocentric"]"#).is_err());
        assert!(
            validate_wgs84_projection(
                br#"GEOGCS["GCS_WGS_1984",UNIT["Degree",0.0174532925199433],AUTHORITY["EPSG","4326"]]"#
            )
            .is_ok()
        );
        assert!(validate_wgs84_projection(br#"GEOGCS["WGS_1984",UNIT["Radian",1]]"#).is_err());
    }
}
