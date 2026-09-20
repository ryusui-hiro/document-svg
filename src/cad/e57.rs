//! Bounded ASTM E57 3D imaging point-cloud preview.
//!
//! E57 combines paged, CRC-protected binary point data with XML metadata. The
//! reader streams compressed point records, applies per-cloud poses and
//! converts spherical coordinates when required. Image blobs and non-point
//! metadata are not rendered or followed.

use std::fmt::Write as FmtWrite;
use std::fs::File;
use std::io::{Cursor, Read, Seek, SeekFrom};
use std::path::Path;

use ::e57::{CartesianCoordinate, E57Reader, Point, PointCloud, RecordName};

use crate::convert::{ConvertOptions, PageConsumer};
use crate::error::{Error, Result};
use crate::ir::{Node, Page};

const MAX_E57_BYTES: u64 = 2 * 1024 * 1024 * 1024;
const MAX_E57_XML_BYTES: u64 = 50 * 1024 * 1024;
const MAX_E57_XML_EVENTS: usize = 1_000_000;
const MAX_E57_XML_NODES: usize = 250_000;
const MAX_E57_POINT_CLOUDS: usize = 10_000;
const MAX_E57_ATTRIBUTES_PER_CLOUD: usize = 128;
const MAX_E57_POINTS_TOTAL: u64 = 20_000_000;
const MAX_E57_PREVIEW_POINTS: usize = 200_000;
const MAX_E57_PLY_BYTES_PER_CLOUD: usize = 32 * 1024 * 1024;
const MAX_E57_COORDINATE: f64 = 1.0e12;
const DEFAULT_POINT_COLOR: [u8; 3] = [37, 99, 235];

struct E57PageSink<'a> {
    inner: &'a mut dyn PageConsumer,
    page_number: usize,
    scan_number: usize,
    point_count: usize,
    warnings: &'a [String],
}

impl PageConsumer for E57PageSink<'_> {
    fn consume(&mut self, mut page: Page) -> Result<()> {
        page.number = self.page_number;
        page.source_format = "e57".into();
        page.title = format!("E57 point cloud {}", self.scan_number);
        page.description = format!(
            "E57 point cloud {} with {} sampled points",
            self.scan_number, self.point_count
        );
        for warning in self.warnings {
            page.warn(warning.clone());
        }
        for node in &mut page.nodes {
            if let Node::Path { meta, .. } = node
                && meta.kind == "ply-point-cloud"
            {
                meta.kind = "e57-point-cloud".into();
                meta.semantic_role = "e57:point-cloud".into();
            }
        }
        self.inner.consume(page)
    }
}

pub(crate) fn looks_like_prefix(bytes: &[u8]) -> bool {
    bytes.starts_with(b"ASTM-E57")
}

