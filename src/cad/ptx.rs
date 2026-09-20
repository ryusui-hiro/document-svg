//! Bounded Leica PTX structured point-cloud preview.
//!
//! PTX is an ASCII, scan-organized point cloud. Each scan carries sensor and
//! cloud transforms followed by a column/row grid of XYZ, intensity, and
//! optional RGB/normals values. Preview point sampling is deterministic and
//! bounded; no scan registration or sensor geometry is recomputed.

use std::fmt::Write as FmtWrite;
use std::io::{Cursor, Read};

use crate::convert::{ConvertOptions, PageConsumer};
use crate::error::{Error, Result};
use crate::ir::{Node, Page};

const MAX_PTX_BYTES: u64 = 128 * 1024 * 1024;
const MAX_PTX_LINE_BYTES: usize = 1024 * 1024;
const MAX_PTX_LINES: usize = 5_000_000;
const MAX_PTX_SCANS: usize = 10_000;
const MAX_PTX_CELLS_PER_SCAN: usize = 2_000_000;
const MAX_PTX_CELLS_TOTAL: usize = 2_000_000;
const MAX_PTX_PREVIEW_POINTS_TOTAL: usize = 200_000;
const MAX_PTX_TOKENS: usize = 24_000_000;
const MAX_PTX_PLY_BYTES: usize = 32 * 1024 * 1024;
const MAX_PTX_COORDINATE: f64 = 1.0e12;
const DEFAULT_POINT_COLOR: [u8; 3] = [37, 99, 235];

#[derive(Clone, Copy, Debug)]
struct Vec3 {
    x: f64,
    y: f64,
    z: f64,
}

impl Vec3 {
    fn dot(self, other: Self) -> f64 {
        self.x * other.x + self.y * other.y + self.z * other.z
    }

    fn cross(self, other: Self) -> Self {
        Self {
            x: self.y * other.z - self.z * other.y,
            y: self.z * other.x - self.x * other.z,
            z: self.x * other.y - self.y * other.x,
        }
    }

    fn scale(self, scalar: f64) -> Self {
        Self {
            x: self.x * scalar,
            y: self.y * scalar,
            z: self.z * scalar,
        }
    }

    fn sub(self, other: Self) -> Self {
        Self {
            x: self.x - other.x,
            y: self.y - other.y,
            z: self.z - other.z,
        }
    }

    fn normalized(self) -> Option<Self> {
        let length = self.dot(self).sqrt();
        (length.is_finite() && length > 1e-12).then(|| self.scale(1.0 / length))
    }

    fn finite_in_range(self) -> bool {
        [self.x, self.y, self.z]
            .iter()
            .all(|value| value.is_finite() && value.abs() <= MAX_PTX_COORDINATE)
    }
}

#[derive(Clone, Copy, Debug)]
struct Transform {
    x_axis: Vec3,
    y_axis: Vec3,
    z_axis: Vec3,
    translation: Vec3,
    corrected_axes: bool,
}

