//! CAE mesh and field renderer for ANSYS CDB, Abaqus, LS-DYNA, Nastran Bulk Data, UNV,
//! SU2, OpenFOAM, Tecplot, EnSight Gold, PLOT3D, Gmsh, and VTK formats.
//!
//! Visualizes finite element meshes (FEA) and scalar fields (stress, temperature, pressure)
//! with colormap gradient heatmaps and colorbars.

mod abaqus;
mod cdb;
mod ensight;
mod lsdyna;
mod medit;
mod nastran;
mod openfoam;
mod plot3d;
mod su2;
mod tecplot;
mod unv;
pub mod writer;

use std::borrow::Cow;
use std::collections::{HashMap, HashSet};
use std::io::Read;
use std::ops::Range;

use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use flate2::read::ZlibDecoder;
use quick_xml::Reader;
use quick_xml::events::{BytesStart, Event};

use crate::convert::{ConvertOptions, PageConsumer};
use crate::error::{Error, Result};
use crate::ir::{
    IDENTITY, LineCap, LineJoin, Node, Page, Paint, SourceMeta, Stroke, TextAnchor, TextRun,
};
use crate::ooxml::{attribute, local_name};

const TARGET_PAGE_LONG_EDGE: f64 = 1200.0;
const MIN_PAGE_DIMENSION: f64 = 400.0;
const MAX_SIMULATION_POINTS: usize = 1_000_000;
const MAX_SIMULATION_CELLS: usize = 1_000_000;
const MAX_VTK_ARRAY_TEXT_BYTES: usize = 128 * 1024 * 1024;
const MAX_VTK_ARRAY_VALUES: usize = 8_000_000;
const MAX_VTK_TOTAL_VALUES: usize = 16_000_000;
const MAX_VTK_RENDERED_PRIMITIVES: usize = 2_000_000;
const MAX_VTK_DECODED_ARRAY_BYTES: usize = 128 * 1024 * 1024;
const MAX_VTK_COMPRESSION_BLOCKS: usize = 100_000;
const MAX_VTK_APPENDED_TEXT_BYTES: usize = 192 * 1024 * 1024;
const MAX_VTK_APPENDED_BINARY_BYTES: usize = 192 * 1024 * 1024;
const MAX_VTK_LEGACY_LINE_BYTES: usize = 1024 * 1024;

#[derive(Clone, Debug)]
struct MeshNode {
    x: f64,
    y: f64,
    scalar: Option<f64>,
}

#[derive(Clone, Debug)]
struct MeshCell {
    node_ids: Vec<usize>,
    scalar: Option<f64>,
}

#[derive(Default)]
struct GmshData {
    node_scalars: HashMap<usize, f64>,
    element_scalars: HashMap<usize, f64>,
    title: Option<String>,
    warnings: Vec<String>,
}

type SimulationParseOutput = (HashMap<usize, MeshNode>, Vec<MeshCell>, String, Vec<String>);

const MAX_GMSH_BINARY_LINE_BYTES: usize = 1024 * 1024;
const MAX_GMSH_BINARY_LINES: usize = 5_000_000;
const MAX_ABAQUS_LINE_BYTES: usize = 1024 * 1024;
const MAX_ABAQUS_LINES: usize = 5_000_000;
const MAX_ABAQUS_PARTS: usize = 100_000;
const MAX_ABAQUS_INSTANCES: usize = 100_000;

#[derive(Clone, Copy, Debug)]
struct AbaqusElementTopology {
    node_count: usize,
    vtk_cell_type: usize,
    corner_count: usize,
}

#[derive(Default)]
struct AbaqusMesh {
    nodes: HashMap<usize, MeshNode>,
    z_coordinates: HashMap<usize, f64>,
    cells: Vec<MeshCell>,
    element_ids: HashSet<usize>,
}

struct AbaqusInstance {
    name: String,
    part_name: String,
    mesh: AbaqusMesh,
    translation: [f64; 3],
    rotation: Option<([f64; 3], [f64; 3], f64)>,
    position_line_count: usize,
}

#[derive(Clone, Copy)]
enum AbaqusSection {
    Ignore,
    Heading,
    Nodes,
    Elements(AbaqusElementTopology),
    InstancePosition,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum GmshByteOrder {
    Little,
    Big,
}

struct GmshBinaryReader<'a> {
    bytes: &'a [u8],
    position: usize,
    byte_order: GmshByteOrder,
    size_t_width: usize,
    lines_read: usize,
}

impl<'a> GmshBinaryReader<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self {
            bytes,
            position: 0,
            byte_order: GmshByteOrder::Little,
            size_t_width: 4,
            lines_read: 0,
        }
    }

    fn line(&mut self, context: &str) -> Result<String> {
        self.lines_read = self.lines_read.saturating_add(1);
        if self.lines_read > MAX_GMSH_BINARY_LINES {
            return Err(Error::LimitExceeded(format!(
                "Gmsh binary input exceeds {MAX_GMSH_BINARY_LINES} text lines"
            )));
        }
        if self.position >= self.bytes.len() {
            return Err(Error::InvalidInput(format!(
                "Gmsh {context} line is missing"
            )));
        }
        let tail = self.bytes.get(self.position..).ok_or_else(|| {
            Error::InvalidInput(format!("Gmsh {context} offset is outside the input"))
        })?;
        let (end, consumed) = match tail.iter().position(|byte| *byte == b'\n') {
            Some(end) => (end, end + 1),
            None => (tail.len(), tail.len()),
        };
        if end > MAX_GMSH_BINARY_LINE_BYTES {
            return Err(Error::LimitExceeded(format!(
                "Gmsh {context} line exceeds {MAX_GMSH_BINARY_LINE_BYTES} bytes"
            )));
        }
        self.position = self
            .position
            .checked_add(consumed)
            .ok_or_else(|| Error::InvalidInput("Gmsh input offset overflowed".into()))?;
        let line = &tail[..end];
        let line = line.strip_suffix(b"\r").unwrap_or(line);
        std::str::from_utf8(line)
            .map(str::to_owned)
            .map_err(|error| Error::InvalidInput(format!("Gmsh {context} is not UTF-8: {error}")))
    }

    fn binary_bytes(&mut self, count: usize, context: &str) -> Result<&'a [u8]> {
        let end = self
            .position
            .checked_add(count)
            .ok_or_else(|| Error::LimitExceeded(format!("Gmsh {context} size overflowed")))?;
        let bytes = self
            .bytes
            .get(self.position..end)
            .ok_or_else(|| Error::InvalidInput(format!("Gmsh {context} is truncated")))?;
        self.position = end;
        Ok(bytes)
    }

    fn i32(&mut self, context: &str) -> Result<i32> {
        let bytes = self.binary_bytes(4, context)?;
        let raw: [u8; 4] = bytes
            .try_into()
            .map_err(|_| Error::InvalidInput(format!("invalid Gmsh {context}")))?;
        Ok(match self.byte_order {
            GmshByteOrder::Little => i32::from_le_bytes(raw),
            GmshByteOrder::Big => i32::from_be_bytes(raw),
        })
    }

    fn u64(&mut self, context: &str) -> Result<u64> {
        let bytes = self.binary_bytes(8, context)?;
        let raw: [u8; 8] = bytes
            .try_into()
            .map_err(|_| Error::InvalidInput(format!("invalid Gmsh {context}")))?;
        Ok(match self.byte_order {
            GmshByteOrder::Little => u64::from_le_bytes(raw),
            GmshByteOrder::Big => u64::from_be_bytes(raw),
        })
    }

    fn size_t(&mut self, context: &str) -> Result<usize> {
        let value = match self.size_t_width {
            4 => {
                let bytes = self.binary_bytes(4, context)?;
                let raw: [u8; 4] = bytes
                    .try_into()
                    .map_err(|_| Error::InvalidInput(format!("invalid Gmsh {context}")))?;
                match self.byte_order {
                    GmshByteOrder::Little => u64::from(u32::from_le_bytes(raw)),
                    GmshByteOrder::Big => u64::from(u32::from_be_bytes(raw)),
                }
            }
            8 => self.u64(context)?,
            width => {
                return Err(Error::Unsupported(format!(
                    "Gmsh size_t width {width} is unsupported"
                )));
            }
        };
        usize::try_from(value).map_err(|_| {
            Error::LimitExceeded(format!(
                "Gmsh {context} exceeds this platform's address space"
            ))
        })
    }

    fn f64(&mut self, context: &str) -> Result<f64> {
        let bytes = self.binary_bytes(8, context)?;
        let raw: [u8; 8] = bytes
            .try_into()
            .map_err(|_| Error::InvalidInput(format!("invalid Gmsh {context}")))?;
        let value = match self.byte_order {
            GmshByteOrder::Little => f64::from_le_bytes(raw),
            GmshByteOrder::Big => f64::from_be_bytes(raw),
        };
        if !value.is_finite() {
            return Err(Error::InvalidInput(format!("Gmsh {context} is non-finite")));
        }
        Ok(value)
    }

    fn binary_section_end(&mut self, expected: &str) -> Result<()> {
        if self
            .bytes
            .get(self.position..)
            .is_some_and(|tail| tail.starts_with(b"\r\n"))
        {
            self.position += 2;
        } else if self.bytes.get(self.position) == Some(&b'\n') {
            self.position += 1;
        } else if !self
            .bytes
            .get(self.position..)
            .is_some_and(|tail| tail.starts_with(expected.as_bytes()))
        {
            return Err(Error::InvalidInput(format!(
                "Gmsh binary {expected} section is missing its line break"
            )));
        }
        let actual = self.line(expected)?;
        if actual.trim() != expected {
            return Err(Error::InvalidInput(format!(
                "expected Gmsh {expected}, found '{actual}'"
            )));
        }
        Ok(())
    }
}

fn gmsh_binary_header(bytes: &[u8]) -> Option<(f64, u8, usize)> {
    let mut lines = bytes.split(|byte| *byte == b'\n');
    let first = lines.next()?;
    let second = lines.next()?;
    let first = std::str::from_utf8(first).ok()?.trim();
    if first != "$MeshFormat" {
        return None;
    }
    let second = std::str::from_utf8(second).ok()?.trim();
    let mut fields = second.split_whitespace();
    let version = fields.next()?.parse::<f64>().ok()?;
    let file_type = fields.next()?.parse::<u8>().ok()?;
    let data_size = fields.next()?.parse::<usize>().ok()?;
    (file_type == 1).then_some((version, file_type, data_size))
}

fn is_vtk_legacy_binary(bytes: &[u8]) -> bool {
    let mut lines = bytes.split(|byte| *byte == b'\n');
    let Some(header) = lines.next() else {
        return false;
    };
    let _title = lines.next();
    let Some(encoding) = lines.next() else {
        return false;
    };
    let header = std::str::from_utf8(header).unwrap_or_default().trim();
    let encoding = std::str::from_utf8(encoding).unwrap_or_default().trim();
    header.starts_with("# vtk DataFile Version") && encoding.eq_ignore_ascii_case("BINARY")
}

#[derive(Default)]
struct VtkXmlPiece {
    declared_points: Option<usize>,
    declared_cells: Option<usize>,
    declared_poly_cells: HashMap<String, usize>,
    structured_extent: Option<[i64; 6]>,
    points: Option<Vec<f64>>,
    coordinates: Vec<Vec<f64>>,
    connectivity: Option<Vec<usize>>,
    offsets: Option<Vec<usize>>,
    types: Option<Vec<usize>>,
    poly_connectivity: HashMap<String, Vec<usize>>,
    poly_offsets: HashMap<String, Vec<usize>>,
    active_point_scalar: Option<String>,
    active_cell_scalar: Option<String>,
    point_scalars: Option<Vec<f64>>,
    cell_scalars: Option<Vec<f64>>,
    pending_appended_arrays: Vec<VtkXmlArray>,
    warnings: Vec<String>,
}

#[derive(Clone, Copy, Debug)]
struct VtkImageGeometry {
    origin: [f64; 3],
    spacing: [f64; 3],
    direction: [f64; 9],
    whole_extent: Option<[i64; 6]>,
}

#[derive(Default)]
struct VtkLegacyPolyData {
    vertices: Vec<Vec<usize>>,
    lines: Vec<Vec<usize>>,
    polygons: Vec<Vec<usize>>,
    triangle_strips: Vec<Vec<usize>>,
}

struct VtkLegacyBinaryReader<'a> {
    bytes: &'a [u8],
    position: usize,
    lines_read: usize,
    max_lines: usize,
}

impl<'a> VtkLegacyBinaryReader<'a> {
    fn new(bytes: &'a [u8], max_lines: usize) -> Self {
        Self {
            bytes,
            position: 0,
            lines_read: 0,
            max_lines,
        }
    }

    fn line(&mut self, context: &str) -> Result<String> {
        self.lines_read = self.lines_read.saturating_add(1);
        if self.lines_read > self.max_lines {
            return Err(Error::LimitExceeded(format!(
                "VTK legacy input exceeds {} text lines",
                self.max_lines
            )));
        }
        if self.position >= self.bytes.len() {
            return Err(Error::InvalidInput(format!(
                "VTK legacy {context} line is missing"
            )));
        }
        let tail = self.bytes.get(self.position..).ok_or_else(|| {
            Error::InvalidInput(format!("VTK legacy {context} offset is invalid"))
        })?;
        let (length, consumed) = match tail.iter().position(|byte| *byte == b'\n') {
            Some(index) => (index, index + 1),
            None => (tail.len(), tail.len()),
        };
        if length > MAX_VTK_LEGACY_LINE_BYTES {
            return Err(Error::LimitExceeded(format!(
                "VTK legacy {context} line exceeds {MAX_VTK_LEGACY_LINE_BYTES} bytes"
            )));
        }
        self.position = self
            .position
            .checked_add(consumed)
            .ok_or_else(|| Error::InvalidInput("VTK legacy file offset overflowed".into()))?;
        let bytes = tail[..length]
            .strip_suffix(b"\r")
            .unwrap_or(&tail[..length]);
        std::str::from_utf8(bytes)
            .map(str::to_owned)
            .map_err(|error| {
                Error::InvalidInput(format!("VTK legacy {context} is not UTF-8: {error}"))
            })
    }

