//! Bounded Point Cloud Library (PCD) point-cloud preview.
//!
//! Reads PCD ASCII, interleaved binary, and LZF-compressed structure-of-arrays
//! data. XYZ coordinates are rendered through the bounded PLY point renderer;
//! other dimensions are validated but not visualized.

use std::io::{BufRead, BufReader, Cursor, Read};

use crate::convert::{ConvertOptions, PageConsumer};
use crate::error::{Error, Result};
use crate::ir::{Node, Page};

const MAX_PCD_HEADER_BYTES: usize = 1024 * 1024;
const MAX_PCD_LINE_BYTES: usize = 1024 * 1024;
const MAX_PCD_FIELDS: usize = 128;
const MAX_PCD_VALUES_PER_POINT: usize = 16_384;
const MAX_PCD_POINT_STEP: usize = 1024 * 1024;
const MAX_PCD_POINTS: usize = 200_000;
const MAX_PCD_DECODED_BYTES: usize = 128 * 1024 * 1024;
const MAX_PCD_PLY_BYTES: usize = 32 * 1024 * 1024;
const MAX_PCD_COORDINATE: f64 = 1.0e12;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ScalarType {
    I8,
    U8,
    I16,
    U16,
    I32,
    U32,
    I64,
    U64,
    F32,
    F64,
}

impl ScalarType {
    fn parse(kind: char, size: usize) -> Option<Self> {
        match (kind.to_ascii_uppercase(), size) {
            ('I', 1) => Some(Self::I8),
            ('U', 1) => Some(Self::U8),
            ('I', 2) => Some(Self::I16),
            ('U', 2) => Some(Self::U16),
            ('I', 4) => Some(Self::I32),
            ('U', 4) => Some(Self::U32),
            ('I', 8) => Some(Self::I64),
            ('U', 8) => Some(Self::U64),
            ('F', 4) => Some(Self::F32),
            ('F', 8) => Some(Self::F64),
            _ => None,
        }
    }

    fn size(self) -> usize {
        match self {
            Self::I8 | Self::U8 => 1,
            Self::I16 | Self::U16 => 2,
            Self::I32 | Self::U32 | Self::F32 => 4,
            Self::I64 | Self::U64 | Self::F64 => 8,
        }
    }

    fn parse_ascii(self, value: &str) -> Option<f64> {
        match self {
            Self::I8 => value.parse::<i8>().ok().map(f64::from),
            Self::U8 => value.parse::<u8>().ok().map(f64::from),
            Self::I16 => value.parse::<i16>().ok().map(f64::from),
            Self::U16 => value.parse::<u16>().ok().map(f64::from),
            Self::I32 => value.parse::<i32>().ok().map(f64::from),
            Self::U32 => value.parse::<u32>().ok().map(f64::from),
            Self::I64 => value.parse::<i64>().ok().map(|value| value as f64),
            Self::U64 => value.parse::<u64>().ok().map(|value| value as f64),
            Self::F32 => value.parse::<f32>().ok().map(f64::from),
            Self::F64 => value.parse::<f64>().ok(),
        }
    }

    fn parse_binary(self, bytes: &[u8]) -> Option<f64> {
        if bytes.len() != self.size() {
            return None;
        }
        Some(match self {
            Self::I8 => i8::from_ne_bytes([bytes[0]]) as f64,
            Self::U8 => bytes[0] as f64,
            Self::I16 => i16::from_le_bytes(bytes.try_into().ok()?) as f64,
            Self::U16 => u16::from_le_bytes(bytes.try_into().ok()?) as f64,
            Self::I32 => i32::from_le_bytes(bytes.try_into().ok()?) as f64,
            Self::U32 => u32::from_le_bytes(bytes.try_into().ok()?) as f64,
            Self::I64 => i64::from_le_bytes(bytes.try_into().ok()?) as f64,
            Self::U64 => u64::from_le_bytes(bytes.try_into().ok()?) as f64,
            Self::F32 => f32::from_le_bytes(bytes.try_into().ok()?) as f64,
            Self::F64 => f64::from_le_bytes(bytes.try_into().ok()?),
        })
    }
}

#[derive(Clone, Debug)]
struct Field {
    name: String,
    scalar: ScalarType,
    count: usize,
    record_offset: usize,
    value_offset: usize,
    compressed_offset: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DataMode {
    Ascii,
    Binary,
    BinaryCompressed,
}

#[derive(Clone, Copy, Debug)]
enum ColorSource {
    Packed(usize),
    Channels([usize; 3]),
}

impl ColorSource {
    fn contains_field(self, field_index: usize) -> bool {
        match self {
            Self::Packed(index) => index == field_index,
            Self::Channels(indices) => indices.contains(&field_index),
        }
    }
}

struct ParsedHeader {
    fields: Vec<Field>,
    point_count: usize,
    point_step: usize,
    mode: DataMode,
    warnings: Vec<String>,
    viewpoint_is_default: bool,
}

struct PointReadConfig<'a> {
    fields: &'a [Field],
    point_count: usize,
    point_step: usize,
    expected_size: usize,
    coordinate_fields: [usize; 3],
    color_source: Option<ColorSource>,
}

