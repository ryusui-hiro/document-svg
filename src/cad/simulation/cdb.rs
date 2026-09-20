//! Bounded ANSYS Mechanical APDL coded database (CDB) mesh reader.
//!
//! Only the portable ASCII `NBLOCK` and `EBLOCK` mesh sections are consumed.
//! Commands, loads, materials, components, solver controls and include paths
//! remain inert; no APDL command is executed and no sidecar file is opened.

use std::collections::{HashMap, HashSet};
use std::io::Read;

use crate::convert::{ConvertOptions, PageConsumer};
use crate::error::{Error, Result};

use super::{
    MAX_SIMULATION_CELLS, MAX_SIMULATION_POINTS, MeshCell, MeshNode, SimulationParseOutput,
    render_simulation,
};

const MAX_CDB_LINES: usize = 5_000_000;
const MAX_CDB_LINE_BYTES: usize = 1024 * 1024;
const MAX_CDB_FIELDS: usize = 256;

pub(super) fn looks_like_prefix(prefix: &[u8]) -> bool {
    let text = String::from_utf8_lossy(prefix).to_ascii_uppercase();
    text.lines()
        .any(|line| line.trim_start().starts_with("NBLOCK"))
        && text
            .lines()
            .any(|line| line.trim_start().starts_with("EBLOCK"))
}

pub(super) fn convert<R: Read>(
    input: R,
    options: &ConvertOptions,
    sink: &mut dyn PageConsumer,
) -> Result<Vec<String>> {
    let mut bytes = Vec::new();
    Read::take(input, options.max_input_bytes.saturating_add(1)).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > options.max_input_bytes {
        return Err(Error::LimitExceeded(format!(
            "ANSYS CDB input exceeds maximum limit of {}",
            options.max_input_bytes
        )));
    }
    let text = String::from_utf8(bytes).map_err(|error| {
        Error::InvalidInput(format!("ANSYS CDB input must be UTF-8/ASCII: {error}"))
    })?;
    render_simulation(parse_cdb(&text)?, sink)
}

