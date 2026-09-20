//! Bounded LAS/LAZ point-cloud preview.
//!
//! Samples coordinates (and optional RGB) from ASPRS LAS 1.0–1.4 files, then
//! reuses the PLY point renderer. The converter never fetches external files or
//! interprets waveform/extra-byte payloads.

use std::fs::File;
use std::io::{BufReader, Read, Seek, SeekFrom};
use std::path::Path;

use crate::convert::{ConvertOptions, PageConsumer};
use crate::error::{Error, Result};
use crate::ir::{Node, Page};

const MAX_LAS_FILE_BYTES: u64 = 2 * 1024 * 1024 * 1024;
const MAX_LAS_VLR_BYTES: u64 = 16 * 1024 * 1024;
const MAX_LAS_EVLRS_BYTES: u64 = 8 * 1024 * 1024;
const MAX_LAS_VLRS: u32 = 100_000;
const MAX_LAS_POINT_RECORD_BYTES: u16 = 4096;
const MAX_LAZ_POINTS_TO_DECODE: u64 = 20_000_000;
const MAX_LAS_PREVIEW_POINTS: u64 = 200_000;
const MAX_LAS_PLY_BYTES: usize = 32 * 1024 * 1024;
const MAX_LAS_COORDINATE: f64 = 1.0e12;
const POINT_BATCH_SIZE: u64 = 8192;

struct LasPageSink<'a> {
    inner: &'a mut dyn PageConsumer,
    warnings: &'a [String],
    point_count: usize,
}

impl PageConsumer for LasPageSink<'_> {
    fn consume(&mut self, mut page: Page) -> Result<()> {
        page.source_format = "las".into();
        page.title = "LAS/LAZ Point Cloud".into();
        page.description = format!(
            "LAS/LAZ point cloud preview with {} sampled points",
            self.point_count
        );
        for warning in self.warnings {
            page.warn(warning.clone());
        }
        for node in &mut page.nodes {
            if let Node::Path { meta, .. } = node
                && meta.kind == "ply-point-cloud"
            {
                meta.kind = "las-point-cloud".into();
                meta.semantic_role = "las:point-cloud".into();
            }
        }
        self.inner.consume(page)
    }
}

