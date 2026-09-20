//! Bounded Tecplot ASCII (`.dat`/`.tec`) finite-element and ordered-zone reader.
//!
//! The reader intentionally handles the geometry-bearing subset used by most
//! mesh previews: POINT and BLOCK packed FE zones (triangle, quadrilateral,
//! tetrahedron and brick) plus ordered I/J grids.  Variables beyond X/Y/Z are
//! treated as an inert scalar field for the shared simulation renderer.

use std::io::Read;

use super::{
    MAX_SIMULATION_CELLS, MAX_SIMULATION_POINTS, MeshCell, MeshNode, SimulationParseOutput,
    render_simulation,
};
use crate::convert::{ConvertOptions, PageConsumer};
use crate::error::{Error, Result};

const MAX_TECPLOT_LINES: usize = 5_000_000;
const MAX_TECPLOT_LINE_BYTES: usize = 1 << 20;
const MAX_TECPLOT_VARIABLES: usize = 256;
const MAX_TECPLOT_ZONES: usize = 256;
const MAX_TECPLOT_DATA_VALUES: usize = 16_000_000;

type ZoneParseOutput = (
    std::collections::HashMap<usize, MeshNode>,
    Vec<MeshCell>,
    Vec<String>,
);

/// Detects the conservative Tecplot ASCII signature used for content sniffing.
pub(super) fn looks_like_prefix(prefix: &[u8]) -> bool {
    let Ok(text) = std::str::from_utf8(prefix) else {
        return false;
    };
    let upper = text.to_ascii_uppercase();
    let has_header = upper.lines().any(|line| {
        let line = line.trim_start();
        line.starts_with("TITLE") || line.starts_with("VARIABLES")
    });
    let has_zone = upper.lines().any(|line| {
        let line = line.trim_start();
        line.starts_with("ZONE") && (line.contains("N=") || line.contains("I="))
    });
    has_header && has_zone
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
            "Tecplot input exceeds maximum limit of {}",
            options.max_input_bytes
        )));
    }
    let text = String::from_utf8(bytes)
        .map_err(|error| Error::InvalidInput(format!("Tecplot ASCII is not UTF-8: {error}")))?;
    let (nodes, cells, title, warnings) = parse(&text)?;
    render_simulation((nodes, cells, title, warnings), sink)
}

fn parse(text: &str) -> Result<SimulationParseOutput> {
    if text.lines().count() > MAX_TECPLOT_LINES {
        return Err(Error::LimitExceeded(format!(
            "Tecplot input exceeds {MAX_TECPLOT_LINES} lines"
        )));
    }
    let lines = text.lines().map(str::trim).collect::<Vec<_>>();
    let mut title = None;
    let mut variables = Vec::new();
    let mut zones = Vec::new();
    let mut index = 0usize;
    while index < lines.len() {
        let line = lines[index];
        if line.len() > MAX_TECPLOT_LINE_BYTES {
            return Err(Error::LimitExceeded(format!(
                "Tecplot line exceeds {MAX_TECPLOT_LINE_BYTES} bytes"
            )));
        }
        if line.is_empty() || line.starts_with('#') {
            index += 1;
            continue;
        }
        let upper = line.to_ascii_uppercase();
        if upper.starts_with("TITLE") {
            title = parse_assignment_value(line, "TITLE").map(unquote);
            index += 1;
            continue;
        }
        if upper.starts_with("VARIABLES") {
            let value = parse_assignment_value(line, "VARIABLES").ok_or_else(|| {
                Error::InvalidInput("Tecplot VARIABLES declaration is missing '='".into())
            })?;
            variables = parse_variables(value)?;
            if variables.is_empty() {
                return Err(Error::InvalidInput(
                    "Tecplot VARIABLES list is empty".into(),
                ));
            }
            index += 1;
            continue;
        }
        if upper.starts_with("ZONE") {
            if zones.len() >= MAX_TECPLOT_ZONES {
                return Err(Error::LimitExceeded(format!(
                    "Tecplot input exceeds {MAX_TECPLOT_ZONES} zones"
                )));
            }
            let (header, next) = collect_zone_header(&lines, index)?;
            let start = next;
            let end = lines[start..]
                .iter()
                .position(|candidate| candidate.to_ascii_uppercase().starts_with("ZONE"))
                .map(|offset| start + offset)
                .unwrap_or(lines.len());
            zones.push((header, start, end));
            index = end;
            continue;
        }
        index += 1;
    }
    if variables.len() < 2 {
        return Err(Error::InvalidInput(
            "Tecplot requires at least X and Y variables".into(),
        ));
    }
    if zones.is_empty() {
        return Err(Error::InvalidInput("Tecplot contains no ZONE".into()));
    }

    let mut nodes = std::collections::HashMap::new();
    let mut cells = Vec::new();
    let mut warnings = Vec::new();
    let mut global_node_offset = 0usize;
    for (zone_index, (header, start, end)) in zones.into_iter().enumerate() {
        let settings = parse_zone_settings(&header)?;
        let data = tokenize_numbers(&lines[start..end])?;
        let (zone_nodes, zone_cells, zone_warnings) = parse_zone(
            &settings,
            &data,
            variables.len(),
            global_node_offset,
            zone_index + 1,
        )?;
        global_node_offset = global_node_offset
            .checked_add(zone_nodes.len())
            .ok_or_else(|| Error::LimitExceeded("Tecplot node count overflowed".into()))?;
        if global_node_offset > MAX_SIMULATION_POINTS {
            return Err(Error::LimitExceeded(format!(
                "Tecplot input exceeds {MAX_SIMULATION_POINTS} nodes"
            )));
        }
        if cells.len().saturating_add(zone_cells.len()) > MAX_SIMULATION_CELLS {
            return Err(Error::LimitExceeded(format!(
                "Tecplot input exceeds {MAX_SIMULATION_CELLS} cells"
            )));
        }
        nodes.extend(zone_nodes);
        cells.extend(zone_cells);
        warnings.extend(zone_warnings);
    }
    if nodes.is_empty() || cells.is_empty() {
        return Err(Error::InvalidInput(
            "Tecplot contains no renderable mesh cells".into(),
        ));
    }
    let title = title.unwrap_or_else(|| "Tecplot simulation".into());
    Ok((nodes, cells, title, warnings))
}

