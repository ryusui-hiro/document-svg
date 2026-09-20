use std::collections::{HashMap, HashSet};
use std::io::Read;

use crate::cad::dxf::geometry::parse_cad_float;
use crate::convert::{ConvertOptions, PageConsumer};
use crate::error::{Error, Result};

use super::{
    MAX_SIMULATION_CELLS, MAX_SIMULATION_POINTS, MAX_VTK_RENDERED_PRIMITIVES, MeshNode,
    SimulationParseOutput, render_simulation, vtk_cell_primitives,
};

const MAX_LSDYNA_LINES: usize = 5_000_000;
const MAX_LSDYNA_LINE_BYTES: usize = 1024 * 1024;
const MAX_LSDYNA_FIELDS_PER_CARD: usize = 128;
const MAX_SOLID_NODE_FIELDS: usize = 64;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Section {
    Ignore,
    Title,
    Nodes,
    Shell { use_midside_fields: bool },
    Solid(SolidKind),
    Beam,
    UnsupportedNode,
    UnsupportedElement,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SolidKind {
    Infer,
    Tetrahedron,
    Wedge,
    Hexahedron,
}

#[derive(Clone, Copy)]
struct SolidTopology {
    vtk_type: usize,
    corner_indices: &'static [usize],
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
            "LS-DYNA input exceeds maximum limit of {}",
            options.max_input_bytes
        )));
    }
    let text = String::from_utf8(bytes).map_err(|error| {
        Error::InvalidInput(format!("LS-DYNA Keyword input must be UTF-8: {error}"))
    })?;
    let text = text.strip_prefix('\u{feff}').unwrap_or(&text);
    render_simulation(parse_keyword_deck(text)?, sink)
}

