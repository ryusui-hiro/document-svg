//! Bounded reader for SU2's native ASCII unstructured mesh files.

use std::collections::HashSet;
use std::fmt::Write as FmtWrite;
use std::io::Read;

use crate::convert::{ConvertOptions, PageConsumer};
use crate::error::{Error, Result};
use crate::ir::{IDENTITY, LineCap, LineJoin, Node, Page, Paint, SourceMeta, Stroke};

use super::{
    MAX_SIMULATION_CELLS, MAX_SIMULATION_POINTS, MeshCell, MeshNode, SimulationViewport,
    render_simulation,
};

const MAX_SU2_INPUT_BYTES: u64 = 128 * 1024 * 1024;
const MAX_SU2_LINES: usize = 2_000_000;
const MAX_SU2_LINE_BYTES: usize = 1024 * 1024;
const MAX_SU2_CONNECTIVITY: usize = 8_000_000;
const MAX_SU2_MARKERS: usize = 10_000;
const MAX_SU2_MARKER_NAME_BYTES: usize = 1024;
const MAX_SU2_MARKER_TEXT_BYTES: usize = 8 * 1024 * 1024;
const MAX_SU2_MARKER_EDGES: usize = 1_000_000;
const MAX_SU2_MARKER_OVERLAY_EDGES: usize = 200_000;
const MAX_SU2_RENDERED_PRIMITIVES: usize = 2_000_000;

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
enum ElementType {
    Line,
    Triangle,
    Quad,
    Tetrahedron,
    Hexahedron,
    Prism,
    Pyramid,
}

impl ElementType {
    fn from_su2_id(id: usize) -> Option<Self> {
        Some(match id {
            3 => Self::Line,
            5 => Self::Triangle,
            9 => Self::Quad,
            10 => Self::Tetrahedron,
            12 => Self::Hexahedron,
            13 => Self::Prism,
            14 => Self::Pyramid,
            _ => return None,
        })
    }

    fn nodes(self) -> usize {
        match self {
            Self::Line => 2,
            Self::Triangle => 3,
            Self::Quad => 4,
            Self::Tetrahedron => 4,
            Self::Hexahedron => 8,
            Self::Prism => 6,
            Self::Pyramid => 5,
        }
    }

    fn is_surface(self, dimension: usize) -> bool {
        match dimension {
            2 => matches!(self, Self::Line | Self::Triangle | Self::Quad),
            3 => matches!(self, Self::Triangle | Self::Quad),
            _ => false,
        }
    }

    fn is_volume(self, dimension: usize) -> bool {
        dimension == 3
            && matches!(
                self,
                Self::Tetrahedron | Self::Hexahedron | Self::Prism | Self::Pyramid
            )
    }
}

#[derive(Clone, Debug)]
struct RawCell {
    element_type: ElementType,
    node_ids: Vec<usize>,
    marker: bool,
}

struct Su2PageSink<'a> {
    inner: &'a mut dyn PageConsumer,
    points: &'a [(f64, f64, f64)],
    marker_edges: &'a [(usize, usize)],
}

impl PageConsumer for Su2PageSink<'_> {
    fn consume(&mut self, mut page: Page) -> Result<()> {
        page.source_format = "su2".into();
        page.title = "SU2 CFD mesh".into();
        if !self.marker_edges.is_empty() {
            let mut min_x = f64::INFINITY;
            let mut min_y = f64::INFINITY;
            let mut max_x = f64::NEG_INFINITY;
            let mut max_y = f64::NEG_INFINITY;
            for &(x, y, _) in self.points {
                min_x = min_x.min(x);
                min_y = min_y.min(y);
                max_x = max_x.max(x);
                max_y = max_y.max(y);
            }
            let viewport = SimulationViewport::new(min_x, min_y, max_x, max_y, false);
            let map = |point: (f64, f64, f64)| viewport.map(point.0, point.1);
            let mut path = String::new();
            for (a, b) in self.marker_edges {
                let (Some(point_a), Some(point_b)) = (self.points.get(*a), self.points.get(*b))
                else {
                    continue;
                };
                let a = map(*point_a);
                let b = map(*point_b);
                let _ = write!(path, "M {:.3} {:.3} L {:.3} {:.3} ", a.0, a.1, b.0, b.1);
            }
            if !path.is_empty() {
                page.nodes.push(Node::Path {
                    id: "su2-marker-boundaries".into(),
                    d: path,
                    fill_rule: "nonzero".into(),
                    fill: Paint::None,
                    stroke: Stroke {
                        paint: Paint::solid("#f97316"),
                        width: 2.0,
                        line_cap: LineCap::Round,
                        line_join: LineJoin::Round,
                        ..Default::default()
                    },
                    transform: IDENTITY,
                    clip_id: None,
                    meta: SourceMeta {
                        kind: "su2:boundary-marker-edges".into(),
                        semantic_role: "simulation:marker-boundary".into(),
                        alt_text: "SU2 boundary marker edges".into(),
                        ..Default::default()
                    },
                });
            }
        }
        self.inner.consume(page)
    }
}