#[derive(Default)]
struct PointAccumulator {
    coordinates: Vec<[f64; 3]>,
    colors: Vec<Option<[u8; 3]>>,
    invalid_points: usize,
    invalid_color_points: usize,
}

struct PcdPageSink<'a> {
    inner: &'a mut dyn PageConsumer,
    warnings: &'a [String],
    point_count: usize,
}

impl PageConsumer for PcdPageSink<'_> {
    fn consume(&mut self, mut page: Page) -> Result<()> {
        page.source_format = "pcd".into();
        page.title = "PCD Point Cloud".into();
        page.description = format!(
            "PCD point cloud with {} renderable points",
            self.point_count
        );
        for warning in self.warnings {
            page.warn(warning.clone());
        }
        for node in &mut page.nodes {
            if let Node::Path { meta, .. } = node
                && meta.kind == "ply-point-cloud"
            {
                meta.kind = "pcd-point-cloud".into();
                meta.semantic_role = "pcd:point-cloud".into();
            }
        }
        self.inner.consume(page)
    }
}

pub(crate) fn convert<R: Read>(
    input: R,
    options: &ConvertOptions,
    sink: &mut dyn PageConsumer,
) -> Result<Vec<String>> {
    let mut reader = BufReader::new(input);
    let ParsedHeader {
        mut fields,
        point_count,
        point_step,
        mode,
        mut warnings,
        viewpoint_is_default,
    } = read_header(&mut reader)?;
    if point_count == 0 {
        return Err(Error::InvalidInput("PCD file contains no points".into()));
    }
    if point_count > MAX_PCD_POINTS {
        return Err(Error::LimitExceeded(format!(
            "PCD contains {point_count} points; maximum preview count is {MAX_PCD_POINTS}"
        )));
    }
    if !viewpoint_is_default {
        warnings.push(
            "PCD VIEWPOINT pose metadata was ignored; stored point coordinates were rendered as-is"
                .into(),
        );
    }
    let x_index = field_index(&fields, "x")?;
    let y_index = field_index(&fields, "y")?;
    let z_index = field_index(&fields, "z")?;
    let color_source = pcd_color_source(&fields);
    let has_color_fields = fields.iter().any(|field| {
        ["rgb", "rgba", "r", "red", "g", "green", "b", "blue"]
            .iter()
            .any(|name| field.name.eq_ignore_ascii_case(name))
    });
    for index in [x_index, y_index, z_index] {
        if fields[index].count != 1 {
            return Err(Error::Unsupported(format!(
                "PCD coordinate field '{}' must have COUNT 1",
                fields[index].name
            )));
        }
    }

    let decoded_size = point_count
        .checked_mul(point_step)
        .ok_or_else(|| Error::LimitExceeded("PCD decoded point data size overflowed".into()))?;
    if decoded_size > MAX_PCD_DECODED_BYTES {
        return Err(Error::LimitExceeded(format!(
            "PCD point data requires {decoded_size} bytes; maximum is {MAX_PCD_DECODED_BYTES}"
        )));
    }
    let mut compressed_offset = 0usize;
    for field in &mut fields {
        field.compressed_offset = compressed_offset;
        compressed_offset = compressed_offset
            .checked_add(
                point_count
                    .checked_mul(field.scalar.size() * field.count)
                    .ok_or_else(|| {
                        Error::LimitExceeded("PCD compressed field size overflowed".into())
                    })?,
            )
            .ok_or_else(|| Error::LimitExceeded("PCD compressed field offset overflowed".into()))?;
    }
    if compressed_offset != decoded_size {
        return Err(Error::InvalidInput(
            "PCD field layout does not match its computed point step".into(),
        ));
    }

    let config = PointReadConfig {
        fields: &fields,
        point_count,
        point_step,
        expected_size: decoded_size,
        coordinate_fields: [x_index, y_index, z_index],
        color_source,
    };
    let mut points = PointAccumulator {
        coordinates: Vec::with_capacity(point_count.min(16_384)),
        colors: Vec::with_capacity(point_count.min(16_384)),
        invalid_points: 0,
        invalid_color_points: 0,
    };
    match mode {
        DataMode::Ascii => {
            read_ascii_points(&mut reader, &config, &mut points)?;
        }
        DataMode::Binary => {
            read_binary_points(&mut reader, &config, &mut points)?;
        }
        DataMode::BinaryCompressed => {
            read_compressed_points(&mut reader, &config, &mut points)?;
        }
    }
    if points.coordinates.is_empty() {
        return Err(Error::InvalidInput(
            "PCD contains no finite, in-range XYZ points".into(),
        ));
    }
    if points.invalid_points > 0 {
        warnings.push(format!(
            "{} PCD point(s) with non-finite or out-of-range coordinates were omitted",
            points.invalid_points
        ));
    }
    if color_source.is_some() && points.invalid_color_points > 0 {
        warnings.push(format!(
            "{} PCD point color value(s) could not be decoded; those points use a default blue marker",
            points.invalid_color_points
        ));
    }
    if has_color_fields && color_source.is_none() {
        warnings.push("PCD color fields have an unsupported layout and were ignored".into());
    }
    if fields.iter().enumerate().any(|(index, _)| {
        ![x_index, y_index, z_index].contains(&index)
            && !color_source.is_some_and(|source| source.contains_field(index))
    }) {
        warnings.push("PCD non-coordinate fields such as normals or intensity were ignored".into());
    }

    let mut ply = String::with_capacity(
        points
            .coordinates
            .len()
            .saturating_mul(78)
            .min(MAX_PCD_PLY_BYTES),
    );
    ply.push_str("ply\nformat ascii 1.0\n");
    ply.push_str(&format!("element vertex {}\n", points.coordinates.len()));
    ply.push_str("property double x\nproperty double y\nproperty double z\n");
    let has_colors = points.colors.iter().any(Option::is_some);
    if has_colors {
        ply.push_str("property uchar red\nproperty uchar green\nproperty uchar blue\n");
    }
    ply.push_str("end_header\n");
    for (index, [x, y, z]) in points.coordinates.iter().enumerate() {
        let record = if has_colors {
            let [red, green, blue] = points.colors[index].unwrap_or([37, 99, 235]);
            format!("{x:.17e} {y:.17e} {z:.17e} {red} {green} {blue}\n")
        } else {
            format!("{x:.17e} {y:.17e} {z:.17e}\n")
        };
        if ply.len().saturating_add(record.len()) > MAX_PCD_PLY_BYTES {
            return Err(Error::LimitExceeded(format!(
                "PCD point preview intermediate exceeds {MAX_PCD_PLY_BYTES} bytes"
            )));
        }
        ply.push_str(&record);
    }
    let mut page_sink = PcdPageSink {
        inner: sink,
        warnings: &warnings,
        point_count: points.coordinates.len(),
    };
    crate::cad::ply::convert(Cursor::new(ply.as_bytes()), options, &mut page_sink)?;
    Ok(warnings)
}

