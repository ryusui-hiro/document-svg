use std::collections::HashMap;
use std::io::Read;

use crate::cad::dxf::geometry::parse_cad_float;
use crate::convert::{ConvertOptions, PageConsumer};
use crate::error::{Error, Result};

use super::{
    MAX_SIMULATION_CELLS, MAX_SIMULATION_POINTS, MAX_VTK_RENDERED_PRIMITIVES, MAX_VTK_TOTAL_VALUES,
    MeshCell, MeshNode, SimulationParseOutput, render_simulation, vtk_cell_primitives,
};

const MAX_MEDIT_BYTES: u64 = 256 * 1024 * 1024;
const MAX_MEDIT_LINES: usize = 5_000_000;
const MAX_MEDIT_LINE_BYTES: usize = 1024 * 1024;
const MAX_MEDIT_BINARY_KEYWORDS: usize = 1_000_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ByteOrder {
    Little,
    Big,
}

struct BinaryReader<'a> {
    bytes: &'a [u8],
    position: usize,
    order: ByteOrder,
    version: usize,
}

impl BinaryReader<'_> {
    fn take(&mut self, size: usize, context: &str) -> Result<&[u8]> {
        let end = self
            .position
            .checked_add(size)
            .ok_or_else(|| Error::LimitExceeded(format!("MEDIT {context} offset overflowed")))?;
        let bytes = self
            .bytes
            .get(self.position..end)
            .ok_or_else(|| Error::InvalidInput(format!("MEDIT binary {context} is truncated")))?;
        self.position = end;
        Ok(bytes)
    }

    fn i32(&mut self, context: &str) -> Result<i32> {
        let raw: [u8; 4] = self
            .take(4, context)?
            .try_into()
            .map_err(|_| Error::InvalidInput(format!("invalid MEDIT binary {context}")))?;
        Ok(match self.order {
            ByteOrder::Little => i32::from_le_bytes(raw),
            ByteOrder::Big => i32::from_be_bytes(raw),
        })
    }

    fn i64(&mut self, context: &str) -> Result<i64> {
        let raw: [u8; 8] = self
            .take(8, context)?
            .try_into()
            .map_err(|_| Error::InvalidInput(format!("invalid MEDIT binary {context}")))?;
        Ok(match self.order {
            ByteOrder::Little => i64::from_le_bytes(raw),
            ByteOrder::Big => i64::from_be_bytes(raw),
        })
    }

    fn integer(&mut self, context: &str) -> Result<i64> {
        if self.version >= 4 {
            self.i64(context)
        } else {
            Ok(i64::from(self.i32(context)?))
        }
    }

    fn offset(&mut self, context: &str) -> Result<u64> {
        let offset = if self.version >= 3 {
            self.i64(context)?
        } else {
            i64::from(self.i32(context)?)
        };
        u64::try_from(offset)
            .map_err(|_| Error::InvalidInput(format!("MEDIT binary {context} is negative")))
    }

    fn real(&mut self, context: &str) -> Result<f64> {
        let value = if self.version == 1 {
            let raw: [u8; 4] = self
                .take(4, context)?
                .try_into()
                .map_err(|_| Error::InvalidInput(format!("invalid MEDIT binary {context}")))?;
            match self.order {
                ByteOrder::Little => f32::from_le_bytes(raw) as f64,
                ByteOrder::Big => f32::from_be_bytes(raw) as f64,
            }
        } else {
            let raw: [u8; 8] = self
                .take(8, context)?
                .try_into()
                .map_err(|_| Error::InvalidInput(format!("invalid MEDIT binary {context}")))?;
            match self.order {
                ByteOrder::Little => f64::from_le_bytes(raw),
                ByteOrder::Big => f64::from_be_bytes(raw),
            }
        };
        if !value.is_finite() || value.abs() > 1.0e12 {
            return Err(Error::InvalidInput(format!(
                "MEDIT binary {context} is non-finite or outside ±1e12"
            )));
        }
        Ok(value)
    }

    fn count(&mut self, context: &str, maximum: usize) -> Result<usize> {
        let value = self.integer(context)?;
        let value = usize::try_from(value)
            .map_err(|_| Error::InvalidInput(format!("invalid MEDIT binary {context}")))?;
        if value > maximum {
            return Err(Error::LimitExceeded(format!(
                "MEDIT binary {context} count exceeds {maximum}"
            )));
        }
        Ok(value)
    }

    fn positive_index(&mut self, context: &str) -> Result<usize> {
        let value = self.integer(context)?;
        usize::try_from(value)
            .ok()
            .filter(|value| *value > 0)
            .ok_or_else(|| Error::InvalidInput(format!("MEDIT binary {context} must be positive")))
    }
}