#[derive(Clone, Debug)]
struct ZoneSettings {
    n: usize,
    e: usize,
    i: Option<usize>,
    j: Option<usize>,
    k: Option<usize>,
    packing: Packing,
    zone_type: ZoneType,
    title: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Packing {
    Point,
    Block,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ZoneType {
    Triangle,
    Quadrilateral,
    Tetrahedron,
    Brick,
    Ordered,
}

fn parse_zone_settings(header: &str) -> Result<ZoneSettings> {
    let entries = parse_key_values(header.trim_start_matches("ZONE").trim())?;
    let get = |name: &str| {
        entries
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
    };
    let n = get("N")
        .or_else(|| get("NODES"))
        .and_then(|value| value.parse().ok())
        .unwrap_or(0);
    let e = get("E")
        .or_else(|| get("ELEMENTS"))
        .and_then(|value| value.parse().ok())
        .unwrap_or(0);
    let parse_dim = |name: &str| get(name).and_then(|value| value.parse::<usize>().ok());
    let i = parse_dim("I");
    let j = parse_dim("J");
    let k = parse_dim("K");
    let packing = match get("DATAPACKING")
        .or_else(|| get("F"))
        .unwrap_or("POINT")
        .to_ascii_uppercase()
        .as_str()
    {
        "POINT" | "FEPOINT" => Packing::Point,
        "BLOCK" | "FEBLOCK" => Packing::Block,
        value => {
            return Err(Error::Unsupported(format!(
                "Tecplot data packing {value:?} is unsupported"
            )));
        }
    };
    let zone_type_value = get("ZONETYPE").or_else(|| get("ET"));
    let zone_type = match zone_type_value
        .map(|value| value.to_ascii_uppercase())
        .as_deref()
    {
        None if i.is_some() => ZoneType::Ordered,
        Some("ORDERED") => ZoneType::Ordered,
        Some("FETRIANGLE") | Some("TRIANGLE") => ZoneType::Triangle,
        Some("FEQUADRILATERAL") | Some("QUADRILATERAL") | Some("QUAD") => ZoneType::Quadrilateral,
        Some("FETETRAHEDRON") | Some("TETRAHEDRON") | Some("TETRA") => ZoneType::Tetrahedron,
        Some("FEBRICK") | Some("BRICK") | Some("HEXAHEDRON") | Some("HEX") => ZoneType::Brick,
        Some(value) => {
            return Err(Error::Unsupported(format!(
                "Tecplot zone type {value:?} is unsupported"
            )));
        }
        None => {
            return Err(Error::InvalidInput(
                "Tecplot FE zone is missing ZONETYPE".into(),
            ));
        }
    };
    let n = if zone_type == ZoneType::Ordered {
        i.and_then(|i| j.unwrap_or(1).checked_mul(i))
            .and_then(|value| value.checked_mul(k.unwrap_or(1)))
            .unwrap_or(0)
    } else {
        n
    };
    if n == 0 || n > MAX_SIMULATION_POINTS {
        return Err(Error::LimitExceeded(format!(
            "Tecplot zone node count {n} is invalid"
        )));
    }
    if zone_type != ZoneType::Ordered && (e == 0 || e > MAX_SIMULATION_CELLS) {
        return Err(Error::LimitExceeded(format!(
            "Tecplot zone element count {e} is invalid"
        )));
    }
    Ok(ZoneSettings {
        n,
        e,
        i,
        j,
        k,
        packing,
        zone_type,
        title: get("T").map(unquote),
    })
}

fn parse_zone(
    settings: &ZoneSettings,
    data: &[f64],
    variable_count: usize,
    offset: usize,
    zone_index: usize,
) -> Result<ZoneParseOutput> {
    let coordinate_values = settings
        .n
        .checked_mul(variable_count)
        .ok_or_else(|| Error::LimitExceeded("Tecplot zone data size overflowed".into()))?;
    let connectivity_count = if settings.zone_type == ZoneType::Ordered {
        0
    } else {
        settings
            .e
            .checked_mul(corner_count(settings.zone_type))
            .ok_or_else(|| Error::LimitExceeded("Tecplot connectivity size overflowed".into()))?
    };
    let required = coordinate_values
        .checked_add(connectivity_count)
        .ok_or_else(|| Error::LimitExceeded("Tecplot zone data size overflowed".into()))?;
    if required > MAX_TECPLOT_DATA_VALUES || data.len() < required {
        return Err(Error::InvalidInput(format!(
            "Tecplot zone {zone_index} is truncated: need {required} numeric values, found {}",
            data.len()
        )));
    }
    let mut values = data;
    let mut rows = Vec::with_capacity(settings.n);
    if settings.packing == Packing::Point {
        for _ in 0..settings.n {
            rows.push(values[..variable_count].to_vec());
            values = &values[variable_count..];
        }
    } else {
        // BLOCK stores one complete variable array after another.
        for node in 0..settings.n {
            let mut row = Vec::with_capacity(variable_count);
            for variable in 0..variable_count {
                row.push(data[variable * settings.n + node]);
            }
            rows.push(row);
        }
        values = &data[coordinate_values..];
    }
    let scalar_index = if variable_count > 3 {
        Some(3)
    } else if variable_count == 3 {
        Some(2)
    } else {
        None
    };
    let mut nodes = std::collections::HashMap::with_capacity(settings.n);
    for (local, row) in rows.into_iter().enumerate() {
        let x = row.first().copied().unwrap_or(0.0);
        let y = row.get(1).copied().unwrap_or(0.0);
        if !x.is_finite() || !y.is_finite() {
            return Err(Error::InvalidInput(format!(
                "Tecplot zone {zone_index} has non-finite coordinates"
            )));
        }
        let scalar = scalar_index
            .and_then(|index| row.get(index).copied())
            .filter(|value| value.is_finite());
        nodes.insert(offset + local + 1, MeshNode { x, y, scalar });
    }
    let mut cells = Vec::new();
    let mut warnings = Vec::new();
    if settings.zone_type == ZoneType::Ordered {
        let i = settings.i.unwrap_or(settings.n);
        let j = settings.j.unwrap_or(1);
        let k = settings.k.unwrap_or(1);
        if k > 1 {
            warnings.push(format!(
                "Tecplot ordered zone {zone_index} K dimension is projected onto the first layer"
            ));
        }
        if i < 2 || j < 2 {
            return Err(Error::InvalidInput(format!(
                "Tecplot ordered zone {zone_index} needs I and J >= 2"
            )));
        }
        for row in 0..j - 1 {
            for col in 0..i - 1 {
                let a = offset + row * i + col + 1;
                cells.push(MeshCell {
                    node_ids: vec![a, a + 1, a + i + 1, a + i],
                    scalar: None,
                });
            }
        }
    } else {
        let corners = corner_count(settings.zone_type);
        for _ in 0..settings.e {
            let mut ids = Vec::with_capacity(corners);
            for raw in &values[..corners] {
                let index = *raw as usize;
                if *raw < 1.0 || *raw != index as f64 || index > settings.n {
                    return Err(Error::InvalidInput(format!(
                        "Tecplot zone {zone_index} has invalid connectivity index {raw}"
                    )));
                }
                ids.push(offset + index);
            }
            values = &values[corners..];
            cells.push(MeshCell {
                node_ids: ids,
                scalar: None,
            });
        }
    }
    if settings.title.is_some() {
        warnings.push(format!(
            "Tecplot zone {zone_index} title is retained only in source metadata"
        ));
    }
    if settings.packing == Packing::Block {
        warnings.push(format!(
            "Tecplot zone {zone_index} BLOCK variables were read as numeric arrays"
        ));
    }
    Ok((nodes, cells, warnings))
}

fn corner_count(zone_type: ZoneType) -> usize {
    match zone_type {
        ZoneType::Triangle => 3,
        ZoneType::Quadrilateral => 4,
        ZoneType::Tetrahedron => 4,
        ZoneType::Brick => 8,
        ZoneType::Ordered => 0,
    }
}

fn collect_zone_header(lines: &[&str], start: usize) -> Result<(String, usize)> {
    let mut header = String::new();
    let mut index = start;
    for _ in 0..16 {
        let line = lines.get(index).copied().unwrap_or_default();
        if line.len() > MAX_TECPLOT_LINE_BYTES {
            return Err(Error::LimitExceeded(format!(
                "Tecplot line exceeds {MAX_TECPLOT_LINE_BYTES} bytes"
            )));
        }
        if !header.is_empty() {
            header.push(' ');
        }
        header.push_str(line);
        index += 1;
        let upper = header.to_ascii_uppercase();
        if (upper.contains("N=")
            && (upper.contains("E=") || upper.contains("ZONETYPE") || upper.contains("ET=")))
            || upper.contains("I=")
        {
            return Ok((header, index));
        }
    }
    Err(Error::InvalidInput(
        "Tecplot ZONE header is incomplete".into(),
    ))
}

fn tokenize_numbers(lines: &[&str]) -> Result<Vec<f64>> {
    let mut values = Vec::new();
    for line in lines {
        let line = line.split('#').next().unwrap_or_default();
        for token in
            line.split(|character: char| character.is_ascii_whitespace() || character == ',')
        {
            if token.is_empty() {
                continue;
            }
            let value = token.parse::<f64>().map_err(|_| {
                Error::InvalidInput(format!("Tecplot data token {token:?} is not numeric"))
            })?;
            if !value.is_finite() {
                return Err(Error::InvalidInput(
                    "Tecplot data contains non-finite value".into(),
                ));
            }
            values.push(value);
            if values.len() > MAX_TECPLOT_DATA_VALUES {
                return Err(Error::LimitExceeded(format!(
                    "Tecplot data exceeds {MAX_TECPLOT_DATA_VALUES} values"
                )));
            }
        }
    }
    Ok(values)
}

fn parse_assignment_value<'a>(line: &'a str, key: &str) -> Option<&'a str> {
    line.find('=').and_then(|position| {
        let lhs = line[..position].trim();
        lhs.eq_ignore_ascii_case(key)
            .then_some(line[position + 1..].trim())
    })
}