pub(crate) fn convert(
    path: &Path,
    options: &ConvertOptions,
    sink: &mut dyn PageConsumer,
) -> Result<Vec<String>> {
    let mut file = File::open(path)?;
    let file_size = file.metadata()?.len();
    let input_limit = options.max_input_bytes.min(MAX_E57_BYTES);
    if file_size > input_limit {
        return Err(Error::LimitExceeded(format!(
            "E57 input exceeds maximum limit of {input_limit} bytes"
        )));
    }
    let mut header_bytes = [0u8; 48];
    file.read_exact(&mut header_bytes)?;
    validate_header(&header_bytes, file_size)?;
    file.seek(SeekFrom::Start(0))?;
    if options.max_pages == 0 {
        return Err(Error::LimitExceeded(
            "E57 conversion requires at least one output page, but max_pages is zero".into(),
        ));
    }

    let xml = E57Reader::<File>::raw_xml(File::open(path)?)
        .map_err(|error| Error::InvalidInput(format!("invalid E57 XML section: {error}")))?;
    let xml_limits = crate::geospatial::xml_tree::XmlLimits {
        max_events: options.max_xml_events.min(MAX_E57_XML_EVENTS),
        max_nodes: MAX_E57_XML_NODES,
        max_depth: 64,
        max_text_bytes: MAX_E57_XML_BYTES as usize,
    };
    let xml_tree = crate::geospatial::xml_tree::parse_xml_tree(&xml, &xml_limits, "E57")?;
    drop(xml_tree);
    drop(xml);

    let mut reader = E57Reader::new(file)
        .map_err(|error| Error::InvalidInput(format!("invalid E57 file: {error}")))?;
    let scans = reader.pointclouds();
    if scans.is_empty() {
        return Err(Error::Unsupported(
            "E57 file contains no point clouds".into(),
        ));
    }
    if scans.len() > MAX_E57_POINT_CLOUDS {
        return Err(Error::LimitExceeded(format!(
            "E57 point-cloud count exceeds {MAX_E57_POINT_CLOUDS}"
        )));
    }
    if scans.len() > options.max_pages {
        return Err(Error::LimitExceeded(format!(
            "E57 contains {} point clouds; maximum output pages is {}",
            scans.len(),
            options.max_pages
        )));
    }
    let mut total_records = 0u64;
    let mut point_counts = Vec::with_capacity(scans.len());
    for scan in &scans {
        if scan.prototype.len() > MAX_E57_ATTRIBUTES_PER_CLOUD {
            return Err(Error::LimitExceeded(format!(
                "E57 point cloud has {} attributes; maximum is {MAX_E57_ATTRIBUTES_PER_CLOUD}",
                scan.prototype.len()
            )));
        }
        total_records = total_records
            .checked_add(scan.records)
            .ok_or_else(|| Error::LimitExceeded("E57 total point count overflowed".into()))?;
        point_counts.push(usize::try_from(scan.records).map_err(|_| {
            Error::LimitExceeded("E57 point count exceeds this platform's integer range".into())
        })?);
    }
    if total_records > MAX_E57_POINTS_TOTAL {
        return Err(Error::LimitExceeded(format!(
            "E57 contains {total_records} points; maximum decoded count is {MAX_E57_POINTS_TOTAL}"
        )));
    }

    let quotas = crate::cad::allocate_sample_quotas(&point_counts, MAX_E57_PREVIEW_POINTS);
    let mut shared_warnings = vec![
        "E57 image blobs, coordinate metadata and non-point objects are not rendered; point-cloud poses are applied".into(),
    ];
    if total_records > MAX_E57_PREVIEW_POINTS as u64 {
        shared_warnings.push(format!(
            "E57 contains {total_records} points; a deterministic sample of at most {MAX_E57_PREVIEW_POINTS} points was rendered"
        ));
    }
    let mut warnings = shared_warnings.clone();
    let mut emitted_pages = 0usize;
    let mut rendered_total = 0usize;
    for (scan_index, scan) in scans.iter().enumerate() {
        if quotas[scan_index] == 0 {
            continue;
        }
        let mut scan_warnings = shared_warnings.clone();
        if has_omitted_attributes(scan) {
            let warning = format!(
                "E57 point cloud {} contains attributes beyond Cartesian/spherical position, intensity, and RGB; those attributes are omitted",
                scan_index + 1
            );
            scan_warnings.push(warning.clone());
            warnings.push(warning);
        }

        let mut point_reader = reader
            .pointcloud_simple(scan)
            .map_err(|error| Error::InvalidInput(format!("invalid E57 point data: {error}")))?;
        point_reader.intensity_to_color(true);
        let mut selected_count = 0usize;
        let mut next_selected = 0usize;
        let quota = quotas[scan_index];
        let mut rendered = 0usize;
        let mut invalid_points = 0usize;
        let mut invalid_colors = 0usize;
        let mut ply = String::new();
        for (point_index, point) in point_reader.enumerate() {
            let point = point.map_err(|error| {
                Error::InvalidInput(format!(
                    "invalid E57 point {} in cloud {}: {error}",
                    point_index + 1,
                    scan_index + 1
                ))
            })?;
            if point_index != next_selected {
                continue;
            }
            selected_count += 1;
            next_selected =
                crate::cad::next_sample_index(selected_count, point_counts[scan_index], quota);
            let Some([x, y, z]) = point_coordinates(&point) else {
                invalid_points += 1;
                continue;
            };
            let (color, invalid_color) = point_color(&point);
            if invalid_color {
                invalid_colors += 1;
            }
            let point_line = format!(
                "{x:.9e} {y:.9e} {z:.9e} {} {} {}\n",
                color[0], color[1], color[2]
            );
            if ply.len().saturating_add(point_line.len()) > MAX_E57_PLY_BYTES_PER_CLOUD {
                return Err(Error::LimitExceeded(format!(
                    "E57 cloud {} PLY intermediate exceeds {MAX_E57_PLY_BYTES_PER_CLOUD} bytes",
                    scan_index + 1
                )));
            }
            ply.push_str(&point_line);
            rendered += 1;
        }
        if invalid_points > 0 {
            let warning = format!(
                "{invalid_points} invalid or out-of-range E57 points were omitted in cloud {}",
                scan_index + 1
            );
            scan_warnings.push(warning.clone());
            warnings.push(warning);
        }
        if invalid_colors > 0 {
            let warning = format!(
                "{invalid_colors} invalid E57 colors used a blue fallback in cloud {}",
                scan_index + 1
            );
            scan_warnings.push(warning.clone());
            warnings.push(warning);
        }
        if rendered == 0 {
            continue;
        }
        let mut ply_text = String::with_capacity(ply.len().saturating_add(128));
        writeln!(ply_text, "ply\nformat ascii 1.0\nelement vertex {rendered}")
            .map_err(|_| Error::InvalidInput("could not write E57 PLY header".into()))?;
        ply_text.push_str(
            "property double x\nproperty double y\nproperty double z\nproperty uchar red\nproperty uchar green\nproperty uchar blue\nend_header\n",
        );
        ply_text.push_str(&ply);
        emitted_pages += 1;
        rendered_total += rendered;
        let mut page_sink = E57PageSink {
            inner: sink,
            page_number: emitted_pages,
            scan_number: scan_index + 1,
            point_count: rendered,
            warnings: &scan_warnings,
        };
        crate::cad::ply::convert(Cursor::new(ply_text.as_bytes()), options, &mut page_sink)?;
    }
    if rendered_total == 0 {
        return Err(Error::Unsupported(
            "E57 point clouds contain no finite, in-range Cartesian coordinates".into(),
        ));
    }
    Ok(warnings)
}