    fn binary_block(&mut self, byte_count: usize, context: &str) -> Result<&'a [u8]> {
        let end = self
            .position
            .checked_add(byte_count)
            .filter(|end| *end <= self.bytes.len())
            .ok_or_else(|| {
                Error::InvalidInput(format!("VTK legacy {context} data is truncated"))
            })?;
        let block = &self.bytes[self.position..end];
        self.position = end;
        Ok(block)
    }

    fn skip_optional_binary_separator(&mut self) {
        if self.bytes.get(self.position..self.position + 2) == Some(b"\r\n") {
            self.position += 2;
        } else if self.bytes.get(self.position) == Some(&b'\n')
            || self.bytes.get(self.position) == Some(&b'\r')
        {
            self.position += 1;
        }
    }

    fn numeric_values(
        &mut self,
        count: usize,
        scalar_type: VtkScalarType,
        context: &str,
    ) -> Result<Vec<f64>> {
        if count > MAX_VTK_ARRAY_VALUES {
            return Err(Error::LimitExceeded(format!(
                "VTK legacy {context} exceeds {MAX_VTK_ARRAY_VALUES} values"
            )));
        }
        let byte_count = count.checked_mul(scalar_type.width()).ok_or_else(|| {
            Error::LimitExceeded(format!("VTK legacy {context} byte count overflowed"))
        })?;
        let bytes = self.binary_block(byte_count, context)?;
        let mut values = Vec::with_capacity(count);
        for chunk in bytes.chunks_exact(scalar_type.width()) {
            let value = scalar_type
                .parse_f64(chunk, VtkByteOrder::Big)
                .ok_or_else(|| {
                    Error::InvalidInput(format!("invalid VTK legacy {context} value"))
                })?;
            if !value.is_finite() {
                return Err(Error::InvalidInput(format!(
                    "VTK legacy {context} contains a non-finite value"
                )));
            }
            values.push(value);
        }
        self.skip_optional_binary_separator();
        Ok(values)
    }

    fn index_values(&mut self, count: usize, context: &str) -> Result<Vec<usize>> {
        if count > MAX_VTK_ARRAY_VALUES {
            return Err(Error::LimitExceeded(format!(
                "VTK legacy {context} exceeds {MAX_VTK_ARRAY_VALUES} indices"
            )));
        }
        let byte_count = count.checked_mul(4).ok_or_else(|| {
            Error::LimitExceeded(format!("VTK legacy {context} byte count overflowed"))
        })?;
        let bytes = self.binary_block(byte_count, context)?;
        let mut values = Vec::with_capacity(count);
        for chunk in bytes.chunks_exact(4) {
            let value = VtkScalarType::I32
                .parse_index(chunk, VtkByteOrder::Big)
                .ok_or_else(|| {
                    Error::InvalidInput(format!("invalid VTK legacy {context} index"))
                })?;
            values.push(value);
        }
        self.skip_optional_binary_separator();
        Ok(values)
    }

    fn skip_values(
        &mut self,
        count: usize,
        scalar_type: VtkScalarType,
        context: &str,
    ) -> Result<()> {
        if count > MAX_VTK_ARRAY_VALUES {
            return Err(Error::LimitExceeded(format!(
                "VTK legacy {context} exceeds {MAX_VTK_ARRAY_VALUES} values"
            )));
        }
        let byte_count = count.checked_mul(scalar_type.width()).ok_or_else(|| {
            Error::LimitExceeded(format!("VTK legacy {context} byte count overflowed"))
        })?;
        self.binary_block(byte_count, context)?;
        self.skip_optional_binary_separator();
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum VtkLegacyAssociation {
    Points,
    Cells,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum VtkXmlArrayFormat {
    Ascii,
    InlineBinary,
    Appended,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum VtkByteOrder {
    Little,
    Big,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum VtkHeaderType {
    UInt32,
    UInt64,
}

#[derive(Clone, Copy, Debug)]
enum VtkScalarType {
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

impl VtkScalarType {
    fn parse(name: &str) -> Option<Self> {
        match name {
            "Int8" => Some(Self::I8),
            "UInt8" => Some(Self::U8),
            "Int16" => Some(Self::I16),
            "UInt16" => Some(Self::U16),
            "Int32" => Some(Self::I32),
            "UInt32" => Some(Self::U32),
            "Int64" => Some(Self::I64),
            "UInt64" => Some(Self::U64),
            "Float32" => Some(Self::F32),
            "Float64" => Some(Self::F64),
            _ => None,
        }
    }

    fn width(self) -> usize {
        match self {
            Self::I8 | Self::U8 => 1,
            Self::I16 | Self::U16 => 2,
            Self::I32 | Self::U32 | Self::F32 => 4,
            Self::I64 | Self::U64 | Self::F64 => 8,
        }
    }

    fn is_integer(self) -> bool {
        !matches!(self, Self::F32 | Self::F64)
    }

    fn parse_f64(self, bytes: &[u8], order: VtkByteOrder) -> Option<f64> {
        if bytes.len() != self.width() {
            return None;
        }
        Some(match (self, order) {
            (Self::I8, _) => i8::from_ne_bytes([bytes[0]]) as f64,
            (Self::U8, _) => bytes[0] as f64,
            (Self::I16, VtkByteOrder::Little) => i16::from_le_bytes(bytes.try_into().ok()?) as f64,
            (Self::I16, VtkByteOrder::Big) => i16::from_be_bytes(bytes.try_into().ok()?) as f64,
            (Self::U16, VtkByteOrder::Little) => u16::from_le_bytes(bytes.try_into().ok()?) as f64,
            (Self::U16, VtkByteOrder::Big) => u16::from_be_bytes(bytes.try_into().ok()?) as f64,
            (Self::I32, VtkByteOrder::Little) => i32::from_le_bytes(bytes.try_into().ok()?) as f64,
            (Self::I32, VtkByteOrder::Big) => i32::from_be_bytes(bytes.try_into().ok()?) as f64,
            (Self::U32, VtkByteOrder::Little) => u32::from_le_bytes(bytes.try_into().ok()?) as f64,
            (Self::U32, VtkByteOrder::Big) => u32::from_be_bytes(bytes.try_into().ok()?) as f64,
            (Self::I64, VtkByteOrder::Little) => i64::from_le_bytes(bytes.try_into().ok()?) as f64,
            (Self::I64, VtkByteOrder::Big) => i64::from_be_bytes(bytes.try_into().ok()?) as f64,
            (Self::U64, VtkByteOrder::Little) => u64::from_le_bytes(bytes.try_into().ok()?) as f64,
            (Self::U64, VtkByteOrder::Big) => u64::from_be_bytes(bytes.try_into().ok()?) as f64,
            (Self::F32, VtkByteOrder::Little) => f32::from_le_bytes(bytes.try_into().ok()?) as f64,
            (Self::F32, VtkByteOrder::Big) => f32::from_be_bytes(bytes.try_into().ok()?) as f64,
            (Self::F64, VtkByteOrder::Little) => f64::from_le_bytes(bytes.try_into().ok()?),
            (Self::F64, VtkByteOrder::Big) => f64::from_be_bytes(bytes.try_into().ok()?),
        })
    }

    fn parse_index(self, bytes: &[u8], order: VtkByteOrder) -> Option<usize> {
        if !self.is_integer() || bytes.len() != self.width() {
            return None;
        }
        match (self, order) {
            (Self::I8, _) => usize::try_from(i8::from_ne_bytes([bytes[0]])).ok(),
            (Self::U8, _) => Some(bytes[0] as usize),
            (Self::I16, VtkByteOrder::Little) => {
                usize::try_from(i16::from_le_bytes(bytes.try_into().ok()?)).ok()
            }
            (Self::I16, VtkByteOrder::Big) => {
                usize::try_from(i16::from_be_bytes(bytes.try_into().ok()?)).ok()
            }
            (Self::U16, VtkByteOrder::Little) => {
                Some(u16::from_le_bytes(bytes.try_into().ok()?) as usize)
            }
            (Self::U16, VtkByteOrder::Big) => {
                Some(u16::from_be_bytes(bytes.try_into().ok()?) as usize)
            }
            (Self::I32, VtkByteOrder::Little) => {
                usize::try_from(i32::from_le_bytes(bytes.try_into().ok()?)).ok()
            }
            (Self::I32, VtkByteOrder::Big) => {
                usize::try_from(i32::from_be_bytes(bytes.try_into().ok()?)).ok()
            }
            (Self::U32, VtkByteOrder::Little) => {
                usize::try_from(u32::from_le_bytes(bytes.try_into().ok()?)).ok()
            }
            (Self::U32, VtkByteOrder::Big) => {
                usize::try_from(u32::from_be_bytes(bytes.try_into().ok()?)).ok()
            }
            (Self::I64, VtkByteOrder::Little) => {
                usize::try_from(i64::from_le_bytes(bytes.try_into().ok()?)).ok()
            }
            (Self::I64, VtkByteOrder::Big) => {
                usize::try_from(i64::from_be_bytes(bytes.try_into().ok()?)).ok()
            }
            (Self::U64, VtkByteOrder::Little) => {
                usize::try_from(u64::from_le_bytes(bytes.try_into().ok()?)).ok()
            }
            (Self::U64, VtkByteOrder::Big) => {
                usize::try_from(u64::from_be_bytes(bytes.try_into().ok()?)).ok()
            }
            (Self::F32 | Self::F64, _) => None,
        }
    }
}

struct VtkXmlArray {
    context: String,
    name: Option<String>,
    components: usize,
    scalar_type: VtkScalarType,
    format: VtkXmlArrayFormat,
    offset: Option<usize>,
    text: String,
}

pub(crate) fn convert<R: Read>(
    input: R,
    options: &ConvertOptions,
    sink: &mut dyn PageConsumer,
) -> Result<Vec<String>> {
    let mut warnings = Vec::new();
    let mut bytes = Vec::new();
    Read::take(input, options.max_input_bytes.saturating_add(1)).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > options.max_input_bytes {
        return Err(Error::LimitExceeded(format!(
            "Simulation file exceeds maximum limit of {}",
            options.max_input_bytes
        )));
    }
    let (nodes, cells, title) = if let Some((version, _, _)) = gmsh_binary_header(&bytes) {
        let (nodes, cells, title, gmsh_warnings) = if version == 2.2 {
            parse_gmsh_binary_v22(&bytes)?
        } else if (4.0..4.1).contains(&version) {
            parse_gmsh_binary_v40(&bytes)?
        } else if (4.1..4.2).contains(&version) {
            parse_gmsh_binary_v41(&bytes)?
        } else {
            return Err(Error::Unsupported(format!(
                "binary Gmsh MSH version {version} is unsupported; use binary MSH 2.2, 4.0, or 4.1"
            )));
        };
        warnings.extend(gmsh_warnings);
        (nodes, cells, title)
    } else if is_vtk_legacy_binary(&bytes) {
        let (nodes, cells, title, vtk_warnings) = parse_vtk_binary(&bytes, options.max_xml_events)?;
        warnings.extend(vtk_warnings);
        (nodes, cells, title)
    } else if let Some((xml, appended_range)) =
        prepare_vtk_xml_raw_appended(&bytes, options.max_xml_events)?
    {
        let (nodes, cells, title, vtk_warnings) = parse_vtk_xml_with_appended(
            &xml,
            options.max_xml_events,
            Some(&bytes[appended_range]),
        )?;
        warnings.extend(vtk_warnings);
        (nodes, cells, title)
    } else {
        let text = String::from_utf8(bytes).map_err(|error| {
            Error::Unsupported(format!(
                "binary simulation data is not supported by this renderer: {error}"
            ))
        })?;

        if text.contains("$MeshFormat") || text.contains("$Nodes") {
            let (nodes, cells, title, gmsh_warnings) = parse_gmsh(&text)?;
            warnings.extend(gmsh_warnings);
            (nodes, cells, title)
        } else if text.contains("<VTKFile") {
            let (nodes, cells, title, vtk_warnings) = parse_vtk_xml(&text, options.max_xml_events)?;
            warnings.extend(vtk_warnings);
            (nodes, cells, title)
        } else if text.starts_with("ply") || text.contains("format ascii") {
            parse_ply(&text)?
        } else {
            let (nodes, cells, title, vtk_warnings) = parse_vtk(&text, options.max_xml_events)?;
            warnings.extend(vtk_warnings);
            (nodes, cells, title)
        }
    };

    render_simulation((nodes, cells, title, warnings), sink)
}

pub(crate) fn convert_abaqus<R: Read>(
    input: R,
    options: &ConvertOptions,
    sink: &mut dyn PageConsumer,
) -> Result<Vec<String>> {
    abaqus::convert(input, options, sink)
}

pub(crate) fn looks_like_cdb_prefix(prefix: &[u8]) -> bool {
    cdb::looks_like_prefix(prefix)
}

pub(crate) fn convert_cdb<R: Read>(
    input: R,
    options: &ConvertOptions,
    sink: &mut dyn PageConsumer,
) -> Result<Vec<String>> {
    cdb::convert(input, options, sink)
}

pub(crate) fn convert_nastran<R: Read>(
    input: R,
    options: &ConvertOptions,
    sink: &mut dyn PageConsumer,
) -> Result<Vec<String>> {
    nastran::convert(input, options, sink)
}

pub(crate) fn convert_lsdyna<R: Read>(
    input: R,
    options: &ConvertOptions,
    sink: &mut dyn PageConsumer,
) -> Result<Vec<String>> {
    lsdyna::convert(input, options, sink)
}

pub(crate) fn convert_medit<R: Read>(
    input: R,
    options: &ConvertOptions,
    sink: &mut dyn PageConsumer,
) -> Result<Vec<String>> {
    medit::convert(input, options, sink)
}

pub(crate) fn looks_like_unv_prefix(prefix: &[u8]) -> bool {
    unv::looks_like_unv_prefix(prefix)
}

pub(crate) fn convert_unv<R: Read>(
    input: R,
    options: &ConvertOptions,
    sink: &mut dyn PageConsumer,
) -> Result<Vec<String>> {
    unv::convert(input, options, sink)
}

pub(crate) fn looks_like_su2_prefix(prefix: &[u8]) -> bool {
    su2::looks_like_prefix(prefix)
}

pub(crate) fn convert_su2<R: Read>(
    input: R,
    options: &ConvertOptions,
    sink: &mut dyn PageConsumer,
) -> Result<Vec<String>> {
    su2::convert(input, options, sink)
}

pub(crate) fn convert_openfoam(
    input: &std::path::Path,
    options: &ConvertOptions,
    sink: &mut dyn PageConsumer,
) -> Result<Vec<String>> {
    openfoam::convert(input, options, sink)
}

pub(crate) fn convert_tecplot<R: Read>(
    input: R,
    options: &ConvertOptions,
    sink: &mut dyn PageConsumer,
) -> Result<Vec<String>> {
    tecplot::convert(input, options, sink)
}

pub(crate) fn looks_like_tecplot_prefix(prefix: &[u8]) -> bool {
    tecplot::looks_like_prefix(prefix)
}

pub(crate) fn convert_ensight_case(
    path: &std::path::Path,
    options: &ConvertOptions,
    sink: &mut dyn PageConsumer,
) -> Result<Vec<String>> {
    ensight::convert_case(path, options, sink)
}

pub(crate) fn convert_ensight_geometry<R: Read>(
    input: R,
    options: &ConvertOptions,
    sink: &mut dyn PageConsumer,
) -> Result<Vec<String>> {
    ensight::convert_geometry(input, options, sink)
}

pub(crate) fn looks_like_ensight_prefix(prefix: &[u8]) -> bool {
    ensight::looks_like_prefix(prefix)
}

pub(crate) fn convert_plot3d<R: Read>(
    input: R,
    options: &ConvertOptions,
    sink: &mut dyn PageConsumer,
) -> Result<Vec<String>> {
    plot3d::convert(input, options, sink)
}

pub(crate) fn looks_like_plot3d_prefix(prefix: &[u8]) -> bool {
    plot3d::looks_like_prefix(prefix)
}

pub(super) struct SimulationViewport {
    min_x: f64,
    max_y: f64,
    span_w: f64,
    span_h: f64,
    plot_origin_x: f64,
    plot_origin_y: f64,
    plot_w: f64,
    plot_h: f64,
    page_w: f64,
    page_h: f64,
}

impl SimulationViewport {
    pub(super) fn new(min_x: f64, min_y: f64, max_x: f64, max_y: f64, has_scalars: bool) -> Self {
        let span_w = max_x - min_x;
        let span_h = max_y - min_y;
        let margin = 24.0;
        let legend_reserve = if has_scalars { 80.0 } else { 0.0 };
        let plot_long_edge =
            (TARGET_PAGE_LONG_EDGE - 2.0 * margin - legend_reserve).max(MIN_PAGE_DIMENSION);
        let max_span = span_w.max(span_h);
        let (plot_w, plot_h) = if max_span > 0.0 {
            if span_w >= span_h {
                (plot_long_edge, (span_h / span_w) * plot_long_edge)
            } else {
                ((span_w / span_h) * plot_long_edge, plot_long_edge)
            }
        } else {
            (1.0, 1.0)
        };
        let page_w = (plot_w + 2.0 * margin + legend_reserve).max(MIN_PAGE_DIMENSION);
        let page_h = (plot_h + 2.0 * margin).max(MIN_PAGE_DIMENSION);
        let plot_origin_x =
            margin + ((page_w - 2.0 * margin - legend_reserve - plot_w) * 0.5).max(0.0);
        let plot_origin_y = margin + ((page_h - 2.0 * margin - plot_h) * 0.5).max(0.0);
        Self {
            min_x,
            max_y,
            span_w,
            span_h,
            plot_origin_x,
            plot_origin_y,
            plot_w,
            plot_h,
            page_w,
            page_h,
        }
    }

    pub(super) fn map(&self, x: f64, y: f64) -> (f64, f64) {
        let sx = if self.span_w > 0.0 {
            self.plot_origin_x + ((x - self.min_x) / self.span_w) * self.plot_w
        } else {
            self.plot_origin_x + self.plot_w * 0.5
        };
        let sy = if self.span_h > 0.0 {
            self.plot_origin_y + ((self.max_y - y) / self.span_h) * self.plot_h
        } else {
            self.plot_origin_y + self.plot_h * 0.5
        };
        (sx, sy)
    }
}

fn render_simulation(
    (nodes, cells, title, mut warnings): SimulationParseOutput,
    sink: &mut dyn PageConsumer,
) -> Result<Vec<String>> {
    if nodes.is_empty() {
        warnings.push("Simulation file contains no node coordinates".into());
    }
    let use_cell_scalars = cells.iter().any(|cell| cell.scalar.is_some());
    let has_node_scalars = nodes.values().any(|node| node.scalar.is_some());
    if use_cell_scalars && has_node_scalars {
        warnings.push(
            "both point and cell scalar arrays are present; the color map uses cell values".into(),
        );
    }

    let mut min_x = f64::INFINITY;
    let mut min_y = f64::INFINITY;
    let mut max_x = f64::NEG_INFINITY;
    let mut max_y = f64::NEG_INFINITY;
    let mut min_val = f64::INFINITY;
    let mut max_val = f64::NEG_INFINITY;

    for node in nodes.values() {
        if node.x < min_x {
            min_x = node.x;
        }
        if node.x > max_x {
            max_x = node.x;
        }
        if node.y < min_y {
            min_y = node.y;
        }
        if node.y > max_y {
            max_y = node.y;
        }
        if !use_cell_scalars && let Some(s) = node.scalar {
            if s < min_val {
                min_val = s;
            }
            if s > max_val {
                max_val = s;
            }
        }
    }

    for cell in &cells {
        if use_cell_scalars && let Some(s) = cell.scalar {
            if s < min_val {
                min_val = s;
            }
            if s > max_val {
                max_val = s;
            }
        }
    }

    let has_scalars = min_val < max_val && min_val.is_finite() && max_val.is_finite();
    let span_w = max_x - min_x;
    let span_h = max_y - min_y;
    if !span_w.is_finite() || !span_h.is_finite() || span_w < 0.0 || span_h < 0.0 {
        return Err(Error::InvalidInput(
            "simulation coordinate range exceeds the supported numeric range".into(),
        ));
    }
    let viewport = SimulationViewport::new(min_x, min_y, max_x, max_y, has_scalars);
    let page_w = viewport.page_w;
    let page_h = viewport.page_h;

    let mut page = Page::new(1, page_w, page_h, "simulation");
    page.title = title;

    let map_pt = |x: f64, y: f64| viewport.map(x, y);

    // Dark background for high-contrast engineering visualization
    page.nodes.push(Node::Path {
        id: "sim-background".into(),
        d: format!(
            "M 0 0 L {} 0 L {} {} L 0 {} Z",
            fmt_coord(page_w),
            fmt_coord(page_w),
            fmt_coord(page_h),
            fmt_coord(page_h)
        ),
        fill_rule: "nonzero".into(),
        fill: Paint::solid("#090d16"), // Deep navy/black
        stroke: Stroke::default(),
        transform: IDENTITY,
        clip_id: None,
        meta: SourceMeta {
            semantic_role: "simulation:background".into(),
            ..Default::default()
        },
    });

    let mesh_stroke = Stroke {
        paint: Paint::solid("#94a3b8"), // High-contrast slate wireframe
        width: 0.75,
        line_cap: LineCap::Round,
        line_join: LineJoin::Round,
        miter_limit: 4.0,
        dash_array: Vec::new(),
        dash_offset: 0.0,
    };

    let mut cell_nodes = Vec::new();

    for cell in cells {
        if cell.node_ids.len() < 2 {
            continue;
        }

        let mut pts = Vec::new();
        let mut node_scalar_mean = 0.0;
        let mut node_scalar_count = 0;

        for id in &cell.node_ids {
            if let Some(n) = nodes.get(id) {
                pts.push(map_pt(n.x, n.y));
                if let Some(s) = n.scalar {
                    node_scalar_count += 1;
                    let count = node_scalar_count as f64;
                    node_scalar_mean = node_scalar_mean * ((count - 1.0) / count) + s / count;
                }
            }
        }

        if pts.len() < 2 {
            continue;
        }

        let cell_val = if use_cell_scalars {
            cell.scalar
        } else if node_scalar_count > 0 {
            Some(node_scalar_mean)
        } else {
            None
        };

        let fill_paint = if let Some(val) = cell_val {
            let scalar_scale = min_val.abs().max(max_val.abs()).max(1.0);
            let normalized_min = min_val / scalar_scale;
            let normalized_range = (max_val / scalar_scale - normalized_min).max(1e-9);
            let t = ((val / scalar_scale - normalized_min) / normalized_range).clamp(0.0, 1.0);
            Paint::solid(jet_colormap(t))
        } else {
            Paint::solid("#1e293b") // Default solid slate
        };

        let mut d = String::new();
        for (i, p) in pts.iter().enumerate() {
            if i == 0 {
                d.push_str(&format!("M {} {}", fmt_coord(p.0), fmt_coord(p.1)));
            } else {
                d.push_str(&format!(" L {} {}", fmt_coord(p.0), fmt_coord(p.1)));
            }
        }
        if pts.len() > 2 {
            d.push_str(" Z");
        }

        cell_nodes.push(Node::Path {
            id: String::new(),
            d,
            fill_rule: "nonzero".into(),
            fill: if pts.len() > 2 {
                fill_paint.clone()
            } else {
                Paint::None
            },
            stroke: if pts.len() == 2 && cell_val.is_some() {
                Stroke {
                    paint: fill_paint.clone(),
                    width: 1.25,
                    ..mesh_stroke.clone()
                }
            } else {
                mesh_stroke.clone()
            },
            transform: IDENTITY,
            clip_id: None,
            meta: SourceMeta::default(),
        });
    }

    if !cell_nodes.is_empty() {
        page.nodes.push(Node::Group {
            id: "mesh-cells".into(),
            nodes: cell_nodes,
            transform: IDENTITY,
            opacity: 1.0,
            clip_id: None,
            meta: SourceMeta {
                semantic_role: "simulation:mesh".into(),
                ..Default::default()
            },
        });
    }

    // Render Colorbar Legend if scalar fields exist
    if has_scalars {
        let bar_x = page_w - 65.0;
        let bar_top = 80.0;
        let bar_bottom = page_h - 80.0;
        let bar_w = 16.0;
        let bar_h = bar_bottom - bar_top;
        let steps = 24;

        for s in 0..steps {
            let t0 = 1.0 - (s as f64 / steps as f64);
            let y_pos = bar_top + (s as f64 / steps as f64) * bar_h;
            let step_h = bar_h / steps as f64;
            let color = jet_colormap(t0);

            page.nodes.push(Node::Path {
                id: String::new(),
                d: format!(
                    "M {} {} H {} V {} H {} Z",
                    fmt_coord(bar_x),
                    fmt_coord(y_pos),
                    fmt_coord(bar_x + bar_w),
                    fmt_coord(y_pos + step_h + 0.5),
                    fmt_coord(bar_x)
                ),
                fill_rule: "nonzero".into(),
                fill: Paint::solid(color),
                stroke: Stroke::default(),
                transform: IDENTITY,
                clip_id: None,
                meta: SourceMeta::default(),
            });
        }

        // Colorbar labels (Max and Min)
        page.nodes.push(Node::Text {
            id: "colorbar-max".into(),
            x: bar_x + bar_w + 6.0,
            y: bar_top + 10.0,
            runs: vec![TextRun {
                text: format!("{max_val:.2e}"),
                font_family: "monospace, monospace".into(),
                font_size: 11.0,
                bold: false,
                italic: false,
                fill: Paint::solid("#f8fafc"),
                baseline_shift: 0.0,
                glyph_x_offsets: Vec::new(),
                target_advance: None,
            }],
            anchor: TextAnchor::Start,
            transform: IDENTITY,
            opacity: 1.0,
            stroke: Stroke::default(),
            clip_id: None,
            meta: SourceMeta::default(),
        });

        page.nodes.push(Node::Text {
            id: "colorbar-min".into(),
            x: bar_x + bar_w + 6.0,
            y: bar_bottom,
            runs: vec![TextRun {
                text: format!("{min_val:.2e}"),
                font_family: "monospace, monospace".into(),
                font_size: 11.0,
                bold: false,
                italic: false,
                fill: Paint::solid("#f8fafc"),
                baseline_shift: 0.0,
                glyph_x_offsets: Vec::new(),
                target_advance: None,
            }],
            anchor: TextAnchor::Start,
            transform: IDENTITY,
            opacity: 1.0,
            stroke: Stroke::default(),
            clip_id: None,
            meta: SourceMeta::default(),
        });
    }

    sink.consume(page)?;
    Ok(warnings)
}

/// Computes RGB hex string using standard Jet/Rainbow colormap for t in [0.0, 1.0].
fn jet_colormap(t: f64) -> &'static str {
    // 16-step smooth jet colormap table
    const JET_TABLE: [&str; 16] = [
        "#000080", "#0000d0", "#0020ff", "#0070ff", "#00b0ff", "#00e0e0", "#00ff90", "#40ff40",
        "#90ff00", "#e0e000", "#ffb000", "#ff7000", "#ff2000", "#d00000", "#900000", "#600000",
    ];
    let idx = ((t * (JET_TABLE.len() - 1) as f64).round() as usize).min(JET_TABLE.len() - 1);
    JET_TABLE[idx]
}

fn parse_gmsh_data_sections(text: &str) -> Result<GmshData> {
    let lines = text.lines().map(str::trim).collect::<Vec<_>>();
    let mut data = GmshData::default();
    let mut selected_node_view = false;
    let mut selected_element_view = false;
    let mut index = 0usize;

    while index < lines.len() {
        let (is_node_data, end_marker) = match lines[index] {
            "$NodeData" => (true, "$EndNodeData"),
            "$ElementData" => (false, "$EndElementData"),
            "$ElementNodeData" => {
                let end = lines[index + 1..]
                    .iter()
                    .position(|line| *line == "$EndElementNodeData")
                    .map(|offset| index + 1 + offset)
                    .ok_or_else(|| {
                        Error::InvalidInput("Gmsh $ElementNodeData is not terminated".into())
                    })?;
                data.warnings.push(
                    "Gmsh ElementNodeData is not rendered; use NodeData or ElementData scalar views".into(),
                );
                index = end + 1;
                continue;
            }
            _ => {
                index += 1;
                continue;
            }
        };

        let section_name = lines[index];
        let end = lines[index + 1..]
            .iter()
            .position(|line| *line == end_marker)
            .map(|offset| index + 1 + offset)
            .ok_or_else(|| Error::InvalidInput(format!("Gmsh {section_name} is not terminated")))?;
        let mut cursor = index + 1;
        let string_tag_count = parse_gmsh_header_count(lines.get(cursor), "string tag count")?;
        cursor += 1;
        if string_tag_count > 64 {
            return Err(Error::LimitExceeded(
                "Gmsh data string tag count exceeds 64".into(),
            ));
        }
        let mut view_name = None;
        for tag_index in 0..string_tag_count {
            let tag = lines.get(cursor).ok_or_else(|| {
                Error::InvalidInput(format!("Gmsh {section_name} has a missing string tag"))
            })?;
            if tag_index == 0 {
                view_name = Some(tag.trim_matches('"').to_owned());
            }
            cursor += 1;
        }
        let real_tag_count = parse_gmsh_header_count(lines.get(cursor), "real tag count")?;
        cursor += 1;
        if real_tag_count > 64 {
            return Err(Error::LimitExceeded(
                "Gmsh data real tag count exceeds 64".into(),
            ));
        }
        for _ in 0..real_tag_count {
            let value = lines.get(cursor).ok_or_else(|| {
                Error::InvalidInput(format!("Gmsh {section_name} has a missing real tag"))
            })?;
            value.parse::<f64>().map_err(|_| {
                Error::InvalidInput(format!("invalid Gmsh {section_name} real tag '{value}'"))
            })?;
            cursor += 1;
        }
        let integer_tag_count = parse_gmsh_header_count(lines.get(cursor), "integer tag count")?;
        cursor += 1;
        if integer_tag_count > 64 {
            return Err(Error::LimitExceeded(
                "Gmsh data integer tag count exceeds 64".into(),
            ));
        }
        let mut integer_tags = Vec::with_capacity(integer_tag_count);
        for _ in 0..integer_tag_count {
            let value = lines.get(cursor).ok_or_else(|| {
                Error::InvalidInput(format!("Gmsh {section_name} has a missing integer tag"))
            })?;
            integer_tags.push(value.parse::<i64>().map_err(|_| {
                Error::InvalidInput(format!("invalid Gmsh {section_name} integer tag '{value}'"))
            })?);
            cursor += 1;
        }
        if integer_tags.len() < 3 {
            return Err(Error::InvalidInput(format!(
                "Gmsh {section_name} must provide time step, component count, and entry count"
            )));
        }
        let component_count = usize::try_from(integer_tags[1]).map_err(|_| {
            Error::InvalidInput(format!("invalid Gmsh {section_name} component count"))
        })?;
        let entry_count = usize::try_from(integer_tags[2])
            .map_err(|_| Error::InvalidInput(format!("invalid Gmsh {section_name} entry count")))?;
        if component_count == 0 || component_count > 9 {
            data.warnings.push(format!(
                "Gmsh {section_name} view '{}' with {component_count} components was skipped",
                view_name.as_deref().unwrap_or("unnamed")
            ));
            index = end + 1;
            continue;
        }
        if entry_count > MAX_SIMULATION_POINTS.max(MAX_SIMULATION_CELLS)
            || entry_count
                .checked_mul(component_count.saturating_add(1))
                .is_none_or(|count| count > MAX_VTK_TOTAL_VALUES)
        {
            return Err(Error::LimitExceeded(format!(
                "Gmsh {section_name} data exceeds the bounded entry/value limit"
            )));
        }
        let selected = if is_node_data {
            if selected_node_view {
                false
            } else {
                selected_node_view = true;
                true
            }
        } else if selected_element_view {
            false
        } else {
            selected_element_view = true;
            true
        };
        if !selected {
            data.warnings.push(format!(
                "additional Gmsh {section_name} view was ignored; only the first view is rendered"
            ));
            index = end + 1;
            continue;
        }
        if component_count > 1 {
            data.warnings.push(format!(
                "Gmsh {section_name} view '{}' has {component_count} components; preview colors use Euclidean magnitude, not domain-specific equivalent stress",
                view_name.as_deref().unwrap_or("unnamed")
            ));
        }
        if let Some(name) = view_name {
            data.title = Some(match data.title.take() {
                Some(previous) => format!("{previous} / {name}"),
                None => name,
            });
        }

        let mut values = lines[cursor..end]
            .iter()
            .copied()
            .flat_map(str::split_whitespace);
        let target = if is_node_data {
            &mut data.node_scalars
        } else {
            &mut data.element_scalars
        };
        for _ in 0..entry_count {
            let tag = next_gmsh_value::<usize>(&mut values, "data record tag")?;
            if tag == 0 {
                return Err(Error::InvalidInput(format!(
                    "Gmsh {section_name} record tag must be positive"
                )));
            }
            let mut magnitude = 0.0f64;
            let mut scalar = None;
            for component_index in 0..component_count {
                let value = next_gmsh_value::<f64>(&mut values, "data record value")?;
                if !value.is_finite() {
                    return Err(Error::InvalidInput(format!(
                        "Gmsh {section_name} contains a non-finite data value"
                    )));
                }
                magnitude = magnitude.hypot(value);
                if component_index == 0 {
                    scalar = Some(value);
                }
            }
            let preview_value = if component_count == 1 {
                scalar.unwrap_or(0.0)
            } else {
                magnitude
            };
            if !preview_value.is_finite() {
                return Err(Error::InvalidInput(format!(
                    "Gmsh {section_name} vector magnitude overflows the supported numeric range"
                )));
            }
            if target.insert(tag, preview_value).is_some() {
                return Err(Error::InvalidInput(format!(
                    "duplicate Gmsh {section_name} data tag {tag}"
                )));
            }
        }
        if values.next().is_some() {
            return Err(Error::InvalidInput(format!(
                "Gmsh {section_name} contains more data than its declared entry count"
            )));
        }
        index = end + 1;
    }
    Ok(data)
}

fn parse_gmsh_header_count(value: Option<&&str>, context: &str) -> Result<usize> {
    value
        .ok_or_else(|| Error::InvalidInput(format!("missing Gmsh {context}")))?
        .parse::<usize>()
        .map_err(|_| Error::InvalidInput(format!("invalid Gmsh {context}")))
}

fn parse_gmsh(text: &str) -> Result<SimulationParseOutput> {
    let data = parse_gmsh_data_sections(text)?;
    let lines = text.lines().map(str::trim).collect::<Vec<_>>();
    let (mut nodes, cells, mut title) =
        if let Some(mesh_format_index) = lines.iter().position(|line| *line == "$MeshFormat") {
            let format_line = lines.get(mesh_format_index + 1).ok_or_else(|| {
                Error::InvalidInput("Gmsh file has an incomplete $MeshFormat section".into())
            })?;
            let mut values = format_line.split_whitespace();
            let version = values
                .next()
                .and_then(|value| value.parse::<f64>().ok())
                .ok_or_else(|| Error::InvalidInput("invalid Gmsh MSH version".into()))?;
            let file_type = values
                .next()
                .and_then(|value| value.parse::<u8>().ok())
                .ok_or_else(|| Error::InvalidInput("invalid Gmsh file type".into()))?;
            if file_type != 0 {
                return Err(Error::Unsupported(
                    "binary Gmsh MSH files are not supported; export ASCII MSH".into(),
                ));
            }
            if (4.1..4.2).contains(&version) {
                parse_gmsh_v41(&lines, &data)?
            } else if (4.0..4.1).contains(&version) {
                parse_gmsh_v40(&lines, &data)?
            } else if (2.0..3.0).contains(&version) {
                parse_gmsh_v2(text, &data)?
            } else {
                return Err(Error::Unsupported(format!(
                    "Gmsh MSH version {version} is not supported; use ASCII MSH 2.x, 4.0, or 4.1"
                )));
            }
        } else {
            parse_gmsh_v2(text, &data)?
        };
    for (tag, value) in &data.node_scalars {
        if let Some(node) = nodes.get_mut(tag) {
            node.scalar = Some(*value);
        }
    }
    if let Some(name) = data.title.as_deref() {
        title = format!("{title}: {name}");
    }
    Ok((nodes, cells, title, data.warnings))
}

fn parse_gmsh_binary_v22(bytes: &[u8]) -> Result<SimulationParseOutput> {
    let mut reader = GmshBinaryReader::new(bytes);
    if reader.line("mesh format marker")?.trim() != "$MeshFormat" {
        return Err(Error::InvalidInput(
            "binary Gmsh file is missing $MeshFormat".into(),
        ));
    }
    let format_line = reader.line("mesh format header")?;
    let mut fields = format_line.split_whitespace();
    let version = fields
        .next()
        .and_then(|value| value.parse::<f64>().ok())
        .ok_or_else(|| Error::InvalidInput("invalid Gmsh MSH version".into()))?;
    let file_type = fields
        .next()
        .and_then(|value| value.parse::<u8>().ok())
        .ok_or_else(|| Error::InvalidInput("invalid Gmsh file type".into()))?;
    let data_size = fields
        .next()
        .and_then(|value| value.parse::<usize>().ok())
        .ok_or_else(|| Error::InvalidInput("invalid Gmsh data size".into()))?;
    if version != 2.2 || file_type != 1 {
        return Err(Error::Unsupported(format!(
            "binary Gmsh MSH version {version} is unsupported; use binary MSH 2.2 or ASCII MSH 2.x/4.0/4.1"
        )));
    }
    if data_size != 8 {
        return Err(Error::Unsupported(format!(
            "binary Gmsh MSH data size {data_size} is unsupported; 8-byte doubles are required"
        )));
    }
    let marker = reader.binary_bytes(4, "byte-order marker")?;
    reader.byte_order = match marker {
        [1, 0, 0, 0] => GmshByteOrder::Little,
        [0, 0, 0, 1] => GmshByteOrder::Big,
        _ => {
            return Err(Error::InvalidInput(
                "binary Gmsh byte-order marker is invalid".into(),
            ));
        }
    };
    let end_format = read_gmsh_nonempty_line(&mut reader, "mesh format end marker")?;
    if end_format.trim() != "$EndMeshFormat" {
        return Err(Error::InvalidInput(format!(
            "expected Gmsh $EndMeshFormat, found '{end_format}'"
        )));
    }

    let mut nodes = HashMap::new();
    let mut elements = Vec::new();
    let mut data = GmshData::default();
    let mut selected_node_view = false;
    let mut selected_element_view = false;
    let mut seen_nodes_section = false;
    let mut seen_elements_section = false;

    while reader.position < bytes.len() {
        let line = read_gmsh_nonempty_line(&mut reader, "section marker")?;
        let section = line.trim();
        match section {
            "$Nodes" => {
                if seen_nodes_section {
                    return Err(Error::InvalidInput(
                        "binary Gmsh file contains multiple $Nodes sections".into(),
                    ));
                }
                seen_nodes_section = true;
                parse_gmsh_binary_nodes(&mut reader, &mut nodes)?;
            }
            "$Elements" => {
                if !seen_nodes_section || seen_elements_section {
                    return Err(Error::InvalidInput(
                        "binary Gmsh $Elements must follow exactly one $Nodes section".into(),
                    ));
                }
                seen_elements_section = true;
                parse_gmsh_binary_elements(&mut reader, &nodes, &mut elements)?;
            }
            "$NodeData" | "$ElementData" => {
                let is_node_data = section == "$NodeData";
                let selected = if is_node_data {
                    let selected = !selected_node_view;
                    selected_node_view = true;
                    selected
                } else {
                    let selected = !selected_element_view;
                    selected_element_view = true;
                    selected
                };
                parse_gmsh_binary_data(&mut reader, section, is_node_data, selected, &mut data)?;
            }
            "$ElementNodeData" => {
                parse_gmsh_binary_element_node_data(&mut reader, &mut data)?;
            }
            section if section.starts_with('$') && !section.starts_with("$End") => {
                skip_gmsh_ascii_section(&mut reader, section)?;
            }
            section if section.starts_with("$End") => {
                return Err(Error::InvalidInput(format!(
                    "binary Gmsh file has an unmatched section terminator '{section}'"
                )));
            }
            _ => {
                return Err(Error::InvalidInput(format!(
                    "unexpected text outside a binary Gmsh section: '{section}'"
                )));
            }
        }
    }

    if !seen_nodes_section || !seen_elements_section {
        return Err(Error::InvalidInput(
            "binary Gmsh file must contain $Nodes and $Elements sections".into(),
        ));
    }
    for (tag, value) in &data.node_scalars {
        if let Some(node) = nodes.get_mut(tag) {
            node.scalar = Some(*value);
        }
    }
    let mut cells = Vec::new();
    for (element_tag, vtk_type, corner_count, node_ids) in elements {
        let scalar = data.element_scalars.get(&element_tag).copied();
        for primitive in vtk_cell_primitives(vtk_type, &node_ids[..corner_count], scalar)? {
            if cells.len() >= MAX_VTK_RENDERED_PRIMITIVES {
                return Err(Error::LimitExceeded(format!(
                    "Gmsh rendered primitive count exceeds {MAX_VTK_RENDERED_PRIMITIVES}"
                )));
            }
            cells.push(primitive);
        }
    }
    let title = match data.title.as_deref() {
        Some(name) => format!("Gmsh MSH 2.2 Binary FEA Mesh: {name}"),
        None => "Gmsh MSH 2.2 Binary FEA Mesh".into(),
    };
    Ok((nodes, cells, title, data.warnings))
}

fn parse_gmsh_binary_v40(bytes: &[u8]) -> Result<SimulationParseOutput> {
    let header = gmsh_binary_header(bytes)
        .ok_or_else(|| Error::InvalidInput("binary Gmsh file is missing $MeshFormat".into()))?;
    if !(4.0..4.1).contains(&header.0) || header.1 != 1 {
        return Err(Error::Unsupported(format!(
            "binary Gmsh MSH version {} is unsupported; use binary MSH 2.2, 4.0, or 4.1",
            header.0
        )));
    }
    if header.2 != 8 {
        return Err(Error::Unsupported(format!(
            "binary Gmsh MSH 4.0 data size {} is unsupported; 8-byte doubles are required",
            header.2
        )));
    }

    // MSH 4.0 stores `unsigned long`, whose width is platform-dependent and is
    // not encoded independently from the double size in $MeshFormat. Try the
    // common 64-bit and 32-bit widths and require every section boundary to match.
    let mut last_error = None;
    for width in [8, 4] {
        match parse_gmsh_binary_v40_with_width(bytes, width) {
            Ok(output) => return Ok(output),
            Err(error) => last_error = Some(error),
        }
    }
    Err(last_error
        .unwrap_or_else(|| Error::InvalidInput("binary Gmsh MSH 4.0 could not be parsed".into())))
}

fn parse_gmsh_binary_v40_with_width(
    bytes: &[u8],
    unsigned_long_width: usize,
) -> Result<SimulationParseOutput> {
    let mut reader = GmshBinaryReader::new(bytes);
    if reader.line("mesh format marker")?.trim() != "$MeshFormat" {
        return Err(Error::InvalidInput(
            "binary Gmsh file is missing $MeshFormat".into(),
        ));
    }
    let format_line = reader.line("mesh format header")?;
    let mut fields = format_line.split_whitespace();
    let version = fields
        .next()
        .and_then(|value| value.parse::<f64>().ok())
        .ok_or_else(|| Error::InvalidInput("invalid Gmsh MSH version".into()))?;
    let file_type = fields
        .next()
        .and_then(|value| value.parse::<u8>().ok())
        .ok_or_else(|| Error::InvalidInput("invalid Gmsh file type".into()))?;
    let data_size = fields
        .next()
        .and_then(|value| value.parse::<usize>().ok())
        .ok_or_else(|| Error::InvalidInput("invalid Gmsh data size".into()))?;
    if !(4.0..4.1).contains(&version) || file_type != 1 || data_size != 8 {
        return Err(Error::InvalidInput(
            "invalid binary Gmsh MSH 4.0 format header".into(),
        ));
    }
    reader.size_t_width = unsigned_long_width;
    let marker = reader.binary_bytes(4, "byte-order marker")?;
    reader.byte_order = match marker {
        [1, 0, 0, 0] => GmshByteOrder::Little,
        [0, 0, 0, 1] => GmshByteOrder::Big,
        _ => {
            return Err(Error::InvalidInput(
                "binary Gmsh byte-order marker is invalid".into(),
            ));
        }
    };
    let end_format = read_gmsh_nonempty_line(&mut reader, "mesh format end marker")?;
    if end_format.trim() != "$EndMeshFormat" {
        return Err(Error::InvalidInput(format!(
            "expected Gmsh $EndMeshFormat, found '{end_format}'"
        )));
    }

    let mut nodes = HashMap::new();
    let mut elements = Vec::<(usize, usize, usize, Vec<usize>)>::new();
    let mut data = GmshData::default();
    let mut selected_node_view = false;
    let mut selected_element_view = false;
    let mut seen_entities = false;
    let mut seen_nodes = false;
    let mut seen_elements = false;
    let mut total_element_nodes = 0usize;

    while reader.position < bytes.len() {
        let line = read_gmsh_nonempty_line(&mut reader, "section marker")?;
        let section = line.trim();
        match section {
            "$Entities" => {
                if seen_entities || seen_nodes {
                    return Err(Error::InvalidInput(
                        "binary Gmsh MSH 4.0 $Entities must appear once before $Nodes".into(),
                    ));
                }
                seen_entities = true;
                skip_gmsh_binary_v40_entities(&mut reader)?;
            }
            "$Nodes" => {
                if !seen_entities || seen_nodes {
                    return Err(Error::InvalidInput(
                        "binary Gmsh MSH 4.0 requires one $Entities section before $Nodes".into(),
                    ));
                }
                seen_nodes = true;
                parse_gmsh_binary_v40_nodes(&mut reader, &mut nodes)?;
            }
            "$Elements" => {
                if !seen_nodes || seen_elements {
                    return Err(Error::InvalidInput(
                        "binary Gmsh $Elements must follow exactly one $Nodes section".into(),
                    ));
                }
                seen_elements = true;
                parse_gmsh_binary_v40_elements(
                    &mut reader,
                    &nodes,
                    &mut elements,
                    &mut total_element_nodes,
                )?;
            }
            "$NodeData" | "$ElementData" => {
                if !seen_elements {
                    return Err(Error::InvalidInput(
                        "binary Gmsh data sections must follow the mesh sections".into(),
                    ));
                }
                let is_node_data = section == "$NodeData";
                let selected = if is_node_data {
                    let selected = !selected_node_view;
                    selected_node_view = true;
                    selected
                } else {
                    let selected = !selected_element_view;
                    selected_element_view = true;
                    selected
                };
                parse_gmsh_binary_data(&mut reader, section, is_node_data, selected, &mut data)?;
            }
            "$ElementNodeData" => {
                return Err(Error::Unsupported(
                    "binary Gmsh MSH 4.0 ElementNodeData is not supported".into(),
                ));
            }
            "$PhysicalNames" | "$Comments" | "$InterpolationScheme" => {
                skip_gmsh_ascii_section(&mut reader, section)?;
            }
            "$PartitionedEntities" | "$Periodic" | "$GhostElements" | "$Parametrizations" => {
                return Err(Error::Unsupported(format!(
                    "binary Gmsh section '{section}' is not supported"
                )));
            }
            section if section.starts_with('$') && !section.starts_with("$End") => {
                return Err(Error::Unsupported(format!(
                    "unknown binary Gmsh section '{section}' is not supported"
                )));
            }
            section if section.starts_with("$End") => {
                return Err(Error::InvalidInput(format!(
                    "binary Gmsh file has an unmatched section terminator '{section}'"
                )));
            }
            _ => {
                return Err(Error::InvalidInput(format!(
                    "unexpected text outside a binary Gmsh section: '{section}'"
                )));
            }
        }
    }

    if !seen_entities || !seen_nodes || !seen_elements {
        return Err(Error::InvalidInput(
            "binary Gmsh MSH 4.0 must contain $Entities, $Nodes and $Elements sections".into(),
        ));
    }
    for (tag, value) in &data.node_scalars {
        if let Some(node) = nodes.get_mut(tag) {
            node.scalar = Some(*value);
        }
    }
    let mut cells = Vec::new();
    for (element_tag, vtk_type, corner_count, node_ids) in elements {
        let scalar = data.element_scalars.get(&element_tag).copied();
        for primitive in vtk_cell_primitives(vtk_type, &node_ids[..corner_count], scalar)? {
            if cells.len() >= MAX_VTK_RENDERED_PRIMITIVES {
                return Err(Error::LimitExceeded(format!(
                    "Gmsh rendered primitive count exceeds {MAX_VTK_RENDERED_PRIMITIVES}"
                )));
            }
            cells.push(primitive);
        }
    }
    let title = match data.title.as_deref() {
        Some(name) => format!("Gmsh MSH 4.0 Binary FEA Mesh: {name}"),
        None => "Gmsh MSH 4.0 Binary FEA Mesh".into(),
    };
    Ok((nodes, cells, title, data.warnings))
}

fn skip_gmsh_binary_v40_entities(reader: &mut GmshBinaryReader<'_>) -> Result<()> {
    let counts = [
        reader.size_t("point entity count")?,
        reader.size_t("curve entity count")?,
        reader.size_t("surface entity count")?,
        reader.size_t("volume entity count")?,
    ];
    let entity_count = counts
        .iter()
        .try_fold(0usize, |total, count| total.checked_add(*count))
        .ok_or_else(|| Error::LimitExceeded("Gmsh entity count overflowed".into()))?;
    if entity_count > MAX_SIMULATION_POINTS {
        return Err(Error::LimitExceeded(format!(
            "Gmsh entity count exceeds {MAX_SIMULATION_POINTS}"
        )));
    }
    for (dimension, count) in counts.into_iter().enumerate() {
        for _ in 0..count {
            let tag = reader.i32("entity tag")?;
            if tag <= 0 {
                return Err(Error::InvalidInput(
                    "Gmsh entity tag must be positive".into(),
                ));
            }
            // MSH 4.0 writes six bounding-box coordinates for points too;
            // MSH 4.1 changes point entities to three coordinate values.
            for _ in 0..6 {
                let _ = reader.f64("entity bounding box")?;
            }
            let physical_tags = reader.size_t("entity physical tag count")?;
            if physical_tags > MAX_SIMULATION_CELLS {
                return Err(Error::LimitExceeded(
                    "Gmsh physical tag count is excessive".into(),
                ));
            }
            for _ in 0..physical_tags {
                let _ = reader.i32("entity physical tag")?;
            }
            if dimension > 0 {
                let boundary_count = reader.size_t("entity boundary count")?;
                if boundary_count > MAX_SIMULATION_POINTS {
                    return Err(Error::LimitExceeded(
                        "Gmsh entity boundary count is excessive".into(),
                    ));
                }
                for _ in 0..boundary_count {
                    let _ = reader.i32("entity boundary tag")?;
                }
            }
        }
    }
    reader.binary_section_end("$EndEntities")
}

fn parse_gmsh_binary_v40_nodes(
    reader: &mut GmshBinaryReader<'_>,
    nodes: &mut HashMap<usize, MeshNode>,
) -> Result<()> {
    let block_count = reader.size_t("node block count")?;
    let declared_node_count = reader.size_t("node count")?;
    if block_count > MAX_SIMULATION_POINTS || declared_node_count > MAX_SIMULATION_POINTS {
        return Err(Error::LimitExceeded(format!(
            "Gmsh MSH 4.0 node or block count exceeds {MAX_SIMULATION_POINTS}"
        )));
    }
    nodes.reserve(declared_node_count);
    for _ in 0..block_count {
        let entity_tag = reader.i32("node entity tag")?;
        let entity_dim = reader.i32("node entity dimension")?;
        let parametric = reader.i32("parametric flag")?;
        let nodes_in_block = reader.size_t("node block size")?;
        if entity_tag <= 0
            || !(0..=3).contains(&entity_dim)
            || !(0..=1).contains(&parametric)
            || nodes_in_block > declared_node_count.saturating_sub(nodes.len())
        {
            return Err(Error::InvalidInput(
                "invalid Gmsh MSH 4.0 node block header".into(),
            ));
        }
        for _ in 0..nodes_in_block {
            let tag = usize::try_from(reader.i32("node tag")?)
                .ok()
                .filter(|tag| *tag > 0)
                .ok_or_else(|| Error::InvalidInput("Gmsh node tag must be positive".into()))?;
            let x = reader.f64("node x coordinate")?;
            let y = reader.f64("node y coordinate")?;
            let z = reader.f64("node z coordinate")?;
            if [x, y, z].iter().any(|coordinate| coordinate.abs() > 1.0e12) {
                return Err(Error::InvalidInput(
                    "Gmsh MSH 4.0 node coordinate is outside the supported range".into(),
                ));
            }
            if parametric == 1 {
                for _ in 0..entity_dim {
                    let parameter = reader.f64("node parametric coordinate")?;
                    if !parameter.is_finite() {
                        return Err(Error::InvalidInput(
                            "Gmsh node has a non-finite parametric coordinate".into(),
                        ));
                    }
                }
            }
            if nodes.insert(tag, MeshNode { x, y, scalar: None }).is_some() {
                return Err(Error::InvalidInput(format!(
                    "duplicate Gmsh node tag {tag}"
                )));
            }
        }
    }
    if nodes.len() != declared_node_count {
        return Err(Error::InvalidInput(
            "Gmsh MSH 4.0 node blocks do not match the declared total".into(),
        ));
    }
    reader.binary_section_end("$EndNodes")
}

fn parse_gmsh_binary_v40_elements(
    reader: &mut GmshBinaryReader<'_>,
    nodes: &HashMap<usize, MeshNode>,
    elements: &mut Vec<(usize, usize, usize, Vec<usize>)>,
    total_element_nodes: &mut usize,
) -> Result<()> {
    let block_count = reader.size_t("element block count")?;
    let declared_element_count = reader.size_t("element count")?;
    if block_count > MAX_SIMULATION_CELLS || declared_element_count > MAX_SIMULATION_CELLS {
        return Err(Error::LimitExceeded(format!(
            "Gmsh MSH 4.0 element or block count exceeds {MAX_SIMULATION_CELLS}"
        )));
    }
    elements.reserve(declared_element_count);
    let mut seen_elements = HashSet::with_capacity(declared_element_count);
    for _ in 0..block_count {
        let entity_tag = reader.i32("element entity tag")?;
        let entity_dim = reader.i32("element entity dimension")?;
        let element_type = usize::try_from(reader.i32("element type")?)
            .map_err(|_| Error::InvalidInput("invalid Gmsh element type".into()))?;
        let elements_in_block = reader.size_t("element block size")?;
        if entity_tag <= 0
            || !(0..=3).contains(&entity_dim)
            || elements_in_block > declared_element_count.saturating_sub(seen_elements.len())
        {
            return Err(Error::InvalidInput(
                "invalid Gmsh MSH 4.0 element block header".into(),
            ));
        }
        let (node_count, vtk_type, corner_count) =
            gmsh_element_info(element_type).ok_or_else(|| {
                Error::Unsupported(format!("Gmsh element type {element_type} is not supported"))
            })?;
        *total_element_nodes =
            total_element_nodes
                .checked_add(elements_in_block.checked_mul(node_count).ok_or_else(|| {
                    Error::LimitExceeded("Gmsh connectivity count overflowed".into())
                })?)
                .ok_or_else(|| Error::LimitExceeded("Gmsh connectivity count overflowed".into()))?;
        if *total_element_nodes > MAX_VTK_TOTAL_VALUES {
            return Err(Error::LimitExceeded(format!(
                "Gmsh connectivity exceeds {MAX_VTK_TOTAL_VALUES} node references"
            )));
        }
        for _ in 0..elements_in_block {
            let element_tag = usize::try_from(reader.i32("element tag")?)
                .ok()
                .filter(|tag| *tag > 0)
                .ok_or_else(|| Error::InvalidInput("Gmsh element tag must be positive".into()))?;
            if !seen_elements.insert(element_tag) {
                return Err(Error::InvalidInput(format!(
                    "duplicate Gmsh element tag {element_tag}"
                )));
            }
            let mut node_ids = Vec::with_capacity(node_count);
            for _ in 0..node_count {
                let node_tag = usize::try_from(reader.i32("element node tag")?)
                    .ok()
                    .filter(|tag| *tag > 0)
                    .ok_or_else(|| {
                        Error::InvalidInput("Gmsh element node tag must be positive".into())
                    })?;
                if !nodes.contains_key(&node_tag) {
                    return Err(Error::InvalidInput(format!(
                        "Gmsh element {element_tag} references unknown node {node_tag}"
                    )));
                }
                node_ids.push(node_tag);
            }
            elements.push((element_tag, vtk_type, corner_count, node_ids));
        }
    }
    if seen_elements.len() != declared_element_count {
        return Err(Error::InvalidInput(
            "Gmsh MSH 4.0 element blocks do not match the declared total".into(),
        ));
    }
    reader.binary_section_end("$EndElements")
}

fn parse_gmsh_binary_v41(bytes: &[u8]) -> Result<SimulationParseOutput> {
    let mut reader = GmshBinaryReader::new(bytes);
    if reader.line("mesh format marker")?.trim() != "$MeshFormat" {
        return Err(Error::InvalidInput(
            "binary Gmsh file is missing $MeshFormat".into(),
        ));
    }
    let format_line = reader.line("mesh format header")?;
    let mut fields = format_line.split_whitespace();
    let version = fields
        .next()
        .and_then(|value| value.parse::<f64>().ok())
        .ok_or_else(|| Error::InvalidInput("invalid Gmsh MSH version".into()))?;
    let file_type = fields
        .next()
        .and_then(|value| value.parse::<u8>().ok())
        .ok_or_else(|| Error::InvalidInput("invalid Gmsh file type".into()))?;
    let size_t_width = fields
        .next()
        .and_then(|value| value.parse::<usize>().ok())
        .ok_or_else(|| Error::InvalidInput("invalid Gmsh data size".into()))?;
    if !(4.1..4.2).contains(&version) || file_type != 1 {
        return Err(Error::Unsupported(format!(
            "binary Gmsh MSH version {version} is unsupported; use binary MSH 2.2 or 4.1"
        )));
    }
    if !matches!(size_t_width, 4 | 8) {
        return Err(Error::Unsupported(format!(
            "binary Gmsh MSH 4.1 size_t width {size_t_width} is unsupported"
        )));
    }
    reader.size_t_width = size_t_width;
    let marker = reader.binary_bytes(4, "byte-order marker")?;
    reader.byte_order = match marker {
        [1, 0, 0, 0] => GmshByteOrder::Little,
        [0, 0, 0, 1] => GmshByteOrder::Big,
        _ => {
            return Err(Error::InvalidInput(
                "binary Gmsh byte-order marker is invalid".into(),
            ));
        }
    };
    let end_format = read_gmsh_nonempty_line(&mut reader, "mesh format end marker")?;
    if end_format.trim() != "$EndMeshFormat" {
        return Err(Error::InvalidInput(format!(
            "expected Gmsh $EndMeshFormat, found '{end_format}'"
        )));
    }

    let mut nodes = HashMap::new();
    let mut elements = Vec::<(usize, usize, usize, Vec<usize>)>::new();
    let mut data = GmshData::default();
    let mut selected_node_view = false;
    let mut selected_element_view = false;
    let mut seen_nodes = false;
    let mut seen_elements = false;
    let mut total_element_nodes = 0usize;

    while reader.position < bytes.len() {
        let line = read_gmsh_nonempty_line(&mut reader, "section marker")?;
        let section = line.trim();
        match section {
            "$Entities" => skip_gmsh_binary_v41_entities(&mut reader)?,
            "$Nodes" => {
                if seen_nodes {
                    return Err(Error::InvalidInput(
                        "binary Gmsh file contains multiple $Nodes sections".into(),
                    ));
                }
                seen_nodes = true;
                parse_gmsh_binary_v41_nodes(&mut reader, &mut nodes)?;
            }
            "$Elements" => {
                if !seen_nodes || seen_elements {
                    return Err(Error::InvalidInput(
                        "binary Gmsh $Elements must follow exactly one $Nodes section".into(),
                    ));
                }
                seen_elements = true;
                parse_gmsh_binary_v41_elements(
                    &mut reader,
                    &nodes,
                    &mut elements,
                    &mut total_element_nodes,
                )?;
            }
            "$NodeData" | "$ElementData" => {
                if !seen_elements {
                    return Err(Error::InvalidInput(
                        "binary Gmsh data sections must follow the mesh sections".into(),
                    ));
                }
                let is_node_data = section == "$NodeData";
                let selected = if is_node_data {
                    let selected = !selected_node_view;
                    selected_node_view = true;
                    selected
                } else {
                    let selected = !selected_element_view;
                    selected_element_view = true;
                    selected
                };
                parse_gmsh_binary_data(&mut reader, section, is_node_data, selected, &mut data)?;
            }
            "$ElementNodeData" => {
                if !seen_elements {
                    return Err(Error::InvalidInput(
                        "binary Gmsh data sections must follow the mesh sections".into(),
                    ));
                }
                parse_gmsh_binary_element_node_data(&mut reader, &mut data)?;
            }
            "$PhysicalNames" | "$Comments" | "$InterpolationScheme" => {
                skip_gmsh_ascii_section(&mut reader, section)?;
            }
            "$PartitionedEntities" | "$Periodic" | "$GhostElements" | "$Parametrizations" => {
                return Err(Error::Unsupported(format!(
                    "binary Gmsh section '{section}' is not supported"
                )));
            }
            section if section.starts_with('$') && !section.starts_with("$End") => {
                return Err(Error::Unsupported(format!(
                    "unknown binary Gmsh section '{section}' is not supported"
                )));
            }
            section if section.starts_with("$End") => {
                return Err(Error::InvalidInput(format!(
                    "binary Gmsh file has an unmatched section terminator '{section}'"
                )));
            }
            _ => {
                return Err(Error::InvalidInput(format!(
                    "unexpected text outside a binary Gmsh section: '{section}'"
                )));
            }
        }
    }

    if !seen_nodes || !seen_elements {
        return Err(Error::InvalidInput(
            "binary Gmsh MSH 4.1 must contain $Nodes and $Elements sections".into(),
        ));
    }
    for (tag, value) in &data.node_scalars {
        if let Some(node) = nodes.get_mut(tag) {
            node.scalar = Some(*value);
        }
    }
    let mut cells = Vec::new();
    for (element_tag, vtk_type, corner_count, node_ids) in elements {
        let scalar = data.element_scalars.get(&element_tag).copied();
        for primitive in vtk_cell_primitives(vtk_type, &node_ids[..corner_count], scalar)? {
            if cells.len() >= MAX_VTK_RENDERED_PRIMITIVES {
                return Err(Error::LimitExceeded(format!(
                    "Gmsh rendered primitive count exceeds {MAX_VTK_RENDERED_PRIMITIVES}"
                )));
            }
            cells.push(primitive);
        }
    }
    let title = match data.title.as_deref() {
        Some(name) => format!("Gmsh MSH 4.1 Binary FEA Mesh: {name}"),
        None => "Gmsh MSH 4.1 Binary FEA Mesh".into(),
    };
    Ok((nodes, cells, title, data.warnings))
}

fn parse_gmsh_binary_v41_nodes(
    reader: &mut GmshBinaryReader<'_>,
    nodes: &mut HashMap<usize, MeshNode>,
) -> Result<()> {
    let block_count = reader.size_t("node block count")?;
    let declared_node_count = reader.size_t("node count")?;
    let min_node_tag = reader.size_t("minimum node tag")?;
    let max_node_tag = reader.size_t("maximum node tag")?;
    if block_count > MAX_SIMULATION_POINTS || declared_node_count > MAX_SIMULATION_POINTS {
        return Err(Error::LimitExceeded(format!(
            "Gmsh MSH 4.1 node or block count exceeds {MAX_SIMULATION_POINTS}"
        )));
    }
    if declared_node_count > 0 && (min_node_tag == 0 || min_node_tag > max_node_tag) {
        return Err(Error::InvalidInput("invalid Gmsh node tag range".into()));
    }
    nodes.reserve(declared_node_count);
    for _ in 0..block_count {
        let entity_dim = usize::try_from(reader.i32("node entity dimension")?)
            .map_err(|_| Error::InvalidInput("invalid Gmsh node entity dimension".into()))?;
        let _entity_tag = reader.i32("node entity tag")?;
        let parametric = reader.i32("parametric flag")?;
        let nodes_in_block = reader.size_t("node block size")?;
        if entity_dim > 3 || !(0..=1).contains(&parametric) {
            return Err(Error::InvalidInput(
                "invalid Gmsh MSH 4.1 node block header".into(),
            ));
        }
        if nodes_in_block > declared_node_count.saturating_sub(nodes.len()) {
            return Err(Error::InvalidInput(
                "Gmsh node block sizes exceed the declared node count".into(),
            ));
        }
        let mut tags = Vec::with_capacity(nodes_in_block);
        for _ in 0..nodes_in_block {
            let tag = reader.size_t("node tag")?;
            if tag == 0 || tag < min_node_tag || tag > max_node_tag || nodes.contains_key(&tag) {
                return Err(Error::InvalidInput(format!(
                    "invalid or duplicate Gmsh node tag {tag}"
                )));
            }
            tags.push(tag);
        }
        for tag in tags {
            let x = reader.f64("node x coordinate")?;
            let y = reader.f64("node y coordinate")?;
            let z = reader.f64("node z coordinate")?;
            if [x, y, z].iter().any(|coordinate| coordinate.abs() > 1.0e12) {
                return Err(Error::InvalidInput(
                    "Gmsh MSH 4.1 node coordinate is outside the supported range".into(),
                ));
            }
            if parametric == 1 {
                for _ in 0..entity_dim {
                    let parameter = reader.f64("node parametric coordinate")?;
                    if !parameter.is_finite() {
                        return Err(Error::InvalidInput(
                            "Gmsh node has a non-finite parametric coordinate".into(),
                        ));
                    }
                }
            }
            nodes.insert(tag, MeshNode { x, y, scalar: None });
        }
    }
    if nodes.len() != declared_node_count {
        return Err(Error::InvalidInput(
            "Gmsh MSH 4.1 node blocks do not match the declared total".into(),
        ));
    }
    reader.binary_section_end("$EndNodes")
}

fn parse_gmsh_binary_v41_elements(
    reader: &mut GmshBinaryReader<'_>,
    nodes: &HashMap<usize, MeshNode>,
    elements: &mut Vec<(usize, usize, usize, Vec<usize>)>,
    total_element_nodes: &mut usize,
) -> Result<()> {
    let block_count = reader.size_t("element block count")?;
    let declared_element_count = reader.size_t("element count")?;
    let min_element_tag = reader.size_t("minimum element tag")?;
    let max_element_tag = reader.size_t("maximum element tag")?;
    if block_count > MAX_SIMULATION_CELLS || declared_element_count > MAX_SIMULATION_CELLS {
        return Err(Error::LimitExceeded(format!(
            "Gmsh MSH 4.1 element or block count exceeds {MAX_SIMULATION_CELLS}"
        )));
    }
    if declared_element_count > 0 && (min_element_tag == 0 || min_element_tag > max_element_tag) {
        return Err(Error::InvalidInput("invalid Gmsh element tag range".into()));
    }
    elements.reserve(declared_element_count);
    let mut seen_elements = HashSet::with_capacity(declared_element_count);
    for _ in 0..block_count {
        let entity_dim = usize::try_from(reader.i32("element entity dimension")?)
            .map_err(|_| Error::InvalidInput("invalid Gmsh element entity dimension".into()))?;
        let _entity_tag = reader.i32("element entity tag")?;
        let element_type = usize::try_from(reader.i32("element type")?)
            .map_err(|_| Error::InvalidInput("invalid Gmsh element type".into()))?;
        let elements_in_block = reader.size_t("element block size")?;
        if entity_dim > 3
            || elements_in_block > declared_element_count.saturating_sub(seen_elements.len())
        {
            return Err(Error::InvalidInput(
                "invalid Gmsh MSH 4.1 element block header".into(),
            ));
        }
        let (node_count, vtk_type, corner_count) =
            gmsh_element_info(element_type).ok_or_else(|| {
                Error::Unsupported(format!("Gmsh element type {element_type} is not supported"))
            })?;
        *total_element_nodes =
            total_element_nodes
                .checked_add(elements_in_block.checked_mul(node_count).ok_or_else(|| {
                    Error::LimitExceeded("Gmsh connectivity count overflowed".into())
                })?)
                .ok_or_else(|| Error::LimitExceeded("Gmsh connectivity count overflowed".into()))?;
        if *total_element_nodes > MAX_VTK_TOTAL_VALUES {
            return Err(Error::LimitExceeded(format!(
                "Gmsh connectivity exceeds {MAX_VTK_TOTAL_VALUES} node references"
            )));
        }
        for _ in 0..elements_in_block {
            let element_tag = reader.size_t("element tag")?;
            if element_tag == 0
                || element_tag < min_element_tag
                || element_tag > max_element_tag
                || !seen_elements.insert(element_tag)
            {
                return Err(Error::InvalidInput(format!(
                    "invalid or duplicate Gmsh element tag {element_tag}"
                )));
            }
            let mut node_ids = Vec::with_capacity(node_count);
            for _ in 0..node_count {
                let node_id = reader.size_t("element node tag")?;
                if node_id == 0 || !nodes.contains_key(&node_id) {
                    return Err(Error::InvalidInput(format!(
                        "Gmsh element {element_tag} references invalid or unknown node {node_id}"
                    )));
                }
                node_ids.push(node_id);
            }
            elements.push((element_tag, vtk_type, corner_count, node_ids));
        }
    }
    if seen_elements.len() != declared_element_count {
        return Err(Error::InvalidInput(
            "Gmsh MSH 4.1 element blocks do not match the declared total".into(),
        ));
    }
    reader.binary_section_end("$EndElements")
}

fn skip_gmsh_binary_v41_entities(reader: &mut GmshBinaryReader<'_>) -> Result<()> {
    let counts = [
        reader.size_t("point entity count")?,
        reader.size_t("curve entity count")?,
        reader.size_t("surface entity count")?,
        reader.size_t("volume entity count")?,
    ];
    let entity_count = counts
        .iter()
        .try_fold(0usize, |total, count| total.checked_add(*count))
        .ok_or_else(|| Error::LimitExceeded("Gmsh entity count overflowed".into()))?;
    if entity_count > MAX_SIMULATION_POINTS {
        return Err(Error::LimitExceeded(format!(
            "Gmsh entity count exceeds {MAX_SIMULATION_POINTS}"
        )));
    }
    for (dimension, count) in counts.into_iter().enumerate() {
        for _ in 0..count {
            let _tag = reader.i32("entity tag")?;
            if dimension == 0 {
                for _ in 0..3 {
                    let _ = reader.f64("point entity coordinate")?;
                }
            } else {
                for _ in 0..6 {
                    let _ = reader.f64("entity bounding box")?;
                }
            }
            let physical_tags = reader.size_t("entity physical tag count")?;
            if physical_tags > MAX_SIMULATION_CELLS {
                return Err(Error::LimitExceeded(
                    "Gmsh physical tag count is excessive".into(),
                ));
            }
            for _ in 0..physical_tags {
                let _ = reader.i32("entity physical tag")?;
            }
            if dimension > 0 {
                let boundary_count = reader.size_t("entity boundary count")?;
                if boundary_count > MAX_SIMULATION_POINTS {
                    return Err(Error::LimitExceeded(
                        "Gmsh entity boundary count is excessive".into(),
                    ));
                }
                for _ in 0..boundary_count {
                    let _ = reader.i32("entity boundary tag")?;
                }
            }
        }
    }
    reader.binary_section_end("$EndEntities")
}

fn read_gmsh_nonempty_line(reader: &mut GmshBinaryReader<'_>, context: &str) -> Result<String> {
    loop {
        let line = reader.line(context)?;
        if !line.trim().is_empty() {
            return Ok(line);
        }
    }
}

fn parse_gmsh_ascii_count(reader: &mut GmshBinaryReader<'_>, context: &str) -> Result<usize> {
    let line = read_gmsh_nonempty_line(reader, context)?;
    line.trim()
        .parse::<usize>()
        .map_err(|_| Error::InvalidInput(format!("invalid binary Gmsh {context} count '{line}'")))
}

fn parse_gmsh_binary_nodes(
    reader: &mut GmshBinaryReader<'_>,
    nodes: &mut HashMap<usize, MeshNode>,
) -> Result<()> {
    let count = parse_gmsh_ascii_count(reader, "node")?;
    if count > MAX_SIMULATION_POINTS {
        return Err(Error::LimitExceeded(format!(
            "Gmsh node count exceeds {MAX_SIMULATION_POINTS}"
        )));
    }
    nodes.reserve(count);
    for _ in 0..count {
        let tag = reader.i32("node tag")?;
        let id = usize::try_from(tag)
            .ok()
            .filter(|id| *id > 0)
            .ok_or_else(|| Error::InvalidInput("Gmsh node tag must be positive".into()))?;
        let x = reader.f64("node x coordinate")?;
        let y = reader.f64("node y coordinate")?;
        let z = reader.f64("node z coordinate")?;
        if [x, y, z].iter().any(|value| value.abs() > 1.0e12) {
            return Err(Error::InvalidInput(
                "Gmsh node coordinate is outside the supported range".into(),
            ));
        }
        if nodes.insert(id, MeshNode { x, y, scalar: None }).is_some() {
            return Err(Error::InvalidInput(format!("duplicate Gmsh node tag {id}")));
        }
    }
    reader.binary_section_end("$EndNodes")
}

fn parse_gmsh_binary_elements(
    reader: &mut GmshBinaryReader<'_>,
    nodes: &HashMap<usize, MeshNode>,
    elements: &mut Vec<(usize, usize, usize, Vec<usize>)>,
) -> Result<()> {
    let count = parse_gmsh_ascii_count(reader, "element")?;
    if count > MAX_SIMULATION_CELLS {
        return Err(Error::LimitExceeded(format!(
            "Gmsh element count exceeds {MAX_SIMULATION_CELLS}"
        )));
    }
    elements.reserve(count);
    let mut seen_elements = HashSet::with_capacity(count);
    let mut read_count = 0usize;
    while read_count < count {
        let element_type = usize::try_from(reader.i32("element type")?)
            .map_err(|_| Error::InvalidInput("invalid Gmsh element type".into()))?;
        let block_count = usize::try_from(reader.i32("element block count")?)
            .map_err(|_| Error::InvalidInput("invalid Gmsh element block count".into()))?;
        let tag_count = usize::try_from(reader.i32("element tag count")?)
            .map_err(|_| Error::InvalidInput("invalid Gmsh element tag count".into()))?;
        if block_count == 0 || block_count > count.saturating_sub(read_count) || tag_count > 64 {
            return Err(Error::InvalidInput(
                "invalid binary Gmsh element block dimensions".into(),
            ));
        }
        let (node_count, vtk_type, corner_count) =
            gmsh_element_info(element_type).ok_or_else(|| {
                Error::Unsupported(format!("Gmsh element type {element_type} is not supported"))
            })?;
        for _ in 0..block_count {
            let element_tag = usize::try_from(reader.i32("element tag")?)
                .ok()
                .filter(|tag| *tag > 0)
                .ok_or_else(|| Error::InvalidInput("Gmsh element tag must be positive".into()))?;
            if !seen_elements.insert(element_tag) {
                return Err(Error::InvalidInput(format!(
                    "duplicate Gmsh element tag {element_tag}"
                )));
            }
            for _ in 0..tag_count {
                let _ = reader.i32("element metadata tag")?;
            }
            let mut node_ids = Vec::with_capacity(node_count);
            for _ in 0..node_count {
                let node_id = usize::try_from(reader.i32("element node tag")?)
                    .ok()
                    .filter(|tag| *tag > 0)
                    .ok_or_else(|| {
                        Error::InvalidInput("Gmsh element node tag must be positive".into())
                    })?;
                if !nodes.contains_key(&node_id) {
                    return Err(Error::InvalidInput(format!(
                        "Gmsh element {element_tag} references unknown node {node_id}"
                    )));
                }
                node_ids.push(node_id);
            }
            elements.push((element_tag, vtk_type, corner_count, node_ids));
        }
        read_count += block_count;
    }
    reader.binary_section_end("$EndElements")
}

fn parse_gmsh_binary_data(
    reader: &mut GmshBinaryReader<'_>,
    section: &str,
    is_node_data: bool,
    selected: bool,
    data: &mut GmshData,
) -> Result<()> {
    let string_tag_count = parse_gmsh_ascii_count(reader, "data string tag")?;
    if string_tag_count > 64 {
        return Err(Error::LimitExceeded(
            "Gmsh data string tag count exceeds 64".into(),
        ));
    }
    let mut view_name = None;
    for index in 0..string_tag_count {
        let tag = read_gmsh_nonempty_line(reader, "data string tag")?;
        if index == 0 {
            view_name = Some(tag.trim().trim_matches('"').to_owned());
        }
    }
    let real_tag_count = parse_gmsh_ascii_count(reader, "data real tag")?;
    if real_tag_count > 64 {
        return Err(Error::LimitExceeded(
            "Gmsh data real tag count exceeds 64".into(),
        ));
    }
    for _ in 0..real_tag_count {
        let value = read_gmsh_nonempty_line(reader, "data real tag")?;
        let parsed = value
            .trim()
            .parse::<f64>()
            .map_err(|_| Error::InvalidInput(format!("invalid Gmsh data real tag '{value}'")))?;
        if !parsed.is_finite() {
            return Err(Error::InvalidInput(
                "Gmsh data real tag is non-finite".into(),
            ));
        }
    }
    let integer_tag_count = parse_gmsh_ascii_count(reader, "data integer tag")?;
    if integer_tag_count > 64 {
        return Err(Error::LimitExceeded(
            "Gmsh data integer tag count exceeds 64".into(),
        ));
    }
    let mut integer_tags = Vec::with_capacity(integer_tag_count);
    for _ in 0..integer_tag_count {
        let value = read_gmsh_nonempty_line(reader, "data integer tag")?;
        integer_tags.push(value.trim().parse::<i64>().map_err(|_| {
            Error::InvalidInput(format!("invalid Gmsh data integer tag '{value}'"))
        })?);
    }
    if integer_tags.len() < 3 {
        return Err(Error::InvalidInput(format!(
            "Gmsh {section} must provide time step, component count, and entry count"
        )));
    }
    let component_count = usize::try_from(integer_tags[1])
        .map_err(|_| Error::InvalidInput(format!("invalid Gmsh {section} component count")))?;
    let entry_count = usize::try_from(integer_tags[2])
        .map_err(|_| Error::InvalidInput(format!("invalid Gmsh {section} entry count")))?;
    if component_count == 0 || component_count > 256 {
        return Err(Error::LimitExceeded(format!(
            "Gmsh {section} component count must be between 1 and 256"
        )));
    }
    if entry_count > MAX_SIMULATION_POINTS.max(MAX_SIMULATION_CELLS)
        || entry_count
            .checked_mul(component_count)
            .is_none_or(|count| count > MAX_VTK_TOTAL_VALUES)
    {
        return Err(Error::LimitExceeded(format!(
            "Gmsh {section} data exceeds the bounded entry/value limit"
        )));
    }
    if !selected {
        data.warnings.push(format!(
            "additional Gmsh {section} view was ignored; only the first view is rendered"
        ));
    } else if component_count > 9 {
        data.warnings.push(format!(
            "Gmsh {section} view '{}' with {component_count} components was skipped",
            view_name.as_deref().unwrap_or("unnamed")
        ));
    } else if component_count > 1 {
        data.warnings.push(format!(
            "Gmsh {section} view '{}' has {component_count} components; preview colors use Euclidean magnitude, not domain-specific equivalent stress",
            view_name.as_deref().unwrap_or("unnamed")
        ));
    }
    if selected
        && component_count <= 9
        && let Some(name) = view_name.as_deref()
    {
        data.title = Some(match data.title.take() {
            Some(previous) => format!("{previous} / {name}"),
            None => name.to_owned(),
        });
    }
    let target = if is_node_data {
        &mut data.node_scalars
    } else {
        &mut data.element_scalars
    };
    for _ in 0..entry_count {
        let tag = usize::try_from(reader.i32("data record tag")?)
            .ok()
            .filter(|tag| *tag > 0)
            .ok_or_else(|| Error::InvalidInput("Gmsh data record tag must be positive".into()))?;
        let mut magnitude = 0.0f64;
        let mut scalar = 0.0;
        for component_index in 0..component_count {
            let value = reader.f64("data value")?;
            if component_index == 0 {
                scalar = value;
            }
            magnitude = magnitude.hypot(value);
        }
        if selected && component_count <= 9 {
            let preview_value = if component_count == 1 {
                scalar
            } else {
                magnitude
            };
            if !preview_value.is_finite() {
                return Err(Error::InvalidInput(format!(
                    "Gmsh {section} vector magnitude overflows the supported numeric range"
                )));
            }
            if target.insert(tag, preview_value).is_some() {
                return Err(Error::InvalidInput(format!(
                    "duplicate Gmsh {section} data tag {tag}"
                )));
            }
        }
    }
    reader.binary_section_end(if is_node_data {
        "$EndNodeData"
    } else {
        "$EndElementData"
    })
}

fn parse_gmsh_binary_element_node_data(
    reader: &mut GmshBinaryReader<'_>,
    data: &mut GmshData,
) -> Result<()> {
    let string_tags = parse_gmsh_ascii_count(reader, "ElementNodeData string tag")?;
    if string_tags > 64 {
        return Err(Error::LimitExceeded(
            "Gmsh ElementNodeData string tag count exceeds 64".into(),
        ));
    }
    for _ in 0..string_tags {
        let _ = read_gmsh_nonempty_line(reader, "ElementNodeData string tag")?;
    }
    let real_tags = parse_gmsh_ascii_count(reader, "ElementNodeData real tag")?;
    if real_tags > 64 {
        return Err(Error::LimitExceeded(
            "Gmsh ElementNodeData real tag count exceeds 64".into(),
        ));
    }
    for _ in 0..real_tags {
        let _ = read_gmsh_nonempty_line(reader, "ElementNodeData real tag")?;
    }
    let integer_tags = parse_gmsh_ascii_count(reader, "ElementNodeData integer tag")?;
    if integer_tags > 64 {
        return Err(Error::LimitExceeded(
            "Gmsh ElementNodeData integer tag count exceeds 64".into(),
        ));
    }
    let mut values = Vec::with_capacity(integer_tags);
    for _ in 0..integer_tags {
        let value = read_gmsh_nonempty_line(reader, "ElementNodeData integer tag")?;
        values.push(value.trim().parse::<i64>().map_err(|_| {
            Error::InvalidInput(format!(
                "invalid Gmsh ElementNodeData integer tag '{value}'"
            ))
        })?);
    }
    if values.len() < 3 {
        return Err(Error::InvalidInput(
            "Gmsh ElementNodeData must provide component and entry counts".into(),
        ));
    }
    let component_count = usize::try_from(values[1])
        .map_err(|_| Error::InvalidInput("invalid Gmsh ElementNodeData component count".into()))?;
    let entry_count = usize::try_from(values[2])
        .map_err(|_| Error::InvalidInput("invalid Gmsh ElementNodeData entry count".into()))?;
    if component_count == 0 || component_count > 256 || entry_count > MAX_SIMULATION_CELLS {
        return Err(Error::LimitExceeded(
            "Gmsh ElementNodeData exceeds the bounded entry/component limit".into(),
        ));
    }
    let mut total_values = 0usize;
    for _ in 0..entry_count {
        let _tag = reader.i32("ElementNodeData element tag")?;
        let nodes_per_element = usize::try_from(reader.i32("ElementNodeData node count")?)
            .map_err(|_| Error::InvalidInput("invalid ElementNodeData node count".into()))?;
        total_values = total_values
            .checked_add(
                nodes_per_element
                    .checked_mul(component_count)
                    .ok_or_else(|| {
                        Error::LimitExceeded("Gmsh ElementNodeData value count overflowed".into())
                    })?,
            )
            .ok_or_else(|| Error::LimitExceeded("Gmsh ElementNodeData size overflowed".into()))?;
        if total_values > MAX_VTK_TOTAL_VALUES {
            return Err(Error::LimitExceeded(
                "Gmsh ElementNodeData exceeds the bounded value limit".into(),
            ));
        }
        for _ in 0..nodes_per_element.saturating_mul(component_count) {
            let _ = reader.f64("ElementNodeData value")?;
        }
    }
    data.warnings.push(
        "Gmsh ElementNodeData is not rendered; use NodeData or ElementData scalar views".into(),
    );
    reader.binary_section_end("$EndElementNodeData")
}

fn skip_gmsh_ascii_section(reader: &mut GmshBinaryReader<'_>, start: &str) -> Result<()> {
    let section_name = start
        .strip_prefix('$')
        .ok_or_else(|| Error::InvalidInput("invalid Gmsh section name".into()))?;
    let expected = format!("$End{section_name}");
    loop {
        let line = reader.line("optional section")?;
        if line.trim() == expected {
            return Ok(());
        }
        if line.trim().starts_with("$End") {
            return Err(Error::InvalidInput(format!(
                "Gmsh section {start} was terminated by '{}'",
                line.trim()
            )));
        }
    }
}

fn parse_gmsh_v2(
    text: &str,
    data: &GmshData,
) -> Result<(HashMap<usize, MeshNode>, Vec<MeshCell>, String)> {
    let mut nodes = HashMap::new();
    let mut cells = Vec::new();
    let lines: Vec<&str> = text.lines().map(str::trim).collect();

    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];
        if line == "$Nodes" {
            i += 1;
            let count_line = lines
                .get(i)
                .ok_or_else(|| Error::InvalidInput("Gmsh $Nodes is missing its count".into()))?;
            let count = count_line
                .parse::<usize>()
                .map_err(|_| Error::InvalidInput("invalid Gmsh node count".into()))?;
            if count > MAX_SIMULATION_POINTS {
                return Err(Error::LimitExceeded(format!(
                    "Gmsh node count exceeds {MAX_SIMULATION_POINTS}"
                )));
            }
            i += 1;
            for _ in 0..count {
                let record = lines
                    .get(i)
                    .filter(|line| !line.starts_with('$'))
                    .ok_or_else(|| {
                        Error::InvalidInput(
                            "Gmsh $Nodes section ends before its declared count".into(),
                        )
                    })?;
                let parts: Vec<&str> = record.split_whitespace().collect();
                if parts.len() < 4 {
                    return Err(Error::InvalidInput("incomplete Gmsh node record".into()));
                }
                let id = parts[0]
                    .parse::<usize>()
                    .map_err(|_| Error::InvalidInput("invalid Gmsh node tag".into()))?;
                let x = parts[1]
                    .parse::<f64>()
                    .map_err(|_| Error::InvalidInput("invalid Gmsh node x coordinate".into()))?;
                let y = parts[2]
                    .parse::<f64>()
                    .map_err(|_| Error::InvalidInput("invalid Gmsh node y coordinate".into()))?;
                let z = parts[3]
                    .parse::<f64>()
                    .map_err(|_| Error::InvalidInput("invalid Gmsh node z coordinate".into()))?;
                if id == 0 || [x, y, z].iter().any(|value| !value.is_finite()) {
                    return Err(Error::InvalidInput(
                        "Gmsh node has a zero tag or non-finite coordinate".into(),
                    ));
                }
                if nodes.insert(id, MeshNode { x, y, scalar: None }).is_some() {
                    return Err(Error::InvalidInput(format!("duplicate Gmsh node tag {id}")));
                }
                i += 1;
            }
        } else if line == "$Elements" {
            i += 1;
            let count_line = lines
                .get(i)
                .ok_or_else(|| Error::InvalidInput("Gmsh $Elements is missing its count".into()))?;
            let count = count_line
                .parse::<usize>()
                .map_err(|_| Error::InvalidInput("invalid Gmsh element count".into()))?;
            if count > MAX_SIMULATION_CELLS {
                return Err(Error::LimitExceeded(format!(
                    "Gmsh element count exceeds {MAX_SIMULATION_CELLS}"
                )));
            }
            i += 1;
            let mut seen_elements = HashSet::with_capacity(count);
            for _ in 0..count {
                let record = lines
                    .get(i)
                    .filter(|line| !line.starts_with('$'))
                    .ok_or_else(|| {
                        Error::InvalidInput(
                            "Gmsh $Elements section ends before its declared count".into(),
                        )
                    })?;
                let parts: Vec<&str> = record.split_whitespace().collect();
                if parts.len() < 4 {
                    return Err(Error::InvalidInput("incomplete Gmsh element record".into()));
                }
                let element_tag = parts[0]
                    .parse::<usize>()
                    .map_err(|_| Error::InvalidInput("invalid Gmsh element tag".into()))?;
                let element_type = parts[1]
                    .parse::<usize>()
                    .map_err(|_| Error::InvalidInput("invalid Gmsh element type".into()))?;
                let num_tags = parts[2]
                    .parse::<usize>()
                    .map_err(|_| Error::InvalidInput("invalid Gmsh element tag count".into()))?;
                if element_tag == 0 || !seen_elements.insert(element_tag) {
                    return Err(Error::InvalidInput(format!(
                        "invalid or duplicate Gmsh element tag {element_tag}"
                    )));
                }
                let node_start = 3usize.checked_add(num_tags).ok_or_else(|| {
                    Error::LimitExceeded("Gmsh element tag offset overflowed".into())
                })?;
                let (node_count, vtk_type, corner_count) = gmsh_element_info(element_type)
                    .ok_or_else(|| {
                        Error::Unsupported(format!(
                            "Gmsh element type {element_type} is not supported"
                        ))
                    })?;
                let node_end = node_start.checked_add(node_count).ok_or_else(|| {
                    Error::LimitExceeded("Gmsh element node list size overflowed".into())
                })?;
                if node_end > parts.len() {
                    return Err(Error::InvalidInput(format!(
                        "Gmsh element {element_tag} has an incomplete node list"
                    )));
                }
                let node_ids = parts[node_start..node_end]
                    .iter()
                    .map(|token| {
                        token.parse::<usize>().map_err(|_| {
                            Error::InvalidInput(format!(
                                "invalid node tag in Gmsh element {element_tag}"
                            ))
                        })
                    })
                    .collect::<Result<Vec<_>>>()?;
                for &node_id in &node_ids {
                    if !nodes.contains_key(&node_id) {
                        return Err(Error::InvalidInput(format!(
                            "Gmsh element {element_tag} references unknown node {node_id}"
                        )));
                    }
                }
                let scalar = data.element_scalars.get(&element_tag).copied();
                for primitive in vtk_cell_primitives(vtk_type, &node_ids[..corner_count], scalar)? {
                    if cells.len() >= MAX_VTK_RENDERED_PRIMITIVES {
                        return Err(Error::LimitExceeded(format!(
                            "Gmsh rendered primitive count exceeds {MAX_VTK_RENDERED_PRIMITIVES}"
                        )));
                    }
                    cells.push(primitive);
                }
                i += 1;
            }
        }
        i += 1;
    }

    Ok((nodes, cells, "Gmsh FEA Mesh".into()))
}

fn gmsh_section_lines<'a>(lines: &'a [&'a str], name: &str) -> Result<&'a [&'a str]> {
    let marker = format!("${name}");
    let end_marker = format!("$End{name}");
    let start = lines
        .iter()
        .position(|line| *line == marker)
        .ok_or_else(|| Error::InvalidInput(format!("Gmsh file is missing {marker}")))?;
    let end = lines[start + 1..]
        .iter()
        .position(|line| *line == end_marker)
        .map(|offset| start + 1 + offset)
        .ok_or_else(|| Error::InvalidInput(format!("Gmsh file is missing {end_marker}")))?;
    Ok(&lines[start + 1..end])
}

fn next_gmsh_value<'a, T>(tokens: &mut impl Iterator<Item = &'a str>, context: &str) -> Result<T>
where
    T: std::str::FromStr,
    T::Err: std::fmt::Display,
{
    let value = tokens
        .next()
        .ok_or_else(|| Error::InvalidInput(format!("missing Gmsh {context}")))?;
    value
        .parse::<T>()
        .map_err(|error| Error::InvalidInput(format!("invalid Gmsh {context} '{value}': {error}")))
}

fn parse_gmsh_v41(
    lines: &[&str],
    data: &GmshData,
) -> Result<(HashMap<usize, MeshNode>, Vec<MeshCell>, String)> {
    let node_lines = gmsh_section_lines(lines, "Nodes")?;
    let mut node_tokens = node_lines.iter().copied().flat_map(str::split_whitespace);
    let block_count = next_gmsh_value::<usize>(&mut node_tokens, "node block count")?;
    let declared_node_count = next_gmsh_value::<usize>(&mut node_tokens, "node count")?;
    let min_node_tag = next_gmsh_value::<usize>(&mut node_tokens, "minimum node tag")?;
    let max_node_tag = next_gmsh_value::<usize>(&mut node_tokens, "maximum node tag")?;
    if block_count > MAX_SIMULATION_POINTS || declared_node_count > MAX_SIMULATION_POINTS {
        return Err(Error::LimitExceeded(format!(
            "Gmsh MSH 4.1 node or block count exceeds {MAX_SIMULATION_POINTS}"
        )));
    }
    if declared_node_count > 0 && (min_node_tag == 0 || min_node_tag > max_node_tag) {
        return Err(Error::InvalidInput("invalid Gmsh node tag range".into()));
    }

    let mut nodes = HashMap::with_capacity(declared_node_count);
    for _ in 0..block_count {
        let entity_dim = next_gmsh_value::<usize>(&mut node_tokens, "node entity dimension")?;
        let _entity_tag = next_gmsh_value::<usize>(&mut node_tokens, "node entity tag")?;
        let parametric = next_gmsh_value::<usize>(&mut node_tokens, "parametric flag")?;
        let nodes_in_block = next_gmsh_value::<usize>(&mut node_tokens, "node block size")?;
        if entity_dim > 3 || parametric > 1 {
            return Err(Error::InvalidInput(
                "invalid Gmsh MSH 4.1 node block header".into(),
            ));
        }
        if nodes_in_block > declared_node_count.saturating_sub(nodes.len()) {
            return Err(Error::InvalidInput(
                "Gmsh node block sizes exceed the declared node count".into(),
            ));
        }
        let mut tags = Vec::with_capacity(nodes_in_block);
        for _ in 0..nodes_in_block {
            let tag = next_gmsh_value::<usize>(&mut node_tokens, "node tag")?;
            if tag == 0 || tag < min_node_tag || tag > max_node_tag || nodes.contains_key(&tag) {
                return Err(Error::InvalidInput(format!(
                    "invalid or duplicate Gmsh node tag {tag}"
                )));
            }
            tags.push(tag);
        }
        for tag in tags {
            let x = next_gmsh_value::<f64>(&mut node_tokens, "node x coordinate")?;
            let y = next_gmsh_value::<f64>(&mut node_tokens, "node y coordinate")?;
            let z = next_gmsh_value::<f64>(&mut node_tokens, "node z coordinate")?;
            if [x, y, z]
                .iter()
                .any(|coordinate| !coordinate.is_finite() || coordinate.abs() > 1.0e12)
            {
                return Err(Error::InvalidInput(
                    "Gmsh MSH 4.1 node has a non-finite or out-of-range coordinate".into(),
                ));
            }
            if parametric == 1 {
                for _ in 0..entity_dim {
                    let parameter = next_gmsh_value::<f64>(&mut node_tokens, "node parameter")?;
                    if !parameter.is_finite() {
                        return Err(Error::InvalidInput(
                            "Gmsh node has a non-finite parametric coordinate".into(),
                        ));
                    }
                }
            }
            nodes.insert(tag, MeshNode { x, y, scalar: None });
        }
    }
    if nodes.len() != declared_node_count || node_tokens.next().is_some() {
        return Err(Error::InvalidInput(
            "Gmsh MSH 4.1 node blocks do not match the declared total".into(),
        ));
    }

    let element_lines = gmsh_section_lines(lines, "Elements")?;
    let mut element_tokens = element_lines
        .iter()
        .copied()
        .flat_map(str::split_whitespace);
    let block_count = next_gmsh_value::<usize>(&mut element_tokens, "element block count")?;
    let declared_element_count = next_gmsh_value::<usize>(&mut element_tokens, "element count")?;
    let min_element_tag = next_gmsh_value::<usize>(&mut element_tokens, "minimum element tag")?;
    let max_element_tag = next_gmsh_value::<usize>(&mut element_tokens, "maximum element tag")?;
    if block_count > MAX_SIMULATION_CELLS || declared_element_count > MAX_SIMULATION_CELLS {
        return Err(Error::LimitExceeded(format!(
            "Gmsh MSH 4.1 element or block count exceeds {MAX_SIMULATION_CELLS}"
        )));
    }
    if declared_element_count > 0 && (min_element_tag == 0 || min_element_tag > max_element_tag) {
        return Err(Error::InvalidInput("invalid Gmsh element tag range".into()));
    }

    let mut cells = Vec::new();
    let mut seen_elements = HashSet::with_capacity(declared_element_count);
    for _ in 0..block_count {
        let entity_dim = next_gmsh_value::<usize>(&mut element_tokens, "element entity dimension")?;
        let _entity_tag = next_gmsh_value::<usize>(&mut element_tokens, "element entity tag")?;
        let element_type = next_gmsh_value::<usize>(&mut element_tokens, "element type")?;
        let elements_in_block =
            next_gmsh_value::<usize>(&mut element_tokens, "element block size")?;
        if entity_dim > 3
            || elements_in_block > declared_element_count.saturating_sub(seen_elements.len())
        {
            return Err(Error::InvalidInput(
                "invalid Gmsh MSH 4.1 element block header".into(),
            ));
        }
        let (node_count, vtk_type, corner_count) =
            gmsh_element_info(element_type).ok_or_else(|| {
                Error::Unsupported(format!("Gmsh element type {element_type} is not supported"))
            })?;
        for _ in 0..elements_in_block {
            let element_tag = next_gmsh_value::<usize>(&mut element_tokens, "element tag")?;
            if element_tag == 0
                || element_tag < min_element_tag
                || element_tag > max_element_tag
                || !seen_elements.insert(element_tag)
            {
                return Err(Error::InvalidInput(format!(
                    "invalid or duplicate Gmsh element tag {element_tag}"
                )));
            }
            let mut node_ids = Vec::with_capacity(node_count);
            for _ in 0..node_count {
                let node_id = next_gmsh_value::<usize>(&mut element_tokens, "element node tag")?;
                if !nodes.contains_key(&node_id) {
                    return Err(Error::InvalidInput(format!(
                        "Gmsh element {element_tag} references unknown node {node_id}"
                    )));
                }
                node_ids.push(node_id);
            }
            let corners = &node_ids[..corner_count];
            let scalar = data.element_scalars.get(&element_tag).copied();
            for primitive in vtk_cell_primitives(vtk_type, corners, scalar)? {
                if cells.len() >= MAX_VTK_RENDERED_PRIMITIVES {
                    return Err(Error::LimitExceeded(format!(
                        "Gmsh rendered primitive count exceeds {MAX_VTK_RENDERED_PRIMITIVES}"
                    )));
                }
                cells.push(primitive);
            }
        }
    }
    if seen_elements.len() != declared_element_count || element_tokens.next().is_some() {
        return Err(Error::InvalidInput(
            "Gmsh MSH 4.1 element blocks do not match the declared total".into(),
        ));
    }
    Ok((nodes, cells, "Gmsh MSH 4.1 FEA Mesh".into()))
}

fn parse_gmsh_v40(
    lines: &[&str],
    data: &GmshData,
) -> Result<(HashMap<usize, MeshNode>, Vec<MeshCell>, String)> {
    let node_lines = gmsh_section_lines(lines, "Nodes")?;
    let mut node_tokens = node_lines.iter().copied().flat_map(str::split_whitespace);
    let block_count = next_gmsh_value::<usize>(&mut node_tokens, "node block count")?;
    let declared_node_count = next_gmsh_value::<usize>(&mut node_tokens, "node count")?;
    if block_count > MAX_SIMULATION_POINTS || declared_node_count > MAX_SIMULATION_POINTS {
        return Err(Error::LimitExceeded(format!(
            "Gmsh MSH 4.0 node or block count exceeds {MAX_SIMULATION_POINTS}"
        )));
    }

    let mut nodes = HashMap::with_capacity(declared_node_count);
    for _ in 0..block_count {
        // MSH 4.0 writes entityTag before entityDim and interleaves each node
        // tag with that node's coordinates. MSH 4.1 changes both conventions.
        let _entity_tag = next_gmsh_value::<usize>(&mut node_tokens, "node entity tag")?;
        let entity_dim = next_gmsh_value::<usize>(&mut node_tokens, "node entity dimension")?;
        let parametric = next_gmsh_value::<usize>(&mut node_tokens, "parametric flag")?;
        let nodes_in_block = next_gmsh_value::<usize>(&mut node_tokens, "node block size")?;
        if entity_dim > 3 || parametric > 1 {
            return Err(Error::InvalidInput(
                "invalid Gmsh MSH 4.0 node block header".into(),
            ));
        }
        if nodes_in_block > declared_node_count.saturating_sub(nodes.len()) {
            return Err(Error::InvalidInput(
                "Gmsh MSH 4.0 node block sizes exceed the declared node count".into(),
            ));
        }
        for _ in 0..nodes_in_block {
            let tag = next_gmsh_value::<usize>(&mut node_tokens, "node tag")?;
            if tag == 0 || nodes.contains_key(&tag) {
                return Err(Error::InvalidInput(format!(
                    "invalid or duplicate Gmsh node tag {tag}"
                )));
            }
            let x = next_gmsh_value::<f64>(&mut node_tokens, "node x coordinate")?;
            let y = next_gmsh_value::<f64>(&mut node_tokens, "node y coordinate")?;
            let z = next_gmsh_value::<f64>(&mut node_tokens, "node z coordinate")?;
            if [x, y, z]
                .iter()
                .any(|coordinate| !coordinate.is_finite() || coordinate.abs() > 1.0e12)
            {
                return Err(Error::InvalidInput(
                    "Gmsh MSH 4.0 node has a non-finite or out-of-range coordinate".into(),
                ));
            }
            if parametric == 1 {
                for _ in 0..entity_dim {
                    let parameter = next_gmsh_value::<f64>(&mut node_tokens, "node parameter")?;
                    if !parameter.is_finite() {
                        return Err(Error::InvalidInput(
                            "Gmsh node has a non-finite parametric coordinate".into(),
                        ));
                    }
                }
            }
            nodes.insert(tag, MeshNode { x, y, scalar: None });
        }
    }
    if nodes.len() != declared_node_count || node_tokens.next().is_some() {
        return Err(Error::InvalidInput(
            "Gmsh MSH 4.0 node blocks do not match the declared total".into(),
        ));
    }

    let element_lines = gmsh_section_lines(lines, "Elements")?;
    let mut element_tokens = element_lines
        .iter()
        .copied()
        .flat_map(str::split_whitespace);
    let block_count = next_gmsh_value::<usize>(&mut element_tokens, "element block count")?;
    let declared_element_count = next_gmsh_value::<usize>(&mut element_tokens, "element count")?;
    if block_count > MAX_SIMULATION_CELLS || declared_element_count > MAX_SIMULATION_CELLS {
        return Err(Error::LimitExceeded(format!(
            "Gmsh MSH 4.0 element or block count exceeds {MAX_SIMULATION_CELLS}"
        )));
    }

    let mut cells = Vec::new();
    let mut seen_elements = HashSet::with_capacity(declared_element_count);
    for _ in 0..block_count {
        let _entity_tag = next_gmsh_value::<usize>(&mut element_tokens, "element entity tag")?;
        let entity_dim = next_gmsh_value::<usize>(&mut element_tokens, "element entity dimension")?;
        let element_type = next_gmsh_value::<usize>(&mut element_tokens, "element type")?;
        let elements_in_block =
            next_gmsh_value::<usize>(&mut element_tokens, "element block size")?;
        if entity_dim > 3
            || elements_in_block > declared_element_count.saturating_sub(seen_elements.len())
        {
            return Err(Error::InvalidInput(
                "invalid Gmsh MSH 4.0 element block header".into(),
            ));
        }
        let (node_count, vtk_type, corner_count) =
            gmsh_element_info(element_type).ok_or_else(|| {
                Error::Unsupported(format!("Gmsh element type {element_type} is not supported"))
            })?;
        for _ in 0..elements_in_block {
            let element_tag = next_gmsh_value::<usize>(&mut element_tokens, "element tag")?;
            if element_tag == 0 || !seen_elements.insert(element_tag) {
                return Err(Error::InvalidInput(format!(
                    "invalid or duplicate Gmsh element tag {element_tag}"
                )));
            }
            let mut node_ids = Vec::with_capacity(node_count);
            for _ in 0..node_count {
                let node_id = next_gmsh_value::<usize>(&mut element_tokens, "element node tag")?;
                if !nodes.contains_key(&node_id) {
                    return Err(Error::InvalidInput(format!(
                        "Gmsh element {element_tag} references unknown node {node_id}"
                    )));
                }
                node_ids.push(node_id);
            }
            let scalar = data.element_scalars.get(&element_tag).copied();
            for primitive in vtk_cell_primitives(vtk_type, &node_ids[..corner_count], scalar)? {
                if cells.len() >= MAX_VTK_RENDERED_PRIMITIVES {
                    return Err(Error::LimitExceeded(format!(
                        "Gmsh rendered primitive count exceeds {MAX_VTK_RENDERED_PRIMITIVES}"
                    )));
                }
                cells.push(primitive);
            }
        }
    }
    if seen_elements.len() != declared_element_count || element_tokens.next().is_some() {
        return Err(Error::InvalidInput(
            "Gmsh MSH 4.0 element blocks do not match the declared total".into(),
        ));
    }
    Ok((nodes, cells, "Gmsh MSH 4.0 FEA Mesh".into()))
}

fn gmsh_element_info(element_type: usize) -> Option<(usize, usize, usize)> {
    Some(match element_type {
        1 => (2, 3, 2),
        2 => (3, 5, 3),
        3 => (4, 9, 4),
        4 => (4, 10, 4),
        5 => (8, 12, 8),
        6 => (6, 13, 6),
        7 => (5, 14, 5),
        8 => (3, 21, 2),
        9 => (6, 22, 3),
        10 => (9, 23, 4),
        11 => (10, 24, 4),
        12 => (27, 25, 8),
        13 => (18, 26, 6),
        14 => (14, 27, 5),
        15 => (1, 1, 1),
        16 => (8, 23, 4),
        17 => (20, 25, 8),
        18 => (15, 26, 6),
        19 => (13, 27, 5),
        20 => (9, 22, 3),
        21 => (10, 22, 3),
        22 => (12, 22, 3),
        23 => (15, 22, 3),
        24 => (15, 22, 3),
        25 => (21, 22, 3),
        26 => (4, 21, 2),
        27 => (5, 21, 2),
        28 => (6, 21, 2),
        29 => (20, 24, 4),
        30 => (35, 24, 4),
        31 => (56, 24, 4),
        92 => (64, 25, 8),
        93 => (125, 25, 8),
        _ => return None,
    })
}

fn append_vtk_legacy_ascii_line(output: &mut String, line: &str) -> Result<()> {
    let new_size = output
        .len()
        .checked_add(line.len())
        .and_then(|size| size.checked_add(1))
        .ok_or_else(|| Error::LimitExceeded("VTK legacy decoded text size overflowed".into()))?;
    if new_size > MAX_VTK_ARRAY_TEXT_BYTES {
        return Err(Error::LimitExceeded(format!(
            "VTK legacy decoded text exceeds {MAX_VTK_ARRAY_TEXT_BYTES} bytes"
        )));
    }
    output.push_str(line);
    output.push('\n');
    Ok(())
}

fn append_vtk_legacy_ascii_f64_values(output: &mut String, values: &[f64]) -> Result<()> {
    for chunk in values.chunks(16) {
        let mut line = String::new();
        for (index, value) in chunk.iter().enumerate() {
            if index != 0 {
                line.push(' ');
            }
            line.push_str(&value.to_string());
        }
        append_vtk_legacy_ascii_line(output, &line)?;
    }
    Ok(())
}

fn append_vtk_legacy_ascii_index_values(output: &mut String, values: &[usize]) -> Result<()> {
    for chunk in values.chunks(16) {
        let line = chunk
            .iter()
            .map(usize::to_string)
            .collect::<Vec<_>>()
            .join(" ");
        append_vtk_legacy_ascii_line(output, &line)?;
    }
    Ok(())
}

fn parse_vtk_binary(bytes: &[u8], max_lines: usize) -> Result<SimulationParseOutput> {
    let mut reader = VtkLegacyBinaryReader::new(bytes, max_lines);
    let header = reader.line("file identifier")?;
    if !header.starts_with("# vtk DataFile Version") {
        return Err(Error::InvalidInput("invalid VTK legacy file header".into()));
    }
    let title = reader.line("title")?;
    if !reader
        .line("encoding")?
        .trim()
        .eq_ignore_ascii_case("BINARY")
    {
        return Err(Error::InvalidInput(
            "VTK legacy binary parser requires a BINARY header".into(),
        ));
    }
    let dataset_line = reader.line("dataset declaration")?;
    let dataset_fields = dataset_line.split_whitespace().collect::<Vec<_>>();
    if dataset_fields.len() != 2 || !dataset_fields[0].eq_ignore_ascii_case("DATASET") {
        return Err(Error::InvalidInput(
            "VTK legacy file is missing its DATASET declaration".into(),
        ));
    }
    let dataset = dataset_fields[1].to_ascii_uppercase();
    if !matches!(dataset.as_str(), "UNSTRUCTURED_GRID" | "POLYDATA") {
        return Err(Error::Unsupported(format!(
            "binary VTK legacy dataset '{dataset}' is unsupported; use UNSTRUCTURED_GRID or POLYDATA"
        )));
    }

    let mut normalized = String::new();
    append_vtk_legacy_ascii_line(&mut normalized, &header)?;
    append_vtk_legacy_ascii_line(&mut normalized, &title)?;
    append_vtk_legacy_ascii_line(&mut normalized, "ASCII")?;
    append_vtk_legacy_ascii_line(&mut normalized, &dataset_line)?;
    let mut point_data_count = None;
    let mut cell_data_count = None;
    let mut data_association = None;
    let mut total_values = 0usize;

    while reader.position < bytes.len() {
        let line = reader.line("data section")?;
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let fields = trimmed.split_whitespace().collect::<Vec<_>>();
        let keyword = fields[0].to_ascii_uppercase();
        match keyword.as_str() {
            "POINTS" => {
                if fields.len() != 3 {
                    return Err(Error::InvalidInput("invalid VTK POINTS header".into()));
                }
                let count = parse_vtk_legacy_count(fields[1], "point")?;
                if count > MAX_SIMULATION_POINTS {
                    return Err(Error::LimitExceeded(format!(
                        "VTK legacy point count exceeds {MAX_SIMULATION_POINTS}"
                    )));
                }
                let value_count = count.checked_mul(3).ok_or_else(|| {
                    Error::LimitExceeded("VTK legacy coordinate count overflowed".into())
                })?;
                let scalar_type = parse_vtk_legacy_binary_type(fields[2])?;
                let values =
                    reader.numeric_values(value_count, scalar_type, "point coordinates")?;
                add_vtk_legacy_binary_values(&mut total_values, value_count, "point coordinates")?;
                append_vtk_legacy_ascii_line(&mut normalized, trimmed)?;
                append_vtk_legacy_ascii_f64_values(&mut normalized, &values)?;
            }
            "CELLS" | "VERTICES" | "LINES" | "POLYGONS" | "TRIANGLE_STRIPS" => {
                let valid_dataset = if keyword == "CELLS" {
                    dataset == "UNSTRUCTURED_GRID"
                } else {
                    dataset == "POLYDATA"
                };
                if !valid_dataset || fields.len() != 3 {
                    return Err(Error::InvalidInput(format!(
                        "invalid VTK {} header for dataset {dataset}",
                        fields[0]
                    )));
                }
                let cells = parse_vtk_legacy_count(fields[1], "cell")?;
                let value_count = parse_vtk_legacy_count(fields[2], "cell connectivity value")?;
                if cells > MAX_SIMULATION_CELLS || value_count > MAX_VTK_ARRAY_VALUES {
                    return Err(Error::LimitExceeded(
                        "VTK legacy cell/connectivity count exceeds the mesh limit".into(),
                    ));
                }
                let values = reader.index_values(value_count, fields[0])?;
                add_vtk_legacy_binary_values(&mut total_values, value_count, fields[0])?;
                append_vtk_legacy_ascii_line(&mut normalized, trimmed)?;
                append_vtk_legacy_ascii_index_values(&mut normalized, &values)?;
            }
            "CELL_TYPES" => {
                if dataset != "UNSTRUCTURED_GRID" || fields.len() != 2 {
                    return Err(Error::InvalidInput("invalid VTK CELL_TYPES header".into()));
                }
                let count = parse_vtk_legacy_count(fields[1], "cell type")?;
                let values = reader.index_values(count, "CELL_TYPES")?;
                add_vtk_legacy_binary_values(&mut total_values, count, "CELL_TYPES")?;
                append_vtk_legacy_ascii_line(&mut normalized, trimmed)?;
                append_vtk_legacy_ascii_index_values(&mut normalized, &values)?;
            }
            "POINT_DATA" | "CELL_DATA" => {
                if fields.len() != 2 {
                    return Err(Error::InvalidInput(format!(
                        "invalid VTK {} header",
                        fields[0]
                    )));
                }
                let count = parse_vtk_legacy_count(fields[1], "data item")?;
                if count > MAX_SIMULATION_POINTS.max(MAX_SIMULATION_CELLS) {
                    return Err(Error::LimitExceeded(
                        "VTK legacy data item count exceeds the mesh limit".into(),
                    ));
                }
                if keyword == "POINT_DATA" {
                    if point_data_count.replace(count).is_some() {
                        return Err(Error::InvalidInput(
                            "duplicate VTK POINT_DATA section".into(),
                        ));
                    }
                    data_association = Some(VtkLegacyAssociation::Points);
                } else {
                    if cell_data_count.replace(count).is_some() {
                        return Err(Error::InvalidInput(
                            "duplicate VTK CELL_DATA section".into(),
                        ));
                    }
                    data_association = Some(VtkLegacyAssociation::Cells);
                }
                append_vtk_legacy_ascii_line(&mut normalized, trimmed)?;
            }
            "SCALARS" => {
                if fields.len() < 3 || fields.len() > 4 {
                    return Err(Error::InvalidInput("invalid VTK SCALARS header".into()));
                }
                let association = data_association.ok_or_else(|| {
                    Error::InvalidInput(
                        "VTK legacy SCALARS must follow POINT_DATA or CELL_DATA".into(),
                    )
                })?;
                let item_count = match association {
                    VtkLegacyAssociation::Points => point_data_count,
                    VtkLegacyAssociation::Cells => cell_data_count,
                }
                .ok_or_else(|| {
                    Error::InvalidInput("VTK legacy scalar data has no item count".into())
                })?;
                let components = fields
                    .get(3)
                    .map(|value| parse_vtk_legacy_count(value, "scalar component"))
                    .transpose()?
                    .unwrap_or(1);
                if components == 0 || components > 64 {
                    return Err(Error::LimitExceeded(
                        "VTK legacy scalar component count must be between 1 and 64".into(),
                    ));
                }
                let scalar_type = parse_vtk_legacy_binary_type(fields[2])?;
                let lookup_line = reader.line("SCALARS LOOKUP_TABLE")?;
                let lookup_fields = lookup_line.split_whitespace().collect::<Vec<_>>();
                if lookup_fields.len() < 2 || !lookup_fields[0].eq_ignore_ascii_case("LOOKUP_TABLE")
                {
                    return Err(Error::InvalidInput(
                        "VTK legacy SCALARS is missing LOOKUP_TABLE".into(),
                    ));
                }
                let value_count = item_count.checked_mul(components).ok_or_else(|| {
                    Error::LimitExceeded("VTK legacy scalar value count overflowed".into())
                })?;
                if value_count > MAX_VTK_ARRAY_VALUES {
                    return Err(Error::LimitExceeded(
                        "VTK legacy scalar array exceeds the per-array value limit".into(),
                    ));
                }
                let values = reader.numeric_values(value_count, scalar_type, "SCALARS values")?;
                add_vtk_legacy_binary_values(&mut total_values, value_count, "SCALARS")?;
                append_vtk_legacy_ascii_line(&mut normalized, trimmed)?;
                append_vtk_legacy_ascii_line(&mut normalized, lookup_line.trim())?;
                append_vtk_legacy_ascii_f64_values(&mut normalized, &values)?;
            }
            "VECTORS" | "NORMALS" | "TENSORS" => {
                if fields.len() != 3 {
                    return Err(Error::InvalidInput(format!(
                        "invalid VTK {} header",
                        fields[0]
                    )));
                }
                let association = data_association.ok_or_else(|| {
                    Error::InvalidInput(format!(
                        "VTK {} must follow POINT_DATA or CELL_DATA",
                        fields[0]
                    ))
                })?;
                let count = match association {
                    VtkLegacyAssociation::Points => point_data_count,
                    VtkLegacyAssociation::Cells => cell_data_count,
                }
                .ok_or_else(|| {
                    Error::InvalidInput("VTK attribute data has no item count".into())
                })?;
                let components = if keyword == "TENSORS" { 9 } else { 3 };
                let value_count = count.checked_mul(components).ok_or_else(|| {
                    Error::LimitExceeded("VTK attribute value count overflowed".into())
                })?;
                let scalar_type = parse_vtk_legacy_binary_type(fields[2])?;
                reader.skip_values(value_count, scalar_type, fields[0])?;
                add_vtk_legacy_binary_values(&mut total_values, value_count, fields[0])?;
                append_vtk_legacy_ascii_line(&mut normalized, trimmed)?;
            }
            "TEXTURE_COORDINATES" => {
                if fields.len() != 4 {
                    return Err(Error::InvalidInput(
                        "invalid VTK TEXTURE_COORDINATES header".into(),
                    ));
                }
                let association = data_association.ok_or_else(|| {
                    Error::InvalidInput(
                        "VTK TEXTURE_COORDINATES must follow POINT_DATA or CELL_DATA".into(),
                    )
                })?;
                let count = match association {
                    VtkLegacyAssociation::Points => point_data_count,
                    VtkLegacyAssociation::Cells => cell_data_count,
                }
                .ok_or_else(|| {
                    Error::InvalidInput("VTK attribute data has no item count".into())
                })?;
                let components = parse_vtk_legacy_count(fields[2], "texture-coordinate component")?;
                if components == 0 || components > 3 {
                    return Err(Error::InvalidInput(
                        "VTK texture-coordinate component count must be between 1 and 3".into(),
                    ));
                }
                let value_count = count.checked_mul(components).ok_or_else(|| {
                    Error::LimitExceeded("VTK texture-coordinate value count overflowed".into())
                })?;
                let scalar_type = parse_vtk_legacy_binary_type(fields[3])?;
                reader.skip_values(value_count, scalar_type, "TEXTURE_COORDINATES")?;
                add_vtk_legacy_binary_values(
                    &mut total_values,
                    value_count,
                    "TEXTURE_COORDINATES",
                )?;
                append_vtk_legacy_ascii_line(&mut normalized, trimmed)?;
            }
            "COLOR_SCALARS" => {
                if fields.len() != 3 {
                    return Err(Error::InvalidInput(
                        "invalid VTK COLOR_SCALARS header".into(),
                    ));
                }
                let association = data_association.ok_or_else(|| {
                    Error::InvalidInput(
                        "VTK COLOR_SCALARS must follow POINT_DATA or CELL_DATA".into(),
                    )
                })?;
                let count = match association {
                    VtkLegacyAssociation::Points => point_data_count,
                    VtkLegacyAssociation::Cells => cell_data_count,
                }
                .ok_or_else(|| {
                    Error::InvalidInput("VTK attribute data has no item count".into())
                })?;
                let components = parse_vtk_legacy_count(fields[2], "color scalar component")?;
                if components == 0 || components > 4 {
                    return Err(Error::InvalidInput(
                        "VTK COLOR_SCALARS component count must be between 1 and 4".into(),
                    ));
                }
                let value_count = count.checked_mul(components).ok_or_else(|| {
                    Error::LimitExceeded("VTK color scalar value count overflowed".into())
                })?;
                reader.skip_values(value_count, VtkScalarType::U8, "COLOR_SCALARS")?;
                add_vtk_legacy_binary_values(&mut total_values, value_count, "COLOR_SCALARS")?;
                append_vtk_legacy_ascii_line(&mut normalized, trimmed)?;
            }
            "FIELD" => {
                if fields.len() != 3 {
                    return Err(Error::InvalidInput("invalid VTK FIELD header".into()));
                }
                let array_count = parse_vtk_legacy_count(fields[2], "FIELD array")?;
                if array_count > 100_000 {
                    return Err(Error::LimitExceeded(
                        "VTK FIELD array count exceeds 100000".into(),
                    ));
                }
                for _ in 0..array_count {
                    let descriptor = reader.line("FIELD array descriptor")?;
                    let array_fields = descriptor.split_whitespace().collect::<Vec<_>>();
                    if array_fields.len() != 4 {
                        return Err(Error::InvalidInput(
                            "invalid VTK FIELD array descriptor".into(),
                        ));
                    }
                    let components = parse_vtk_legacy_count(array_fields[1], "FIELD component")?;
                    let tuples = parse_vtk_legacy_count(array_fields[2], "FIELD tuple")?;
                    if components == 0 || components > 64 {
                        return Err(Error::LimitExceeded(
                            "VTK FIELD component count must be between 1 and 64".into(),
                        ));
                    }
                    let value_count = components.checked_mul(tuples).ok_or_else(|| {
                        Error::LimitExceeded("VTK FIELD value count overflowed".into())
                    })?;
                    let scalar_type = parse_vtk_legacy_binary_type(array_fields[3])?;
                    reader.skip_values(value_count, scalar_type, "FIELD")?;
                    add_vtk_legacy_binary_values(&mut total_values, value_count, "FIELD")?;
                }
                append_vtk_legacy_ascii_line(&mut normalized, trimmed)?;
            }
            "LOOKUP_TABLE" => {
                if fields.len() != 3 {
                    return Err(Error::InvalidInput(
                        "invalid VTK LOOKUP_TABLE header".into(),
                    ));
                }
                let entries = parse_vtk_legacy_count(fields[2], "lookup table entry")?;
                let value_count = entries.checked_mul(4).ok_or_else(|| {
                    Error::LimitExceeded("VTK lookup table value count overflowed".into())
                })?;
                reader.skip_values(value_count, VtkScalarType::U8, "LOOKUP_TABLE")?;
                add_vtk_legacy_binary_values(&mut total_values, value_count, "LOOKUP_TABLE")?;
                append_vtk_legacy_ascii_line(&mut normalized, trimmed)?;
            }
            _ => {
                return Err(Error::Unsupported(format!(
                    "binary VTK legacy section '{}' is unsupported",
                    fields[0]
                )));
            }
        }
    }
    parse_vtk(&normalized, max_lines)
}

fn parse_vtk(text: &str, max_lines: usize) -> Result<SimulationParseOutput> {
    if text.lines().count() > max_lines {
        return Err(Error::LimitExceeded(format!(
            "VTK legacy input exceeds {max_lines} lines"
        )));
    }
    let lines = text.lines().map(str::trim).collect::<Vec<_>>();
    if lines
        .iter()
        .any(|line| line.len() > MAX_VTK_LEGACY_LINE_BYTES)
    {
        return Err(Error::LimitExceeded(format!(
            "VTK legacy line exceeds {MAX_VTK_LEGACY_LINE_BYTES} bytes"
        )));
    }
    if lines.len() < 4 || !lines[0].starts_with("# vtk DataFile Version") {
        return Err(Error::InvalidInput("invalid VTK legacy file header".into()));
    }
    if lines[2].eq_ignore_ascii_case("BINARY") {
        return Err(Error::Unsupported(
            "binary VTK legacy files are not supported; export ASCII".into(),
        ));
    }
    if !lines[2].eq_ignore_ascii_case("ASCII") {
        return Err(Error::InvalidInput(
            "VTK legacy file must declare ASCII or BINARY encoding".into(),
        ));
    }
    let dataset_fields = lines[3].split_whitespace().collect::<Vec<_>>();
    if dataset_fields.len() != 2 || dataset_fields[0] != "DATASET" {
        return Err(Error::InvalidInput(
            "VTK legacy file is missing its DATASET declaration".into(),
        ));
    }
    let dataset = dataset_fields[1];
    if !matches!(dataset, "UNSTRUCTURED_GRID" | "POLYDATA") {
        return Err(Error::Unsupported(format!(
            "VTK legacy dataset '{dataset}' is unsupported; use UNSTRUCTURED_GRID or POLYDATA"
        )));
    }

    let mut title = lines[1].to_owned();
    if title.is_empty() {
        title = "VTK Simulation Field".into();
    }
    let mut nodes = HashMap::new();
    let mut points_declared = None;
    let mut unstructured_cells: Option<Vec<Vec<usize>>> = None;
    let mut unstructured_cell_types: Option<Vec<usize>> = None;
    let mut polydata = VtkLegacyPolyData::default();
    let mut point_data_count = None;
    let mut cell_data_count = None;
    let mut data_association = None;
    let mut point_scalars = None;
    let mut cell_scalars = None;
    let mut warnings = Vec::new();
    let mut warned_extra_scalars = false;
    let mut warned_non_scalar = false;
    let mut index = 4usize;

    while index < lines.len() {
        let line = lines[index];
        if line.is_empty() {
            index += 1;
            continue;
        }
        let fields = line.split_whitespace().collect::<Vec<_>>();
        match fields.first().copied().unwrap_or_default() {
            "POINTS" => {
                if points_declared.is_some() || fields.len() != 3 {
                    return Err(Error::InvalidInput(
                        "invalid or duplicate VTK POINTS header".into(),
                    ));
                }
                let count = parse_vtk_legacy_count(fields[1], "point")?;
                if count > MAX_SIMULATION_POINTS {
                    return Err(Error::LimitExceeded(format!(
                        "VTK legacy point count exceeds {MAX_SIMULATION_POINTS}"
                    )));
                }
                if !matches!(fields[2].to_ascii_lowercase().as_str(), "float" | "double") {
                    return Err(Error::Unsupported(format!(
                        "VTK legacy point type '{}' is unsupported",
                        fields[2]
                    )));
                }
                let value_count = count.checked_mul(3).ok_or_else(|| {
                    Error::LimitExceeded("VTK legacy coordinate count overflowed".into())
                })?;
                check_vtk_legacy_value_count(value_count, "point coordinates")?;
                index += 1;
                let values = read_vtk_legacy_f64_values(
                    &lines,
                    &mut index,
                    value_count,
                    "point coordinates",
                )?;
                for (point_id, xyz) in values.chunks_exact(3).enumerate() {
                    if xyz.iter().any(|value| value.abs() > 1.0e12) {
                        return Err(Error::InvalidInput(
                            "VTK legacy point coordinate is outside the supported range".into(),
                        ));
                    }
                    nodes.insert(
                        point_id,
                        MeshNode {
                            x: xyz[0],
                            y: xyz[1],
                            scalar: None,
                        },
                    );
                }
                points_declared = Some(count);
            }
            "CELLS" => {
                if dataset != "UNSTRUCTURED_GRID"
                    || unstructured_cells.is_some()
                    || fields.len() != 3
                {
                    return Err(Error::InvalidInput(
                        "invalid or duplicate VTK CELLS header".into(),
                    ));
                }
                let count = parse_vtk_legacy_count(fields[1], "cell")?;
                let size = parse_vtk_legacy_count(fields[2], "connectivity value")?;
                index += 1;
                unstructured_cells = Some(read_vtk_legacy_cells(
                    &lines, &mut index, count, size, "CELLS",
                )?);
            }
            "CELL_TYPES" => {
                if dataset != "UNSTRUCTURED_GRID"
                    || unstructured_cell_types.is_some()
                    || fields.len() != 2
                {
                    return Err(Error::InvalidInput(
                        "invalid or duplicate VTK CELL_TYPES header".into(),
                    ));
                }
                let count = parse_vtk_legacy_count(fields[1], "cell type")?;
                index += 1;
                unstructured_cell_types = Some(read_vtk_legacy_usize_values(
                    &lines,
                    &mut index,
                    count,
                    "CELL_TYPES",
                )?);
            }
            "VERTICES" | "LINES" | "POLYGONS" | "TRIANGLE_STRIPS" => {
                if dataset != "POLYDATA" || fields.len() != 3 {
                    return Err(Error::InvalidInput(format!(
                        "invalid VTK {} header for dataset {dataset}",
                        fields[0]
                    )));
                }
                let count = parse_vtk_legacy_count(fields[1], "polydata cell")?;
                let size = parse_vtk_legacy_count(fields[2], "polydata connectivity value")?;
                index += 1;
                let records = read_vtk_legacy_cells(&lines, &mut index, count, size, fields[0])?;
                let target = match fields[0] {
                    "VERTICES" => &mut polydata.vertices,
                    "LINES" => &mut polydata.lines,
                    "POLYGONS" => &mut polydata.polygons,
                    _ => &mut polydata.triangle_strips,
                };
                if !target.is_empty() {
                    return Err(Error::InvalidInput(format!(
                        "duplicate VTK {} section",
                        fields[0]
                    )));
                }
                *target = records;
            }
            "POINT_DATA" | "CELL_DATA" => {
                if fields.len() != 2 {
                    return Err(Error::InvalidInput(format!(
                        "invalid VTK {} header",
                        fields[0]
                    )));
                }
                let count = parse_vtk_legacy_count(fields[1], "data item")?;
                if count > MAX_SIMULATION_POINTS.max(MAX_SIMULATION_CELLS) {
                    return Err(Error::LimitExceeded(
                        "VTK legacy data item count exceeds the mesh limit".into(),
                    ));
                }
                if fields[0] == "POINT_DATA" {
                    if point_data_count.replace(count).is_some() {
                        return Err(Error::InvalidInput(
                            "duplicate VTK POINT_DATA section".into(),
                        ));
                    }
                    data_association = Some(VtkLegacyAssociation::Points);
                } else {
                    if cell_data_count.replace(count).is_some() {
                        return Err(Error::InvalidInput(
                            "duplicate VTK CELL_DATA section".into(),
                        ));
                    }
                    data_association = Some(VtkLegacyAssociation::Cells);
                }
                index += 1;
            }
            "SCALARS" => {
                if fields.len() < 3 || fields.len() > 4 {
                    return Err(Error::InvalidInput(
                        "invalid VTK legacy SCALARS header".into(),
                    ));
                }
                if !is_vtk_legacy_numeric_type(fields[2]) {
                    return Err(Error::Unsupported(format!(
                        "VTK legacy scalar type '{}' is unsupported",
                        fields[2]
                    )));
                }
                let association = data_association.ok_or_else(|| {
                    Error::InvalidInput(
                        "VTK legacy SCALARS must follow POINT_DATA or CELL_DATA".into(),
                    )
                })?;
                let item_count = match association {
                    VtkLegacyAssociation::Points => point_data_count,
                    VtkLegacyAssociation::Cells => cell_data_count,
                }
                .ok_or_else(|| {
                    Error::InvalidInput("VTK legacy scalar data has no item count".into())
                })?;
                let components = fields
                    .get(3)
                    .map(|value| parse_vtk_legacy_count(value, "scalar component"))
                    .transpose()?
                    .unwrap_or(1);
                if components == 0 || components > 64 {
                    return Err(Error::LimitExceeded(
                        "VTK legacy scalar component count must be between 1 and 64".into(),
                    ));
                }
                let value_count = item_count.checked_mul(components).ok_or_else(|| {
                    Error::LimitExceeded("VTK legacy scalar value count overflowed".into())
                })?;
                check_vtk_legacy_value_count(value_count, "scalar array")?;
                index += 1;
                while index < lines.len() && lines[index].is_empty() {
                    index += 1;
                }
                if index >= lines.len() || !lines[index].starts_with("LOOKUP_TABLE") {
                    return Err(Error::InvalidInput(
                        "VTK legacy SCALARS is missing LOOKUP_TABLE".into(),
                    ));
                }
                index += 1;
                let values =
                    read_vtk_legacy_f64_values(&lines, &mut index, value_count, "SCALARS values")?;
                let first_array = match association {
                    VtkLegacyAssociation::Points => point_scalars.is_none(),
                    VtkLegacyAssociation::Cells => cell_scalars.is_none(),
                };
                if !first_array {
                    if !warned_extra_scalars {
                        warnings.push(
                            "additional VTK legacy scalar arrays were ignored; only the first per association is rendered".into(),
                        );
                        warned_extra_scalars = true;
                    }
                    continue;
                }
                let mut scalar_values = Vec::with_capacity(item_count);
                for tuple in values.chunks_exact(components) {
                    let magnitude = tuple.iter().copied().fold(0.0f64, f64::hypot);
                    if !magnitude.is_finite() {
                        return Err(Error::InvalidInput(
                            "VTK legacy scalar tuple magnitude overflows".into(),
                        ));
                    }
                    scalar_values.push(if components == 1 { tuple[0] } else { magnitude });
                }
                if components > 1 {
                    warnings.push(format!(
                        "VTK legacy {:?} scalar array '{}' has {components} components; preview colors use Euclidean magnitude",
                        association,
                        fields[1]
                    ));
                }
                match association {
                    VtkLegacyAssociation::Points => point_scalars = Some(scalar_values),
                    VtkLegacyAssociation::Cells => cell_scalars = Some(scalar_values),
                }
            }
            "VECTORS"
            | "NORMALS"
            | "TENSORS"
            | "COLOR_SCALARS"
            | "TEXTURE_COORDINATES"
            | "FIELD" => {
                if !warned_non_scalar {
                    warnings.push(
                        "non-scalar VTK legacy attribute arrays are omitted from this preview"
                            .into(),
                    );
                    warned_non_scalar = true;
                }
                index += 1;
            }
            _ => index += 1,
        }
    }

    let declared_points = points_declared
        .ok_or_else(|| Error::InvalidInput("VTK legacy dataset is missing POINTS".into()))?;
    if nodes.len() != declared_points {
        return Err(Error::InvalidInput(
            "VTK legacy point array does not match its declaration".into(),
        ));
    }
    if point_data_count.is_some_and(|count| count != declared_points) {
        return Err(Error::InvalidInput(
            "VTK legacy POINT_DATA count does not match POINTS".into(),
        ));
    }
    if let Some(values) = point_scalars {
        if values.len() != declared_points {
            return Err(Error::InvalidInput(
                "VTK legacy point scalar count does not match POINT_DATA".into(),
            ));
        }
        for (point_id, value) in values.into_iter().enumerate() {
            if let Some(node) = nodes.get_mut(&point_id) {
                node.scalar = Some(value);
            }
        }
    }

    let raw_cells = if dataset == "UNSTRUCTURED_GRID" {
        let raw = unstructured_cells.unwrap_or_default();
        let cell_types = unstructured_cell_types.unwrap_or_default();
        let only_empty_legacy_cells = cell_types.is_empty() && raw.iter().all(Vec::is_empty);
        if cell_types.len() != raw.len() && !only_empty_legacy_cells {
            return Err(Error::InvalidInput(
                "VTK legacy CELL_TYPES count does not match CELLS".into(),
            ));
        }
        raw.into_iter()
            .enumerate()
            .map(|(index, ids)| (ids, cell_types.get(index).copied().unwrap_or(0)))
            .collect::<Vec<_>>()
    } else {
        if unstructured_cells.is_some() || unstructured_cell_types.is_some() {
            return Err(Error::InvalidInput(
                "VTK legacy POLYDATA cannot contain CELLS or CELL_TYPES".into(),
            ));
        }
        let mut raw = Vec::new();
        raw.extend(polydata.vertices.into_iter().map(|ids| (ids, 1usize)));
        raw.extend(polydata.lines.into_iter().map(|ids| {
            let cell_type = if ids.len() == 2 { 3 } else { 4 };
            (ids, cell_type)
        }));
        raw.extend(polydata.polygons.into_iter().map(|ids| (ids, 7usize)));
        raw.extend(
            polydata
                .triangle_strips
                .into_iter()
                .map(|ids| (ids, 6usize)),
        );
        raw
    };
    if raw_cells.len() > MAX_SIMULATION_CELLS {
        return Err(Error::LimitExceeded(format!(
            "VTK legacy cell count exceeds {MAX_SIMULATION_CELLS}"
        )));
    }
    if cell_data_count.is_some_and(|count| count != raw_cells.len()) {
        return Err(Error::InvalidInput(
            "VTK legacy CELL_DATA count does not match its cells".into(),
        ));
    }
    if cell_scalars
        .as_ref()
        .is_some_and(|values| values.len() != raw_cells.len())
    {
        return Err(Error::InvalidInput(
            "VTK legacy cell scalar count does not match CELL_DATA".into(),
        ));
    }

    let mut cells = Vec::new();
    for (index, (ids, cell_type)) in raw_cells.into_iter().enumerate() {
        if ids.iter().any(|node_id| *node_id >= nodes.len()) {
            return Err(Error::InvalidInput(format!(
                "VTK legacy cell {index} references a point outside POINTS"
            )));
        }
        let scalar = cell_scalars
            .as_ref()
            .and_then(|values| values.get(index).copied());
        for primitive in vtk_legacy_cell_primitives(cell_type, &ids, scalar)? {
            if cells.len() >= MAX_VTK_RENDERED_PRIMITIVES {
                return Err(Error::LimitExceeded(format!(
                    "VTK legacy rendered primitive count exceeds {MAX_VTK_RENDERED_PRIMITIVES}"
                )));
            }
            cells.push(primitive);
        }
    }
    Ok((nodes, cells, title, warnings))
}

fn parse_vtk_legacy_count(value: &str, context: &str) -> Result<usize> {
    value
        .parse::<usize>()
        .map_err(|_| Error::InvalidInput(format!("invalid VTK legacy {context} count '{value}'")))
}

fn parse_vtk_legacy_binary_type(name: &str) -> Result<VtkScalarType> {
    match name.to_ascii_lowercase().as_str() {
        "char" => Ok(VtkScalarType::I8),
        "unsigned_char" => Ok(VtkScalarType::U8),
        "short" => Ok(VtkScalarType::I16),
        "unsigned_short" => Ok(VtkScalarType::U16),
        "int" => Ok(VtkScalarType::I32),
        "unsigned_int" => Ok(VtkScalarType::U32),
        "float" => Ok(VtkScalarType::F32),
        "double" => Ok(VtkScalarType::F64),
        "bit" | "long" | "unsigned_long" | "long_long" | "unsigned_long_long" | "vtkidtype" => {
            Err(Error::Unsupported(format!(
                "VTK legacy binary data type '{name}' has packed or platform-dependent width; use a fixed-width numeric type"
            )))
        }
        other => Err(Error::Unsupported(format!(
            "VTK legacy binary data type '{other}' is unsupported"
        ))),
    }
}

fn add_vtk_legacy_binary_values(total: &mut usize, count: usize, context: &str) -> Result<()> {
    *total = total.checked_add(count).ok_or_else(|| {
        Error::LimitExceeded(format!("VTK legacy {context} value count overflowed"))
    })?;
    if *total > MAX_VTK_TOTAL_VALUES {
        return Err(Error::LimitExceeded(format!(
            "VTK legacy arrays exceed {MAX_VTK_TOTAL_VALUES} total values"
        )));
    }
    Ok(())
}

fn check_vtk_legacy_value_count(count: usize, context: &str) -> Result<()> {
    if count > MAX_VTK_TOTAL_VALUES {
        return Err(Error::LimitExceeded(format!(
            "VTK legacy {context} exceeds {MAX_VTK_TOTAL_VALUES} values"
        )));
    }
    Ok(())
}

fn read_vtk_legacy_f64_values(
    lines: &[&str],
    index: &mut usize,
    count: usize,
    context: &str,
) -> Result<Vec<f64>> {
    check_vtk_legacy_value_count(count, context)?;
    let mut values = Vec::with_capacity(count);
    while values.len() < count {
        let line = lines
            .get(*index)
            .ok_or_else(|| Error::InvalidInput(format!("VTK legacy {context} is truncated")))?;
        for token in line.split_whitespace() {
            if values.len() >= count {
                return Err(Error::InvalidInput(format!(
                    "VTK legacy {context} contains too many values"
                )));
            }
            let value = token.parse::<f64>().map_err(|_| {
                Error::InvalidInput(format!("invalid VTK legacy {context} value '{token}'"))
            })?;
            if !value.is_finite() {
                return Err(Error::InvalidInput(format!(
                    "VTK legacy {context} contains a non-finite value"
                )));
            }
            values.push(value);
        }
        *index += 1;
    }
    Ok(values)
}

fn read_vtk_legacy_usize_values(
    lines: &[&str],
    index: &mut usize,
    count: usize,
    context: &str,
) -> Result<Vec<usize>> {
    check_vtk_legacy_value_count(count, context)?;
    let mut values = Vec::with_capacity(count);
    while values.len() < count {
        let line = lines
            .get(*index)
            .ok_or_else(|| Error::InvalidInput(format!("VTK legacy {context} is truncated")))?;
        for token in line.split_whitespace() {
            if values.len() >= count {
                return Err(Error::InvalidInput(format!(
                    "VTK legacy {context} contains too many values"
                )));
            }
            let value = token.parse::<usize>().map_err(|_| {
                Error::InvalidInput(format!("invalid VTK legacy {context} value '{token}'"))
            })?;
            values.push(value);
        }
        *index += 1;
    }
    Ok(values)
}

fn read_vtk_legacy_cells(
    lines: &[&str],
    index: &mut usize,
    cell_count: usize,
    encoded_size: usize,
    context: &str,
) -> Result<Vec<Vec<usize>>> {
    if cell_count > MAX_SIMULATION_CELLS {
        return Err(Error::LimitExceeded(format!(
            "VTK legacy {context} count exceeds {MAX_SIMULATION_CELLS}"
        )));
    }
    let encoded = read_vtk_legacy_usize_values(lines, index, encoded_size, context)?;
    let mut cursor = 0usize;
    let mut cells = Vec::with_capacity(cell_count);
    for _ in 0..cell_count {
        let point_count = *encoded.get(cursor).ok_or_else(|| {
            Error::InvalidInput(format!("VTK legacy {context} has too few cell records"))
        })?;
        cursor += 1;
        if point_count > MAX_VTK_ARRAY_VALUES {
            return Err(Error::LimitExceeded(format!(
                "VTK legacy {context} cell exceeds {MAX_VTK_ARRAY_VALUES} points"
            )));
        }
        let end = cursor
            .checked_add(point_count)
            .filter(|end| *end <= encoded.len())
            .ok_or_else(|| {
                Error::InvalidInput(format!("VTK legacy {context} cell indices are truncated"))
            })?;
        cells.push(encoded[cursor..end].to_vec());
        cursor = end;
    }
    if cursor != encoded.len() {
        return Err(Error::InvalidInput(format!(
            "VTK legacy {context} contains trailing connectivity values"
        )));
    }
    Ok(cells)
}

fn is_vtk_legacy_numeric_type(value: &str) -> bool {
    matches!(
        value.to_ascii_lowercase().as_str(),
        "bit"
            | "char"
            | "unsigned_char"
            | "short"
            | "unsigned_short"
            | "int"
            | "unsigned_int"
            | "long"
            | "unsigned_long"
            | "long_long"
            | "unsigned_long_long"
            | "float"
            | "double"
            | "vtkidtype"
    )
}

fn vtk_legacy_cell_primitives(
    cell_type: usize,
    ids: &[usize],
    scalar: Option<f64>,
) -> Result<Vec<MeshCell>> {
    if cell_type == 4 && ids.len() >= 2 {
        return Ok(ids
            .windows(2)
            .map(|edge| MeshCell {
                node_ids: edge.to_vec(),
                scalar,
            })
            .collect());
    }
    if cell_type == 6 && ids.len() >= 3 {
        return (0..ids.len() - 2)
            .map(|triangle_index| {
                let triangle = if triangle_index % 2 == 0 {
                    [
                        ids[triangle_index],
                        ids[triangle_index + 1],
                        ids[triangle_index + 2],
                    ]
                } else {
                    [
                        ids[triangle_index + 1],
                        ids[triangle_index],
                        ids[triangle_index + 2],
                    ]
                };
                Ok(MeshCell {
                    node_ids: triangle.to_vec(),
                    scalar,
                })
            })
            .collect();
    }
    vtk_cell_primitives(cell_type, ids, scalar)
}

fn parse_vtk_xml(text: &str, max_xml_events: usize) -> Result<SimulationParseOutput> {
    parse_vtk_xml_with_appended(text, max_xml_events, None)
}

fn prepare_vtk_xml_raw_appended(
    bytes: &[u8],
    max_xml_events: usize,
) -> Result<Option<(String, Range<usize>)>> {
    if !bytes
        .windows(b"<VTKFile".len())
        .any(|window| window == b"<VTKFile")
        || !bytes
            .windows(b"<AppendedData".len())
            .any(|window| window == b"<AppendedData")
    {
        return Ok(None);
    }
    let mut reader = Reader::from_reader(bytes);
    reader.config_mut().trim_text(false);
    let mut buffer = Vec::new();
    let mut event_count = 0usize;
    let payload_offset = loop {
        event_count = event_count.saturating_add(1);
        if event_count > max_xml_events {
            return Err(Error::LimitExceeded(format!(
                "VTK XML exceeds {max_xml_events} parser events"
            )));
        }
        match reader.read_event_into(&mut buffer)? {
            Event::Start(element) if local_name(element.name().as_ref()) == b"AppendedData" => {
                let encoding = attribute(&element, b"encoding").unwrap_or_default();
                if !encoding.eq_ignore_ascii_case("raw") {
                    return Ok(None);
                }
                break usize::try_from(reader.buffer_position()).map_err(|_| {
                    Error::LimitExceeded("VTK XML AppendedData position exceeds usize".into())
                })?;
            }
            Event::Eof => return Ok(None),
            _ => {}
        }
        buffer.clear();
    };

    let mut payload_start = payload_offset;
    while bytes
        .get(payload_start)
        .is_some_and(u8::is_ascii_whitespace)
    {
        payload_start += 1;
    }
    if bytes.get(payload_start) != Some(&b'_') {
        return Err(Error::InvalidInput(
            "raw VTK XML AppendedData is missing its required '_' prefix".into(),
        ));
    }
    payload_start += 1;

    let root_close = b"</VTKFile>";
    let root_close_start = bytes
        .windows(root_close.len())
        .rposition(|window| window == root_close)
        .ok_or_else(|| Error::InvalidInput("VTK XML is missing its closing VTKFile tag".into()))?;
    let appended_close = b"</AppendedData>";
    let appended_close_start = bytes[..root_close_start]
        .windows(appended_close.len())
        .rposition(|window| window == appended_close)
        .ok_or_else(|| {
            Error::InvalidInput("raw VTK XML AppendedData is missing its closing tag".into())
        })?;
    if appended_close_start < payload_start {
        return Err(Error::InvalidInput(
            "raw VTK XML AppendedData closing tag precedes its payload".into(),
        ));
    }
    let appended_length = appended_close_start - payload_start;
    if appended_length > MAX_VTK_APPENDED_BINARY_BYTES {
        return Err(Error::LimitExceeded(format!(
            "VTK XML raw appended data exceeds {MAX_VTK_APPENDED_BINARY_BYTES} bytes"
        )));
    }

    // Raw AppendedData is intentionally outside XML's character model. Preserve its
    // underscore sentinel and the surrounding markup, then parse the binary slice
    // separately. The last closing tags are the XML suffix because the format places
    // AppendedData after the dataset and before VTKFile's close.
    let mut sanitized = Vec::with_capacity(bytes.len() - appended_length);
    sanitized.extend_from_slice(&bytes[..payload_start]);
    sanitized.extend_from_slice(&bytes[appended_close_start..]);
    let xml = String::from_utf8(sanitized).map_err(|error| {
        Error::InvalidInput(format!(
            "VTK XML metadata around raw appended data is not UTF-8: {error}"
        ))
    })?;
    Ok(Some((xml, payload_start..appended_close_start)))
}

fn parse_vtk_xml_with_appended(
    text: &str,
    max_xml_events: usize,
    raw_appended_data: Option<&[u8]>,
) -> Result<SimulationParseOutput> {
    let mut reader = Reader::from_str(text);
    reader.config_mut().trim_text(false);
    let mut buffer = Vec::new();
    let mut stack: Vec<String> = Vec::new();
    let mut current_piece: Option<VtkXmlPiece> = None;
    let mut current_array: Option<VtkXmlArray> = None;
    let mut root_type = None;
    let mut image_geometry = None;
    let mut structured_whole_extent = None;
    let mut byte_order = VtkByteOrder::Little;
    let mut header_type = VtkHeaderType::UInt32;
    let mut compressor = None;
    let mut appended_encoding = None;
    let mut appended_text = String::new();
    let mut pieces = Vec::new();
    let mut nodes = HashMap::new();
    let mut cells = Vec::new();
    let mut total_values = 0usize;
    let mut event_count = 0usize;
    let mut warnings = Vec::new();

    loop {
        event_count = event_count.saturating_add(1);
        if event_count > max_xml_events {
            return Err(Error::LimitExceeded(format!(
                "VTK XML exceeds {max_xml_events} parser events"
            )));
        }

        match reader.read_event_into(&mut buffer)? {
            Event::Start(ref element) => {
                let name =
                    String::from_utf8_lossy(local_name(element.name().as_ref())).into_owned();
                match name.as_str() {
                    "VTKFile" => {
                        root_type = attribute(element, b"type");
                        byte_order = parse_vtk_byte_order(attribute(element, b"byte_order"))?;
                        header_type = parse_vtk_header_type(attribute(element, b"header_type"))?;
                        compressor = attribute(element, b"compressor");
                    }
                    "ImageData" => {
                        if image_geometry.is_some() {
                            return Err(Error::InvalidInput(
                                "VTK XML has multiple ImageData elements".into(),
                            ));
                        }
                        image_geometry = Some(parse_vtk_image_geometry(element)?);
                    }
                    "RectilinearGrid" | "StructuredGrid" => {
                        if structured_whole_extent.is_some() {
                            return Err(Error::InvalidInput(
                                "VTK XML has multiple structured-grid elements".into(),
                            ));
                        }
                        structured_whole_extent = attribute(element, b"WholeExtent")
                            .map(|value| parse_vtk_extent(&value, "WholeExtent"))
                            .transpose()?;
                    }
                    "Piece" => {
                        if current_piece.is_some() {
                            return Err(Error::InvalidInput(
                                "nested VTK XML Piece elements are invalid".into(),
                            ));
                        }
                        current_piece = Some(VtkXmlPiece {
                            declared_points: parse_vtk_optional_count(element, b"NumberOfPoints")?,
                            declared_cells: parse_vtk_optional_count(element, b"NumberOfCells")?,
                            structured_extent: attribute(element, b"Extent")
                                .map(|value| parse_vtk_extent(&value, "Piece Extent"))
                                .transpose()?,
                            declared_poly_cells: [
                                "NumberOfVerts",
                                "NumberOfLines",
                                "NumberOfStrips",
                                "NumberOfPolys",
                            ]
                            .into_iter()
                            .filter_map(|key| {
                                attribute(element, key.as_bytes())
                                    .map(|value| (key.to_owned(), value))
                            })
                            .map(|(key, value)| {
                                let count = value.parse::<usize>().map_err(|_| {
                                    Error::InvalidInput(format!("invalid VTK XML {key} count"))
                                })?;
                                Ok((key, count))
                            })
                            .collect::<Result<HashMap<_, _>>>()?,
                            ..VtkXmlPiece::default()
                        });
                    }
                    "PointData" => {
                        let piece = current_piece.as_mut().ok_or_else(|| {
                            Error::InvalidInput("VTK XML PointData is outside a Piece".into())
                        })?;
                        piece.active_point_scalar = attribute(element, b"Scalars");
                    }
                    "CellData" => {
                        let piece = current_piece.as_mut().ok_or_else(|| {
                            Error::InvalidInput("VTK XML CellData is outside a Piece".into())
                        })?;
                        piece.active_cell_scalar = attribute(element, b"Scalars");
                    }
                    "AppendedData" => {
                        let encoding = attribute(element, b"encoding").unwrap_or_default();
                        if !(encoding.eq_ignore_ascii_case("base64")
                            || (encoding.eq_ignore_ascii_case("raw")
                                && raw_appended_data.is_some()))
                        {
                            return Err(Error::Unsupported(format!(
                                "VTK XML AppendedData encoding '{encoding}' is unsupported; use base64 or raw binary input"
                            )));
                        }
                        if appended_encoding.replace(encoding).is_some() {
                            return Err(Error::InvalidInput(
                                "VTK XML has multiple AppendedData sections".into(),
                            ));
                        }
                    }
                    "DataArray" => {
                        if current_array.is_some() {
                            return Err(Error::InvalidInput(
                                "nested VTK XML DataArray elements are invalid".into(),
                            ));
                        }
                        if current_piece.is_none() {
                            return Err(Error::InvalidInput(
                                "VTK XML DataArray is outside a Piece".into(),
                            ));
                        }
                        current_array = Some(make_vtk_xml_array(
                            element,
                            stack.last().map(String::as_str).unwrap_or_default(),
                        )?);
                    }
                    _ => {}
                }
                stack.push(name);
            }
            Event::Empty(ref element) => {
                let qualified_name = element.name();
                let name = local_name(qualified_name.as_ref());
                if name == b"DataArray" {
                    let array = make_vtk_xml_array(
                        element,
                        stack.last().map(String::as_str).unwrap_or_default(),
                    )?;
                    total_values = total_values
                        .checked_add(finish_vtk_xml_array(
                            current_piece.as_mut().ok_or_else(|| {
                                Error::InvalidInput("VTK XML DataArray is outside a Piece".into())
                            })?,
                            array,
                            byte_order,
                            header_type,
                            compressor.as_deref(),
                        )?)
                        .ok_or_else(|| {
                            Error::LimitExceeded("VTK XML data value count overflowed".into())
                        })?;
                    check_vtk_xml_value_budget(total_values)?;
                }
            }
            Event::Text(ref value) => {
                if stack.last().is_some_and(|name| name == "AppendedData") {
                    let decoded = value.decode().map_err(|error| {
                        Error::InvalidInput(format!("invalid VTK XML appended data: {error}"))
                    })?;
                    if appended_text.len().saturating_add(decoded.len())
                        > MAX_VTK_APPENDED_TEXT_BYTES
                    {
                        return Err(Error::LimitExceeded(format!(
                            "VTK XML appended data exceeds {MAX_VTK_APPENDED_TEXT_BYTES} encoded bytes"
                        )));
                    }
                    appended_text.push_str(&decoded);
                } else if let Some(array) = current_array.as_mut()
                    && array.format != VtkXmlArrayFormat::Appended
                    && is_vtk_data_context(&array.context)
                {
                    let decoded = value.decode().map_err(|error| {
                        Error::InvalidInput(format!("invalid VTK XML DataArray text: {error}"))
                    })?;
                    if array.text.len().saturating_add(decoded.len()) > MAX_VTK_ARRAY_TEXT_BYTES {
                        return Err(Error::LimitExceeded(format!(
                            "VTK XML DataArray exceeds {MAX_VTK_ARRAY_TEXT_BYTES} text bytes"
                        )));
                    }
                    array.text.push_str(&decoded);
                }
            }
            Event::CData(ref value) => {
                if stack.last().is_some_and(|name| name == "AppendedData") {
                    return Err(Error::Unsupported(
                        "CDATA sections are not supported in VTK XML AppendedData; use text base64 or raw byte-oriented input".into(),
                    ));
                } else if let Some(array) = current_array.as_mut()
                    && array.format != VtkXmlArrayFormat::Appended
                    && is_vtk_data_context(&array.context)
                {
                    let decoded = value.decode().map_err(|error| {
                        Error::InvalidInput(format!("invalid VTK XML DataArray text: {error}"))
                    })?;
                    if array.text.len().saturating_add(decoded.len()) > MAX_VTK_ARRAY_TEXT_BYTES {
                        return Err(Error::LimitExceeded(format!(
                            "VTK XML DataArray exceeds {MAX_VTK_ARRAY_TEXT_BYTES} text bytes"
                        )));
                    }
                    array.text.push_str(&decoded);
                }
            }
            Event::GeneralRef(_) => {
                if current_array.is_some() {
                    return Err(Error::InvalidInput(
                        "XML entities are not valid in VTK numeric arrays".into(),
                    ));
                }
            }
            Event::DocType(_) => {
                return Err(Error::InvalidInput(
                    "VTK XML document type declarations are not supported".into(),
                ));
            }
            Event::End(ref element) => {
                let name =
                    String::from_utf8_lossy(local_name(element.name().as_ref())).into_owned();
                if name == "DataArray" {
                    let array = current_array.take().ok_or_else(|| {
                        Error::InvalidInput("VTK XML DataArray close has no opening tag".into())
                    })?;
                    total_values = total_values
                        .checked_add(finish_vtk_xml_array(
                            current_piece.as_mut().ok_or_else(|| {
                                Error::InvalidInput("VTK XML DataArray is outside a Piece".into())
                            })?,
                            array,
                            byte_order,
                            header_type,
                            compressor.as_deref(),
                        )?)
                        .ok_or_else(|| {
                            Error::LimitExceeded("VTK XML data value count overflowed".into())
                        })?;
                    check_vtk_xml_value_budget(total_values)?;
                } else if name == "Piece" {
                    let piece = current_piece.take().ok_or_else(|| {
                        Error::InvalidInput("VTK XML Piece close has no opening tag".into())
                    })?;
                    pieces.push(piece);
                }
                if stack.pop().as_deref() != Some(name.as_str()) {
                    return Err(Error::InvalidInput(format!(
                        "mismatched VTK XML end tag '{name}'"
                    )));
                }
            }
            Event::Eof => break,
            _ => {}
        }
        buffer.clear();
    }

    if current_piece.is_some() || current_array.is_some() {
        return Err(Error::InvalidInput("incomplete VTK XML document".into()));
    }
    let root_type = root_type
        .ok_or_else(|| Error::InvalidInput("VTK XML file is missing a VTKFile root".into()))?;
    if !matches!(
        root_type.as_str(),
        "UnstructuredGrid" | "PolyData" | "ImageData" | "RectilinearGrid" | "StructuredGrid"
    ) {
        return Err(Error::Unsupported(format!(
            "VTK XML dataset type '{root_type}' is unsupported; use UnstructuredGrid, PolyData, ImageData, RectilinearGrid, or StructuredGrid"
        )));
    }
    match root_type.as_str() {
        "ImageData" if image_geometry.is_none() => {
            return Err(Error::InvalidInput(
                "VTK XML ImageData is missing its ImageData element".into(),
            ));
        }
        "RectilinearGrid" | "StructuredGrid" if structured_whole_extent.is_none() => {
            return Err(Error::InvalidInput(format!(
                "VTK XML {root_type} is missing WholeExtent"
            )));
        }
        _ => {}
    }
    let appended_bytes = if let Some(raw) = raw_appended_data {
        if !appended_encoding
            .as_deref()
            .is_some_and(|encoding| encoding.eq_ignore_ascii_case("raw"))
        {
            return Err(Error::InvalidInput(
                "raw VTK XML payload has no raw AppendedData section".into(),
            ));
        }
        if appended_text.trim() != "_" {
            return Err(Error::InvalidInput(
                "raw VTK XML AppendedData contains unexpected XML text around its payload".into(),
            ));
        }
        Some(Cow::Borrowed(raw))
    } else if appended_encoding
        .as_deref()
        .is_some_and(|encoding| encoding.eq_ignore_ascii_case("raw"))
    {
        return Err(Error::Unsupported(
            "raw VTK XML AppendedData requires byte-oriented input".into(),
        ));
    } else if appended_text.trim().is_empty() {
        None
    } else {
        if !appended_encoding
            .as_deref()
            .is_some_and(|encoding| encoding.eq_ignore_ascii_case("base64"))
        {
            return Err(Error::InvalidInput(
                "VTK XML appended arrays are missing a base64 AppendedData section".into(),
            ));
        }
        Some(Cow::Owned(decode_vtk_base64(
            &appended_text,
            "AppendedData",
        )?))
    };
    for mut piece in pieces {
        for array in std::mem::take(&mut piece.pending_appended_arrays) {
            let offset = array.offset.ok_or_else(|| {
                Error::InvalidInput("VTK XML appended DataArray is missing its offset".into())
            })?;
            let bytes = appended_bytes.as_deref().ok_or_else(|| {
                Error::InvalidInput("VTK XML appended DataArray has no appended payload".into())
            })?;
            let payload = bytes.get(offset..).ok_or_else(|| {
                Error::InvalidInput("VTK XML DataArray offset exceeds appended payload".into())
            })?;
            let (decoded, consumed) =
                decode_vtk_binary_payload(payload, header_type, byte_order, compressor.as_deref())?;
            if consumed == 0 {
                return Err(Error::InvalidInput(
                    "VTK XML appended DataArray has an empty header".into(),
                ));
            }
            total_values = total_values
                .checked_add(assign_vtk_xml_array(
                    &mut piece,
                    array,
                    Some(&decoded),
                    byte_order,
                )?)
                .ok_or_else(|| {
                    Error::LimitExceeded("VTK XML data value count overflowed".into())
                })?;
            check_vtk_xml_value_budget(total_values)?;
        }
        warnings.extend(std::mem::take(&mut piece.warnings));
        append_vtk_xml_piece(
            piece,
            &root_type,
            image_geometry.as_ref(),
            structured_whole_extent,
            &mut nodes,
            &mut cells,
        )?;
    }
    if nodes.is_empty() {
        return Err(Error::InvalidInput("VTK XML dataset has no points".into()));
    }
    Ok((nodes, cells, format!("VTK XML {root_type}"), warnings))
}

fn is_vtk_data_context(context: &str) -> bool {
    matches!(
        context,
        "Points"
            | "Coordinates"
            | "Cells"
            | "PointData"
            | "CellData"
            | "Verts"
            | "Lines"
            | "Strips"
            | "Polys"
    )
}

fn parse_vtk_byte_order(value: Option<String>) -> Result<VtkByteOrder> {
    match value.as_deref().unwrap_or("LittleEndian") {
        "LittleEndian" => Ok(VtkByteOrder::Little),
        "BigEndian" => Ok(VtkByteOrder::Big),
        other => Err(Error::Unsupported(format!(
            "VTK XML byte order '{other}' is unsupported"
        ))),
    }
}

fn parse_vtk_header_type(value: Option<String>) -> Result<VtkHeaderType> {
    match value.as_deref().unwrap_or("UInt32") {
        "UInt32" => Ok(VtkHeaderType::UInt32),
        "UInt64" => Ok(VtkHeaderType::UInt64),
        other => Err(Error::Unsupported(format!(
            "VTK XML header type '{other}' is unsupported"
        ))),
    }
}

fn make_vtk_xml_array(element: &BytesStart<'_>, context: &str) -> Result<VtkXmlArray> {
    let format = match attribute(element, b"format").as_deref() {
        Some(format) if format.eq_ignore_ascii_case("ascii") => VtkXmlArrayFormat::Ascii,
        Some(format) if format.eq_ignore_ascii_case("binary") => VtkXmlArrayFormat::InlineBinary,
        Some(format) if format.eq_ignore_ascii_case("appended") => VtkXmlArrayFormat::Appended,
        Some(format) => {
            return Err(Error::Unsupported(format!(
                "VTK XML DataArray format '{format}' is unsupported"
            )));
        }
        None => {
            return Err(Error::InvalidInput(
                "VTK XML DataArray is missing its format attribute".into(),
            ));
        }
    };
    let type_name = attribute(element, b"type")
        .ok_or_else(|| Error::InvalidInput("VTK XML DataArray is missing its type".into()))?;
    let scalar_type = VtkScalarType::parse(&type_name).ok_or_else(|| {
        Error::Unsupported(format!(
            "VTK XML DataArray type '{type_name}' is unsupported"
        ))
    })?;
    let components = attribute(element, b"NumberOfComponents")
        .map(|value| {
            value
                .parse::<usize>()
                .map_err(|_| Error::InvalidInput("invalid VTK XML NumberOfComponents".into()))
        })
        .transpose()?
        .unwrap_or(1);
    if components == 0 || components > 64 {
        return Err(Error::LimitExceeded(
            "VTK XML NumberOfComponents must be between 1 and 64".into(),
        ));
    }
    let offset = if format == VtkXmlArrayFormat::Appended {
        Some(
            attribute(element, b"offset")
                .ok_or_else(|| {
                    Error::InvalidInput("VTK XML appended DataArray is missing its offset".into())
                })?
                .parse::<usize>()
                .map_err(|_| Error::InvalidInput("invalid VTK XML DataArray offset".into()))?,
        )
    } else {
        None
    };
    Ok(VtkXmlArray {
        context: context.to_owned(),
        name: attribute(element, b"Name"),
        components,
        scalar_type,
        format,
        offset,
        text: String::new(),
    })
}

fn decode_vtk_base64(text: &str, context: &str) -> Result<Vec<u8>> {
    let text = text.trim();
    let encoded = if context == "AppendedData" {
        text.strip_prefix('_').ok_or_else(|| {
            Error::InvalidInput("VTK XML AppendedData is missing its required '_' prefix".into())
        })?
    } else {
        text
    };
    let compact = encoded
        .bytes()
        .filter(|byte| !byte.is_ascii_whitespace())
        .collect::<Vec<_>>();
    BASE64_STANDARD.decode(compact).map_err(|error| {
        Error::InvalidInput(format!("invalid base64 in VTK XML {context}: {error}"))
    })
}

fn vtk_header_width(header_type: VtkHeaderType) -> usize {
    match header_type {
        VtkHeaderType::UInt32 => 4,
        VtkHeaderType::UInt64 => 8,
    }
}

fn read_vtk_header_value(
    bytes: &[u8],
    offset: &mut usize,
    header_type: VtkHeaderType,
    byte_order: VtkByteOrder,
) -> Result<u64> {
    let width = vtk_header_width(header_type);
    let end = offset
        .checked_add(width)
        .ok_or_else(|| Error::InvalidInput("VTK XML binary header offset overflowed".into()))?;
    let value = bytes
        .get(*offset..end)
        .ok_or_else(|| Error::InvalidInput("VTK XML binary array has a truncated header".into()))?;
    *offset = end;
    Ok(match (header_type, byte_order) {
        (VtkHeaderType::UInt32, VtkByteOrder::Little) => u32::from_le_bytes(
            value
                .try_into()
                .map_err(|_| Error::InvalidInput("invalid VTK XML UInt32 header".into()))?,
        ) as u64,
        (VtkHeaderType::UInt32, VtkByteOrder::Big) => u32::from_be_bytes(
            value
                .try_into()
                .map_err(|_| Error::InvalidInput("invalid VTK XML UInt32 header".into()))?,
        ) as u64,
        (VtkHeaderType::UInt64, VtkByteOrder::Little) => u64::from_le_bytes(
            value
                .try_into()
                .map_err(|_| Error::InvalidInput("invalid VTK XML UInt64 header".into()))?,
        ),
        (VtkHeaderType::UInt64, VtkByteOrder::Big) => u64::from_be_bytes(
            value
                .try_into()
                .map_err(|_| Error::InvalidInput("invalid VTK XML UInt64 header".into()))?,
        ),
    })
}

fn decode_vtk_binary_payload(
    bytes: &[u8],
    header_type: VtkHeaderType,
    byte_order: VtkByteOrder,
    compressor: Option<&str>,
) -> Result<(Vec<u8>, usize)> {
    let mut offset = 0usize;
    if compressor.is_none_or(str::is_empty) {
        let length = usize::try_from(read_vtk_header_value(
            bytes,
            &mut offset,
            header_type,
            byte_order,
        )?)
        .map_err(|_| Error::LimitExceeded("VTK XML binary array does not fit in memory".into()))?;
        if length > MAX_VTK_DECODED_ARRAY_BYTES {
            return Err(Error::LimitExceeded(format!(
                "VTK XML binary array exceeds {MAX_VTK_DECODED_ARRAY_BYTES} decoded bytes"
            )));
        }
        let end = offset
            .checked_add(length)
            .ok_or_else(|| Error::InvalidInput("VTK XML binary array size overflowed".into()))?;
        let payload = bytes.get(offset..end).ok_or_else(|| {
            Error::InvalidInput("VTK XML binary array payload is truncated".into())
        })?;
        return Ok((payload.to_vec(), end));
    }

    if compressor != Some("vtkZLibDataCompressor") {
        return Err(Error::Unsupported(format!(
            "VTK XML compressor '{}' is unsupported; vtkZLibDataCompressor is supported",
            compressor.unwrap_or_default()
        )));
    }
    let block_count = usize::try_from(read_vtk_header_value(
        bytes,
        &mut offset,
        header_type,
        byte_order,
    )?)
    .map_err(|_| Error::LimitExceeded("VTK XML compression block count is too large".into()))?;
    let block_size = usize::try_from(read_vtk_header_value(
        bytes,
        &mut offset,
        header_type,
        byte_order,
    )?)
    .map_err(|_| Error::LimitExceeded("VTK XML compression block size is too large".into()))?;
    let last_block_size = usize::try_from(read_vtk_header_value(
        bytes,
        &mut offset,
        header_type,
        byte_order,
    )?)
    .map_err(|_| Error::LimitExceeded("VTK XML final compression block is too large".into()))?;
    if block_count > MAX_VTK_COMPRESSION_BLOCKS {
        return Err(Error::LimitExceeded(format!(
            "VTK XML compression block count exceeds {MAX_VTK_COMPRESSION_BLOCKS}"
        )));
    }
    if block_count == 0 {
        return Ok((Vec::new(), offset));
    }
    if block_size == 0 || last_block_size > block_size {
        return Err(Error::InvalidInput(
            "invalid VTK XML compression block dimensions".into(),
        ));
    }
    let full_prefix = block_size
        .checked_mul(block_count - 1)
        .ok_or_else(|| Error::LimitExceeded("VTK XML decoded size overflowed".into()))?;
    let decoded_size = full_prefix
        .checked_add(last_block_size)
        .ok_or_else(|| Error::LimitExceeded("VTK XML decoded size overflowed".into()))?;
    if decoded_size > MAX_VTK_DECODED_ARRAY_BYTES {
        return Err(Error::LimitExceeded(format!(
            "VTK XML compressed array exceeds {MAX_VTK_DECODED_ARRAY_BYTES} decoded bytes"
        )));
    }
    let mut compressed_sizes = Vec::with_capacity(block_count);
    let mut total_compressed = 0usize;
    for _ in 0..block_count {
        let size = usize::try_from(read_vtk_header_value(
            bytes,
            &mut offset,
            header_type,
            byte_order,
        )?)
        .map_err(|_| Error::LimitExceeded("VTK XML compressed block is too large".into()))?;
        total_compressed = total_compressed
            .checked_add(size)
            .ok_or_else(|| Error::LimitExceeded("VTK XML compressed size overflowed".into()))?;
        compressed_sizes.push(size);
    }
    let compressed_end = offset
        .checked_add(total_compressed)
        .ok_or_else(|| Error::InvalidInput("VTK XML compressed payload size overflowed".into()))?;
    if compressed_end > bytes.len() {
        return Err(Error::InvalidInput(
            "VTK XML compressed payload is truncated".into(),
        ));
    }

    let mut output = Vec::with_capacity(decoded_size);
    for (block_index, compressed_size) in compressed_sizes.iter().copied().enumerate() {
        let block_end = offset + compressed_size;
        let expected_size = if block_index + 1 == block_count {
            last_block_size
        } else {
            block_size
        };
        let mut decoder = ZlibDecoder::new(&bytes[offset..block_end]);
        let mut decoded = Vec::with_capacity(expected_size);
        Read::take(&mut decoder, expected_size.saturating_add(1) as u64)
            .read_to_end(&mut decoded)?;
        if decoded.len() != expected_size {
            return Err(Error::InvalidInput(format!(
                "VTK XML zlib block {block_index} decoded to {} bytes; expected {expected_size}",
                decoded.len()
            )));
        }
        output.extend_from_slice(&decoded);
        offset = block_end;
    }
    Ok((output, compressed_end))
}

fn parse_vtk_optional_count(
    element: &quick_xml::events::BytesStart<'_>,
    name: &[u8],
) -> Result<Option<usize>> {
    attribute(element, name)
        .map(|value| {
            value.parse::<usize>().map_err(|_| {
                Error::InvalidInput(format!(
                    "invalid VTK XML {} count",
                    String::from_utf8_lossy(name)
                ))
            })
        })
        .transpose()
}

fn parse_vtk_extent(value: &str, context: &str) -> Result<[i64; 6]> {
    let parsed = value
        .split_whitespace()
        .map(|token| {
            token.parse::<i64>().map_err(|_| {
                Error::InvalidInput(format!("invalid VTK XML {context} value '{token}'"))
            })
        })
        .collect::<Result<Vec<_>>>()?;
    let extent: [i64; 6] = parsed
        .try_into()
        .map_err(|_| Error::InvalidInput(format!("VTK XML {context} must contain six integers")))?;
    vtk_image_dimensions(extent)?;
    Ok(extent)
}

fn parse_vtk_image_geometry(element: &BytesStart<'_>) -> Result<VtkImageGeometry> {
    let origin = parse_vtk_float_tuple(
        attribute(element, b"Origin").as_deref(),
        [0.0; 3],
        3,
        "Origin",
    )?
    .try_into()
    .map_err(|_| {
        Error::InvalidInput("VTK XML ImageData Origin must contain three values".into())
    })?;
    let spacing = parse_vtk_float_tuple(
        attribute(element, b"Spacing").as_deref(),
        [1.0; 3],
        3,
        "Spacing",
    )?
    .try_into()
    .map_err(|_| {
        Error::InvalidInput("VTK XML ImageData Spacing must contain three values".into())
    })?;
    let direction = parse_vtk_float_tuple(
        attribute(element, b"Direction").as_deref(),
        vec![1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0],
        9,
        "Direction",
    )?
    .try_into()
    .map_err(|_| {
        Error::InvalidInput("VTK XML ImageData Direction must contain nine values".into())
    })?;
    let whole_extent = attribute(element, b"WholeExtent")
        .map(|value| parse_vtk_extent(&value, "WholeExtent"))
        .transpose()?;
    if whole_extent.is_none() {
        return Err(Error::InvalidInput(
            "VTK XML ImageData is missing WholeExtent".into(),
        ));
    }
    Ok(VtkImageGeometry {
        origin,
        spacing,
        direction,
        whole_extent,
    })
}

fn parse_vtk_float_tuple(
    value: Option<&str>,
    default: impl Into<Vec<f64>>,
    expected: usize,
    context: &str,
) -> Result<Vec<f64>> {
    let Some(value) = value else {
        return Ok(default.into());
    };
    let values = value
        .split_whitespace()
        .map(|token| {
            let parsed = token.parse::<f64>().map_err(|_| {
                Error::InvalidInput(format!(
                    "invalid VTK XML ImageData {context} value '{token}'"
                ))
            })?;
            if !parsed.is_finite() {
                return Err(Error::InvalidInput(format!(
                    "VTK XML ImageData {context} contains a non-finite value"
                )));
            }
            Ok(parsed)
        })
        .collect::<Result<Vec<_>>>()?;
    if values.len() != expected {
        return Err(Error::InvalidInput(format!(
            "VTK XML ImageData {context} must contain {expected} values"
        )));
    }
    Ok(values)
}

fn vtk_image_dimensions(extent: [i64; 6]) -> Result<[usize; 3]> {
    let mut dimensions = [0usize; 3];
    for axis in 0..3 {
        let min = extent[axis * 2];
        let max = extent[axis * 2 + 1];
        let size = max
            .checked_sub(min)
            .and_then(|difference| difference.checked_add(1))
            .and_then(|size| usize::try_from(size).ok())
            .filter(|size| *size > 0)
            .ok_or_else(|| {
                Error::InvalidInput("VTK XML ImageData Extent is empty or invalid".into())
            })?;
        dimensions[axis] = size;
    }
    let points = dimensions[0]
        .checked_mul(dimensions[1])
        .and_then(|count| count.checked_mul(dimensions[2]))
        .ok_or_else(|| Error::LimitExceeded("VTK XML ImageData point count overflowed".into()))?;
    if points > MAX_SIMULATION_POINTS {
        return Err(Error::LimitExceeded(format!(
            "VTK XML ImageData exceeds {MAX_SIMULATION_POINTS} points"
        )));
    }
    Ok(dimensions)
}

fn vtk_extent_contains(whole: [i64; 6], piece: [i64; 6]) -> bool {
    (0..3).all(|axis| {
        piece[axis * 2] >= whole[axis * 2] && piece[axis * 2 + 1] <= whole[axis * 2 + 1]
    })
}

fn vtk_image_points(extent: [i64; 6], geometry: &VtkImageGeometry) -> Result<Vec<f64>> {
    let dimensions = vtk_image_dimensions(extent)?;
    let point_count = dimensions[0]
        .checked_mul(dimensions[1])
        .and_then(|count| count.checked_mul(dimensions[2]))
        .ok_or_else(|| Error::LimitExceeded("VTK XML ImageData point count overflowed".into()))?;
    let value_count = point_count.checked_mul(3).ok_or_else(|| {
        Error::LimitExceeded("VTK XML ImageData coordinate count overflowed".into())
    })?;
    let mut values = Vec::with_capacity(value_count);
    for k in 0..dimensions[2] {
        for j in 0..dimensions[1] {
            for i in 0..dimensions[0] {
                let indices = [i, j, k].map(|local| local as f64);
                let scaled = [0, 1, 2]
                    .map(|axis| (extent[axis * 2] as f64 + indices[axis]) * geometry.spacing[axis]);
                for output_axis in 0..3 {
                    let coordinate = geometry.origin[output_axis]
                        + (0..3)
                            .map(|input_axis| {
                                geometry.direction[output_axis * 3 + input_axis]
                                    * scaled[input_axis]
                            })
                            .sum::<f64>();
                    if !coordinate.is_finite() {
                        return Err(Error::InvalidInput(
                            "VTK XML ImageData coordinate is outside the supported range".into(),
                        ));
                    }
                    values.push(coordinate);
                }
            }
        }
    }
    Ok(values)
}

fn vtk_rectilinear_points(extent: [i64; 6], coordinates: &[Vec<f64>]) -> Result<Vec<f64>> {
    let dimensions = vtk_image_dimensions(extent)?;
    if coordinates.len() != 3
        || coordinates
            .iter()
            .zip(dimensions)
            .any(|(axis, expected)| axis.len() != expected)
    {
        return Err(Error::InvalidInput(
            "VTK XML RectilinearGrid Coordinates do not match its Piece Extent".into(),
        ));
    }
    let point_count = dimensions[0]
        .checked_mul(dimensions[1])
        .and_then(|count| count.checked_mul(dimensions[2]))
        .ok_or_else(|| {
            Error::LimitExceeded("VTK XML RectilinearGrid point count overflowed".into())
        })?;
    let value_count = point_count.checked_mul(3).ok_or_else(|| {
        Error::LimitExceeded("VTK XML RectilinearGrid coordinate count overflowed".into())
    })?;
    let mut values = Vec::with_capacity(value_count);
    for k in 0..dimensions[2] {
        for j in 0..dimensions[1] {
            for i in 0..dimensions[0] {
                values.extend_from_slice(&[
                    coordinates[0][i],
                    coordinates[1][j],
                    coordinates[2][k],
                ]);
            }
        }
    }
    Ok(values)
}

fn vtk_image_cells(extent: [i64; 6]) -> Result<Vec<Vec<usize>>> {
    let dimensions = vtk_image_dimensions(extent)?;
    let active_axes = (0..3)
        .filter(|axis| dimensions[*axis] > 1)
        .collect::<Vec<_>>();
    if active_axes.is_empty() {
        return Ok(Vec::new());
    }
    let cell_dimensions = dimensions.map(|size| if size > 1 { size - 1 } else { 1 });
    let cell_count = cell_dimensions[0]
        .checked_mul(cell_dimensions[1])
        .and_then(|count| count.checked_mul(cell_dimensions[2]))
        .ok_or_else(|| Error::LimitExceeded("VTK XML ImageData cell count overflowed".into()))?;
    if cell_count > MAX_SIMULATION_CELLS {
        return Err(Error::LimitExceeded(format!(
            "VTK XML ImageData exceeds {MAX_SIMULATION_CELLS} cells"
        )));
    }
    let rendered_count = cell_count
        .checked_mul(if active_axes.len() == 3 { 12 } else { 1 })
        .ok_or_else(|| {
            Error::LimitExceeded("VTK XML ImageData rendered count overflowed".into())
        })?;
    if rendered_count > MAX_VTK_RENDERED_PRIMITIVES {
        return Err(Error::LimitExceeded(format!(
            "VTK XML ImageData exceeds {MAX_VTK_RENDERED_PRIMITIVES} rendered primitives"
        )));
    }
    let point_id = |coordinate: [usize; 3]| -> Result<usize> {
        coordinate[2]
            .checked_mul(dimensions[1])
            .and_then(|value| value.checked_add(coordinate[1]))
            .and_then(|value| value.checked_mul(dimensions[0]))
            .and_then(|value| value.checked_add(coordinate[0]))
            .ok_or_else(|| Error::LimitExceeded("VTK XML ImageData point index overflowed".into()))
    };
    let mut cells = Vec::with_capacity(cell_count);
    for k in 0..cell_dimensions[2] {
        for j in 0..cell_dimensions[1] {
            for i in 0..cell_dimensions[0] {
                let base = [i, j, k];
                let cell = match active_axes.as_slice() {
                    [axis] => {
                        let mut next = base;
                        next[*axis] += 1;
                        vec![point_id(base)?, point_id(next)?]
                    }
                    [first, second] => {
                        let mut p10 = base;
                        let mut p11 = base;
                        let mut p01 = base;
                        p10[*first] += 1;
                        p11[*first] += 1;
                        p11[*second] += 1;
                        p01[*second] += 1;
                        vec![
                            point_id(base)?,
                            point_id(p10)?,
                            point_id(p11)?,
                            point_id(p01)?,
                        ]
                    }
                    [_, _, _] => {
                        let mut p100 = base;
                        let mut p110 = base;
                        let mut p010 = base;
                        let mut p001 = base;
                        let mut p101 = base;
                        let mut p111 = base;
                        let mut p011 = base;
                        p100[0] += 1;
                        p110[0] += 1;
                        p110[1] += 1;
                        p010[1] += 1;
                        p001[2] += 1;
                        p101[0] += 1;
                        p101[2] += 1;
                        p111[0] += 1;
                        p111[1] += 1;
                        p111[2] += 1;
                        p011[1] += 1;
                        p011[2] += 1;
                        vec![
                            point_id(base)?,
                            point_id(p100)?,
                            point_id(p110)?,
                            point_id(p010)?,
                            point_id(p001)?,
                            point_id(p101)?,
                            point_id(p111)?,
                            point_id(p011)?,
                        ]
                    }
                    _ => {
                        return Err(Error::InvalidInput(
                            "invalid VTK ImageData dimension".into(),
                        ));
                    }
                };
                cells.push(cell);
            }
        }
    }
    Ok(cells)
}

fn finish_vtk_xml_array(
    piece: &mut VtkXmlPiece,
    array: VtkXmlArray,
    byte_order: VtkByteOrder,
    header_type: VtkHeaderType,
    compressor: Option<&str>,
) -> Result<usize> {
    match array.format {
        VtkXmlArrayFormat::Ascii => assign_vtk_xml_array(piece, array, None, byte_order),
        VtkXmlArrayFormat::InlineBinary => {
            let encoded = decode_vtk_base64(&array.text, "DataArray")?;
            let (decoded, consumed) =
                decode_vtk_binary_payload(&encoded, header_type, byte_order, compressor)?;
            if consumed != encoded.len() {
                return Err(Error::InvalidInput(
                    "VTK XML inline binary DataArray contains trailing bytes".into(),
                ));
            }
            assign_vtk_xml_array(piece, array, Some(&decoded), byte_order)
        }
        VtkXmlArrayFormat::Appended => {
            piece.pending_appended_arrays.push(array);
            Ok(0)
        }
    }
}

fn assign_vtk_xml_array(
    piece: &mut VtkXmlPiece,
    array: VtkXmlArray,
    binary: Option<&[u8]>,
    byte_order: VtkByteOrder,
) -> Result<usize> {
    let context = array.context.as_str();
    let name = array.name.as_deref().unwrap_or_default();
    match context {
        "Points" => {
            if array.components != 3 {
                return Err(Error::InvalidInput(
                    "VTK XML Points DataArray must have three components".into(),
                ));
            }
            let values = vtk_array_f64_values(&array, binary, byte_order, "point coordinates")?;
            let count = values.len();
            if piece.points.replace(values).is_some() {
                return Err(Error::InvalidInput(
                    "VTK XML Piece has multiple Points arrays".into(),
                ));
            }
            Ok(count)
        }
        "Coordinates" => {
            if array.components != 1 || piece.coordinates.len() >= 3 {
                return Err(Error::InvalidInput(
                    "VTK XML RectilinearGrid Coordinates must contain exactly three single-component arrays".into(),
                ));
            }
            let values = vtk_array_f64_values(&array, binary, byte_order, "coordinates")?;
            let count = values.len();
            piece.coordinates.push(values);
            Ok(count)
        }
        "Cells" => {
            let target = match name.to_ascii_lowercase().as_str() {
                "connectivity" => &mut piece.connectivity,
                "offsets" => &mut piece.offsets,
                "types" => &mut piece.types,
                _ => return Ok(0),
            };
            let values = vtk_array_index_values(&array, binary, byte_order, name)?;
            let count = values.len();
            if target.replace(values).is_some() {
                return Err(Error::InvalidInput(format!(
                    "VTK XML Piece has duplicate '{name}' arrays"
                )));
            }
            Ok(count)
        }
        "PointData" => {
            let is_active = piece
                .active_point_scalar
                .as_deref()
                .is_some_and(|active| active == name);
            if piece.point_scalars.is_some()
                || piece
                    .active_point_scalar
                    .as_deref()
                    .is_some_and(|active| active != name)
                || (piece.active_point_scalar.is_none() && array.components != 1)
            {
                if piece.active_point_scalar.is_none() && array.components > 1 {
                    piece.warnings.push(format!(
                        "VTK XML point array '{}' has multiple components but is not the active Scalars array; it was omitted",
                        name
                    ));
                }
                return Ok(0);
            }
            let (values, count) =
                vtk_array_scalar_values(&array, binary, byte_order, "point scalars")?;
            if is_active && array.components > 1 {
                piece.warnings.push(format!(
                    "VTK XML point array '{}' has {} components; preview colors use Euclidean magnitude",
                    name, array.components
                ));
            }
            piece.point_scalars = Some(values);
            Ok(count)
        }
        "CellData" => {
            let is_active = piece
                .active_cell_scalar
                .as_deref()
                .is_some_and(|active| active == name);
            if piece.cell_scalars.is_some()
                || piece
                    .active_cell_scalar
                    .as_deref()
                    .is_some_and(|active| active != name)
                || (piece.active_cell_scalar.is_none() && array.components != 1)
            {
                if piece.active_cell_scalar.is_none() && array.components > 1 {
                    piece.warnings.push(format!(
                        "VTK XML cell array '{}' has multiple components but is not the active Scalars array; it was omitted",
                        name
                    ));
                }
                return Ok(0);
            }
            let (values, count) =
                vtk_array_scalar_values(&array, binary, byte_order, "cell scalars")?;
            if is_active && array.components > 1 {
                piece.warnings.push(format!(
                    "VTK XML cell array '{}' has {} components; preview colors use Euclidean magnitude",
                    name, array.components
                ));
            }
            piece.cell_scalars = Some(values);
            Ok(count)
        }
        "Verts" | "Lines" | "Strips" | "Polys" => {
            let name = name.to_ascii_lowercase();
            let target = if name == "connectivity" {
                &mut piece.poly_connectivity
            } else if name == "offsets" {
                &mut piece.poly_offsets
            } else {
                return Ok(0);
            };
            let values = vtk_array_index_values(&array, binary, byte_order, &name)?;
            let count = values.len();
            if target.insert(context.to_owned(), values).is_some() {
                return Err(Error::InvalidInput(format!(
                    "VTK XML {context} has duplicate '{name}' arrays"
                )));
            }
            Ok(count)
        }
        _ => Ok(0),
    }
}

fn vtk_array_f64_values(
    array: &VtkXmlArray,
    binary: Option<&[u8]>,
    byte_order: VtkByteOrder,
    context: &str,
) -> Result<Vec<f64>> {
    let Some(bytes) = binary else {
        return parse_vtk_f64_values(&array.text, context);
    };
    let width = array.scalar_type.width();
    if bytes.len() % width != 0 {
        return Err(Error::InvalidInput(format!(
            "binary VTK XML {context} byte count is not aligned to its scalar type"
        )));
    }
    let count = bytes.len() / width;
    if count > MAX_VTK_ARRAY_VALUES {
        return Err(Error::LimitExceeded(format!(
            "binary VTK XML {context} exceeds {MAX_VTK_ARRAY_VALUES} values"
        )));
    }
    let mut values = Vec::with_capacity(count);
    for chunk in bytes.chunks_exact(width) {
        let value = array
            .scalar_type
            .parse_f64(chunk, byte_order)
            .ok_or_else(|| {
                Error::InvalidInput(format!("invalid binary VTK XML {context} value"))
            })?;
        if !value.is_finite() {
            return Err(Error::InvalidInput(format!(
                "binary VTK XML {context} contains a non-finite value"
            )));
        }
        values.push(value);
    }
    Ok(values)
}

fn vtk_array_scalar_values(
    array: &VtkXmlArray,
    binary: Option<&[u8]>,
    byte_order: VtkByteOrder,
    context: &str,
) -> Result<(Vec<f64>, usize)> {
    let values = vtk_array_f64_values(array, binary, byte_order, context)?;
    if values.len() % array.components != 0 {
        return Err(Error::InvalidInput(format!(
            "VTK XML {context} value count is not divisible by NumberOfComponents"
        )));
    }
    let raw_count = values.len();
    let mut scalars = Vec::with_capacity(raw_count / array.components);
    for tuple in values.chunks_exact(array.components) {
        let magnitude = tuple.iter().copied().fold(0.0f64, f64::hypot);
        if !magnitude.is_finite() {
            return Err(Error::InvalidInput(format!(
                "VTK XML {context} tuple magnitude overflows"
            )));
        }
        scalars.push(if array.components == 1 {
            tuple[0]
        } else {
            magnitude
        });
    }
    Ok((scalars, raw_count))
}

fn vtk_array_index_values(
    array: &VtkXmlArray,
    binary: Option<&[u8]>,
    byte_order: VtkByteOrder,
    context: &str,
) -> Result<Vec<usize>> {
    if !array.scalar_type.is_integer() {
        return Err(Error::InvalidInput(format!(
            "VTK XML {context} DataArray must use an integer type"
        )));
    }
    let Some(bytes) = binary else {
        return parse_vtk_index_values(&array.text, context);
    };
    let width = array.scalar_type.width();
    if bytes.len() % width != 0 {
        return Err(Error::InvalidInput(format!(
            "binary VTK XML {context} byte count is not aligned to its scalar type"
        )));
    }
    let count = bytes.len() / width;
    if count > MAX_VTK_ARRAY_VALUES {
        return Err(Error::LimitExceeded(format!(
            "binary VTK XML {context} exceeds {MAX_VTK_ARRAY_VALUES} indices"
        )));
    }
    bytes
        .chunks_exact(width)
        .map(|chunk| {
            array
                .scalar_type
                .parse_index(chunk, byte_order)
                .ok_or_else(|| {
                    Error::InvalidInput(format!("invalid binary VTK XML {context} index"))
                })
        })
        .collect()
}

fn parse_vtk_f64_values(text: &str, context: &str) -> Result<Vec<f64>> {
    let mut values = Vec::new();
    for token in text.split_whitespace() {
        if values.len() >= MAX_VTK_ARRAY_VALUES {
            return Err(Error::LimitExceeded(format!(
                "VTK XML {context} exceeds {MAX_VTK_ARRAY_VALUES} values"
            )));
        }
        let value = token.parse::<f64>().map_err(|_| {
            Error::InvalidInput(format!("invalid VTK XML {context} value '{token}'"))
        })?;
        if !value.is_finite() {
            return Err(Error::InvalidInput(format!(
                "VTK XML {context} contains a non-finite value"
            )));
        }
        values.push(value);
    }
    Ok(values)
}

fn parse_vtk_index_values(text: &str, context: &str) -> Result<Vec<usize>> {
    let mut values = Vec::new();
    for token in text.split_whitespace() {
        if values.len() >= MAX_VTK_ARRAY_VALUES {
            return Err(Error::LimitExceeded(format!(
                "VTK XML {context} exceeds {MAX_VTK_ARRAY_VALUES} indices"
            )));
        }
        values.push(token.parse::<usize>().map_err(|_| {
            Error::InvalidInput(format!("invalid VTK XML {context} index '{token}'"))
        })?);
    }
    Ok(values)
}

fn check_vtk_xml_value_budget(total: usize) -> Result<()> {
    if total > MAX_VTK_TOTAL_VALUES {
        return Err(Error::LimitExceeded(format!(
            "VTK XML arrays exceed {MAX_VTK_TOTAL_VALUES} total values"
        )));
    }
    Ok(())
}

fn append_vtk_xml_piece(
    mut piece: VtkXmlPiece,
    root_type: &str,
    image_geometry: Option<&VtkImageGeometry>,
    structured_whole_extent: Option<[i64; 6]>,
    nodes: &mut HashMap<usize, MeshNode>,
    cells: &mut Vec<MeshCell>,
) -> Result<()> {
    let is_structured = matches!(
        root_type,
        "ImageData" | "RectilinearGrid" | "StructuredGrid"
    );
    let structured_extent = if is_structured {
        let extent = piece.structured_extent.ok_or_else(|| {
            Error::InvalidInput(format!("VTK XML {root_type} Piece is missing Extent"))
        })?;
        let whole_extent = image_geometry
            .and_then(|geometry| geometry.whole_extent)
            .or(structured_whole_extent)
            .ok_or_else(|| {
                Error::InvalidInput(format!("VTK XML {root_type} is missing WholeExtent"))
            })?;
        if !vtk_extent_contains(whole_extent, extent) {
            return Err(Error::InvalidInput(format!(
                "VTK XML {root_type} Piece Extent exceeds WholeExtent"
            )));
        }
        Some(extent)
    } else {
        None
    };
    let point_values = match root_type {
        "ImageData" => vtk_image_points(
            structured_extent.ok_or_else(|| {
                Error::InvalidInput("VTK XML ImageData Piece is missing Extent".into())
            })?,
            image_geometry.ok_or_else(|| {
                Error::InvalidInput("VTK XML ImageData has no image geometry".into())
            })?,
        )?,
        "RectilinearGrid" => vtk_rectilinear_points(
            structured_extent.ok_or_else(|| {
                Error::InvalidInput("VTK XML RectilinearGrid Piece is missing Extent".into())
            })?,
            &piece.coordinates,
        )?,
        _ => piece
            .points
            .take()
            .ok_or_else(|| Error::InvalidInput("VTK XML Piece is missing Points".into()))?,
    };
    if point_values.len() % 3 != 0 {
        return Err(Error::InvalidInput(
            "VTK XML Points array length is not divisible by three".into(),
        ));
    }
    let point_count = point_values.len() / 3;
    if let Some(extent) = structured_extent {
        let dimensions = vtk_image_dimensions(extent)?;
        let expected = dimensions[0]
            .checked_mul(dimensions[1])
            .and_then(|count| count.checked_mul(dimensions[2]))
            .ok_or_else(|| {
                Error::LimitExceeded(format!("VTK XML {root_type} point count overflowed"))
            })?;
        if point_count != expected {
            return Err(Error::InvalidInput(format!(
                "VTK XML {root_type} point count does not match its Piece Extent"
            )));
        }
    }
    if point_count > MAX_SIMULATION_POINTS
        || piece
            .declared_points
            .is_some_and(|declared| declared != point_count)
    {
        return Err(Error::LimitExceeded(format!(
            "VTK XML point count does not match its declaration or exceeds {MAX_SIMULATION_POINTS}"
        )));
    }
    if nodes.len().saturating_add(point_count) > MAX_SIMULATION_POINTS {
        return Err(Error::LimitExceeded(format!(
            "VTK XML mesh exceeds {MAX_SIMULATION_POINTS} total points"
        )));
    }
    if piece
        .point_scalars
        .as_ref()
        .is_some_and(|values| values.len() != point_count)
    {
        return Err(Error::InvalidInput(
            "VTK XML point scalar count does not match NumberOfPoints".into(),
        ));
    }
    let node_base = nodes.len();
    for (index, coordinate) in point_values.chunks_exact(3).enumerate() {
        let global_id = node_base
            .checked_add(index)
            .ok_or_else(|| Error::LimitExceeded("VTK XML node id overflowed".into()))?;
        nodes.insert(
            global_id,
            MeshNode {
                x: coordinate[0],
                y: coordinate[1],
                scalar: piece.point_scalars.as_ref().map(|values| values[index]),
            },
        );
    }

    let mut append_cell = |local_ids: &[usize], scalar: Option<f64>| -> Result<()> {
        if local_ids.len() < 2 {
            return Ok(());
        }
        if cells.len() >= MAX_VTK_RENDERED_PRIMITIVES {
            return Err(Error::LimitExceeded(format!(
                "VTK XML rendered primitive count exceeds {MAX_VTK_RENDERED_PRIMITIVES}"
            )));
        }
        let mut node_ids = Vec::with_capacity(local_ids.len());
        for &local_id in local_ids {
            if local_id >= point_count {
                return Err(Error::InvalidInput(format!(
                    "VTK XML cell refers to point {local_id}, but its Piece has {point_count} points"
                )));
            }
            node_ids.push(node_base.checked_add(local_id).ok_or_else(|| {
                Error::LimitExceeded("VTK XML connectivity id overflowed".into())
            })?);
        }
        cells.push(MeshCell { node_ids, scalar });
        Ok(())
    };

    if is_structured {
        let extent = structured_extent.ok_or_else(|| {
            Error::InvalidInput(format!("VTK XML {root_type} Piece is missing Extent"))
        })?;
        let image_cells = vtk_image_cells(extent)?;
        if image_cells.len() > MAX_SIMULATION_CELLS {
            return Err(Error::LimitExceeded(format!(
                "VTK XML ImageData cell count exceeds {MAX_SIMULATION_CELLS}"
            )));
        }
        if piece
            .cell_scalars
            .as_ref()
            .is_some_and(|values| values.len() != image_cells.len())
        {
            return Err(Error::InvalidInput(format!(
                "VTK XML {root_type} cell scalar count does not match its Extent"
            )));
        }
        if piece
            .declared_points
            .is_some_and(|declared| declared != point_count)
            || piece
                .declared_cells
                .is_some_and(|declared| declared != image_cells.len())
        {
            return Err(Error::InvalidInput(format!(
                "VTK XML {root_type} piece counts do not match its Extent"
            )));
        }
        for (index, cell) in image_cells.iter().enumerate() {
            let scalar = piece.cell_scalars.as_ref().map(|values| values[index]);
            let cell_type = match cell.len() {
                2 => 3,
                4 => 9,
                8 => 12,
                _ => {
                    return Err(Error::InvalidInput(
                        "VTK XML ImageData generated an invalid cell".into(),
                    ));
                }
            };
            for primitive in vtk_cell_primitives(cell_type, cell, scalar)? {
                append_cell(&primitive.node_ids, primitive.scalar)?;
            }
        }
    } else if root_type == "UnstructuredGrid" {
        let connectivity = piece.connectivity.ok_or_else(|| {
            Error::InvalidInput("VTK XML UnstructuredGrid is missing connectivity".into())
        })?;
        let offsets = piece.offsets.ok_or_else(|| {
            Error::InvalidInput("VTK XML UnstructuredGrid is missing offsets".into())
        })?;
        let types = piece.types.ok_or_else(|| {
            Error::InvalidInput("VTK XML UnstructuredGrid is missing cell types".into())
        })?;
        if offsets.len() > MAX_SIMULATION_CELLS {
            return Err(Error::LimitExceeded(format!(
                "VTK XML cell count exceeds {MAX_SIMULATION_CELLS}"
            )));
        }
        if offsets.len() != types.len()
            || piece
                .declared_cells
                .is_some_and(|declared| declared != offsets.len())
        {
            return Err(Error::InvalidInput(
                "VTK XML cell arrays do not match NumberOfCells".into(),
            ));
        }
        if piece
            .cell_scalars
            .as_ref()
            .is_some_and(|values| values.len() != offsets.len())
        {
            return Err(Error::InvalidInput(
                "VTK XML cell scalar count does not match NumberOfCells".into(),
            ));
        }
        let mut start = 0usize;
        for (index, &end) in offsets.iter().enumerate() {
            if end < start || end > connectivity.len() {
                return Err(Error::InvalidInput(
                    "VTK XML cell offsets are not monotonic or exceed connectivity".into(),
                ));
            }
            let ids = &connectivity[start..end];
            let scalar = piece.cell_scalars.as_ref().map(|values| values[index]);
            for primitive in vtk_cell_primitives(types[index], ids, scalar)? {
                append_cell(&primitive.node_ids, primitive.scalar)?;
            }
            start = end;
        }
        if start != connectivity.len() {
            return Err(Error::InvalidInput(
                "VTK XML connectivity has trailing indices past the final offset".into(),
            ));
        }
    } else if root_type == "PolyData" {
        let mut raw_cell_index = 0usize;
        for section in ["Verts", "Lines", "Polys", "Strips"] {
            let connectivity = piece.poly_connectivity.remove(section).unwrap_or_default();
            let offsets = piece.poly_offsets.remove(section).unwrap_or_default();
            if connectivity.is_empty() && offsets.is_empty() {
                if piece
                    .declared_poly_cells
                    .get(&format!("NumberOf{section}"))
                    .is_some_and(|declared| *declared != 0)
                {
                    return Err(Error::InvalidInput(format!(
                        "VTK XML {section} is missing its connectivity arrays"
                    )));
                }
                continue;
            }
            if offsets.len() > MAX_SIMULATION_CELLS {
                return Err(Error::LimitExceeded(format!(
                    "VTK XML {section} cell count exceeds {MAX_SIMULATION_CELLS}"
                )));
            }
            if connectivity.is_empty() != offsets.is_empty()
                || piece
                    .declared_poly_cells
                    .get(&format!("NumberOf{section}"))
                    .is_some_and(|declared| *declared != offsets.len())
            {
                return Err(Error::InvalidInput(format!(
                    "VTK XML {section} arrays do not match the Piece declaration"
                )));
            }
            let mut start = 0usize;
            for &end in &offsets {
                if end < start || end > connectivity.len() {
                    return Err(Error::InvalidInput(format!(
                        "VTK XML {section} offsets are invalid"
                    )));
                }
                let ids = &connectivity[start..end];
                let scalar = piece
                    .cell_scalars
                    .as_ref()
                    .and_then(|values| values.get(raw_cell_index).copied());
                match section {
                    "Verts" => {}
                    "Lines" => {
                        for segment in ids.windows(2) {
                            append_cell(segment, scalar)?;
                        }
                    }
                    "Strips" if ids.len() >= 3 => {
                        for triangle_index in 0..ids.len() - 2 {
                            let triangle = if triangle_index % 2 == 0 {
                                [
                                    ids[triangle_index],
                                    ids[triangle_index + 1],
                                    ids[triangle_index + 2],
                                ]
                            } else {
                                [
                                    ids[triangle_index + 1],
                                    ids[triangle_index],
                                    ids[triangle_index + 2],
                                ]
                            };
                            append_cell(&triangle, scalar)?;
                        }
                    }
                    _ => append_cell(ids, scalar)?,
                }
                start = end;
                raw_cell_index += 1;
                if raw_cell_index > MAX_SIMULATION_CELLS {
                    return Err(Error::LimitExceeded(format!(
                        "VTK XML PolyData cell count exceeds {MAX_SIMULATION_CELLS}"
                    )));
                }
            }
            if start != connectivity.len() {
                return Err(Error::InvalidInput(format!(
                    "VTK XML {section} connectivity has trailing indices"
                )));
            }
        }
        if piece
            .cell_scalars
            .as_ref()
            .is_some_and(|values| values.len() != raw_cell_index)
        {
            return Err(Error::InvalidInput(
                "VTK XML PolyData cell scalar count does not match its cells".into(),
            ));
        }
    } else {
        return Err(Error::Unsupported(format!(
            "VTK XML dataset type '{root_type}' is unsupported"
        )));
    }
    Ok(())
}

fn vtk_cell_primitives(
    cell_type: usize,
    ids: &[usize],
    scalar: Option<f64>,
) -> Result<Vec<MeshCell>> {
    if cell_type == 1 || ids.len() < 2 {
        return Ok(Vec::new());
    }
    if matches!(cell_type, 3 | 21) {
        return Ok(vec![MeshCell {
            node_ids: vec![ids[0], ids[1]],
            scalar,
        }]);
    }
    if matches!(cell_type, 5 | 22) && ids.len() >= 3 {
        return Ok(vec![MeshCell {
            node_ids: ids[..3].to_vec(),
            scalar,
        }]);
    }
    if matches!(cell_type, 9 | 23) && ids.len() >= 4 {
        return Ok(vec![MeshCell {
            node_ids: ids[..4].to_vec(),
            scalar,
        }]);
    }
    if cell_type == 8 && ids.len() >= 4 {
        return Ok(vec![MeshCell {
            node_ids: vec![ids[0], ids[1], ids[3], ids[2]],
            scalar,
        }]);
    }
    let (edge_patterns, corner_count): (&[&[usize]], usize) = match cell_type {
        10 | 24 if ids.len() >= 4 => (&[&[0, 1], &[1, 2], &[2, 0], &[0, 3], &[1, 3], &[2, 3]], 4),
        12 | 25 if ids.len() >= 8 => (
            &[
                &[0, 1],
                &[1, 2],
                &[2, 3],
                &[3, 0],
                &[4, 5],
                &[5, 6],
                &[6, 7],
                &[7, 4],
                &[0, 4],
                &[1, 5],
                &[2, 6],
                &[3, 7],
            ],
            8,
        ),
        13 | 26 if ids.len() >= 6 => (
            &[
                &[0, 1],
                &[1, 2],
                &[2, 0],
                &[3, 4],
                &[4, 5],
                &[5, 3],
                &[0, 3],
                &[1, 4],
                &[2, 5],
            ],
            6,
        ),
        14 | 27 if ids.len() >= 5 => (
            &[
                &[0, 1],
                &[1, 2],
                &[2, 3],
                &[3, 0],
                &[0, 4],
                &[1, 4],
                &[2, 4],
                &[3, 4],
            ],
            5,
        ),
        _ => {
            return Ok(vec![MeshCell {
                node_ids: ids.to_vec(),
                scalar,
            }]);
        }
    };
    Ok(edge_patterns
        .iter()
        .map(|edge| MeshCell {
            node_ids: edge
                .iter()
                .map(|&index| ids[..corner_count][index])
                .collect(),
            scalar,
        })
        .collect())
}

fn parse_ply(text: &str) -> Result<(HashMap<usize, MeshNode>, Vec<MeshCell>, String)> {
    let mut nodes = HashMap::new();
    let mut cells = Vec::new();
    let mut title = "Stanford PLY Dataset".to_string();

    let mut num_verts = 0;
    let mut num_faces = 0;
    let in_header = true;
    let mut vert_props: Vec<String> = Vec::new();

    let mut lines = text.lines();

    for line in lines.by_ref() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }

        if in_header {
            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.is_empty() {
                continue;
            }
            if parts[0] == "comment" && parts.len() > 1 {
                title = parts[1..].join(" ");
            } else if parts[0] == "element" && parts.len() >= 3 {
                if parts[1] == "vertex" {
                    num_verts = parts[2].parse().unwrap_or(0);
                } else if parts[1] == "face" {
                    num_faces = parts[2].parse().unwrap_or(0);
                }
            } else if parts[0] == "property" && parts.len() >= 3 && num_faces == 0 {
                vert_props.push(parts[2].to_string());
            } else if line == "end_header" {
                break;
            }
        }
    }

    let x_idx = vert_props.iter().position(|p| p == "x").unwrap_or(0);
    let y_idx = vert_props.iter().position(|p| p == "y").unwrap_or(1);
    let z_idx = vert_props.iter().position(|p| p == "z").unwrap_or(2);

    for idx in 0..num_verts {
        if let Some(line) = lines.next() {
            let nums: Vec<f64> = line
                .split_whitespace()
                .filter_map(|s| s.parse().ok())
                .collect();
            if nums.len() > y_idx {
                let x = nums.get(x_idx).copied().unwrap_or(0.0);
                let y = nums.get(y_idx).copied().unwrap_or(0.0);
                let z = nums.get(z_idx).copied().unwrap_or(0.0);
                let proj_x = (x - y) * 0.8660254;
                let proj_y = (x + y) * 0.5 - z;
                nodes.insert(
                    idx,
                    MeshNode {
                        x: proj_x,
                        y: proj_y,
                        scalar: None,
                    },
                );
            }
        }
    }

    for _ in 0..num_faces {
        if let Some(line) = lines.next() {
            let parts: Vec<usize> = line
                .split_whitespace()
                .filter_map(|s| s.parse().ok())
                .collect();
            if parts.len() >= 2 {
                let count = parts[0];
                let node_ids = parts[1..=count.min(parts.len() - 1)].to_vec();
                cells.push(MeshCell {
                    node_ids,
                    scalar: None,
                });
            }
        }
    }

    Ok((nodes, cells, title))
}

