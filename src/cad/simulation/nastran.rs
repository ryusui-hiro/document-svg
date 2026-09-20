use std::collections::HashSet;
use std::io::Read;

use crate::cad::dxf::geometry::parse_cad_float;
use crate::convert::{ConvertOptions, PageConsumer};
use crate::error::{Error, Result};

use super::{
    MAX_SIMULATION_CELLS, MAX_SIMULATION_POINTS, MAX_VTK_RENDERED_PRIMITIVES, MeshNode,
    SimulationParseOutput, render_simulation, vtk_cell_primitives,
};

const MAX_NASTRAN_LINES: usize = 5_000_000;
const MAX_NASTRAN_LINE_BYTES: usize = 1024 * 1024;
const MAX_NASTRAN_FIELDS_PER_CARD: usize = 128;

struct BdfRecord<'a> {
    name: String,
    data: Vec<&'a str>,
    leading_continuation: Option<&'a str>,
    trailing_continuation: Option<&'a str>,
    is_continuation: bool,
    trailing_comma: bool,
}

#[derive(Clone, Copy)]
struct ElementSpec {
    node_start: usize,
    min_nodes: usize,
    max_nodes: usize,
    linear_vtk_type: usize,
    quadratic_vtk_type: usize,
    linear_corner_count: usize,
    quadratic_corner_count: usize,
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
            "Nastran input exceeds maximum limit of {}",
            options.max_input_bytes
        )));
    }
    let text = String::from_utf8(bytes).map_err(|error| {
        Error::InvalidInput(format!(
            "Nastran Bulk Data input must be UTF-8/ASCII: {error}"
        ))
    })?;
    render_simulation(parse_bdf(&text)?, sink)
}