pub(crate) fn looks_like_prefix(prefix: &[u8]) -> bool {
    let prefix = String::from_utf8_lossy(prefix).to_ascii_uppercase();
    prefix.contains("NDIME=") && prefix.contains("NELEM=") && prefix.contains("NPOIN=")
}

pub(crate) fn convert<R: Read>(
    input: R,
    options: &ConvertOptions,
    sink: &mut dyn PageConsumer,
) -> Result<Vec<String>> {
    let mut reader = input.take(
        options
            .max_input_bytes
            .min(MAX_SU2_INPUT_BYTES)
            .saturating_add(1),
    );
    let mut bytes = Vec::new();
    reader.read_to_end(&mut bytes)?;
    if bytes.len() as u64 > options.max_input_bytes.min(MAX_SU2_INPUT_BYTES) {
        return Err(Error::LimitExceeded(format!(
            "SU2 mesh exceeds maximum input size of {} bytes",
            options.max_input_bytes.min(MAX_SU2_INPUT_BYTES)
        )));
    }
    let text = String::from_utf8(bytes)
        .map_err(|error| Error::Unsupported(format!("SU2 mesh is not UTF-8/ASCII: {error}")))?;
    let lines = text.lines().collect::<Vec<_>>();
    if lines.len() > MAX_SU2_LINES {
        return Err(Error::LimitExceeded(format!(
            "SU2 mesh exceeds {MAX_SU2_LINES} lines"
        )));
    }
    for (index, line) in lines.iter().enumerate() {
        if line.len() > MAX_SU2_LINE_BYTES {
            return Err(Error::LimitExceeded(format!(
                "SU2 line {} exceeds {MAX_SU2_LINE_BYTES} bytes",
                index + 1
            )));
        }
    }

    let (model, mut warnings) = parse_su2(&lines)?;
    let mut simulation_cells = Vec::new();
    let mut volume_edges = HashSet::new();
    let mut omitted_types = HashSet::new();
    for cell in model.cells {
        let valid_dimension = if model.dimension == 2 {
            matches!(
                cell.element_type,
                ElementType::Line | ElementType::Triangle | ElementType::Quad
            )
        } else if cell.marker {
            cell.element_type.is_surface(3)
        } else {
            cell.element_type.is_volume(3)
        };
        if !valid_dimension {
            omitted_types.insert(cell.element_type);
            continue;
        }
        if model.dimension == 2 {
            simulation_cells.push(MeshCell {
                node_ids: cell.node_ids,
                scalar: None,
            });
        } else if cell.marker {
            // In a 3D XY projection, show the outline of marker faces rather
            // than filling their projected area over the volume wireframe.
            append_cycle_edges(&cell.node_ids, &mut volume_edges)?;
        } else {
            append_volume_edges(cell.element_type, &cell.node_ids, &mut volume_edges)?;
        }
    }
    if model.dimension == 3 {
        let mut ordered_edges = volume_edges.into_iter().collect::<Vec<_>>();
        ordered_edges.sort_unstable();
        simulation_cells.extend(ordered_edges.into_iter().map(|(a, b)| MeshCell {
            node_ids: vec![a, b],
            scalar: None,
        }));
    }
    if simulation_cells.is_empty() {
        return Err(Error::Unsupported(
            "SU2 mesh contains no supported line, surface, or volume elements".into(),
        ));
    }
    if simulation_cells.len() > MAX_SU2_RENDERED_PRIMITIVES {
        return Err(Error::LimitExceeded(format!(
            "SU2 rendered mesh primitives exceed {MAX_SU2_RENDERED_PRIMITIVES}"
        )));
    }
    if !omitted_types.is_empty() {
        warnings.push(format!(
            "{} SU2 element(s) incompatible with the declared dimension or supported topology were omitted",
            omitted_types.len()
        ));
    }
    if model.marker_count > 0 {
        warnings.push(format!(
            "{} SU2 boundary marker name(s) and solver boundary conditions are not displayed; marker edges are highlighted",
            model.marker_count
        ));
    }
    let marker_edges = if model.marker_edges.len() > MAX_SU2_MARKER_OVERLAY_EDGES {
        warnings.push(format!(
            "{} SU2 marker edge(s) exceed the overlay limit of {MAX_SU2_MARKER_OVERLAY_EDGES}; edges remain in the base mesh preview",
            model.marker_edges.len()
        ));
        &[][..]
    } else {
        model.marker_edges.as_slice()
    };
    if model.dimension == 3 {
        warnings.push("SU2 3D coordinates are projected onto the XY plane".into());
    }
    let nodes = model
        .points
        .iter()
        .copied()
        .enumerate()
        .map(|(id, (x, y, _))| (id, MeshNode { x, y, scalar: None }))
        .collect();
    let title = format!("SU2 CFD mesh ({})", model.dimension);
    let mut page_sink = Su2PageSink {
        inner: sink,
        points: &model.points,
        marker_edges,
    };
    warnings.extend(render_simulation(
        (nodes, simulation_cells, title, warnings.clone()),
        &mut page_sink,
    )?);
    Ok(deduplicate_warnings(warnings))
}