pub(crate) fn convert(
    path: &Path,
    options: &ConvertOptions,
    sink: &mut dyn PageConsumer,
) -> Result<Vec<String>> {
    let mut file = File::open(path)?;
    let mut warnings = preflight(&mut file, path)?;
    file.seek(SeekFrom::Start(0))?;
    let mut reader = las::Reader::new(BufReader::new(file))
        .map_err(|error| Error::InvalidInput(format!("invalid LAS/LAZ file: {error}")))?;
    let header = reader.header();
    let point_count = header.number_of_points();
    let compressed = header.point_format().is_compressed;
    if point_count == 0 {
        return Err(Error::InvalidInput(
            "LAS/LAZ file contains no points".into(),
        ));
    }
    if compressed && point_count > MAX_LAZ_POINTS_TO_DECODE {
        return Err(Error::LimitExceeded(format!(
            "LAZ contains {point_count} points; maximum sequential decode count is {MAX_LAZ_POINTS_TO_DECODE}"
        )));
    }

    if header.has_crs_vlrs() {
        warnings.push(
            "LAS/LAZ CRS metadata was detected but coordinates were not transformed; stored XY coordinates were projected as-is".into(),
        );
    } else {
        warnings.push(
            "LAS/LAZ coordinates were projected as stored; no CRS transformation was applied"
                .into(),
        );
    }
    warnings.push(
        "LAS/LAZ waveform, classification, intensity, GPS time, NIR, and extra-byte attributes are not shown".into(),
    );
    if header.evlrs().len() > 1 {
        warnings.push("additional LAS/LAZ extended variable-length records were ignored".into());
    }

    let mut ply_data = String::new();
    let has_color = header.point_format().has_color;
    let mut point_data = las::PointDataBuilder::new()
        .for_header(reader.header())
        .build();

    let mut sampled = 0usize;
    let mut invalid_points = 0u64;
    let target_points = point_count.min(MAX_LAS_PREVIEW_POINTS);
    if compressed {
        let mut index = 0u64;
        let mut sample_index = 0u64;
        while index < point_count {
            let read_count = reader
                .fill_points(POINT_BATCH_SIZE.min(point_count - index), &mut point_data)
                .map_err(|error| {
                    Error::InvalidInput(format!("failed to decode LAZ points: {error}"))
                })?;
            if read_count == 0 {
                return Err(Error::InvalidInput(format!(
                    "LAZ ended after {index} points; header declares {point_count}"
                )));
            }
            let mut xs = point_data.x();
            let mut ys = point_data.y();
            let mut zs = point_data.z();
            let mut colors = point_data.rgb();
            for _ in 0..read_count {
                let coordinates = next_coordinates(&mut xs, &mut ys, &mut zs)?;
                let color = next_color(&mut colors);
                if sample_index < target_points
                    && index == sample_index * point_count / target_points
                {
                    if append_point(&mut ply_data, coordinates, color, has_color)? {
                        sampled += 1;
                    } else {
                        invalid_points += 1;
                    }
                    sample_index += 1;
                }
                index += 1;
            }
        }
    } else {
        for sample_index in 0..target_points {
            let index = sample_index * point_count / target_points;
            reader.seek(index).map_err(|error| {
                Error::InvalidInput(format!("failed to seek LAS point {index}: {error}"))
            })?;
            let read_count = reader.fill_points(1, &mut point_data).map_err(|error| {
                Error::InvalidInput(format!("failed to decode LAS point {index}: {error}"))
            })?;
            if read_count == 0 {
                return Err(Error::InvalidInput(format!(
                    "LAS ended before point {index}"
                )));
            }
            let mut xs = point_data.x();
            let mut ys = point_data.y();
            let mut zs = point_data.z();
            let mut colors = point_data.rgb();
            let coordinates = next_coordinates(&mut xs, &mut ys, &mut zs)?;
            if append_point(
                &mut ply_data,
                coordinates,
                next_color(&mut colors),
                has_color,
            )? {
                sampled += 1;
            } else {
                invalid_points += 1;
            }
        }
    }
    if sampled == 0 {
        return Err(Error::InvalidInput(
            "LAS/LAZ contains no finite, in-range XYZ points".into(),
        ));
    }
    let mut ply = String::with_capacity(ply_data.len().saturating_add(160));
    ply.push_str("ply\nformat ascii 1.0\n");
    ply.push_str(&format!(
        "element vertex {sampled}\nproperty double x\nproperty double y\nproperty double z\n"
    ));
    if has_color {
        ply.push_str("property uchar red\nproperty uchar green\nproperty uchar blue\n");
    }
    ply.push_str("end_header\n");
    ply.push_str(&ply_data);
    if invalid_points > 0 {
        warnings.push(format!(
            "{invalid_points} sampled LAS/LAZ point(s) with non-finite or out-of-range coordinates were omitted"
        ));
    }
    if point_count > MAX_LAS_PREVIEW_POINTS {
        warnings.push(format!(
            "LAS/LAZ point cloud was evenly sampled from {point_count} source points to at most {MAX_LAS_PREVIEW_POINTS} preview points"
        ));
    }
    if ply.len() > MAX_LAS_PLY_BYTES {
        return Err(Error::LimitExceeded(format!(
            "LAS/LAZ point preview intermediate exceeds {MAX_LAS_PLY_BYTES} bytes"
        )));
    }
    let mut page_sink = LasPageSink {
        inner: sink,
        warnings: &warnings,
        point_count: sampled,
    };
    let render_warnings = crate::cad::ply::convert(ply.as_bytes(), options, &mut page_sink)?;
    warnings.extend(render_warnings);
    Ok(warnings)
}

fn next_coordinates(
    xs: &mut impl Iterator<Item = f64>,
    ys: &mut impl Iterator<Item = f64>,
    zs: &mut impl Iterator<Item = f64>,
) -> Result<[f64; 3]> {
    Ok([
        xs.next().ok_or_else(|| {
            Error::InvalidInput("LAS point batch is missing x coordinates".into())
        })?,
        ys.next().ok_or_else(|| {
            Error::InvalidInput("LAS point batch is missing y coordinates".into())
        })?,
        zs.next().ok_or_else(|| {
            Error::InvalidInput("LAS point batch is missing z coordinates".into())
        })?,
    ])
}

