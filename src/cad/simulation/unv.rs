//! Bounded ASCII UNV/UFF mesh preview for finite-element geometry.
//!
//! Reads node datasets 15/2411 and element dataset 2412. Other result, group,
//! coordinate-system, and property datasets are not interpreted.

use std::collections::{HashMap, HashSet};
use std::io::Read;

use crate::convert::{ConvertOptions, PageConsumer};
use crate::error::{Error, Result};

use super::{MAX_SIMULATION_CELLS, MAX_SIMULATION_POINTS, MeshCell, MeshNode, render_simulation};

const MAX_UNV_INPUT_BYTES: u64 = 128 * 1024 * 1024;
const MAX_UNV_LINES: usize = 2_000_000;
const MAX_UNV_LINE_BYTES: usize = 1024 * 1024;
const MAX_UNV_DATASETS: usize = 100_000;
const MAX_UNV_NODES_PER_ELEMENT: usize = 128;
const MAX_UNV_ELEMENT_NODES_TOTAL: usize = 16_000_000;
const MAX_UNV_RENDERED_CELLS: usize = 2_000_000;
type UnvParseOutput = (HashMap<usize, MeshNode>, Vec<MeshCell>, Vec<String>);

#[derive(Clone, Copy)]
struct Dataset<'a> {
    id: i64,
    lines: &'a [&'a str],
}

struct RawElement {
    id: i64,
    descriptor: i64,
    node_ids: Vec<i64>,
}

#[derive(Default)]
struct UnvWarnings {
    projected_z_nodes: usize,
    omitted_midside_elements: usize,
    unsupported_elements: usize,
    unsupported_examples: Vec<i64>,
    ignored_datasets: usize,
    ignored_dataset_examples: Vec<i64>,
}

pub(crate) fn looks_like_unv_prefix(prefix: &[u8]) -> bool {
    let text = String::from_utf8_lossy(prefix);
    let lines = text.lines().map(str::trim).collect::<Vec<_>>();
    lines
        .windows(2)
        .take(512)
        .any(|pair| pair[0] == "-1" && matches!(pair[1], "15" | "2411"))
}

pub(crate) fn convert<R: Read>(
    input: R,
    options: &ConvertOptions,
    sink: &mut dyn PageConsumer,
) -> Result<Vec<String>> {
    let max_bytes = options.max_input_bytes.min(MAX_UNV_INPUT_BYTES);
    let mut bytes = Vec::new();
    Read::take(input, max_bytes.saturating_add(1)).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > max_bytes {
        return Err(Error::LimitExceeded(format!(
            "UNV input exceeds maximum size of {max_bytes} bytes"
        )));
    }
    let text = String::from_utf8(bytes).map_err(|error| {
        Error::Unsupported(format!(
            "binary or non-UTF-8 UNV data is unsupported: {error}"
        ))
    })?;
    let (nodes, cells, warnings) = parse_unv(&text)?;
    let mut page_sink = UnvPageSink { inner: sink };
    render_simulation(
        (nodes, cells, "UNV Universal FEA mesh".into(), warnings),
        &mut page_sink,
    )
}

struct UnvPageSink<'a> {
    inner: &'a mut dyn PageConsumer,
}

impl PageConsumer for UnvPageSink<'_> {
    fn consume(&mut self, mut page: crate::ir::Page) -> Result<()> {
        page.source_format = "unv".into();
        self.inner.consume(page)
    }
}

