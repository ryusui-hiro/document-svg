//! Bounded EnSight Gold ASCII case/geometry reader.
//!
//! EnSight case files are small manifests which reference a geometry sidecar.
//! This module follows only one local geometry model and confines it to the
//! case directory.  It renders the common linear unstructured element blocks
//! through the shared CAE simulation renderer; variable files and solver
//! metadata remain inert.

use std::collections::{HashMap, HashSet};
use std::io::Read;
use std::path::{Path, PathBuf};

use super::{
    MAX_SIMULATION_CELLS, MAX_SIMULATION_POINTS, MeshCell, MeshNode, SimulationParseOutput,
    render_simulation,
};
use crate::convert::{ConvertOptions, PageConsumer, read_limited_file};
use crate::error::{Error, Result};

const MAX_ENSIGHT_LINES: usize = 5_000_000;
const MAX_ENSIGHT_LINE_BYTES: usize = 1 << 20;
const MAX_ENSIGHT_PARTS: usize = 100_000;
const MAX_ENSIGHT_ELEMENTS_PER_BLOCK: usize = 1_000_000;
const MAX_ENSIGHT_CONNECTIVITY: usize = 8_000_000;

pub(super) fn looks_like_prefix(prefix: &[u8]) -> bool {
    let Ok(text) = std::str::from_utf8(prefix) else {
        return false;
    };
    let lower = text.to_ascii_lowercase();
    lower.lines().any(|line| line.trim() == "part")
        && lower.contains("coordinates")
        && lower.contains("node id")
}

pub(super) fn convert_case(
    path: &Path,
    options: &ConvertOptions,
    sink: &mut dyn PageConsumer,
) -> Result<Vec<String>> {
    let case_bytes = read_limited_file(path, options.max_input_bytes, "EnSight case file")?;
    let case_text = String::from_utf8(case_bytes)
        .map_err(|error| Error::InvalidInput(format!("EnSight case file is not UTF-8: {error}")))?;
    let geometry_path = geometry_path_from_case(path, &case_text)?;
    let geometry_bytes = read_limited_file(
        &geometry_path,
        options.max_input_bytes,
        "EnSight geometry file",
    )?;
    let geometry_text = String::from_utf8(geometry_bytes).map_err(|error| {
        Error::InvalidInput(format!("EnSight ASCII geometry is not UTF-8: {error}"))
    })?;
    let (nodes, cells, mut title, mut warnings) = parse_geometry(&geometry_text)?;
    title = format!("EnSight: {}", title.trim());
    warnings.push(format!(
        "EnSight case variable files and solver metadata were not rendered (geometry: {})",
        geometry_path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("geometry")
    ));
    render_simulation((nodes, cells, title, warnings), sink)
}

pub(super) fn convert_geometry<R: Read>(
    input: R,
    options: &ConvertOptions,
    sink: &mut dyn PageConsumer,
) -> Result<Vec<String>> {
    let mut bytes = Vec::new();
    Read::take(input, options.max_input_bytes.saturating_add(1)).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > options.max_input_bytes {
        return Err(Error::LimitExceeded(format!(
            "EnSight geometry exceeds maximum limit of {}",
            options.max_input_bytes
        )));
    }
    let text = String::from_utf8(bytes).map_err(|error| {
        Error::InvalidInput(format!("EnSight ASCII geometry is not UTF-8: {error}"))
    })?;
    let (nodes, cells, title, warnings) = parse_geometry(&text)?;
    render_simulation((nodes, cells, title, warnings), sink)
}