fn parse_bdf(text: &str) -> Result<SimulationParseOutput> {
    if text.lines().count() > MAX_NASTRAN_LINES {
        return Err(Error::LimitExceeded(format!(
            "Nastran input exceeds {MAX_NASTRAN_LINES} lines"
        )));
    }
    let lines = text.lines().collect::<Vec<_>>();
    let mut nodes = std::collections::HashMap::new();
    let mut cells = Vec::new();
    let mut node_ids = HashSet::new();
    let mut element_ids = HashSet::new();
    let mut warnings = Vec::new();
    let mut included_files = 0usize;
    let mut unsupported_elements = 0usize;
    let mut unsupported_examples = Vec::<String>::new();
    let mut has_nonzero_z = false;
    let mut omitted_midside_nodes = false;

    let mut line_index = 0usize;
    // Read nodes in a dedicated pass because Bulk Data decks may place GRID cards after elements.
    while line_index < lines.len() {
        let Some(record) = parse_bdf_record(lines[line_index], line_index + 1)? else {
            line_index += 1;
            continue;
        };
        if record.is_continuation {
            line_index += 1;
            continue;
        }
        if record.name == "ENDDATA" {
            break;
        }
        if record.name == "GRID" {
            let (fields, last_line, _) = collect_card_fields(&lines, line_index, record, 5)?;
            let id = parse_positive_integer(field(&fields, 0), "GRID ID", line_index + 1)?;
            let cp = parse_integer_or_default(field(&fields, 1), 0, "GRID CP", line_index + 1)?;
            if cp != 0 {
                return Err(Error::Unsupported(format!(
                    "Nastran GRID {id} uses coordinate system CP={cp}; only basic-coordinate GRID data are rendered"
                )));
            }
            if !node_ids.insert(id) {
                return Err(Error::InvalidInput(format!(
                    "duplicate Nastran GRID ID {id}"
                )));
            }
            if nodes.len() >= MAX_SIMULATION_POINTS {
                return Err(Error::LimitExceeded(format!(
                    "Nastran GRID count exceeds {MAX_SIMULATION_POINTS}"
                )));
            }
            let x = parse_real_or_default(field(&fields, 2), 0.0, "GRID X1", line_index + 1)?;
            let y = parse_real_or_default(field(&fields, 3), 0.0, "GRID X2", line_index + 1)?;
            let z = parse_real_or_default(field(&fields, 4), 0.0, "GRID X3", line_index + 1)?;
            has_nonzero_z |= z.abs() > 1.0e-12;
            nodes.insert(id, MeshNode { x, y, scalar: None });
            line_index = last_line + 1;
            continue;
        }

        line_index += 1;
    }

    line_index = 0;
    while line_index < lines.len() {
        let Some(record) = parse_bdf_record(lines[line_index], line_index + 1)? else {
            line_index += 1;
            continue;
        };
        if record.is_continuation {
            line_index += 1;
            continue;
        }
        if record.name == "ENDDATA" {
            break;
        }
        if record.name == "INCLUDE" {
            included_files += 1;
            line_index += 1;
            continue;
        }
        if record.name == "GRID" {
            let (_, last_line, _) = collect_card_fields(&lines, line_index, record, 5)?;
            line_index = last_line + 1;
            continue;
        }
        if let Some(spec) = element_spec(&record.name) {
            let card_name = record.name.clone();
            let min_fields = spec.node_start + spec.min_nodes;
            let (fields, last_line, _had_continuation) =
                collect_card_fields(&lines, line_index, record, min_fields)?;
            let element_id =
                parse_positive_integer(field(&fields, 0), "element ID", line_index + 1)?;
            if !element_ids.insert(element_id) {
                return Err(Error::InvalidInput(format!(
                    "duplicate Nastran element ID {element_id}"
                )));
            }
            if element_ids.len() > MAX_SIMULATION_CELLS {
                return Err(Error::LimitExceeded(format!(
                    "Nastran element count exceeds {MAX_SIMULATION_CELLS}"
                )));
            }
            let node_count = contiguous_node_count(&fields, spec.node_start, spec.max_nodes);
            let selected_node_count = node_count;
            if node_count < spec.min_nodes {
                return Err(Error::InvalidInput(format!(
                    "Nastran {} element {element_id} has {node_count} node IDs; expected at least {}",
                    card_name, spec.min_nodes
                )));
            }
            if fields
                .get(spec.node_start + node_count..spec.node_start + spec.max_nodes)
                .is_some_and(|trailing| trailing.iter().any(|value| !value.trim().is_empty()))
            {
                return Err(Error::InvalidInput(format!(
                    "Nastran {} element {element_id} has a blank node ID before later connectivity fields",
                    card_name
                )));
            }
            let node_labels = fields
                .get(spec.node_start..spec.node_start + selected_node_count)
                .ok_or_else(|| {
                    Error::InvalidInput(format!(
                        "Nastran {} element {element_id} has incomplete node fields",
                        card_name
                    ))
                })?
                .iter()
                .map(|value| parse_positive_integer(value, "element node ID", line_index + 1))
                .collect::<Result<Vec<_>>>()?;
            for node_id in &node_labels {
                if !nodes.contains_key(node_id) {
                    return Err(Error::InvalidInput(format!(
                        "Nastran element {element_id} references unknown GRID {node_id}"
                    )));
                }
            }
            let (vtk_type, corner_count) = if selected_node_count > spec.min_nodes {
                (spec.quadratic_vtk_type, spec.quadratic_corner_count)
            } else {
                (spec.linear_vtk_type, spec.linear_corner_count)
            };
            let primitives = vtk_cell_primitives(vtk_type, &node_labels, None)?;
            if cells.len().saturating_add(primitives.len()) > MAX_VTK_RENDERED_PRIMITIVES {
                return Err(Error::LimitExceeded(format!(
                    "Nastran rendered primitive count exceeds {MAX_VTK_RENDERED_PRIMITIVES}"
                )));
            }
            cells.extend(primitives);
            omitted_midside_nodes |= selected_node_count > corner_count;
            line_index = last_line + 1;
            continue;
        }

        if is_nastran_element_card(&record.name) {
            unsupported_elements += 1;
            if unsupported_examples.len() < 8 && !unsupported_examples.contains(&record.name) {
                unsupported_examples.push(record.name.clone());
            }
        }
        line_index += 1;
    }

    if nodes.is_empty() || cells.is_empty() {
        return Err(Error::Unsupported(
            "Nastran Bulk Data has no supported GRID and structural element cards".into(),
        ));
    }
    if included_files > 0 {
        warnings.push(format!(
            "Nastran input references {included_files} external INCLUDE file(s); they were not read"
        ));
    }
    if unsupported_elements > 0 {
        warnings.push(format!(
            "Nastran skipped {unsupported_elements} unsupported element card(s) (examples: {})",
            unsupported_examples.join(", ")
        ));
    }
    if omitted_midside_nodes {
        warnings.push(
            "Nastran higher-order element midside nodes were omitted; straight corner-node edges are shown".into(),
        );
    }
    if has_nonzero_z {
        warnings.push(
            "Nastran 3D GRID coordinates are shown in the XY projection; Z depth is not represented".into(),
        );
    }
    Ok((nodes, cells, "Nastran Bulk Data mesh".into(), warnings))
}