fn fmt_coord(v: f64) -> String {
    if !v.is_finite() {
        return "0".to_string();
    }
    let rounded = (v * 1000.0).round() / 1000.0;
    if rounded.fract().abs() < 1e-6 {
        format!("{rounded:.0}")
    } else {
        format!("{rounded}")
    }
}

#[cfg(test)]
mod tests {
    use flate2::write::ZlibEncoder;

    use super::*;
    use std::io::{Cursor, Write};

    fn vtu_binary_fixture(compress: bool, appended: bool) -> String {
        let mut arrays: Vec<(&str, &str, Vec<u8>)> = Vec::new();
        let mut points = Vec::new();
        for value in [
            0.0f32, 0.0, 0.0, 100.0, 0.0, 0.0, 100.0, 100.0, 0.0, 0.0, 100.0, 0.0,
        ] {
            points.extend_from_slice(&value.to_le_bytes());
        }
        arrays.push(("Float32", "Points", points));
        let mut point_scalars = Vec::new();
        for value in [10.0f32, 20.0, 30.0, 40.0] {
            point_scalars.extend_from_slice(&value.to_le_bytes());
        }
        arrays.push(("Float32", "temperature", point_scalars));
        let mut cell_scalars = Vec::new();
        for value in [2.0f32, 8.0] {
            cell_scalars.extend_from_slice(&value.to_le_bytes());
        }
        arrays.push(("Float32", "stress", cell_scalars));
        let mut connectivity = Vec::new();
        for value in [0i32, 1, 2, 0, 2, 3] {
            connectivity.extend_from_slice(&value.to_le_bytes());
        }
        arrays.push(("Int32", "connectivity", connectivity));
        let mut offsets = Vec::new();
        for value in [3i32, 6] {
            offsets.extend_from_slice(&value.to_le_bytes());
        }
        arrays.push(("Int32", "offsets", offsets));
        arrays.push(("UInt8", "types", vec![5, 5]));

        let encode = |bytes: &[u8]| {
            if !compress {
                let mut result = Vec::new();
                result.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
                result.extend_from_slice(bytes);
                return result;
            }
            let mut encoder = ZlibEncoder::new(Vec::new(), flate2::Compression::default());
            encoder.write_all(bytes).unwrap();
            let compressed = encoder.finish().unwrap();
            let mut result = Vec::new();
            result.extend_from_slice(&1u32.to_le_bytes());
            result.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
            result.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
            result.extend_from_slice(&(compressed.len() as u32).to_le_bytes());
            result.extend_from_slice(&compressed);
            result
        };

        let mut appended_bytes = Vec::new();
        let mut arrays_xml = String::new();
        let mut cell_arrays_xml = String::new();
        for (type_name, name, raw) in arrays {
            let values = encode(&raw);
            let (format, encoded_value) = if appended {
                let offset = appended_bytes.len();
                appended_bytes.extend_from_slice(&values);
                (format!("appended\" offset=\"{offset}"), String::new())
            } else {
                ("binary".to_owned(), BASE64_STANDARD.encode(&values))
            };
            match name {
                "Points" => arrays_xml.push_str(&format!(
                    "<Points><DataArray type=\"{type_name}\" NumberOfComponents=\"3\" format=\"{format}\">{encoded_value}</DataArray></Points>"
                )),
                "connectivity" | "offsets" | "types" => cell_arrays_xml.push_str(&format!(
                    "<DataArray type=\"{type_name}\" Name=\"{name}\" format=\"{format}\">{encoded_value}</DataArray>"
                )),
                "temperature" => arrays_xml.push_str(&format!(
                    "<PointData Scalars=\"temperature\"><DataArray type=\"{type_name}\" Name=\"{name}\" format=\"{format}\">{encoded_value}</DataArray></PointData>"
                )),
                "stress" => arrays_xml.push_str(&format!(
                    "<CellData Scalars=\"stress\"><DataArray type=\"{type_name}\" Name=\"{name}\" format=\"{format}\">{encoded_value}</DataArray></CellData>"
                )),
                _ => unreachable!(),
            }
        }
        arrays_xml.push_str(&format!("<Cells>{cell_arrays_xml}</Cells>"));
        let compressor = if compress {
            " compressor=\"vtkZLibDataCompressor\""
        } else {
            ""
        };
        if appended {
            format!(
                "<VTKFile type=\"UnstructuredGrid\" byte_order=\"LittleEndian\" header_type=\"UInt32\"{compressor}><UnstructuredGrid><Piece NumberOfPoints=\"4\" NumberOfCells=\"2\">{arrays_xml}</Piece></UnstructuredGrid><AppendedData encoding=\"base64\">_{}</AppendedData></VTKFile>",
                BASE64_STANDARD.encode(appended_bytes)
            )
        } else {
            format!(
                "<VTKFile type=\"UnstructuredGrid\" byte_order=\"LittleEndian\" header_type=\"UInt32\"{compressor}><UnstructuredGrid><Piece NumberOfPoints=\"4\" NumberOfCells=\"2\">{arrays_xml}<Cells/></Piece></UnstructuredGrid></VTKFile>"
            )
        }
    }