struct Tokenizer<'a> {
    lines: std::str::Lines<'a>,
    current: std::str::SplitWhitespace<'a>,
    line_number: usize,
}

#[derive(Default)]
struct MeshData {
    nodes: HashMap<usize, MeshNode>,
    cells: Vec<MeshCell>,
    total_elements: usize,
}

impl<'a> Tokenizer<'a> {
    fn new(text: &'a str) -> Self {
        Self {
            lines: text.lines(),
            current: "".split_whitespace(),
            line_number: 0,
        }
    }

    fn next(&mut self) -> Result<Option<(&'a str, usize)>> {
        loop {
            if let Some(token) = self.current.next() {
                return Ok(Some((token, self.line_number)));
            }
            let Some(line) = self.lines.next() else {
                return Ok(None);
            };
            self.line_number += 1;
            if self.line_number > MAX_MEDIT_LINES {
                return Err(Error::LimitExceeded(format!(
                    "MEDIT input exceeds {MAX_MEDIT_LINES} lines"
                )));
            }
            if line.len() > MAX_MEDIT_LINE_BYTES {
                return Err(Error::LimitExceeded(format!(
                    "MEDIT line {} exceeds {MAX_MEDIT_LINE_BYTES} bytes",
                    self.line_number
                )));
            }
            self.current = line
                .split('#')
                .next()
                .unwrap_or_default()
                .split_whitespace();
        }
    }

    fn required(&mut self, context: &str) -> Result<(&'a str, usize)> {
        self.next()?
            .ok_or_else(|| Error::InvalidInput(format!("MEDIT input ended before {context}")))
    }

    fn count(&mut self, context: &str, maximum: usize) -> Result<usize> {
        let (value, line_number) = self.required(context)?;
        let count = value.parse::<usize>().map_err(|_| {
            Error::InvalidInput(format!(
                "invalid MEDIT {context} count '{value}' at line {line_number}"
            ))
        })?;
        if count > maximum {
            return Err(Error::LimitExceeded(format!(
                "MEDIT {context} count exceeds {maximum}"
            )));
        }
        Ok(count)
    }
}

pub(super) fn convert<R: Read>(
    input: R,
    options: &ConvertOptions,
    sink: &mut dyn PageConsumer,
) -> Result<Vec<String>> {
    let max_bytes = options.max_input_bytes.min(MAX_MEDIT_BYTES);
    let mut bytes = Vec::new();
    Read::take(input, max_bytes.saturating_add(1)).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > max_bytes {
        return Err(Error::LimitExceeded(format!(
            "MEDIT input exceeds maximum limit of {max_bytes} bytes"
        )));
    }
    let parsed = if looks_like_binary_medit(&bytes) {
        parse_medit_binary(&bytes)?
    } else {
        let text = String::from_utf8(bytes).map_err(|error| {
            Error::Unsupported(format!(
                "MEDIT input is neither recognized binary meshb nor UTF-8 ASCII: {error}"
            ))
        })?;
        parse_medit(&text)?
    };
    render_simulation(parsed, sink)
}

pub(crate) fn looks_like_binary_medit(bytes: &[u8]) -> bool {
    if bytes.len() < 20 {
        return false;
    }
    let Some(marker) = bytes.get(..4) else {
        return false;
    };
    let order = match marker {
        [1, 0, 0, 0] => ByteOrder::Little,
        [0, 0, 0, 1] => ByteOrder::Big,
        _ => return false,
    };
    let read_i32 = |offset: usize| -> Option<i32> {
        let raw: [u8; 4] = bytes.get(offset..offset + 4)?.try_into().ok()?;
        Some(match order {
            ByteOrder::Little => i32::from_le_bytes(raw),
            ByteOrder::Big => i32::from_be_bytes(raw),
        })
    };
    let version = read_i32(4);
    if !version.is_some_and(|version| (1..=4).contains(&version)) || read_i32(8) != Some(3) {
        return false;
    }
    let position_width = if version.unwrap_or_default() >= 3 {
        8
    } else {
        4
    };
    let dimension_offset = 12 + position_width;
    read_i32(dimension_offset).is_some_and(|dimension| matches!(dimension, 2 | 3))
}