fn parse_variables(value: &str) -> Result<Vec<String>> {
    let mut result = Vec::new();
    let mut current = String::new();
    let mut quote = None;
    for character in value.chars() {
        if let Some(delimiter) = quote {
            if character == delimiter {
                quote = None;
                if !current.trim().is_empty() {
                    result.push(current.trim().to_owned());
                    current.clear();
                }
            } else {
                current.push(character);
            }
        } else if character == '"' || character == '\'' {
            quote = Some(character);
        } else if character == ',' || character.is_ascii_whitespace() {
            if !current.trim().is_empty() {
                result.push(current.trim().to_owned());
            }
            current.clear();
        } else {
            current.push(character);
        }
    }
    if quote.is_some() {
        return Err(Error::InvalidInput(
            "Tecplot VARIABLES has an unterminated quote".into(),
        ));
    }
    if !current.trim().is_empty() {
        result.push(current.trim().to_owned());
    }
    if result.len() > MAX_TECPLOT_VARIABLES {
        return Err(Error::LimitExceeded(format!(
            "Tecplot exceeds {MAX_TECPLOT_VARIABLES} variables"
        )));
    }
    Ok(result)
}

fn parse_key_values(value: &str) -> Result<Vec<(String, String)>> {
    let mut entries = Vec::new();
    let mut token = String::new();
    let mut quote = None;
    let flush = |token: &mut String, entries: &mut Vec<(String, String)>| -> Result<()> {
        let token = token.trim();
        if token.is_empty() {
            return Ok(());
        }
        let Some(position) = token.find('=') else {
            return Ok(());
        };
        let key = token[..position].trim().to_ascii_uppercase();
        let value = unquote(token[position + 1..].trim());
        entries.push((key, value));
        Ok(())
    };
    for character in value.chars() {
        if let Some(delimiter) = quote {
            token.push(character);
            if character == delimiter {
                quote = None;
            }
        } else if character == '"' || character == '\'' {
            quote = Some(character);
            token.push(character);
        } else if character == ',' || character.is_ascii_whitespace() {
            if token.contains('=') {
                flush(&mut token, &mut entries)?;
                token.clear();
            }
        } else {
            token.push(character);
        }
    }
    flush(&mut token, &mut entries)?;
    Ok(entries)
}

fn unquote(value: &str) -> String {
    let value = value.trim();
    if value.len() >= 2
        && ((value.starts_with('"') && value.ends_with('"'))
            || (value.starts_with('\'') && value.ends_with('\'')))
    {
        value[1..value.len() - 1].to_owned()
    } else {
        value.to_owned()
    }
}