fn parse_bdf_record<'a>(line: &'a str, line_number: usize) -> Result<Option<BdfRecord<'a>>> {
    let line = line.trim_end_matches('\r');
    if line.trim().is_empty() || line.trim_start().starts_with('$') {
        return Ok(None);
    }
    if line.len() > MAX_NASTRAN_LINE_BYTES {
        return Err(Error::LimitExceeded(format!(
            "Nastran line {} exceeds {MAX_NASTRAN_LINE_BYTES} bytes",
            line_number
        )));
    }
    if !line.is_ascii() {
        return Err(Error::InvalidInput(format!(
            "Nastran Bulk Data card at line {line_number} is not ASCII"
        )));
    }
    if line.contains(',') {
        let mut fields = line.split(',');
        let first = fields.next().unwrap_or_default().trim();
        let trailing_comma = line.trim_end().ends_with(',');
        let is_continuation = first.is_empty() || first.starts_with('+') || first.starts_with('*');
        let name = if is_continuation {
            String::new()
        } else {
            first.trim_end_matches('*').trim().to_ascii_uppercase()
        };
        let mut data = fields
            .by_ref()
            .take(MAX_NASTRAN_FIELDS_PER_CARD + 2)
            .collect::<Vec<_>>();
        let has_extra_fields = fields.next().is_some();
        if trailing_comma && data.last().is_some_and(|value| value.trim().is_empty()) {
            data.pop();
        }
        let trailing_continuation = data
            .last()
            .map(|value| value.trim())
            .filter(|value| value.starts_with('+') || value.starts_with('*'));
        if trailing_continuation.is_some() {
            data.pop();
        }
        if has_extra_fields || data.len() > MAX_NASTRAN_FIELDS_PER_CARD {
            return Err(Error::LimitExceeded(format!(
                "Nastran comma-separated card at line {line_number} has too many fields"
            )));
        }
        return Ok(Some(BdfRecord {
            name,
            data,
            leading_continuation: (is_continuation && !first.is_empty()).then_some(first),
            trailing_continuation,
            is_continuation,
            trailing_comma,
        }));
    }

    let first = fixed_field(line, 0, 8).trim();
    let free_fields = first.split_whitespace().collect::<Vec<_>>();
    if free_fields.len() > 1 {
        let mut tokens = line.split_whitespace();
        let name_or_marker = tokens.next().unwrap_or_default();
        let is_continuation = name_or_marker.starts_with('+') || name_or_marker.starts_with('*');
        let mut data = tokens
            .by_ref()
            .take(MAX_NASTRAN_FIELDS_PER_CARD + 2)
            .collect::<Vec<_>>();
        let has_extra_fields = tokens.next().is_some();
        let trailing_continuation = data
            .last()
            .copied()
            .filter(|value| value.starts_with('+') || value.starts_with('*'));
        if trailing_continuation.is_some() {
            data.pop();
        }
        if has_extra_fields || data.len() > MAX_NASTRAN_FIELDS_PER_CARD {
            return Err(Error::LimitExceeded(format!(
                "Nastran whitespace-separated card at line {line_number} has too many fields"
            )));
        }
        return Ok(Some(BdfRecord {
            name: if is_continuation {
                String::new()
            } else {
                name_or_marker.to_ascii_uppercase()
            },
            data,
            leading_continuation: is_continuation.then_some(name_or_marker),
            trailing_continuation,
            is_continuation,
            trailing_comma: false,
        }));
    }

    let is_continuation = first.is_empty() || first.starts_with('+') || first.starts_with('*');
    let is_large = !is_continuation && first.ends_with('*');
    let width = if is_large || (is_continuation && first.starts_with('*')) {
        16
    } else {
        8
    };
    if line.len() > 80 {
        return Err(Error::InvalidInput(format!(
            "Nastran fixed-field line {line_number} exceeds 80 columns"
        )));
    }
    let data = (8..72)
        .step_by(width)
        .map(|start| fixed_field(line, start, width))
        .collect::<Vec<_>>();
    let trailing_continuation = fixed_field(line, 72, 8).trim();
    Ok(Some(BdfRecord {
        name: if is_continuation {
            String::new()
        } else {
            first.trim_end_matches('*').trim().to_ascii_uppercase()
        },
        data,
        leading_continuation: (is_continuation && !first.is_empty()).then_some(first),
        trailing_continuation: (!trailing_continuation.is_empty()).then_some(trailing_continuation),
        is_continuation,
        trailing_comma: false,
    }))
}