fn parse_medit_binary(bytes: &[u8]) -> Result<SimulationParseOutput> {
    let order = match bytes.get(..4) {
        Some([1, 0, 0, 0]) => ByteOrder::Little,
        Some([0, 0, 0, 1]) => ByteOrder::Big,
        _ => {
            return Err(Error::InvalidInput(
                "MEDIT binary mesh has an invalid endian marker".into(),
            ));
        }
    };
    let mut reader = BinaryReader {
        bytes,
        position: 4,
        order,
        version: 0,
    };
    let version = reader.i32("version")?;
    if !(1..=4).contains(&version) {
        return Err(Error::Unsupported(format!(
            "MEDIT binary version {version} is unsupported; versions 1–4 are supported"
        )));
    }
    reader.version = version as usize;
    if reader.i32("dimension keyword")? != 3 {
        return Err(Error::InvalidInput(
            "MEDIT binary header is missing the Dimension keyword".into(),
        ));
    }
    let next_position = reader.offset("Dimension next-keyword offset")?;
    let dimension = usize::try_from(reader.integer("dimension")?)
        .map_err(|_| Error::InvalidInput("invalid MEDIT binary dimension".into()))?;
    if !matches!(dimension, 2 | 3) {
        return Err(Error::Unsupported(format!(
            "MEDIT dimension {dimension} is unsupported; use dimension 2 or 3"
        )));
    }
    let next_usize = usize::try_from(next_position)
        .map_err(|_| Error::LimitExceeded("MEDIT keyword offset exceeds this platform".into()))?;
    if next_usize < reader.position || next_usize > bytes.len() {
        return Err(Error::InvalidInput(
            "MEDIT binary Dimension next-keyword offset is outside the input".into(),
        ));
    }
    reader.position = next_usize;

    let mut mesh = MeshData::default();
    let mut vertices_seen = false;
    let mut end_seen = false;
    let mut has_nonzero_z = false;
    let mut skipped_nonmesh_sections = false;
    let mut keyword_count = 0usize;
    while reader.position < bytes.len() {
        keyword_count += 1;
        if keyword_count > MAX_MEDIT_BINARY_KEYWORDS {
            return Err(Error::LimitExceeded(format!(
                "MEDIT binary input exceeds {MAX_MEDIT_BINARY_KEYWORDS} keyword sections"
            )));
        }
        let keyword = reader.i32("keyword code")?;
        let next_position = reader.offset("next-keyword offset")?;
        if keyword <= 0 {
            return Err(Error::InvalidInput(
                "MEDIT binary keyword code must be positive".into(),
            ));
        }
        if keyword == 54 {
            if next_position != 0 {
                return Err(Error::InvalidInput(
                    "MEDIT binary End keyword has a nonzero next offset".into(),
                ));
            }
            end_seen = true;
            break;
        }
        if next_position == 0 {
            return Err(Error::InvalidInput(
                "MEDIT binary keyword chain ended before End".into(),
            ));
        }
        let next_usize = usize::try_from(next_position).map_err(|_| {
            Error::LimitExceeded("MEDIT keyword offset exceeds this platform".into())
        })?;
        if next_usize < reader.position || next_usize > bytes.len() {
            return Err(Error::InvalidInput(format!(
                "MEDIT binary keyword {keyword} has a backward or out-of-file offset"
            )));
        }
        match keyword {
            4 => {
                if vertices_seen {
                    return Err(Error::InvalidInput(
                        "MEDIT binary file contains multiple Vertices sections".into(),
                    ));
                }
                vertices_seen = true;
                let count = reader.count("vertex", MAX_SIMULATION_POINTS)?;
                mesh.nodes.reserve(count);
                for id in 1..=count {
                    let x = reader.real("vertex X coordinate")?;
                    let y = reader.real("vertex Y coordinate")?;
                    let z = if dimension == 3 {
                        reader.real("vertex Z coordinate")?
                    } else {
                        0.0
                    };
                    let _reference = reader.integer("vertex reference")?;
                    has_nonzero_z |= z.abs() > 1.0e-12;
                    mesh.nodes.insert(id, MeshNode { x, y, scalar: None });
                }
                if reader.position != next_usize {
                    return Err(Error::InvalidInput(
                        "MEDIT binary Vertices records do not match their next-keyword offset"
                            .into(),
                    ));
                }
            }
            5 | 6 | 7 | 8 | 9 | 10 | 49 => {
                if !vertices_seen {
                    return Err(Error::InvalidInput(
                        "MEDIT binary element section appears before Vertices".into(),
                    ));
                }
                let (section, nodes_per_element, vtk_type) = match keyword {
                    5 => ("Edges", 2, 3),
                    6 => ("Triangles", 3, 5),
                    7 => ("Quadrilaterals", 4, 9),
                    8 => ("Tetrahedra", 4, 10),
                    9 => ("Prisms", 6, 13),
                    10 => ("Hexahedra", 8, 12),
                    49 => ("Pyramids", 5, 14),
                    _ => unreachable!(),
                };
                let count = reader.count(&format!("{section} element"), MAX_SIMULATION_CELLS)?;
                mesh.total_elements = mesh
                    .total_elements
                    .checked_add(count)
                    .ok_or_else(|| Error::LimitExceeded("MEDIT element count overflowed".into()))?;
                if mesh.total_elements > MAX_SIMULATION_CELLS {
                    return Err(Error::LimitExceeded(format!(
                        "MEDIT element count exceeds {MAX_SIMULATION_CELLS}"
                    )));
                }
                for element_id in 1..=count {
                    let mut node_ids = Vec::with_capacity(nodes_per_element);
                    for _ in 0..nodes_per_element {
                        let node_id = reader.positive_index(&format!("{section} vertex index"))?;
                        if !mesh.nodes.contains_key(&node_id) {
                            return Err(Error::InvalidInput(format!(
                                "MEDIT {section} element {element_id} references unknown vertex {node_id}"
                            )));
                        }
                        node_ids.push(node_id);
                    }
                    let _reference = reader.integer(&format!("{section} reference"))?;
                    let primitives = vtk_cell_primitives(vtk_type, &node_ids, None)?;
                    if mesh
                        .cells
                        .len()
                        .checked_add(primitives.len())
                        .is_none_or(|new_count| new_count > MAX_VTK_RENDERED_PRIMITIVES)
                    {
                        return Err(Error::LimitExceeded(format!(
                            "MEDIT rendered primitive count exceeds {MAX_VTK_RENDERED_PRIMITIVES}"
                        )));
                    }
                    mesh.cells.extend(primitives);
                }
                if reader.position != next_usize {
                    return Err(Error::InvalidInput(format!(
                        "MEDIT binary {section} records do not match their next-keyword offset"
                    )));
                }
            }
            13..=18 => {} // Corner, ridge, and required-entity marker lists.
            _ => skipped_nonmesh_sections = true,
        }
        if reader.position > next_usize {
            return Err(Error::InvalidInput(format!(
                "MEDIT binary keyword {keyword} exceeds its declared record span"
            )));
        }
        reader.position = next_usize;
    }

    if !end_seen {
        return Err(Error::InvalidInput(
            "MEDIT binary file is missing the End keyword".into(),
        ));
    }
    if reader.position != bytes.len() {
        return Err(Error::InvalidInput(
            "MEDIT binary file contains data after its End keyword".into(),
        ));
    }
    if mesh.nodes.is_empty() || mesh.cells.is_empty() {
        return Err(Error::Unsupported(
            "MEDIT file has no supported Vertices and element sections".into(),
        ));
    }
    let mut warnings = Vec::new();
    if skipped_nonmesh_sections {
        warnings.push(
            "MEDIT binary non-rendered, high-order, or solution sections were skipped".into(),
        );
    }
    if has_nonzero_z {
        warnings.push(
            "MEDIT 3D coordinates are shown in the XY projection; Z depth is not represented"
                .into(),
        );
    }
    Ok((
        mesh.nodes,
        mesh.cells,
        "MEDIT binary finite-element mesh".into(),
        warnings,
    ))
}