fn validate_header(bytes: &[u8; 48], actual_size: u64) -> Result<::e57::Header> {
    if actual_size < 48 {
        return Err(Error::InvalidInput(
            "E57 input is shorter than its 48-byte file header".into(),
        ));
    }
    let mut cursor = Cursor::new(bytes.as_slice());
    let header = ::e57::Header::read(&mut cursor)
        .map_err(|error| Error::InvalidInput(format!("invalid E57 header: {error}")))?;
    if header.phys_length != actual_size {
        return Err(Error::InvalidInput(format!(
            "E57 header declares {} physical bytes but the file contains {actual_size}",
            header.phys_length
        )));
    }
    if header.xml_length == 0 || header.xml_length > MAX_E57_XML_BYTES {
        return Err(Error::LimitExceeded(format!(
            "E57 XML section length {} is outside the supported range 1..={MAX_E57_XML_BYTES}",
            header.xml_length
        )));
    }
    let xml_end = header
        .phys_xml_offset
        .checked_add(header.xml_length)
        .ok_or_else(|| Error::InvalidInput("E57 XML section offset overflowed".into()))?;
    if header.phys_xml_offset < 48 || xml_end > actual_size {
        return Err(Error::InvalidInput(
            "E57 XML section offset lies outside the file".into(),
        ));
    }
    if !actual_size.is_multiple_of(header.page_size) {
        return Err(Error::InvalidInput(
            "E57 physical file length is not aligned to its CRC page size".into(),
        ));
    }
    Ok(header)
}

fn point_coordinates(point: &Point) -> Option<[f64; 3]> {
    let CartesianCoordinate::Valid { x, y, z } = &point.cartesian else {
        return None;
    };
    [*x, *y, *z]
        .iter()
        .all(|value| value.is_finite() && value.abs() <= MAX_E57_COORDINATE)
        .then_some([*x, *y, *z])
}

fn point_color(point: &Point) -> ([u8; 3], bool) {
    if let Some(color) = &point.color {
        let values = [color.red, color.green, color.blue];
        if values
            .iter()
            .all(|value| value.is_finite() && (0.0..=1.0).contains(value))
        {
            return (values.map(|value| (value * 255.0).round() as u8), false);
        }
        return (DEFAULT_POINT_COLOR, true);
    }
    if let Some(intensity) = point.intensity {
        let gray = if intensity.is_finite() {
            (intensity.clamp(0.0, 1.0) * 255.0).round() as u8
        } else {
            0
        };
        return ([gray; 3], !intensity.is_finite());
    }
    (DEFAULT_POINT_COLOR, false)
}

fn has_omitted_attributes(point_cloud: &PointCloud) -> bool {
    point_cloud.prototype.iter().any(|record| {
        !matches!(
            record.name,
            RecordName::CartesianX
                | RecordName::CartesianY
                | RecordName::CartesianZ
                | RecordName::CartesianInvalidState
                | RecordName::SphericalRange
                | RecordName::SphericalAzimuth
                | RecordName::SphericalElevation
                | RecordName::SphericalInvalidState
                | RecordName::Intensity
                | RecordName::IsIntensityInvalid
                | RecordName::ColorRed
                | RecordName::ColorGreen
                | RecordName::ColorBlue
                | RecordName::IsColorInvalid
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_the_astm_file_signature() {
        assert!(looks_like_prefix(b"ASTM-E57\0"));
        assert!(!looks_like_prefix(b"E57"));
    }

    #[test]
    fn rejects_inconsistent_file_header_lengths() {
        let mut cursor = Cursor::new(Vec::new());
        let mut header = ::e57::Header::default();
        header.phys_length = 1024;
        header.phys_xml_offset = 48;
        header.xml_length = 64;
        header.write(&mut cursor).unwrap();
        assert!(validate_header(cursor.get_ref().as_slice().try_into().unwrap(), 2048).is_err());
    }

    #[test]
    fn recognizes_only_finite_bounded_cartesian_points() {
        assert_eq!(
            point_coordinates(&Point {
                cartesian: CartesianCoordinate::Valid {
                    x: 0.0,
                    y: 1.0,
                    z: -2.0,
                },
                spherical: ::e57::SphericalCoordinate::Invalid,
                color: None,
                intensity: None,
                row: -1,
                column: -1,
            }),
            Some([0.0, 1.0, -2.0])
        );
    }

    #[test]
    fn missing_optional_color_uses_preview_color_without_warning() {
        assert_eq!(
            point_color(&Point {
                cartesian: CartesianCoordinate::Valid {
                    x: 0.0,
                    y: 0.0,
                    z: 0.0
                },
                spherical: ::e57::SphericalCoordinate::Invalid,
                color: None,
                intensity: None,
                row: -1,
                column: -1,
            }),
            (DEFAULT_POINT_COLOR, false)
        );
    }
}