struct Su2Model {
    dimension: usize,
    points: Vec<(f64, f64, f64)>,
    cells: Vec<RawCell>,
    marker_count: usize,
    marker_edges: Vec<(usize, usize)>,
}

fn parse_su2(lines: &[&str]) -> Result<(Su2Model, Vec<String>)> {
    let mut dimension = None;
    let mut points: Option<Vec<(f64, f64, f64)>> = None;
    let mut cells = Vec::new();
    let mut warnings = Vec::new();
    let mut marker_count = 0usize;
    let mut marker_name_bytes = 0usize;
    let mut total_connectivity = 0usize;
    let mut unsupported_type_count = 0usize;
    let mut total_element_rows = 0usize;
    let mut marker_edge_set = HashSet::new();
    let mut ignored_section_names = HashSet::new();
    let mut ignored_section_count = 0usize;
    let mut unrecognized_text = false;
    let mut position = 0usize;
    let mut seen_nelem = false;
    let mut seen_nmark = false;

    while position < lines.len() {
        let line = lines[position].trim();
        if line.is_empty() || line.starts_with('%') {
            position += 1;
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            unrecognized_text = true;
            position += 1;
            continue;
        };
        let key = key
            .chars()
            .take(64)
            .collect::<String>()
            .to_ascii_uppercase();
        let value = value.trim();
        position += 1;
        match key.as_str() {
            "NDIME" => {
                if dimension.is_some() {
                    return Err(Error::InvalidInput("SU2 mesh repeats NDIME".into()));
                }
                let value = parse_count(value, "NDIME", 3)?;
                if value != 2 && value != 3 {
                    return Err(Error::Unsupported(format!(
                        "SU2 NDIME {value} is unsupported; only 2D and 3D are previewed"
                    )));
                }
                dimension = Some(value);
            }
            "NELEM" => {
                if seen_nelem {
                    return Err(Error::InvalidInput("SU2 mesh repeats NELEM".into()));
                }
                seen_nelem = true;
                let count = parse_count(value, "NELEM", MAX_SIMULATION_CELLS)?;
                total_element_rows = total_element_rows
                    .checked_add(count)
                    .ok_or_else(|| Error::LimitExceeded("SU2 element count overflowed".into()))?;
                for _ in 0..count {
                    let row = next_record_line(lines, &mut position, "element")?;
                    let Some((element_type, node_ids)) = parse_cell_row(row, "SU2 element")? else {
                        unsupported_type_count += 1;
                        continue;
                    };
                    total_connectivity = total_connectivity
                        .checked_add(node_ids.len())
                        .ok_or_else(|| {
                            Error::LimitExceeded("SU2 connectivity count overflowed".into())
                        })?;
                    if total_connectivity > MAX_SU2_CONNECTIVITY {
                        return Err(Error::LimitExceeded(format!(
                            "SU2 connectivity exceeds {MAX_SU2_CONNECTIVITY} node references"
                        )));
                    }
                    cells.push(RawCell {
                        element_type,
                        node_ids,
                        marker: false,
                    });
                }
            }
            "NPOIN" => {
                if points.is_some() {
                    return Err(Error::InvalidInput("SU2 mesh repeats NPOIN".into()));
                }
                let count = parse_count(value, "NPOIN", MAX_SIMULATION_POINTS)?;
                let dimension = dimension.ok_or_else(|| {
                    Error::InvalidInput("SU2 NDIME must appear before NPOIN".into())
                })?;
                let mut parsed_points = Vec::with_capacity(count);
                for index in 0..count {
                    let row = next_record_line(lines, &mut position, "point")?;
                    parsed_points.push(parse_point_row(row, index, dimension)?);
                }
                points = Some(parsed_points);
            }
            "NMARK" => {
                if seen_nmark {
                    return Err(Error::InvalidInput("SU2 mesh repeats NMARK".into()));
                }
                seen_nmark = true;
                let mesh_dimension = dimension.ok_or_else(|| {
                    Error::InvalidInput("SU2 NDIME must appear before NMARK".into())
                })?;
                let count = parse_count(value, "NMARK", MAX_SU2_MARKERS)?;
                marker_count = count;
                for _ in 0..count {
                    let tag_line = next_record_line(lines, &mut position, "marker tag")?;
                    let Some((tag_key, tag_value)) = tag_line.split_once('=') else {
                        return Err(Error::InvalidInput(
                            "SU2 marker is missing MARKER_TAG".into(),
                        ));
                    };
                    if !tag_key.trim().eq_ignore_ascii_case("MARKER_TAG") {
                        return Err(Error::InvalidInput(
                            "SU2 marker is missing MARKER_TAG".into(),
                        ));
                    }
                    let tag = tag_value.trim();
                    if tag.is_empty() || tag.len() > MAX_SU2_MARKER_NAME_BYTES {
                        return Err(Error::InvalidInput(format!(
                            "SU2 marker tag must be 1..={MAX_SU2_MARKER_NAME_BYTES} bytes"
                        )));
                    }
                    marker_name_bytes = marker_name_bytes
                        .checked_add(tag.len())
                        .ok_or_else(|| Error::LimitExceeded("SU2 marker text overflowed".into()))?;
                    if marker_name_bytes > MAX_SU2_MARKER_TEXT_BYTES {
                        return Err(Error::LimitExceeded(format!(
                            "SU2 marker text exceeds {MAX_SU2_MARKER_TEXT_BYTES} bytes"
                        )));
                    }
                    let elements_line = next_record_line(lines, &mut position, "marker elements")?;
                    let Some((elements_key, elements_value)) = elements_line.split_once('=') else {
                        return Err(Error::InvalidInput(
                            "SU2 marker is missing MARKER_ELEMS".into(),
                        ));
                    };
                    if !elements_key.trim().eq_ignore_ascii_case("MARKER_ELEMS") {
                        return Err(Error::InvalidInput(
                            "SU2 marker is missing MARKER_ELEMS".into(),
                        ));
                    }
                    let element_count =
                        parse_count(elements_value.trim(), "MARKER_ELEMS", MAX_SIMULATION_CELLS)?;
                    total_element_rows =
                        total_element_rows
                            .checked_add(element_count)
                            .ok_or_else(|| {
                                Error::LimitExceeded("SU2 total element count overflowed".into())
                            })?;
                    if total_element_rows > MAX_SIMULATION_CELLS {
                        return Err(Error::LimitExceeded(format!(
                            "SU2 total cells exceed {MAX_SIMULATION_CELLS}"
                        )));
                    }
                    for _ in 0..element_count {
                        let row = next_record_line(lines, &mut position, "marker element")?;
                        let Some((element_type, node_ids)) = parse_cell_row(row, "SU2 marker")?
                        else {
                            unsupported_type_count += 1;
                            continue;
                        };
                        total_connectivity = total_connectivity
                            .checked_add(node_ids.len())
                            .ok_or_else(|| {
                                Error::LimitExceeded("SU2 connectivity count overflowed".into())
                            })?;
                        if total_connectivity > MAX_SU2_CONNECTIVITY {
                            return Err(Error::LimitExceeded(format!(
                                "SU2 connectivity exceeds {MAX_SU2_CONNECTIVITY} node references"
                            )));
                        }
                        cells.push(RawCell {
                            element_type,
                            node_ids: node_ids.clone(),
                            marker: true,
                        });
                        if (mesh_dimension == 2 && element_type == ElementType::Line)
                            || (mesh_dimension == 3 && element_type.is_surface(3))
                        {
                            append_cycle_edges(&node_ids, &mut marker_edge_set)?;
                            if marker_edge_set.len() > MAX_SU2_MARKER_EDGES {
                                return Err(Error::LimitExceeded(format!(
                                    "SU2 boundary marker edges exceed {MAX_SU2_MARKER_EDGES}"
                                )));
                            }
                        }
                    }
                }
            }
            _ => {
                if ignored_section_names.len() < 64 {
                    ignored_section_names.insert(key.clone());
                } else {
                    ignored_section_count += 1;
                }
            }
        }
    }

    let dimension =
        dimension.ok_or_else(|| Error::InvalidInput("SU2 mesh is missing NDIME".into()))?;
    let points = points.ok_or_else(|| Error::InvalidInput("SU2 mesh is missing NPOIN".into()))?;
    if points.is_empty() {
        return Err(Error::Unsupported("SU2 mesh contains no points".into()));
    }
    if !seen_nelem {
        return Err(Error::InvalidInput("SU2 mesh is missing NELEM".into()));
    }
    if unsupported_type_count > 0 {
        warnings.push(format!(
            "{unsupported_type_count} SU2 element(s) use unsupported or high-order VTK types and were omitted"
        ));
    }
    let mut ignored_section_names = ignored_section_names.into_iter().collect::<Vec<_>>();
    ignored_section_names.sort_unstable();
    warnings.extend(
        ignored_section_names
            .into_iter()
            .map(|key| format!("SU2 section or option '{key}' was ignored")),
    );
    if ignored_section_count > 0 {
        warnings.push(format!(
            "{ignored_section_count} additional SU2 section/option name(s) were omitted from warnings"
        ));
    }
    if unrecognized_text {
        warnings.push("unrecognized text between SU2 mesh sections was ignored".into());
    }
    for cell in &cells {
        if cell.node_ids.iter().any(|node| *node >= points.len()) {
            return Err(Error::InvalidInput(
                "SU2 element references a node outside the NPOIN range".into(),
            ));
        }
    }
    let mut marker_edges = marker_edge_set.into_iter().collect::<Vec<_>>();
    marker_edges.sort_unstable();
    Ok((
        Su2Model {
            dimension,
            points,
            cells,
            marker_count,
            marker_edges,
        },
        warnings,
    ))
}