fn parse_keyword_deck(text: &str) -> Result<SimulationParseOutput> {
    if text.lines().count() > MAX_LSDYNA_LINES {
        return Err(Error::LimitExceeded(format!(
            "LS-DYNA input exceeds {MAX_LSDYNA_LINES} lines"
        )));
    }
    let lines = text.lines().collect::<Vec<_>>();
    let mut nodes = HashMap::new();
    let mut node_ids = HashSet::new();
    let mut title = None;
    let mut has_nonzero_z = false;

    // Nodes are read before elements so valid keyword files may order those blocks either way.
    let mut section = Section::Ignore;
    for (line_index, raw_line) in lines.iter().enumerate() {
        let line = checked_line(raw_line, line_index + 1)?;
        if is_comment_or_blank(line) {
            continue;
        }
        if let Some((keyword, long_format)) = parse_keyword(line) {
            if keyword == "END" {
                break;
            }
            section = section_for_keyword(&keyword);
            if section == Section::Nodes && long_format {
                return Err(Error::Unsupported(
                    "LS-DYNA *NODE long-format records are not supported".into(),
                ));
            }
            continue;
        }

        match section {
            Section::Title => {
                title = Some(line.trim().to_string());
                section = Section::Ignore;
            }
            Section::Nodes => {
                let fields = parse_data_fields(line, line_index + 1)?;
                if fields.len() < 4 {
                    return Err(Error::InvalidInput(format!(
                        "LS-DYNA *NODE record at line {} requires NID and X/Y/Z",
                        line_index + 1
                    )));
                }
                let id = parse_positive_id(field(&fields, 0), "node ID", line_index + 1)?;
                if !node_ids.insert(id) {
                    return Err(Error::InvalidInput(format!(
                        "duplicate LS-DYNA node ID {id}"
                    )));
                }
                if nodes.len() >= MAX_SIMULATION_POINTS {
                    return Err(Error::LimitExceeded(format!(
                        "LS-DYNA node count exceeds {MAX_SIMULATION_POINTS}"
                    )));
                }
                let x = parse_real(field(&fields, 1), "node X", line_index + 1)?;
                let y = parse_real(field(&fields, 2), "node Y", line_index + 1)?;
                let z = parse_real(field(&fields, 3), "node Z", line_index + 1)?;
                has_nonzero_z |= z.abs() > 1.0e-12;
                nodes.insert(id, MeshNode { x, y, scalar: None });
            }
            _ => {}
        }
    }

    let mut cells = Vec::new();
    let mut element_ids = HashSet::new();
    let mut include_count = 0usize;
    let mut unsupported_element_blocks = 0usize;
    let mut unsupported_element_examples = Vec::<String>::new();
    let mut unsupported_node_blocks = 0usize;
    let mut unsupported_node_examples = Vec::<String>::new();
    let mut omitted_midside_nodes = false;
    section = Section::Ignore;

    for (line_index, raw_line) in lines.iter().enumerate() {
        let line = checked_line(raw_line, line_index + 1)?;
        if is_comment_or_blank(line) {
            continue;
        }
        if let Some((keyword, long_format)) = parse_keyword(line) {
            if keyword == "END" {
                break;
            }
            if is_external_include_keyword(&keyword) {
                include_count += 1;
            }
            section = section_for_keyword(&keyword);
            if long_format
                && matches!(
                    section,
                    Section::Nodes | Section::Shell { .. } | Section::Solid(_) | Section::Beam
                )
            {
                return Err(Error::Unsupported(format!(
                    "LS-DYNA *{keyword} long-format records are not supported"
                )));
            }
            if section == Section::UnsupportedElement {
                unsupported_element_blocks += 1;
                if unsupported_element_examples.len() < 8
                    && !unsupported_element_examples.contains(&keyword)
                {
                    unsupported_element_examples.push(keyword.clone());
                }
            }
            if section == Section::UnsupportedNode {
                unsupported_node_blocks += 1;
                if unsupported_node_examples.len() < 8
                    && !unsupported_node_examples.contains(&keyword)
                {
                    unsupported_node_examples.push(keyword.clone());
                }
            }
            continue;
        }

        let spec = match section {
            Section::Shell { use_midside_fields } => Some(parse_shell_element(
                line,
                line_index + 1,
                use_midside_fields,
            )?),
            Section::Solid(kind) => Some(parse_solid_element(line, line_index + 1, kind)?),
            Section::Beam => Some(parse_beam_element(line, line_index + 1)?),
            _ => None,
        };
        let Some((element_id, node_labels, vtk_type, midside_nodes)) = spec else {
            continue;
        };
        if !element_ids.insert(element_id) {
            return Err(Error::InvalidInput(format!(
                "duplicate LS-DYNA element ID {element_id}"
            )));
        }
        if element_ids.len() > MAX_SIMULATION_CELLS {
            return Err(Error::LimitExceeded(format!(
                "LS-DYNA element count exceeds {MAX_SIMULATION_CELLS}"
            )));
        }
        for node_id in &node_labels {
            if !nodes.contains_key(node_id) {
                return Err(Error::InvalidInput(format!(
                    "LS-DYNA element {element_id} references unknown node {node_id}"
                )));
            }
        }
        let primitives = vtk_cell_primitives(vtk_type, &node_labels, None)?;
        if cells.len().saturating_add(primitives.len()) > MAX_VTK_RENDERED_PRIMITIVES {
            return Err(Error::LimitExceeded(format!(
                "LS-DYNA rendered primitive count exceeds {MAX_VTK_RENDERED_PRIMITIVES}"
            )));
        }
        cells.extend(primitives);
        omitted_midside_nodes |= midside_nodes;
    }

    if nodes.is_empty() || cells.is_empty() {
        return Err(Error::Unsupported(
            "LS-DYNA Keyword input has no supported *NODE and *ELEMENT_SHELL, *ELEMENT_SOLID, or *ELEMENT_BEAM mesh; external INCLUDE files are not read".into(),
        ));
    }
    let mut warnings = Vec::new();
    if include_count > 0 {
        warnings.push(format!(
            "LS-DYNA input references {include_count} external INCLUDE file(s); they were not read"
        ));
    }
    if unsupported_element_blocks > 0 {
        warnings.push(format!(
            "LS-DYNA skipped {unsupported_element_blocks} unsupported element keyword block(s) (examples: {})",
            unsupported_element_examples.join(", ")
        ));
    }
    if unsupported_node_blocks > 0 {
        warnings.push(format!(
            "LS-DYNA skipped {unsupported_node_blocks} unsupported node keyword block(s) (examples: {})",
            unsupported_node_examples.join(", ")
        ));
    }
    if omitted_midside_nodes {
        warnings.push(
            "LS-DYNA higher-order element midside nodes were omitted; straight corner-node edges are shown".into(),
        );
    }
    if has_nonzero_z {
        warnings.push(
            "LS-DYNA 3D coordinates are shown in the XY projection; Z depth is not represented"
                .into(),
        );
    }
    Ok((
        nodes,
        cells,
        title.unwrap_or_else(|| "LS-DYNA Keyword mesh".into()),
        warnings,
    ))
}

