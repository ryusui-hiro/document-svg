//! Bounded RFC 8142 GeoJSON Text Sequence and newline-delimited previews.

use std::path::Path;

use serde_json::{Map, Value, json};

use crate::convert::{ConvertOptions, PageConsumer, read_limited_file};
use crate::error::{Error, Result};

const MAX_GEOJSON_SEQUENCE_BYTES: u64 = 32 * 1024 * 1024;
const MAX_GEOJSON_SEQUENCE_RECORD_BYTES: usize = 8 * 1024 * 1024;
const MAX_GEOJSON_SEQUENCE_RECORDS: usize = 100_000;
const RFC8142_RECORD_SEPARATOR: u8 = 0x1e;

pub(crate) fn looks_like_rfc8142_prefix(bytes: &[u8]) -> bool {
    if !bytes.starts_with(&[RFC8142_RECORD_SEPARATOR]) {
        return false;
    }
    let payload = &bytes[1..];
    let end = payload
        .iter()
        .position(|byte| *byte == RFC8142_RECORD_SEPARATOR)
        .unwrap_or(payload.len());
    crate::convert::looks_like_geojson(&payload[..end])
}

pub(crate) fn convert(
    path: &Path,
    options: &ConvertOptions,
    sink: &mut dyn PageConsumer,
) -> Result<Vec<String>> {
    let bytes = read_limited_file(
        path,
        options.max_input_bytes.min(MAX_GEOJSON_SEQUENCE_BYTES),
        "GeoJSON sequence input",
    )?;
    let newline_delimited = path
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("geojsonl"));
    let mut features = Vec::new();
    let record_count = if newline_delimited {
        parse_newline_delimited(&bytes, &mut features)?
    } else {
        parse_rfc8142(&bytes, &mut features)?
    };
    if features.is_empty() {
        return Err(invalid(
            "sequence contains no GeoJSON features or geometries",
        ));
    }
    let root = json!({ "type": "FeatureCollection", "features": features });
    let mut warnings = vec![format!(
        "{record_count} GeoJSON sequence record(s) were merged into one bounded map preview; feature properties and identifiers are omitted"
    )];
    if newline_delimited {
        warnings.push(
            "newline-delimited GeoJSON was read in compatibility mode; RFC 8142 uses RS-delimited records".into(),
        );
    }
    crate::geospatial::geojson::convert_value(
        &root,
        "geojsonseq",
        "GeoJSON Text Sequence Map Preview",
        "GeoJSON Text Sequence",
        warnings,
        sink,
    )
}

fn parse_rfc8142(bytes: &[u8], features: &mut Vec<Value>) -> Result<usize> {
    if bytes.first() != Some(&RFC8142_RECORD_SEPARATOR) {
        return Err(invalid(
            "RFC 8142 sequence must begin with an ASCII Record Separator (0x1E)",
        ));
    }
    let mut cursor = 0usize;
    let mut record_count = 0usize;
    while cursor < bytes.len() {
        if bytes[cursor] != RFC8142_RECORD_SEPARATOR {
            return Err(invalid("expected an RFC 8142 Record Separator"));
        }
        let body_start = cursor + 1;
        let next_separator = bytes[body_start..]
            .iter()
            .position(|byte| *byte == RFC8142_RECORD_SEPARATOR)
            .map(|offset| body_start + offset)
            .unwrap_or(bytes.len());
        let mut record = &bytes[body_start..next_separator];
        if !record.ends_with(b"\n") {
            return Err(invalid(
                "each RFC 8142 GeoJSON text must end with a line feed",
            ));
        }
        record = &record[..record.len() - 1];
        if record.ends_with(b"\r") {
            record = &record[..record.len() - 1];
        }
        append_record(record, features)?;
        record_count += 1;
        if record_count > MAX_GEOJSON_SEQUENCE_RECORDS {
            return Err(Error::LimitExceeded(format!(
                "GeoJSON Text Sequence exceeds {MAX_GEOJSON_SEQUENCE_RECORDS} records"
            )));
        }
        cursor = next_separator;
    }
    Ok(record_count)
}

fn parse_newline_delimited(bytes: &[u8], features: &mut Vec<Value>) -> Result<usize> {
    let mut record_count = 0usize;
    for line in bytes.split(|byte| *byte == b'\n') {
        let line = line.strip_suffix(b"\r").unwrap_or(line);
        if line.iter().all(u8::is_ascii_whitespace) {
            continue;
        }
        if line.first() == Some(&RFC8142_RECORD_SEPARATOR) {
            return Err(invalid(
                "newline-delimited GeoJSON cannot contain RFC 8142 Record Separators",
            ));
        }
        append_record(line, features)?;
        record_count += 1;
        if record_count > MAX_GEOJSON_SEQUENCE_RECORDS {
            return Err(Error::LimitExceeded(format!(
                "newline-delimited GeoJSON exceeds {MAX_GEOJSON_SEQUENCE_RECORDS} records"
            )));
        }
    }
    if record_count == 0 {
        return Err(invalid("newline-delimited GeoJSON contains no records"));
    }
    Ok(record_count)
}