fn parse_medit(text: &str) -> Result<SimulationParseOutput> {
    let mut tokens = Tokenizer::new(text);
    let mut version = None;
    let mut dimension = None;
    let mut mesh = MeshData::default();
    let mut section_counts = HashMap::<String, usize>::new();
    let mut warnings = Vec::new();
    let mut has_nonzero_z = false;
    let mut saw_end = false;

    while let Some((keyword, line_number)) = tokens.next()? {
        match keyword.to_ascii_lowercase().as_str() {
            "meshversionformatted" => {
                let (value, version_line) = tokens.required("MeshVersionFormatted value")?;
                let parsed = value.parse::<usize>().map_err(|_| {
                    Error::InvalidInput(format!(
                        "invalid MEDIT mesh version '{value}' at line {version_line}"
                    ))
                })?;
                if !(1..=5).contains(&parsed) {
                    return Err(Error::Unsupported(format!(
                        "MEDIT ASCII mesh version {parsed} is unsupported"
                    )));
                }
                if version.replace(parsed).is_some() {
                    return Err(Error::InvalidInput(
                        "MEDIT file contains multiple MeshVersionFormatted headers".into(),
                    ));
                }
            }
            "dimension" => {
                let (value, dimension_line) = tokens.required("Dimension value")?;
                let parsed = value.parse::<usize>().map_err(|_| {
                    Error::InvalidInput(format!(
                        "invalid MEDIT Dimension '{value}' at line {dimension_line}"
                    ))
                })?;
                if !matches!(parsed, 2 | 3) {
                    return Err(Error::Unsupported(format!(
                        "MEDIT dimension {parsed} is unsupported; use dimension 2 or 3"
                    )));
                }
                if dimension.replace(parsed).is_some() {
                    return Err(Error::InvalidInput(
                        "MEDIT file contains multiple Dimension headers".into(),
                    ));
                }
            }
            "vertices" => {
                if section_counts.contains_key("vertices") {
                    return Err(Error::InvalidInput(
                        "MEDIT file contains multiple Vertices sections".into(),
                    ));
                }
                let dimension = dimension.ok_or_else(|| {
                    Error::InvalidInput("MEDIT Dimension must precede Vertices".into())
                })?;
                let count = tokens.count("vertex", MAX_SIMULATION_POINTS)?;
                mesh.nodes.reserve(count);
                for id in 1..=count {
                    let (x, x_line) = tokens.required("vertex X coordinate")?;
                    let (y, _) = tokens.required("vertex Y coordinate")?;
                    let z = if dimension == 3 {
                        let (z, _) = tokens.required("vertex Z coordinate")?;
                        parse_real(z, "vertex Z", x_line)?
                    } else {
                        0.0
                    };
                    let reference = tokens.required("vertex reference")?;
                    let x = parse_real(x, "vertex X", x_line)?;
                    let y = parse_real(y, "vertex Y", x_line)?;
                    parse_reference(reference.0, "vertex", reference.1)?;
                    has_nonzero_z |= z.abs() > 1.0e-12;
                    mesh.nodes.insert(id, MeshNode { x, y, scalar: None });
                }
                section_counts.insert("vertices".into(), count);
            }
            "edges" => parse_elements(&mut tokens, "Edges", 2, 3, &mut mesh, &mut section_counts)?,
            "triangles" => parse_elements(
                &mut tokens,
                "Triangles",
                3,
                5,
                &mut mesh,
                &mut section_counts,
            )?,
            "quadrilaterals" => parse_elements(
                &mut tokens,
                "Quadrilaterals",
                4,
                9,
                &mut mesh,
                &mut section_counts,
            )?,
            "tetrahedra" => parse_elements(
                &mut tokens,
                "Tetrahedra",
                4,
                10,
                &mut mesh,
                &mut section_counts,
            )?,
            "pyramids" => parse_elements(
                &mut tokens,
                "Pyramids",
                5,
                14,
                &mut mesh,
                &mut section_counts,
            )?,
            "prisms" | "pentahedra" => {
                parse_elements(&mut tokens, "Prisms", 6, 13, &mut mesh, &mut section_counts)?
            }
            "hexahedra" => parse_elements(
                &mut tokens,
                "Hexahedra",
                8,
                12,
                &mut mesh,
                &mut section_counts,
            )?,
            "corners"
            | "ridges"
            | "requiredvertices"
            | "requirededges"
            | "requiredtriangles"
            | "requiredquadrilaterals"
            | "requiredtetrahedra"
            | "requiredpyramids"
            | "requiredprisms"
            | "requiredpentahedra"
            | "requiredhexahedra" => {
                let key = keyword.to_ascii_lowercase();
                let count = tokens.count(&format!("{keyword} reference"), MAX_SIMULATION_POINTS)?;
                for _ in 0..count {
                    let (value, value_line) = tokens.required(&format!("{keyword} reference"))?;
                    parse_reference(value, keyword, value_line)?;
                }
                section_counts.insert(key, count);
            }
            "angleofcornerbound" => {
                let (value, value_line) = tokens.required("AngleOfCornerBound value")?;
                let _ = parse_real(value, "AngleOfCornerBound", value_line)?;
            }
            name if name.starts_with("solat") => {
                skip_solution_section(
                    &mut tokens,
                    keyword,
                    name.strip_prefix("solat").unwrap_or_default(),
                    &section_counts,
                    dimension.unwrap_or(3),
                    &mut warnings,
                )?;
            }
            "end" => {
                saw_end = true;
                break;
            }
            _ => {
                return Err(Error::Unsupported(format!(
                    "MEDIT section '{keyword}' at line {line_number} is unsupported"
                )));
            }
        }
    }

    if version.is_none() {
        return Err(Error::InvalidInput(
            "MEDIT file is missing MeshVersionFormatted".into(),
        ));
    }
    if dimension.is_none() {
        return Err(Error::InvalidInput(
            "MEDIT file is missing Dimension".into(),
        ));
    }
    if mesh.nodes.is_empty() || mesh.cells.is_empty() {
        return Err(Error::Unsupported(
            "MEDIT file has no supported Vertices and element sections".into(),
        ));
    }
    if !saw_end {
        warnings.push("MEDIT file has no End section; the mesh body was read to EOF".into());
    }
    if has_nonzero_z {
        warnings.push(
            "MEDIT 3D coordinates are shown in the XY projection; Z depth is not represented"
                .into(),
        );
    }
    Ok((
        mesh.nodes,
        mesh.cells,
        "MEDIT finite-element mesh".into(),
        warnings,
    ))
}