    fn vtu_raw_appended_fixture(compress: bool) -> Vec<u8> {
        let xml = vtu_binary_fixture(compress, true);
        let payload_marker = "<AppendedData encoding=\"base64\">_";
        let underscore_index = xml.find(payload_marker).unwrap() + payload_marker.len() - 1;
        let payload_end =
            xml[underscore_index..].find("</AppendedData>").unwrap() + underscore_index;
        let mut payload =
            decode_vtk_base64(&xml[underscore_index..payload_end], "AppendedData").unwrap();
        // Raw payloads are arbitrary bytes and can contain strings that look like XML tags.
        payload.extend_from_slice(&[0, 0xff]);
        payload.extend_from_slice(b"</AppendedData>payload bytes");
        payload.push(0);
        let mut bytes = xml.as_bytes()[..underscore_index + 1].to_vec();
        bytes.extend_from_slice(&payload);
        bytes.extend_from_slice(xml.as_bytes()[payload_end..].as_ref());
        let encoded_marker = b"encoding=\"base64\"";
        let encoding_start = bytes
            .windows(encoded_marker.len())
            .position(|window| window == encoded_marker)
            .unwrap();
        bytes.splice(
            encoding_start..encoding_start + encoded_marker.len(),
            b"encoding=\"raw\"".iter().copied(),
        );
        bytes
    }

