//! Bounded WKT/EWKT simple-geometry map previews.

use std::path::Path;

use serde_json::{Value, json};

use crate::convert::{ConvertOptions, PageConsumer, read_limited_file};
use crate::error::{Error, Result};

const MAX_WKT_INPUT_BYTES: u64 = 32 * 1024 * 1024;
const MAX_WKT_POSITIONS: usize = 500_000;
const MAX_WKT_GEOMETRIES: usize = 200_000;
const MAX_WKT_GEOMETRY_DEPTH: usize = 16;

#[derive(Default)]
struct WktState {
    positions: usize,
    geometries: usize,
    extra_dimensions_ignored: bool,
    empty_members_ignored: bool,
}

struct WktParser<'a> {
    text: &'a str,
    bytes: &'a [u8],
    cursor: usize,
    state: WktState,
}

pub(crate) fn looks_like_wkt_prefix(bytes: &[u8]) -> bool {
    let Ok(text) = std::str::from_utf8(bytes) else {
        return false;
    };
    let mut text = text.trim_start_matches('\u{feff}').trim_start();
    if text
        .get(..5)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("SRID="))
    {
        let Some(separator) = text.find(';') else {
            return false;
        };
        text = text[separator + 1..].trim_start();
    }
    let word_end = text
        .find(|character: char| character.is_ascii_whitespace() || character == '(')
        .unwrap_or(text.len());
    let word = &text[..word_end];
    matches!(
        word.to_ascii_uppercase().as_str(),
        "POINT"
            | "MULTIPOINT"
            | "LINESTRING"
            | "MULTILINESTRING"
            | "POLYGON"
            | "MULTIPOLYGON"
            | "GEOMETRYCOLLECTION"
    )
}

pub(crate) fn convert(
    path: &Path,
    options: &ConvertOptions,
    sink: &mut dyn PageConsumer,
) -> Result<Vec<String>> {
    let bytes = read_limited_file(
        path,
        options.max_input_bytes.min(MAX_WKT_INPUT_BYTES),
        "WKT input",
    )?;
    let text = std::str::from_utf8(&bytes)
        .map_err(|error| Error::InvalidInput(format!("WKT input is not UTF-8: {error}")))?;
    let cursor = usize::from(text.starts_with('\u{feff}')) * '\u{feff}'.len_utf8();
    let mut parser = WktParser {
        text,
        bytes: text.as_bytes(),
        cursor,
        state: WktState::default(),
    };
    let srid = parser.parse_optional_srid()?;
    let geometry = parser
        .parse_geometry(0, 2)?
        .ok_or_else(|| Error::InvalidInput("WKT input contains no non-empty geometry".into()))?;
    parser.skip_whitespace();
    if parser.cursor != parser.bytes.len() {
        return Err(parser.invalid("trailing data after the WKT geometry"));
    }
    let mut warnings = Vec::new();
    if srid.is_none() {
        warnings.push(
            "WKT has no SRID; x/y are assumed to be WGS 84 longitude/latitude for map preview"
                .into(),
        );
    }
    if parser.state.extra_dimensions_ignored {
        warnings
            .push("WKT Z/M ordinates were validated and omitted from the 2D map preview".into());
    }
    if parser.state.empty_members_ignored {
        warnings.push("empty WKT geometry members were omitted".into());
    }
    let root = json!({
        "type": "Feature",
        "geometry": geometry,
        "properties": null
    });
    crate::geospatial::geojson::convert_value(
        &root,
        "wkt",
        "WKT Geometry Map Preview",
        "WKT",
        warnings,
        sink,
    )
}

