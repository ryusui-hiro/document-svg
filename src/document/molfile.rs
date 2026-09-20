//! Bounded V2000 MOL/SDF/RXN chemical structure previews.
//!
//! This reader draws the supplied atom coordinates and connection table. It
//! does not perform chemical perception, layout, valence repair, or reaction
//! analysis, and it never evaluates SDF data fields.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use crate::convert::{ConvertOptions, PageConsumer, SourceFormat, read_limited_file};
use crate::error::{Error, Result};
use crate::ir::{IDENTITY, Node, Page, Paint, SourceMeta, Stroke, TextAnchor, TextRun};

const MAX_CTFILE_BYTES: u64 = 64 * 1024 * 1024;
const MAX_CTFILE_LINES: usize = 1_000_000;
const MAX_CTFILE_LINE_BYTES: usize = 1024 * 1024;
const MAX_RECORDS: usize = 10_000;
const MAX_RECORD_BYTES: usize = 8 * 1024 * 1024;
const MAX_ATOMS_PER_RECORD: usize = 20_000;
const MAX_BONDS_PER_RECORD: usize = 100_000;
const MAX_TOTAL_ATOMS: usize = 200_000;
const MAX_TOTAL_BONDS: usize = 500_000;
const MAX_PAGE_NODES: usize = 150_000;
const MAX_TOTAL_OUTPUT_NODES: usize = 1_000_000;
const MAX_RXN_COMPONENTS_PER_SIDE: usize = 2;
const MAX_RXN_OUTPUT_NODES: usize = 700_000;
const MAX_COORDINATE: f64 = 1_000_000.0;
const MAX_PREVIEW_SCALE: f64 = 320.0;