fn checked_line(line: &str, line_number: usize) -> Result<&str> {
    let line = line.trim_end_matches('\r');
    if line.len() > MAX_LSDYNA_LINE_BYTES {
        return Err(Error::LimitExceeded(format!(
            "LS-DYNA line {line_number} exceeds {MAX_LSDYNA_LINE_BYTES} bytes"
        )));
    }
    Ok(line)
}

fn is_comment_or_blank(line: &str) -> bool {
    line.trim().is_empty() || line.trim_start().starts_with('$')
}

fn parse_keyword(line: &str) -> Option<(String, bool)> {
    let trimmed = line.trim();
    if !trimmed.starts_with('*') {
        return None;
    }
    let token = trimmed.split_whitespace().next()?;
    let token = token.split(',').next().unwrap_or(token);
    let raw_keyword = token.trim_start_matches('*');
    let long_format = raw_keyword.ends_with('+')
        || trimmed
            .strip_prefix(token)
            .unwrap_or_default()
            .split_whitespace()
            .next()
            .is_some_and(|modifier| modifier == "+");
    let keyword = raw_keyword
        .trim_end_matches(['+', '-', '%'])
        .to_ascii_uppercase();
    Some((keyword, long_format))
}

fn section_for_keyword(keyword: &str) -> Section {
    match keyword {
        "TITLE" => Section::Title,
        "NODE" => Section::Nodes,
        "ELEMENT_SHELL" => Section::Shell {
            use_midside_fields: true,
        },
        "ELEMENT_SOLID" => Section::Solid(SolidKind::Infer),
        name if name.starts_with("ELEMENT_SOLID_") => {
            let options = &name["ELEMENT_SOLID_".len()..];
            if options.contains("TET4TOTET10")
                || options
                    .split('_')
                    .any(|option| matches!(option, "T15" | "T20"))
            {
                Section::Solid(SolidKind::Tetrahedron)
            } else if options
                .split('_')
                .any(|option| matches!(option, "P21" | "P40"))
            {
                Section::Solid(SolidKind::Wedge)
            } else if options.starts_with("H8TOH")
                || options
                    .split('_')
                    .any(|option| matches!(option, "H20" | "H27" | "H64"))
            {
                Section::Solid(SolidKind::Hexahedron)
            } else {
                Section::UnsupportedElement
            }
        }
        "ELEMENT_BEAM" => Section::Beam,
        name if name.starts_with("NODE_") => Section::UnsupportedNode,
        name if name.starts_with("ELEMENT_") => Section::UnsupportedElement,
        _ => Section::Ignore,
    }
}

fn is_external_include_keyword(keyword: &str) -> bool {
    keyword == "INCLUDE"
        || (keyword.starts_with("INCLUDE_") && !keyword.starts_with("INCLUDE_PATH"))
}