fn parse_elements(
    tokens: &mut Tokenizer<'_>,
    section: &str,
    node_count: usize,
    vtk_type: usize,
    mesh: &mut MeshData,
    section_counts: &mut HashMap<String, usize>,
) -> Result<()> {
    if mesh.nodes.is_empty() {
        return Err(Error::InvalidInput(format!(
            "MEDIT {section} section appears before Vertices"
        )));
    }
    let count = tokens.count(&format!("{section} element"), MAX_SIMULATION_CELLS)?;
    mesh.total_elements = mesh
        .total_elements
        .checked_add(count)
        .ok_or_else(|| Error::LimitExceeded("MEDIT element count overflowed".into()))?;
    if mesh.total_elements > MAX_SIMULATION_CELLS {
        return Err(Error::LimitExceeded(format!(
            "MEDIT element count exceeds {MAX_SIMULATION_CELLS}"
        )));
    }
    let entity_key = section.to_ascii_lowercase();
    let mut raw_elements = 0usize;
    for element_id in 1..=count {
        let mut node_ids = Vec::with_capacity(node_count);
        for _ in 0..node_count {
            let (value, line_number) = tokens.required(&format!("{section} connectivity"))?;
            let node_id = parse_positive_id(value, section, line_number)?;
            if !mesh.nodes.contains_key(&node_id) {
                return Err(Error::InvalidInput(format!(
                    "MEDIT {section} element {element_id} references unknown vertex {node_id}"
                )));
            }
            node_ids.push(node_id);
        }
        let (reference, reference_line) = tokens.required(&format!("{section} reference"))?;
        parse_reference(reference, section, reference_line)?;
        raw_elements += 1;
        let primitives = vtk_cell_primitives(vtk_type, &node_ids, None)?;
        if mesh
            .cells
            .len()
            .checked_add(primitives.len())
            .is_none_or(|count| count > MAX_VTK_RENDERED_PRIMITIVES)
        {
            return Err(Error::LimitExceeded(format!(
                "MEDIT rendered primitive count exceeds {MAX_VTK_RENDERED_PRIMITIVES}"
            )));
        }
        mesh.cells.extend(primitives);
    }
    let previous_count = section_counts.get(&entity_key).copied().unwrap_or_default();
    section_counts.insert(entity_key, previous_count.saturating_add(raw_elements));
    Ok(())
}