fn parse_unv(text: &str) -> Result<UnvParseOutput> {
    if text.len() as u64 > MAX_UNV_INPUT_BYTES {
        return Err(Error::LimitExceeded(format!(
            "UNV input exceeds maximum size of {MAX_UNV_INPUT_BYTES} bytes"
        )));
    }
    let text = text.trim_start_matches('\u{feff}');
    let lines = text
        .lines()
        .map(|line| line.trim_end_matches('\r'))
        .collect::<Vec<_>>();
    if lines.len() > MAX_UNV_LINES {
        return Err(Error::LimitExceeded(format!(
            "UNV input exceeds {MAX_UNV_LINES} lines"
        )));
    }
    if lines.iter().any(|line| line.len() > MAX_UNV_LINE_BYTES) {
        return Err(Error::LimitExceeded(format!(
            "UNV line exceeds {MAX_UNV_LINE_BYTES} bytes"
        )));
    }
    let datasets = collect_datasets(&lines)?;
    let mut nodes = HashMap::<usize, MeshNode>::new();
    let mut raw_elements = Vec::<RawElement>::new();
    let mut element_ids = HashSet::<i64>::new();
    let mut node_dataset = None::<i64>;
    let mut warnings = UnvWarnings::default();
    let mut total_element_nodes = 0usize;

    for dataset in datasets {
        match dataset.id {
            15 | 2411 => {
                if node_dataset.is_some_and(|existing| existing != dataset.id) {
                    return Err(Error::Unsupported(
                        "UNV contains both legacy node dataset 15 and dataset 2411".into(),
                    ));
                }
                node_dataset = Some(dataset.id);
                parse_node_dataset(dataset, &mut nodes, &mut warnings)?;
            }
            2412 => {
                parse_element_dataset(
                    dataset,
                    &mut raw_elements,
                    &mut element_ids,
                    &mut total_element_nodes,
                )?;
            }
            other => {
                warnings.ignored_datasets = warnings.ignored_datasets.saturating_add(1);
                if warnings.ignored_dataset_examples.len() < 8
                    && !warnings.ignored_dataset_examples.contains(&other)
                {
                    warnings.ignored_dataset_examples.push(other);
                }
            }
        }
    }

    if nodes.is_empty() {
        return Err(Error::Unsupported(
            "UNV input has no supported node dataset (15 or 2411)".into(),
        ));
    }
    if raw_elements.is_empty() {
        return Err(Error::Unsupported(
            "UNV input has no supported element dataset 2412".into(),
        ));
    }

    let mut cells = Vec::new();
    let mut element_count = 0usize;
    for element in raw_elements {
        let Some((corner_ids, family, high_order)) = element_corners(&element)? else {
            warnings.unsupported_elements = warnings.unsupported_elements.saturating_add(1);
            if warnings.unsupported_examples.len() < 8
                && !warnings.unsupported_examples.contains(&element.descriptor)
            {
                warnings.unsupported_examples.push(element.descriptor);
            }
            continue;
        };
        for &node_id in &corner_ids {
            if !nodes.contains_key(&usize::try_from(node_id).unwrap_or(usize::MAX)) {
                return Err(Error::InvalidInput(format!(
                    "UNV element {} references unknown node {node_id}",
                    element.id
                )));
            }
        }
        let generated = match family {
            ElementFamily::Line | ElementFamily::Surface => vec![MeshCell {
                node_ids: corner_ids
                    .iter()
                    .map(|id| usize::try_from(*id).expect("positive node ID was validated"))
                    .collect(),
                scalar: None,
            }],
            ElementFamily::Tetrahedron => edge_cells(
                &corner_ids,
                &[[0, 1], [1, 2], [2, 0], [0, 3], [1, 3], [2, 3]],
            ),
            ElementFamily::Prism => edge_cells(
                &corner_ids,
                &[
                    [0, 1],
                    [1, 2],
                    [2, 0],
                    [3, 4],
                    [4, 5],
                    [5, 3],
                    [0, 3],
                    [1, 4],
                    [2, 5],
                ],
            ),
            ElementFamily::Hexahedron => edge_cells(
                &corner_ids,
                &[
                    [0, 1],
                    [1, 2],
                    [2, 3],
                    [3, 0],
                    [4, 5],
                    [5, 6],
                    [6, 7],
                    [7, 4],
                    [0, 4],
                    [1, 5],
                    [2, 6],
                    [3, 7],
                ],
            ),
        };
        element_count = element_count.saturating_add(1);
        if cells.len().saturating_add(generated.len()) > MAX_UNV_RENDERED_CELLS {
            return Err(Error::LimitExceeded(format!(
                "UNV rendered primitive count exceeds {MAX_UNV_RENDERED_CELLS}"
            )));
        }
        cells.extend(generated);
        if high_order {
            warnings.omitted_midside_elements = warnings.omitted_midside_elements.saturating_add(1);
        }
    }

    if element_count == 0 {
        return Err(Error::Unsupported(
            "UNV input contains no supported linear or corner-projected element types".into(),
        ));
    }
    Ok((nodes, cells, warning_messages(warnings)))
}