fn read_header<R: BufRead>(reader: &mut R) -> Result<ParsedHeader> {
    let mut fields: Option<Vec<String>> = None;
    let mut sizes: Option<Vec<usize>> = None;
    let mut types: Option<Vec<char>> = None;
    let mut counts: Option<Vec<usize>> = None;
    let mut width = None;
    let mut height = None;
    let mut point_count = None;
    let mut viewpoint_is_default = true;
    let mut version_seen = false;
    let mut header_bytes = 0usize;
    let mut warnings = Vec::new();
    let mut line = Vec::new();

    let data_mode = loop {
        let bytes_read = read_pcd_line(reader, &mut line, MAX_PCD_LINE_BYTES)?;
        if bytes_read == 0 {
            return Err(Error::InvalidInput("PCD header is missing DATA".into()));
        }
        header_bytes = header_bytes.saturating_add(bytes_read);
        if header_bytes > MAX_PCD_HEADER_BYTES {
            return Err(Error::LimitExceeded(format!(
                "PCD header exceeds {MAX_PCD_HEADER_BYTES} bytes"
            )));
        }
        let text = std::str::from_utf8(&line).map_err(|error| {
            Error::InvalidInput(format!("PCD header is not ASCII/UTF-8: {error}"))
        })?;
        let text = text.trim();
        if text.is_empty() || text.starts_with('#') {
            continue;
        }
        let mut words = text.split_whitespace();
        let key = words.next().unwrap_or_default().to_ascii_uppercase();
        let values = words.collect::<Vec<_>>();
        match key.as_str() {
            "VERSION" => {
                if version_seen || values.len() != 1 {
                    return Err(Error::InvalidInput(
                        "invalid or duplicate PCD VERSION".into(),
                    ));
                }
                version_seen = true;
                if !matches!(values[0], ".5" | ".6" | ".7" | "0.5" | "0.6" | "0.7") {
                    return Err(Error::Unsupported(format!(
                        "unsupported PCD version '{}'",
                        values[0]
                    )));
                }
            }
            "FIELDS" => {
                if fields.is_some() || values.is_empty() || values.len() > MAX_PCD_FIELDS {
                    return Err(Error::InvalidInput(
                        "invalid or duplicate PCD FIELDS".into(),
                    ));
                }
                let parsed = values
                    .iter()
                    .map(|value| (*value).to_owned())
                    .collect::<Vec<_>>();
                for index in 0..parsed.len() {
                    if parsed[..index]
                        .iter()
                        .any(|prior| prior.eq_ignore_ascii_case(&parsed[index]))
                    {
                        return Err(Error::InvalidInput("PCD field names must be unique".into()));
                    }
                }
                fields = Some(parsed);
            }
            "SIZE" => set_once(&mut sizes, parse_usize_list(&values, "SIZE")?, "SIZE")?,
            "TYPE" => set_once(
                &mut types,
                values
                    .iter()
                    .map(|value| {
                        let mut chars = value.chars();
                        let first = chars
                            .next()
                            .ok_or_else(|| Error::InvalidInput("empty PCD TYPE value".into()))?;
                        if chars.next().is_some() {
                            return Err(Error::InvalidInput(format!(
                                "invalid PCD TYPE value '{value}'"
                            )));
                        }
                        Ok(first)
                    })
                    .collect::<Result<Vec<_>>>()?,
                "TYPE",
            )?,
            "COUNT" => set_once(&mut counts, parse_usize_list(&values, "COUNT")?, "COUNT")?,
            "WIDTH" => {
                width = set_scalar_once(width, parse_single_usize(&values, "WIDTH")?, "WIDTH")?
            }
            "HEIGHT" => {
                height = set_scalar_once(height, parse_single_usize(&values, "HEIGHT")?, "HEIGHT")?
            }
            "POINTS" => {
                point_count = set_scalar_once(
                    point_count,
                    parse_single_usize(&values, "POINTS")?,
                    "POINTS",
                )?;
            }
            "VIEWPOINT" => {
                if values.len() != 7 {
                    return Err(Error::InvalidInput(
                        "PCD VIEWPOINT must contain translation and quaternion values".into(),
                    ));
                }
                let viewpoint = values
                    .iter()
                    .map(|value| value.parse::<f64>())
                    .collect::<std::result::Result<Vec<_>, _>>()
                    .map_err(|_| Error::InvalidInput("invalid PCD VIEWPOINT value".into()))?;
                viewpoint_is_default = viewpoint == [0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0];
            }
            "DATA" => {
                if values.len() != 1 {
                    return Err(Error::InvalidInput("invalid PCD DATA mode".into()));
                }
                break match values[0].to_ascii_lowercase().as_str() {
                    "ascii" => DataMode::Ascii,
                    "binary" => DataMode::Binary,
                    "binary_compressed" => DataMode::BinaryCompressed,
                    other => {
                        return Err(Error::Unsupported(format!(
                            "unsupported PCD DATA mode '{other}'"
                        )));
                    }
                };
            }
            _ => warnings.push(format!("unknown PCD header field '{key}' was ignored")),
        }
    };

    let names = fields.ok_or_else(|| Error::InvalidInput("PCD header is missing FIELDS".into()))?;
    let sizes = sizes.ok_or_else(|| Error::InvalidInput("PCD header is missing SIZE".into()))?;
    let types = types.ok_or_else(|| Error::InvalidInput("PCD header is missing TYPE".into()))?;
    if names.len() != sizes.len() || names.len() != types.len() {
        return Err(Error::InvalidInput(
            "PCD FIELDS, SIZE, and TYPE entries must have equal lengths".into(),
        ));
    }
    let counts = counts.unwrap_or_else(|| vec![1; names.len()]);
    if counts.len() != names.len() {
        return Err(Error::InvalidInput(
            "PCD COUNT must contain one value per field".into(),
        ));
    }
    let mut record_offset = 0usize;
    let mut value_offset = 0usize;
    let mut fields = Vec::with_capacity(names.len());
    for ((name, size), (kind, count)) in names
        .into_iter()
        .zip(sizes)
        .zip(types.into_iter().zip(counts))
    {
        if count == 0 || count > MAX_PCD_VALUES_PER_POINT {
            return Err(Error::LimitExceeded(format!(
                "PCD field '{name}' COUNT must be 1..={MAX_PCD_VALUES_PER_POINT}"
            )));
        }
        let scalar = ScalarType::parse(kind, size).ok_or_else(|| {
            Error::Unsupported(format!(
                "unsupported PCD field '{name}' TYPE {kind} SIZE {size}"
            ))
        })?;
        let field_bytes = scalar
            .size()
            .checked_mul(count)
            .ok_or_else(|| Error::LimitExceeded("PCD field size overflowed".into()))?;
        record_offset = record_offset
            .checked_add(field_bytes)
            .ok_or_else(|| Error::LimitExceeded("PCD point step overflowed".into()))?;
        value_offset = value_offset
            .checked_add(count)
            .ok_or_else(|| Error::LimitExceeded("PCD point value count overflowed".into()))?;
        if value_offset > MAX_PCD_VALUES_PER_POINT {
            return Err(Error::LimitExceeded(format!(
                "PCD point contains more than {MAX_PCD_VALUES_PER_POINT} scalar values"
            )));
        }
        fields.push(Field {
            name,
            scalar,
            count,
            record_offset: record_offset - field_bytes,
            value_offset: value_offset - count,
            compressed_offset: 0,
        });
    }
    if record_offset == 0 || record_offset > MAX_PCD_POINT_STEP {
        return Err(Error::LimitExceeded(format!(
            "PCD point record size {record_offset} exceeds {MAX_PCD_POINT_STEP} bytes"
        )));
    }
    let width = width.ok_or_else(|| Error::InvalidInput("PCD header is missing WIDTH".into()))?;
    let height =
        height.ok_or_else(|| Error::InvalidInput("PCD header is missing HEIGHT".into()))?;
    let point_count =
        point_count.ok_or_else(|| Error::InvalidInput("PCD header is missing POINTS".into()))?;
    if width.checked_mul(height) != Some(point_count) {
        return Err(Error::InvalidInput(
            "PCD WIDTH times HEIGHT must equal POINTS".into(),
        ));
    }
    Ok(ParsedHeader {
        fields,
        point_count,
        point_step: record_offset,
        mode: data_mode,
        warnings,
        viewpoint_is_default,
    })
}