fn parse_count(value: &str, context: &str, limit: usize) -> Result<usize> {
    let count = value
        .split_whitespace()
        .next()
        .unwrap_or_default()
        .parse::<usize>()
        .map_err(|_| Error::InvalidInput(format!("invalid SU2 {context} count '{value}'")))?;
    if count > limit {
        return Err(Error::LimitExceeded(format!(
            "SU2 {context} count exceeds {limit}"
        )));
    }
    Ok(count)
}

fn next_record_line<'a>(lines: &'a [&str], position: &mut usize, context: &str) -> Result<&'a str> {
    let line = lines
        .get(*position)
        .ok_or_else(|| Error::InvalidInput(format!("SU2 {context} record is missing")))?;
    *position += 1;
    let line = line.trim();
    if line.is_empty() || line.starts_with('%') {
        return Err(Error::InvalidInput(format!(
            "SU2 {context} record is blank or is not a data row"
        )));
    }
    Ok(line)
}

fn parse_point_row(row: &str, index: usize, dimension: usize) -> Result<(f64, f64, f64)> {
    let mut fields = row.split_whitespace();
    let mut point = [0.0_f64; 3];
    for coordinate in point.iter_mut().take(dimension) {
        let value = fields.next().ok_or_else(|| {
            Error::InvalidInput(format!("SU2 point {} has too few coordinates", index + 1))
        })?;
        *coordinate = parse_coordinate(value, index + 1)?;
    }
    if let Some(optional_index) = fields.next() {
        optional_index.parse::<usize>().map_err(|_| {
            Error::InvalidInput(format!(
                "invalid optional SU2 point index '{optional_index}'"
            ))
        })?;
    }
    if fields.next().is_some() {
        return Err(Error::InvalidInput(format!(
            "SU2 point {} has extra coordinate fields",
            index + 1
        )));
    }
    Ok((point[0], point[1], point[2]))
}