fn collect_datasets<'a>(lines: &'a [&'a str]) -> Result<Vec<Dataset<'a>>> {
    let mut datasets = Vec::new();
    let mut index = 0usize;
    while index < lines.len() {
        if lines[index].trim() != "-1" {
            index += 1;
            continue;
        }
        let mut number_line = index + 1;
        while number_line < lines.len() && lines[number_line].trim().is_empty() {
            number_line += 1;
        }
        if number_line >= lines.len() {
            index += 1;
            continue;
        }
        let Ok(dataset_id) = lines[number_line].trim().parse::<i64>() else {
            index += 1;
            continue;
        };
        let body_start = number_line + 1;
        let Some(relative_end) = lines[body_start..]
            .iter()
            .position(|line| line.trim() == "-1")
        else {
            return Err(Error::InvalidInput(format!(
                "UNV dataset {dataset_id} is not terminated"
            )));
        };
        let body_end = body_start + relative_end;
        if datasets.len() >= MAX_UNV_DATASETS {
            return Err(Error::LimitExceeded(format!(
                "UNV input exceeds {MAX_UNV_DATASETS} datasets"
            )));
        }
        datasets.push(Dataset {
            id: dataset_id,
            lines: &lines[body_start..body_end],
        });
        index = body_end + 1;
    }
    Ok(datasets)
}

fn parse_node_dataset(
    dataset: Dataset<'_>,
    nodes: &mut HashMap<usize, MeshNode>,
    warnings: &mut UnvWarnings,
) -> Result<()> {
    if dataset.id == 15 {
        parse_dataset15_nodes(dataset, nodes, warnings)
    } else {
        parse_dataset2411_nodes(dataset, nodes, warnings)
    }
}

fn parse_dataset2411_nodes(
    dataset: Dataset<'_>,
    nodes: &mut HashMap<usize, MeshNode>,
    warnings: &mut UnvWarnings,
) -> Result<()> {
    let mut index = 0usize;
    while index < dataset.lines.len() {
        if dataset.lines[index].trim().is_empty() {
            index += 1;
            continue;
        }
        let header_line = dataset.lines[index];
        let fields = integer_fields(header_line, 10, "2411 node header")?;
        if fields.len() < 4 {
            return Err(Error::InvalidInput(
                "UNV 2411 node header requires four integer fields".into(),
            ));
        }
        let id = parse_positive_id(fields[0], "node")?;
        let export_coordinate_system = parse_int(fields[1], "node coordinate system")?;
        if export_coordinate_system != 0 {
            return Err(Error::Unsupported(format!(
                "UNV node {id} uses coordinate system {export_coordinate_system}; only global coordinates are supported"
            )));
        }
        if nodes.len() >= MAX_SIMULATION_POINTS {
            return Err(Error::LimitExceeded(format!(
                "UNV node count exceeds {MAX_SIMULATION_POINTS}"
            )));
        }
        index += 1;
        let coordinate_line = dataset.lines.get(index).ok_or_else(|| {
            Error::InvalidInput(format!("UNV 2411 node {id} has no coordinate record"))
        })?;
        let coordinates = float_fields(coordinate_line, 25, "2411 node coordinates")?;
        if coordinates.len() < 3 {
            return Err(Error::InvalidInput(format!(
                "UNV 2411 node {id} requires three coordinates"
            )));
        }
        let x = parse_coordinate(coordinates[0], id)?;
        let y = parse_coordinate(coordinates[1], id)?;
        let z = parse_coordinate(coordinates[2], id)?;
        insert_node(nodes, id, x, y, z, warnings)?;
        index += 1;
    }
    Ok(())
}

fn parse_dataset15_nodes(
    dataset: Dataset<'_>,
    nodes: &mut HashMap<usize, MeshNode>,
    warnings: &mut UnvWarnings,
) -> Result<()> {
    for line in dataset
        .lines
        .iter()
        .copied()
        .filter(|line| !line.trim().is_empty())
    {
        let (header, coordinate_fields) = if line.len() >= 79 {
            let header = integer_fields(&line[..40], 10, "15 node header")?;
            let coords = float_fields(&line[40..79], 13, "15 node coordinates")?;
            (header, Some(coords))
        } else {
            (integer_fields(line, 10, "15 node record")?, None)
        };
        if header.len() < 4 {
            return Err(Error::InvalidInput(
                "UNV 15 node record requires four integer fields".into(),
            ));
        }
        let id = parse_positive_id(header[0], "node")?;
        let coordinate_system = parse_int(header[1], "node coordinate system")?;
        if coordinate_system != 0 {
            return Err(Error::Unsupported(format!(
                "UNV node {id} uses coordinate system {coordinate_system}; only global coordinates are supported"
            )));
        }
        let coords = match coordinate_fields {
            Some(coordinates) => coordinates,
            None => {
                let fields = float_fields(line, 13, "15 node coordinates")?;
                if fields.len() < 7 {
                    return Err(Error::InvalidInput(format!(
                        "UNV 15 node {id} requires four metadata and three coordinate fields"
                    )));
                }
                fields[4..7].to_vec()
            }
        };
        if coords.len() < 3 {
            return Err(Error::InvalidInput(format!(
                "UNV 15 node {id} requires three coordinates"
            )));
        }
        if nodes.len() >= MAX_SIMULATION_POINTS {
            return Err(Error::LimitExceeded(format!(
                "UNV node count exceeds {MAX_SIMULATION_POINTS}"
            )));
        }
        let x = parse_coordinate(coords[0], id)?;
        let y = parse_coordinate(coords[1], id)?;
        let z = parse_coordinate(coords[2], id)?;
        insert_node(nodes, id, x, y, z, warnings)?;
    }
    Ok(())
}

fn insert_node(
    nodes: &mut HashMap<usize, MeshNode>,
    id: usize,
    x: f64,
    y: f64,
    z: f64,
    warnings: &mut UnvWarnings,
) -> Result<()> {
    if nodes.contains_key(&id) {
        return Err(Error::InvalidInput(format!("duplicate UNV node ID {id}")));
    }
    if z.abs() > 1e-12 {
        warnings.projected_z_nodes = warnings.projected_z_nodes.saturating_add(1);
    }
    nodes.insert(id, MeshNode { x, y, scalar: None });
    Ok(())
}

fn parse_element_dataset(
    dataset: Dataset<'_>,
    elements: &mut Vec<RawElement>,
    element_ids: &mut HashSet<i64>,
    total_nodes: &mut usize,
) -> Result<()> {
    let mut index = 0usize;
    while index < dataset.lines.len() {
        if dataset.lines[index].trim().is_empty() {
            index += 1;
            continue;
        }
        let fields = integer_fields(dataset.lines[index], 10, "2412 element header")?;
        if fields.len() < 6 {
            return Err(Error::InvalidInput(
                "UNV 2412 element header requires six integer fields".into(),
            ));
        }
        let element_id = parse_int(fields[0], "element")?;
        if element_id <= 0 {
            return Err(Error::InvalidInput(
                "UNV element ID must be positive".into(),
            ));
        }
        let id = usize::try_from(element_id)
            .map_err(|_| Error::LimitExceeded("UNV element ID exceeds this platform".into()))?;
        let descriptor = parse_int(fields[1], "element descriptor")?;
        let node_count =
            usize::try_from(parse_int(fields[5], "element node count")?).map_err(|_| {
                Error::InvalidInput(format!("UNV element {id} has a negative node count"))
            })?;
        if node_count == 0 || node_count > MAX_UNV_NODES_PER_ELEMENT {
            return Err(Error::LimitExceeded(format!(
                "UNV element {id} has unsupported node count {node_count}"
            )));
        }
        if !element_ids.insert(element_id) {
            return Err(Error::InvalidInput(format!(
                "duplicate UNV element ID {id}"
            )));
        }
        if elements.len() >= MAX_SIMULATION_CELLS {
            return Err(Error::LimitExceeded(format!(
                "UNV element count exceeds {MAX_SIMULATION_CELLS}"
            )));
        }
        *total_nodes = total_nodes.saturating_add(node_count);
        if *total_nodes > MAX_UNV_ELEMENT_NODES_TOTAL {
            return Err(Error::LimitExceeded(format!(
                "UNV element connectivity exceeds {MAX_UNV_ELEMENT_NODES_TOTAL} node IDs"
            )));
        }
        index += 1;
        if descriptor < 25 {
            let beam_record = dataset.lines.get(index).ok_or_else(|| {
                Error::InvalidInput(format!("UNV beam element {id} has no orientation record"))
            })?;
            let beam_fields = integer_fields(beam_record, 10, "2412 beam record")?;
            if beam_fields.len() < 3 {
                return Err(Error::InvalidInput(format!(
                    "UNV beam element {id} requires an orientation record"
                )));
            }
            index += 1;
        }
        let mut node_ids = Vec::with_capacity(node_count);
        while node_ids.len() < node_count {
            let connectivity_line = dataset.lines.get(index).ok_or_else(|| {
                Error::InvalidInput(format!("UNV element {id} has truncated connectivity"))
            })?;
            if connectivity_line.trim().is_empty() {
                index += 1;
                continue;
            }
            let fields = integer_fields(connectivity_line, 10, "2412 connectivity")?;
            if fields.is_empty() || fields.len() > node_count - node_ids.len() {
                return Err(Error::InvalidInput(format!(
                    "UNV element {id} has an invalid connectivity record"
                )));
            }
            for field in fields {
                node_ids.push(parse_positive_id(field, "element node")? as i64);
            }
            index += 1;
        }
        elements.push(RawElement {
            id: element_id,
            descriptor,
            node_ids,
        });
    }
    Ok(())
}

fn integer_fields<'a>(line: &'a str, field_width: usize, context: &str) -> Result<Vec<&'a str>> {
    let fields = if line.len() >= field_width && line.len().is_multiple_of(field_width) {
        let fixed = line
            .as_bytes()
            .chunks_exact(field_width)
            .map(|field| std::str::from_utf8(field).unwrap_or_default().trim())
            .filter(|field| !field.is_empty())
            .collect::<Vec<_>>();
        if fixed.iter().all(|field| field.parse::<i64>().is_ok()) {
            fixed
        } else {
            line.split_whitespace().collect()
        }
    } else {
        line.split_whitespace().collect()
    };
    if fields.iter().any(|field| field.parse::<i64>().is_err()) {
        return Err(Error::InvalidInput(format!(
            "UNV {context} contains an invalid integer"
        )));
    }
    Ok(fields)
}

