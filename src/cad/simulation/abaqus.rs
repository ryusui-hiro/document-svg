use std::collections::{HashMap, HashSet};
use std::io::Read;

use crate::cad::dxf::geometry::parse_cad_float;
use crate::convert::{ConvertOptions, PageConsumer};
use crate::error::{Error, Result};

use super::{
    AbaqusElementTopology, AbaqusInstance, AbaqusMesh, AbaqusSection, MAX_ABAQUS_INSTANCES,
    MAX_ABAQUS_LINE_BYTES, MAX_ABAQUS_LINES, MAX_ABAQUS_PARTS, MAX_SIMULATION_CELLS,
    MAX_SIMULATION_POINTS, MAX_VTK_RENDERED_PRIMITIVES, MeshCell, MeshNode, SimulationParseOutput,
    render_simulation, vtk_cell_primitives,
};

const MAX_ABAQUS_PART_NAME_BYTES: usize = 256;

pub(super) fn convert<R: Read>(
    input: R,
    options: &ConvertOptions,
    sink: &mut dyn PageConsumer,
) -> Result<Vec<String>> {
    let mut bytes = Vec::new();
    Read::take(input, options.max_input_bytes.saturating_add(1)).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > options.max_input_bytes {
        return Err(Error::LimitExceeded(format!(
            "Abaqus input exceeds maximum limit of {}",
            options.max_input_bytes
        )));
    }
    let text = String::from_utf8(bytes).map_err(|error| {
        Error::InvalidInput(format!("Abaqus input must be UTF-8/ASCII: {error}"))
    })?;
    render_simulation(parse_abaqus(&text)?, sink)
}

