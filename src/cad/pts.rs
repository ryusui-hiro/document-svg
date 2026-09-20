//! Bounded Leica PTS ASCII point-cloud preview.
//!
//! PTS stores one unstructured cloud with a declared point count. The preview
//! keeps XYZ coordinates and either colors RGB points or maps intensity to
//! grayscale; it does not reconstruct scanner poses, grid order, or attributes.

use std::fmt::Write as FmtWrite;
use std::io::{Cursor, Read};

use crate::convert::{ConvertOptions, PageConsumer};
use crate::error::{Error, Result};
use crate::ir::{Node, Page};

const MAX_PTS_BYTES: u64 = 128 * 1024 * 1024;
const MAX_PTS_LINE_BYTES: usize = 1024 * 1024;
const MAX_PTS_POINTS: usize = 5_000_000;
const MAX_PTS_LINES: usize = MAX_PTS_POINTS + 1;
const MAX_PTS_PREVIEW_POINTS: usize = 200_000;
const MAX_PTS_PLY_BYTES: usize = 32 * 1024 * 1024;
const MAX_PTS_COORDINATE: f64 = 1.0e12;
const DEFAULT_POINT_COLOR: [u8; 3] = [37, 99, 235];

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RecordFormat {
    Intensity,
    IntensityRgb,
}

impl RecordFormat {
    fn from_field_count(count: usize) -> Option<Self> {
        match count {
            4 => Some(Self::Intensity),
            7 => Some(Self::IntensityRgb),
            _ => None,
        }
    }