fn fixed_field(line: &str, start: usize, width: usize) -> &str {
    if start >= line.len() {
        return "";
    }
    let end = start.saturating_add(width).min(line.len());
    line.get(start..end).unwrap_or_default()
}

fn collect_card_fields<'a>(
    lines: &[&'a str],
    start_line: usize,
    record: BdfRecord<'a>,
    minimum_field_count: usize,
) -> Result<(Vec<&'a str>, usize, bool)> {
    let mut fields = record.data;
    let mut last_line = start_line;
    let mut had_continuation = false;
    let mut expects_continuation = record.trailing_comma || record.trailing_continuation.is_some();
    let mut expected_marker = record.trailing_continuation;
    loop {
        let Some(next_line) = next_noncomment_line(lines, last_line + 1) else {
            if fields.len() < minimum_field_count || expects_continuation {
                return Err(Error::InvalidInput(format!(
                    "Nastran card at line {} ends before its continuation data is complete",
                    start_line + 1
                )));
            }
            break;
        };
        let Some(next) = parse_bdf_record(lines[next_line], next_line + 1)? else {
            last_line = next_line;
            continue;
        };
        if !next.is_continuation {
            if fields.len() < minimum_field_count || expects_continuation {
                return Err(Error::InvalidInput(format!(
                    "Nastran card at line {} is missing a continuation entry",
                    start_line + 1
                )));
            }
            break;
        }
        let parent_marker = expected_marker
            .map(normalize_continuation_marker)
            .unwrap_or_default();
        let child_marker = next
            .leading_continuation
            .map(normalize_continuation_marker)
            .unwrap_or_default();
        if !parent_marker.eq_ignore_ascii_case(child_marker) {
            return Err(Error::InvalidInput(format!(
                "Nastran continuation label at line {} does not match its parent card",
                next_line + 1
            )));
        }
        if fields
            .len()
            .checked_add(next.data.len())
            .is_none_or(|count| count > MAX_NASTRAN_FIELDS_PER_CARD)
        {
            return Err(Error::LimitExceeded(format!(
                "Nastran card at line {} exceeds {MAX_NASTRAN_FIELDS_PER_CARD} fields",
                start_line + 1
            )));
        }
        fields.extend(next.data);
        had_continuation = true;
        expects_continuation = next.trailing_comma || next.trailing_continuation.is_some();
        expected_marker = next.trailing_continuation;
        last_line = next_line;
    }
    if fields.len() < minimum_field_count {
        return Err(Error::InvalidInput(format!(
            "Nastran card at line {} has incomplete fields",
            start_line + 1
        )));
    }
    Ok((fields, last_line, had_continuation))
}