fn read_ascii_points<R: BufRead>(
    reader: &mut R,
    config: &PointReadConfig<'_>,
    points: &mut PointAccumulator,
) -> Result<()> {
    let fields = config.fields;
    let value_count = fields.iter().map(|field| field.count).sum::<usize>();
    let mut line = Vec::new();
    for point_index in 0..config.point_count {
        let bytes = read_pcd_line(reader, &mut line, MAX_PCD_LINE_BYTES)?;
        if bytes == 0 {
            return Err(Error::InvalidInput(format!(
                "PCD ASCII data ended before point {}",
                point_index + 1
            )));
        }
        let text = std::str::from_utf8(&line).map_err(|error| {
            Error::InvalidInput(format!("PCD ASCII point data is not UTF-8: {error}"))
        })?;
        let values = text.split_whitespace().collect::<Vec<_>>();
        if values.len() != value_count {
            return Err(Error::InvalidInput(format!(
                "PCD ASCII point {} has {} values; expected {value_count}",
                point_index + 1,
                values.len()
            )));
        }
        for field in fields {
            for value in &values[field.value_offset..field.value_offset + field.count] {
                if field.scalar.parse_ascii(value).is_none() {
                    return Err(Error::InvalidInput(format!(
                        "invalid PCD ASCII value in field '{}'",
                        field.name
                    )));
                }
            }
        }
        let [x_index, y_index, z_index] = config.coordinate_fields;
        append_point(
            fields[x_index]
                .scalar
                .parse_ascii(values[fields[x_index].value_offset]),
            fields[y_index]
                .scalar
                .parse_ascii(values[fields[y_index].value_offset]),
            fields[z_index]
                .scalar
                .parse_ascii(values[fields[z_index].value_offset]),
            config
                .color_source
                .and_then(|source| pcd_color_from_ascii(fields, source, &values)),
            config.color_source.is_some(),
            points,
        );
    }
    while read_pcd_line(reader, &mut line, MAX_PCD_LINE_BYTES)? > 0 {
        if !line.iter().all(u8::is_ascii_whitespace) {
            return Err(Error::InvalidInput(
                "PCD ASCII data contains records after POINTS entries".into(),
            ));
        }
    }
    Ok(())
}