    fn vtk_legacy_binary_unstructured_fixture() -> Vec<u8> {
        let mut bytes = b"# vtk DataFile Version 3.0\nBinary legacy mesh\nBINARY\nDATASET UNSTRUCTURED_GRID\nPOINTS 4 float\n".to_vec();
        for value in [
            0.0f32, 0.0, 0.0, 100.0, 0.0, 0.0, 100.0, 100.0, 0.0, 0.0, 100.0, 0.0,
        ] {
            bytes.extend_from_slice(&value.to_be_bytes());
        }
        bytes.extend_from_slice(b"\nCELLS 2 8\n");
        for value in [3i32, 0, 1, 2, 3, 0, 2, 3] {
            bytes.extend_from_slice(&value.to_be_bytes());
        }
        bytes.extend_from_slice(b"\nCELL_TYPES 2\n");
        for value in [5i32, 5] {
            bytes.extend_from_slice(&value.to_be_bytes());
        }
        bytes.extend_from_slice(b"\nPOINT_DATA 4\nVECTORS velocity float\n");
        for value in [
            1.0f32, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 1.0, 1.0, 1.0,
        ] {
            bytes.extend_from_slice(&value.to_be_bytes());
        }
        bytes.extend_from_slice(b"\nSCALARS temperature float\nLOOKUP_TABLE default\n");
        for value in [10.0f32, 20.0, 30.0, 40.0] {
            bytes.extend_from_slice(&value.to_be_bytes());
        }
        bytes.extend_from_slice(b"\nCELL_DATA 2\nSCALARS stress float\nLOOKUP_TABLE default\n");
        for value in [2.0f32, 8.0] {
            bytes.extend_from_slice(&value.to_be_bytes());
        }
        bytes
    }

