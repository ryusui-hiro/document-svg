//! Stanford PLY (Polygon File Format) 3D model parser and vector polygon renderer.
//!
//! Parses ASCII and little-/big-endian binary PLY 1.0 meshes and point clouds.
//! Scalar vertex properties are located by their declared names, while extra
//! scalar fields are skipped. Input counts and face index lists are bounded
//! before allocation; projected faces use Lambertian shading and point clouds
//! use a bounded isometric point-mark path.

pub mod writer;

use std::collections::{BTreeMap, HashSet};
use std::io::{BufRead, BufReader, Read};

use crate::convert::{ConvertOptions, PageConsumer};
use crate::error::{Error, Result};
use crate::ir::{IDENTITY, LineCap, LineJoin, Node, Page, Paint, SourceMeta, Stroke};

const TARGET_PAGE_LONG_EDGE: f64 = 1200.0;
const MIN_PAGE_DIMENSION: f64 = 400.0;
const MAX_PLY_HEADER_BYTES: usize = 1024 * 1024;
const MAX_PLY_LINE_BYTES: usize = 1024 * 1024;
const MAX_PLY_VERTICES: usize = 1_000_000;
const MAX_PLY_FACES: usize = 200_000;
const MAX_PLY_POINT_MARKS: usize = 200_000;
const MAX_PLY_POINT_PATH_BYTES: usize = 32 * 1024 * 1024;
const MAX_PLY_POINT_COLOR_GROUPS: usize = 512;
const DEFAULT_PLY_POINT_COLOR: [u8; 3] = [37, 99, 235];
const PLY_POINT_MARK_RADIUS: f64 = 2.5;
const MAX_PLY_FACE_VERTICES: usize = 100_000;
const MAX_PLY_INDICES: usize = 4_000_000;
const MAX_PLY_VERTEX_PROPERTIES: usize = 128;
const MAX_PLY_COORDINATE: f64 = 1.0e12;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ByteOrder {
    Little,
    Big,
}

#[derive(Clone, Copy, Debug)]
enum Encoding {
    Ascii,
    Binary(ByteOrder),
}

#[derive(Clone, Copy, Debug)]
enum ScalarType {
    I8,
    U8,
    I16,
    U16,
    I32,
    U32,
    F32,
    F64,
}

impl ScalarType {
    fn parse(name: &str) -> Option<Self> {
        match name {
            "char" | "int8" => Some(Self::I8),
            "uchar" | "uint8" => Some(Self::U8),
            "short" | "int16" => Some(Self::I16),
            "ushort" | "uint16" => Some(Self::U16),
            "int" | "int32" => Some(Self::I32),
            "uint" | "uint32" => Some(Self::U32),
            "float" | "float32" => Some(Self::F32),
            "double" | "float64" => Some(Self::F64),
            _ => None,
        }
    }

    fn byte_width(self) -> usize {
        match self {
            Self::I8 | Self::U8 => 1,
            Self::I16 | Self::U16 => 2,
            Self::I32 | Self::U32 | Self::F32 => 4,
            Self::F64 => 8,
        }
    }

    fn is_integer(self) -> bool {
        !matches!(self, Self::F32 | Self::F64)
    }

    fn parse_ascii(self, value: &str) -> Option<f64> {
        match self {
            Self::I8 => value.parse::<i8>().ok().map(f64::from),
            Self::U8 => value.parse::<u8>().ok().map(f64::from),
            Self::I16 => value.parse::<i16>().ok().map(f64::from),
            Self::U16 => value.parse::<u16>().ok().map(f64::from),
            Self::I32 => value.parse::<i32>().ok().map(f64::from),
            Self::U32 => value.parse::<u32>().ok().map(f64::from),
            Self::F32 => crate::cad::dxf::geometry::parse_cad_float(value),
            Self::F64 => crate::cad::dxf::geometry::parse_cad_float(value),
        }
    }

    fn parse_binary(self, bytes: &[u8], order: ByteOrder) -> Option<f64> {
        if bytes.len() != self.byte_width() {
            return None;
        }
        Some(match (self, order) {
            (Self::I8, _) => i8::from_ne_bytes([bytes[0]]) as f64,
            (Self::U8, _) => bytes[0] as f64,
            (Self::I16, ByteOrder::Little) => i16::from_le_bytes(bytes.try_into().ok()?) as f64,
            (Self::I16, ByteOrder::Big) => i16::from_be_bytes(bytes.try_into().ok()?) as f64,
            (Self::U16, ByteOrder::Little) => u16::from_le_bytes(bytes.try_into().ok()?) as f64,
            (Self::U16, ByteOrder::Big) => u16::from_be_bytes(bytes.try_into().ok()?) as f64,
            (Self::I32, ByteOrder::Little) => i32::from_le_bytes(bytes.try_into().ok()?) as f64,
            (Self::I32, ByteOrder::Big) => i32::from_be_bytes(bytes.try_into().ok()?) as f64,
            (Self::U32, ByteOrder::Little) => u32::from_le_bytes(bytes.try_into().ok()?) as f64,
            (Self::U32, ByteOrder::Big) => u32::from_be_bytes(bytes.try_into().ok()?) as f64,
            (Self::F32, ByteOrder::Little) => f32::from_le_bytes(bytes.try_into().ok()?) as f64,
            (Self::F32, ByteOrder::Big) => f32::from_be_bytes(bytes.try_into().ok()?) as f64,
            (Self::F64, ByteOrder::Little) => f64::from_le_bytes(bytes.try_into().ok()?),
            (Self::F64, ByteOrder::Big) => f64::from_be_bytes(bytes.try_into().ok()?),
        })
    }