    fn field_count(self) -> usize {
        match self {
            Self::Intensity => 4,
            Self::IntensityRgb => 7,
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct PointRecord {
    x: f64,
    y: f64,
    z: f64,
    intensity: f64,
    color: Option<[u8; 3]>,
    valid_coordinates: bool,
    valid_intensity: bool,
    valid_color: bool,
    integer_intensity: bool,
}

struct PtsPageSink<'a> {
    inner: &'a mut dyn PageConsumer,
    point_count: usize,
    warnings: &'a [String],
}

impl PageConsumer for PtsPageSink<'_> {
    fn consume(&mut self, mut page: Page) -> Result<()> {
        page.number = 1;
        page.source_format = "pts".into();
        page.title = "PTS point cloud".into();
        page.description = format!("PTS point cloud with {} sampled points", self.point_count);
        for warning in self.warnings {
            page.warn(warning.clone());
        }
        for node in &mut page.nodes {
            if let Node::Path { meta, .. } = node
                && meta.kind == "ply-point-cloud"
            {
                meta.kind = "pts-point-cloud".into();
                meta.semantic_role = "pts:point-cloud".into();
            }
        }
        self.inner.consume(page)
    }
}

pub(crate) fn looks_like_prefix(bytes: &[u8]) -> bool {
    let Ok(text) = std::str::from_utf8(bytes) else {
        return false;
    };
    let mut lines = text.lines();
    let Some(count_line) = lines.next() else {
        return false;
    };
    let Some(count) = parse_count_line(count_line) else {
        return false;
    };
    if count == 0 || count > MAX_PTS_POINTS {
        return false;
    }
    let Some(first_record) = lines.find(|line| !line.trim().is_empty()) else {
        return false;
    };
    let Some(format) = RecordFormat::from_field_count(first_record.split_whitespace().count())
    else {
        return false;
    };
    let mut values = first_record.split_whitespace();
    (0..format.field_count()).all(|_| values.next().and_then(parse_scalar).is_some())
        && values.next().is_none()
}

pub(crate) fn convert<R: Read>(
    input: R,
    options: &ConvertOptions,
    sink: &mut dyn PageConsumer,
) -> Result<Vec<String>> {
    let input_limit = options.max_input_bytes.min(MAX_PTS_BYTES);
    let mut bytes = Vec::new();
    Read::take(input, input_limit.saturating_add(1)).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > input_limit {
        return Err(Error::LimitExceeded(format!(
            "PTS input exceeds maximum limit of {input_limit} bytes"
        )));
    }
    let text = String::from_utf8(bytes)
        .map_err(|error| Error::Unsupported(format!("PTS input must be ASCII/UTF-8: {error}")))?;
    let lines = text.lines().collect::<Vec<_>>();
    if lines.len() > MAX_PTS_LINES {
        return Err(Error::LimitExceeded(format!(
            "PTS line count exceeds {MAX_PTS_LINES}"
        )));
    }
    if lines.iter().any(|line| line.len() > MAX_PTS_LINE_BYTES) {
        return Err(Error::LimitExceeded(format!(
            "PTS line exceeds {MAX_PTS_LINE_BYTES} bytes"
        )));
    }
    let count_line = lines
        .first()
        .ok_or_else(|| Error::InvalidInput("PTS file is empty".into()))?;
    let declared_count = parse_count_line(count_line)
        .ok_or_else(|| Error::InvalidInput("PTS first line must declare a point count".into()))?;
    if declared_count == 0 {
        return Err(Error::InvalidInput(
            "PTS point count must be positive".into(),
        ));
    }
    if declared_count > MAX_PTS_POINTS {
        return Err(Error::LimitExceeded(format!(
            "PTS declares {declared_count} points; maximum is {MAX_PTS_POINTS}"
        )));
    }
    if options.max_pages == 0 {
        return Err(Error::LimitExceeded(
            "PTS conversion requires one output page, but max_pages is zero".into(),
        ));
    }
    let point_line_count = lines
        .iter()
        .skip(1)
        .filter(|line| !line.trim().is_empty())
        .count();
    if point_line_count != declared_count {
        return Err(Error::InvalidInput(format!(
            "PTS declares {declared_count} points but contains {point_line_count} non-empty point records"
        )));
    }
    let first_record = lines
        .iter()
        .skip(1)
        .find(|line| !line.trim().is_empty())
        .copied()
        .ok_or_else(|| Error::InvalidInput("PTS contains no point records".into()))?;
    let record_format = RecordFormat::from_field_count(first_record.split_whitespace().count())
        .ok_or_else(|| {
            Error::Unsupported(
                "PTS records must have four XYZ/intensity fields or seven XYZ/intensity/RGB fields"
                    .into(),
            )
        })?;
    let numeric_fields = declared_count
        .checked_mul(record_format.field_count())
        .ok_or_else(|| Error::LimitExceeded("PTS numeric field count overflowed".into()))?;
    if numeric_fields > MAX_PTS_POINTS * 7 {
        return Err(Error::LimitExceeded(
            "PTS numeric field limit exceeded".into(),
        ));
    }

    let mut all_integer_intensity = record_format == RecordFormat::Intensity;
    let mut invalid_points = 0usize;
    let mut invalid_colors = 0usize;
    let mut legacy_intensity_out_of_range = 0usize;
    let mut normalized_intensity_out_of_range = 0usize;
    for (point_index, line) in lines
        .iter()
        .skip(1)
        .filter(|line| !line.trim().is_empty())
        .copied()
        .enumerate()
    {
        let record = parse_record(line, record_format, point_index)?;
        if !record.valid_coordinates {
            invalid_points += 1;
        }
        if !record.valid_color {
            invalid_colors += 1;
        }
        if record_format == RecordFormat::Intensity {
            all_integer_intensity &= record.integer_intensity;
            if !record.valid_intensity || !(-2048.0..=2047.0).contains(&record.intensity) {
                legacy_intensity_out_of_range += 1;
            }
            if !record.valid_intensity || !(0.0..=1.0).contains(&record.intensity) {
                normalized_intensity_out_of_range += 1;
            }
        }
    }
    let legacy_intensity = record_format == RecordFormat::Intensity && all_integer_intensity;
    let out_of_range_intensity = if legacy_intensity {
        legacy_intensity_out_of_range
    } else {
        normalized_intensity_out_of_range
    };

    let quota = declared_count.min(MAX_PTS_PREVIEW_POINTS);
    let mut warnings = vec![
        "PTS is an unstructured point cloud; scanner pose and scan-grid order are not available"
            .into(),
    ];
    if declared_count > MAX_PTS_PREVIEW_POINTS {
        warnings.push(format!(
            "PTS contains {declared_count} points; a deterministic sample of at most {MAX_PTS_PREVIEW_POINTS} points was rendered"
        ));
    }
    if invalid_points > 0 {
        warnings.push(format!(
            "{invalid_points} non-finite or out-of-range PTS points were omitted"
        ));
    }
    if invalid_colors > 0 {
        warnings.push(format!(
            "{invalid_colors} PTS RGB values are invalid or mark no color; a blue fallback is used"
        ));
    }
    if out_of_range_intensity > 0 {
        warnings.push(format!(
            "{out_of_range_intensity} PTS intensity value(s) outside the supported range were clamped for grayscale preview"
        ));
    }
    if record_format == RecordFormat::IntensityRgb {
        warnings.push("PTS intensity scalar values are not rendered in RGB point clouds".into());
    }

    let mut selected_count = 0usize;
    let mut next_selected = 0usize;
    let mut rendered = 0usize;
    let mut ply = String::new();
    for (point_index, line) in lines
        .iter()
        .skip(1)
        .filter(|line| !line.trim().is_empty())
        .copied()
        .enumerate()
    {
        let selected = point_index == next_selected;
        if selected {
            let record = parse_record(line, record_format, point_index)?;
            if record.valid_coordinates {
                let color = record.color.unwrap_or_else(|| match record_format {
                    RecordFormat::Intensity => intensity_gray(record.intensity, legacy_intensity),
                    RecordFormat::IntensityRgb => DEFAULT_POINT_COLOR,
                });
                let point_line = format!(
                    "{:.9e} {:.9e} {:.9e} {} {} {}\n",
                    record.x, record.y, record.z, color[0], color[1], color[2]
                );
                if ply.len().saturating_add(point_line.len()) > MAX_PTS_PLY_BYTES {
                    return Err(Error::LimitExceeded(format!(
                        "PTS PLY intermediate exceeds {MAX_PTS_PLY_BYTES} bytes"
                    )));
                }
                ply.push_str(&point_line);
                rendered += 1;
            }
            selected_count += 1;
            next_selected = crate::cad::next_sample_index(selected_count, declared_count, quota);
        }
    }
    if rendered == 0 {
        return Err(Error::Unsupported(
            "PTS contains no finite, in-range coordinates".into(),
        ));
    }

    let mut ply_text = String::with_capacity(ply.len().saturating_add(128));
    writeln!(ply_text, "ply\nformat ascii 1.0\nelement vertex {rendered}")
        .map_err(|_| Error::InvalidInput("could not write PTS PLY header".into()))?;
    ply_text.push_str(
        "property double x\nproperty double y\nproperty double z\nproperty uchar red\nproperty uchar green\nproperty uchar blue\nend_header\n",
    );
    ply_text.push_str(&ply);
    let mut page_sink = PtsPageSink {
        inner: sink,
        point_count: rendered,
        warnings: &warnings,
    };
    crate::cad::ply::convert(Cursor::new(ply_text.as_bytes()), options, &mut page_sink)?;
    Ok(warnings)
}

fn parse_record(line: &str, format: RecordFormat, point_index: usize) -> Result<PointRecord> {
    let mut tokens = [""; 7];
    let mut split = line.split_whitespace();
    for token in tokens.iter_mut().take(format.field_count()) {
        *token = split.next().ok_or_else(|| {
            Error::InvalidInput(format!("PTS point {} has too few fields", point_index + 1))
        })?;
    }
    if split.next().is_some() {
        return Err(Error::InvalidInput(format!(
            "PTS point {} has too many fields",
            point_index + 1
        )));
    }
    let mut values = [0.0; 7];
    for (index, token) in tokens.iter().take(format.field_count()).enumerate() {
        values[index] = parse_scalar(token).ok_or_else(|| {
            Error::InvalidInput(format!(
                "PTS point {} has an invalid numeric value",
                point_index + 1
            ))
        })?;
    }
    let color = if format == RecordFormat::IntensityRgb {
        Some(parse_color(
            [values[4], values[5], values[6]],
            &tokens[4..7],
        ))
    } else {
        None
    };
    Ok(PointRecord {
        x: values[0],
        y: values[1],
        z: values[2],
        intensity: values[3],
        color: color.and_then(|(color, valid)| valid.then_some(color)),
        valid_coordinates: [values[0], values[1], values[2]]
            .iter()
            .all(|value| value.is_finite() && value.abs() <= MAX_PTS_COORDINATE),
        valid_intensity: values[3].is_finite(),
        valid_color: color.is_none_or(|(_, valid)| valid),
        integer_intensity: tokens[3].parse::<i32>().is_ok(),
    })
}

fn parse_color(values: [f64; 3], lexical: &[&str]) -> ([u8; 3], bool) {
    let valid = values
        .iter()
        .all(|value| value.is_finite() && (0.0..=255.0).contains(value));
    let normalized = values.iter().all(|value| (0.0..=1.0).contains(value))
        && lexical
            .iter()
            .any(|value| value.contains('.') || value.contains('e') || value.contains('E'));
    let byte_encoded = values.iter().all(|value| value.fract() == 0.0);
    if !valid || !(normalized || byte_encoded) {
        return (DEFAULT_POINT_COLOR, false);
    }
    let color = values.map(|value| {
        let value = if normalized { value * 255.0 } else { value };
        value.round() as u8
    });
    if color == [0, 0, 0] {
        return (DEFAULT_POINT_COLOR, false);
    }
    (color, true)
}

fn intensity_gray(value: f64, legacy_integer: bool) -> [u8; 3] {
    let normalized = if !value.is_finite() {
        0.0
    } else if legacy_integer {
        (value.clamp(-2048.0, 2047.0) + 2048.0) / 4096.0
    } else {
        value.clamp(0.0, 1.0)
    };
    let gray = (normalized * 255.0).round() as u8;
    [gray, gray, gray]
}

fn parse_count_line(line: &str) -> Option<usize> {
    if line.len() > MAX_PTS_LINE_BYTES {
        return None;
    }
    let mut tokens = line.trim_start_matches('\u{feff}').split_whitespace();
    let value = tokens.next()?.parse::<usize>().ok()?;
    tokens.next().is_none().then_some(value)
}

fn parse_scalar(token: &str) -> Option<f64> {
    let normalized;
    let token = if token.contains(',') {
        normalized = token.replace(',', ".");
        normalized.as_str()
    } else {
        token
    };
    token.parse::<f64>().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognizes_leica_point_count_and_records() {
        assert!(looks_like_prefix(b"2\n0 0 0 0.5\n1 2 3 0.7\n"));
        assert!(looks_like_prefix(
            b"2\n0 0 0 0.5 255 0 0\n1 2 3 0.7 0 255 0\n"
        ));
        assert!(!looks_like_prefix(b"2\n0 0 0\n"));
        assert_eq!(parse_count_line("\u{feff}17"), Some(17));
    }

    #[test]
    fn decodes_signed_leica_intensity_and_normalized_rgb() {
        assert_eq!(intensity_gray(-2048.0, true), [0, 0, 0]);
        assert_eq!(intensity_gray(0.0, true), [128, 128, 128]);
        assert_eq!(intensity_gray(2047.0, true), [255, 255, 255]);
        assert_eq!(
            parse_color([0.5, 0.25, 1.0], &["0.5", "0.25", "1.0"]),
            ([128, 64, 255], true)
        );
        assert_eq!(
            parse_color([0.0, 0.0, 0.0], &["0", "0", "0"]),
            (DEFAULT_POINT_COLOR, false)
        );
    }

    #[test]
    fn point_sampling_uses_evenly_spaced_full_quota() {
        let total = 10;
        let quota = 4;
        let mut selected_count = 0;
        let mut next_selected = 0;
        let mut indexes = Vec::new();
        for index in 0..total {
            if index == next_selected {
                indexes.push(index);
                selected_count += 1;
                next_selected = crate::cad::next_sample_index(selected_count, total, quota);
            }
        }
        assert_eq!(indexes, [0, 2, 5, 7]);
    }
}
