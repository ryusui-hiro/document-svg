//! Bounded ASCII XYZ point-cloud preview for common whitespace/comma layouts.
//!
//! The format has no universal schema or coordinate metadata. This reader
//! accepts homogeneous numeric XYZ/XYZI/RGB/normal layouts, optionally maps a
//! small set of common header names, and preserves coordinates as stored.

use std::fmt::Write as FmtWrite;
use std::io::{Cursor, Read};

use crate::convert::{ConvertOptions, PageConsumer};
use crate::error::{Error, Result};
use crate::ir::{Node, Page};

const MAX_XYZ_BYTES: u64 = 128 * 1024 * 1024;
const MAX_XYZ_LINE_BYTES: usize = 1024 * 1024;
const MAX_XYZ_POINTS: usize = 5_000_000;
const MAX_XYZ_LINES: usize = MAX_XYZ_POINTS + 1;
const MAX_XYZ_COLUMNS: usize = 128;
const MAX_XYZ_PREVIEW_POINTS: usize = 200_000;
const MAX_XYZ_PLY_BYTES: usize = 32 * 1024 * 1024;
const MAX_XYZ_COORDINATE: f64 = 1.0e12;
const DEFAULT_POINT_COLOR: [u8; 3] = [37, 99, 235];

#[derive(Clone, Copy, Debug)]
struct RecordLayout {
    field_count: usize,
    x: usize,
    y: usize,
    z: usize,
    intensity: Option<usize>,
    rgb: Option<[usize; 3]>,
    normals: bool,
    ignored_columns: usize,
    headered: bool,
}

impl RecordLayout {
    fn positional(field_count: usize) -> Option<Self> {
        let (intensity, rgb, normals) = match field_count {
            3 => (None, None, false),
            4 => (Some(3), None, false),
            6 => (None, Some([3, 4, 5]), false),
            7 => (Some(3), Some([4, 5, 6]), false),
            9 => (None, None, true),
            10 => (Some(3), Some([4, 5, 6]), true),
            _ => return None,
        };
        Some(Self {
            field_count,
            x: 0,
            y: 1,
            z: 2,
            intensity,
            rgb,
            normals,
            ignored_columns: 0,
            headered: false,
        })
    }

    fn from_header(names: &[&str]) -> Result<Self> {
        if names.len() > MAX_XYZ_COLUMNS {
            return Err(Error::LimitExceeded(format!(
                "XYZ column header exceeds {MAX_XYZ_COLUMNS} fields"
            )));
        }
        let x = find_column(names, &["x"], "X")?
            .ok_or_else(|| Error::Unsupported("XYZ header must identify an X column".into()))?;
        let y = find_column(names, &["y"], "Y")?
            .ok_or_else(|| Error::Unsupported("XYZ header must identify a Y column".into()))?;
        let z = find_column(names, &["z"], "Z")?
            .ok_or_else(|| Error::Unsupported("XYZ header must identify a Z column".into()))?;
        let intensity = find_column(
            names,
            &["i", "intensity", "reflectance", "scalar"],
            "intensity",
        )?;
        let red = find_column(names, &["r", "red"], "red")?;
        let green = find_column(names, &["g", "green"], "green")?;
        let blue = find_column(names, &["b", "blue"], "blue")?;
        let rgb_count = [red, green, blue]
            .iter()
            .filter(|index| index.is_some())
            .count();
        if rgb_count != 0 && rgb_count != 3 {
            return Err(Error::InvalidInput(
                "XYZ header must contain all three red, green and blue columns".into(),
            ));
        }
        let nx = find_column(names, &["nx", "normal_x", "normalx"], "normal X")?;
        let ny = find_column(names, &["ny", "normal_y", "normaly"], "normal Y")?;
        let nz = find_column(names, &["nz", "normal_z", "normalz"], "normal Z")?;
        let normal_count = [nx, ny, nz].iter().filter(|index| index.is_some()).count();
        if normal_count != 0 && normal_count != 3 {
            return Err(Error::InvalidInput(
                "XYZ header must contain all three normal columns".into(),
            ));
        }
        let recognized = 3 + usize::from(intensity.is_some()) + rgb_count + normal_count;
        let rgb = match (red, green, blue) {
            (Some(red), Some(green), Some(blue)) => Some([red, green, blue]),
            _ => None,
        };
        Ok(Self {
            field_count: names.len(),
            x,
            y,
            z,
            intensity,
            rgb,
            normals: normal_count == 3,
            ignored_columns: names.len().saturating_sub(recognized),
            headered: true,
        })
    }