fn append_record(bytes: &[u8], features: &mut Vec<Value>) -> Result<()> {
    if bytes.is_empty() || bytes.len() > MAX_GEOJSON_SEQUENCE_RECORD_BYTES {
        return Err(Error::LimitExceeded(format!(
            "GeoJSON sequence record is empty or exceeds {MAX_GEOJSON_SEQUENCE_RECORD_BYTES} bytes"
        )));
    }
    crate::geospatial::geojson::preflight_json_budget(bytes)?;
    let value: Value = serde_json::from_slice(bytes)
        .map_err(|error| invalid(format!("invalid JSON sequence record: {error}")))?;
    append_geojson_object(value, features)
}

fn append_geojson_object(value: Value, features: &mut Vec<Value>) -> Result<()> {
    let Value::Object(mut object) = value else {
        return Err(invalid("each sequence record must be a GeoJSON object"));
    };
    let kind = object
        .remove("type")
        .and_then(|value| value.as_str().map(str::to_owned))
        .ok_or_else(|| invalid("GeoJSON sequence object is missing a string type"))?;
    match kind.as_str() {
        "Feature" => append_feature(object, features),
        "FeatureCollection" => {
            let values = object
                .remove("features")
                .ok_or_else(|| invalid("GeoJSON FeatureCollection has no features array"))?;
            let Value::Array(values) = values else {
                return Err(invalid("GeoJSON FeatureCollection has no features array"));
            };
            for value in values {
                let Value::Object(mut feature) = value else {
                    return Err(invalid(
                        "GeoJSON FeatureCollection contains a non-Feature member",
                    ));
                };
                if feature
                    .remove("type")
                    .and_then(|value| value.as_str().map(str::to_owned))
                    .as_deref()
                    != Some("Feature")
                {
                    return Err(invalid(
                        "GeoJSON FeatureCollection contains a non-Feature member",
                    ));
                }
                append_feature(feature, features)?;
            }
            Ok(())
        }
        "Point" | "MultiPoint" | "LineString" | "MultiLineString" | "Polygon" | "MultiPolygon"
        | "GeometryCollection" => {
            let mut geometry = Map::new();
            geometry.insert("type".into(), Value::String(kind));
            geometry.extend(object);
            push_feature(Value::Object(geometry), features)
        }
        other => Err(Error::Unsupported(format!(
            "GeoJSON Text Sequence record type '{other}' is unsupported"
        ))),
    }
}

fn append_feature(mut object: Map<String, Value>, features: &mut Vec<Value>) -> Result<()> {
    let geometry = object.remove("geometry").unwrap_or(Value::Null);
    push_feature(geometry, features)
}

fn push_feature(geometry: Value, features: &mut Vec<Value>) -> Result<()> {
    if features.len() >= MAX_GEOJSON_SEQUENCE_RECORDS {
        return Err(Error::LimitExceeded(format!(
            "GeoJSON Text Sequence exceeds {MAX_GEOJSON_SEQUENCE_RECORDS} expanded features"
        )));
    }
    features.push(json!({
        "type": "Feature",
        "geometry": geometry,
        "properties": null
    }));
    Ok(())
}

fn invalid(message: impl Into<String>) -> Error {
    Error::InvalidInput(format!("invalid GeoJSON Text Sequence: {}", message.into()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_rfc8142_content_without_an_extension() {
        assert!(looks_like_rfc8142_prefix(
            b"\x1e{\"type\":\"Feature\",\"geometry\":null}\n"
        ));
        assert!(!looks_like_rfc8142_prefix(b"{\"type\":\"Feature\"}"));
    }

    #[test]
    fn parses_heterogeneous_rfc8142_records_and_drops_properties() {
        let bytes = b"\x1e{\"type\":\"Feature\",\"id\":\"secret\",\"properties\":{\"name\":\"private\"},\"geometry\":{\"type\":\"Point\",\"coordinates\":[139,35]}}\n\x1e{\"type\":\"LineString\",\"coordinates\":[[139,35],[140,36]]}\n";
        let mut features = Vec::new();
        let records = parse_rfc8142(bytes, &mut features).unwrap();
        assert_eq!(records, 2);
        assert_eq!(features.len(), 2);
        assert!(features[0]["properties"].is_null());
        assert_eq!(features[1]["geometry"]["type"], "LineString");
        assert!(!features[0].to_string().contains("private"));
    }

    #[test]
    fn rejects_missing_record_separator_or_final_line_feed() {
        assert!(
            parse_rfc8142(
                b"{\"type\":\"Point\",\"coordinates\":[1,2]}\n",
                &mut Vec::new()
            )
            .is_err()
        );
        assert!(
            parse_rfc8142(
                b"\x1e{\"type\":\"Point\",\"coordinates\":[1,2]}",
                &mut Vec::new()
            )
            .is_err()
        );
    }

    #[test]
    fn supports_newline_delimited_compatibility_records() {
        let mut features = Vec::new();
        let count = parse_newline_delimited(
            b"{\"type\":\"Feature\",\"geometry\":null}\n{\"type\":\"Point\",\"coordinates\":[1,2]}\n",
            &mut features,
        )
        .unwrap();
        assert_eq!(count, 2);
        assert_eq!(features.len(), 2);
    }
}