fn parse_coordinate(value: &str, point: usize) -> Result<f64> {
    let coordinate = value.parse::<f64>().map_err(|_| {
        Error::InvalidInput(format!("invalid SU2 coordinate '{value}' at point {point}"))
    })?;
    if !coordinate.is_finite() || coordinate.abs() > 1e12 {
        return Err(Error::InvalidInput(format!(
            "SU2 point {point} coordinate is non-finite or outside ±1e12"
        )));
    }
    Ok(coordinate)
}

fn parse_cell_row(row: &str, context: &str) -> Result<Option<(ElementType, Vec<usize>)>> {
    let mut fields = row.split_whitespace();
    let type_token = fields
        .next()
        .ok_or_else(|| Error::InvalidInput(format!("{context} row is empty")))?;
    let type_id = type_token
        .parse::<usize>()
        .map_err(|_| Error::InvalidInput(format!("invalid SU2 VTK element type '{type_token}'")))?;
    let Some(element_type) = ElementType::from_su2_id(type_id) else {
        return Ok(None);
    };
    let node_count = element_type.nodes();
    let mut values = Vec::with_capacity(node_count + 1);
    for _ in 0..node_count {
        let token = fields.next().ok_or_else(|| {
            Error::InvalidInput(format!(
                "{context} type {type_id} has incomplete connectivity"
            ))
        })?;
        values.push(token.parse::<usize>().map_err(|_| {
            Error::InvalidInput(format!("invalid SU2 node index '{token}' in {context}"))
        })?);
    }
    if let Some(optional_cell_id) = fields.next() {
        optional_cell_id.parse::<usize>().map_err(|_| {
            Error::InvalidInput(format!(
                "invalid optional SU2 cell index '{optional_cell_id}'"
            ))
        })?;
    }
    if fields.next().is_some() {
        return Err(Error::InvalidInput(format!(
            "{context} type {type_id} has extra connectivity fields"
        )));
    }
    Ok(Some((element_type, values)))
}