fn parse_abaqus(text: &str) -> Result<SimulationParseOutput> {
    let mut global_mesh = AbaqusMesh::default();
    let mut parts = HashMap::<String, AbaqusMesh>::new();
    let mut current_part = None::<String>;
    let mut current_instance = None::<AbaqusInstance>;
    let mut instances = Vec::new();
    let mut instance_names = HashSet::new();
    let mut in_assembly = false;
    let mut has_assembly = false;
    let mut section = AbaqusSection::Ignore;
    let mut pending_element = [0usize; 21];
    let mut pending_element_len = 0usize;
    let mut pending_line = 0usize;
    let mut title = None::<String>;
    let mut warnings = Vec::new();
    let mut unsupported_element_blocks = 0usize;
    let mut unsupported_element_examples = Vec::<String>::new();
    let mut include_count = 0usize;
    let mut ignored_assembly_mesh_blocks = 0usize;
    let mut high_order_elements = false;
    let mut parsed_node_records = 0usize;
    let mut parsed_element_records = 0usize;

    for (line_index, raw_line) in text.lines().enumerate() {
        if line_index >= MAX_ABAQUS_LINES {
            return Err(Error::LimitExceeded(format!(
                "Abaqus input exceeds {MAX_ABAQUS_LINES} lines"
            )));
        }
        if raw_line.len() > MAX_ABAQUS_LINE_BYTES {
            return Err(Error::LimitExceeded(format!(
                "Abaqus line {} exceeds {MAX_ABAQUS_LINE_BYTES} bytes",
                line_index + 1
            )));
        }
        let line = raw_line.trim().trim_start_matches('\u{feff}').trim();
        if line.is_empty() || line.starts_with("**") {
            continue;
        }

        if line.starts_with('*') {
            if pending_element_len > 0 {
                return Err(Error::InvalidInput(format!(
                    "Abaqus element data at line {pending_line} ended before all connectivity values were supplied"
                )));
            }
            let (keyword, parameters) = parse_keyword(line, line_index + 1)?;
            match keyword.as_str() {
                "PART" => {
                    if in_assembly
                        || has_assembly
                        || current_part.is_some()
                        || current_instance.is_some()
                    {
                        return Err(Error::InvalidInput(
                            "Abaqus PART blocks must be top-level and cannot be nested".into(),
                        ));
                    }
                    let name =
                        normalize_label(required_parameter(&parameters, "NAME", line_index + 1)?);
                    if name.len() > MAX_ABAQUS_PART_NAME_BYTES {
                        return Err(Error::LimitExceeded(format!(
                            "Abaqus part name exceeds {MAX_ABAQUS_PART_NAME_BYTES} bytes"
                        )));
                    }
                    if parts.len() >= MAX_ABAQUS_PARTS {
                        return Err(Error::LimitExceeded(format!(
                            "Abaqus part count exceeds {MAX_ABAQUS_PARTS}"
                        )));
                    }
                    if parts.insert(name.clone(), AbaqusMesh::default()).is_some() {
                        return Err(Error::InvalidInput(format!(
                            "duplicate Abaqus part name '{name}'"
                        )));
                    }
                    current_part = Some(name);
                    section = AbaqusSection::Ignore;
                }
                "END PART" => {
                    if current_part.take().is_none() || in_assembly {
                        return Err(Error::InvalidInput(
                            "Abaqus END PART does not match an open PART".into(),
                        ));
                    }
                    section = AbaqusSection::Ignore;
                }
                "ASSEMBLY" => {
                    if in_assembly
                        || has_assembly
                        || current_part.is_some()
                        || current_instance.is_some()
                    {
                        return Err(Error::InvalidInput(
                            "Abaqus ASSEMBLY is nested or inside a PART".into(),
                        ));
                    }
                    in_assembly = true;
                    has_assembly = true;
                    section = AbaqusSection::Ignore;
                }
                "END ASSEMBLY" => {
                    if !in_assembly || current_instance.is_some() {
                        return Err(Error::InvalidInput(
                            "Abaqus END ASSEMBLY does not match a closed set of instances".into(),
                        ));
                    }
                    in_assembly = false;
                    section = AbaqusSection::Ignore;
                }
                "INSTANCE" => {
                    if !in_assembly || current_instance.is_some() || current_part.is_some() {
                        return Err(Error::Unsupported(
                            "Abaqus INSTANCE blocks must be directly inside ASSEMBLY".into(),
                        ));
                    }
                    let name =
                        normalize_label(required_parameter(&parameters, "NAME", line_index + 1)?);
                    if !parameters.contains_key("PART") && parameters.contains_key("INSTANCE") {
                        return Err(Error::Unsupported(
                            "Abaqus imported/restart part instances are not supported".into(),
                        ));
                    }
                    let part_name =
                        normalize_label(required_parameter(&parameters, "PART", line_index + 1)?);
                    if !parts.contains_key(&part_name) {
                        return Err(Error::InvalidInput(format!(
                            "Abaqus instance '{name}' references undefined part '{part_name}'"
                        )));
                    }
                    if name.len() > MAX_ABAQUS_PART_NAME_BYTES
                        || part_name.len() > MAX_ABAQUS_PART_NAME_BYTES
                    {
                        return Err(Error::LimitExceeded(format!(
                            "Abaqus instance or part name exceeds {MAX_ABAQUS_PART_NAME_BYTES} bytes"
                        )));
                    }
                    if instances.len() >= MAX_ABAQUS_INSTANCES {
                        return Err(Error::LimitExceeded(format!(
                            "Abaqus instance count exceeds {MAX_ABAQUS_INSTANCES}"
                        )));
                    }
                    if !instance_names.insert(name.clone()) {
                        return Err(Error::InvalidInput(format!(
                            "duplicate Abaqus instance name '{name}'"
                        )));
                    }
                    current_instance = Some(AbaqusInstance {
                        name,
                        part_name,
                        mesh: AbaqusMesh::default(),
                        translation: [0.0; 3],
                        rotation: None,
                        position_line_count: 0,
                    });
                    section = AbaqusSection::InstancePosition;
                }
                "END INSTANCE" => {
                    let instance = current_instance.take().ok_or_else(|| {
                        Error::InvalidInput(
                            "Abaqus END INSTANCE does not match an open INSTANCE".into(),
                        )
                    })?;
                    instances.push(instance);
                    section = AbaqusSection::Ignore;
                }
                "NODE" => {
                    if let Some(system) = parameters.get("SYSTEM")
                        && !system.is_empty()
                        && !system.eq_ignore_ascii_case("R")
                    {
                        return Err(Error::Unsupported(format!(
                            "Abaqus node coordinate system '{system}' is unsupported; SYSTEM=R is Cartesian"
                        )));
                    }
                    if parameters.contains_key("INPUT") {
                        return Err(Error::Unsupported(
                            "Abaqus *NODE INPUT= external data files are not read".into(),
                        ));
                    }
                    if in_assembly && current_instance.is_none() {
                        ignored_assembly_mesh_blocks += 1;
                        section = AbaqusSection::Ignore;
                    } else {
                        section = AbaqusSection::Nodes;
                    }
                }
                "ELEMENT" => {
                    if parameters.contains_key("INPUT") {
                        return Err(Error::Unsupported(
                            "Abaqus *ELEMENT INPUT= external data files are not read".into(),
                        ));
                    }
                    if in_assembly && current_instance.is_none() {
                        ignored_assembly_mesh_blocks += 1;
                        section = AbaqusSection::Ignore;
                    } else {
                        let element_type = required_parameter(&parameters, "TYPE", line_index + 1)?;
                        let element_type = element_type.to_ascii_uppercase();
                        if let Some(topology) = element_topology(&element_type) {
                            section = AbaqusSection::Elements(topology);
                            high_order_elements |= topology.node_count > topology.corner_count;
                        } else {
                            unsupported_element_blocks += 1;
                            if unsupported_element_examples.len() < 8
                                && !unsupported_element_examples.contains(&element_type)
                            {
                                unsupported_element_examples
                                    .push(element_type.chars().take(64).collect());
                            }
                            section = AbaqusSection::Ignore;
                        }
                    }
                }
                "HEADING" => section = AbaqusSection::Heading,
                "INCLUDE" => {
                    include_count += 1;
                    section = AbaqusSection::Ignore;
                }
                "SYSTEM" => {
                    return Err(Error::Unsupported(
                        "Abaqus *SYSTEM coordinate transformations are not applied".into(),
                    ));
                }
                _ => section = AbaqusSection::Ignore,
            }
            continue;
        }

        match section {
            AbaqusSection::Ignore => {}
            AbaqusSection::Heading => {
                if title.is_none() {
                    title = Some(line.chars().take(256).collect());
                }
                section = AbaqusSection::Ignore;
            }
            AbaqusSection::InstancePosition => {
                let instance = current_instance.as_mut().ok_or_else(|| {
                    Error::InvalidInput("Abaqus instance positioning data has no instance".into())
                })?;
                parse_instance_position(instance, line, line_index + 1)?;
            }
            AbaqusSection::Nodes => {
                let (id, x, y, z) = parse_node_record(line, line_index + 1)?;
                parsed_node_records += 1;
                if parsed_node_records > MAX_SIMULATION_POINTS {
                    return Err(Error::LimitExceeded(format!(
                        "Abaqus node count exceeds {MAX_SIMULATION_POINTS}"
                    )));
                }
                let mesh = active_mesh_mut(
                    &mut current_instance,
                    current_part.as_deref(),
                    &mut parts,
                    &mut global_mesh,
                )?;
                if mesh
                    .nodes
                    .insert(id, MeshNode { x, y, scalar: None })
                    .is_some()
                {
                    return Err(Error::InvalidInput(format!(
                        "duplicate Abaqus node label {id} in one mesh scope"
                    )));
                }
                mesh.z_coordinates.insert(id, z);
            }
            AbaqusSection::Elements(topology) => {
                let expected_fields = topology.node_count + 1;
                let complete = append_element_values(
                    line,
                    line_index + 1,
                    expected_fields,
                    &mut pending_element,
                    &mut pending_element_len,
                    &mut pending_line,
                )?;
                if complete {
                    let element_id = pending_element[0];
                    parsed_element_records += 1;
                    if parsed_element_records > MAX_SIMULATION_CELLS {
                        return Err(Error::LimitExceeded(format!(
                            "Abaqus element count exceeds {MAX_SIMULATION_CELLS}"
                        )));
                    }
                    let mesh = active_mesh_mut(
                        &mut current_instance,
                        current_part.as_deref(),
                        &mut parts,
                        &mut global_mesh,
                    )?;
                    if !mesh.element_ids.insert(element_id) {
                        return Err(Error::InvalidInput(format!(
                            "duplicate Abaqus element label {element_id} in one mesh scope"
                        )));
                    }
                    let node_ids = &pending_element[1..expected_fields];
                    for node_id in node_ids {
                        if !mesh.nodes.contains_key(node_id) {
                            return Err(Error::InvalidInput(format!(
                                "Abaqus element {element_id} references unknown node {node_id}"
                            )));
                        }
                    }
                    let primitives = vtk_cell_primitives(topology.vtk_cell_type, node_ids, None)?;
                    if mesh.cells.len().saturating_add(primitives.len())
                        > MAX_VTK_RENDERED_PRIMITIVES
                    {
                        return Err(Error::LimitExceeded(format!(
                            "Abaqus rendered primitive count exceeds {MAX_VTK_RENDERED_PRIMITIVES}"
                        )));
                    }
                    mesh.cells.extend(primitives);
                    pending_element_len = 0;
                }
            }
        }
    }

    if pending_element_len > 0 {
        return Err(Error::InvalidInput(format!(
            "Abaqus element data at line {pending_line} ends with incomplete connectivity"
        )));
    }
    if current_part.is_some() || current_instance.is_some() || in_assembly {
        return Err(Error::InvalidInput(
            "Abaqus PART, INSTANCE, or ASSEMBLY block is not terminated".into(),
        ));
    }

    let mut mesh = AbaqusMesh::default();
    if !instances.is_empty() {
        if !global_mesh.nodes.is_empty() || !global_mesh.cells.is_empty() {
            ignored_assembly_mesh_blocks += 1;
        }
        for instance in &instances {
            let part_mesh = parts.get(&instance.part_name);
            let instance_has_mesh =
                !instance.mesh.nodes.is_empty() || !instance.mesh.cells.is_empty();
            if instance_has_mesh
                && part_mesh.is_some_and(|part| !part.nodes.is_empty() || !part.cells.is_empty())
            {
                return Err(Error::InvalidInput(format!(
                    "Abaqus instance '{}' defines a mesh while its part also has one",
                    instance.name
                )));
            }
            let source = if instance_has_mesh {
                Some(&instance.mesh)
            } else {
                part_mesh
            };
            let Some(source) =
                source.filter(|source| !source.nodes.is_empty() && !source.cells.is_empty())
            else {
                ignored_assembly_mesh_blocks += 1;
                continue;
            };
            append_mesh_instance(
                &mut mesh,
                source,
                instance.translation,
                instance.rotation,
                &instance.name,
            )?;
        }
    } else if has_assembly {
        return Err(Error::Unsupported(
            "Abaqus ASSEMBLY contains no renderable part instances".into(),
        ));
    } else if !parts.is_empty() {
        let mut part_names = parts.keys().cloned().collect::<Vec<_>>();
        part_names.sort();
        for name in &part_names {
            let source = parts.get(name).expect("sorted part name remains present");
            if !source.nodes.is_empty() && !source.cells.is_empty() {
                append_mesh_instance(&mut mesh, source, [0.0; 3], None, name)?;
            }
        }
        warnings.push(
            "Abaqus parts were rendered in their part-local positions because no instances were defined".into(),
        );
        if !global_mesh.nodes.is_empty() && !global_mesh.cells.is_empty() {
            append_mesh_instance(&mut mesh, &global_mesh, [0.0; 3], None, "global")?;
        }
    } else {
        mesh = global_mesh;
    }

    if mesh.nodes.is_empty() || mesh.cells.is_empty() {
        return Err(Error::Unsupported(
            "Abaqus input has no supported node-and-element mesh; supported structural types include common C3D, CPS, CPE, CAX, shell, and beam elements".into(),
        ));
    }
    if ignored_assembly_mesh_blocks > 0 {
        warnings.push(format!(
            "Abaqus ignored {ignored_assembly_mesh_blocks} assembly-level or empty instance mesh block(s)"
        ));
    }
    if unsupported_element_blocks > 0 {
        warnings.push(format!(
            "Abaqus ignored {unsupported_element_blocks} unsupported element block(s); element type examples: {}",
            unsupported_element_examples.join(", ")
        ));
    }
    if include_count > 0 {
        warnings.push(format!(
            "Abaqus references {include_count} external *INCLUDE file(s); external files were not read"
        ));
    }
    if high_order_elements {
        warnings.push(
            "Abaqus higher-order element midside nodes are not used; straight corner-node edges are shown".into(),
        );
    }
    if mesh.z_coordinates.values().any(|z| z.abs() > 1.0e-12) {
        warnings.push(
            "Abaqus 3D coordinates are shown in the XY projection; Z depth is not represented"
                .into(),
        );
    }
    Ok((
        mesh.nodes,
        mesh.cells,
        title.unwrap_or_else(|| "Abaqus finite-element mesh".into()),
        warnings,
    ))
}