fn geometry_path_from_case(case_path: &Path, text: &str) -> Result<PathBuf> {
    if text.lines().any(|line| line.len() > MAX_ENSIGHT_LINE_BYTES) {
        return Err(Error::LimitExceeded(format!(
            "EnSight case line exceeds {MAX_ENSIGHT_LINE_BYTES} bytes"
        )));
    }
    let lower = text.to_ascii_lowercase();
    if lower.contains("type: ensight gold binary") || lower.contains("type: ensight6 binary") {
        return Err(Error::Unsupported(
            "binary EnSight case files are unsupported; use an ASCII Gold geometry".into(),
        ));
    }
    let mut in_geometry = false;
    let mut model = None;
    for line in text.lines() {
        let trimmed = strip_comment(line).trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let upper = trimmed.to_ascii_uppercase();
        if upper == "GEOMETRY" {
            in_geometry = true;
            continue;
        }
        if in_geometry
            && matches!(
                upper.as_str(),
                "VARIABLE" | "VARIABLES" | "TIME" | "FILE" | "FORMAT"
            )
        {
            break;
        }
        if in_geometry {
            let Some((key, value)) = trimmed.split_once(':') else {
                continue;
            };
            if key.trim().eq_ignore_ascii_case("model")
                || key.trim().eq_ignore_ascii_case("measured")
            {
                let candidate = value.split_whitespace().last().unwrap_or_default();
                if candidate.is_empty() {
                    return Err(Error::InvalidInput(
                        "EnSight GEOMETRY model path is empty".into(),
                    ));
                }
                if model.is_some() {
                    return Err(Error::Unsupported(
                        "multiple EnSight geometry models are unsupported".into(),
                    ));
                }
                model = Some(candidate.to_owned());
            }
        }
    }
    let model = model
        .ok_or_else(|| Error::InvalidInput("EnSight case has no GEOMETRY model entry".into()))?;
    let root = case_path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .canonicalize()?;
    let candidate = Path::new(&model);
    if candidate.is_absolute()
        || model.contains('\0')
        || candidate
            .components()
            .any(|component| matches!(component, std::path::Component::ParentDir))
    {
        return Err(Error::InvalidInput(
            "EnSight geometry path must stay inside the case directory".into(),
        ));
    }
    let resolved = root.join(candidate).canonicalize().map_err(|error| {
        Error::InvalidInput(format!(
            "EnSight geometry sidecar cannot be resolved: {error}"
        ))
    })?;
    if !resolved.starts_with(&root) {
        return Err(Error::InvalidInput(
            "EnSight geometry path escapes the case directory".into(),
        ));
    }
    Ok(resolved)
}