    fn vtk_legacy_binary_polydata_fixture() -> Vec<u8> {
        let mut bytes = b"# vtk DataFile Version 3.0\nBinary PolyData\nBINARY\nDATASET POLYDATA\nPOINTS 4 float\n".to_vec();
        for value in [
            0.0f32, 0.0, 0.0, 100.0, 0.0, 0.0, 100.0, 100.0, 0.0, 0.0, 100.0, 0.0,
        ] {
            bytes.extend_from_slice(&value.to_be_bytes());
        }
        bytes.extend_from_slice(b"\nLINES 1 3\n");
        for value in [2i32, 0, 1] {
            bytes.extend_from_slice(&value.to_be_bytes());
        }
        bytes.extend_from_slice(b"\nPOLYGONS 1 4\n");
        for value in [3i32, 0, 2, 3] {
            bytes.extend_from_slice(&value.to_be_bytes());
        }
        bytes.extend_from_slice(b"\nCELL_DATA 2\nSCALARS value float\nLOOKUP_TABLE default\n");
        for value in [3.0f32, 9.0] {
            bytes.extend_from_slice(&value.to_be_bytes());
        }
        bytes
    }

    struct DummySink(pub Vec<Page>);
    impl PageConsumer for DummySink {
        fn consume(&mut self, page: Page) -> Result<()> {
            self.0.push(page);
            Ok(())
        }
    }