fn parse_keyword(line: &str, line_number: usize) -> Result<(String, HashMap<String, String>)> {
    let body = line.strip_prefix('*').ok_or_else(|| {
        Error::InvalidInput(format!("Abaqus keyword at line {line_number} is invalid"))
    })?;
    if body.trim_end().ends_with(',') {
        return Err(Error::Unsupported(format!(
            "Abaqus keyword continuation at line {line_number} is unsupported"
        )));
    }
    let mut fields = body.split(',');
    let keyword = fields
        .next()
        .map(str::trim)
        .filter(|keyword| !keyword.is_empty())
        .ok_or_else(|| Error::InvalidInput(format!("empty Abaqus keyword at line {line_number}")))?
        .to_ascii_uppercase();
    let mut parameters = HashMap::new();
    for field in fields {
        let Some((key, value)) = field.split_once('=') else {
            continue;
        };
        let key = key.trim().to_ascii_uppercase();
        let value = value
            .trim()
            .trim_matches(|character| character == '\'' || character == '"');
        if !key.is_empty() {
            parameters.insert(key, value.to_owned());
        }
    }
    Ok((keyword, parameters))
}

fn required_parameter<'a>(
    parameters: &'a HashMap<String, String>,
    key: &str,
    line_number: usize,
) -> Result<&'a str> {
    parameters
        .get(key)
        .map(String::as_str)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| {
            Error::InvalidInput(format!(
                "Abaqus keyword at line {line_number} is missing {key}="
            ))
        })
}