    fn parse_index_ascii(self, value: &str) -> Option<usize> {
        if !self.is_integer() {
            return None;
        }
        match self {
            Self::I8 => usize::try_from(value.parse::<i8>().ok()?).ok(),
            Self::U8 => Some(value.parse::<u8>().ok()? as usize),
            Self::I16 => usize::try_from(value.parse::<i16>().ok()?).ok(),
            Self::U16 => Some(value.parse::<u16>().ok()? as usize),
            Self::I32 => usize::try_from(value.parse::<i32>().ok()?).ok(),
            Self::U32 => usize::try_from(value.parse::<u32>().ok()?).ok(),
            Self::F32 | Self::F64 => None,
        }
    }

    fn parse_index_binary(self, bytes: &[u8], order: ByteOrder) -> Option<usize> {
        let value = self.parse_binary(bytes, order)?;
        if !self.is_integer() || value < 0.0 || value.fract() != 0.0 {
            return None;
        }
        usize::try_from(value as u64).ok()
    }
}

#[derive(Clone, Debug)]
struct VertexProperty {
    name: String,
    kind: ScalarType,
    byte_offset: usize,
}

#[derive(Clone, Copy, Debug)]
struct FaceList {
    count_type: ScalarType,
    index_type: ScalarType,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Element {
    Vertex,
    Face,
}

#[derive(Clone, Copy, Debug)]
struct Vec3 {
    x: f64,
    y: f64,
    z: f64,
}

impl Vec3 {
    fn dot(&self, other: &Vec3) -> f64 {
        self.x * other.x + self.y * other.y + self.z * other.z
    }

    fn sub(&self, other: &Vec3) -> Vec3 {
        Vec3 {
            x: self.x - other.x,
            y: self.y - other.y,
            z: self.z - other.z,
        }
    }

    fn cross(&self, other: &Vec3) -> Vec3 {
        Vec3 {
            x: self.y * other.z - self.z * other.y,
            y: self.z * other.x - self.x * other.z,
            z: self.x * other.y - self.y * other.x,
        }
    }