fn parse_cdb(text: &str) -> Result<SimulationParseOutput> {
    let lines = text.lines().collect::<Vec<_>>();
    if lines.len() > MAX_CDB_LINES {
        return Err(Error::LimitExceeded(format!(
            "ANSYS CDB input exceeds {MAX_CDB_LINES} lines"
        )));
    }
    let mut nodes = HashMap::<usize, MeshNode>::new();
    let mut cells = Vec::<MeshCell>::new();
    let mut node_ids = HashSet::new();
    let mut element_ids = HashSet::new();
    let mut warnings = Vec::new();
    let mut i = 0usize;
    let mut has_nonzero_z = false;
    let mut saw_nblock = false;
    let mut saw_eblock = false;
    let mut skipped_commands = 0usize;

    while i < lines.len() {
        let line = lines[i];
        if line.len() > MAX_CDB_LINE_BYTES {
            return Err(Error::LimitExceeded(format!(
                "ANSYS CDB line {} exceeds {MAX_CDB_LINE_BYTES} bytes",
                i + 1
            )));
        }
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('!') {
            i += 1;
            continue;
        }
        let upper = trimmed.to_ascii_uppercase();
        if upper.starts_with("NBLOCK") {
            saw_nblock = true;
            let (next, count) =
                parse_nblock(&lines, i + 1, &mut nodes, &mut node_ids, &mut has_nonzero_z)?;
            i = next;
            if count > MAX_SIMULATION_POINTS {
                return Err(Error::LimitExceeded(format!(
                    "ANSYS NBLOCK count exceeds {MAX_SIMULATION_POINTS}"
                )));
            }
            continue;
        }
        if upper.starts_with("EBLOCK") {
            saw_eblock = true;
            let (next, count) = parse_eblock(&lines, i + 1, &nodes, &mut cells, &mut element_ids)?;
            i = next;
            if count > MAX_SIMULATION_CELLS {
                return Err(Error::LimitExceeded(format!(
                    "ANSYS EBLOCK count exceeds {MAX_SIMULATION_CELLS}"
                )));
            }
            continue;
        }
        // Common unblocked command syntax is accepted for small hand-authored
        // decks, but it remains a mesh-only preview.
        if upper.starts_with("N,") {
            if let Some((id, x, y, z)) = parse_unblocked_node(trimmed, i + 1)? {
                if nodes.len() >= MAX_SIMULATION_POINTS {
                    return Err(Error::LimitExceeded(format!(
                        "ANSYS node count exceeds {MAX_SIMULATION_POINTS}"
                    )));
                }
                if !node_ids.insert(id) {
                    return Err(Error::InvalidInput(format!("duplicate ANSYS node ID {id}")));
                }
                has_nonzero_z |= z.abs() > 1.0e-12;
                nodes.insert(id, MeshNode { x, y, scalar: None });
            }
        } else if upper.starts_with("EN,") {
            if let Some((id, node_ids_for_cell)) = parse_unblocked_element(trimmed, i + 1)? {
                if cells.len() >= MAX_SIMULATION_CELLS {
                    return Err(Error::LimitExceeded(format!(
                        "ANSYS element count exceeds {MAX_SIMULATION_CELLS}"
                    )));
                }
                if !element_ids.insert(id) {
                    return Err(Error::InvalidInput(format!(
                        "duplicate ANSYS element ID {id}"
                    )));
                }
                if node_ids_for_cell.len() >= 2
                    && node_ids_for_cell
                        .iter()
                        .all(|node| nodes.contains_key(node))
                {
                    cells.push(MeshCell {
                        node_ids: node_ids_for_cell,
                        scalar: None,
                    });
                } else {
                    return Err(Error::InvalidInput(format!(
                        "ANSYS element {id} references an unknown or incomplete node"
                    )));
                }
            }
        } else if upper.starts_with("/INPUT") || upper.starts_with("CDREAD") {
            skipped_commands += 1;
        }
        i += 1;
    }

    if !saw_nblock && !saw_eblock && nodes.is_empty() {
        return Err(Error::InvalidInput(
            "ANSYS CDB input contains no NBLOCK/EBLOCK or N/EN mesh data".into(),
        ));
    }
    if has_nonzero_z {
        warnings
            .push("ANSYS CDB contains nonzero Z coordinates; the mesh is projected onto XY".into());
    }
    if skipped_commands > 0 {
        warnings.push(format!(
            "{skipped_commands} APDL input command(s) were ignored; no command or include was executed"
        ));
    }
    warnings.push("Only ANSYS NBLOCK/EBLOCK (and basic N/EN) mesh data are rendered; loads, materials, components and solver settings remain inert".into());
    Ok((nodes, cells, "ANSYS CDB mesh".into(), warnings))
}

fn parse_nblock(
    lines: &[&str],
    mut index: usize,
    nodes: &mut HashMap<usize, MeshNode>,
    node_ids: &mut HashSet<usize>,
    has_nonzero_z: &mut bool,
) -> Result<(usize, usize)> {
    // The next line is a blocked-format descriptor such as `(3i8,6e21.13)`.
    if index >= lines.len() {
        return Err(Error::InvalidInput(
            "ANSYS NBLOCK format line is missing".into(),
        ));
    }
    index += 1;
    let mut count = 0usize;
    while index < lines.len() {
        let line = lines[index];
        if line.len() > MAX_CDB_LINE_BYTES {
            return Err(Error::LimitExceeded(format!(
                "ANSYS NBLOCK line {} exceeds {MAX_CDB_LINE_BYTES} bytes",
                index + 1
            )));
        }
        if first_fixed_integer(line, 8) == Some(-1) || line.trim() == "-1" {
            return Ok((index + 1, count));
        }
        let (id, x, y, z) = parse_blocked_node(line, index + 1)?;
        if id == 0 {
            return Err(Error::InvalidInput(format!(
                "ANSYS NBLOCK line {} has node ID 0",
                index + 1
            )));
        }
        if count >= MAX_SIMULATION_POINTS {
            return Err(Error::LimitExceeded(format!(
                "ANSYS NBLOCK count exceeds {MAX_SIMULATION_POINTS}"
            )));
        }
        if !node_ids.insert(id) {
            return Err(Error::InvalidInput(format!("duplicate ANSYS node ID {id}")));
        }
        *has_nonzero_z |= z.abs() > 1.0e-12;
        nodes.insert(id, MeshNode { x, y, scalar: None });
        count += 1;
        index += 1;
    }
    Err(Error::InvalidInput(
        "ANSYS NBLOCK is missing its -1 terminator".into(),
    ))
}