fn skip_solution_section(
    tokens: &mut Tokenizer<'_>,
    keyword: &str,
    entity_name: &str,
    section_counts: &HashMap<String, usize>,
    dimension: usize,
    warnings: &mut Vec<String>,
) -> Result<()> {
    let entity_key = entity_name.to_ascii_lowercase();
    let declared_entities = tokens.count(&format!("{keyword} entity"), MAX_SIMULATION_CELLS)?;
    let solutions = tokens.count(&format!("{keyword} solution"), 64)?;
    if solutions == 0 {
        return Err(Error::InvalidInput(format!(
            "MEDIT {keyword} section has no solution fields"
        )));
    }
    let mut values_per_entity = 0usize;
    for _ in 0..solutions {
        let (kind, line_number) = tokens.required(&format!("{keyword} solution type"))?;
        let kind = kind.parse::<usize>().map_err(|_| {
            Error::InvalidInput(format!(
                "invalid MEDIT {keyword} solution type '{kind}' at line {line_number}"
            ))
        })?;
        let components = match kind {
            1 => 1,
            2 => dimension,
            3 => dimension * (dimension + 1) / 2,
            4 => dimension * dimension,
            _ => {
                return Err(Error::Unsupported(format!(
                    "MEDIT solution type {kind} in {keyword} is unsupported"
                )));
            }
        };
        values_per_entity = values_per_entity.checked_add(components).ok_or_else(|| {
            Error::LimitExceeded("MEDIT solution component count overflowed".into())
        })?;
    }
    let value_count = declared_entities
        .checked_mul(values_per_entity)
        .filter(|count| *count <= MAX_VTK_TOTAL_VALUES)
        .ok_or_else(|| {
            Error::LimitExceeded("MEDIT solution section exceeds the value limit".into())
        })?;
    let expected = section_counts.get(&entity_key).copied();
    if expected.is_some_and(|count| count != declared_entities) {
        return Err(Error::InvalidInput(format!(
            "MEDIT {keyword} declares {declared_entities} entities but the mesh section has {}",
            expected.unwrap_or_default()
        )));
    }
    for _ in 0..value_count {
        let (value, line_number) = tokens.required(&format!("{keyword} value"))?;
        let _ = parse_real(value, keyword, line_number)?;
    }
    if !warnings
        .iter()
        .any(|warning| warning.contains("solution data"))
    {
        warnings.push(
            "MEDIT solution data sections are not rendered; only mesh geometry is shown".into(),
        );
    }
    Ok(())
}