fn next_color(colors: &mut Option<impl Iterator<Item = (u16, u16, u16)>>) -> Option<[u16; 3]> {
    colors
        .as_mut()
        .and_then(Iterator::next)
        .map(|(red, green, blue)| [red, green, blue])
}

fn append_point(
    output: &mut String,
    [x, y, z]: [f64; 3],
    color: Option<[u16; 3]>,
    has_color: bool,
) -> Result<bool> {
    if ![x, y, z]
        .iter()
        .all(|value| value.is_finite() && value.abs() <= MAX_LAS_COORDINATE)
    {
        return Ok(false);
    }
    let color = color
        .map(|color| color.map(|channel| ((u32::from(channel) * 255 + 32_767) / 65_535) as u8));
    let record = if has_color {
        let [red, green, blue] = color.unwrap_or([37, 99, 235]);
        format!("{:.17e} {:.17e} {:.17e} {red} {green} {blue}\n", x, y, z)
    } else {
        format!("{x:.17e} {y:.17e} {z:.17e}\n")
    };
    if output.len().saturating_add(record.len()) > MAX_LAS_PLY_BYTES {
        return Err(Error::LimitExceeded(format!(
            "LAS/LAZ point preview intermediate exceeds {MAX_LAS_PLY_BYTES} bytes"
        )));
    }
    output.push_str(&record);
    Ok(true)
}