    fn has_intensity(self) -> bool {
        self.intensity.is_some()
    }
}

fn find_column(names: &[&str], aliases: &[&str], label: &str) -> Result<Option<usize>> {
    let mut found = None;
    for (index, name) in names.iter().enumerate() {
        let name = name.trim().trim_matches('"').trim_matches('\'');
        if aliases.iter().any(|alias| name.eq_ignore_ascii_case(alias))
            && found.replace(index).is_some()
        {
            return Err(Error::InvalidInput(format!(
                "XYZ header has duplicate {label} columns"
            )));
        }
    }
    Ok(found)
}

#[derive(Clone, Copy, Debug)]
struct PointRecord {
    coordinates: [f64; 3],
    intensity: Option<f64>,
    rgb: Option<[u8; 3]>,
    valid_coordinates: bool,
    valid_intensity: bool,
    valid_rgb: bool,
}

struct XyzPageSink<'a> {
    inner: &'a mut dyn PageConsumer,
    point_count: usize,
    warnings: &'a [String],
}

impl PageConsumer for XyzPageSink<'_> {
    fn consume(&mut self, mut page: Page) -> Result<()> {
        page.number = 1;
        page.source_format = "xyz".into();
        page.title = "XYZ point cloud".into();
        page.description = format!("XYZ point cloud with {} sampled points", self.point_count);
        for warning in self.warnings {
            page.warn(warning.clone());
        }
        for node in &mut page.nodes {
            if let Node::Path { meta, .. } = node
                && meta.kind == "ply-point-cloud"
            {
                meta.kind = "xyz-point-cloud".into();
                meta.semantic_role = "xyz:point-cloud".into();
            }
        }
        self.inner.consume(page)
    }
}

pub(crate) fn looks_like_prefix(bytes: &[u8]) -> bool {
    let Ok(text) = std::str::from_utf8(bytes) else {
        return false;
    };
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let rows = text
        .lines()
        .map(str::trim)
        .filter(|line| !is_ignored_line(line))
        .collect::<Vec<_>>();
    let Some(first_line) = rows.first() else {
        return false;
    };
    let first_fields = fields(first_line).collect::<Vec<_>>();
    let headered = first_fields
        .iter()
        .any(|field| parse_scalar(field).is_none());
    let (layout, data_start) = if headered {
        let Ok(layout) = RecordLayout::from_header(&first_fields) else {
            return false;
        };
        (layout, 1)
    } else {
        let Some(layout) = RecordLayout::positional(first_fields.len()) else {
            return false;
        };
        if first_fields.len() != 3 {
            return false;
        }
        (layout, 0)
    };
    if rows.len().saturating_sub(data_start) < 2 {
        return false;
    }
    for line in rows.iter().skip(data_start).take(2) {
        let values = fields(line).collect::<Vec<_>>();
        if values.len() != layout.field_count {
            return false;
        }
        for coordinate in [layout.x, layout.y, layout.z] {
            let Some(value) = parse_scalar(values[coordinate]) else {
                return false;
            };
            if !value.is_finite() || value.abs() > MAX_XYZ_COORDINATE {
                return false;
            }
        }
    }
    true
}