fn normalize_label(value: &str) -> String {
    value.trim().to_ascii_uppercase()
}

fn parse_positive_label(value: &str, context: &str, line_number: usize) -> Result<usize> {
    let label = value.trim().parse::<usize>().map_err(|_| {
        Error::InvalidInput(format!(
            "invalid Abaqus {context} label '{value}' at line {line_number}"
        ))
    })?;
    if label == 0 {
        return Err(Error::InvalidInput(format!(
            "Abaqus {context} label must be positive at line {line_number}"
        )));
    }
    Ok(label)
}

fn parse_coordinate(value: &str, line_number: usize) -> Result<f64> {
    let normalized = value.trim().replace(['D', 'd'], "E");
    let coordinate = parse_cad_float(&normalized).ok_or_else(|| {
        Error::InvalidInput(format!(
            "invalid Abaqus coordinate '{value}' at line {line_number}"
        ))
    })?;
    if !coordinate.is_finite() || coordinate.abs() > 1.0e12 {
        return Err(Error::InvalidInput(format!(
            "Abaqus coordinate is non-finite or outside ±1e12 at line {line_number}"
        )));
    }
    Ok(coordinate)
}

fn parse_node_record(line: &str, line_number: usize) -> Result<(usize, f64, f64, f64)> {
    if line.trim_end().ends_with(',') {
        return Err(Error::InvalidInput(format!(
            "Abaqus node data at line {line_number} cannot continue across lines"
        )));
    }
    let mut fields = line.split(',').map(str::trim);
    let label = fields.next().ok_or_else(|| {
        Error::InvalidInput(format!("missing Abaqus node label at line {line_number}"))
    })?;
    let x = fields.next().ok_or_else(|| {
        Error::InvalidInput(format!("missing Abaqus node X at line {line_number}"))
    })?;
    let y = fields.next().ok_or_else(|| {
        Error::InvalidInput(format!("missing Abaqus node Y at line {line_number}"))
    })?;
    let id = parse_positive_label(label, "node", line_number)?;
    let x = parse_coordinate(x, line_number)?;
    let y = parse_coordinate(y, line_number)?;
    let z = fields
        .next()
        .map(|value| parse_coordinate(value, line_number))
        .transpose()?
        .unwrap_or(0.0);
    if fields.next().is_some() {
        return Err(Error::InvalidInput(format!(
            "Abaqus node data at line {line_number} has more than three coordinates"
        )));
    }
    Ok((id, x, y, z))
}