fn float_fields<'a>(line: &'a str, field_width: usize, context: &str) -> Result<Vec<&'a str>> {
    let fields = if line.len() >= field_width && line.len().is_multiple_of(field_width) {
        let fixed = line
            .as_bytes()
            .chunks_exact(field_width)
            .map(|field| std::str::from_utf8(field).unwrap_or_default().trim())
            .filter(|field| !field.is_empty())
            .collect::<Vec<_>>();
        if fixed
            .iter()
            .all(|field| parse_fortran_float(field).is_some())
        {
            fixed
        } else {
            line.split_whitespace().collect()
        }
    } else {
        line.split_whitespace().collect()
    };
    if fields
        .iter()
        .any(|field| parse_fortran_float(field).is_none())
    {
        return Err(Error::InvalidInput(format!(
            "UNV {context} contains an invalid coordinate"
        )));
    }
    Ok(fields)
}

fn parse_int(value: &str, context: &str) -> Result<i64> {
    value
        .parse::<i64>()
        .map_err(|_| Error::InvalidInput(format!("UNV {context} '{value}' is not an integer")))
}

fn parse_positive_id(value: &str, context: &str) -> Result<usize> {
    let id = parse_int(value, context)?;
    if id <= 0 {
        return Err(Error::InvalidInput(format!(
            "UNV {context} ID must be positive"
        )));
    }
    usize::try_from(id)
        .map_err(|_| Error::LimitExceeded(format!("UNV {context} ID exceeds this platform")))
}