fn append_cycle_edges(nodes: &[usize], edges: &mut HashSet<(usize, usize)>) -> Result<()> {
    for index in 0..nodes.len() {
        insert_edge(edges, nodes[index], nodes[(index + 1) % nodes.len()])?;
    }
    Ok(())
}

fn append_volume_edges(
    element_type: ElementType,
    nodes: &[usize],
    edges: &mut HashSet<(usize, usize)>,
) -> Result<()> {
    let pairs: &[(usize, usize)] = match element_type {
        ElementType::Tetrahedron => &[(0, 1), (1, 2), (2, 0), (0, 3), (1, 3), (2, 3)],
        ElementType::Hexahedron => &[
            (0, 1),
            (1, 2),
            (2, 3),
            (3, 0),
            (4, 5),
            (5, 6),
            (6, 7),
            (7, 4),
            (0, 4),
            (1, 5),
            (2, 6),
            (3, 7),
        ],
        ElementType::Prism => &[
            (0, 1),
            (1, 2),
            (2, 0),
            (3, 4),
            (4, 5),
            (5, 3),
            (0, 3),
            (1, 4),
            (2, 5),
        ],
        ElementType::Pyramid => &[
            (0, 1),
            (1, 2),
            (2, 3),
            (3, 0),
            (0, 4),
            (1, 4),
            (2, 4),
            (3, 4),
        ],
        _ => return Ok(()),
    };
    for &(a, b) in pairs {
        insert_edge(edges, nodes[a], nodes[b])?;
    }
    Ok(())
}