fn append_element_values(
    line: &str,
    line_number: usize,
    expected_fields: usize,
    pending: &mut [usize; 21],
    pending_len: &mut usize,
    pending_line: &mut usize,
) -> Result<bool> {
    let continued = line.trim_end().ends_with(',');
    if *pending_len == 0 {
        *pending_line = line_number;
    }
    let mut fields = line.split(',').peekable();
    while let Some(field) = fields.next() {
        let field = field.trim();
        if field.is_empty() && continued && fields.peek().is_none() {
            break;
        }
        if field.is_empty() {
            return Err(Error::InvalidInput(format!(
                "Abaqus element data at line {line_number} contains an empty field"
            )));
        }
        if *pending_len >= expected_fields || *pending_len >= pending.len() {
            return Err(Error::InvalidInput(format!(
                "Abaqus element data at line {pending_line} has more than {} node fields",
                expected_fields.saturating_sub(1)
            )));
        }
        let context = if *pending_len == 0 {
            "element"
        } else {
            "element node"
        };
        pending[*pending_len] = parse_positive_label(field, context, line_number)?;
        *pending_len += 1;
    }
    if *pending_len == expected_fields {
        if continued {
            return Err(Error::InvalidInput(format!(
                "Abaqus element data at line {pending_line} continues after the expected connectivity"
            )));
        }
        return Ok(true);
    }
    if !continued {
        return Err(Error::InvalidInput(format!(
            "Abaqus element data at line {pending_line} has incomplete connectivity"
        )));
    }
    Ok(false)
}