fn parse_coordinate(value: &str, node_id: usize) -> Result<f64> {
    let value = parse_fortran_float(value).ok_or_else(|| {
        Error::InvalidInput(format!("UNV node {node_id} has an invalid coordinate"))
    })?;
    if !value.is_finite() || value.abs() > 1e12 {
        return Err(Error::InvalidInput(format!(
            "UNV node {node_id} coordinate is non-finite or outside ±1e12"
        )));
    }
    Ok(value)
}

fn parse_fortran_float(value: &str) -> Option<f64> {
    value
        .replace('D', "E")
        .replace('d', "e")
        .parse::<f64>()
        .ok()
}

#[derive(Clone, Copy)]
enum ElementFamily {
    Line,
    Surface,
    Tetrahedron,
    Prism,
    Hexahedron,
}

fn element_corners(element: &RawElement) -> Result<Option<(Vec<i64>, ElementFamily, bool)>> {
    let (indices, family): (&[usize], ElementFamily) = match element.descriptor {
        11 | 21..=24 => (&[0, 1], ElementFamily::Line),
        41 | 91 => (&[0, 2, 1], ElementFamily::Surface),
        42 | 92 => (&[0, 2, 4], ElementFamily::Surface),
        44 | 94 => (&[0, 3, 2, 1], ElementFamily::Surface),
        45 | 95 | 300 => (&[0, 6, 4, 2], ElementFamily::Surface),
        111 => (&[0, 1, 2, 3], ElementFamily::Tetrahedron),
        112 => (&[0, 1, 2, 3, 4, 5], ElementFamily::Prism),
        115 => (&[0, 3, 7, 4, 1, 2, 6, 5], ElementFamily::Hexahedron),
        116 => (&[0, 6, 18, 12, 2, 4, 16, 14], ElementFamily::Hexahedron),
        118 => (&[0, 2, 4, 9], ElementFamily::Tetrahedron),
        _ => return Ok(None),
    };
    if indices.iter().any(|index| *index >= element.node_ids.len()) {
        return Err(Error::InvalidInput(format!(
            "UNV element {} descriptor {} has {} connectivity IDs; expected at least {}",
            element.id,
            element.descriptor,
            element.node_ids.len(),
            indices.iter().max().unwrap_or(&0) + 1
        )));
    }
    let high_order = element.node_ids.len() > indices.len();
    Ok(Some((
        indices
            .iter()
            .map(|index| element.node_ids[*index])
            .collect(),
        family,
        high_order,
    )))
}