pub(crate) fn convert<R: Read>(
    input: R,
    options: &ConvertOptions,
    sink: &mut dyn PageConsumer,
) -> Result<Vec<String>> {
    let input_limit = options.max_input_bytes.min(MAX_XYZ_BYTES);
    let mut bytes = Vec::new();
    Read::take(input, input_limit.saturating_add(1)).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > input_limit {
        return Err(Error::LimitExceeded(format!(
            "XYZ input exceeds maximum limit of {input_limit} bytes"
        )));
    }
    let text = String::from_utf8(bytes)
        .map_err(|error| Error::Unsupported(format!("XYZ input must be ASCII/UTF-8: {error}")))?;
    let text = text.strip_prefix('\u{feff}').unwrap_or(&text);
    let lines = text.lines().collect::<Vec<_>>();
    if lines.len() > MAX_XYZ_LINES {
        return Err(Error::LimitExceeded(format!(
            "XYZ line count exceeds {MAX_XYZ_LINES}"
        )));
    }
    if lines.iter().any(|line| line.len() > MAX_XYZ_LINE_BYTES) {
        return Err(Error::LimitExceeded(format!(
            "XYZ line exceeds {MAX_XYZ_LINE_BYTES} bytes"
        )));
    }
    if options.max_pages == 0 {
        return Err(Error::LimitExceeded(
            "XYZ conversion requires one output page, but max_pages is zero".into(),
        ));
    }
    let first_index = lines
        .iter()
        .position(|line| !is_ignored_line(line))
        .ok_or_else(|| Error::InvalidInput("XYZ file contains no point records".into()))?;
    let first_line = lines[first_index].trim();
    let first_fields = fields(first_line).collect::<Vec<_>>();
    let headered = first_fields
        .iter()
        .any(|field| parse_scalar(field).is_none());
    let (layout, data_start) = if headered {
        (RecordLayout::from_header(&first_fields)?, first_index + 1)
    } else {
        let layout = RecordLayout::positional(first_fields.len()).ok_or_else(|| {
            Error::Unsupported(format!(
                "headerless XYZ records must have 3, 4, 6, 7, 9, or 10 fields; first record has {}",
                first_fields.len()
            ))
        })?;
        (layout, first_index)
    };
    let data_line_count = lines
        .iter()
        .skip(data_start)
        .filter(|line| !is_ignored_line(line))
        .count();
    if data_line_count == 0 {
        return Err(Error::InvalidInput(
            "XYZ file contains no point records".into(),
        ));
    }
    if data_line_count > MAX_XYZ_POINTS {
        return Err(Error::LimitExceeded(format!(
            "XYZ contains {data_line_count} points; maximum is {MAX_XYZ_POINTS}"
        )));
    }
    let numeric_field_count = data_line_count
        .checked_mul(layout.field_count)
        .ok_or_else(|| Error::LimitExceeded("XYZ numeric field count overflowed".into()))?;
    if numeric_field_count > MAX_XYZ_POINTS * 10 {
        return Err(Error::LimitExceeded(
            "XYZ numeric field limit exceeded".into(),
        ));
    }

    let mut invalid_points = 0usize;
    let mut invalid_intensity = 0usize;
    let mut invalid_colors = 0usize;
    let mut intensity_min = f64::INFINITY;
    let mut intensity_max = f64::NEG_INFINITY;
    for (point_index, line) in lines
        .iter()
        .skip(data_start)
        .map(|line| line.trim())
        .filter(|line| !is_ignored_line(line))
        .enumerate()
    {
        let record = parse_record(line, layout, point_index)?;
        if !record.valid_coordinates {
            invalid_points += 1;
        } else {
            if !record.valid_rgb {
                invalid_colors += 1;
            }
            if layout.has_intensity() && !record.valid_intensity {
                invalid_intensity += 1;
            } else if let Some(intensity) = record.intensity {
                intensity_min = intensity_min.min(intensity);
                intensity_max = intensity_max.max(intensity);
            }
        }
    }

    let quota = data_line_count.min(MAX_XYZ_PREVIEW_POINTS);
    let mut warnings = vec![if layout.headered {
        "XYZ named columns were mapped for supported coordinates/colors/intensity; units, CRS and scanner pose are not inferred".into()
    } else {
        "XYZ has no standard header; coordinate units, CRS, scanner pose and column meaning are inferred from the supported record width and coordinates are kept as stored".into()
    }];
    if layout.ignored_columns > 0 {
        warnings.push(format!(
            "{} unrecognized XYZ header column(s) are ignored",
            layout.ignored_columns
        ));
    }
    if layout.has_intensity() {
        warnings.push(
            "XYZ intensity values are linearly scaled from their finite observed minimum and maximum for grayscale preview".into(),
        );
    }
    if data_line_count > MAX_XYZ_PREVIEW_POINTS {
        warnings.push(format!(
            "XYZ contains {data_line_count} points; a deterministic sample of at most {MAX_XYZ_PREVIEW_POINTS} points was rendered"
        ));
    }
    if invalid_points > 0 {
        warnings.push(format!(
            "{invalid_points} non-finite or out-of-range XYZ coordinates were omitted"
        ));
    }
    if invalid_intensity > 0 {
        warnings.push(format!(
            "{invalid_intensity} non-finite XYZ intensity value(s) were ignored; RGB is retained where present and other points use blue"
        ));
    }
    if invalid_colors > 0 {
        warnings.push(format!(
            "{invalid_colors} invalid XYZ RGB value(s) were replaced with intensity grayscale when available, otherwise blue"
        ));
    }
    if layout.normals {
        warnings.push("XYZ normal vectors are not rendered".into());
    }

    let mut selected_count = 0usize;
    let mut next_selected = 0usize;
    let mut rendered = 0usize;
    let mut ply = String::new();
    for (point_index, line) in lines
        .iter()
        .skip(data_start)
        .map(|line| line.trim())
        .filter(|line| !is_ignored_line(line))
        .enumerate()
    {
        if point_index != next_selected {
            continue;
        }
        let record = parse_record(line, layout, point_index)?;
        selected_count += 1;
        next_selected = crate::cad::next_sample_index(selected_count, data_line_count, quota);
        if !record.valid_coordinates {
            continue;
        }
        let color = if let Some(rgb) = record.rgb {
            rgb
        } else if layout.has_intensity() {
            if record.valid_intensity {
                record
                    .intensity
                    .map(|value| intensity_gray(value, intensity_min, intensity_max))
                    .unwrap_or(DEFAULT_POINT_COLOR)
            } else {
                DEFAULT_POINT_COLOR
            }
        } else {
            DEFAULT_POINT_COLOR
        };
        let [x, y, z] = record.coordinates;
        let point_line = format!(
            "{x:.9e} {y:.9e} {z:.9e} {} {} {}\n",
            color[0], color[1], color[2]
        );
        if ply.len().saturating_add(point_line.len()) > MAX_XYZ_PLY_BYTES {
            return Err(Error::LimitExceeded(format!(
                "XYZ PLY intermediate exceeds {MAX_XYZ_PLY_BYTES} bytes"
            )));
        }
        ply.push_str(&point_line);
        rendered += 1;
    }
    if rendered == 0 {
        return Err(Error::Unsupported(
            "XYZ file contains no finite, in-range coordinates".into(),
        ));
    }

    let mut ply_text = String::with_capacity(ply.len().saturating_add(128));
    writeln!(ply_text, "ply\nformat ascii 1.0\nelement vertex {rendered}")
        .map_err(|_| Error::InvalidInput("could not write XYZ PLY header".into()))?;
    ply_text.push_str(
        "property double x\nproperty double y\nproperty double z\nproperty uchar red\nproperty uchar green\nproperty uchar blue\nend_header\n",
    );
    ply_text.push_str(&ply);
    let mut page_sink = XyzPageSink {
        inner: sink,
        point_count: rendered,
        warnings: &warnings,
    };
    crate::cad::ply::convert(Cursor::new(ply_text.as_bytes()), options, &mut page_sink)?;
    Ok(warnings)
}