    fn normalize(&self) -> Vec3 {
        let len = (self.x * self.x + self.y * self.y + self.z * self.z).sqrt();
        if len > 1e-9 {
            Vec3 {
                x: self.x / len,
                y: self.y / len,
                z: self.z / len,
            }
        } else {
            Vec3 {
                x: 0.0,
                y: 0.0,
                z: 1.0,
            }
        }
    }
}

#[derive(Clone, Debug)]
struct Face {
    indices: Vec<usize>,
    normal: Vec3,
    center_depth: f64,
}

pub(crate) fn convert<R: Read>(
    reader: R,
    _options: &ConvertOptions,
    sink: &mut dyn PageConsumer,
) -> Result<Vec<String>> {
    let mut buf_reader = BufReader::new(reader);
    let mut is_ply = false;
    let mut end_header = false;
    let mut encoding = None;
    let mut vertex_count = None;
    let mut face_count = None;
    let mut current_element = None;
    let mut vertex_properties = Vec::new();
    let mut vertex_stride = 0usize;
    let mut face_list = None;
    let mut saw_vertex_element = false;
    let mut saw_face_element = false;
    let mut header_size = 0usize;

    // Parse header
    let mut line = String::new();
    loop {
        let bytes_read = read_ply_line(&mut buf_reader, &mut line, MAX_PLY_LINE_BYTES)?;
        if bytes_read == 0 {
            break;
        }
        header_size = header_size.saturating_add(bytes_read);
        if header_size > MAX_PLY_HEADER_BYTES {
            return Err(Error::LimitExceeded(format!(
                "PLY header exceeds {MAX_PLY_HEADER_BYTES} bytes"
            )));
        }
        let trimmed = line.trim();
        if trimmed == "ply" {
            if is_ply || header_size != bytes_read {
                return Err(Error::InvalidInput(
                    "PLY magic must be the first header line".into(),
                ));
            }
            is_ply = true;
        } else if !is_ply {
            return Err(Error::InvalidInput(
                "not a valid PLY file (missing 'ply' header)".into(),
            ));
        } else {
            let words: Vec<&str> = trimmed.split_whitespace().collect();
            match words.as_slice() {
                ["format", "ascii", "1.0"] => encoding = Some(Encoding::Ascii),
                ["format", "binary_little_endian", "1.0"] => {
                    encoding = Some(Encoding::Binary(ByteOrder::Little));
                }
                ["format", "binary_big_endian", "1.0"] => {
                    encoding = Some(Encoding::Binary(ByteOrder::Big));
                }
                ["format", ..] => {
                    return Err(Error::Unsupported(format!(
                        "unsupported PLY format declaration: {trimmed}"
                    )));
                }
                ["element", "vertex", count] => {
                    if saw_vertex_element || saw_face_element {
                        return Err(Error::InvalidInput(
                            "PLY must declare vertices once and before faces".into(),
                        ));
                    }
                    vertex_count = Some(parse_element_count(count, "vertex")?);
                    current_element = Some(Element::Vertex);
                    saw_vertex_element = true;
                }
                ["element", "face", count] => {
                    if saw_face_element || !saw_vertex_element {
                        return Err(Error::InvalidInput(
                            "PLY must declare faces once and after vertices".into(),
                        ));
                    }
                    face_count = Some(parse_element_count(count, "face")?);
                    current_element = Some(Element::Face);
                    saw_face_element = true;
                }
                ["element", name, ..] => {
                    return Err(Error::Unsupported(format!(
                        "PLY element '{name}' is not supported; only vertex and face elements can be rendered"
                    )));
                }
                ["property", "list", count_kind, item_kind, name]
                    if current_element == Some(Element::Face)
                        && (name.eq_ignore_ascii_case("vertex_indices")
                            || name.eq_ignore_ascii_case("vertex_index")) =>
                {
                    if face_list.is_some() {
                        return Err(Error::Unsupported(
                            "PLY faces with multiple list properties are unsupported".into(),
                        ));
                    }
                    let count_type = ScalarType::parse(count_kind).ok_or_else(|| {
                        Error::Unsupported(format!("unsupported PLY list count type {count_kind}"))
                    })?;
                    let index_type = ScalarType::parse(item_kind).ok_or_else(|| {
                        Error::Unsupported(format!("unsupported PLY vertex index type {item_kind}"))
                    })?;
                    if !count_type.is_integer() || !index_type.is_integer() {
                        return Err(Error::InvalidInput(
                            "PLY face list count and indices must use integer types".into(),
                        ));
                    }
                    face_list = Some(FaceList {
                        count_type,
                        index_type,
                    });
                }
                ["property", kind, name] if current_element == Some(Element::Vertex) => {
                    if vertex_properties.len() >= MAX_PLY_VERTEX_PROPERTIES {
                        return Err(Error::LimitExceeded(format!(
                            "PLY vertex properties exceed {MAX_PLY_VERTEX_PROPERTIES}"
                        )));
                    }
                    let kind = ScalarType::parse(kind).ok_or_else(|| {
                        Error::Unsupported(format!("unsupported PLY vertex scalar type {kind}"))
                    })?;
                    let byte_offset = vertex_stride;
                    vertex_stride =
                        vertex_stride
                            .checked_add(kind.byte_width())
                            .ok_or_else(|| {
                                Error::LimitExceeded("PLY vertex record is too large".into())
                            })?;
                    vertex_properties.push(VertexProperty {
                        name: (*name).to_owned(),
                        kind,
                        byte_offset,
                    });
                }
                ["property", "list", ..] if current_element == Some(Element::Vertex) => {
                    return Err(Error::Unsupported(
                        "PLY vertex list properties are unsupported".into(),
                    ));
                }
                ["property", ..] if current_element == Some(Element::Face) => {
                    return Err(Error::Unsupported(
                        "PLY face properties other than vertex_indices are unsupported".into(),
                    ));
                }
                ["property", ..] => {
                    return Err(Error::InvalidInput(
                        "PLY property appears before a supported element".into(),
                    ));
                }
                ["end_header"] => {
                    end_header = true;
                    break;
                }
                ["comment", ..] | ["obj_info", ..] | [] => {}
                _ => {}
            }
        }
    }

    if !is_ply || !end_header {
        return Err(Error::InvalidInput("incomplete PLY header".into()));
    }
    let encoding = encoding.ok_or_else(|| {
        Error::InvalidInput("PLY header is missing a supported format declaration".into())
    })?;
    let vertex_count = vertex_count
        .ok_or_else(|| Error::InvalidInput("PLY header is missing an element vertex".into()))?;
    let face_count = face_count.unwrap_or(0);
    if vertex_count > MAX_PLY_VERTICES {
        return Err(Error::LimitExceeded(format!(
            "PLY vertex count exceeds safety limit ({vertex_count} > {MAX_PLY_VERTICES})"
        )));
    }
    if face_count > MAX_PLY_FACES {
        return Err(Error::LimitExceeded(format!(
            "PLY face count exceeds safety limit ({face_count} > {MAX_PLY_FACES})"
        )));
    }
    if vertex_count == 0 {
        return Err(Error::InvalidInput(
            "PLY contains no renderable vertices".into(),
        ));
    }
    if vertex_properties.is_empty() || vertex_stride == 0 {
        return Err(Error::InvalidInput(
            "PLY vertex element has no scalar properties".into(),
        ));
    }
    if face_count > 0 && face_list.is_none() {
        return Err(Error::InvalidInput(
            "PLY face element has no vertex_indices list property".into(),
        ));
    }
    let axis_property = |name: &str| {
        vertex_properties
            .iter()
            .position(|property| property.name.eq_ignore_ascii_case(name))
    };
    let coordinate_properties = [axis_property("x"), axis_property("y"), axis_property("z")];
    if coordinate_properties.iter().any(Option::is_none) {
        return Err(Error::InvalidInput(
            "PLY vertices must provide scalar x, y, and z properties".into(),
        ));
    }
    let coordinate_properties = coordinate_properties.map(Option::unwrap);
    let (color_properties, has_color_properties) = ply_color_property_indices(&vertex_properties);
    let is_point_cloud = face_count == 0;

    let mut vertices = Vec::with_capacity(vertex_count);
    let mut faces = Vec::with_capacity(face_count);
    let mut vertex_colors = if is_point_cloud {
        Vec::with_capacity(vertex_count)
    } else {
        Vec::new()
    };
    let mut invalid_color_points = 0usize;
    match encoding {
        Encoding::Ascii => {
            for vertex_number in 0..vertex_count {
                read_ply_line(&mut buf_reader, &mut line, MAX_PLY_LINE_BYTES)?;
                let values = line.split_whitespace().collect::<Vec<_>>();
                if values.len() != vertex_properties.len() {
                    return Err(Error::InvalidInput(format!(
                        "PLY vertex record {} has {} values; expected {}",
                        vertex_number + 1,
                        values.len(),
                        vertex_properties.len()
                    )));
                }
                let mut coordinate = [0.0; 3];
                for (axis, property_index) in coordinate_properties.iter().copied().enumerate() {
                    let property = &vertex_properties[property_index];
                    coordinate[axis] = property
                        .kind
                        .parse_ascii(values[property_index])
                        .ok_or_else(|| {
                            Error::InvalidInput(format!(
                                "invalid PLY vertex {} property '{}'",
                                vertex_number + 1,
                                property.name
                            ))
                        })?;
                }
                vertices.push(validate_vertex(coordinate, vertex_number + 1)?);
                if is_point_cloud {
                    let color = color_properties.and_then(|indices| {
                        read_vertex_color_ascii(&vertex_properties, &values, indices)
                    });
                    if has_color_properties && color.is_none() {
                        invalid_color_points = invalid_color_points.saturating_add(1);
                    }
                    vertex_colors.push(color);
                }
            }

            if face_count > 0 {
                let face_list = face_list.ok_or_else(|| {
                    Error::InvalidInput(
                        "PLY face element has no vertex_indices list property".into(),
                    )
                })?;
                let mut total_indices = 0usize;
                for face_number in 0..face_count {
                    read_ply_line(&mut buf_reader, &mut line, MAX_PLY_LINE_BYTES)?;
                    let values = line.split_whitespace().collect::<Vec<_>>();
                    let count = face_list
                        .count_type
                        .parse_index_ascii(values.first().copied().unwrap_or_default())
                        .ok_or_else(|| {
                            Error::InvalidInput(format!(
                                "invalid PLY face {} vertex count",
                                face_number + 1
                            ))
                        })?;
                    total_indices = check_ply_index_budget(total_indices, count, face_number + 1)?;
                    if values.len() != count.saturating_add(1) {
                        return Err(Error::InvalidInput(format!(
                            "PLY face {} has {} indices; expected {count}",
                            face_number + 1,
                            values.len().saturating_sub(1)
                        )));
                    }
                    let mut indices = Vec::with_capacity(count);
                    for value in values.iter().skip(1) {
                        let index =
                            face_list
                                .index_type
                                .parse_index_ascii(value)
                                .ok_or_else(|| {
                                    Error::InvalidInput(format!(
                                        "invalid PLY vertex index in face {}",
                                        face_number + 1
                                    ))
                                })?;
                        validate_vertex_index(index, vertex_count, face_number + 1)?;
                        indices.push(index);
                    }
                    if count >= 3 {
                        faces.push(Face {
                            indices,
                            normal: Vec3 {
                                x: 0.0,
                                y: 0.0,
                                z: 1.0,
                            },
                            center_depth: 0.0,
                        });
                    }
                }
            }
        }
        Encoding::Binary(byte_order) => {
            let mut record = vec![0; vertex_stride];
            for vertex_number in 0..vertex_count {
                buf_reader.read_exact(&mut record)?;
                let mut coordinate = [0.0; 3];
                for (axis, property_index) in coordinate_properties.iter().copied().enumerate() {
                    let property = &vertex_properties[property_index];
                    let start = property.byte_offset;
                    let end = start + property.kind.byte_width();
                    coordinate[axis] = property
                        .kind
                        .parse_binary(&record[start..end], byte_order)
                        .ok_or_else(|| {
                            Error::InvalidInput(format!(
                                "invalid binary PLY vertex {} property '{}'",
                                vertex_number + 1,
                                property.name
                            ))
                        })?;
                }
                vertices.push(validate_vertex(coordinate, vertex_number + 1)?);
                if is_point_cloud {
                    let color = color_properties.and_then(|indices| {
                        read_vertex_color_binary(&vertex_properties, &record, indices, byte_order)
                    });
                    if has_color_properties && color.is_none() {
                        invalid_color_points = invalid_color_points.saturating_add(1);
                    }
                    vertex_colors.push(color);
                }
            }

            if face_count > 0 {
                let face_list = face_list.ok_or_else(|| {
                    Error::InvalidInput(
                        "PLY face element has no vertex_indices list property".into(),
                    )
                })?;
                let mut total_indices = 0usize;
                let mut scalar_bytes = [0u8; 8];
                for face_number in 0..face_count {
                    let count = read_binary_index(
                        &mut buf_reader,
                        face_list.count_type,
                        byte_order,
                        &mut scalar_bytes,
                    )?
                    .ok_or_else(|| {
                        Error::InvalidInput(format!(
                            "invalid binary PLY face {} vertex count",
                            face_number + 1
                        ))
                    })?;
                    total_indices = check_ply_index_budget(total_indices, count, face_number + 1)?;
                    let mut indices = Vec::with_capacity(count);
                    for _ in 0..count {
                        let index = read_binary_index(
                            &mut buf_reader,
                            face_list.index_type,
                            byte_order,
                            &mut scalar_bytes,
                        )?
                        .ok_or_else(|| {
                            Error::InvalidInput(format!(
                                "invalid binary PLY vertex index in face {}",
                                face_number + 1
                            ))
                        })?;
                        validate_vertex_index(index, vertex_count, face_number + 1)?;
                        indices.push(index);
                    }
                    if count >= 3 {
                        faces.push(Face {
                            indices,
                            normal: Vec3 {
                                x: 0.0,
                                y: 0.0,
                                z: 1.0,
                            },
                            center_depth: 0.0,
                        });
                    }
                }
            }
        }
    }

    if vertices.is_empty() {
        return Err(Error::InvalidInput(
            "PLY contains no renderable vertices".into(),
        ));
    }

    // Camera transform: isometric projection (rotation around X=35.264°, Y=45°)
    let cos_y = (std::f64::consts::PI / 4.0).cos();
    let sin_y = (std::f64::consts::PI / 4.0).sin();
    let angle_x = (35.264f64).to_radians();
    let cos_x = angle_x.cos();
    let sin_x = angle_x.sin();

    let project_point = |p: Vec3| -> Vec3 {
        let x1 = p.x * cos_y + p.z * sin_y;
        let y1 = p.y;
        let z1 = -p.x * sin_y + p.z * cos_y;

        let x2 = x1;
        let y2 = y1 * cos_x - z1 * sin_x;
        let z2 = y1 * sin_x + z1 * cos_x;
        Vec3 {
            x: x2,
            y: -y2,
            z: z2,
        }
    };

    let projected_vertices: Vec<Vec3> = vertices.iter().map(|&v| project_point(v)).collect();

    // Compute bounds
    let mut min_x = f64::INFINITY;
    let mut max_x = f64::NEG_INFINITY;
    let mut min_y = f64::INFINITY;
    let mut max_y = f64::NEG_INFINITY;

    for p in &projected_vertices {
        min_x = min_x.min(p.x);
        max_x = max_x.max(p.x);
        min_y = min_y.min(p.y);
        max_y = max_y.max(p.y);
    }

    let dx = (max_x - min_x).max(1e-4);
    let dy = (max_y - min_y).max(1e-4);
    let scale = (TARGET_PAGE_LONG_EDGE - 100.0) / dx.max(dy);
    let margin = 50.0;
    let width = (dx * scale + margin * 2.0).max(MIN_PAGE_DIMENSION);
    let height = (dy * scale + margin * 2.0).max(MIN_PAGE_DIMENSION);

    // Directional light vector
    let light_dir = Vec3 {
        x: 0.5,
        y: -0.8,
        z: 0.6,
    }
    .normalize();
    let ambient = 0.25;

    // Compute normals & depths
    for face in &mut faces {
        if face.indices.len() >= 3 {
            let i0 = face.indices[0];
            let i1 = face.indices[1];
            let i2 = face.indices[2];
            if i0 < vertices.len() && i1 < vertices.len() && i2 < vertices.len() {
                let v0 = vertices[i0];
                let v1 = vertices[i1];
                let v2 = vertices[i2];
                let edge1 = v1.sub(&v0);
                let edge2 = v2.sub(&v0);
                face.normal = edge1.cross(&edge2).normalize();
            }
            let mut total_depth = 0.0;
            for &idx in &face.indices {
                if idx < projected_vertices.len() {
                    total_depth += projected_vertices[idx].z;
                }
            }
            face.center_depth = total_depth / (face.indices.len() as f64);
        }
    }

    // Sort by depth (Painter's algorithm: draw farthest first)
    faces.sort_by(|a, b| {
        a.center_depth
            .partial_cmp(&b.center_depth)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    let mut page = Page::new(1, width, height, "ply-model");
    if face_count == 0 {
        if projected_vertices.len() > MAX_PLY_POINT_MARKS {
            return Err(Error::LimitExceeded(format!(
                "PLY point cloud contains {} points; maximum renderable count is {MAX_PLY_POINT_MARKS}",
                projected_vertices.len()
            )));
        }
        let mut unique_colors = HashSet::new();
        for index in 0..projected_vertices.len() {
            unique_colors.insert(
                vertex_colors
                    .get(index)
                    .copied()
                    .flatten()
                    .unwrap_or(DEFAULT_PLY_POINT_COLOR),
            );
            if unique_colors.len() > MAX_PLY_POINT_COLOR_GROUPS {
                break;
            }
        }
        let quantize_colors = unique_colors.len() > MAX_PLY_POINT_COLOR_GROUPS;
        let mut paths = BTreeMap::<[u8; 3], String>::new();
        let mut path_bytes = 0usize;
        for (index, point) in projected_vertices.iter().enumerate() {
            let sx = (point.x - min_x) * scale + margin;
            let sy = (point.y - min_y) * scale + margin;
            let circle = format!(
                "M {:.3} {:.3} a{PLY_POINT_MARK_RADIUS} {PLY_POINT_MARK_RADIUS} 0 1 0 {} 0 a{PLY_POINT_MARK_RADIUS} {PLY_POINT_MARK_RADIUS} 0 1 0 -{} 0 ",
                sx - PLY_POINT_MARK_RADIUS,
                sy,
                PLY_POINT_MARK_RADIUS * 2.0,
                PLY_POINT_MARK_RADIUS * 2.0,
            );
            path_bytes = path_bytes.saturating_add(circle.len());
            if path_bytes > MAX_PLY_POINT_PATH_BYTES {
                return Err(Error::LimitExceeded(format!(
                    "PLY point cloud SVG path exceeds {MAX_PLY_POINT_PATH_BYTES} bytes"
                )));
            }
            let color = vertex_colors
                .get(index)
                .copied()
                .flatten()
                .unwrap_or(DEFAULT_PLY_POINT_COLOR);
            let color = if quantize_colors {
                quantize_ply_color(color)
            } else {
                color
            };
            paths.entry(color).or_default().push_str(&circle);
        }
        page.title = "PLY Point Cloud".into();
        page.description = format!("PLY point cloud with {} vertices", vertices.len());
        for (index, (color, path)) in paths.into_iter().enumerate() {
            page.nodes.push(Node::Path {
                id: format!("point-cloud-{index}"),
                d: path,
                fill_rule: "nonzero".into(),
                fill: Paint::solid(format!("#{:02X}{:02X}{:02X}", color[0], color[1], color[2])),
                stroke: Stroke::default(),
                transform: IDENTITY,
                clip_id: None,
                meta: SourceMeta {
                    kind: "ply-point-cloud".into(),
                    semantic_role: "ply:point-cloud".into(),
                    ..Default::default()
                },
            });
        }
        if has_color_properties && color_properties.is_none() {
            page.warn("PLY point-cloud RGB properties were incomplete; blue markers were used");
        }
        if invalid_color_points > 0 {
            page.warn(format!(
                "{invalid_color_points} PLY point-cloud RGB value(s) were invalid; blue markers were used"
            ));
        }
        if quantize_colors {
            page.warn(format!(
                "PLY point-cloud colors were quantized to a bounded {MAX_PLY_POINT_COLOR_GROUPS}-color palette"
            ));
        }
        sink.consume(page)?;
        return Ok(Vec::new());
    }
    if faces.is_empty() {
        return Err(Error::InvalidInput(
            "PLY contains no renderable vertex faces".into(),
        ));
    }

    for (i, face) in faces.iter().enumerate() {
        if face.indices.len() < 3 {
            continue;
        }
        let mut d = String::new();
        let mut valid = true;
        for (step, &idx) in face.indices.iter().enumerate() {
            if idx >= projected_vertices.len() {
                valid = false;
                break;
            }
            let pt = projected_vertices[idx];
            let sx = (pt.x - min_x) * scale + margin;
            let sy = (pt.y - min_y) * scale + margin;
            if step == 0 {
                d.push_str(&format!("M {:.3},{:.3}", sx, sy));
            } else {
                d.push_str(&format!(" L {:.3},{:.3}", sx, sy));
            }
        }
        if !valid {
            continue;
        }
        d.push_str(" Z");

        let diffuse = face.normal.dot(&light_dir).abs();
        let intensity = (ambient + (1.0 - ambient) * diffuse).clamp(0.0, 1.0);
        let r = (intensity * 180.0) as u8;
        let g = (intensity * 200.0) as u8;
        let b = (intensity * 220.0) as u8;
        let color = format!("#{:02X}{:02X}{:02X}", r, g, b);

        page.nodes.push(Node::Path {
            id: format!("face_{i}"),
            d,
            fill_rule: "nonzero".into(),
            fill: Paint::solid(color),
            stroke: Stroke {
                paint: Paint::solid("#334155"),
                width: 0.5,
                line_cap: LineCap::Round,
                line_join: LineJoin::Round,
                ..Default::default()
            },
            transform: IDENTITY,
            clip_id: None,
            meta: SourceMeta {
                kind: "ply-face".into(),
                ..Default::default()
            },
        });
    }

    sink.consume(page)?;
    Ok(Vec::new())
}

fn ply_color_property_indices(properties: &[VertexProperty]) -> (Option<[usize; 3]>, bool) {
    let find = |long_name: &str, short_name: &str| {
        properties
            .iter()
            .position(|property| property.name.eq_ignore_ascii_case(long_name))
            .or_else(|| {
                properties
                    .iter()
                    .position(|property| property.name.eq_ignore_ascii_case(short_name))
            })
    };
    let channels = [find("red", "r"), find("green", "g"), find("blue", "b")];
    let has_color = channels.iter().any(Option::is_some);
    if channels.iter().all(Option::is_some) {
        (Some(channels.map(Option::unwrap)), has_color)
    } else {
        (None, has_color)
    }
}

fn read_vertex_color_ascii(
    properties: &[VertexProperty],
    values: &[&str],
    indices: [usize; 3],
) -> Option<[u8; 3]> {
    Some([
        color_channel(
            properties[indices[0]].kind,
            properties[indices[0]]
                .kind
                .parse_ascii(values[indices[0]])?,
        )?,
        color_channel(
            properties[indices[1]].kind,
            properties[indices[1]]
                .kind
                .parse_ascii(values[indices[1]])?,
        )?,
        color_channel(
            properties[indices[2]].kind,
            properties[indices[2]]
                .kind
                .parse_ascii(values[indices[2]])?,
        )?,
    ])
}

fn read_vertex_color_binary(
    properties: &[VertexProperty],
    record: &[u8],
    indices: [usize; 3],
    byte_order: ByteOrder,
) -> Option<[u8; 3]> {
    let channel = |index: usize| {
        let property = &properties[index];
        let start = property.byte_offset;
        let end = start.checked_add(property.kind.byte_width())?;
        color_channel(
            property.kind,
            property
                .kind
                .parse_binary(&record[start..end], byte_order)?,
        )
    };
    Some([
        channel(indices[0])?,
        channel(indices[1])?,
        channel(indices[2])?,
    ])
}

fn color_channel(kind: ScalarType, value: f64) -> Option<u8> {
    if !value.is_finite() || value < 0.0 {
        return None;
    }
    let normalized = match kind {
        ScalarType::U8 => value,
        ScalarType::U16 => {
            if value <= 255.0 {
                value
            } else if value <= f64::from(u16::MAX) {
                value * 255.0 / f64::from(u16::MAX)
            } else {
                return None;
            }
        }
        ScalarType::F32 | ScalarType::F64 => {
            if value <= 1.0 {
                value * 255.0
            } else if value <= 255.0 {
                value
            } else {
                return None;
            }
        }
        ScalarType::I8 | ScalarType::I16 | ScalarType::I32 | ScalarType::U32 => return None,
    };
    (0.0..=255.0)
        .contains(&normalized)
        .then_some(normalized.round() as u8)
}

fn quantize_ply_color(color: [u8; 3]) -> [u8; 3] {
    color.map(|channel| {
        let bucket = (u16::from(channel) * 7 + 127) / 255;
        ((bucket * 255 + 3) / 7) as u8
    })
}

fn parse_element_count(value: &str, element: &str) -> Result<usize> {
    value
        .parse::<usize>()
        .map_err(|_| Error::InvalidInput(format!("invalid PLY {element} element count: {value}")))
}

fn read_ply_line<R: BufRead>(reader: &mut R, line: &mut String, max_bytes: usize) -> Result<usize> {
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
                    "PLY line exceeds {max_bytes} bytes"
                )));
            }
            let text = std::str::from_utf8(&available[..chunk_len]).map_err(|error| {
                Error::InvalidInput(format!(
                    "PLY header/data line is not ASCII or UTF-8: {error}"
                ))
            })?;
            line.push_str(text);
            (chunk_len, available[chunk_len - 1] == b'\n')
        };
        reader.consume(chunk_len);
        total += chunk_len;
        if has_newline {
            return Ok(total);
        }
    }
}