fn parse_data_fields(line: &str, line_number: usize) -> Result<Vec<&str>> {
    let mut fields = if line.contains(',') {
        let mut values = line.split(',');
        let bounded = values
            .by_ref()
            .take(MAX_LSDYNA_FIELDS_PER_CARD + 1)
            .collect::<Vec<_>>();
        if values.next().is_some() || bounded.len() > MAX_LSDYNA_FIELDS_PER_CARD {
            return Err(Error::LimitExceeded(format!(
                "LS-DYNA data card at line {line_number} exceeds {MAX_LSDYNA_FIELDS_PER_CARD} fields"
            )));
        }
        bounded
    } else {
        let mut values = line.split_whitespace();
        let bounded = values
            .by_ref()
            .take(MAX_LSDYNA_FIELDS_PER_CARD + 1)
            .collect::<Vec<_>>();
        if values.next().is_some() || bounded.len() > MAX_LSDYNA_FIELDS_PER_CARD {
            return Err(Error::LimitExceeded(format!(
                "LS-DYNA data card at line {line_number} exceeds {MAX_LSDYNA_FIELDS_PER_CARD} fields"
            )));
        }
        bounded
    };
    for value in &mut fields {
        *value = value.trim();
    }
    Ok(fields)
}

fn field<'a>(fields: &[&'a str], index: usize) -> &'a str {
    fields.get(index).copied().unwrap_or_default().trim()
}

fn parse_positive_id(value: &str, context: &str, line_number: usize) -> Result<usize> {
    let value = value.trim();
    let id = value.parse::<usize>().map_err(|_| {
        Error::InvalidInput(format!(
            "invalid LS-DYNA {context} '{value}' at line {line_number}"
        ))
    })?;
    if id == 0 {
        return Err(Error::InvalidInput(format!(
            "LS-DYNA {context} must be positive at line {line_number}"
        )));
    }
    Ok(id)
}

fn parse_real(value: &str, context: &str, line_number: usize) -> Result<f64> {
    let value = value.trim();
    let normalized = value.replace(['D', 'd'], "E");
    let parsed = parse_cad_float(&normalized).ok_or_else(|| {
        Error::InvalidInput(format!(
            "invalid LS-DYNA {context} '{value}' at line {line_number}"
        ))
    })?;
    if !parsed.is_finite() || parsed.abs() > 1.0e12 {
        return Err(Error::InvalidInput(format!(
            "LS-DYNA {context} is non-finite or outside ±1e12 at line {line_number}"
        )));
    }
    Ok(parsed)
}

type ParsedElement = (usize, Vec<usize>, usize, bool);

fn parse_shell_element(
    line: &str,
    line_number: usize,
    use_midside_fields: bool,
) -> Result<ParsedElement> {
    let fields = parse_data_fields(line, line_number)?;
    if fields.len() < 5 {
        return Err(Error::InvalidInput(format!(
            "LS-DYNA *ELEMENT_SHELL record at line {line_number} requires EID, PID, and three nodes"
        )));
    }
    let element_id = parse_positive_id(field(&fields, 0), "element ID", line_number)?;
    let node1 = parse_positive_id(field(&fields, 2), "shell node ID", line_number)?;
    let node2 = parse_positive_id(field(&fields, 3), "shell node ID", line_number)?;
    let node3 = parse_positive_id(field(&fields, 4), "shell node ID", line_number)?;
    let node4 = optional_node_id(field(&fields, 5), "shell node ID", line_number)?;
    let (corners, triangular) = match node4 {
        None => (vec![node1, node2, node3], true),
        Some(node4) if node4 == node3 => (vec![node1, node2, node3], true),
        Some(node4) => (vec![node1, node2, node3, node4], false),
    };
    let vtk_type = if triangular { 5 } else { 9 };
    let midside_nodes = use_midside_fields
        && fields.get(6..10).is_some_and(|nodes| {
            nodes
                .iter()
                .any(|value| !value.trim().is_empty() && *value != "0")
        });
    Ok((element_id, corners, vtk_type, midside_nodes))
}