#[derive(Clone, Debug)]
pub(crate) struct Atom {
    pub(crate) x: f64,
    pub(crate) y: f64,
    pub(crate) element: String,
    pub(crate) charge: i8,
    pub(crate) isotope: Option<u16>,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct Bond {
    pub(crate) from: usize,
    pub(crate) to: usize,
    pub(crate) order: u8,
    pub(crate) stereo: u8,
}

#[derive(Default)]
pub(crate) struct Molecule {
    pub(crate) title: String,
    pub(crate) atoms: Vec<Atom>,
    pub(crate) bonds: Vec<Bond>,
    pub(crate) warnings: Vec<String>,
}

pub(crate) fn looks_like_prefix(bytes: &[u8]) -> Option<SourceFormat> {
    let text = std::str::from_utf8(bytes).ok()?;
    let lines = text.lines().take(4).collect::<Vec<_>>();
    if lines.len() < 4 || parse_counts_line(lines[3]).is_err() {
        return None;
    }
    if lines.iter().any(|line| line.contains("V3000")) {
        return Some(SourceFormat::Mol);
    }
    if !lines[3].contains("V2000") {
        return None;
    }
    Some(if text.contains("$$$$") {
        SourceFormat::Sdf
    } else {
        SourceFormat::Mol
    })
}

pub(crate) fn looks_like_rxn_prefix(bytes: &[u8]) -> bool {
    let prefix = String::from_utf8_lossy(bytes);
    prefix.lines().next().is_some_and(|line| {
        let marker = line.trim();
        marker == "$RXN" || marker.starts_with("$RXN ")
    })
}

pub(crate) fn convert(
    path: &Path,
    options: &ConvertOptions,
    format: SourceFormat,
    sink: &mut dyn PageConsumer,
) -> Result<Vec<String>> {
    let max_bytes = options.max_input_bytes.min(MAX_CTFILE_BYTES);
    let bytes = read_limited_file(path, max_bytes, "MOL/SDF input")?;
    let text = std::str::from_utf8(&bytes).map_err(|error| {
        Error::InvalidInput(format!("MOL/SDF input is not UTF-8 or ASCII: {error}"))
    })?;
    convert_text(text, options, format, sink)
}

pub(crate) fn convert_rxn(
    path: &Path,
    options: &ConvertOptions,
    sink: &mut dyn PageConsumer,
) -> Result<Vec<String>> {
    let max_bytes = options.max_input_bytes.min(MAX_CTFILE_BYTES);
    let bytes = read_limited_file(path, max_bytes, "RXN input")?;
    let text = std::str::from_utf8(&bytes).map_err(|error| {
        Error::InvalidInput(format!("RXN input is not UTF-8 or ASCII: {error}"))
    })?;
    let mut line_count = 0usize;
    for line in text.lines() {
        line_count = line_count.saturating_add(1);
        if line_count > MAX_CTFILE_LINES {
            return Err(Error::LimitExceeded(format!(
                "RXN input exceeds {MAX_CTFILE_LINES} lines"
            )));
        }
        if line.len() > MAX_CTFILE_LINE_BYTES {
            return Err(Error::LimitExceeded(format!(
                "RXN line exceeds {MAX_CTFILE_LINE_BYTES} bytes"
            )));
        }
    }
    if options.max_pages == 0 {
        return Err(Error::LimitExceeded(
            "RXN needs one output page, but max_pages is 0".into(),
        ));
    }
    let lines = text.lines().collect::<Vec<_>>();
    let reaction = parse_reaction(&lines)?;
    let page = render_reaction(&reaction)?;
    let warnings = page.warnings.clone();
    sink.consume(page)?;
    Ok(warnings)
}

struct Reaction {
    title: String,
    reactants: Vec<Molecule>,
    products: Vec<Molecule>,
    warnings: Vec<String>,
}

fn parse_reaction(lines: &[&str]) -> Result<Reaction> {
    let marker = lines.first().map(|line| line.trim()).unwrap_or_default();
    if lines.len() < 5 || !(marker == "$RXN" || marker.starts_with("$RXN ")) {
        return Err(invalid(
            "RXN header must begin with $RXN and include a counts line",
        ));
    }
    if marker != "$RXN" || lines[4].contains("V3000") {
        return Err(Error::Unsupported(
            "RXN V3000 reaction tables are not supported; this reader accepts V2000".into(),
        ));
    }
    let reactant_count = parse_fixed_count_field(lines[4], 0, "reactant count")?;
    let product_count = parse_fixed_count_field(lines[4], 3, "product count")?;
    if !(1..=MAX_RXN_COMPONENTS_PER_SIDE).contains(&reactant_count)
        || !(1..=MAX_RXN_COMPONENTS_PER_SIDE).contains(&product_count)
    {
        return Err(Error::Unsupported(format!(
            "RXN preview supports 1..={MAX_RXN_COMPONENTS_PER_SIDE} reactants and products; found {reactant_count} and {product_count}"
        )));
    }
    let mut cursor = 5usize;
    let mut reactants = Vec::with_capacity(reactant_count);
    let mut products = Vec::with_capacity(product_count);
    for (role, count, output) in [
        ("reactant", reactant_count, &mut reactants),
        ("product", product_count, &mut products),
    ] {
        for index in 0..count {
            while lines.get(cursor).is_some_and(|line| line.trim().is_empty()) {
                cursor += 1;
            }
            if lines.get(cursor).map(|line| line.trim()) != Some("$MOL") {
                return Err(invalid(format!(
                    "RXN is missing $MOL before {role} {}",
                    index + 1
                )));
            }
            cursor += 1;
            let molecule_start = cursor;
            let end_offset = lines[molecule_start..]
                .iter()
                .position(|line| matches!(line.trim(), "M  END" | "M END"))
                .ok_or_else(|| invalid(format!("RXN {role} {} has no M END", index + 1)))?;
            let end = molecule_start + end_offset;
            let molecule_lines = &lines[molecule_start..=end];
            if molecule_lines
                .iter()
                .map(|line| line.len() + 1)
                .sum::<usize>()
                > MAX_RECORD_BYTES
            {
                return Err(Error::LimitExceeded(format!(
                    "RXN {role} {} exceeds {MAX_RECORD_BYTES} bytes",
                    index + 1
                )));
            }
            output.push(parse_molecule(molecule_lines)?);
            cursor = end + 1;
        }
    }
    let mut warnings = Vec::new();
    if lines[cursor..].iter().any(|line| !line.trim().is_empty()) {
        warnings.push("RXN reaction-level metadata is omitted".into());
    }
    Ok(Reaction {
        title: lines[1]
            .trim()
            .chars()
            .filter(|ch| !ch.is_control())
            .take(160)
            .collect(),
        reactants,
        products,
        warnings,
    })
}

fn parse_fixed_count_field(line: &str, start: usize, label: &str) -> Result<usize> {
    line.get(start..start + 3)
        .ok_or_else(|| invalid(format!("RXN counts line is missing {label}")))?
        .trim()
        .parse::<usize>()
        .map_err(|_| invalid(format!("RXN counts line has an invalid {label}")))
}

fn render_reaction(reaction: &Reaction) -> Result<Page> {
    const WIDTH: f64 = 1_400.0;
    const HEIGHT: f64 = 720.0;
    const REGION_X: [f64; 2] = [20.0, 880.0];
    const REGION_WIDTH: f64 = 500.0;
    const TOP: f64 = 70.0;
    const REGION_HEIGHT: f64 = 580.0;

    let mut page = Page::new(1, WIDTH, HEIGHT, "rxn");
    page.title = if reaction.title.is_empty() {
        "Chemical reaction".into()
    } else {
        reaction.title.clone()
    };
    page.description = format!(
        "V2000 reaction preview: {} reactant(s) to {} product(s)",
        reaction.reactants.len(),
        reaction.products.len()
    );
    for warning in &reaction.warnings {
        page.warn(warning.clone());
    }
    let reactant_label = component_heading("Reactants", REGION_X[0] + REGION_WIDTH / 2.0, 45.0);
    let product_label = component_heading("Products", REGION_X[1] + REGION_WIDTH / 2.0, 45.0);
    page.nodes.extend([reactant_label, product_label]);

    for (side_index, molecules) in [&reaction.reactants, &reaction.products]
        .into_iter()
        .enumerate()
    {
        let panel_height = REGION_HEIGHT / molecules.len() as f64;
        for (index, molecule) in molecules.iter().enumerate() {
            let component_page = render_molecule(molecule, index + 1, "mol")?;
            for warning in &component_page.warnings {
                page.warn(warning.clone());
            }
            let scale =
                (REGION_WIDTH / component_page.width).min(panel_height / component_page.height);
            let translate_x =
                REGION_X[side_index] + (REGION_WIDTH - component_page.width * scale) / 2.0;
            let row_y = TOP + index as f64 * panel_height;
            let translate_y = row_y + (panel_height - component_page.height * scale) / 2.0;
            let prefix = format!(
                "rxn-{}-{}",
                if side_index == 0 {
                    "reactant"
                } else {
                    "product"
                },
                index + 1
            );
            let mut child_nodes = component_page.nodes;
            prefix_node_ids(&mut child_nodes, &prefix);
            page.nodes.push(Node::Group {
                id: prefix,
                nodes: child_nodes,
                transform: [scale, 0.0, 0.0, scale, translate_x, translate_y],
                opacity: 1.0,
                clip_id: None,
                meta: SourceMeta {
                    semantic_role: if side_index == 0 {
                        "chemical:reactant"
                    } else {
                        "chemical:product"
                    }
                    .into(),
                    ..Default::default()
                },
            });
            if index + 1 < molecules.len() {
                page.nodes.push(Node::Text {
                    id: format!("rxn-plus-{}-{index}", side_index),
                    x: REGION_X[side_index] + REGION_WIDTH / 2.0,
                    y: row_y + panel_height,
                    runs: vec![TextRun {
                        text: "+".into(),
                        font_family: "Arial, sans-serif".into(),
                        font_size: 28.0,
                        bold: true,
                        fill: Paint::solid("#475569"),
                        ..TextRun::default()
                    }],
                    anchor: TextAnchor::Middle,
                    transform: IDENTITY,
                    opacity: 1.0,
                    stroke: Stroke::default(),
                    clip_id: None,
                    meta: SourceMeta {
                        semantic_role: "chemical:plus-separator".into(),
                        ..Default::default()
                    },
                });
            }
        }
    }
    page.nodes.push(Node::Path {
        id: "rxn-arrow".into(),
        d: "M 615 360 L 785 360 M 765 345 L 785 360 L 765 375".into(),
        fill_rule: String::new(),
        fill: Paint::None,
        stroke: Stroke {
            paint: Paint::solid("#334155"),
            width: 4.0,
            line_cap: crate::ir::LineCap::Round,
            line_join: crate::ir::LineJoin::Round,
            ..Default::default()
        },
        transform: IDENTITY,
        clip_id: None,
        meta: SourceMeta {
            semantic_role: "chemical:reaction-arrow".into(),
            ..Default::default()
        },
    });
    if page.nodes.len() > MAX_RXN_OUTPUT_NODES {
        return Err(Error::LimitExceeded(format!(
            "RXN diagram exceeds {MAX_RXN_OUTPUT_NODES} nodes"
        )));
    }
    Ok(page)
}

fn component_heading(text: &str, x: f64, y: f64) -> Node {
    Node::Text {
        id: format!("rxn-heading-{}", text.to_ascii_lowercase()),
        x,
        y,
        runs: vec![TextRun {
            text: text.into(),
            font_family: "Arial, sans-serif".into(),
            font_size: 18.0,
            bold: true,
            fill: Paint::solid("#64748b"),
            ..TextRun::default()
        }],
        anchor: TextAnchor::Middle,
        transform: IDENTITY,
        opacity: 1.0,
        stroke: Stroke::default(),
        clip_id: None,
        meta: SourceMeta {
            semantic_role: "chemical:reaction-heading".into(),
            ..Default::default()
        },
    }
}

fn prefix_node_ids(nodes: &mut [Node], prefix: &str) {
    for node in nodes {
        match node {
            Node::Path { id, .. } | Node::Text { id, .. } | Node::Image { id, .. } => {
                *id = format!("{prefix}-{id}");
            }
            Node::Group { id, nodes, .. } => {
                *id = format!("{prefix}-{id}");
                prefix_node_ids(nodes, prefix);
            }
        }
    }
}

fn convert_text(
    text: &str,
    options: &ConvertOptions,
    format: SourceFormat,
    sink: &mut dyn PageConsumer,
) -> Result<Vec<String>> {
    if !matches!(format, SourceFormat::Mol | SourceFormat::Sdf) {
        return Err(Error::InvalidInput(
            "chemical structure converter received the wrong source format".into(),
        ));
    }
    let mut line_count = 0usize;
    for line in text.lines() {
        line_count = line_count.saturating_add(1);
        if line_count > MAX_CTFILE_LINES {
            return Err(Error::LimitExceeded(format!(
                "MOL/SDF input exceeds {MAX_CTFILE_LINES} lines"
            )));
        }
        if line.len() > MAX_CTFILE_LINE_BYTES {
            return Err(Error::LimitExceeded(format!(
                "MOL/SDF line exceeds {MAX_CTFILE_LINE_BYTES} bytes"
            )));
        }
    }

    let is_sdf = format == SourceFormat::Sdf;
    if !is_sdf && text.lines().any(|line| line.trim() == "$$$$") {
        return Err(Error::InvalidInput(
            "MOL input contains SDF record separators; use the .sdf extension".into(),
        ));
    }
    let records = split_records(text, is_sdf)?;
    if records.is_empty() {
        return Err(Error::InvalidInput(
            "MOL/SDF input contains no molecule records".into(),
        ));
    }
    if records.len() > MAX_RECORDS || records.len() > options.max_pages {
        return Err(Error::LimitExceeded(format!(
            "MOL/SDF contains {} molecule records; maximum is {}",
            records.len(),
            MAX_RECORDS.min(options.max_pages)
        )));
    }

    let source_format = if is_sdf { "sdf" } else { "mol" };
    let mut total_atoms = 0usize;
    let mut total_bonds = 0usize;
    let mut total_nodes = 0usize;
    let mut warnings = Vec::new();
    for (index, record) in records.iter().enumerate() {
        if record.len() > MAX_RECORD_BYTES {
            return Err(Error::LimitExceeded(format!(
                "MOL/SDF record {} exceeds {MAX_RECORD_BYTES} bytes",
                index + 1
            )));
        }
        let molecule = parse_molecule(record)?;
        total_atoms = total_atoms
            .checked_add(molecule.atoms.len())
            .ok_or_else(|| Error::LimitExceeded("MOL/SDF atom count overflowed".into()))?;
        total_bonds = total_bonds
            .checked_add(molecule.bonds.len())
            .ok_or_else(|| Error::LimitExceeded("MOL/SDF bond count overflowed".into()))?;
        if total_atoms > MAX_TOTAL_ATOMS || total_bonds > MAX_TOTAL_BONDS {
            return Err(Error::LimitExceeded(format!(
                "MOL/SDF total atoms or bonds exceed {MAX_TOTAL_ATOMS} atoms / {MAX_TOTAL_BONDS} bonds"
            )));
        }
        let page = render_molecule(&molecule, index + 1, source_format)?;
        total_nodes = total_nodes
            .checked_add(page.nodes.len())
            .ok_or_else(|| Error::LimitExceeded("MOL/SDF output node count overflowed".into()))?;
        if total_nodes > MAX_TOTAL_OUTPUT_NODES {
            return Err(Error::LimitExceeded(format!(
                "MOL/SDF exceeds {MAX_TOTAL_OUTPUT_NODES} total SVG nodes"
            )));
        }
        for warning in &page.warnings {
            warnings.push(warning.clone());
        }
        sink.consume(page)?;
    }
    warnings.sort();
    warnings.dedup();
    Ok(warnings)
}

fn split_records(text: &str, is_sdf: bool) -> Result<Vec<Vec<&str>>> {
    if !is_sdf {
        return Ok(vec![text.lines().collect()]);
    }
    let mut records = Vec::new();
    let mut current = Vec::new();
    for line in text.lines() {
        if line.trim() == "$$$$" {
            if current.iter().any(|line: &&str| !line.trim().is_empty()) {
                records.push(std::mem::take(&mut current));
            }
        } else {
            current.push(line);
        }
    }
    if current.iter().any(|line| !line.trim().is_empty()) {
        records.push(current);
    }
    if records.len() > MAX_RECORDS {
        return Err(Error::LimitExceeded(format!(
            "SDF exceeds {MAX_RECORDS} molecule records"
        )));
    }
    Ok(records)
}

fn parse_molecule(lines: &[&str]) -> Result<Molecule> {
    if lines.len() < 4 {
        return Err(invalid(
            "molecule record is shorter than its header and counts line",
        ));
    }
    let title = lines[0]
        .trim()
        .chars()
        .filter(|character| !character.is_control())
        .take(160)
        .collect::<String>();
    if lines[3].contains("V3000") {
        return parse_v3000_molecule(lines, title);
    }
    let (atom_count, bond_count) = parse_counts_line(lines[3])?;
    if atom_count > MAX_ATOMS_PER_RECORD || bond_count > MAX_BONDS_PER_RECORD {
        return Err(Error::LimitExceeded(format!(
            "MOL/SDF record exceeds {MAX_ATOMS_PER_RECORD} atoms or {MAX_BONDS_PER_RECORD} bonds"
        )));
    }
    if !lines[3].contains("V2000") {
        return Err(Error::Unsupported(
            "MOL/SDF connection table has no V2000 version marker".into(),
        ));
    }
    let records_end = 4usize
        .checked_add(atom_count)
        .and_then(|value| value.checked_add(bond_count))
        .ok_or_else(|| Error::LimitExceeded("MOL/SDF record block offsets overflowed".into()))?;
    if records_end > lines.len() {
        return Err(invalid(
            "counts line exceeds available atom or bond records",
        ));
    }

    let mut molecule = Molecule {
        title,
        atoms: Vec::with_capacity(atom_count),
        bonds: Vec::with_capacity(bond_count),
        warnings: Vec::new(),
    };
    let mut has_nonzero_z = false;
    for line in &lines[4..4 + atom_count] {
        let fields = line.split_whitespace().collect::<Vec<_>>();
        if fields.len() < 4 {
            return Err(invalid(
                "atom line must contain x, y, z, and an element symbol",
            ));
        }
        let x = parse_coordinate(fields[0])?;
        let y = parse_coordinate(fields[1])?;
        let z = parse_coordinate(fields[2])?;
        let element = normalize_element(fields[3])?;
        has_nonzero_z |= z.abs() > 1e-6;
        let charge_code = fields
            .get(5)
            .map(|value| value.parse::<u8>())
            .transpose()
            .map_err(|_| invalid("atom charge code is not an integer"))?
            .unwrap_or(0);
        if charge_code == 4 {
            molecule
                .warnings
                .push("MOL/SDF radical spin state is not represented in the preview".into());
        }
        let charge = charge_from_code(charge_code)?;
        molecule.atoms.push(Atom {
            x,
            y,
            element,
            charge,
            isotope: None,
        });
    }

    let mut seen_bonds = HashSet::new();
    for line in &lines[4 + atom_count..records_end] {
        let fields = line.split_whitespace().collect::<Vec<_>>();
        if fields.len() < 4 {
            return Err(invalid(
                "bond line must contain atom indices, order, and stereo",
            ));
        }
        let from = parse_index(fields[0], atom_count, "bond endpoint")?;
        let to = parse_index(fields[1], atom_count, "bond endpoint")?;
        if from == to {
            return Err(invalid("bond connects an atom to itself"));
        }
        let order = fields[2]
            .parse::<u8>()
            .map_err(|_| invalid("bond order is not an integer"))?;
        if !(1..=8).contains(&order) {
            return Err(invalid(
                "V2000 bond order is outside the supported 1..=8 range",
            ));
        }
        let stereo = fields[3]
            .parse::<u8>()
            .map_err(|_| invalid("bond stereo code is not an integer"))?;
        if !matches!(stereo, 0 | 1 | 3 | 4 | 6) {
            return Err(invalid("V2000 bond stereo code is unsupported"));
        }
        let key = (from.min(to), from.max(to));
        if !seen_bonds.insert(key) {
            return Err(invalid("bond block repeats an atom pair"));
        }
        molecule.bonds.push(Bond {
            from,
            to,
            order,
            stereo,
        });
    }

    let mut found_m_end = false;
    let mut ignored_property = false;
    for line in &lines[records_end..] {
        let trimmed = line.trim();
        if trimmed == "M  END" || trimmed == "M END" {
            found_m_end = true;
            continue;
        }
        if trimmed.starts_with('>') || trimmed.starts_with("$$$$") {
            break;
        }
        if trimmed.starts_with("M  CHG") || trimmed.starts_with("M CHG") {
            apply_atom_properties(trimmed, &mut molecule.atoms, false)?;
        } else if trimmed.starts_with("M  ISO") || trimmed.starts_with("M ISO") {
            apply_atom_properties(trimmed, &mut molecule.atoms, true)?;
        } else if trimmed.starts_with("M ") {
            ignored_property = true;
        }
    }
    if !found_m_end {
        return Err(invalid("property block is missing M END"));
    }
    add_common_molecule_warnings(&mut molecule, has_nonzero_z);
    if ignored_property {
        molecule
            .warnings
            .push("MOL/SDF connection-table properties other than formal charge and isotope labels are omitted".into());
    }
    if lines.iter().any(|line| line.trim_start().starts_with('>')) {
        molecule
            .warnings
            .push("SDF data fields are not displayed or evaluated".into());
    }
    Ok(molecule)
}

fn add_common_molecule_warnings(molecule: &mut Molecule, has_nonzero_z: bool) {
    if has_nonzero_z {
        molecule
            .warnings
            .push("MOL/SDF 3D atom coordinates are projected onto the XY plane".into());
    }
    if !molecule.atoms.is_empty() && molecule.bonds.is_empty() {
        molecule
            .warnings
            .push("MOL/SDF atom-only structure is shown as labeled atom marks".into());
    }
    if molecule
        .bonds
        .iter()
        .any(|bond| (5..=8).contains(&bond.order))
    {
        molecule
            .warnings
            .push("MOL/SDF query bond orders are shown as dashed single bonds".into());
    }
    if molecule.bonds.iter().any(|bond| bond.stereo == 3) {
        molecule
            .warnings
            .push("MOL/SDF cis/trans double-bond stereochemistry is omitted".into());
    }
    if molecule.bonds.iter().any(|bond| bond.stereo == 4) {
        molecule
            .warnings
            .push("MOL/SDF either single-bond stereochemistry is shown as a dashed line".into());
    }
    if molecule
        .atoms
        .iter()
        .any(|atom| matches!(atom.element.as_str(), "L" | "A" | "Q" | "*" | "LP" | "R#"))
    {
        molecule
            .warnings
            .push("MOL/SDF query and placeholder atoms are shown by their literal labels".into());
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum V30Section {
    None,
    Atom,
    Bond,
    Ignored(usize),
}

fn parse_v3000_molecule(lines: &[&str], title: String) -> Result<Molecule> {
    let (v30_lines, after_m_end) = collect_v30_lines(&lines[4..])?;
    let mut molecule = Molecule {
        title,
        atoms: Vec::new(),
        bonds: Vec::new(),
        warnings: Vec::new(),
    };
    let mut atom_ids = HashMap::<u64, usize>::new();
    let mut bond_ids = HashSet::<u64>::new();
    let mut bond_pairs = HashSet::<(usize, usize)>::new();
    let mut section = V30Section::None;
    let mut in_ctab = false;
    let mut found_counts = false;
    let mut found_atom_block = false;
    let mut found_bond_block = false;
    let mut found_ctab_end = false;
    let mut expected_atoms = 0usize;
    let mut expected_bonds = 0usize;
    let mut ignored_metadata = false;
    let mut ignored_atom_properties = false;
    let mut ignored_bond_properties = false;
    let mut atom_maps = false;
    let mut has_nonzero_z = false;

    for line in &v30_lines {
        let fields = split_v30_tokens(line);
        let Some(first) = fields.first().map(String::as_str) else {
            continue;
        };
        if let V30Section::Ignored(depth) = section {
            if first == "BEGIN" {
                section = V30Section::Ignored(depth.saturating_add(1));
            } else if first == "END" {
                section = if depth <= 1 {
                    V30Section::None
                } else {
                    V30Section::Ignored(depth - 1)
                };
            }
            continue;
        }

        if first == "BEGIN" {
            let kind = fields.get(1).map(String::as_str).unwrap_or("");
            match kind {
                "CTAB" if !in_ctab => in_ctab = true,
                "ATOM"
                    if in_ctab
                        && found_counts
                        && section == V30Section::None
                        && !found_atom_block =>
                {
                    section = V30Section::Atom;
                    found_atom_block = true;
                }
                "BOND"
                    if in_ctab
                        && found_atom_block
                        && section == V30Section::None
                        && !found_bond_block =>
                {
                    section = V30Section::Bond;
                    found_bond_block = true;
                }
                "CTAB" => {
                    ignored_metadata = true;
                    section = V30Section::Ignored(1);
                }
                _ => {
                    ignored_metadata = true;
                    section = V30Section::Ignored(1);
                }
            }
            continue;
        }
        if first == "END" {
            let kind = fields.get(1).map(String::as_str).unwrap_or("");
            match (kind, section) {
                ("ATOM", V30Section::Atom) => section = V30Section::None,
                ("BOND", V30Section::Bond) => section = V30Section::None,
                ("CTAB", V30Section::None) if in_ctab => {
                    in_ctab = false;
                    found_ctab_end = true;
                }
                _ => return Err(invalid("V3000 block end does not match its open block")),
            }
            continue;
        }

        match section {
            V30Section::Atom => {
                if fields.len() < 6 {
                    return Err(invalid("V3000 atom line is truncated"));
                }
                let id = parse_positive_v30_integer(&fields[0], "atom index")?;
                if atom_ids.contains_key(&id) {
                    return Err(invalid("V3000 atom index is repeated"));
                }
                let element = normalize_element(&fields[1])?;
                let x = parse_coordinate(&fields[2])?;
                let y = parse_coordinate(&fields[3])?;
                let z = parse_coordinate(&fields[4])?;
                has_nonzero_z |= z.abs() > 1e-6;
                let atom_map = fields[5]
                    .parse::<u32>()
                    .map_err(|_| invalid("V3000 atom-atom map index is invalid"))?;
                atom_maps |= atom_map != 0;
                let mut atom = Atom {
                    x,
                    y,
                    element,
                    charge: 0,
                    isotope: None,
                };
                for property in fields.iter().skip(6) {
                    let Some((key, value)) = property.split_once('=') else {
                        ignored_atom_properties = true;
                        continue;
                    };
                    match key {
                        "CHG" => {
                            let charge = value
                                .parse::<i16>()
                                .map_err(|_| invalid("V3000 formal charge is invalid"))?;
                            if !(-15..=15).contains(&charge) {
                                return Err(invalid("V3000 formal charge is outside -15..=15"));
                            }
                            atom.charge = charge as i8;
                        }
                        "MASS" => {
                            let mass = value
                                .parse::<u16>()
                                .map_err(|_| invalid("V3000 isotope mass is invalid"))?;
                            if !(1..=400).contains(&mass) {
                                return Err(invalid("V3000 isotope mass is outside 1..=400"));
                            }
                            atom.isotope = Some(mass);
                        }
                        "RAD" => ignored_atom_properties = true,
                        "CFG" => ignored_atom_properties = true,
                        _ => ignored_atom_properties = true,
                    }
                }
                if molecule.atoms.len() >= MAX_ATOMS_PER_RECORD {
                    return Err(Error::LimitExceeded(format!(
                        "V3000 MOL/SDF exceeds {MAX_ATOMS_PER_RECORD} atoms"
                    )));
                }
                let atom_slot = molecule.atoms.len();
                molecule.atoms.push(atom);
                atom_ids.insert(id, atom_slot);
            }
            V30Section::Bond => {
                if fields.len() < 4 {
                    return Err(invalid("V3000 bond line is truncated"));
                }
                let id = parse_positive_v30_integer(&fields[0], "bond index")?;
                if !bond_ids.insert(id) {
                    return Err(invalid("V3000 bond index is repeated"));
                }
                let order = fields[1]
                    .parse::<u8>()
                    .map_err(|_| invalid("V3000 bond order is invalid"))?;
                if !(1..=8).contains(&order) {
                    return Err(invalid("V3000 bond order is outside 1..=8"));
                }
                let from_id = parse_positive_v30_integer(&fields[2], "bond atom reference")?;
                let to_id = parse_positive_v30_integer(&fields[3], "bond atom reference")?;
                let from = *atom_ids
                    .get(&from_id)
                    .ok_or_else(|| invalid("V3000 bond references an unknown atom index"))?;
                let to = *atom_ids
                    .get(&to_id)
                    .ok_or_else(|| invalid("V3000 bond references an unknown atom index"))?;
                if from == to {
                    return Err(invalid("V3000 bond connects an atom to itself"));
                }
                if !bond_pairs.insert((from.min(to), from.max(to))) {
                    return Err(invalid("V3000 bond block repeats an atom pair"));
                }
                let mut stereo = 0;
                for property in fields.iter().skip(4) {
                    let Some((key, value)) = property.split_once('=') else {
                        ignored_bond_properties = true;
                        continue;
                    };
                    match key {
                        "CFG" => {
                            stereo = match value.parse::<u8>() {
                                Ok(1) => 1,
                                Ok(2) => 4,
                                Ok(3) => 6,
                                Ok(0) => 0,
                                _ => return Err(invalid("V3000 bond CFG is outside 0..=3")),
                            };
                        }
                        _ => ignored_bond_properties = true,
                    }
                }
                if molecule.bonds.len() >= MAX_BONDS_PER_RECORD {
                    return Err(Error::LimitExceeded(format!(
                        "V3000 MOL/SDF exceeds {MAX_BONDS_PER_RECORD} bonds"
                    )));
                }
                molecule.bonds.push(Bond {
                    from,
                    to,
                    order,
                    stereo,
                });
            }
            V30Section::None => {
                if first == "COUNTS" {
                    if !in_ctab || found_counts || fields.len() < 6 {
                        return Err(invalid(
                            "V3000 counts line is missing, repeated, or truncated",
                        ));
                    }
                    expected_atoms = parse_v30_count(&fields[1], "atom count")?;
                    expected_bonds = parse_v30_count(&fields[2], "bond count")?;
                    let groups = parse_v30_count(&fields[3], "Sgroup count")?;
                    let constraints = parse_v30_count(&fields[4], "3D constraint count")?;
                    let chiral = parse_v30_count(&fields[5], "chiral flag")?;
                    if expected_atoms > MAX_ATOMS_PER_RECORD
                        || expected_bonds > MAX_BONDS_PER_RECORD
                    {
                        return Err(Error::LimitExceeded(format!(
                            "V3000 record exceeds {MAX_ATOMS_PER_RECORD} atoms or {MAX_BONDS_PER_RECORD} bonds"
                        )));
                    }
                    if groups != 0 || constraints != 0 || chiral != 0 {
                        ignored_metadata = true;
                    }
                    found_counts = true;
                } else {
                    ignored_metadata = true;
                }
            }
            V30Section::Ignored(_) => unreachable!(),
        }
    }

    if section != V30Section::None
        || in_ctab
        || !found_ctab_end
        || !found_counts
        || !found_atom_block
    {
        return Err(invalid(
            "V3000 CTAB is missing a required block or terminator",
        ));
    }
    if !molecule.bonds.is_empty() && !found_bond_block {
        return Err(invalid("V3000 bonds are present without a BOND block"));
    }
    if molecule.atoms.len() != expected_atoms || molecule.bonds.len() != expected_bonds {
        return Err(invalid(format!(
            "V3000 counts line declares {expected_atoms} atoms/{expected_bonds} bonds but contains {}/{}",
            molecule.atoms.len(),
            molecule.bonds.len()
        )));
    }
    if has_nonzero_z {
        molecule
            .warnings
            .push("MOL/SDF 3D atom coordinates are projected onto the XY plane".into());
    }
    if atom_maps {
        molecule
            .warnings
            .push("V3000 atom-atom mapping numbers are omitted".into());
    }
    if ignored_atom_properties || ignored_bond_properties {
        molecule.warnings.push(
            "V3000 atom/bond properties beyond displayed charge and isotope data are omitted"
                .into(),
        );
    }
    if ignored_metadata {
        molecule.warnings.push(
            "V3000 Sgroups, collection data, 3D constraints, or other metadata are omitted".into(),
        );
    }
    if lines[after_m_end..]
        .iter()
        .any(|line| line.trim_start().starts_with('>'))
    {
        molecule
            .warnings
            .push("SDF data fields are not displayed or evaluated".into());
    }
    add_common_molecule_warnings(&mut molecule, has_nonzero_z);
    Ok(molecule)
}

fn collect_v30_lines(lines: &[&str]) -> Result<(Vec<String>, usize)> {
    let mut records = Vec::new();
    let mut pending = String::new();
    let mut continuing = false;
    for (index, line) in lines.iter().enumerate() {
        if matches!(line.trim(), "M  END" | "M END") {
            if continuing {
                return Err(invalid("V3000 continuation is unfinished before M END"));
            }
            return Ok((records, index + 1));
        }
        if line.trim().is_empty() {
            continue;
        }
        if line.len() > 80 {
            return Err(Error::LimitExceeded(
                "V3000 physical line exceeds the 80-character format limit".into(),
            ));
        }
        let payload = strip_v30_prefix(line)
            .ok_or_else(|| invalid("V3000 CTAB line does not begin with M V30"))?
            .trim_end();
        if continuing {
            pending.push(' ');
            pending.push_str(payload);
        } else {
            pending.clear();
            pending.push_str(payload);
        }
        continuing = pending.ends_with('-');
        if continuing {
            pending.pop();
        } else {
            if pending.len() > MAX_CTFILE_LINE_BYTES {
                return Err(Error::LimitExceeded(format!(
                    "V3000 logical line exceeds {MAX_CTFILE_LINE_BYTES} bytes"
                )));
            }
            records.push(std::mem::take(&mut pending));
        }
    }
    Err(invalid("V3000 connection table is missing M END"))
}

fn strip_v30_prefix(line: &str) -> Option<&str> {
    line.strip_prefix("M  V30")
        .or_else(|| line.strip_prefix("M V30"))
        .map(str::trim_start)
}

fn split_v30_tokens(line: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut token = String::new();
    let mut quoted = false;
    let mut parentheses = 0usize;
    let mut chars = line.chars().peekable();
    while let Some(character) = chars.next() {
        if character == '"' {
            if quoted && chars.peek() == Some(&'"') {
                token.push('"');
                chars.next();
            } else {
                quoted = !quoted;
            }
        } else if !quoted && character == '(' {
            parentheses += 1;
            token.push(character);
        } else if !quoted && character == ')' {
            parentheses = parentheses.saturating_sub(1);
            token.push(character);
        } else if character.is_whitespace() && !quoted && parentheses == 0 {
            if !token.is_empty() {
                tokens.push(std::mem::take(&mut token));
            }
        } else {
            token.push(character);
        }
    }
    if !token.is_empty() {
        tokens.push(token);
    }
    tokens
}

fn parse_positive_v30_integer(value: &str, label: &str) -> Result<u64> {
    value
        .parse::<u64>()
        .ok()
        .filter(|value| *value > 0)
        .ok_or_else(|| invalid(format!("V3000 {label} must be a positive integer")))
}

fn parse_v30_count(value: &str, label: &str) -> Result<usize> {
    value
        .parse::<usize>()
        .map_err(|_| invalid(format!("V3000 {label} is invalid")))
}

pub(crate) fn render_molecule(
    molecule: &Molecule,
    page_number: usize,
    source_format: &str,
) -> Result<Page> {
    let mut page = Page::new(page_number, 800.0, 600.0, source_format);
    page.title = if molecule.title.is_empty() {
        format!("Molecule {page_number}")
    } else {
        molecule.title.clone()
    };
    page.description = format!(
        "Chemical structure preview with {} atoms and {} bonds",
        molecule.atoms.len(),
        molecule.bonds.len()
    );
    for warning in &molecule.warnings {
        page.warn(warning.clone());
    }
    if molecule.atoms.is_empty() {
        page.warn("MOL/SDF record has no atoms; rendered as an empty structure page");
        return Ok(page);
    }
    let label_count = molecule
        .atoms
        .iter()
        .filter(|atom| !is_implicit_carbon(atom))
        .count();
    let bond_node_count = molecule
        .bonds
        .iter()
        .map(|bond| match bond.order {
            3 => 3,
            2 | 4 => 2,
            _ => 1,
        })
        .sum::<usize>();
    let estimated_nodes = 1usize
        .saturating_add(molecule.atoms.len())
        .saturating_add(label_count)
        .saturating_add(bond_node_count);
    if estimated_nodes > MAX_PAGE_NODES {
        return Err(Error::LimitExceeded(format!(
            "MOL/SDF record expands to {estimated_nodes} SVG nodes; maximum is {MAX_PAGE_NODES}"
        )));
    }
    page.nodes.push(Node::Text {
        id: "molecule-title".into(),
        x: 28.0,
        y: 32.0,
        runs: vec![TextRun {
            text: page.title.clone(),
            font_family: "Arial, sans-serif".into(),
            font_size: 16.0,
            bold: true,
            fill: Paint::solid("#334155"),
            ..TextRun::default()
        }],
        anchor: TextAnchor::Start,
        transform: IDENTITY,
        opacity: 1.0,
        stroke: Stroke::default(),
        clip_id: None,
        meta: SourceMeta {
            semantic_role: "chemical:title".into(),
            ..Default::default()
        },
    });

    let min_x = molecule
        .atoms
        .iter()
        .map(|atom| atom.x)
        .fold(f64::INFINITY, f64::min);
    let max_x = molecule
        .atoms
        .iter()
        .map(|atom| atom.x)
        .fold(f64::NEG_INFINITY, f64::max);
    let min_y = molecule
        .atoms
        .iter()
        .map(|atom| atom.y)
        .fold(f64::INFINITY, f64::min);
    let max_y = molecule
        .atoms
        .iter()
        .map(|atom| atom.y)
        .fold(f64::NEG_INFINITY, f64::max);
    let span_x = (max_x - min_x).max(0.01);
    let span_y = (max_y - min_y).max(0.01);
    let scale = MAX_PREVIEW_SCALE.min(650.0 / span_x).min(440.0 / span_y);
    let center_x = (min_x + max_x) / 2.0;
    let center_y = (min_y + max_y) / 2.0;
    let project = |atom: &Atom| {
        (
            400.0 + (atom.x - center_x) * scale,
            320.0 - (atom.y - center_y) * scale,
        )
    };

    for (bond_index, bond) in molecule.bonds.iter().enumerate() {
        let from = &molecule.atoms[bond.from];
        let to = &molecule.atoms[bond.to];
        let (x1, y1) = project(from);
        let (x2, y2) = project(to);
        let dx = x2 - x1;
        let dy = y2 - y1;
        let length = dx.hypot(dy);
        if length < 1e-5 {
            continue;
        }
        if bond.stereo == 1 {
            let half_width = 6.0;
            let nx = -dy / length * half_width;
            let ny = dx / length * half_width;
            page.nodes.push(Node::Path {
                id: format!("bond-{bond_index}-wedge"),
                d: format!(
                    "M {x1:.2} {y1:.2} L {:.2} {:.2} L {:.2} {:.2} Z",
                    x2 + nx,
                    y2 + ny,
                    x2 - nx,
                    y2 - ny
                ),
                fill_rule: "nonzero".into(),
                fill: Paint::solid("#263238"),
                stroke: Stroke::default(),
                transform: IDENTITY,
                clip_id: None,
                meta: SourceMeta {
                    semantic_role: "chemical:stereo-bond".into(),
                    ..Default::default()
                },
            });
        } else if bond.stereo == 6 {
            push_hashed_wedge(&mut page, bond_index, x1, y1, x2, y2);
        } else if bond.order == 2 || bond.order == 3 || bond.order == 4 {
            let (nx, ny) = (-dy / length, dx / length);
            push_bond_line(
                &mut page,
                bond_index,
                Vec2(x1, y1),
                Vec2(x2, y2),
                0.0,
                Vec2(nx, ny),
                false,
            );
            let offset = 3.0;
            if bond.order == 3 {
                push_bond_line(
                    &mut page,
                    bond_index,
                    Vec2(x1, y1),
                    Vec2(x2, y2),
                    -offset,
                    Vec2(nx, ny),
                    false,
                );
                push_bond_line(
                    &mut page,
                    bond_index,
                    Vec2(x1, y1),
                    Vec2(x2, y2),
                    offset,
                    Vec2(nx, ny),
                    false,
                );
            } else {
                push_bond_line(
                    &mut page,
                    bond_index,
                    Vec2(x1, y1),
                    Vec2(x2, y2),
                    offset,
                    Vec2(nx, ny),
                    bond.order == 4,
                );
            }
        } else {
            let dashed = bond.order >= 5 || bond.stereo == 4 || bond.stereo == 6;
            push_bond_line(
                &mut page,
                bond_index,
                Vec2(x1, y1),
                Vec2(x2, y2),
                0.0,
                Vec2(0.0, 0.0),
                dashed,
            );
        }
        if page.nodes.len() > MAX_PAGE_NODES {
            return Err(Error::LimitExceeded(format!(
                "MOL/SDF rendered page exceeds {MAX_PAGE_NODES} nodes"
            )));
        }
    }

    for (index, atom) in molecule.atoms.iter().enumerate() {
        let (x, y) = project(atom);
        if !is_implicit_carbon(atom) {
            let mut label = atom
                .isotope
                .map(|mass| format!("{mass}{}", atom.element))
                .unwrap_or_else(|| atom.element.clone());
            if atom.charge != 0 {
                let sign = if atom.charge > 0 { '+' } else { '-' };
                let magnitude = atom.charge.unsigned_abs();
                if magnitude > 1 {
                    label.push(char::from(b'0' + magnitude));
                }
                label.push(sign);
            }
            let label_width = (label.chars().count() as f64 * 9.5).max(15.0);
            page.nodes.push(Node::Path {
                id: format!("atom-background-{}", index + 1),
                d: format!(
                    "M {:.2} {:.2} h {:.2} v 22 h {:.2} Z",
                    x - label_width / 2.0 - 3.0,
                    y - 13.0,
                    label_width + 6.0,
                    -(label_width + 6.0)
                ),
                fill_rule: "nonzero".into(),
                fill: Paint::solid("#ffffff"),
                stroke: Stroke::default(),
                transform: IDENTITY,
                clip_id: None,
                meta: SourceMeta::default(),
            });
            let fill = element_color(&atom.element);
            page.nodes.push(Node::Text {
                id: format!("atom-label-{}", index + 1),
                x,
                y: y + 5.0,
                runs: vec![TextRun {
                    text: label,
                    font_family: "Arial, sans-serif".into(),
                    font_size: 16.0,
                    bold: true,
                    fill: Paint::solid(fill),
                    ..TextRun::default()
                }],
                anchor: TextAnchor::Middle,
                transform: IDENTITY,
                opacity: 1.0,
                stroke: Stroke::default(),
                clip_id: None,
                meta: SourceMeta {
                    semantic_role: "chemical:atom".into(),
                    alt_text: atom.element.clone(),
                    ..Default::default()
                },
            });
        }
        if page.nodes.len() > MAX_PAGE_NODES {
            return Err(Error::LimitExceeded(format!(
                "MOL/SDF rendered page exceeds {MAX_PAGE_NODES} nodes"
            )));
        }
    }
    Ok(page)
}

#[derive(Clone, Copy)]
struct Vec2(f64, f64);

fn push_bond_line(
    page: &mut Page,
    index: usize,
    from: Vec2,
    to: Vec2,
    offset: f64,
    normal: Vec2,
    dashed: bool,
) {
    push_bond_segment(
        page,
        format!("bond-{index}-{offset}"),
        Vec2(from.0 + normal.0 * offset, from.1 + normal.1 * offset),
        Vec2(to.0 + normal.0 * offset, to.1 + normal.1 * offset),
        dashed,
        "chemical:bond",
    );
}

fn push_hashed_wedge(page: &mut Page, index: usize, x1: f64, y1: f64, x2: f64, y2: f64) {
    let dx = x2 - x1;
    let dy = y2 - y1;
    let length = dx.hypot(dy);
    if length < 1e-5 {
        return;
    }
    let normal = Vec2(-dy / length, dx / length);
    for tick in 1..=6 {
        let progress = tick as f64 / 7.0;
        let half_width = 0.7 + progress * 4.3;
        let center = Vec2(x1 + dx * progress, y1 + dy * progress);
        push_bond_segment(
            page,
            format!("bond-{index}-hash-{tick}"),
            Vec2(
                center.0 - normal.0 * half_width,
                center.1 - normal.1 * half_width,
            ),
            Vec2(
                center.0 + normal.0 * half_width,
                center.1 + normal.1 * half_width,
            ),
            false,
            "chemical:stereo-bond",
        );
    }
}

fn push_bond_segment(
    page: &mut Page,
    id: String,
    from: Vec2,
    to: Vec2,
    dashed: bool,
    semantic_role: &str,
) {
    page.nodes.push(Node::Path {
        id,
        d: format!("M {:.2} {:.2} L {:.2} {:.2}", from.0, from.1, to.0, to.1),
        fill_rule: String::new(),
        fill: Paint::None,
        stroke: Stroke {
            paint: Paint::solid("#263238"),
            width: 2.2,
            line_cap: crate::ir::LineCap::Round,
            dash_array: if dashed { vec![4.0, 3.0] } else { Vec::new() },
            ..Default::default()
        },
        transform: IDENTITY,
        clip_id: None,
        meta: SourceMeta {
            semantic_role: semantic_role.into(),
            ..Default::default()
        },
    });
}

fn parse_counts_line(line: &str) -> Result<(usize, usize)> {
    let atoms = line
        .get(0..3)
        .ok_or_else(|| invalid("counts line is shorter than its atom field"))?
        .trim()
        .parse::<usize>()
        .map_err(|_| invalid("counts line has an invalid atom count"))?;
    let bonds = line
        .get(3..6)
        .ok_or_else(|| invalid("counts line is shorter than its bond field"))?
        .trim()
        .parse::<usize>()
        .map_err(|_| invalid("counts line has an invalid bond count"))?;
    Ok((atoms, bonds))
}

fn parse_coordinate(value: &str) -> Result<f64> {
    let coordinate = value
        .parse::<f64>()
        .map_err(|_| invalid("atom coordinate is not a number"))?;
    if !coordinate.is_finite() || coordinate.abs() > MAX_COORDINATE {
        return Err(invalid(
            "atom coordinate is non-finite or outside ±1,000,000",
        ));
    }
    Ok(coordinate)
}

fn parse_index(value: &str, atom_count: usize, label: &str) -> Result<usize> {
    let index = value
        .parse::<usize>()
        .map_err(|_| invalid(format!("{label} is not an integer")))?;
    if index == 0 || index > atom_count {
        return Err(invalid(format!("{label} is outside 1..={atom_count}")));
    }
    Ok(index - 1)
}

fn normalize_element(value: &str) -> Result<String> {
    if value.is_empty()
        || value.len() > 3
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'*' || byte == b'#')
    {
        return Err(invalid("atom symbol is empty or malformed"));
    }
    if matches!(value, "L" | "A" | "Q" | "*" | "LP" | "R#" | "D" | "T") || value.starts_with('R') {
        return Ok(value.to_string());
    }
    let mut characters = value.chars();
    let first = characters.next().unwrap().to_ascii_uppercase();
    let remainder = characters.as_str().to_ascii_lowercase();
    let element = format!("{first}{remainder}");
    if atomic_number(&element).is_none() {
        return Err(invalid(format!("unknown atom symbol '{value}'")));
    }
    Ok(element)
}

fn atomic_number(element: &str) -> Option<u8> {
    Some(match element {
        "H" | "D" | "T" => 1,
        "He" => 2,
        "Li" => 3,
        "Be" => 4,
        "B" => 5,
        "C" => 6,
        "N" => 7,
        "O" => 8,
        "F" => 9,
        "Ne" => 10,
        "Na" => 11,
        "Mg" => 12,
        "Al" => 13,
        "Si" => 14,
        "P" => 15,
        "S" => 16,
        "Cl" => 17,
        "Ar" => 18,
        "K" => 19,
        "Ca" => 20,
        "Sc" => 21,
        "Ti" => 22,
        "V" => 23,
        "Cr" => 24,
        "Mn" => 25,
        "Fe" => 26,
        "Co" => 27,
        "Ni" => 28,
        "Cu" => 29,
        "Zn" => 30,
        "Ga" => 31,
        "Ge" => 32,
        "As" => 33,
        "Se" => 34,
        "Br" => 35,
        "Kr" => 36,
        "Rb" => 37,
        "Sr" => 38,
        "Y" => 39,
        "Zr" => 40,
        "Nb" => 41,
        "Mo" => 42,
        "Tc" => 43,
        "Ru" => 44,
        "Rh" => 45,
        "Pd" => 46,
        "Ag" => 47,
        "Cd" => 48,
        "In" => 49,
        "Sn" => 50,
        "Sb" => 51,
        "Te" => 52,
        "I" => 53,
        "Xe" => 54,
        "Cs" => 55,
        "Ba" => 56,
        "La" => 57,
        "Ce" => 58,
        "Pr" => 59,
        "Nd" => 60,
        "Pm" => 61,
        "Sm" => 62,
        "Eu" => 63,
        "Gd" => 64,
        "Tb" => 65,
        "Dy" => 66,
        "Ho" => 67,
        "Er" => 68,
        "Tm" => 69,
        "Yb" => 70,
        "Lu" => 71,
        "Hf" => 72,
        "Ta" => 73,
        "W" => 74,
        "Re" => 75,
        "Os" => 76,
        "Ir" => 77,
        "Pt" => 78,
        "Au" => 79,
        "Hg" => 80,
        "Tl" => 81,
        "Pb" => 82,
        "Bi" => 83,
        "Po" => 84,
        "At" => 85,
        "Rn" => 86,
        "Fr" => 87,
        "Ra" => 88,
        "Ac" => 89,
        "Th" => 90,
        "Pa" => 91,
        "U" => 92,
        "Np" => 93,
        "Pu" => 94,
        "Am" => 95,
        "Cm" => 96,
        "Bk" => 97,
        "Cf" => 98,
        "Es" => 99,
        "Fm" => 100,
        "Md" => 101,
        "No" => 102,
        "Lr" => 103,
        "Rf" => 104,
        "Db" => 105,
        "Sg" => 106,
        "Bh" => 107,
        "Hs" => 108,
        "Mt" => 109,
        "Ds" => 110,
        "Rg" => 111,
        "Cn" => 112,
        "Nh" => 113,
        "Fl" => 114,
        "Mc" => 115,
        "Lv" => 116,
        "Ts" => 117,
        "Og" => 118,
        _ => return None,
    })
}

fn charge_from_code(code: u8) -> Result<i8> {
    Ok(match code {
        0 => 0,
        1 => 3,
        2 => 2,
        3 => 1,
        4 => 0,
        5 => -1,
        6 => -2,
        7 => -3,
        _ => return Err(invalid("atom charge code is outside the V2000 range 0..=7")),
    })
}

fn apply_atom_properties(line: &str, atoms: &mut [Atom], isotope: bool) -> Result<()> {
    let fields = line.split_whitespace().collect::<Vec<_>>();
    if fields.len() < 3 {
        return Err(invalid("atom property line is truncated"));
    }
    let count = fields[2]
        .parse::<usize>()
        .map_err(|_| invalid("atom property count is invalid"))?;
    if count > 8 || fields.len() != 3 + count.saturating_mul(2) {
        return Err(invalid("atom property count does not match its entries"));
    }
    for pair in fields[3..].chunks_exact(2) {
        let atom_index = parse_index(pair[0], atoms.len(), "atom property reference")?;
        let value = pair[1]
            .parse::<i16>()
            .map_err(|_| invalid("atom property value is invalid"))?;
        if isotope {
            if !(1..=400).contains(&value) {
                return Err(invalid("isotope mass is outside 1..=400"));
            }
            atoms[atom_index].isotope = Some(value as u16);
        } else {
            if !(-15..=15).contains(&value) {
                return Err(invalid("formal charge is outside -15..=15"));
            }
            atoms[atom_index].charge = value as i8;
        }
    }
    Ok(())
}

fn element_color(element: &str) -> &'static str {
    match element {
        "N" => "#2455c3",
        "O" => "#d62728",
        "F" | "Cl" => "#198c38",
        "Br" => "#8b4513",
        "I" => "#6f42c1",
        "S" => "#b8860b",
        "P" => "#e67e22",
        "L" | "A" | "Q" | "*" | "R#" | "LP" => "#475569",
        _ => "#202124",
    }
}

fn is_implicit_carbon(atom: &Atom) -> bool {
    atom.element == "C" && atom.charge == 0 && atom.isotope.is_none()
}

fn invalid(message: impl Into<String>) -> Error {
    Error::InvalidInput(format!("invalid MOL/SDF: {}", message.into()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn down_stereo_bonds_render_as_bounded_hash_marks() {
        let molecule = Molecule {
            title: "Stereo test".into(),
            atoms: vec![
                Atom {
                    x: 0.0,
                    y: 0.0,
                    element: "C".into(),
                    charge: 0,
                    isotope: None,
                },
                Atom {
                    x: 1.5,
                    y: 0.0,
                    element: "O".into(),
                    charge: 0,
                    isotope: None,
                },
            ],
            bonds: vec![Bond {
                from: 0,
                to: 1,
                order: 1,
                stereo: 6,
            }],
            warnings: Vec::new(),
        };
        let page = render_molecule(&molecule, 1, "mol").unwrap();
        let hash_marks = page
            .nodes
            .iter()
            .filter(|node| matches!(node, Node::Path { id, meta, .. } if id.contains("-hash-") && meta.semantic_role == "chemical:stereo-bond"))
            .count();
        assert_eq!(hash_marks, 6);
    }
}