fn csv_fields(
    line: &str,
    allow_continuation: bool,
    line_number: usize,
) -> Result<(Vec<String>, bool)> {
    let continued = line.trim_end().ends_with(',');
    if continued && !allow_continuation {
        return Err(Error::InvalidInput(format!(
            "Abaqus data record at line {line_number} ends with an unsupported continuation comma"
        )));
    }
    let mut fields = line.split(',').map(str::trim).collect::<Vec<_>>();
    if continued && fields.last().is_some_and(|field| field.is_empty()) {
        fields.pop();
    }
    if fields.iter().any(|field| field.is_empty()) {
        return Err(Error::InvalidInput(format!(
            "Abaqus data record at line {line_number} contains an empty field"
        )));
    }
    Ok((fields.into_iter().map(str::to_owned).collect(), continued))
}

fn element_topology(name: &str) -> Option<AbaqusElementTopology> {
    let (node_count, vtk_cell_type, corner_count) = if name.starts_with("C3D20") {
        (20, 25, 8)
    } else if name.starts_with("C3D15") {
        (15, 26, 6)
    } else if name.starts_with("C3D10") {
        (10, 24, 4)
    } else if name.starts_with("C3D8") {
        (8, 12, 8)
    } else if name.starts_with("C3D6") {
        (6, 13, 6)
    } else if name.starts_with("C3D5") {
        (5, 14, 5)
    } else if name.starts_with("C3D4") {
        (4, 10, 4)
    } else if ["CPS3", "CPE3", "CAX3", "S3", "M3D3"]
        .iter()
        .any(|prefix| name.starts_with(prefix))
    {
        (3, 5, 3)
    } else if ["CPS4", "CPE4", "CAX4", "S4", "M3D4"]
        .iter()
        .any(|prefix| name.starts_with(prefix))
    {
        (4, 9, 4)
    } else if ["CPS6", "CPE6", "CAX6", "S6"]
        .iter()
        .any(|prefix| name.starts_with(prefix))
    {
        (6, 22, 3)
    } else if ["CPS8", "CPE8", "CAX8", "S8"]
        .iter()
        .any(|prefix| name.starts_with(prefix))
    {
        (8, 23, 4)
    } else if ["B21", "B31", "T2D2", "T3D2"]
        .iter()
        .any(|prefix| name.starts_with(prefix))
    {
        (2, 3, 2)
    } else if ["B22", "B32", "T2D3", "T3D3"]
        .iter()
        .any(|prefix| name.starts_with(prefix))
    {
        (3, 21, 2)
    } else {
        return None;
    };
    Some(AbaqusElementTopology {
        node_count,
        vtk_cell_type,
        corner_count,
    })
}

fn active_mesh_mut<'a>(
    instance: &'a mut Option<AbaqusInstance>,
    part_name: Option<&str>,
    parts: &'a mut HashMap<String, AbaqusMesh>,
    global_mesh: &'a mut AbaqusMesh,
) -> Result<&'a mut AbaqusMesh> {
    if let Some(instance) = instance.as_mut() {
        return Ok(&mut instance.mesh);
    }
    if let Some(part_name) = part_name {
        return parts.get_mut(part_name).ok_or_else(|| {
            Error::InvalidInput(format!("Abaqus part '{part_name}' was not initialized"))
        });
    }
    Ok(global_mesh)
}