    fn gmsh_v22_binary_fixture(order: GmshByteOrder) -> Vec<u8> {
        let mut bytes = b"$MeshFormat\n2.2 1 8\n".to_vec();
        let append_i32 = |output: &mut Vec<u8>, value: i32| match order {
            GmshByteOrder::Little => output.extend_from_slice(&value.to_le_bytes()),
            GmshByteOrder::Big => output.extend_from_slice(&value.to_be_bytes()),
        };
        let append_f64 = |output: &mut Vec<u8>, value: f64| match order {
            GmshByteOrder::Little => output.extend_from_slice(&value.to_le_bytes()),
            GmshByteOrder::Big => output.extend_from_slice(&value.to_be_bytes()),
        };

        append_i32(&mut bytes, 1);
        bytes.extend_from_slice(b"\n$EndMeshFormat\n$Nodes\n4\n");
        for (tag, x, y) in [
            (1, 0.0, 0.0),
            (2, 50.0, 0.0),
            (3, 50.0, 50.0),
            (4, 0.0, 50.0),
        ] {
            append_i32(&mut bytes, tag);
            append_f64(&mut bytes, x);
            append_f64(&mut bytes, y);
            append_f64(&mut bytes, 0.0);
        }
        bytes.extend_from_slice(b"\n$EndNodes\n$Elements\n2\n");
        for nodes in [[1, 2, 3], [1, 3, 4]] {
            append_i32(&mut bytes, 2); // triangle block
            append_i32(&mut bytes, 1); // one element
            append_i32(&mut bytes, 2); // physical and elementary tags
            append_i32(
                &mut bytes,
                if nodes[0] == 1 && nodes[1] == 2 { 1 } else { 2 },
            );
            append_i32(&mut bytes, 7);
            append_i32(&mut bytes, 1);
            for node in nodes {
                append_i32(&mut bytes, node);
            }
        }
        bytes.extend_from_slice(
            b"\n$EndElements\n$NodeData\n1\n\"Temperature\"\n1\n0.0\n3\n0\n1\n4\n",
        );
        for (tag, value) in [(1, 10.0), (2, 20.0), (3, 30.0), (4, 40.0)] {
            append_i32(&mut bytes, tag);
            append_f64(&mut bytes, value);
        }
        bytes.extend_from_slice(
            b"\n$EndNodeData\n$ElementData\n1\n\"Stress\"\n1\n0.0\n3\n0\n1\n2\n",
        );
        for (tag, value) in [(1, 2.0), (2, 8.0)] {
            append_i32(&mut bytes, tag);
            append_f64(&mut bytes, value);
        }
        bytes.extend_from_slice(b"\n$EndElementData\n");
        bytes
    }

    fn write_gmsh_test_i32(bytes: &mut Vec<u8>, order: GmshByteOrder, value: i32) {
        match order {
            GmshByteOrder::Little => bytes.extend_from_slice(&value.to_le_bytes()),
            GmshByteOrder::Big => bytes.extend_from_slice(&value.to_be_bytes()),
        }
    }

    fn write_gmsh_test_size(bytes: &mut Vec<u8>, order: GmshByteOrder, width: usize, value: u64) {
        match width {
            4 => {
                let value = u32::try_from(value).unwrap();
                match order {
                    GmshByteOrder::Little => bytes.extend_from_slice(&value.to_le_bytes()),
                    GmshByteOrder::Big => bytes.extend_from_slice(&value.to_be_bytes()),
                }
            }
            8 => match order {
                GmshByteOrder::Little => bytes.extend_from_slice(&value.to_le_bytes()),
                GmshByteOrder::Big => bytes.extend_from_slice(&value.to_be_bytes()),
            },
            _ => unreachable!(),
        }
    }

    fn write_gmsh_test_f64(bytes: &mut Vec<u8>, order: GmshByteOrder, value: f64) {
        match order {
            GmshByteOrder::Little => bytes.extend_from_slice(&value.to_le_bytes()),
            GmshByteOrder::Big => bytes.extend_from_slice(&value.to_be_bytes()),
        }
    }

    fn gmsh_v41_binary_fixture(
        order: GmshByteOrder,
        size_t_width: usize,
        include_entities: bool,
        include_data: bool,
    ) -> Vec<u8> {
        let mut bytes = format!("$MeshFormat\n4.1 1 {size_t_width}\n").into_bytes();
        write_gmsh_test_i32(&mut bytes, order, 1);
        bytes.extend_from_slice(b"\n$EndMeshFormat\n");

        if include_entities {
            bytes.extend_from_slice(b"$Entities\n");
            for count in [1, 0, 1, 0] {
                write_gmsh_test_size(&mut bytes, order, size_t_width, count);
            }
            write_gmsh_test_i32(&mut bytes, order, 1);
            for value in [0.0, 0.0, 0.0] {
                write_gmsh_test_f64(&mut bytes, order, value);
            }
            write_gmsh_test_size(&mut bytes, order, size_t_width, 0);
            write_gmsh_test_i32(&mut bytes, order, 1);
            for value in [0.0, 0.0, 0.0, 1.0, 1.0, 0.0] {
                write_gmsh_test_f64(&mut bytes, order, value);
            }
            write_gmsh_test_size(&mut bytes, order, size_t_width, 0);
            write_gmsh_test_size(&mut bytes, order, size_t_width, 0);
            bytes.extend_from_slice(b"\n$EndEntities\n");
        }

        bytes.extend_from_slice(b"$Nodes\n");
        for value in [1, 4, 1, 4] {
            write_gmsh_test_size(&mut bytes, order, size_t_width, value);
        }
        write_gmsh_test_i32(&mut bytes, order, 2);
        write_gmsh_test_i32(&mut bytes, order, 1);
        write_gmsh_test_i32(&mut bytes, order, 0);
        write_gmsh_test_size(&mut bytes, order, size_t_width, 4);
        for tag in 1..=4 {
            write_gmsh_test_size(&mut bytes, order, size_t_width, tag);
        }
        for [x, y, z] in [
            [0.0, 0.0, 0.0],
            [1.0, 0.0, 0.0],
            [1.0, 1.0, 0.0],
            [0.0, 1.0, 0.0],
        ] {
            for value in [x, y, z] {
                write_gmsh_test_f64(&mut bytes, order, value);
            }
        }
        bytes.extend_from_slice(b"\n$EndNodes\n$Elements\n");
        for value in [1, 2, 1, 2] {
            write_gmsh_test_size(&mut bytes, order, size_t_width, value);
        }
        write_gmsh_test_i32(&mut bytes, order, 2);
        write_gmsh_test_i32(&mut bytes, order, 1);
        write_gmsh_test_i32(&mut bytes, order, 2);
        write_gmsh_test_size(&mut bytes, order, size_t_width, 2);
        for (tag, node_tags) in [(1u64, [1u64, 2, 3]), (2, [1, 3, 4])] {
            write_gmsh_test_size(&mut bytes, order, size_t_width, tag);
            for node_tag in node_tags {
                write_gmsh_test_size(&mut bytes, order, size_t_width, node_tag);
            }
        }
        bytes.extend_from_slice(b"\n$EndElements\n");

        if include_data {
            bytes.extend_from_slice(b"$NodeData\n1\n\"Temperature\"\n1\n0.0\n3\n0\n1\n4\n");
            for (tag, value) in [(1, 10.0), (2, 20.0), (3, 30.0), (4, 40.0)] {
                write_gmsh_test_i32(&mut bytes, order, tag);
                write_gmsh_test_f64(&mut bytes, order, value);
            }
            bytes.extend_from_slice(
                b"\n$EndNodeData\n$ElementData\n1\n\"Stress\"\n1\n0.0\n3\n0\n1\n2\n",
            );
            for (tag, value) in [(1, 2.0), (2, 8.0)] {
                write_gmsh_test_i32(&mut bytes, order, tag);
                write_gmsh_test_f64(&mut bytes, order, value);
            }
            bytes.extend_from_slice(b"\n$EndElementData\n");
        }
        bytes
    }