fn parse_geometry(text: &str) -> Result<SimulationParseOutput> {
    let lines = text.lines().map(str::trim_end).collect::<Vec<_>>();
    if lines.len() > MAX_ENSIGHT_LINES {
        return Err(Error::LimitExceeded(format!(
            "EnSight geometry exceeds {MAX_ENSIGHT_LINES} lines"
        )));
    }
    if lines.iter().any(|line| line.len() > MAX_ENSIGHT_LINE_BYTES) {
        return Err(Error::LimitExceeded(format!(
            "EnSight geometry line exceeds {MAX_ENSIGHT_LINE_BYTES} bytes"
        )));
    }
    if lines.len() < 5 {
        return Err(Error::InvalidInput(
            "EnSight geometry header is truncated".into(),
        ));
    }
    let title = lines[0].trim().to_owned();
    // Gold geometry files have two 80-character description lines in the
    // standard form, but exporters occasionally add another description. Find
    // the id-mode keywords instead of relying on fixed offsets.
    let node_mode_index = (0..lines.len().min(16))
        .find(|index| {
            strip_comment(lines[*index])
                .trim()
                .to_ascii_lowercase()
                .starts_with("node id")
        })
        .ok_or_else(|| Error::InvalidInput("EnSight geometry node id mode is missing".into()))?;
    let node_ids_given = parse_id_mode(lines[node_mode_index], "node")?;
    let element_mode_index = next_nonempty(&lines, node_mode_index + 1)
        .ok_or_else(|| Error::InvalidInput("EnSight geometry element id mode is missing".into()))?;
    let element_ids_given = parse_id_mode(lines[element_mode_index], "element")?;
    let mut index = element_mode_index + 1;
    if lines
        .get(index)
        .is_some_and(|line| strip_comment(line).trim().eq_ignore_ascii_case("extents"))
    {
        index += 1;
        let _ = read_values(&lines, &mut index, 6, "extents")?;
    }

    let mut nodes = HashMap::new();
    let mut cells = Vec::new();
    let mut warnings = Vec::new();
    let mut part_count = 0usize;
    let mut connectivity_total = 0usize;
    while let Some(next) = next_nonempty(&lines, index) {
        index = next;
        if !strip_comment(lines[index])
            .trim()
            .eq_ignore_ascii_case("part")
        {
            return Err(Error::InvalidInput(format!(
                "EnSight geometry expected 'part' at line {}",
                index + 1
            )));
        }
        part_count += 1;
        if part_count > MAX_ENSIGHT_PARTS {
            return Err(Error::LimitExceeded(format!(
                "EnSight geometry exceeds {MAX_ENSIGHT_PARTS} parts"
            )));
        }
        index += 1;
        let part_id = parse_usize_line(&lines, &mut index, "part id")?;
        if part_id == 0 {
            warnings.push("EnSight part id 0 was normalized through local node IDs".into());
        }
        let _description = next_required_line(&lines, &mut index, "part description")?;
        let coordinates = next_required_line(&lines, &mut index, "coordinates keyword")?;
        if !coordinates.trim().eq_ignore_ascii_case("coordinates") {
            return Err(Error::InvalidInput(format!(
                "EnSight part {part_id} is missing coordinates section"
            )));
        }
        let count = parse_usize_line(&lines, &mut index, "coordinate count")?;
        if count == 0 || count > MAX_SIMULATION_POINTS {
            return Err(Error::LimitExceeded(format!(
                "EnSight part {part_id} coordinate count {count} is invalid"
            )));
        }
        let source_ids = if node_ids_given {
            read_usize_values(&lines, &mut index, count, "node ids")?
        } else {
            (1..=count).collect()
        };
        let xs = read_values(&lines, &mut index, count, "x coordinates")?;
        let ys = read_values(&lines, &mut index, count, "y coordinates")?;
        let zs = read_values(&lines, &mut index, count, "z coordinates")?;
        let global_offset = nodes.len();
        // Gold connectivity is always 1-based local coordinate-array order;
        // optional node IDs are labels only and are not connectivity keys.
        let mut seen_source_ids = HashSet::with_capacity(count);
        let mut local_to_global = HashMap::with_capacity(count);
        for (local, source_id) in source_ids.into_iter().enumerate() {
            if !seen_source_ids.insert(source_id) {
                return Err(Error::InvalidInput(format!(
                    "EnSight part {part_id} contains duplicate node id {source_id}"
                )));
            }
            local_to_global.insert(local + 1, global_offset + local + 1);
            let x = xs[local];
            let y = ys[local];
            let z = zs[local];
            if !x.is_finite() || !y.is_finite() || !z.is_finite() {
                return Err(Error::InvalidInput(format!(
                    "EnSight part {part_id} contains non-finite coordinates"
                )));
            }
            nodes.insert(global_offset + local + 1, MeshNode { x, y, scalar: None });
        }

        loop {
            let Some(next_block) = next_nonempty(&lines, index) else {
                index = lines.len();
                break;
            };
            index = next_block;
            if strip_comment(lines[index])
                .trim()
                .eq_ignore_ascii_case("part")
            {
                break;
            }
            let element_type = strip_comment(lines[index]).trim().to_ascii_lowercase();
            let corners = match element_type.as_str() {
                "bar2" => 2,
                "tria3" => 3,
                "quad4" => 4,
                "tetra4" => 4,
                "penta6" => 6,
                "pyramid5" => 5,
                "hexa8" => 8,
                "nsided" | "nfaced" => {
                    return Err(Error::Unsupported(format!(
                        "EnSight element type {element_type:?} is unsupported"
                    )));
                }
                _ => {
                    return Err(Error::Unsupported(format!(
                        "EnSight element type {element_type:?} is unsupported"
                    )));
                }
            };
            index += 1;
            let element_count = parse_usize_line(&lines, &mut index, "element count")?;
            if element_count == 0 || element_count > MAX_ENSIGHT_ELEMENTS_PER_BLOCK {
                return Err(Error::LimitExceeded(format!(
                    "EnSight {element_type} element count {element_count} is invalid"
                )));
            }
            if element_ids_given {
                let _ = read_usize_values(&lines, &mut index, element_count, "element ids")?;
            }
            let references = element_count.checked_mul(corners).ok_or_else(|| {
                Error::LimitExceeded("EnSight connectivity size overflowed".into())
            })?;
            connectivity_total = connectivity_total.checked_add(references).ok_or_else(|| {
                Error::LimitExceeded("EnSight connectivity size overflowed".into())
            })?;
            if connectivity_total > MAX_ENSIGHT_CONNECTIVITY {
                return Err(Error::LimitExceeded(format!(
                    "EnSight connectivity exceeds {MAX_ENSIGHT_CONNECTIVITY} references"
                )));
            }
            let connectivity = read_usize_values(&lines, &mut index, references, "connectivity")?;
            for chunk in connectivity.chunks_exact(corners) {
                let mut ids = Vec::with_capacity(corners);
                for source_id in chunk {
                    let Some(global_id) = local_to_global.get(source_id) else {
                        return Err(Error::InvalidInput(format!(
                            "EnSight {element_type} references unknown node {source_id}"
                        )));
                    };
                    ids.push(*global_id);
                }
                cells.push(MeshCell {
                    node_ids: ids,
                    scalar: None,
                });
                if cells.len() > MAX_SIMULATION_CELLS {
                    return Err(Error::LimitExceeded(format!(
                        "EnSight geometry exceeds {MAX_SIMULATION_CELLS} cells"
                    )));
                }
            }
        }
    }
    if nodes.is_empty() || cells.is_empty() {
        return Err(Error::InvalidInput(
            "EnSight geometry contains no renderable cells".into(),
        ));
    }
    Ok((nodes, cells, title, warnings))
}