fn read_binary_points<R: Read>(
    reader: &mut R,
    config: &PointReadConfig<'_>,
    points: &mut PointAccumulator,
) -> Result<()> {
    let fields = config.fields;
    let [x_index, y_index, z_index] = config.coordinate_fields;
    let mut record = vec![0u8; config.point_step];
    for point_index in 0..config.point_count {
        reader.read_exact(&mut record).map_err(|error| {
            Error::InvalidInput(format!(
                "PCD binary data ended before point {}: {error}",
                point_index + 1
            ))
        })?;
        append_point(
            read_field_binary(&record, &fields[x_index]),
            read_field_binary(&record, &fields[y_index]),
            read_field_binary(&record, &fields[z_index]),
            config
                .color_source
                .and_then(|source| pcd_color_from_binary(&record, fields, source)),
            config.color_source.is_some(),
            points,
        );
    }
    reject_trailing_binary_data(reader)
}

fn read_compressed_points<R: Read>(
    reader: &mut R,
    config: &PointReadConfig<'_>,
    points: &mut PointAccumulator,
) -> Result<()> {
    let fields = config.fields;
    let [x_index, y_index, z_index] = config.coordinate_fields;
    let mut sizes = [0u8; 8];
    reader.read_exact(&mut sizes).map_err(|error| {
        Error::InvalidInput(format!(
            "PCD binary_compressed size header is truncated: {error}"
        ))
    })?;
    let compressed_size = u32::from_le_bytes(sizes[..4].try_into().unwrap()) as usize;
    let uncompressed_size = u32::from_le_bytes(sizes[4..].try_into().unwrap()) as usize;
    if compressed_size > MAX_PCD_DECODED_BYTES || uncompressed_size != config.expected_size {
        return Err(Error::InvalidInput(format!(
            "PCD compressed size header is inconsistent (compressed {compressed_size}, decoded {uncompressed_size}, expected {})",
            config.expected_size
        )));
    }
    let mut compressed = vec![0u8; compressed_size];
    reader.read_exact(&mut compressed).map_err(|error| {
        Error::InvalidInput(format!("PCD compressed payload is truncated: {error}"))
    })?;
    reject_trailing_binary_data(reader)?;
    let decoded = lzf_decompress(&compressed, config.expected_size)?;
    for point_index in 0..config.point_count {
        let offset_for = |field_index: usize| -> usize {
            let field = &fields[field_index];
            field.compressed_offset + point_index * (field.scalar.size() * field.count)
        };
        let coordinate = |field_index: usize| {
            let field = &fields[field_index];
            let start = offset_for(field_index);
            field
                .scalar
                .parse_binary(&decoded[start..start + field.scalar.size()])
        };
        append_point(
            coordinate(x_index),
            coordinate(y_index),
            coordinate(z_index),
            config.color_source.and_then(|source| {
                pcd_color_from_compressed(&decoded, fields, point_index, source)
            }),
            config.color_source.is_some(),
            points,
        );
    }
    Ok(())
}