fn validate_vertex(coordinate: [f64; 3], number: usize) -> Result<Vec3> {
    if coordinate
        .iter()
        .any(|value| !value.is_finite() || value.abs() > MAX_PLY_COORDINATE)
    {
        return Err(Error::InvalidInput(format!(
            "PLY vertex {number} has a non-finite or out-of-range coordinate"
        )));
    }
    Ok(Vec3 {
        x: coordinate[0],
        y: coordinate[1],
        z: coordinate[2],
    })
}

fn check_ply_index_budget(total: usize, count: usize, face_number: usize) -> Result<usize> {
    if count > MAX_PLY_FACE_VERTICES {
        return Err(Error::LimitExceeded(format!(
            "PLY face {face_number} exceeds {MAX_PLY_FACE_VERTICES} vertices"
        )));
    }
    let new_total = total
        .checked_add(count)
        .ok_or_else(|| Error::LimitExceeded("PLY total face index count overflowed".into()))?;
    if new_total > MAX_PLY_INDICES {
        return Err(Error::LimitExceeded(format!(
            "PLY total face indices exceed safety limit ({new_total} > {MAX_PLY_INDICES})"
        )));
    }
    Ok(new_total)
}

fn validate_vertex_index(index: usize, vertex_count: usize, face_number: usize) -> Result<()> {
    if index >= vertex_count {
        return Err(Error::InvalidInput(format!(
            "PLY face {face_number} refers to vertex {index}, but the file declares {vertex_count} vertices"
        )));
    }
    Ok(())
}