fn next_noncomment_line(lines: &[&str], start: usize) -> Option<usize> {
    (start..lines.len()).find(|index| {
        let line = lines[*index].trim_end_matches('\r');
        !line.trim().is_empty() && !line.trim_start().starts_with('$')
    })
}

fn normalize_continuation_marker(marker: &str) -> &str {
    marker.trim().trim_start_matches(['+', '*'])
}

fn element_spec(name: &str) -> Option<ElementSpec> {
    let fixed = |node_field_start, nodes, vtk_type, corners| ElementSpec {
        node_start: node_field_start,
        min_nodes: nodes,
        max_nodes: nodes,
        linear_vtk_type: vtk_type,
        quadratic_vtk_type: vtk_type,
        linear_corner_count: corners,
        quadratic_corner_count: corners,
    };
    Some(match name {
        "CTRIA" | "CTRIA3" | "CTRIAR" => fixed(2, 3, 5, 3),
        "CTRIA6" => fixed(2, 6, 22, 3),
        "CQUAD" | "CQUAD4" | "CQUADR" => fixed(2, 4, 9, 4),
        "CQUAD8" => fixed(2, 8, 23, 4),
        "CTRIAAX6" | "CTRIAX6" => fixed(2, 6, 22, 3),
        "CSHEAR" => fixed(2, 4, 9, 4),
        "CTETRA" => ElementSpec {
            node_start: 2,
            min_nodes: 4,
            max_nodes: 10,
            linear_vtk_type: 10,
            quadratic_vtk_type: 24,
            linear_corner_count: 4,
            quadratic_corner_count: 4,
        },
        "CHEXA" => ElementSpec {
            node_start: 2,
            min_nodes: 8,
            max_nodes: 20,
            linear_vtk_type: 12,
            quadratic_vtk_type: 25,
            linear_corner_count: 8,
            quadratic_corner_count: 8,
        },
        "CPENTA" => ElementSpec {
            node_start: 2,
            min_nodes: 6,
            max_nodes: 15,
            linear_vtk_type: 13,
            quadratic_vtk_type: 26,
            linear_corner_count: 6,
            quadratic_corner_count: 6,
        },
        "CPYRA" | "CPYRAM" => ElementSpec {
            node_start: 2,
            min_nodes: 5,
            max_nodes: 13,
            linear_vtk_type: 14,
            quadratic_vtk_type: 27,
            linear_corner_count: 5,
            quadratic_corner_count: 5,
        },
        "CROD" | "CBAR" | "CBEAM" | "CTUBE" | "CBUSH" => fixed(2, 2, 3, 2),
        "CONROD" => fixed(1, 2, 3, 2),
        _ => return None,
    })
}

fn is_nastran_element_card(name: &str) -> bool {
    [
        "CTRIA", "CQUAD", "CTETRA", "CHEXA", "CPENTA", "CPYRA", "CROD", "CBAR", "CBEAM", "CTUBE",
        "CONROD", "CSHEAR", "CBUSH",
    ]
    .iter()
    .any(|prefix| name.starts_with(prefix))
}

fn contiguous_node_count(fields: &[&str], node_start: usize, maximum: usize) -> usize {
    fields
        .get(node_start..)
        .unwrap_or_default()
        .iter()
        .take(maximum)
        .take_while(|value| !value.trim().is_empty())
        .count()
}

fn field<'a>(fields: &[&'a str], index: usize) -> &'a str {
    fields.get(index).copied().unwrap_or_default().trim()
}

fn parse_positive_integer(value: &str, context: &str, line_number: usize) -> Result<usize> {
    let value = value.trim();
    let integer = value.parse::<usize>().map_err(|_| {
        Error::InvalidInput(format!(
            "invalid Nastran {context} '{value}' at line {line_number}"
        ))
    })?;
    if integer == 0 {
        return Err(Error::InvalidInput(format!(
            "Nastran {context} must be positive at line {line_number}"
        )));
    }
    Ok(integer)
}