fn parse_beam_element(line: &str, line_number: usize) -> Result<ParsedElement> {
    let fields = parse_data_fields(line, line_number)?;
    if fields.len() < 4 {
        return Err(Error::InvalidInput(format!(
            "LS-DYNA *ELEMENT_BEAM record at line {line_number} requires EID, PID, and two nodes"
        )));
    }
    let element_id = parse_positive_id(field(&fields, 0), "element ID", line_number)?;
    let node1 = parse_positive_id(field(&fields, 2), "beam node ID", line_number)?;
    let node2 = parse_positive_id(field(&fields, 3), "beam node ID", line_number)?;
    Ok((element_id, vec![node1, node2], 3, false))
}

fn parse_solid_element(line: &str, line_number: usize, kind: SolidKind) -> Result<ParsedElement> {
    let fields = parse_data_fields(line, line_number)?;
    if fields.len() < 6 {
        return Err(Error::InvalidInput(format!(
            "LS-DYNA *ELEMENT_SOLID record at line {line_number} requires EID, PID, and at least four nodes"
        )));
    }
    let element_id = parse_positive_id(field(&fields, 0), "element ID", line_number)?;
    let mut node_fields = Vec::new();
    for value in fields
        .get(2..)
        .unwrap_or_default()
        .iter()
        .take(MAX_SOLID_NODE_FIELDS)
    {
        let value = value.trim();
        if value.is_empty() {
            break;
        }
        match value.parse::<usize>() {
            Ok(0) => node_fields.push(None),
            Ok(node_id) => node_fields.push(Some(node_id)),
            Err(_) => break,
        }
    }
    if node_fields.len() < 4 {
        return Err(Error::InvalidInput(format!(
            "LS-DYNA *ELEMENT_SOLID record at line {line_number} has fewer than four node fields"
        )));
    }
    while node_fields.last().is_some_and(Option::is_none) {
        node_fields.pop();
    }
    let unique_nodes = node_fields
        .iter()
        .flatten()
        .copied()
        .collect::<HashSet<_>>();
    let topology = solid_topology(node_fields.len(), unique_nodes.len(), kind).ok_or_else(|| {
        Error::InvalidInput(format!(
            "LS-DYNA *ELEMENT_SOLID record at line {line_number} has unsupported node connectivity"
        ))
    })?;
    let mut corners = Vec::with_capacity(topology.corner_indices.len());
    for index in topology.corner_indices {
        let node_id = node_fields
            .get(*index)
            .and_then(|node| *node)
            .ok_or_else(|| {
                Error::InvalidInput(format!(
                    "LS-DYNA *ELEMENT_SOLID record at line {line_number} has a missing corner node"
                ))
            })?;
        corners.push(node_id);
    }
    let midside_nodes = unique_nodes.len() > corners.len();
    Ok((element_id, corners, topology.vtk_type, midside_nodes))
}