fn read_field_binary(record: &[u8], field: &Field) -> Option<f64> {
    let start = field.record_offset;
    field
        .scalar
        .parse_binary(&record[start..start + field.scalar.size()])
}

fn append_point(
    x: Option<f64>,
    y: Option<f64>,
    z: Option<f64>,
    color: Option<[u8; 3]>,
    color_expected: bool,
    points: &mut PointAccumulator,
) {
    match (x, y, z) {
        (Some(x), Some(y), Some(z))
            if [x, y, z]
                .iter()
                .all(|value| value.is_finite() && value.abs() <= MAX_PCD_COORDINATE) =>
        {
            points.coordinates.push([x, y, z]);
            if color_expected && color.is_none() {
                points.invalid_color_points = points.invalid_color_points.saturating_add(1);
            }
            points.colors.push(color);
        }
        _ => points.invalid_points = points.invalid_points.saturating_add(1),
    }
}

fn field_index(fields: &[Field], name: &str) -> Result<usize> {
    fields
        .iter()
        .position(|field| field.name.eq_ignore_ascii_case(name))
        .ok_or_else(|| Error::InvalidInput(format!("PCD point cloud is missing '{name}' field")))
}

fn pcd_color_source(fields: &[Field]) -> Option<ColorSource> {
    let packed = fields
        .iter()
        .position(|field| {
            field.name.eq_ignore_ascii_case("rgb") || field.name.eq_ignore_ascii_case("rgba")
        })
        .filter(|index| {
            let field = &fields[*index];
            field.count == 1
                && matches!(field.scalar, ScalarType::F32 | ScalarType::U32)
                && field.scalar.size() == 4
        });
    if let Some(index) = packed {
        return Some(ColorSource::Packed(index));
    }
    let find_channel = |short: &str, long: &str| {
        fields
            .iter()
            .position(|field| {
                field.name.eq_ignore_ascii_case(short) || field.name.eq_ignore_ascii_case(long)
            })
            .filter(|index| {
                fields[*index].count == 1
                    && matches!(fields[*index].scalar, ScalarType::U8 | ScalarType::U16)
            })
    };
    Some(ColorSource::Channels([
        find_channel("r", "red")?,
        find_channel("g", "green")?,
        find_channel("b", "blue")?,
    ]))
}

fn pcd_color_from_ascii(fields: &[Field], source: ColorSource, values: &[&str]) -> Option<[u8; 3]> {
    match source {
        ColorSource::Packed(index) => {
            let field = &fields[index];
            let text = values[field.value_offset];
            let packed = match field.scalar {
                ScalarType::F32 => text.parse::<f32>().ok()?.to_bits(),
                ScalarType::U32 => text.parse::<u32>().ok()?,
                _ => return None,
            };
            Some(unpack_pcd_rgb(packed))
        }
        ColorSource::Channels(indices) => Some([
            pcd_color_channel(
                fields[indices[0]].scalar,
                fields[indices[0]]
                    .scalar
                    .parse_ascii(values[fields[indices[0]].value_offset])?,
            )?,
            pcd_color_channel(
                fields[indices[1]].scalar,
                fields[indices[1]]
                    .scalar
                    .parse_ascii(values[fields[indices[1]].value_offset])?,
            )?,
            pcd_color_channel(
                fields[indices[2]].scalar,
                fields[indices[2]]
                    .scalar
                    .parse_ascii(values[fields[indices[2]].value_offset])?,
            )?,
        ]),
    }
}