fn parse_integer_or_default(
    value: &str,
    default: usize,
    context: &str,
    line_number: usize,
) -> Result<usize> {
    if value.trim().is_empty() {
        return Ok(default);
    }
    value.trim().parse::<usize>().map_err(|_| {
        Error::InvalidInput(format!(
            "invalid Nastran {context} '{value}' at line {line_number}"
        ))
    })
}

fn parse_real_or_default(
    value: &str,
    default: f64,
    context: &str,
    line_number: usize,
) -> Result<f64> {
    if value.trim().is_empty() {
        return Ok(default);
    }
    let value = value.trim();
    let normalized = value.replace(['D', 'd'], "E");
    let parsed = parse_cad_float(&normalized).or_else(|| {
        if normalized.contains(['E', 'e']) {
            return None;
        }
        let exponent_sign = normalized
            .char_indices()
            .skip(1)
            .filter(|(_, character)| *character == '+' || *character == '-')
            .map(|(index, _)| index)
            .last()?;
        let (mantissa, exponent) = normalized.split_at(exponent_sign);
        format!("{mantissa}E{exponent}").parse::<f64>().ok()
    });
    let parsed = parsed.ok_or_else(|| {
        Error::InvalidInput(format!(
            "invalid Nastran {context} value '{value}' at line {line_number}"
        ))
    })?;
    if !parsed.is_finite() || parsed.abs() > 1.0e12 {
        return Err(Error::InvalidInput(format!(
            "Nastran {context} is non-finite or outside ±1e12 at line {line_number}"
        )));
    }
    Ok(parsed)
}

#[cfg(test)]
mod tests {
    use super::{parse_bdf, parse_bdf_record, parse_real_or_default};
    use crate::error::Error;
    use std::fmt::Write as _;

    fn fixed_card(name: &str, fields: &[&str], continuation: &str, width: usize) -> String {
        let slots = if width == 8 { 8 } else { 4 };
        let mut line = format!("{name:<8}");
        for index in 0..slots {
            let value = fields.get(index).copied().unwrap_or_default();
            write!(&mut line, "{value:>width$}").unwrap();
        }
        if !continuation.is_empty() {
            while line.len() < 72 {
                line.push(' ');
            }
            write!(&mut line, "{continuation:<8}").unwrap();
        }
        line
    }

    #[test]
    fn parses_free_format_mesh_and_compact_exponent_coordinates() {
        let deck =
            "CTRIA3,8,1,1,2,3\nGRID,1,,1.25-3,-2.5+1,0.\nGRID,2,,1,0,0\nGRID,3,,0,1,0\nENDDATA\n";
        let (nodes, cells, _, warnings) = parse_bdf(deck).unwrap();
        assert_eq!(nodes.len(), 3);
        assert!((nodes[&1].x - 0.00125).abs() < 1.0e-12);
        assert!((nodes[&1].y + 25.0).abs() < 1.0e-12);
        assert_eq!(cells.len(), 1);
        assert!(warnings.is_empty());
        assert_eq!(
            parse_real_or_default("3.0D+2", 0.0, "test", 1).unwrap(),
            300.0
        );

        let whitespace_deck = "GRID 1 0 0 0 0\nGRID 2 0 1 0 0\nGRID 3 0 0 1 0\nCTRIA3 9 1 1 2 3";
        let (space_nodes, space_cells, _, _) = parse_bdf(whitespace_deck).unwrap();
        assert_eq!(space_nodes.len(), 3);
        assert_eq!(space_cells.len(), 1);
    }