fn parse_eblock(
    lines: &[&str],
    mut index: usize,
    nodes: &HashMap<usize, MeshNode>,
    cells: &mut Vec<MeshCell>,
    element_ids: &mut HashSet<usize>,
) -> Result<(usize, usize)> {
    if index >= lines.len() {
        return Err(Error::InvalidInput(
            "ANSYS EBLOCK format line is missing".into(),
        ));
    }
    let descriptor = lines[index].trim().to_ascii_uppercase();
    let compact = descriptor.contains("COMPACT");
    index += 1;
    let mut count = 0usize;
    while index < lines.len() {
        let line = lines[index];
        if line.len() > MAX_CDB_LINE_BYTES {
            return Err(Error::LimitExceeded(format!(
                "ANSYS EBLOCK line {} exceeds {MAX_CDB_LINE_BYTES} bytes",
                index + 1
            )));
        }
        if first_fixed_integer(line, 9) == Some(-1) || line.trim() == "-1" {
            return Ok((index + 1, count));
        }
        let fields = fixed_integer_fields(line, 9)?;
        if fields.is_empty() || fields.len() > MAX_CDB_FIELDS {
            return Err(Error::InvalidInput(format!(
                "ANSYS EBLOCK line {} has invalid fields",
                index + 1
            )));
        }
        let (element_id, node_fields) = if compact {
            (fields[0], fields[1..].to_vec())
        } else if fields.len() >= 12 && fields[8] > 0 {
            let node_count = usize::try_from(fields[8])
                .map_err(|_| Error::InvalidInput("negative ANSYS node count".into()))?;
            let start = 11usize.min(fields.len());
            let end = start.saturating_add(node_count).min(fields.len());
            (fields[10], fields[start..end].to_vec())
        } else {
            (fields[0], fields[1..].to_vec())
        };
        if element_id <= 0 || node_fields.len() < 2 {
            return Err(Error::InvalidInput(format!(
                "ANSYS EBLOCK line {} has incomplete connectivity",
                index + 1
            )));
        }
        let node_ids = node_fields
            .into_iter()
            .filter(|node| *node > 0)
            .map(|node| usize::try_from(node).unwrap_or(0))
            .collect::<Vec<_>>();
        if node_ids.len() < 2 || node_ids.iter().any(|node| !nodes.contains_key(node)) {
            return Err(Error::InvalidInput(format!(
                "ANSYS element {element_id} references an unknown node"
            )));
        }
        if !element_ids.insert(usize::try_from(element_id).unwrap_or(0)) {
            return Err(Error::InvalidInput(format!(
                "duplicate ANSYS element ID {element_id}"
            )));
        }
        cells.push(MeshCell {
            node_ids,
            scalar: None,
        });
        count += 1;
        index += 1;
    }
    Err(Error::InvalidInput(
        "ANSYS EBLOCK is missing its -1 terminator".into(),
    ))
}