fn parse_record(line: &str, layout: RecordLayout, point_index: usize) -> Result<PointRecord> {
    let tokens = fields(line).collect::<Vec<_>>();
    if tokens.len() != layout.field_count {
        return Err(Error::InvalidInput(format!(
            "XYZ point {} has {} fields; expected {}",
            point_index + 1,
            tokens.len(),
            layout.field_count
        )));
    }
    let parse_at = |index: usize| {
        parse_scalar(tokens[index]).ok_or_else(|| {
            Error::InvalidInput(format!(
                "XYZ point {} has an invalid numeric value in column {}",
                point_index + 1,
                index + 1
            ))
        })
    };
    let coordinates = [
        parse_at(layout.x)?,
        parse_at(layout.y)?,
        parse_at(layout.z)?,
    ];
    let intensity = layout.intensity.map(parse_at).transpose()?;
    let (rgb, valid_rgb) = if let Some(indices) = layout.rgb {
        let values = [
            parse_at(indices[0])?,
            parse_at(indices[1])?,
            parse_at(indices[2])?,
        ];
        let lexical = [tokens[indices[0]], tokens[indices[1]], tokens[indices[2]]];
        parse_rgb(values, &lexical)
    } else {
        (None, true)
    };
    Ok(PointRecord {
        coordinates,
        intensity,
        rgb,
        valid_coordinates: coordinates
            .iter()
            .all(|value| value.is_finite() && value.abs() <= MAX_XYZ_COORDINATE),
        valid_intensity: intensity.is_none_or(f64::is_finite),
        valid_rgb,
    })
}