fn parse_instance_position(
    instance: &mut AbaqusInstance,
    line: &str,
    line_number: usize,
) -> Result<()> {
    let (fields, _) = csv_fields(line, false, line_number)?;
    let numbers = fields
        .iter()
        .map(|field| parse_coordinate(field, line_number))
        .collect::<Result<Vec<_>>>()?;
    match numbers.as_slice() {
        [x, y, z] if instance.position_line_count == 0 && instance.rotation.is_none() => {
            instance.translation = [*x, *y, *z];
            instance.position_line_count = 1;
        }
        [ax, ay, az, bx, by, bz, angle]
            if instance.position_line_count == 1 && instance.rotation.is_none() =>
        {
            instance.rotation = Some(([*ax, *ay, *az], [*bx, *by, *bz], *angle));
            instance.position_line_count = 2;
        }
        _ => {
            return Err(Error::InvalidInput(format!(
                "Abaqus instance positioning at line {line_number} must start with a 3-value translation and may then include a 7-value axis-angle rotation"
            )));
        }
    }
    Ok(())
}

fn append_mesh_instance(
    destination: &mut AbaqusMesh,
    source: &AbaqusMesh,
    translation: [f64; 3],
    rotation: Option<([f64; 3], [f64; 3], f64)>,
    scope_name: &str,
) -> Result<()> {
    if destination
        .nodes
        .len()
        .checked_add(source.nodes.len())
        .is_none_or(|total| total > MAX_SIMULATION_POINTS)
    {
        return Err(Error::LimitExceeded(format!(
            "Abaqus expanded instance nodes exceed {MAX_SIMULATION_POINTS}"
        )));
    }
    let mut source_ids = source.nodes.keys().copied().collect::<Vec<_>>();
    source_ids.sort_unstable();
    let mut remapped = HashMap::with_capacity(source_ids.len());
    for source_id in source_ids {
        let source_node = source.nodes.get(&source_id).ok_or_else(|| {
            Error::InvalidInput(format!(
                "Abaqus node {source_id} disappeared during expansion"
            ))
        })?;
        let [x, y, z] = transform_instance_coordinate(
            [
                source_node.x,
                source_node.y,
                source.z_coordinates.get(&source_id).copied().unwrap_or(0.0),
            ],
            translation,
            rotation,
            scope_name,
        )?;
        let target_id =
            destination.nodes.len().checked_add(1).ok_or_else(|| {
                Error::LimitExceeded("Abaqus expanded node label overflowed".into())
            })?;
        destination.nodes.insert(
            target_id,
            MeshNode {
                x,
                y,
                scalar: source_node.scalar,
            },
        );
        destination.z_coordinates.insert(target_id, z);
        remapped.insert(source_id, target_id);
    }
    for source_cell in &source.cells {
        if destination.cells.len() >= MAX_VTK_RENDERED_PRIMITIVES {
            return Err(Error::LimitExceeded(format!(
                "Abaqus expanded primitive count exceeds {MAX_VTK_RENDERED_PRIMITIVES}"
            )));
        }
        let node_ids = source_cell
            .node_ids
            .iter()
            .map(|source_id| {
                remapped.get(source_id).copied().ok_or_else(|| {
                    Error::InvalidInput(format!(
                        "Abaqus element references missing local node {source_id}"
                    ))
                })
            })
            .collect::<Result<Vec<_>>>()?;
        destination.cells.push(MeshCell {
            node_ids,
            scalar: source_cell.scalar,
        });
    }
    Ok(())
}