impl WktParser<'_> {
    fn parse_optional_srid(&mut self) -> Result<Option<u32>> {
        self.skip_whitespace();
        if !self.remaining_ascii().starts_with_ignore_ascii_case("SRID") {
            return Ok(None);
        }
        self.consume_word("SRID")?;
        self.expect_char(b'=')?;
        let value = self.read_atom()?;
        let srid = value
            .parse::<u32>()
            .map_err(|_| self.invalid("EWKT SRID must be an unsigned integer"))?;
        self.expect_char(b';')?;
        if srid != 4326 {
            return Err(Error::Unsupported(format!(
                "WKT SRID {srid} cannot be projected without a coordinate transformation"
            )));
        }
        Ok(Some(srid))
    }

    fn parse_geometry(
        &mut self,
        depth: usize,
        inherited_dimensions: usize,
    ) -> Result<Option<Value>> {
        if depth > MAX_WKT_GEOMETRY_DEPTH {
            return Err(Error::LimitExceeded(format!(
                "WKT GeometryCollection nesting exceeds {MAX_WKT_GEOMETRY_DEPTH}"
            )));
        }
        self.state.geometries = self.state.geometries.saturating_add(1);
        if self.state.geometries > MAX_WKT_GEOMETRIES {
            return Err(Error::LimitExceeded(format!(
                "WKT contains more than {MAX_WKT_GEOMETRIES} geometries"
            )));
        }
        let geometry_type = self.read_word()?.to_ascii_uppercase();
        let dimensions = if self.consume_word_if("ZM") {
            4
        } else if self.consume_word_if("Z") || self.consume_word_if("M") {
            3
        } else {
            inherited_dimensions
        };
        self.state.extra_dimensions_ignored |= dimensions > 2;
        if self.consume_word_if("EMPTY") {
            self.state.empty_members_ignored = true;
            return Ok(None);
        }
        match geometry_type.as_str() {
            "POINT" => self.parse_point(dimensions).map(Some),
            "MULTIPOINT" => self.parse_multipoint(dimensions).map(Some),
            "LINESTRING" => self.parse_linestring(dimensions).map(Some),
            "MULTILINESTRING" => self.parse_multilinestring(dimensions).map(Some),
            "POLYGON" => self.parse_polygon(dimensions).map(Some),
            "MULTIPOLYGON" => self.parse_multipolygon(dimensions).map(Some),
            "GEOMETRYCOLLECTION" => self.parse_geometrycollection(depth, dimensions).map(Some),
            _ => Err(Error::Unsupported(format!(
                "WKT geometry type '{geometry_type}' is outside the supported linear simple-feature subset"
            ))),
        }
    }

    fn parse_point(&mut self, dimensions: usize) -> Result<Value> {
        self.expect_char(b'(')?;
        let position = self.parse_position(dimensions)?;
        self.expect_char(b')')?;
        Ok(json!({ "type": "Point", "coordinates": position }))
    }

    fn parse_multipoint(&mut self, dimensions: usize) -> Result<Value> {
        self.expect_char(b'(')?;
        let mut points = Vec::new();
        loop {
            if self.consume_word_if("EMPTY") {
                self.state.empty_members_ignored = true;
            } else {
                let point = if self.consume_char_if(b'(') {
                    let position = self.parse_position(dimensions)?;
                    self.expect_char(b')')?;
                    position
                } else {
                    self.parse_position(dimensions)?
                };
                points.push(point);
            }
            if self.consume_char_if(b',') {
                continue;
            }
            self.expect_char(b')')?;
            break;
        }
        Ok(json!({ "type": "MultiPoint", "coordinates": points }))
    }

    fn parse_linestring(&mut self, dimensions: usize) -> Result<Value> {
        let positions = self.parse_position_list(dimensions)?;
        if positions.len() < 2 {
            return Err(self.invalid("a non-empty WKT LineString requires at least two positions"));
        }
        Ok(json!({ "type": "LineString", "coordinates": positions }))
    }

    fn parse_multilinestring(&mut self, dimensions: usize) -> Result<Value> {
        self.expect_char(b'(')?;
        let mut lines = Vec::new();
        loop {
            if self.consume_word_if("EMPTY") {
                self.state.empty_members_ignored = true;
            } else {
                let positions = self.parse_position_list(dimensions)?;
                if positions.len() < 2 {
                    return Err(
                        self.invalid("a non-empty WKT LineString requires at least two positions")
                    );
                }
                lines.push(positions);
            }
            if self.consume_char_if(b',') {
                continue;
            }
            self.expect_char(b')')?;
            break;
        }
        Ok(json!({ "type": "MultiLineString", "coordinates": lines }))
    }

    fn parse_polygon(&mut self, dimensions: usize) -> Result<Value> {
        let rings = self.parse_polygon_rings(dimensions)?;
        Ok(json!({ "type": "Polygon", "coordinates": rings }))
    }

    fn parse_multipolygon(&mut self, dimensions: usize) -> Result<Value> {
        self.expect_char(b'(')?;
        let mut polygons = Vec::new();
        loop {
            if self.consume_word_if("EMPTY") {
                self.state.empty_members_ignored = true;
                polygons.push(Vec::<Vec<Value>>::new());
            } else {
                polygons.push(self.parse_polygon_rings(dimensions)?);
            }
            if self.consume_char_if(b',') {
                continue;
            }
            self.expect_char(b')')?;
            break;
        }
        Ok(json!({ "type": "MultiPolygon", "coordinates": polygons }))
    }

    fn parse_geometrycollection(&mut self, depth: usize, dimensions: usize) -> Result<Value> {
        self.expect_char(b'(')?;
        let mut geometries = Vec::new();
        loop {
            if let Some(geometry) = self.parse_geometry(depth + 1, dimensions)? {
                geometries.push(geometry);
            }
            if self.consume_char_if(b',') {
                continue;
            }
            self.expect_char(b')')?;
            break;
        }
        Ok(json!({ "type": "GeometryCollection", "geometries": geometries }))
    }

    fn parse_polygon_rings(&mut self, dimensions: usize) -> Result<Vec<Vec<Value>>> {
        self.expect_char(b'(')?;
        let mut rings = Vec::new();
        loop {
            let positions = self.parse_position_list(dimensions)?;
            if positions.len() < 3 {
                return Err(
                    self.invalid("a non-empty WKT polygon ring requires at least three positions")
                );
            }
            rings.push(positions);
            if self.consume_char_if(b',') {
                continue;
            }
            self.expect_char(b')')?;
            break;
        }
        Ok(rings)
    }

    fn parse_position_list(&mut self, dimensions: usize) -> Result<Vec<Value>> {
        self.expect_char(b'(')?;
        let mut positions = Vec::new();
        loop {
            positions.push(self.parse_position(dimensions)?);
            if self.consume_char_if(b',') {
                continue;
            }
            self.expect_char(b')')?;
            break;
        }
        Ok(positions)
    }

    fn parse_position(&mut self, dimensions: usize) -> Result<Value> {
        let x = self.parse_number()?;
        let y = self.parse_number()?;
        let position = vec![json!(x), json!(y)];
        for _ in 2..dimensions {
            let _ = self.parse_number()?;
        }
        self.state.positions = self.state.positions.saturating_add(1);
        if self.state.positions > MAX_WKT_POSITIONS {
            return Err(Error::LimitExceeded(format!(
                "WKT exceeds {MAX_WKT_POSITIONS} coordinate positions"
            )));
        }
        Ok(json!(position))
    }

    fn parse_number(&mut self) -> Result<f64> {
        let atom = self.read_atom()?;
        let value = atom
            .parse::<f64>()
            .map_err(|_| self.invalid("WKT coordinate ordinate is not numeric"))?;
        if !value.is_finite() {
            return Err(self.invalid("WKT coordinate ordinate is not finite"));
        }
        Ok(value)
    }

    fn read_word(&mut self) -> Result<&str> {
        self.skip_whitespace();
        let start = self.cursor;
        while self.cursor < self.bytes.len()
            && (self.bytes[self.cursor].is_ascii_alphabetic() || self.bytes[self.cursor] == b'_')
        {
            self.cursor += 1;
        }
        if self.cursor == start {
            return Err(self.invalid("expected a WKT geometry keyword"));
        }
        Ok(&self.text[start..self.cursor])
    }

    fn read_atom(&mut self) -> Result<&str> {
        self.skip_whitespace();
        let start = self.cursor;
        while self.cursor < self.bytes.len()
            && !self.bytes[self.cursor].is_ascii_whitespace()
            && !matches!(self.bytes[self.cursor], b'(' | b')' | b',' | b';' | b'=')
        {
            self.cursor += 1;
        }
        if self.cursor == start {
            return Err(self.invalid("expected a WKT coordinate or identifier"));
        }
        Ok(&self.text[start..self.cursor])
    }

    fn consume_word_if(&mut self, expected: &str) -> bool {
        self.skip_whitespace();
        let start = self.cursor;
        while self.cursor < self.bytes.len()
            && (self.bytes[self.cursor].is_ascii_alphabetic() || self.bytes[self.cursor] == b'_')
        {
            self.cursor += 1;
        }
        if self.cursor > start && self.text[start..self.cursor].eq_ignore_ascii_case(expected) {
            true
        } else {
            self.cursor = start;
            false
        }
    }

    fn consume_word(&mut self, expected: &str) -> Result<()> {
        if self.consume_word_if(expected) {
            Ok(())
        } else {
            Err(self.invalid(&format!("expected '{expected}'")))
        }
    }

    fn expect_char(&mut self, expected: u8) -> Result<()> {
        if self.consume_char_if(expected) {
            Ok(())
        } else {
            Err(self.invalid(&format!("expected '{}'", expected as char)))
        }
    }

    fn consume_char_if(&mut self, expected: u8) -> bool {
        self.skip_whitespace();
        if self.bytes.get(self.cursor) == Some(&expected) {
            self.cursor += 1;
            true
        } else {
            false
        }
    }

    fn skip_whitespace(&mut self) {
        while self.cursor < self.bytes.len() && self.bytes[self.cursor].is_ascii_whitespace() {
            self.cursor += 1;
        }
    }

    fn remaining_ascii(&self) -> &str {
        self.text.get(self.cursor..).unwrap_or_default()
    }

    fn invalid(&self, message: &str) -> Error {
        Error::InvalidInput(format!("invalid WKT at byte {}: {message}", self.cursor))
    }
}

trait StartsWithIgnoreAsciiCase {
    fn starts_with_ignore_ascii_case(&self, expected: &str) -> bool;
}

impl StartsWithIgnoreAsciiCase for str {
    fn starts_with_ignore_ascii_case(&self, expected: &str) -> bool {
        self.get(..expected.len())
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case(expected))
    }
}