fn preflight(file: &mut File, path: &Path) -> Result<Vec<String>> {
    let file_len = file.metadata()?.len();
    if file_len > MAX_LAS_FILE_BYTES {
        return Err(Error::LimitExceeded(format!(
            "LAS/LAZ file is {file_len} bytes; maximum is {MAX_LAS_FILE_BYTES}"
        )));
    }
    let raw = las::raw::Header::read_from(&mut *file)
        .map_err(|error| Error::InvalidInput(format!("invalid LAS/LAZ header: {error}")))?;
    if raw.version.major != 1 || raw.version.minor > 4 {
        return Err(Error::Unsupported(format!(
            "LAS version {}.{} is unsupported; supported versions are 1.0 through 1.4",
            raw.version.major, raw.version.minor
        )));
    }
    let minimum_header_size = raw.version.header_size();
    if raw.header_size < minimum_header_size || u64::from(raw.header_size) > file_len {
        return Err(Error::InvalidInput(format!(
            "LAS header size {} is invalid for version {}.{}",
            raw.header_size, raw.version.major, raw.version.minor
        )));
    }
    let point_offset = u64::from(raw.offset_to_point_data);
    if point_offset < u64::from(raw.header_size) || point_offset > file_len {
        return Err(Error::InvalidInput(
            "LAS point-data offset is outside the file".into(),
        ));
    }
    let vlr_bytes = point_offset - u64::from(raw.header_size);
    if vlr_bytes > MAX_LAS_VLR_BYTES {
        return Err(Error::LimitExceeded(format!(
            "LAS variable-length-record area exceeds {MAX_LAS_VLR_BYTES} bytes"
        )));
    }
    if raw.number_of_variable_length_records > MAX_LAS_VLRS
        || u64::from(raw.number_of_variable_length_records) > vlr_bytes / 54
    {
        return Err(Error::LimitExceeded(
            "LAS declares more variable-length records than fit in the bounded header area".into(),
        ));
    }
    let format_id = raw.point_data_record_format & 0x3f;
    if raw.point_data_record_format & 0x40 != 0 {
        return Err(Error::Unsupported(
            "LAS point format uses a reserved compression flag".into(),
        ));
    }
    let format = las::point::Format::new(format_id)
        .map_err(|error| Error::Unsupported(format!("unsupported LAS point format: {error}")))?;
    if !raw.version.supports_point_format(format) {
        return Err(Error::InvalidInput(format!(
            "LAS point format {format_id} is not valid for version {}.{}",
            raw.version.major, raw.version.minor
        )));
    }
    if raw.point_data_record_length < format.len() {
        return Err(Error::InvalidInput(format!(
            "LAS point record length {} is shorter than point format {} minimum {}",
            raw.point_data_record_length,
            format_id,
            format.len()
        )));
    }
    if raw.point_data_record_length > MAX_LAS_POINT_RECORD_BYTES {
        return Err(Error::LimitExceeded(format!(
            "LAS point record length {} exceeds {MAX_LAS_POINT_RECORD_BYTES} bytes",
            raw.point_data_record_length
        )));
    }

    let point_count = if raw.number_of_point_records > 0 {
        u64::from(raw.number_of_point_records)
    } else {
        raw.large_file
            .map(|header| header.number_of_point_records)
            .unwrap_or(0)
    };
    if point_count == 0 {
        return Err(Error::InvalidInput(
            "LAS/LAZ file contains no points".into(),
        ));
    }
    let compressed = raw.point_data_record_format & 0x80 != 0;
    if compressed && point_count > MAX_LAZ_POINTS_TO_DECODE {
        return Err(Error::LimitExceeded(format!(
            "LAZ declares {point_count} points; maximum sequential decode count is {MAX_LAZ_POINTS_TO_DECODE}"
        )));
    }
    if !compressed {
        let point_bytes = point_count
            .checked_mul(u64::from(raw.point_data_record_length))
            .ok_or_else(|| Error::LimitExceeded("LAS point-data size overflowed".into()))?;
        let end = point_offset
            .checked_add(point_bytes)
            .ok_or_else(|| Error::LimitExceeded("LAS point-data offset overflowed".into()))?;
        if end > file_len {
            return Err(Error::InvalidInput(format!(
                "LAS point records require {end} bytes but file contains {file_len}"
            )));
        }
    }
    let mut warnings = Vec::new();
    if raw.point_data_record_format & 0x80 != 0
        && path
            .extension()
            .and_then(|ext| ext.to_str())
            .is_some_and(|ext| ext.eq_ignore_ascii_case("las"))
    {
        warnings.push("compressed LAS point data was detected despite the .las extension".into());
    } else if raw.point_data_record_format & 0x80 == 0
        && path
            .extension()
            .and_then(|ext| ext.to_str())
            .is_some_and(|ext| ext.eq_ignore_ascii_case("laz"))
    {
        warnings.push("uncompressed LAS point data was detected despite the .laz extension".into());
    }
    if let Some(evlr) = raw.evlr {
        if evlr.number_of_evlrs > 100_000 {
            return Err(Error::LimitExceeded(
                "LAS extended variable-length-record count exceeds 100000".into(),
            ));
        }
        if evlr.number_of_evlrs > 0 {
            let offset = evlr.start_of_first_evlr;
            if offset.checked_add(60).is_none_or(|end| end > file_len) {
                return Err(Error::InvalidInput(
                    "LAS extended variable-length-record header is outside the file".into(),
                ));
            }
            file.seek(SeekFrom::Start(offset))?;
            let mut record_header = [0u8; 60];
            file.read_exact(&mut record_header)?;
            let record_bytes = u64::from_le_bytes(record_header[20..28].try_into().unwrap());
            if record_bytes > MAX_LAS_EVLRS_BYTES {
                return Err(Error::LimitExceeded(format!(
                    "LAS first extended variable-length record exceeds {MAX_LAS_EVLRS_BYTES} bytes"
                )));
            }
            if offset
                .checked_add(60)
                .and_then(|start| start.checked_add(record_bytes))
                .is_none_or(|end| end > file_len)
            {
                return Err(Error::InvalidInput(
                    "LAS extended variable-length record extends beyond the file".into(),
                ));
            }
            if !compressed {
                let point_bytes = point_count
                    .checked_mul(u64::from(raw.point_data_record_length))
                    .ok_or_else(|| Error::LimitExceeded("LAS point-data size overflowed".into()))?;
                let end_of_points = point_offset.checked_add(point_bytes).ok_or_else(|| {
                    Error::LimitExceeded("LAS point-data offset overflowed".into())
                })?;
                if offset < end_of_points {
                    return Err(Error::InvalidInput(
                        "LAS extended variable-length record overlaps point data".into(),
                    ));
                }
                if offset - end_of_points > MAX_LAS_VLR_BYTES {
                    return Err(Error::LimitExceeded(format!(
                        "LAS padding before extended records exceeds {MAX_LAS_VLR_BYTES} bytes"
                    )));
                }
            }
        }
    }
    Ok(warnings)
}