fn pcd_color_from_binary(record: &[u8], fields: &[Field], source: ColorSource) -> Option<[u8; 3]> {
    match source {
        ColorSource::Packed(index) => {
            let field = &fields[index];
            let start = field.record_offset;
            let packed = u32::from_le_bytes(record.get(start..start + 4)?.try_into().ok()?);
            Some(unpack_pcd_rgb(packed))
        }
        ColorSource::Channels(indices) => Some([
            pcd_color_channel(
                fields[indices[0]].scalar,
                read_field_binary(record, &fields[indices[0]])?,
            )?,
            pcd_color_channel(
                fields[indices[1]].scalar,
                read_field_binary(record, &fields[indices[1]])?,
            )?,
            pcd_color_channel(
                fields[indices[2]].scalar,
                read_field_binary(record, &fields[indices[2]])?,
            )?,
        ]),
    }
}

fn pcd_color_from_compressed(
    bytes: &[u8],
    fields: &[Field],
    point_index: usize,
    source: ColorSource,
) -> Option<[u8; 3]> {
    let field_bytes = |index: usize| {
        let field = &fields[index];
        let width = field.scalar.size() * field.count;
        let start = field
            .compressed_offset
            .checked_add(point_index.checked_mul(width)?)?;
        bytes.get(start..start + field.scalar.size())
    };
    match source {
        ColorSource::Packed(index) => {
            let packed = u32::from_le_bytes(field_bytes(index)?.try_into().ok()?);
            Some(unpack_pcd_rgb(packed))
        }
        ColorSource::Channels(indices) => Some([
            pcd_color_channel(
                fields[indices[0]].scalar,
                read_numeric_field(field_bytes(indices[0])?, fields[indices[0]].scalar)? as f64,
            )?,
            pcd_color_channel(
                fields[indices[1]].scalar,
                read_numeric_field(field_bytes(indices[1])?, fields[indices[1]].scalar)? as f64,
            )?,
            pcd_color_channel(
                fields[indices[2]].scalar,
                read_numeric_field(field_bytes(indices[2])?, fields[indices[2]].scalar)? as f64,
            )?,
        ]),
    }
}

fn read_numeric_field(bytes: &[u8], scalar: ScalarType) -> Option<f64> {
    scalar.parse_binary(bytes)
}

fn pcd_color_channel(scalar: ScalarType, value: f64) -> Option<u8> {
    match scalar {
        ScalarType::U8 => u8::try_from(value as u64)
            .ok()
            .filter(|channel| f64::from(*channel) == value),
        ScalarType::U16 if value.is_finite() && (0.0..=f64::from(u16::MAX)).contains(&value) => {
            Some(((value * 255.0 / f64::from(u16::MAX)).round()) as u8)
        }
        _ => None,
    }
}

fn unpack_pcd_rgb(packed: u32) -> [u8; 3] {
    [
        ((packed >> 16) & 0xff) as u8,
        ((packed >> 8) & 0xff) as u8,
        (packed & 0xff) as u8,
    ]
}

fn parse_usize_list(values: &[&str], key: &str) -> Result<Vec<usize>> {
    if values.is_empty() || values.len() > MAX_PCD_FIELDS {
        return Err(Error::InvalidInput(format!("invalid PCD {key} list")));
    }
    values
        .iter()
        .map(|value| {
            value
                .parse::<usize>()
                .map_err(|_| Error::InvalidInput(format!("invalid PCD {key} value '{value}'")))
        })
        .collect()
}

fn parse_single_usize(values: &[&str], key: &str) -> Result<usize> {
    if values.len() != 1 {
        return Err(Error::InvalidInput(format!("invalid PCD {key} value")));
    }
    values[0]
        .parse::<usize>()
        .map_err(|_| Error::InvalidInput(format!("invalid PCD {key} value '{}'", values[0])))
}

fn set_once<T>(slot: &mut Option<T>, value: T, key: &str) -> Result<()> {
    if slot.replace(value).is_some() {
        return Err(Error::InvalidInput(format!("duplicate PCD {key} header")));
    }
    Ok(())
}

fn set_scalar_once<T>(slot: Option<T>, value: T, key: &str) -> Result<Option<T>> {
    if slot.is_some() {
        return Err(Error::InvalidInput(format!("duplicate PCD {key} header")));
    }
    Ok(Some(value))
}

fn read_pcd_line<R: BufRead>(
    reader: &mut R,
    line: &mut Vec<u8>,
    max_bytes: usize,
) -> Result<usize> {
    line.clear();
    let mut total = 0usize;
    loop {
        let (chunk_len, has_newline) = {
            let available = reader.fill_buf()?;
            if available.is_empty() {
                return Ok(total);
            }
            let chunk_len = available
                .iter()
                .position(|byte| *byte == b'\n')
                .map_or(available.len(), |index| index + 1);
            if total.saturating_add(chunk_len) > max_bytes {
                return Err(Error::LimitExceeded(format!(
                    "PCD line exceeds {max_bytes} bytes"
                )));
            }
            (chunk_len, available[chunk_len - 1] == b'\n')
        };
        let available = reader.fill_buf()?;
        line.extend_from_slice(&available[..chunk_len]);
        reader.consume(chunk_len);
        total += chunk_len;
        if has_newline {
            return Ok(total);
        }
    }
}