fn edge_cells(corners: &[i64], edges: &[[usize; 2]]) -> Vec<MeshCell> {
    edges
        .iter()
        .filter_map(|edge| {
            Some(MeshCell {
                node_ids: vec![
                    usize::try_from(*corners.get(edge[0])?).ok()?,
                    usize::try_from(*corners.get(edge[1])?).ok()?,
                ],
                scalar: None,
            })
        })
        .collect()
}

fn warning_messages(warnings: UnvWarnings) -> Vec<String> {
    let mut messages = Vec::new();
    if warnings.projected_z_nodes > 0 {
        messages.push(format!(
            "{} UNV node(s) had nonzero Z coordinates and were projected onto XY",
            warnings.projected_z_nodes
        ));
    }
    if warnings.omitted_midside_elements > 0 {
        messages.push(format!(
            "{} higher-order UNV element(s) are shown using corner nodes only",
            warnings.omitted_midside_elements
        ));
    }
    if warnings.unsupported_elements > 0 {
        messages.push(format!(
            "{} unsupported UNV element(s) were skipped (descriptor IDs: {})",
            warnings.unsupported_elements,
            warnings
                .unsupported_examples
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    if warnings.ignored_datasets > 0 {
        messages.push(format!(
            "{} non-mesh UNV dataset(s) were ignored (examples: {})",
            warnings.ignored_datasets,
            warnings
                .ignored_dataset_examples
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    messages
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixed_i10(values: &[i64]) -> String {
        values.iter().map(|value| format!("{value:10}")).collect()
    }

    fn fixed_f25(values: &[f64]) -> String {
        values
            .iter()
            .map(|value| format!("{value:25.16E}"))
            .collect()
    }

    fn fixed_e13(values: &[f64]) -> String {
        values
            .iter()
            .map(|value| format!("{value:13.5E}"))
            .collect()
    }

    fn fixture(dataset_first: bool) -> String {
        let nodes = [
            (1, [0.0, 0.0, 0.0]),
            (2, [100.0, 0.0, 0.0]),
            (3, [100.0, 100.0, 0.0]),
            (4, [0.0, 100.0, 12.0]),
        ];
        let mut node_data = String::from("    -1\n  2411\n");
        for (id, [x, y, z]) in nodes {
            node_data.push_str(&fixed_i10(&[id, 0, 0, 0]));
            node_data.push('\n');
            node_data.push_str(&fixed_f25(&[x, y, z]));
            node_data.push('\n');
        }
        node_data.push_str("    -1\n");

        let mut element_data = String::from("    -1\n  2412\n");
        for (id, connectivity) in [(10, [1, 2, 3]), (11, [1, 3, 4])] {
            element_data.push_str(&fixed_i10(&[id, 91, 0, 0, 0, 3]));
            element_data.push('\n');
            element_data.push_str(&fixed_i10(&connectivity));
            element_data.push('\n');
        }
        element_data.push_str("    -1\n");

        if dataset_first {
            format!("{element_data}{node_data}    -1\n")
        } else {
            format!("{node_data}    -1\n{element_data}    -1\n")
        }
    }

    #[test]
    fn parses_nodes_and_shell_elements_with_dataset_order_and_fortran_numbers() {
        let text = fixture(true);
        let (nodes, cells, warnings) = parse_unv(&text).unwrap();
        assert_eq!(nodes.len(), 4);
        assert_eq!(cells.len(), 2);
        assert_eq!(nodes[&4].y, 100.0);
        assert!(warnings.iter().any(|warning| warning.contains("nonzero Z")));
        assert_eq!(cells[0].node_ids.len(), 3);
    }

    #[test]
    fn parses_legacy_dataset15_inline_node_records() {
        let mut source = String::from("    -1\n  15\n");
        for (id, [x, y, z]) in [
            (1, [0.0, 0.0, 0.0]),
            (2, [10.0, 0.0, 0.0]),
            (3, [0.0, 10.0, 0.0]),
        ] {
            source.push_str(&fixed_i10(&[id, 0, 0, 0]));
            source.push_str(&fixed_e13(&[x, y, z]));
            source.push('\n');
        }
        source.push_str("    -1\n    -1\n  2412\n");
        source.push_str(&fixed_i10(&[1, 91, 0, 0, 0, 3]));
        source.push('\n');
        source.push_str(&fixed_i10(&[1, 2, 3]));
        source.push_str("\n    -1\n    -1\n");

        let (nodes, cells, warnings) = parse_unv(&source).unwrap();
        assert_eq!(nodes.len(), 3);
        assert_eq!(nodes[&2].x, 10.0);
        assert_eq!(cells.len(), 1);
        assert!(warnings.is_empty());
    }

    #[test]
    fn projects_volume_elements_to_outline_edges() {
        for (descriptor, nodes, expected_edges) in [
            (111, vec![1, 2, 3, 4], 6),
            (112, vec![1, 2, 3, 4, 5, 6], 9),
            (115, vec![1, 2, 3, 4, 5, 6, 7, 8], 12),
        ] {
            let element = RawElement {
                id: 1,
                descriptor,
                node_ids: nodes,
            };
            let (corners, family, _) = element_corners(&element).unwrap().unwrap();
            let edges = match family {
                ElementFamily::Tetrahedron => {
                    edge_cells(&corners, &[[0, 1], [1, 2], [2, 0], [0, 3], [1, 3], [2, 3]])
                }
                ElementFamily::Prism => edge_cells(
                    &corners,
                    &[
                        [0, 1],
                        [1, 2],
                        [2, 0],
                        [3, 4],
                        [4, 5],
                        [5, 3],
                        [0, 3],
                        [1, 4],
                        [2, 5],
                    ],
                ),
                ElementFamily::Hexahedron => edge_cells(
                    &corners,
                    &[
                        [0, 1],
                        [1, 2],
                        [2, 3],
                        [3, 0],
                        [4, 5],
                        [5, 6],
                        [6, 7],
                        [7, 4],
                        [0, 4],
                        [1, 5],
                        [2, 6],
                        [3, 7],
                    ],
                ),
                _ => panic!("unexpected UNV volume family"),
            };
            assert_eq!(edges.len(), expected_edges);
            assert!(edges.iter().all(|edge| edge.node_ids.len() == 2));
        }
    }

    #[test]
    fn maps_known_high_order_unv_nodes_to_corner_geometry() {
        for (descriptor, node_ids, expected) in [
            (42, (1..=6).collect::<Vec<_>>(), vec![1, 3, 5]),
            (45, (1..=8).collect::<Vec<_>>(), vec![1, 7, 5, 3]),
            (
                116,
                (1..=20).collect::<Vec<_>>(),
                vec![1, 7, 19, 13, 3, 5, 17, 15],
            ),
            (118, (1..=10).collect::<Vec<_>>(), vec![1, 3, 5, 10]),
        ] {
            let element = RawElement {
                id: 1,
                descriptor,
                node_ids,
            };
            let (corners, _, high_order) = element_corners(&element).unwrap().unwrap();
            assert_eq!(corners, expected);
            assert!(high_order);
        }
    }

    #[test]
    fn consumes_beam_orientation_record_before_connectivity() {
        let mut source = String::from("    -1\n  2411\n");
        for (id, x) in [(1, 0.0), (2, 10.0)] {
            source.push_str(&fixed_i10(&[id, 0, 0, 0]));
            source.push('\n');
            source.push_str(&fixed_f25(&[x, 0.0, 0.0]));
            source.push('\n');
        }
        source.push_str("    -1\n    -1\n  2412\n");
        source.push_str(&fixed_i10(&[1, 21, 0, 0, 0, 2]));
        source.push('\n');
        source.push_str(&fixed_i10(&[0, 0, 0]));
        source.push('\n');
        source.push_str(&fixed_i10(&[1, 2]));
        source.push_str("\n    -1\n    -1\n");
        let (nodes, cells, _) = parse_unv(&source).unwrap();
        assert_eq!(nodes.len(), 2);
        assert_eq!(cells.len(), 1);
        assert_eq!(cells[0].node_ids, vec![1, 2]);
    }

    #[test]
    fn rejects_non_global_nodes_and_malformed_connectivity() {
        let non_global = fixture(false).replace(
            "         1         0         0         0",
            "         1         1         0         0",
        );
        assert!(matches!(parse_unv(&non_global), Err(Error::Unsupported(_))));

        let malformed = "    -1\n  2411\n         1         0         0         0\n0 0 0\n    -1\n    -1\n  2412\n        10        91         0         0         0         3\n         1         2\n    -1\n";
        assert!(parse_unv(malformed).is_err());
    }

    #[test]
    fn detects_unv_dataset_headers() {
        assert!(looks_like_unv_prefix(b"    -1\n  2411\n"));
        assert!(!looks_like_unv_prefix(b"-1\n151\nnot a mesh"));
    }
}