fn solid_topology(
    node_field_count: usize,
    unique_node_count: usize,
    kind: SolidKind,
) -> Option<SolidTopology> {
    const TETRA: &[usize] = &[0, 1, 2, 3];
    const PYRAMID: &[usize] = &[0, 1, 2, 3, 4];
    const WEDGE_PADDED: &[usize] = &[0, 1, 2, 3, 4, 6];
    const WEDGE: &[usize] = &[0, 1, 2, 3, 4, 5];
    const HEX: &[usize] = &[0, 1, 2, 3, 4, 5, 6, 7];

    Some(match kind {
        SolidKind::Tetrahedron if node_field_count >= 4 => SolidTopology {
            vtk_type: 10,
            corner_indices: TETRA,
        },
        SolidKind::Wedge if node_field_count >= 6 => SolidTopology {
            vtk_type: 13,
            corner_indices: WEDGE,
        },
        SolidKind::Hexahedron if node_field_count >= 8 => SolidTopology {
            vtk_type: 12,
            corner_indices: HEX,
        },
        SolidKind::Tetrahedron | SolidKind::Wedge | SolidKind::Hexahedron => return None,
        SolidKind::Infer => match (node_field_count, unique_node_count) {
            (4..=8, 4) => SolidTopology {
                vtk_type: 10,
                corner_indices: TETRA,
            },
            (5..=8, 5) => SolidTopology {
                vtk_type: 14,
                corner_indices: PYRAMID,
            },
            (6, 6) => SolidTopology {
                vtk_type: 13,
                corner_indices: WEDGE,
            },
            (7..=8, 6) => SolidTopology {
                vtk_type: 13,
                corner_indices: WEDGE_PADDED,
            },
            (8, 8) => SolidTopology {
                vtk_type: 12,
                corner_indices: HEX,
            },
            (10, 10) | (15, 15) => SolidTopology {
                vtk_type: 10,
                corner_indices: TETRA,
            },
            (21 | 40, _) => SolidTopology {
                vtk_type: 13,
                corner_indices: WEDGE,
            },
            (27 | 64, _) => SolidTopology {
                vtk_type: 12,
                corner_indices: HEX,
            },
            // A 20-node row is ambiguous without its *SECTION_SOLID formulation.
            _ => return None,
        },
    })
}

fn optional_node_id(value: &str, context: &str, line_number: usize) -> Result<Option<usize>> {
    let value = value.trim();
    if value.is_empty() || value == "0" {
        return Ok(None);
    }
    parse_positive_id(value, context, line_number).map(Some)
}

#[cfg(test)]
mod tests {
    use super::{SolidKind, parse_keyword_deck, solid_topology};
    use crate::error::Error;

    #[test]
    fn parses_standard_keyword_shell_mesh_with_title_and_nodes_after_elements() {
        let deck = "*KEYWORD\n*TITLE\nLS-DYNA shell example\n*ELEMENT_SHELL\n         1         1         1         2         3         4\n*NODE\n         1             0.0             0.0             0.0\n         2           100.0             0.0             0.0\n         3           100.0            50.0             0.0\n         4             0.0            50.0             0.0\n*END\n";
        let (nodes, cells, title, warnings) = parse_keyword_deck(deck).unwrap();
        assert_eq!(nodes.len(), 4);
        assert_eq!(cells.len(), 1);
        assert_eq!(title, "LS-DYNA shell example");
        assert!(warnings.is_empty());
    }

    #[test]
    fn parses_free_field_shell_beam_and_solid_cards_and_reports_includes() {
        let deck = "*KEYWORD\n*NODE\n1,0,0,0\n2,100,0,0\n3,100,50,0\n4,0,50,1\n5,0,0,100\n6,100,0,100\n7,100,50,100\n8,0,50,100\n*ELEMENT_SHELL\n10,1,1,2,3,3,0,0,0,0\n*ELEMENT_BEAM\n11,1,1,2,3\n*ELEMENT_SOLID\n12,1,1,2,3,4,5,6,7,8\n*INCLUDE\nexternal.k\n*ELEMENT_DISCRETE\n20,1,1,2\n*END\n";
        let (nodes, cells, _, warnings) = parse_keyword_deck(deck).unwrap();
        assert_eq!(nodes.len(), 8);
        assert_eq!(cells.len(), 14);
        assert!(warnings.iter().any(|warning| warning.contains("INCLUDE")));
        assert!(
            warnings
                .iter()
                .any(|warning| warning.contains("unsupported element"))
        );
        assert!(
            warnings
                .iter()
                .any(|warning| warning.contains("XY projection"))
        );
    }