fn insert_edge(edges: &mut HashSet<(usize, usize)>, a: usize, b: usize) -> Result<()> {
    if a != b {
        edges.insert((a.min(b), a.max(b)));
        if edges.len() > MAX_SU2_RENDERED_PRIMITIVES {
            return Err(Error::LimitExceeded(format!(
                "SU2 rendered mesh primitives exceed {MAX_SU2_RENDERED_PRIMITIVES}"
            )));
        }
    }
    Ok(())
}

fn deduplicate_warnings(warnings: Vec<String>) -> Vec<String> {
    let mut seen = HashSet::new();
    warnings
        .into_iter()
        .filter(|warning| seen.insert(warning.clone()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const TWO_D: &str = "NDIME= 2\nNELEM= 2\n5 0 1 4 0\n5 1 2 4 1\nNPOIN= 5\n0.0 0.0 0\n1.0 0.0 1\n1.0 1.0 2\n0.0 1.0 3\n0.5 0.5 4\nNMARK= 4\nMARKER_TAG= lower\nMARKER_ELEMS= 1\n3 0 1\nMARKER_TAG= right\nMARKER_ELEMS= 1\n3 1 2\nMARKER_TAG= upper\nMARKER_ELEMS= 1\n3 2 3\nMARKER_TAG= left\nMARKER_ELEMS= 1\n3 3 0\n";

    #[test]
    fn parses_su2_two_dimensional_elements_and_named_markers() {
        let (model, warnings) = parse_su2(&TWO_D.lines().collect::<Vec<_>>()).unwrap();
        assert_eq!(model.dimension, 2);
        assert_eq!(model.points.len(), 5);
        assert_eq!(model.cells.len(), 6);
        assert_eq!(model.marker_count, 4);
        assert!(model.cells[0].node_ids == vec![0, 1, 4]);
        assert!(warnings.is_empty());
    }

    #[test]
    fn parses_three_dimensional_volume_and_surface_element_codes() {
        let text = "NDIME= 3\nNELEM= 1\n10 0 1 2 3\nNPOIN= 4\n0 0 0\n1 0 0\n0 1 0\n0 0 1\nNMARK= 1\nMARKER_TAG= walls\nMARKER_ELEMS= 1\n5 0 2 1\n";
        let (model, warnings) = parse_su2(&text.lines().collect::<Vec<_>>()).unwrap();
        assert_eq!(model.dimension, 3);
        assert_eq!(model.cells.len(), 2);
        assert_eq!(model.cells[0].element_type, ElementType::Tetrahedron);
        assert_eq!(model.cells[1].element_type, ElementType::Triangle);
        assert!(model.cells[1].marker);
        assert!(warnings.is_empty());
    }

    #[test]
    fn rejects_out_of_range_connectivity_and_nonfinite_coordinates() {
        let bad_index = TWO_D.replace("5 0 1 4 0", "5 0 1 99 0");
        assert!(parse_su2(&bad_index.lines().collect::<Vec<_>>()).is_err());
        let bad_point = TWO_D.replace("0.0 0.0 0", "NaN 0.0 0");
        assert!(parse_su2(&bad_point.lines().collect::<Vec<_>>()).is_err());
    }

    #[test]
    fn content_sniff_requires_mesh_sections() {
        assert!(looks_like_prefix(b"NDIME=2\nNELEM=1\nNPOIN=3\n"));
        assert!(!looks_like_prefix(b"NDIME=2\nNPOIN=3\n"));
    }
}