fn reject_trailing_binary_data<R: Read>(reader: &mut R) -> Result<()> {
    let mut extra = [0u8; 1];
    if reader.read(&mut extra)? != 0 {
        return Err(Error::InvalidInput(
            "PCD binary data contains bytes after its declared points".into(),
        ));
    }
    Ok(())
}

fn lzf_decompress(input: &[u8], expected_size: usize) -> Result<Vec<u8>> {
    if expected_size > MAX_PCD_DECODED_BYTES {
        return Err(Error::LimitExceeded(format!(
            "PCD LZF output exceeds {MAX_PCD_DECODED_BYTES} bytes"
        )));
    }
    let mut output = Vec::with_capacity(expected_size);
    let mut cursor = 0usize;
    while cursor < input.len() {
        let control = usize::from(input[cursor]);
        cursor += 1;
        if control < 32 {
            let literal_len = control + 1;
            let end = cursor
                .checked_add(literal_len)
                .ok_or_else(|| Error::InvalidInput("PCD LZF literal size overflowed".into()))?;
            if end > input.len() || output.len().saturating_add(literal_len) > expected_size {
                return Err(Error::InvalidInput("invalid PCD LZF literal run".into()));
            }
            output.extend_from_slice(&input[cursor..end]);
            cursor = end;
            continue;
        }
        let mut length = control >> 5;
        let offset_high = (control & 0x1f) << 8;
        if length == 7 {
            let extra = input
                .get(cursor)
                .copied()
                .ok_or_else(|| Error::InvalidInput("truncated PCD LZF back-reference".into()))?;
            cursor += 1;
            length += usize::from(extra);
        }
        let offset_low = usize::from(
            *input
                .get(cursor)
                .ok_or_else(|| Error::InvalidInput("truncated PCD LZF offset".into()))?,
        );
        cursor += 1;
        let offset = offset_high + offset_low + 1;
        length += 2;
        if offset > output.len() || output.len().saturating_add(length) > expected_size {
            return Err(Error::InvalidInput("invalid PCD LZF back-reference".into()));
        }
        for _ in 0..length {
            let source = output.len() - offset;
            let byte = output[source];
            output.push(byte);
        }
    }
    if output.len() != expected_size {
        return Err(Error::InvalidInput(format!(
            "PCD LZF data decoded to {} bytes; expected {expected_size}",
            output.len()
        )));
    }
    Ok(output)
}

pub(crate) fn looks_like_pcd_prefix(bytes: &[u8]) -> bool {
    let text = String::from_utf8_lossy(bytes);
    let mut has_version = false;
    let mut has_fields = false;
    let mut has_width = false;
    let mut has_height = false;
    let mut has_points = false;
    let mut has_data = false;
    for line in text.lines().map(str::trim) {
        if line.starts_with('#') || line.is_empty() {
            continue;
        }
        let mut words = line.split_whitespace();
        let Some(key) = words.next() else {
            continue;
        };
        if key.eq_ignore_ascii_case("FIELDS") && words.next().is_some() {
            has_fields = true;
        }
        if key.eq_ignore_ascii_case("VERSION") && words.next().is_some() {
            has_version = true;
        }
        if key.eq_ignore_ascii_case("WIDTH") && words.next().is_some() {
            has_width = true;
        }
        if key.eq_ignore_ascii_case("HEIGHT") && words.next().is_some() {
            has_height = true;
        }
        if key.eq_ignore_ascii_case("POINTS") && words.next().is_some() {
            has_points = true;
        }
        if key.eq_ignore_ascii_case("DATA")
            && words.next().is_some_and(|value| {
                matches!(
                    value.to_ascii_lowercase().as_str(),
                    "ascii" | "binary" | "binary_compressed"
                )
            })
        {
            has_data = true;
            break;
        }
    }
    has_fields && has_data && (has_version || (has_width && has_height && has_points))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lzf_decompresses_overlapping_back_references() {
        let compressed = [2, b'a', b'b', b'c', 32, 2];
        assert_eq!(lzf_decompress(&compressed, 6).unwrap(), b"abcabc");
    }

    #[test]
    fn lzf_rejects_back_references_before_the_output_start() {
        assert!(matches!(
            lzf_decompress(&[32, 0], 3),
            Err(Error::InvalidInput(_))
        ));
    }

    #[test]
    fn pcd_content_sniff_requires_a_point_cloud_header() {
        assert!(looks_like_pcd_prefix(
            b"# .PCD v0.7\nVERSION .7\nFIELDS x y z\nDATA ascii\n"
        ));
        assert!(!looks_like_pcd_prefix(b"FIELDS x y z\nDATA ascii\n"));
        assert!(!looks_like_pcd_prefix(b"FIELDS x y z\nDATA random\n"));
    }
}