    #[test]
    fn maps_degenerate_tetra_pyramid_and_wedge_connectivity() {
        assert_eq!(solid_topology(8, 4, SolidKind::Infer).unwrap().vtk_type, 10);
        assert_eq!(solid_topology(8, 5, SolidKind::Infer).unwrap().vtk_type, 14);
        assert_eq!(solid_topology(8, 6, SolidKind::Infer).unwrap().vtk_type, 13);
        assert_eq!(solid_topology(8, 8, SolidKind::Infer).unwrap().vtk_type, 12);
        assert_eq!(
            solid_topology(10, 10, SolidKind::Infer)
                .unwrap()
                .corner_indices,
            &[0, 1, 2, 3]
        );
        assert_eq!(
            solid_topology(15, 15, SolidKind::Infer)
                .unwrap()
                .corner_indices,
            &[0, 1, 2, 3]
        );
        assert_eq!(
            solid_topology(21, 21, SolidKind::Infer)
                .unwrap()
                .corner_indices,
            &[0, 1, 2, 3, 4, 5]
        );
        assert_eq!(
            solid_topology(40, 40, SolidKind::Infer)
                .unwrap()
                .corner_indices,
            &[0, 1, 2, 3, 4, 5]
        );
        assert_eq!(
            solid_topology(64, 64, SolidKind::Infer)
                .unwrap()
                .corner_indices,
            &[0, 1, 2, 3, 4, 5, 6, 7]
        );
        assert!(solid_topology(20, 20, SolidKind::Infer).is_none());
        assert_eq!(
            solid_topology(20, 20, SolidKind::Hexahedron)
                .unwrap()
                .corner_indices,
            &[0, 1, 2, 3, 4, 5, 6, 7]
        );
        assert_eq!(
            solid_topology(20, 20, SolidKind::Tetrahedron)
                .unwrap()
                .corner_indices,
            &[0, 1, 2, 3]
        );
        assert!(solid_topology(13, 13, SolidKind::Infer).is_none());
        assert!(solid_topology(8, 7, SolidKind::Infer).is_none());
    }

    #[test]
    fn parses_explicit_high_order_solid_formulation_cards() {
        let mut lines = vec!["*KEYWORD".to_string(), "*NODE".to_string()];
        for id in 1..=64 {
            lines.push(format!("{id}, {}, 0, 0", id as f64));
        }
        for (keyword, element_id, node_count) in [
            ("*ELEMENT_SOLID_H20", 100, 20),
            ("*ELEMENT_SOLID_T20", 101, 20),
            ("*ELEMENT_SOLID_P21", 102, 21),
            ("*ELEMENT_SOLID_P40", 103, 40),
            ("*ELEMENT_SOLID_H27", 104, 27),
            ("*ELEMENT_SOLID_H64", 105, 64),
        ] {
            lines.push(keyword.into());
            let nodes = (1..=node_count)
                .map(|node_id| node_id.to_string())
                .collect::<Vec<_>>()
                .join(",");
            lines.push(format!("{element_id},1,{nodes}"));
        }
        lines.push("*END".into());

        let (nodes, cells, _, warnings) = parse_keyword_deck(&lines.join("\n")).unwrap();
        assert_eq!(nodes.len(), 64);
        assert_eq!(cells.len(), 60);
        assert!(warnings.iter().any(|warning| warning.contains("midside")));
    }

    #[test]
    fn rejects_long_format_and_unknown_corner_nodes() {
        assert!(matches!(
            parse_keyword_deck("*KEYWORD\n*NODE +\n1,0,0,0\n*END\n"),
            Err(Error::Unsupported(_))
        ));
        assert!(matches!(
            parse_keyword_deck(
                "*KEYWORD\n*NODE\n1,0,0,0\n2,1,0,0\n3,0,1,0\n*ELEMENT_SHELL\n4,1,1,2,99,99\n*END\n"
            ),
            Err(Error::InvalidInput(_))
        ));
    }
}