    fn gmsh_v40_binary_fixture(order: GmshByteOrder, unsigned_long_width: usize) -> Vec<u8> {
        let mut bytes = b"$MeshFormat\n4.0 1 8\n".to_vec();
        write_gmsh_test_i32(&mut bytes, order, 1);
        bytes.extend_from_slice(b"\n$EndMeshFormat\n$Entities\n");
        for count in [1, 0, 1, 0] {
            write_gmsh_test_size(&mut bytes, order, unsigned_long_width, count);
        }
        // MSH 4.0 writes a six-value bounding box for point entities too.
        write_gmsh_test_i32(&mut bytes, order, 1);
        for value in [0.0, 0.0, 0.0, 0.0, 0.0, 0.0] {
            write_gmsh_test_f64(&mut bytes, order, value);
        }
        write_gmsh_test_size(&mut bytes, order, unsigned_long_width, 0);
        write_gmsh_test_i32(&mut bytes, order, 1);
        for value in [0.0, 0.0, 0.0, 1.0, 1.0, 0.0] {
            write_gmsh_test_f64(&mut bytes, order, value);
        }
        write_gmsh_test_size(&mut bytes, order, unsigned_long_width, 0);
        write_gmsh_test_size(&mut bytes, order, unsigned_long_width, 0);
        bytes.extend_from_slice(b"\n$EndEntities\n$Nodes\n");
        write_gmsh_test_size(&mut bytes, order, unsigned_long_width, 1);
        write_gmsh_test_size(&mut bytes, order, unsigned_long_width, 4);
        for value in [1, 2, 0] {
            write_gmsh_test_i32(&mut bytes, order, value);
        }
        write_gmsh_test_size(&mut bytes, order, unsigned_long_width, 4);
        for (tag, [x, y, z]) in [
            (1, [0.0, 0.0, 0.0]),
            (2, [100.0, 0.0, 0.0]),
            (3, [100.0, 100.0, 0.0]),
            (4, [0.0, 100.0, 0.0]),
        ] {
            write_gmsh_test_i32(&mut bytes, order, tag);
            for coordinate in [x, y, z] {
                write_gmsh_test_f64(&mut bytes, order, coordinate);
            }
        }
        bytes.extend_from_slice(b"\n$EndNodes\n$Elements\n");
        write_gmsh_test_size(&mut bytes, order, unsigned_long_width, 1);
        write_gmsh_test_size(&mut bytes, order, unsigned_long_width, 2);
        for value in [1, 2, 2] {
            write_gmsh_test_i32(&mut bytes, order, value);
        }
        write_gmsh_test_size(&mut bytes, order, unsigned_long_width, 2);
        for (tag, node_tags) in [(1, [1, 2, 3]), (2, [1, 3, 4])] {
            write_gmsh_test_i32(&mut bytes, order, tag);
            for node_tag in node_tags {
                write_gmsh_test_i32(&mut bytes, order, node_tag);
            }
        }
        bytes.extend_from_slice(
            b"\n$EndElements\n$NodeData\n1\n\"Temperature\"\n1\n0.0\n3\n0\n1\n4\n",
        );
        for (tag, value) in [(1, 10.0), (2, 20.0), (3, 30.0), (4, 40.0)] {
            write_gmsh_test_i32(&mut bytes, order, tag);
            write_gmsh_test_f64(&mut bytes, order, value);
        }
        bytes.extend_from_slice(
            b"\n$EndNodeData\n$ElementData\n1\n\"Stress\"\n1\n0.0\n3\n0\n1\n2\n",
        );
        for (tag, value) in [(1, 2.0), (2, 8.0)] {
            write_gmsh_test_i32(&mut bytes, order, tag);
            write_gmsh_test_f64(&mut bytes, order, value);
        }
        bytes.extend_from_slice(b"\n$EndElementData\n");
        bytes
    }

    #[test]
    fn parses_gmsh_mesh() {
        let msh = r#"$MeshFormat
2.2 0 8
$EndMeshFormat
$Nodes
4
1 0.0 0.0 0.0
2 50.0 0.0 0.0
3 50.0 50.0 0.0
4 0.0 50.0 0.0
$EndNodes
$Elements
2
1 2 2 0 1 1 2 3
2 2 2 0 1 1 3 4
$EndElements
"#;
        let mut sink = DummySink(Vec::new());
        let options = ConvertOptions::default();
        let warnings = convert(Cursor::new(msh), &options, &mut sink).expect("convert gmsh");
        assert!(warnings.is_empty());
        assert_eq!(sink.0.len(), 1);
        let page = &sink.0[0];
        assert_eq!(page.source_format, "simulation");
        assert!(page.nodes.len() >= 2);
    }

    #[test]
    fn parses_gmsh_v41_ascii_block_mesh() {
        let msh = r#"$MeshFormat
4.1 0 8
$EndMeshFormat
$Nodes
1 6 1 6
2 1 0 6
1
2
3
4
5
6
0 0 0
1 0 0
1 1 0
0 1 0
2 0 0
2 1 0
$EndNodes
$Elements
1 2 1 2
2 1 3 2
1 1 2 3 4
2 2 5 6 3
$EndElements
$NodeData
1
"Temperature"
1
0.0
3
0
1
6
1 10.0
2 20.0
3 30.0
4 40.0
5 50.0
6 60.0
$EndNodeData
$ElementData
1
"Strain"
1
0.0
3
0
1
2
1 2.0
2 8.0
$EndElementData
"#;
        let (nodes, cells, title, warnings) = parse_gmsh(msh).unwrap();
        assert_eq!(title, "Gmsh MSH 4.1 FEA Mesh: Temperature / Strain");
        assert_eq!(nodes.len(), 6);
        assert_eq!(cells.len(), 2);
        assert_eq!(cells[0].node_ids, vec![1, 2, 3, 4]);
        assert_eq!(nodes.get(&4).unwrap().scalar, Some(40.0));
        assert_eq!(cells[1].scalar, Some(8.0));
        assert!(warnings.is_empty());
    }

    #[test]
    fn parses_gmsh_v40_interleaved_nodes_and_swapped_entity_fields() {
        let msh = r#"$MeshFormat
4.0 0 8
$EndMeshFormat
$Nodes
1 4
1 2 0 4
10 0 0 0
20 50 0 0
30 50 50 0
40 0 50 0
$EndNodes
$Elements
1 2
1 2 2 2
100 10 20 30
200 10 30 40
$EndElements
$NodeData
1
"Temperature"
1
0.0
3
0
1
4
10 1.0
20 2.0
30 3.0
40 4.0
$EndNodeData
$ElementData
1
"Stress"
1
0.0
3
0
1
2
100 5.0
200 9.0
$EndElementData
"#;
        let (nodes, cells, title, warnings) = parse_gmsh(msh).unwrap();
        assert_eq!(title, "Gmsh MSH 4.0 FEA Mesh: Temperature / Stress");
        assert_eq!(nodes.len(), 4);
        assert_eq!(nodes.get(&40).unwrap().scalar, Some(4.0));
        assert_eq!(cells.len(), 2);
        assert_eq!(cells[0].node_ids, vec![10, 20, 30]);
        assert_eq!(cells[1].scalar, Some(9.0));
        assert!(warnings.is_empty());
    }

    #[test]
    fn parses_gmsh_v41_volume_cell_as_wireframe_edges() {
        let msh = r#"$MeshFormat
4.1 0 8
$EndMeshFormat
$Nodes
1 4 1 4
3 1 0 4
1 2 3 4
0 0 0
1 0 0
0 1 0
0 0 1
$EndNodes
$Elements
1 1 1 1
3 1 4 1
1 1 2 3 4
$EndElements
"#;
        let (nodes, cells, _, warnings) = parse_gmsh(msh).unwrap();
        assert_eq!(nodes.len(), 4);
        assert_eq!(cells.len(), 6);
        assert!(cells.iter().all(|cell| cell.node_ids.len() == 2));
        assert!(warnings.is_empty());
    }

    #[test]
    fn refuses_binary_gmsh_without_attempting_text_recovery() {
        let msh = "$MeshFormat\n4.1 1 8\n$EndMeshFormat\n";
        assert!(matches!(parse_gmsh(msh), Err(Error::Unsupported(_))));
    }

    #[test]
    fn parses_binary_gmsh_v22_little_and_big_endian_meshes_and_fields() {
        for order in [GmshByteOrder::Little, GmshByteOrder::Big] {
            let bytes = gmsh_v22_binary_fixture(order);
            assert!(gmsh_binary_header(&bytes).is_some());
            let (nodes, cells, title, warnings) = parse_gmsh_binary_v22(&bytes).unwrap();
            assert_eq!(nodes.len(), 4);
            assert_eq!(nodes.get(&4).unwrap().scalar, Some(40.0));
            assert_eq!(cells.len(), 2);
            assert_eq!(cells[0].scalar, Some(2.0));
            assert_eq!(cells[1].scalar, Some(8.0));
            assert!(title.contains("Temperature / Stress"));
            assert!(warnings.is_empty());

            let mut no_trailing_newline = bytes.clone();
            no_trailing_newline.pop();
            assert!(parse_gmsh_binary_v22(&no_trailing_newline).is_ok());

            let mut sink = DummySink(Vec::new());
            let warnings =
                convert(Cursor::new(bytes), &ConvertOptions::default(), &mut sink).unwrap();
            assert!(
                warnings
                    .iter()
                    .any(|warning| warning.contains("color map uses cell values"))
            );
            assert_eq!(sink.0.len(), 1);
        }
    }

    #[test]
    fn parses_binary_gmsh_v41_blocks_entities_fields_and_size_t_widths() {
        for (order, size_t_width) in [
            (GmshByteOrder::Little, 8),
            (GmshByteOrder::Big, 8),
            (GmshByteOrder::Little, 4),
        ] {
            let bytes = gmsh_v41_binary_fixture(order, size_t_width, true, true);
            assert_eq!(gmsh_binary_header(&bytes).unwrap().0, 4.1);
            let (nodes, cells, title, warnings) = parse_gmsh_binary_v41(&bytes).unwrap();
            assert_eq!(nodes.len(), 4);
            assert_eq!(nodes.get(&4).unwrap().scalar, Some(40.0));
            assert_eq!(cells.len(), 2);
            assert_eq!(cells[0].node_ids, vec![1, 2, 3]);
            assert_eq!(cells[1].scalar, Some(8.0));
            assert_eq!(title, "Gmsh MSH 4.1 Binary FEA Mesh: Temperature / Stress");
            assert!(warnings.is_empty());

            let mut sink = DummySink(Vec::new());
            let warnings = convert(
                Cursor::new(bytes.clone()),
                &ConvertOptions::default(),
                &mut sink,
            )
            .unwrap();
            assert!(
                warnings
                    .iter()
                    .any(|warning| warning.contains("both point and cell"))
            );
            assert_eq!(sink.0.len(), 1);

            let mut truncated = bytes;
            truncated.truncate(truncated.len() - 3);
            assert!(matches!(
                parse_gmsh_binary_v41(&truncated),
                Err(Error::InvalidInput(_))
            ));
        }
    }

    #[test]
    fn parses_binary_gmsh_v40_mixed_records_with_both_endiannesses_and_word_widths() {
        for order in [GmshByteOrder::Little, GmshByteOrder::Big] {
            for width in [4, 8] {
                let bytes = gmsh_v40_binary_fixture(order, width);
                assert_eq!(gmsh_binary_header(&bytes).unwrap().0, 4.0);
                let (nodes, cells, title, warnings) = parse_gmsh_binary_v40(&bytes).unwrap();
                assert_eq!(nodes.len(), 4);
                assert_eq!(nodes[&1].scalar, Some(10.0));
                assert_eq!(cells.len(), 2);
                assert_eq!(cells[1].scalar, Some(8.0));
                assert!(title.contains("Temperature"));
                assert!(warnings.is_empty());

                let mut sink = DummySink(Vec::new());
                convert(Cursor::new(bytes), &ConvertOptions::default(), &mut sink).unwrap();
                assert_eq!(sink.0.len(), 1);
            }
        }
    }

    #[test]
    fn enforces_binary_gmsh_limits_and_rejects_truncated_or_newer_binary_meshes() {
        let fixture = gmsh_v22_binary_fixture(GmshByteOrder::Little);
        assert!(matches!(
            parse_gmsh_binary_v22(&fixture[..fixture.len() - 5]),
            Err(Error::InvalidInput(_))
        ));

        let mut oversized = b"$MeshFormat\n2.2 1 8\n".to_vec();
        oversized.extend_from_slice(&1i32.to_le_bytes());
        oversized.extend_from_slice(b"\n$EndMeshFormat\n$Nodes\n1000001\n");
        assert!(matches!(
            parse_gmsh_binary_v22(&oversized),
            Err(Error::LimitExceeded(_))
        ));

        let mut binary_v41 = b"$MeshFormat\n4.1 1 8\n".to_vec();
        binary_v41.extend_from_slice(&1i32.to_le_bytes());
        binary_v41.extend_from_slice(b"\n$EndMeshFormat\n");
        assert!(matches!(
            parse_gmsh_binary_v22(&binary_v41),
            Err(Error::Unsupported(_))
        ));
    }

    #[test]
    fn maps_multicomponent_gmsh_node_data_to_magnitude_with_a_warning() {
        let msh = r#"$MeshFormat
2.2 0 8
$EndMeshFormat
$Nodes
3
1 0 0 0
2 1 0 0
3 0 1 0
$EndNodes
$Elements
1
1 2 0 1 2 3
$EndElements
$NodeData
1
"Displacement"
1
0.0
3
0
3
3
1 1 2 2
2 3 4 0
3 0 0 5
$EndNodeData
"#;
        let (nodes, _, title, warnings) = parse_gmsh(msh).unwrap();
        assert_eq!(title, "Gmsh FEA Mesh: Displacement");
        assert_eq!(nodes.get(&1).unwrap().scalar, Some(3.0));
        assert_eq!(nodes.get(&2).unwrap().scalar, Some(5.0));
        assert_eq!(nodes.get(&3).unwrap().scalar, Some(5.0));
        assert!(
            warnings
                .iter()
                .any(|warning| warning.contains("Euclidean magnitude"))
        );
    }

    #[test]
    fn colors_point_scalars_on_line_mesh_edges() {
        let msh = r#"$MeshFormat
2.2 0 8
$EndMeshFormat
$Nodes
3
1 0 0 0
2 100 0 0
3 100 100 0
$EndNodes
$Elements
2
1 1 0 1 2
2 1 0 2 3
$EndElements
$NodeData
1
"Temperature"
1
0.0
3
0
1
3
1 0.0
2 5.0
3 10.0
$EndNodeData
"#;
        let mut sink = DummySink(Vec::new());
        let warnings = convert(Cursor::new(msh), &ConvertOptions::default(), &mut sink).unwrap();
        assert!(warnings.is_empty());
        let colors = mesh_stroke_colors(&sink.0[0]);
        assert_eq!(colors.len(), 2);
        assert!(!colors.contains("#334155"));
    }

    #[test]
    fn colors_cell_scalars_on_volume_wireframe_edges() {
        let msh = r#"$MeshFormat
2.2 0 8
$EndMeshFormat
$Nodes
5
1 0 0 0
2 100 0 0
3 0 100 0
4 0 0 100
5 100 100 100
$EndNodes
$Elements
2
1 4 0 1 2 3 4
2 4 0 2 3 4 5
$EndElements
$ElementData
1
"Stress"
1
0.0
3
0
1
2
1 0.0
2 10.0
$EndElementData
"#;
        let mut sink = DummySink(Vec::new());
        let warnings = convert(Cursor::new(msh), &ConvertOptions::default(), &mut sink).unwrap();
        assert!(warnings.is_empty());
        let colors = mesh_stroke_colors(&sink.0[0]);
        assert_eq!(colors.len(), 2);
        assert!(!colors.contains("#334155"));
    }

    fn mesh_stroke_colors(page: &Page) -> HashSet<String> {
        page.nodes
            .iter()
            .find_map(|node| match node {
                Node::Group { id, nodes, .. } if id == "mesh-cells" => Some(nodes),
                _ => None,
            })
            .unwrap()
            .iter()
            .filter_map(|node| match node {
                Node::Path {
                    stroke:
                        Stroke {
                            paint: Paint::Solid { color, .. },
                            ..
                        },
                    ..
                } => Some(color.clone()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn parses_vtk_with_scalars_and_colorbar() {
        let vtk = r#"# vtk DataFile Version 3.0
Stress Field Simulation
ASCII
DATASET UNSTRUCTURED_GRID
POINTS 4 float
0.0 0.0 0.0
100.0 0.0 0.0
100.0 100.0 0.0
0.0 100.0 0.0
CELLS 2 8
3 0 1 2
3 0 2 3
CELL_TYPES 2
5
5
POINT_DATA 4
SCALARS von_mises float 1
LOOKUP_TABLE default
10.0 50.0 120.0 200.0
"#;
        let mut sink = DummySink(Vec::new());
        let options = ConvertOptions::default();
        let warnings = convert(Cursor::new(vtk), &options, &mut sink).expect("convert vtk");
        assert!(warnings.is_empty());
        assert_eq!(sink.0.len(), 1);
        let page = &sink.0[0];
        assert_eq!(page.source_format, "simulation");
        // Colorbar nodes must be present
        assert!(page.nodes.iter().any(|n| match n {
            Node::Text { id, .. } => id == "colorbar-max",
            _ => false,
        }));
    }

    #[test]
    fn parses_legacy_vtk_polydata_cells_and_point_and_cell_scalars() {
        let vtk = r#"# vtk DataFile Version 3.0
PolyData scalar test
ASCII
DATASET POLYDATA
POINTS 5 float
0 0 0
100 0 0
100 100 0
0 100 0
50 50 0
VERTICES 1 2
1 4
LINES 1 3
2 0 1
POLYGONS 1 4
3 0 1 4
TRIANGLE_STRIPS 1 5
4 0 1 4 2
POINT_DATA 5
SCALARS temperature float
LOOKUP_TABLE default
10 20 30 40 50
CELL_DATA 4
SCALARS stress float 1
LOOKUP_TABLE default
1 2 3 4
"#;
        let (nodes, cells, title, warnings) = parse_vtk(vtk, 10_000).unwrap();
        assert_eq!(title, "PolyData scalar test");
        assert_eq!(nodes.len(), 5);
        assert_eq!(nodes.get(&4).unwrap().scalar, Some(50.0));
        assert_eq!(cells.len(), 4);
        assert_eq!(cells[0].node_ids, vec![0, 1]);
        assert_eq!(cells[0].scalar, Some(2.0));
        assert_eq!(cells[1].scalar, Some(3.0));
        assert_eq!(cells[2].scalar, Some(4.0));
        assert_eq!(cells[3].scalar, Some(4.0));
        assert!(warnings.is_empty());
    }

    #[test]
    fn parses_big_endian_binary_legacy_vtk_unstructured_and_polydata() {
        let unstructured = vtk_legacy_binary_unstructured_fixture();
        let (nodes, cells, title, warnings) = parse_vtk_binary(&unstructured, 10_000).unwrap();
        assert_eq!(title, "Binary legacy mesh");
        assert_eq!(nodes.len(), 4);
        assert_eq!(nodes[&0].scalar, Some(10.0));
        assert_eq!(cells.len(), 2);
        assert_eq!(cells[1].scalar, Some(8.0));
        assert!(
            warnings
                .iter()
                .any(|warning| warning.contains("non-scalar"))
        );

        let mut sink = DummySink(Vec::new());
        let conversion_warnings = convert(
            Cursor::new(unstructured.clone()),
            &ConvertOptions::default(),
            &mut sink,
        )
        .unwrap();
        assert!(
            conversion_warnings
                .iter()
                .any(|warning| warning.contains("color map uses cell values"))
        );
        assert!(sink.0[0].nodes.iter().any(|node| matches!(
            node,
            Node::Text { id, .. } if id == "colorbar-max"
        )));

        let polydata = vtk_legacy_binary_polydata_fixture();
        let (nodes, cells, title, warnings) = parse_vtk_binary(&polydata, 10_000).unwrap();
        assert_eq!(title, "Binary PolyData");
        assert_eq!(nodes.len(), 4);
        assert_eq!(cells.len(), 2);
        assert_eq!(cells[0].node_ids, vec![0, 1]);
        assert_eq!(cells[0].scalar, Some(3.0));
        assert_eq!(cells[1].scalar, Some(9.0));
        assert!(warnings.is_empty());

        let truncated = &polydata[..polydata.len() - 3];
        assert!(matches!(
            parse_vtk_binary(truncated, 10_000),
            Err(Error::InvalidInput(_))
        ));

        let oversized =
            b"# vtk DataFile Version 3.0\nmesh\nBINARY\nDATASET POLYDATA\nPOINTS 1000001 float\n";
        assert!(matches!(
            parse_vtk_binary(oversized, 10_000),
            Err(Error::LimitExceeded(_))
        ));

        let platform_width =
            b"# vtk DataFile Version 3.0\nmesh\nBINARY\nDATASET POLYDATA\nPOINTS 1 long\n";
        assert!(matches!(
            parse_vtk_binary(platform_width, 10_000),
            Err(Error::Unsupported(_))
        ));
    }

    #[test]
    fn parses_ascii_vtk_xml_unstructured_grid_and_active_scalars() {
        let vtu = r#"<?xml version="1.0"?>
<VTKFile type="UnstructuredGrid" version="1.0" byte_order="LittleEndian">
  <UnstructuredGrid><Piece NumberOfPoints="4" NumberOfCells="2">
    <PointData Scalars="temperature"><DataArray type="Float32" Name="temperature" format="ascii">10 20 30 40</DataArray></PointData>
    <CellData Scalars="stress"><DataArray type="Float32" Name="stress" format="ascii">2 8</DataArray></CellData>
    <Points><DataArray type="Float32" NumberOfComponents="3" format="ascii">0 0 0  100 0 0  100 100 0  0 100 0</DataArray></Points>
    <Cells>
      <DataArray type="Int32" Name="connectivity" format="ascii">0 1 2  0 2 3</DataArray>
      <DataArray type="Int32" Name="offsets" format="ascii">3 6</DataArray>
      <DataArray type="UInt8" Name="types" format="ascii">5 5</DataArray>
    </Cells>
  </Piece></UnstructuredGrid>
</VTKFile>"#;
        let (nodes, cells, title, warnings) = parse_vtk_xml(vtu, 10_000).unwrap();
        assert_eq!(title, "VTK XML UnstructuredGrid");
        assert_eq!(nodes.len(), 4);
        assert_eq!(nodes.get(&0).unwrap().scalar, Some(10.0));
        assert_eq!(cells.len(), 2);
        assert_eq!(cells[1].scalar, Some(8.0));
        assert!(warnings.is_empty());
    }

    #[test]
    fn parses_ascii_vtk_xml_polydata_lines_and_polygons() {
        let vtp = r#"<VTKFile type="PolyData" version="1.0" byte_order="LittleEndian">
  <PolyData><Piece NumberOfPoints="4" NumberOfVerts="0" NumberOfLines="1" NumberOfStrips="0" NumberOfPolys="1">
    <PointData Scalars="temperature"><DataArray type="Float64" Name="temperature" format="ascii">0 1 2 3</DataArray></PointData>
    <CellData Scalars="value"><DataArray type="Float64" Name="value" format="ascii">4 9</DataArray></CellData>
    <Points><DataArray type="Float32" NumberOfComponents="3" format="ascii">0 0 0  1 0 0  1 1 0  0 1 0</DataArray></Points>
    <Lines><DataArray type="Int32" Name="connectivity" format="ascii">0 1 2</DataArray><DataArray type="Int32" Name="offsets" format="ascii">3</DataArray></Lines>
    <Polys><DataArray type="Int32" Name="connectivity" format="ascii">0 1 2 3</DataArray><DataArray type="Int32" Name="offsets" format="ascii">4</DataArray></Polys>
  </Piece></PolyData>
</VTKFile>"#;
        let (nodes, cells, title, warnings) = parse_vtk_xml(vtp, 10_000).unwrap();
        assert_eq!(title, "VTK XML PolyData");
        assert_eq!(nodes.len(), 4);
        assert_eq!(cells.len(), 3);
        assert_eq!(cells[0].node_ids.len(), 2);
        assert_eq!(cells[0].scalar, Some(4.0));
        assert_eq!(cells[1].node_ids.len(), 2);
        assert_eq!(cells[1].scalar, Some(4.0));
        assert_eq!(cells[2].node_ids.len(), 4);
        assert_eq!(cells[2].scalar, Some(9.0));
        assert!(warnings.is_empty());
    }

    #[test]
    fn parses_vtk_xml_image_data_extent_geometry_and_multicomponent_scalars() {
        let vti = r#"<VTKFile type="ImageData" version="1.0" byte_order="LittleEndian">
  <ImageData WholeExtent="1 3 4 5 0 0" Origin="10 20 0" Spacing="2 3 1" Direction="0 -1 0 1 0 0 0 0 1">
    <Piece Extent="1 3 4 5 0 0">
      <PointData Scalars="velocity"><DataArray type="Float32" Name="velocity" NumberOfComponents="3" format="ascii">3 4 0 5 12 0 8 15 0 3 4 0 5 12 0 8 15 0</DataArray></PointData>
      <CellData Scalars="pressure"><DataArray type="Float32" Name="pressure" format="ascii">7 9</DataArray></CellData>
    </Piece>
  </ImageData>
</VTKFile>"#;
        let (nodes, cells, title, warnings) = parse_vtk_xml(vti, 10_000).unwrap();
        assert_eq!(title, "VTK XML ImageData");
        assert_eq!(nodes.len(), 6);
        assert_eq!((nodes[&0].x, nodes[&0].y), (-2.0, 22.0));
        assert_eq!((nodes[&5].x, nodes[&5].y), (-5.0, 26.0));
        assert_eq!(nodes[&0].scalar, Some(5.0));
        assert_eq!(nodes[&1].scalar, Some(13.0));
        assert_eq!(cells.len(), 2);
        assert_eq!(cells[0].node_ids, vec![0, 1, 4, 3]);
        assert_eq!(cells[0].scalar, Some(7.0));
        assert_eq!(cells[1].scalar, Some(9.0));
        assert!(
            warnings
                .iter()
                .any(|warning| warning.contains("Euclidean magnitude"))
        );
    }

    #[test]
    fn rejects_invalid_vtk_xml_image_data_extents_and_array_counts() {
        let inverted_extent = r#"<VTKFile type="ImageData"><ImageData WholeExtent="0 1 0 1 0 0" Origin="0 0 0" Spacing="1 1 1"><Piece Extent="0 2 0 1 0 0"/></ImageData></VTKFile>"#;
        assert!(matches!(
            parse_vtk_xml(inverted_extent, 10_000),
            Err(Error::InvalidInput(_))
        ));

        let wrong_count = r#"<VTKFile type="ImageData"><ImageData WholeExtent="0 2 0 1 0 0"><Piece Extent="0 2 0 1 0 0"><PointData Scalars="value"><DataArray type="Float32" Name="value" format="ascii">1 2</DataArray></PointData></Piece></ImageData></VTKFile>"#;
        assert!(matches!(
            parse_vtk_xml(wrong_count, 10_000),
            Err(Error::InvalidInput(_))
        ));
    }

    #[test]
    fn parses_vtk_xml_rectilinear_grid_coordinates_and_cell_scalars() {
        let vtr = r#"<VTKFile type="RectilinearGrid" version="1.0" byte_order="LittleEndian">
  <RectilinearGrid WholeExtent="0 2 0 1 0 0">
    <Piece Extent="0 2 0 1 0 0">
      <CellData Scalars="pressure"><DataArray type="Float32" Name="pressure" format="ascii">2 8</DataArray></CellData>
      <Coordinates>
        <DataArray type="Float64" format="ascii">0 2 5</DataArray>
        <DataArray type="Float64" format="ascii">10 12</DataArray>
        <DataArray type="Float64" format="ascii">3</DataArray>
      </Coordinates>
    </Piece>
  </RectilinearGrid>
</VTKFile>"#;
        let (nodes, cells, title, warnings) = parse_vtk_xml(vtr, 10_000).unwrap();
        assert_eq!(title, "VTK XML RectilinearGrid");
        assert_eq!(nodes.len(), 6);
        assert_eq!((nodes[&1].x, nodes[&1].y), (2.0, 10.0));
        assert_eq!((nodes[&5].x, nodes[&5].y), (5.0, 12.0));
        assert_eq!(cells.len(), 2);
        assert_eq!(cells[0].scalar, Some(2.0));
        assert_eq!(cells[1].scalar, Some(8.0));
        assert!(warnings.is_empty());
    }

    #[test]
    fn parses_vtk_xml_structured_grid_explicit_points_and_volume_wireframe() {
        let vts = r#"<VTKFile type="StructuredGrid" version="1.0" byte_order="LittleEndian">
  <StructuredGrid WholeExtent="0 1 0 1 0 1">
    <Piece Extent="0 1 0 1 0 1">
      <CellData Scalars="stress"><DataArray type="Float32" Name="stress" format="ascii">42</DataArray></CellData>
      <Points><DataArray type="Float32" NumberOfComponents="3" format="ascii">0 0 0 2 0 0 0 3 0 2 3 0 0 0 4 2 0 4 0 3 4 2 3 4</DataArray></Points>
    </Piece>
  </StructuredGrid>
</VTKFile>"#;
        let (nodes, cells, title, warnings) = parse_vtk_xml(vts, 10_000).unwrap();
        assert_eq!(title, "VTK XML StructuredGrid");
        assert_eq!(nodes.len(), 8);
        assert_eq!((nodes[&7].x, nodes[&7].y), (2.0, 3.0));
        assert_eq!(cells.len(), 12);
        assert!(cells.iter().all(|cell| cell.scalar == Some(42.0)));
        assert!(warnings.is_empty());
    }

    #[test]
    fn rejects_vtk_xml_binary_arrays_and_excessive_event_counts() {
        let binary = r#"<VTKFile type="UnstructuredGrid" compressor="vtkLZMADataCompressor"><UnstructuredGrid><Piece><Points><DataArray type="Float32" NumberOfComponents="3" format="appended" offset="0"/></Points></Piece></UnstructuredGrid><AppendedData encoding="base64">_</AppendedData></VTKFile>"#;
        assert!(matches!(
            parse_vtk_xml(binary, 10_000),
            Err(Error::Unsupported(_))
        ));
        assert!(matches!(
            parse_vtk_xml("<VTKFile type=\"UnstructuredGrid\"/>", 1),
            Err(Error::LimitExceeded(_))
        ));
    }

    #[test]
    fn parses_vtk_xml_base64_appended_binary_with_and_without_zlib() {
        for compressed in [false, true] {
            let xml = vtu_binary_fixture(compressed, true);
            let (nodes, cells, _, _) = parse_vtk_xml(&xml, 10_000).unwrap();
            assert_eq!(nodes.len(), 4);
            assert_eq!(nodes.get(&0).unwrap().scalar, Some(10.0));
            assert_eq!(cells.len(), 2);
            assert_eq!(cells[1].scalar, Some(8.0));
        }
    }

    #[test]
    fn parses_raw_vtk_xml_appended_binary_with_non_utf8_payload() {
        for compressed in [false, true] {
            let bytes = vtu_raw_appended_fixture(compressed);
            let (xml, appended_range) = prepare_vtk_xml_raw_appended(&bytes, 10_000)
                .unwrap()
                .expect("raw appended section should be detected");
            let appended = &bytes[appended_range];
            assert!(appended.contains(&0xff));
            let (nodes, cells, _, _) =
                parse_vtk_xml_with_appended(&xml, 10_000, Some(appended)).unwrap();
            assert_eq!(nodes.len(), 4);
            assert_eq!(nodes.get(&0).unwrap().scalar, Some(10.0));
            assert_eq!(cells.len(), 2);
            assert_eq!(cells[1].scalar, Some(8.0));

            let mut sink = DummySink(Vec::new());
            convert(Cursor::new(bytes), &ConvertOptions::default(), &mut sink).unwrap();
            assert_eq!(sink.0.len(), 1);
        }
    }

    #[test]
    fn renders_vtk_xml_zlib_appended_arrays_through_the_page_consumer() {
        let mut sink = DummySink(Vec::new());
        let warnings = convert(
            Cursor::new(vtu_binary_fixture(true, true)),
            &ConvertOptions::default(),
            &mut sink,
        )
        .unwrap();
        assert!(
            warnings
                .iter()
                .any(|warning| warning.contains("color map uses cell values"))
        );
        assert_eq!(sink.0.len(), 1);
        assert!(sink.0[0].nodes.iter().any(|node| matches!(
            node,
            Node::Text { id, .. } if id == "colorbar-max"
        )));
    }

    #[test]
    fn decodes_vtk_uint64_big_endian_length_headers() {
        let mut encoded = 4u64.to_be_bytes().to_vec();
        encoded.extend_from_slice(&[1, 2, 3, 4]);
        let (decoded, consumed) =
            decode_vtk_binary_payload(&encoded, VtkHeaderType::UInt64, VtkByteOrder::Big, None)
                .unwrap();
        assert_eq!(decoded, [1, 2, 3, 4]);
        assert_eq!(consumed, encoded.len());
    }

    #[test]
    fn parses_vtk_xml_inline_base64_binary() {
        let xml = vtu_binary_fixture(false, false);
        let (nodes, cells, _, _) = parse_vtk_xml(&xml, 10_000).unwrap();
        assert_eq!(nodes.len(), 4);
        assert_eq!(cells.len(), 2);
    }

    #[test]
    fn legacy_vtk_empty_cell_does_not_panic() {
        let vtk = "# vtk DataFile Version 3.0\nempty cell\nASCII\nDATASET UNSTRUCTURED_GRID\nPOINTS 1 float\n0 0 0\nCELLS 1 1\n0\n";
        let (nodes, cells, _, _) = parse_vtk(vtk, 10_000).unwrap();
        assert_eq!(nodes.len(), 1);
        assert!(cells.is_empty());
    }

    #[test]
    fn bounds_and_validates_legacy_vtk_points_before_rendering() {
        let oversized =
            "# vtk DataFile Version 3.0\nmesh\nASCII\nDATASET POLYDATA\nPOINTS 1000001 float\n";
        assert!(matches!(
            parse_vtk(oversized, 10_000),
            Err(Error::LimitExceeded(_))
        ));
        let non_finite =
            "# vtk DataFile Version 3.0\nmesh\nASCII\nDATASET POLYDATA\nPOINTS 1 float\nNaN 0 0\n";
        assert!(matches!(
            parse_vtk(non_finite, 10_000),
            Err(Error::InvalidInput(_))
        ));
        let cell_mismatch = "# vtk DataFile Version 3.0\nmesh\nASCII\nDATASET POLYDATA\nPOINTS 3 float\n0 0 0\n1 0 0\n0 1 0\nPOLYGONS 1 3\n3 0 1\n";
        assert!(matches!(
            parse_vtk(cell_mismatch, 10_000),
            Err(Error::InvalidInput(_))
        ));
    }
}