fn parse_blocked_node(line: &str, line_number: usize) -> Result<(usize, f64, f64, f64)> {
    let id = first_fixed_integer(line, 8).ok_or_else(|| {
        Error::InvalidInput(format!("ANSYS NBLOCK line {line_number} has no node ID"))
    })?;
    let x = fixed_float(line, 24, 21).or_else(|| whitespace_float(line, 1));
    let y = fixed_float(line, 45, 21).or_else(|| whitespace_float(line, 2));
    let z = fixed_float(line, 66, 21).or_else(|| whitespace_float(line, 3));
    let (Some(x), Some(y), Some(z)) = (x, y, z) else {
        return Err(Error::InvalidInput(format!(
            "ANSYS NBLOCK line {line_number} has incomplete coordinates"
        )));
    };
    Ok((
        usize::try_from(id).map_err(|_| Error::InvalidInput("negative ANSYS node ID".into()))?,
        x,
        y,
        z,
    ))
}

fn parse_unblocked_node(line: &str, line_number: usize) -> Result<Option<(usize, f64, f64, f64)>> {
    let fields = line.split(',').map(str::trim).collect::<Vec<_>>();
    if fields.len() < 5 {
        return Ok(None);
    }
    let id = parse_integer(fields[1], "node ID", line_number)?;
    let x = parse_float(fields[2], "X", line_number)?;
    let y = parse_float(fields[3], "Y", line_number)?;
    let z = parse_float(fields[4], "Z", line_number)?;
    Ok(Some((
        usize::try_from(id).map_err(|_| Error::InvalidInput("negative ANSYS node ID".into()))?,
        x,
        y,
        z,
    )))
}

fn parse_unblocked_element(line: &str, line_number: usize) -> Result<Option<(usize, Vec<usize>)>> {
    let fields = line.split(',').map(str::trim).collect::<Vec<_>>();
    if fields.len() < 3 {
        return Ok(None);
    }
    let id = parse_integer(fields[1], "element ID", line_number)?;
    let mut nodes = Vec::new();
    for field in &fields[2..] {
        if field.is_empty() {
            continue;
        }
        let value = parse_integer(field, "element node ID", line_number)?;
        if value > 0 {
            nodes.push(usize::try_from(value).unwrap_or(0));
        }
    }
    Ok(Some((
        usize::try_from(id).map_err(|_| Error::InvalidInput("negative ANSYS element ID".into()))?,
        nodes,
    )))
}

fn fixed_integer_fields(line: &str, width: usize) -> Result<Vec<i64>> {
    if line.trim().is_empty() {
        return Ok(Vec::new());
    }
    let mut values = Vec::new();
    let mut start = 0usize;
    while start < line.len() && values.len() < MAX_CDB_FIELDS {
        let end = (start + width).min(line.len());
        let field = line.get(start..end).unwrap_or_default().trim();
        if !field.is_empty() {
            values.push(field.parse::<i64>().map_err(|_| {
                Error::InvalidInput(format!("invalid ANSYS integer field `{field}`"))
            })?);
        }
        start = end;
    }
    Ok(values)
}

fn first_fixed_integer(line: &str, width: usize) -> Option<i64> {
    line.get(..width)
        .and_then(|field| field.trim().parse().ok())
}

fn fixed_float(line: &str, start: usize, width: usize) -> Option<f64> {
    line.get(start..start.saturating_add(width))?
        .trim()
        .parse()
        .ok()
}

fn whitespace_float(line: &str, index: usize) -> Option<f64> {
    line.split_whitespace().nth(index)?.parse().ok()
}

fn parse_integer(value: &str, label: &str, line_number: usize) -> Result<i64> {
    value
        .parse()
        .map_err(|_| Error::InvalidInput(format!("invalid ANSYS {label} at line {line_number}")))
}

fn parse_float(value: &str, label: &str, line_number: usize) -> Result<f64> {
    value
        .parse()
        .map_err(|_| Error::InvalidInput(format!("invalid ANSYS {label} at line {line_number}")))
}