fn transform_instance_coordinate(
    mut point: [f64; 3],
    translation: [f64; 3],
    rotation: Option<([f64; 3], [f64; 3], f64)>,
    instance_name: &str,
) -> Result<[f64; 3]> {
    // Abaqus applies the optional translation before the axis-angle rotation.
    for (coordinate, offset) in point.iter_mut().zip(translation) {
        *coordinate += offset;
    }
    if let Some((axis_start, axis_end, angle_degrees)) = rotation {
        let axis = [
            axis_end[0] - axis_start[0],
            axis_end[1] - axis_start[1],
            axis_end[2] - axis_start[2],
        ];
        let axis_length = (axis[0] * axis[0] + axis[1] * axis[1] + axis[2] * axis[2]).sqrt();
        if !axis_length.is_finite() || axis_length <= 1.0e-12 {
            return Err(Error::InvalidInput(format!(
                "Abaqus instance '{instance_name}' has a degenerate rotation axis"
            )));
        }
        let unit_axis = [
            axis[0] / axis_length,
            axis[1] / axis_length,
            axis[2] / axis_length,
        ];
        let angle = angle_degrees.to_radians();
        let cosine = angle.cos();
        let sine = angle.sin();
        let relative = [
            point[0] - axis_start[0],
            point[1] - axis_start[1],
            point[2] - axis_start[2],
        ];
        let cross = [
            unit_axis[1] * relative[2] - unit_axis[2] * relative[1],
            unit_axis[2] * relative[0] - unit_axis[0] * relative[2],
            unit_axis[0] * relative[1] - unit_axis[1] * relative[0],
        ];
        let dot =
            unit_axis[0] * relative[0] + unit_axis[1] * relative[1] + unit_axis[2] * relative[2];
        for coordinate in 0..3 {
            point[coordinate] = axis_start[coordinate]
                + relative[coordinate] * cosine
                + cross[coordinate] * sine
                + unit_axis[coordinate] * dot * (1.0 - cosine);
        }
    }
    for coordinate in &point {
        if !coordinate.is_finite() || coordinate.abs() > 1.0e12 {
            return Err(Error::InvalidInput(format!(
                "Abaqus instance '{instance_name}' transformed a node outside ±1e12"
            )));
        }
    }
    Ok(point)
}

#[cfg(test)]
mod tests {
    use super::parse_abaqus;
    use crate::error::Error;

    #[test]
    fn parses_a_flat_quadratic_tetrahedron_and_warns_about_midside_nodes() {
        let input = "*Heading\nC3D10 tetrahedron\n*Node\n1, 0, 0, 0\n2, 1, 0, 0\n3, 0, 1, 0\n4, 0, 0, 1\n5, .5, 0, 0\n6, .5, .5, 0\n7, 0, .5, 0\n8, 0, 0, .5\n9, .5, 0, .5\n10, 0, .5, .5\n*Element, type=C3D10\n1, 1, 2, 3, 4, 5,\n6, 7, 8, 9, 10\n";
        let (nodes, cells, title, warnings) = parse_abaqus(input).unwrap();
        assert_eq!(title, "C3D10 tetrahedron");
        assert_eq!(nodes.len(), 10);
        assert_eq!(cells.len(), 6);
        assert!(cells.iter().all(|cell| cell.node_ids.len() == 2));
        assert!(
            warnings
                .iter()
                .any(|warning| warning.contains("midside nodes"))
        );
    }

    #[test]
    fn expands_part_meshes_to_transformed_instances() {
        let input = "*Part, name=Triangle\n*Node\n1, 1, 0, 0\n2, 2, 0, 0\n3, 1, 1, 0\n*Element, type=CPS3\n1, 1, 2, 3\n*End Part\n*Assembly, name=Model\n*Instance, name=Base, part=Triangle\n*End Instance\n*Instance, name=Rotated, part=Triangle\n10, 0, 0\n0, 0, 0, 0, 0, 1, 90\n*End Instance\n*End Assembly\n";
        let (nodes, cells, _, warnings) = parse_abaqus(input).unwrap();
        assert_eq!(nodes.len(), 6);
        assert_eq!(cells.len(), 2);
        assert!(nodes[&4].x.abs() < 1.0e-12);
        assert!((nodes[&4].y - 11.0).abs() < 1.0e-12);
        assert!(warnings.is_empty());
    }

    #[test]
    fn reports_external_includes_without_reading_them() {
        let input = "*Heading\nLocal mesh\n*Include, input=outside-mesh.inp\n*Node\n1, 0, 0\n2, 1, 0\n3, 0, 1\n*Element, type=CPS3\n1, 1, 2, 3\n";
        let (nodes, cells, _, warnings) = parse_abaqus(input).unwrap();
        assert_eq!(nodes.len(), 3);
        assert_eq!(cells.len(), 1);
        assert!(
            warnings
                .iter()
                .any(|warning| warning.contains("external *INCLUDE"))
        );
    }

    #[test]
    fn rejects_non_cartesian_node_coordinate_systems() {
        let input = "*Heading\nUnsupported cylindrical coordinates\n*Node, system=C\n1, 0, 0, 0\n*Element, type=CPS3\n1, 1, 1, 1\n";
        assert!(matches!(
            parse_abaqus(input),
            Err(Error::Unsupported(message)) if message.contains("SYSTEM=R is Cartesian")
        ));
    }
}