    #[test]
    fn parses_small_and_large_field_grids_and_element_cards() {
        let small_grid = fixed_card("GRID", &["1", "", "0", "0", "0"], "", 8);
        let large_grid = fixed_card("GRID*", &["2", "0", "1.0", "0.0"], "*G2", 16);
        let large_cont = fixed_card("*G2", &["0.0", "0", "", ""], "", 16);
        let small_grid3 = fixed_card("GRID", &["3", "", "0", "1", "0"], "", 8);
        let small_tri = fixed_card("CTRIA3", &["11", "1", "1", "2", "3"], "", 8);
        let large_tri = fixed_card("CTRIA3*", &["12", "1", "1", "2"], "*NXT", 16);
        let large_tri_cont = fixed_card("*NXT", &["3", "", "", ""], "", 16);
        let deck = [
            small_grid,
            large_grid,
            large_cont,
            small_grid3,
            small_tri,
            large_tri,
            large_tri_cont,
        ]
        .join("\n");
        let (nodes, cells, _, _) = parse_bdf(&deck).unwrap();
        assert_eq!(nodes.len(), 3);
        assert!((nodes[&2].x - 1.0).abs() < 1.0e-12);
        assert_eq!(cells.len(), 2);
    }

    #[test]
    fn parses_labeled_small_and_free_continuations_for_partial_quadratic_tetrahedra() {
        let mut lines = (1..=5)
            .map(|id| format!("GRID,{id},,{id},0,0"))
            .collect::<Vec<_>>();
        let first = fixed_card("CTETRA", &["21", "1", "1", "2", "3", "4", "5"], "+REST", 8);
        let continued = fixed_card("+rest", &[""], "", 8);
        lines.push(first);
        lines.push(continued);
        lines.push("CTETRA,22,1,1,2,3,4,+FREE".into());
        lines.push("+free,5".into());
        let (nodes, cells, _, warnings) = parse_bdf(&lines.join("\n")).unwrap();
        assert_eq!(nodes.len(), 5);
        assert_eq!(cells.len(), 12);
        assert!(warnings.iter().any(|warning| warning.contains("midside")));
    }

    #[test]
    fn includes_are_reported_and_unsupported_elements_are_skipped() {
        let deck = "GRID,1,,0,0,0\nGRID,2,,1,0,0\nGRID,3,,0,1,1\nINCLUDE 'external.bdf'\nCTRIA3,1,1,1,2,3\nCTRIA6,2,1,1,2,3,1,2,3\nCQUAD9,3,1,1,2,3,1,2,3,1,2,3\n";
        let (_, cells, _, warnings) = parse_bdf(deck).unwrap();
        assert_eq!(cells.len(), 2);
        assert!(warnings.iter().any(|warning| warning.contains("INCLUDE")));
        assert!(
            warnings
                .iter()
                .any(|warning| warning.contains("unsupported"))
        );
        assert!(
            warnings
                .iter()
                .any(|warning| warning.contains("XY projection"))
        );
    }

    #[test]
    fn rejects_bad_continuations_coordinate_systems_and_unknown_nodes() {
        let parent = fixed_card("GRID", &["1", "", "0", "0", "0"], "+FIRST", 8);
        let mismatched = fixed_card("+OTHER", &["", "", "", ""], "", 8);
        let unlabeled = fixed_card("", &["", "", "", ""], "", 8);
        let record = parse_bdf_record(&parent, 1).unwrap().unwrap();
        assert_eq!(record.trailing_continuation, Some("+FIRST"));
        let error = parse_bdf(&format!("{parent}\n{mismatched}")).unwrap_err();
        assert!(matches!(error, Error::InvalidInput(_)));
        let error = parse_bdf(&format!("{parent}\n{unlabeled}")).unwrap_err();
        assert!(matches!(error, Error::InvalidInput(_)));

        let nonbasic = "GRID,1,42,0,0,0\nGRID,2,,1,0,0\nGRID,3,,0,1,0\nCTRIA3,1,1,1,2,3";
        assert!(matches!(parse_bdf(nonbasic), Err(Error::Unsupported(_))));
        let unknown = "GRID,1,,0,0,0\nGRID,2,,1,0,0\nGRID,3,,0,1,0\nCTRIA3,1,1,1,2,99";
        assert!(matches!(parse_bdf(unknown), Err(Error::InvalidInput(_))));
    }
}