fn parse_positive_id(value: &str, context: &str, line_number: usize) -> Result<usize> {
    let value = value.parse::<usize>().map_err(|_| {
        Error::InvalidInput(format!(
            "invalid MEDIT {context} index '{value}' at line {line_number}"
        ))
    })?;
    if value == 0 {
        return Err(Error::InvalidInput(format!(
            "MEDIT {context} index must be positive at line {line_number}"
        )));
    }
    Ok(value)
}

fn parse_reference(value: &str, context: &str, line_number: usize) -> Result<i64> {
    value.parse::<i64>().map_err(|_| {
        Error::InvalidInput(format!(
            "invalid MEDIT {context} reference '{value}' at line {line_number}"
        ))
    })
}

fn parse_real(value: &str, context: &str, line_number: usize) -> Result<f64> {
    let value = value.trim();
    let parsed = parse_cad_float(value).ok_or_else(|| {
        Error::InvalidInput(format!(
            "invalid MEDIT {context} value '{value}' at line {line_number}"
        ))
    })?;
    if !parsed.is_finite() || parsed.abs() > 1.0e12 {
        return Err(Error::InvalidInput(format!(
            "MEDIT {context} value is non-finite or outside ±1e12 at line {line_number}"
        )));
    }
    Ok(parsed)
}

#[cfg(test)]
mod tests {
    use super::{ByteOrder, parse_medit, parse_medit_binary};
    use crate::error::Error;

    fn write_i32(bytes: &mut Vec<u8>, order: ByteOrder, value: i32) {
        bytes.extend_from_slice(&match order {
            ByteOrder::Little => value.to_le_bytes(),
            ByteOrder::Big => value.to_be_bytes(),
        });
    }

    fn write_i64(bytes: &mut Vec<u8>, order: ByteOrder, value: i64) {
        bytes.extend_from_slice(&match order {
            ByteOrder::Little => value.to_le_bytes(),
            ByteOrder::Big => value.to_be_bytes(),
        });
    }

    fn write_int(bytes: &mut Vec<u8>, version: usize, order: ByteOrder, value: i64) {
        if version >= 4 {
            write_i64(bytes, order, value);
        } else {
            write_i32(bytes, order, value as i32);
        }
    }

    fn write_offset(bytes: &mut [u8], version: usize, order: ByteOrder, offset: usize) {
        if version >= 3 {
            let value = offset as i64;
            bytes[0..8].copy_from_slice(&match order {
                ByteOrder::Little => value.to_le_bytes(),
                ByteOrder::Big => value.to_be_bytes(),
            });
        } else {
            let value = offset as i32;
            bytes[0..4].copy_from_slice(&match order {
                ByteOrder::Little => value.to_le_bytes(),
                ByteOrder::Big => value.to_be_bytes(),
            });
        }
    }

    fn push_float(bytes: &mut Vec<u8>, version: usize, order: ByteOrder, value: f64) {
        if version == 1 {
            let value = value as f32;
            bytes.extend_from_slice(&match order {
                ByteOrder::Little => value.to_le_bytes(),
                ByteOrder::Big => value.to_be_bytes(),
            });
        } else {
            bytes.extend_from_slice(&match order {
                ByteOrder::Little => value.to_le_bytes(),
                ByteOrder::Big => value.to_be_bytes(),
            });
        }
    }

    fn begin_keyword(
        bytes: &mut Vec<u8>,
        version: usize,
        order: ByteOrder,
        code: i32,
        count: Option<i64>,
    ) -> usize {
        write_i32(bytes, order, code);
        let offset = bytes.len();
        bytes.resize(offset + if version >= 3 { 8 } else { 4 }, 0);
        if let Some(count) = count {
            write_int(bytes, version, order, count);
        }
        offset
    }

    fn finish_keyword(bytes: &mut [u8], version: usize, order: ByteOrder, patch: usize) {
        let next = bytes.len();
        write_offset(&mut bytes[patch..], version, order, next);
    }

    fn meshb_fixture(version: usize, order: ByteOrder) -> Vec<u8> {
        let mut bytes = Vec::new();
        write_i32(&mut bytes, order, 1);
        write_i32(&mut bytes, order, version as i32);

        let dimension = begin_keyword(&mut bytes, version, order, 3, None);
        write_int(&mut bytes, version, order, 3);
        finish_keyword(&mut bytes, version, order, dimension);

        let vertices = begin_keyword(&mut bytes, version, order, 4, Some(4));
        for [x, y, z] in [
            [0.0, 0.0, 0.0],
            [100.0, 0.0, 0.0],
            [0.0, 100.0, 0.0],
            [0.0, 0.0, 100.0],
        ] {
            for coordinate in [x, y, z] {
                push_float(&mut bytes, version, order, coordinate);
            }
            write_int(&mut bytes, version, order, 1);
        }
        finish_keyword(&mut bytes, version, order, vertices);

        let empty_edges = begin_keyword(&mut bytes, version, order, 5, Some(0));
        finish_keyword(&mut bytes, version, order, empty_edges);

        let triangles = begin_keyword(&mut bytes, version, order, 6, Some(2));
        for nodes in [[1, 2, 3], [1, 3, 4]] {
            for node in nodes {
                write_int(&mut bytes, version, order, node);
            }
            write_int(&mut bytes, version, order, 9);
        }
        finish_keyword(&mut bytes, version, order, triangles);

        let tetrahedra = begin_keyword(&mut bytes, version, order, 8, Some(1));
        for node in [1, 2, 3, 4] {
            write_int(&mut bytes, version, order, node);
        }
        write_int(&mut bytes, version, order, 7);
        finish_keyword(&mut bytes, version, order, tetrahedra);

        write_i32(&mut bytes, order, 54);
        if version >= 3 {
            write_i64(&mut bytes, order, 0);
        } else {
            write_i32(&mut bytes, order, 0);
        }
        bytes
    }