fn parse_rgb(values: [f64; 3], lexical: &[&str]) -> (Option<[u8; 3]>, bool) {
    let valid = values
        .iter()
        .all(|value| value.is_finite() && (0.0..=255.0).contains(value));
    let normalized = values.iter().all(|value| (0.0..=1.0).contains(value))
        && lexical
            .iter()
            .any(|value| value.contains('.') || value.contains('e') || value.contains('E'));
    let integer_bytes = values.iter().all(|value| value.fract() == 0.0);
    if !valid || !(normalized || integer_bytes) {
        return (None, false);
    }
    let color = values.map(|value| {
        let value = if normalized { value * 255.0 } else { value };
        value.round() as u8
    });
    (Some(color), true)
}

fn intensity_gray(value: f64, minimum: f64, maximum: f64) -> [u8; 3] {
    let gray = if !value.is_finite() {
        0
    } else if minimum.is_finite() && maximum.is_finite() && maximum > minimum {
        (((value - minimum) / (maximum - minimum)).clamp(0.0, 1.0) * 255.0).round() as u8
    } else {
        128
    };
    [gray; 3]
}

fn fields(line: &str) -> impl Iterator<Item = &str> {
    line.split(|character: char| character.is_ascii_whitespace() || character == ',')
        .filter(|field| !field.is_empty())
}

fn is_ignored_line(line: &str) -> bool {
    let line = line.trim();
    line.is_empty() || line.starts_with('#') || line.starts_with("//")
}

fn parse_scalar(token: &str) -> Option<f64> {
    token.parse::<f64>().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sniffs_headerless_whitespace_xyz_rows() {
        assert!(looks_like_prefix(b"# scan points\n0 0 0\n1 0 0\n"));
        assert!(looks_like_prefix(b"\xEF\xBB\xBF0 0 0\n1 0 0\n"));
        assert!(looks_like_prefix(
            b"X,Red,Z,Y,Intensity,Class,Blue,Green\n0,255,0,0,0,origin,0,0\n0,0,0,1,0,building,0,255\n"
        ));
        assert!(!looks_like_prefix(b"1\n2\n3\n"));
    }

    #[test]
    fn maps_named_xyz_columns_and_skips_extra_text_columns() {
        let layout = RecordLayout::from_header(&[
            "Z",
            "Red",
            "X",
            "Class",
            "Green",
            "Intensity",
            "Y",
            "Blue",
        ])
        .unwrap();
        assert_eq!((layout.x, layout.y, layout.z), (2, 6, 0));
        assert_eq!(layout.intensity, Some(5));
        assert_eq!(layout.rgb, Some([1, 4, 7]));
        assert_eq!(layout.ignored_columns, 1);
        let point = parse_record("0,255,0,origin,0,0.0,0,0", layout, 0).unwrap();
        assert_eq!(point.coordinates, [0.0, 0.0, 0.0]);
        assert_eq!(point.rgb, Some([255, 0, 0]));
        assert!(RecordLayout::from_header(&["X", "x", "Y", "Z"]).is_err());
    }

    #[test]
    fn keeps_valid_black_rgb_and_normalizes_decimal_channels() {
        assert_eq!(
            parse_rgb([0.0, 0.0, 0.0], &["0", "0", "0"]),
            (Some([0, 0, 0]), true)
        );
        assert_eq!(
            parse_rgb([0.5, 0.25, 1.0], &["0.5", "0.25", "1.0"]),
            (Some([128, 64, 255]), true)
        );
        assert_eq!(intensity_gray(0.5, 0.0, 1.0), [128, 128, 128]);
    }
}