fn read_binary_index<R: Read>(
    reader: &mut R,
    kind: ScalarType,
    byte_order: ByteOrder,
    buffer: &mut [u8; 8],
) -> Result<Option<usize>> {
    let width = kind.byte_width();
    reader.read_exact(&mut buffer[..width])?;
    Ok(kind.parse_index_binary(&buffer[..width], byte_order))
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::*;

    #[derive(Default)]
    struct Pages(Vec<Page>);

    impl PageConsumer for Pages {
        fn consume(&mut self, page: Page) -> Result<()> {
            self.0.push(page);
            Ok(())
        }
    }

    fn convert_test_data(data: impl AsRef<[u8]>) -> Result<Pages> {
        let mut pages = Pages::default();
        convert(
            Cursor::new(data.as_ref()),
            &ConvertOptions::default(),
            &mut pages,
        )?;
        Ok(pages)
    }

    #[test]
    fn ascii_ply_reads_coordinates_by_declared_property_name() {
        let input = br#"ply
format ascii 1.0
element vertex 3
property float nx
property float x
property float y
property float z
property float ny
property float nz
element face 1
property list uchar uint vertex_indices
end_header
0 0 0 0 0 1
0 10 0 0 0 1
0 0 10 0 0 1
3 0 1 2
"#;
        let pages = convert_test_data(input).unwrap();
        assert_eq!(pages.0.len(), 1);
        assert_eq!(pages.0[0].nodes.len(), 1);
        assert!(matches!(pages.0[0].nodes[0], Node::Path { .. }));
    }

    #[test]
    fn binary_ply_skips_extra_vertex_properties_and_reads_u32_indices() {
        let mut input = b"ply\nformat binary_little_endian 1.0\nelement vertex 3\nproperty float x\nproperty float y\nproperty float z\nproperty float nx\nproperty float ny\nproperty float nz\nelement face 1\nproperty list uchar uint vertex_indices\nend_header\n".to_vec();
        for vertex in [
            [0.0f32, 0.0, 0.0, 0.0, 0.0, 1.0],
            [10.0, 0.0, 0.0, 0.0, 0.0, 1.0],
            [0.0, 10.0, 0.0, 0.0, 0.0, 1.0],
        ] {
            for value in vertex {
                input.extend_from_slice(&value.to_le_bytes());
            }
        }
        input.push(3);
        for index in [0u32, 1, 2] {
            input.extend_from_slice(&index.to_le_bytes());
        }

        let pages = convert_test_data(input).unwrap();
        assert_eq!(pages.0[0].nodes.len(), 1);
    }

    #[test]
    fn binary_big_endian_ply_is_supported() {
        let mut input = b"ply\nformat binary_big_endian 1.0\nelement vertex 3\nproperty float x\nproperty float y\nproperty float z\nelement face 1\nproperty list uchar int vertex_indices\nend_header\n".to_vec();
        for vertex in [[0.0f32, 0.0, 0.0], [10.0, 0.0, 0.0], [0.0, 10.0, 0.0]] {
            for value in vertex {
                input.extend_from_slice(&value.to_be_bytes());
            }
        }
        input.push(3);
        for index in [0i32, 1, 2] {
            input.extend_from_slice(&index.to_be_bytes());
        }

        let pages = convert_test_data(input).unwrap();
        assert_eq!(pages.0[0].nodes.len(), 1);
    }

    #[test]
    fn oversized_face_index_list_fails_before_allocation() {
        let input = b"ply\nformat ascii 1.0\nelement vertex 3\nproperty float x\nproperty float y\nproperty float z\nelement face 1\nproperty list uint uint vertex_indices\nend_header\n0 0 0\n1 0 0\n0 1 0\n4294967295\n";
        assert!(matches!(
            convert_test_data(input),
            Err(Error::LimitExceeded(_))
        ));
    }

    #[test]
    fn non_finite_coordinates_and_out_of_range_indices_are_rejected() {
        let non_finite = b"ply\nformat ascii 1.0\nelement vertex 3\nproperty float x\nproperty float y\nproperty float z\nelement face 1\nproperty list uchar int vertex_indices\nend_header\nNaN 0 0\n1 0 0\n0 1 0\n3 0 1 2\n";
        assert!(matches!(
            convert_test_data(non_finite),
            Err(Error::InvalidInput(_))
        ));

        let invalid_index = b"ply\nformat ascii 1.0\nelement vertex 3\nproperty float x\nproperty float y\nproperty float z\nelement face 1\nproperty list uchar int vertex_indices\nend_header\n0 0 0\n1 0 0\n0 1 0\n3 0 1 9\n";
        assert!(matches!(
            convert_test_data(invalid_index),
            Err(Error::InvalidInput(_))
        ));
    }
}