impl Transform {
    fn apply(self, point: Vec3) -> Vec3 {
        self.x_axis
            .scale(point.x)
            .add(self.y_axis.scale(point.y))
            .add(self.z_axis.scale(point.z))
            .add(self.translation)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_cloud_matrix_columns_and_applies_the_registered_pose() {
        let matrix = [
            [0.0, 1.0, 0.0, 0.0],
            [-1.0, 0.0, 0.0, 0.0],
            [0.0, 0.0, 1.0, 0.0],
            [10.0, 20.0, 30.0, 1.0],
        ];
        let transform = Transform::from_ptx_columns(matrix).unwrap();
        let point = transform.apply(Vec3 {
            x: 1.0,
            y: 0.0,
            z: 0.0,
        });
        assert!((point.x - 10.0).abs() < 1e-9);
        assert!((point.y - 21.0).abs() < 1e-9);
        assert!((point.z - 30.0).abs() < 1e-9);
        assert!(!transform.corrected_axes);
    }

    #[test]
    fn detects_sample_scans_and_decodes_ptx_color_encodings() {
        let sample = include_bytes!("../../tests/fixtures/sample.ptx");
        assert!(looks_like_prefix(sample));
        assert_eq!(parse_count_line("\u{feff}3"), Some(3));
        assert_eq!(
            parse_ptx_color([0.5, 0.25, 1.0], &["0.5", "0.25", "1.0"]),
            ([128, 64, 255], true)
        );
        assert_eq!(
            parse_ptx_color([1.0, 0.0, 7.0], &["1", "0", "7"]),
            ([1, 0, 7], true)
        );
    }

    #[test]
    fn treats_leica_reserved_black_rgb_as_missing_color() {
        assert_eq!(
            parse_ptx_color([0.0, 0.0, 0.0], &["0", "0", "0"]),
            (DEFAULT_POINT_COLOR, false)
        );
    }

    #[test]
    fn deterministic_sampling_selects_the_full_quota() {
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

impl Vec3 {
    fn add(self, other: Self) -> Self {
        Self {
            x: self.x + other.x,
            y: self.y + other.y,
            z: self.z + other.z,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RecordFormat {
    Intensity,
    Rgb,
    RgbNormals,
}

impl RecordFormat {
    fn from_field_count(count: usize) -> Option<Self> {
        match count {
            4 => Some(Self::Intensity),
            7 => Some(Self::Rgb),
            10 => Some(Self::RgbNormals),
            _ => None,
        }
    }

    fn field_count(self) -> usize {
        match self {
            Self::Intensity => 4,
            Self::Rgb => 7,
            Self::RgbNormals => 10,
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct PointRecord {
    format: RecordFormat,
    coordinates: Vec3,
    intensity: f64,
    color: Option<[u8; 3]>,
    valid_coordinates: bool,
    valid_color: bool,
    valid_intensity: bool,
}

#[derive(Clone, Debug)]
struct Scan {
    columns: usize,
    rows: usize,
    cells: usize,
    point_start: usize,
    record_format: RecordFormat,
    transform: Transform,
    invalid_points: usize,
    invalid_colors: usize,
    out_of_range_intensity: usize,
}

struct PtxPageSink<'a> {
    inner: &'a mut dyn PageConsumer,
    page_number: usize,
    scan_number: usize,
    columns: usize,
    rows: usize,
    point_count: usize,
    warnings: &'a [String],
}

impl PageConsumer for PtxPageSink<'_> {
    fn consume(&mut self, mut page: Page) -> Result<()> {
        page.number = self.page_number;
        page.source_format = "ptx".into();
        page.title = format!("PTX scan {}", self.scan_number);
        page.description = format!(
            "PTX structured point cloud scan {} ({}×{}) with {} sampled points",
            self.scan_number, self.columns, self.rows, self.point_count
        );
        for warning in self.warnings {
            page.warn(warning.clone());
        }
        for node in &mut page.nodes {
            if let Node::Path { meta, .. } = node
                && meta.kind == "ply-point-cloud"
            {
                meta.kind = "ptx-point-cloud".into();
                meta.semantic_role = "ptx:point-cloud".into();
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
    let Some(columns) = lines.next().and_then(parse_count_line) else {
        return false;
    };
    let Some(rows) = lines.next().and_then(parse_count_line) else {
        return false;
    };
    if columns == 0 || rows == 0 || columns.checked_mul(rows).is_none() {
        return false;
    }
    for _ in 0..4 {
        let Some(line) = lines.next() else {
            return false;
        };
        if parse_number_line(line, 3).is_none() {
            return false;
        }
    }
    for _ in 0..4 {
        let Some(line) = lines.next() else {
            return false;
        };
        if parse_number_line(line, 4).is_none() {
            return false;
        }
    }
    true
}

pub(crate) fn convert<R: Read>(
    input: R,
    options: &ConvertOptions,
    sink: &mut dyn PageConsumer,
) -> Result<Vec<String>> {
    let input_limit = options.max_input_bytes.min(MAX_PTX_BYTES);
    let mut bytes = Vec::new();
    Read::take(input, input_limit.saturating_add(1)).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > input_limit {
        return Err(Error::LimitExceeded(format!(
            "PTX input exceeds maximum limit of {input_limit} bytes"
        )));
    }
    let text = String::from_utf8(bytes)
        .map_err(|error| Error::Unsupported(format!("PTX input must be ASCII/UTF-8: {error}")))?;
    let mut lines = text.lines().collect::<Vec<_>>();
    if lines.len() > MAX_PTX_LINES {
        return Err(Error::LimitExceeded(format!(
            "PTX line count exceeds {MAX_PTX_LINES}"
        )));
    }
    if let Some(first) = lines.first_mut() {
        *first = first.trim_start_matches('\u{feff}');
    }
    if lines.iter().any(|line| line.len() > MAX_PTX_LINE_BYTES) {
        return Err(Error::LimitExceeded(format!(
            "PTX line exceeds {MAX_PTX_LINE_BYTES} bytes"
        )));
    }

    let mut scans = Vec::new();
    let mut cursor = 0usize;
    let mut total_cells = 0usize;
    let mut total_tokens = 0usize;
    while cursor < lines.len() {
        while cursor < lines.len() && lines[cursor].trim().is_empty() {
            cursor += 1;
        }
        if cursor >= lines.len() {
            break;
        }
        if scans.len() >= MAX_PTX_SCANS {
            return Err(Error::LimitExceeded(format!(
                "PTX scan count exceeds {MAX_PTX_SCANS}"
            )));
        }
        let columns = parse_count_line(lines[cursor]).ok_or_else(|| {
            Error::InvalidInput(format!(
                "PTX scan {} has an invalid column count",
                scans.len() + 1
            ))
        })?;
        cursor += 1;
        let rows =
            parse_count_line(lines.get(cursor).ok_or_else(|| {
                Error::InvalidInput("PTX header ended before the row count".into())
            })?)
            .ok_or_else(|| {
                Error::InvalidInput(format!(
                    "PTX scan {} has an invalid row count",
                    scans.len() + 1
                ))
            })?;
        cursor += 1;
        if columns == 0 || rows == 0 {
            return Err(Error::InvalidInput(
                "PTX scan grid dimensions must be positive".into(),
            ));
        }
        let cells = columns
            .checked_mul(rows)
            .ok_or_else(|| Error::LimitExceeded("PTX scan grid cell count overflowed".into()))?;
        if cells > MAX_PTX_CELLS_PER_SCAN {
            return Err(Error::LimitExceeded(format!(
                "PTX scan grid exceeds {MAX_PTX_CELLS_PER_SCAN} cells"
            )));
        }
        total_cells = total_cells
            .checked_add(cells)
            .ok_or_else(|| Error::LimitExceeded("PTX total grid cell count overflowed".into()))?;
        if total_cells > MAX_PTX_CELLS_TOTAL {
            return Err(Error::LimitExceeded(format!(
                "PTX scans exceed {MAX_PTX_CELLS_TOTAL} grid cells"
            )));
        }

        // PTX header: four 3-component scanner transform columns followed by
        // four 4-component cloud transform columns (as in CloudCompare's reader).
        for _ in 0..4 {
            parse_required_number_line(&lines, &mut cursor, 3, "sensor transform")?;
        }
        let mut matrix_columns = [[0.0; 4]; 4];
        for column in &mut matrix_columns {
            let values = parse_required_number_line(&lines, &mut cursor, 4, "cloud transform")?;
            column.copy_from_slice(&values);
        }
        let transform = Transform::from_ptx_columns(matrix_columns)?;
        let point_start = cursor;
        let point_end = point_start
            .checked_add(cells)
            .ok_or_else(|| Error::LimitExceeded("PTX point record range overflowed".into()))?;
        if point_end > lines.len() {
            return Err(Error::InvalidInput(format!(
                "PTX scan {} declares {cells} grid cells but its point records are truncated",
                scans.len() + 1
            )));
        }
        let first = lines[point_start];
        let record_format = RecordFormat::from_field_count(first.split_whitespace().count())
            .ok_or_else(|| {
                Error::InvalidInput(format!(
                    "PTX scan {} point records must have 4, 7, or 10 fields",
                    scans.len() + 1
                ))
            })?;
        let mut invalid_points = 0usize;
        let mut invalid_colors = 0usize;
        let mut out_of_range_intensity = 0usize;
        for (cell_index, line) in lines[point_start..point_end].iter().enumerate() {
            let record = parse_point_record(line, record_format, scans.len() + 1, cell_index)?;
            total_tokens = total_tokens
                .checked_add(record_format.field_count())
                .ok_or_else(|| Error::LimitExceeded("PTX token count overflowed".into()))?;
            if total_tokens > MAX_PTX_TOKENS {
                return Err(Error::LimitExceeded(format!(
                    "PTX numeric tokens exceed {MAX_PTX_TOKENS}"
                )));
            }
            if !record.valid_coordinates {
                invalid_points = invalid_points.saturating_add(1);
            }
            if record.color.is_none() && !record.valid_color {
                invalid_colors = invalid_colors.saturating_add(1);
            }
            if record.format == RecordFormat::Intensity
                && (!record.valid_intensity || !(0.0..=1.0).contains(&record.intensity))
            {
                out_of_range_intensity = out_of_range_intensity.saturating_add(1);
            }
        }
        scans.push(Scan {
            columns,
            rows,
            cells,
            point_start,
            record_format,
            transform,
            invalid_points,
            invalid_colors,
            out_of_range_intensity,
        });
        cursor = point_end;
    }
    if scans.is_empty() {
        return Err(Error::InvalidInput(
            "PTX file contains no scan sections".into(),
        ));
    }
    if scans.len() > options.max_pages {
        return Err(Error::LimitExceeded(format!(
            "PTX contains {} scans; maximum output pages is {}",
            scans.len(),
            options.max_pages
        )));
    }

    let scan_cells = scans.iter().map(|scan| scan.cells).collect::<Vec<_>>();
    let quotas = crate::cad::allocate_sample_quotas(&scan_cells, MAX_PTX_PREVIEW_POINTS_TOTAL);
    let mut shared_warnings = vec![
        "PTX scanner pose/grid metadata is not shown; per-scan 4x4 cloud transforms are applied"
            .into(),
    ];
    if total_cells > MAX_PTX_PREVIEW_POINTS_TOTAL {
        shared_warnings.push(format!(
            "PTX total grid has {total_cells} cells; a deterministic sample of at most {MAX_PTX_PREVIEW_POINTS_TOTAL} points was rendered"
        ));
    }
    let mut warnings = shared_warnings.clone();
    let mut rendered_point_total = 0usize;
    let mut emitted_pages = 0usize;
    for (scan_index, scan) in scans.iter().enumerate() {
        let mut page_warnings = shared_warnings.clone();
        {
            let mut add_scan_warning = |warning: String| {
                page_warnings.push(warning.clone());
                warnings.push(warning);
            };
            if scan.transform.corrected_axes {
                add_scan_warning(format!(
                    "PTX scan {} transform axes were orthonormalized for the preview",
                    scan_index + 1
                ));
            }
            if scan.invalid_points > 0 {
                add_scan_warning(format!(
                    "{} PTX no-return/non-finite/out-of-range grid cells were omitted in scan {}",
                    scan.invalid_points,
                    scan_index + 1
                ));
            }
            if scan.invalid_colors > 0 {
                add_scan_warning(format!(
                    "{} PTX RGB values are invalid or mark no color; a blue fallback is used in scan {}",
                    scan.invalid_colors,
                    scan_index + 1
                ));
            }
            if scan.out_of_range_intensity > 0 {
                add_scan_warning(format!(
                    "{} PTX intensity value(s) outside [0,1] were clamped for grayscale preview in scan {}",
                    scan.out_of_range_intensity,
                    scan_index + 1
                ));
            }
            if scan.record_format == RecordFormat::RgbNormals {
                add_scan_warning(format!(
                    "PTX normals are not rendered in scan {}",
                    scan_index + 1
                ));
            }
            if scan.record_format != RecordFormat::Intensity {
                add_scan_warning(format!(
                    "PTX intensity scalar values are not rendered in RGB scan {}",
                    scan_index + 1
                ));
            }
        }

        let quota = quotas[scan_index];
        let mut selected_count = 0usize;
        let mut next_selected = 0usize;
        let mut rendered = 0usize;
        let mut ply = String::new();
        for (selected_cell, line) in lines[scan.point_start..scan.point_start + scan.cells]
            .iter()
            .enumerate()
        {
            let record =
                parse_point_record(line, scan.record_format, scan_index + 1, selected_cell)?;
            let selected = quota > 0 && selected_cell == next_selected;
            if selected && record.valid_coordinates {
                let transformed = scan.transform.apply(record.coordinates);
                if transformed.finite_in_range() {
                    let color = record.color.unwrap_or_else(|| match record.format {
                        RecordFormat::Intensity => intensity_gray(record.intensity),
                        RecordFormat::Rgb | RecordFormat::RgbNormals => DEFAULT_POINT_COLOR,
                    });
                    let point_line = format!(
                        "{:.9e} {:.9e} {:.9e} {} {} {}\n",
                        transformed.x, transformed.y, transformed.z, color[0], color[1], color[2]
                    );
                    if ply.len().saturating_add(point_line.len()) > MAX_PTX_PLY_BYTES {
                        return Err(Error::LimitExceeded(format!(
                            "PTX scan {} PLY intermediate exceeds {MAX_PTX_PLY_BYTES} bytes",
                            scan_index + 1
                        )));
                    }
                    ply.push_str(&point_line);
                    rendered += 1;
                    rendered_point_total += 1;
                }
            }
            if selected {
                selected_count += 1;
                next_selected = crate::cad::next_sample_index(selected_count, scan.cells, quota);
            }
        }
        if rendered == 0 {
            continue;
        }
        emitted_pages += 1;
        let mut ply_text = String::with_capacity(ply.len().saturating_add(128));
        writeln!(ply_text, "ply\nformat ascii 1.0\nelement vertex {rendered}")
            .map_err(|_| Error::InvalidInput("could not write PTX PLY header".into()))?;
        ply_text.push_str(
            "property double x\nproperty double y\nproperty double z\nproperty uchar red\nproperty uchar green\nproperty uchar blue\nend_header\n",
        );
        ply_text.push_str(&ply);
        let mut page_sink = PtxPageSink {
            inner: sink,
            page_number: emitted_pages,
            scan_number: scan_index + 1,
            columns: scan.columns,
            rows: scan.rows,
            point_count: rendered,
            warnings: &page_warnings,
        };
        crate::cad::ply::convert(Cursor::new(ply_text.as_bytes()), options, &mut page_sink)?;
    }
    if rendered_point_total == 0 {
        return Err(Error::Unsupported(
            "PTX scans contain no finite, non-zero coordinates".into(),
        ));
    }
    Ok(warnings)
}

fn parse_point_record(
    line: &str,
    format: RecordFormat,
    scan_number: usize,
    cell_index: usize,
) -> Result<PointRecord> {
    let mut tokens = [""; 10];
    let mut split = line.split_whitespace();
    for token in tokens.iter_mut().take(format.field_count()) {
        *token = split.next().ok_or_else(|| {
            Error::InvalidInput(format!(
                "PTX scan {scan_number} cell {cell_index} has too few fields"
            ))
        })?;
    }
    if split.next().is_some() {
        return Err(Error::InvalidInput(format!(
            "PTX scan {scan_number} cell {cell_index} has too many fields"
        )));
    }
    let mut values = [0.0; 10];
    for (index, token) in tokens.iter().take(format.field_count()).enumerate() {
        values[index] = parse_ptx_scalar(token).ok_or_else(|| {
            Error::InvalidInput(format!(
                "PTX scan {scan_number} cell {cell_index} has an invalid numeric value"
            ))
        })?;
    }
    let coordinates = Vec3 {
        x: values[0],
        y: values[1],
        z: values[2],
    };
    let valid_coordinates = coordinates.finite_in_range()
        && (coordinates.x != 0.0 || coordinates.y != 0.0 || coordinates.z != 0.0);
    let intensity = values[3];
    let valid_intensity = intensity.is_finite();
    let color = if matches!(format, RecordFormat::Rgb | RecordFormat::RgbNormals) {
        let rgb = [values[4], values[5], values[6]];
        Some(parse_ptx_color(rgb, &tokens[4..7]))
    } else {
        None
    };
    Ok(PointRecord {
        format,
        coordinates,
        intensity,
        color: color.and_then(|(value, valid)| valid.then_some(value)),
        valid_coordinates,
        valid_color: color.is_none_or(|(_, valid)| valid),
        valid_intensity,
    })
}

fn parse_ptx_color(values: [f64; 3], lexical: &[&str]) -> ([u8; 3], bool) {
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

fn intensity_gray(value: f64) -> [u8; 3] {
    let gray = if value.is_finite() {
        (value.clamp(0.0, 1.0) * 255.0).round() as u8
    } else {
        0
    };
    [gray, gray, gray]
}

fn parse_count_line(line: &str) -> Option<usize> {
    if line.len() > MAX_PTX_LINE_BYTES {
        return None;
    }
    let mut tokens = line.trim_start_matches('\u{feff}').split_whitespace();
    let value = tokens.next()?.parse::<usize>().ok()?;
    tokens.next().is_none().then_some(value)
}

fn parse_ptx_scalar(token: &str) -> Option<f64> {
    let normalized;
    let token = if token.contains(',') {
        normalized = token.replace(',', ".");
        normalized.as_str()
    } else {
        token
    };
    token.parse::<f64>().ok()
}

fn parse_number_line(line: &str, expected: usize) -> Option<Vec<f64>> {
    if line.len() > MAX_PTX_LINE_BYTES {
        return None;
    }
    let mut values = Vec::with_capacity(expected);
    for token in line.split_whitespace() {
        if values.len() == expected {
            return None;
        }
        let value = parse_ptx_scalar(token)?;
        if !value.is_finite() || value.abs() > MAX_PTX_COORDINATE {
            return None;
        }
        values.push(value);
    }
    (values.len() == expected).then_some(values)
}

fn parse_required_number_line(
    lines: &[&str],
    cursor: &mut usize,
    expected: usize,
    context: &str,
) -> Result<Vec<f64>> {
    let line = lines
        .get(*cursor)
        .ok_or_else(|| Error::InvalidInput(format!("PTX {context} header is truncated")))?;
    *cursor += 1;
    parse_number_line(line, expected).ok_or_else(|| {
        Error::InvalidInput(format!(
            "PTX {context} line must contain {expected} finite numbers"
        ))
    })
}

impl Transform {
    fn from_ptx_columns(columns: [[f64; 4]; 4]) -> Result<Self> {
        if columns
            .iter()
            .flatten()
            .any(|value| !value.is_finite() || value.abs() > MAX_PTX_COORDINATE)
        {
            return Err(Error::InvalidInput(
                "PTX transformation matrix is non-finite or outside ±1e12".into(),
            ));
        }
        if columns[0][3].abs() > 1e-6
            || columns[1][3].abs() > 1e-6
            || columns[2][3].abs() > 1e-6
            || (columns[3][3] - 1.0).abs() > 1e-6
        {
            return Err(Error::Unsupported(
                "PTX cloud matrix is not an affine 4x4 transform".into(),
            ));
        }
        let x_raw = Vec3 {
            x: columns[0][0],
            y: columns[0][1],
            z: columns[0][2],
        };
        let y_raw = Vec3 {
            x: columns[1][0],
            y: columns[1][1],
            z: columns[1][2],
        };
        let z_raw = Vec3 {
            x: columns[2][0],
            y: columns[2][1],
            z: columns[2][2],
        };
        let x_axis = x_raw
            .normalized()
            .ok_or_else(|| Error::InvalidInput("PTX transform X axis is degenerate".into()))?;
        let y_axis = y_raw
            .sub(x_axis.scale(y_raw.dot(x_axis)))
            .normalized()
            .ok_or_else(|| Error::InvalidInput("PTX transform axes are parallel".into()))?;
        let z_axis = x_axis
            .cross(y_axis)
            .normalized()
            .ok_or_else(|| Error::InvalidInput("PTX transform axes are degenerate".into()))?;
        let y_axis = z_axis
            .cross(x_axis)
            .normalized()
            .ok_or_else(|| Error::InvalidInput("PTX transform axes are degenerate".into()))?;
        let translation = Vec3 {
            x: columns[3][0],
            y: columns[3][1],
            z: columns[3][2],
        };
        let corrected_axes = (x_raw.dot(x_raw) - 1.0).abs() > 1e-6
            || (y_raw.dot(y_raw) - 1.0).abs() > 1e-6
            || x_raw.dot(y_raw).abs() > 1e-6
            || z_raw
                .normalized()
                .is_none_or(|z| z.dot(z_axis) < 1.0 - 1e-6);
        Ok(Self {
            x_axis,
            y_axis,
            z_axis,
            translation,
            corrected_axes,
        })
    }
}