    #[test]
    fn parses_2d_and_3d_mesh_entities_and_reference_sections() {
        let source = "# MEDIT mesh example\nMeshVersionFormatted 2\nDimension 2\nVertices\n4\n0 0 1\n1 0 1\n1 1 1\n0 1 1\nEdges\n1\n1 2 4\nTriangles\n2\n1 2 3 9\n1 3 4 9\nCorners\n4\n1\n2\n3\n4\nEnd\n";
        let (nodes, cells, _, warnings) = parse_medit(source).unwrap();
        assert_eq!(nodes.len(), 4);
        assert_eq!(nodes[&3].x, 1.0);
        assert_eq!(nodes[&3].y, 1.0);
        assert_eq!(cells.len(), 3);
        assert!(warnings.is_empty());

        let tetra = "MeshVersionFormatted 1\nDimension 3\nVertices 4\n0 0 0 1\n1 0 0 1\n0 1 0 1\n0 0 1 1\nTetrahedra 1\n1 2 3 4 0\n";
        let (tet_nodes, tet_cells, _, warnings) = parse_medit(tetra).unwrap();
        assert_eq!(tet_nodes.len(), 4);
        assert_eq!(tet_cells.len(), 6);
        assert!(
            warnings
                .iter()
                .any(|warning| warning.contains("XY projection"))
        );
        assert!(warnings.iter().any(|warning| warning.contains("no End")));
    }

    #[test]
    fn skips_scalar_solution_arrays_and_rejects_bad_connectivity_or_versions() {
        let with_solution = "MeshVersionFormatted 2\nDimension 2\nVertices 3\n0 0 0\n1 0 0\n0 1 0\nTriangles 1\n1 2 3 0\nSolAtVertices 3\n1\n1\n0.1\n0.2\n0.3\nEnd\n";
        let (_, _, _, warnings) = parse_medit(with_solution).unwrap();
        assert!(
            warnings
                .iter()
                .any(|warning| warning.contains("solution data"))
        );
        let unknown = "MeshVersionFormatted 1\nDimension 2\nVertices 3\n0 0 0\n1 0 0\n0 1 0\nTriangles 1\n1 2 99 0\nEnd\n";
        assert!(matches!(parse_medit(unknown), Err(Error::InvalidInput(_))));
        assert!(matches!(
            parse_medit("MeshVersionFormatted 7\nDimension 2\n"),
            Err(Error::Unsupported(_))
        ));
    }

    #[test]
    fn parses_binary_meshb_versions_one_through_four_in_both_endian_orders() {
        for version in 1..=4 {
            for order in [ByteOrder::Little, ByteOrder::Big] {
                let bytes = meshb_fixture(version, order);
                let (nodes, cells, title, warnings) = parse_medit_binary(&bytes).unwrap();
                assert_eq!(nodes.len(), 4, "version {version}, {order:?}");
                assert_eq!(cells.len(), 8, "version {version}, {order:?}");
                assert!(title.contains("binary"));
                assert!(
                    warnings
                        .iter()
                        .any(|warning| warning.contains("XY projection"))
                );
            }
        }
    }

    #[test]
    fn rejects_malformed_binary_meshb_keyword_offsets_and_counts() {
        let mut bad_offset = meshb_fixture(2, ByteOrder::Little);
        bad_offset[12..16].copy_from_slice(&1i32.to_le_bytes());
        assert!(matches!(
            parse_medit_binary(&bad_offset),
            Err(Error::InvalidInput(_))
        ));

        let mut bad_count = meshb_fixture(2, ByteOrder::Little);
        // Dimension record (20 bytes), then Vertices keyword/count at bytes 28..32.
        bad_count[28..32].copy_from_slice(&1_000_001i32.to_le_bytes());
        assert!(matches!(
            parse_medit_binary(&bad_count),
            Err(Error::LimitExceeded(_))
        ));
    }
}