fn parse_id_mode(line: &str, expected: &str) -> Result<bool> {
    let lower = strip_comment(line).trim().to_ascii_lowercase();
    if !lower.starts_with(expected) || !lower.contains("id") {
        return Err(Error::InvalidInput(format!(
            "EnSight geometry header is missing {expected} id mode"
        )));
    }
    Ok(lower.contains("given"))
}

fn next_nonempty(lines: &[&str], mut index: usize) -> Option<usize> {
    while index < lines.len() {
        let line = strip_comment(lines[index]).trim();
        if !line.is_empty() && !line.starts_with('#') {
            return Some(index);
        }
        index += 1;
    }
    None
}

fn next_required_line<'a>(lines: &[&'a str], index: &mut usize, context: &str) -> Result<&'a str> {
    let Some(next) = next_nonempty(lines, *index) else {
        return Err(Error::InvalidInput(format!("EnSight {context} is missing")));
    };
    *index = next + 1;
    Ok(lines[next])
}

fn parse_usize_line(lines: &[&str], index: &mut usize, context: &str) -> Result<usize> {
    let line = next_required_line(lines, index, context)?;
    let token = strip_comment(line)
        .split_whitespace()
        .next()
        .ok_or_else(|| Error::InvalidInput(format!("EnSight {context} is empty")))?;
    token
        .parse::<usize>()
        .map_err(|_| Error::InvalidInput(format!("EnSight {context} {token:?} is not an integer")))
}

fn read_values(lines: &[&str], index: &mut usize, count: usize, context: &str) -> Result<Vec<f64>> {
    let mut result = Vec::with_capacity(count);
    while result.len() < count {
        let line = next_required_line(lines, index, context)?;
        for token in strip_comment(line).split_whitespace() {
            let value = token.parse::<f64>().map_err(|_| {
                Error::InvalidInput(format!("EnSight {context} token {token:?} is not numeric"))
            })?;
            if !value.is_finite() {
                return Err(Error::InvalidInput(format!(
                    "EnSight {context} contains a non-finite value"
                )));
            }
            result.push(value);
            if result.len() > count {
                return Err(Error::InvalidInput(format!(
                    "EnSight {context} contains more values than declared"
                )));
            }
        }
    }
    Ok(result)
}

fn strip_comment(line: &str) -> &str {
    line.split('#').next().unwrap_or_default()
}

fn read_usize_values(
    lines: &[&str],
    index: &mut usize,
    count: usize,
    context: &str,
) -> Result<Vec<usize>> {
    let values = read_values(lines, index, count, context)?;
    values
        .into_iter()
        .map(|value| {
            let integer = value as usize;
            if value < 0.0 || value != integer as f64 {
                return Err(Error::InvalidInput(format!(
                    "EnSight {context} contains invalid integer {value}"
                )));
            }
            Ok(integer)
        })
        .collect()
}
