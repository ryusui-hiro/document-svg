//! Bounded Tripos MOL2 chemical structure preview.
//!
//! Parses MOLECULE/ATOM/BOND sections and reuses the bounded molecule renderer.
//! Charges, substructures, force-field metadata, and unsupported molecule
//! annotations remain inert.

use std::collections::HashMap;
use std::path::Path;

use crate::convert::{ConvertOptions, PageConsumer, read_limited_file};
use crate::document::molfile::{Atom, Bond, Molecule, render_molecule};
use crate::error::{Error, Result};

const MAX_MOL2_INPUT_BYTES: u64 = 64 * 1024 * 1024;
const MAX_MOL2_LINES: usize = 1_000_000;
const MAX_MOL2_LINE_BYTES: usize = 1024 * 1024;
const MAX_MOL2_MOLECULES: usize = 10_000;
const MAX_MOL2_ATOMS: usize = 100_000;
const MAX_MOL2_BONDS: usize = 200_000;
const MAX_MOL2_TOTAL_ATOMS: usize = 500_000;

pub(crate) fn looks_like_prefix(bytes: &[u8]) -> bool {
    String::from_utf8_lossy(bytes)
        .lines()
        .take(32)
        .any(|line| line.trim().eq_ignore_ascii_case("@<TRIPOS>MOLECULE"))
}

pub(crate) fn convert(
    path: &Path,
    options: &ConvertOptions,
    sink: &mut dyn PageConsumer,
) -> Result<Vec<String>> {
    let bytes = read_limited_file(
        path,
        options.max_input_bytes.min(MAX_MOL2_INPUT_BYTES),
        "MOL2 input",
    )?;
    let text = std::str::from_utf8(&bytes).map_err(|error| {
        Error::InvalidInput(format!("MOL2 input is not UTF-8 or ASCII: {error}"))
    })?;
    let (molecules, mut warnings) = parse_molecules(text)?;
    if molecules.is_empty() {
        return Err(Error::InvalidInput(
            "MOL2 contains no MOLECULE sections".into(),
        ));
    }
    if molecules.len() > options.max_pages {
        return Err(Error::LimitExceeded(format!(
            "MOL2 contains {} molecules but max_pages is {}",
            molecules.len(),
            options.max_pages
        )));
    }
    for (index, molecule) in molecules.iter().enumerate() {
        let page = render_molecule(molecule, index + 1, "mol2")?;
        warnings.extend(page.warnings.iter().cloned());
        sink.consume(page)?;
    }
    warnings.sort();
    warnings.dedup();
    Ok(warnings)
}

fn parse_molecules(text: &str) -> Result<(Vec<Molecule>, Vec<String>)> {
    let lines = text.lines().collect::<Vec<_>>();
    let mut molecules = Vec::new();
    let mut warnings = Vec::new();
    let mut index = 0usize;
    let mut total_atoms = 0usize;
    while index < lines.len() {
        if index >= MAX_MOL2_LINES {
            return Err(Error::LimitExceeded(format!(
                "MOL2 exceeds {MAX_MOL2_LINES} lines"
            )));
        }
        if lines[index].len() > MAX_MOL2_LINE_BYTES {
            return Err(Error::LimitExceeded(format!(
                "MOL2 line exceeds {MAX_MOL2_LINE_BYTES} bytes"
            )));
        }
        if !lines[index]
            .trim()
            .eq_ignore_ascii_case("@<TRIPOS>MOLECULE")
        {
            index += 1;
            continue;
        }
        index += 1;
        let title = lines
            .get(index)
            .map(|line| line.trim().to_owned())
            .unwrap_or_default();
        index += 1;
        let counts = lines
            .get(index)
            .unwrap_or(&"")
            .split_whitespace()
            .collect::<Vec<_>>();
        let atom_count = counts
            .first()
            .and_then(|value| value.parse::<usize>().ok())
            .unwrap_or(0);
        let bond_count = counts
            .get(1)
            .and_then(|value| value.parse::<usize>().ok())
            .unwrap_or(0);
        if atom_count > MAX_MOL2_ATOMS || bond_count > MAX_MOL2_BONDS {
            return Err(Error::LimitExceeded(format!(
                "MOL2 molecule exceeds {MAX_MOL2_ATOMS} atoms or {MAX_MOL2_BONDS} bonds"
            )));
        }
        index += 1;
        while index < lines.len() && !lines[index].trim().eq_ignore_ascii_case("@<TRIPOS>ATOM") {
            index += 1;
        }
        if index >= lines.len() {
            return Err(Error::InvalidInput(
                "MOL2 molecule has no ATOM section".into(),
            ));
        }
        index += 1;
        let mut molecule = Molecule {
            title,
            atoms: Vec::with_capacity(atom_count),
            bonds: Vec::with_capacity(bond_count),
            warnings: Vec::new(),
        };
        let mut id_map = HashMap::new();
        while index < lines.len() && !lines[index].trim_start().starts_with("@<TRIPOS>") {
            let fields = lines[index].split_whitespace().collect::<Vec<_>>();
            if fields.len() >= 6 {
                let id = fields[0]
                    .parse::<i64>()
                    .map_err(|_| Error::InvalidInput("MOL2 atom id is invalid".into()))?;
                let x = fields[2]
                    .parse::<f64>()
                    .map_err(|_| Error::InvalidInput("MOL2 atom x is invalid".into()))?;
                let y = fields[3]
                    .parse::<f64>()
                    .map_err(|_| Error::InvalidInput("MOL2 atom y is invalid".into()))?;
                let element = clean_element(fields[5]);
                id_map.insert(id, molecule.atoms.len());
                molecule.atoms.push(Atom {
                    x,
                    y,
                    element,
                    charge: 0,
                    isotope: None,
                });
            }
            index += 1;
        }
        total_atoms = total_atoms.saturating_add(molecule.atoms.len());
        if total_atoms > MAX_MOL2_TOTAL_ATOMS {
            return Err(Error::LimitExceeded(format!(
                "MOL2 exceeds {MAX_MOL2_TOTAL_ATOMS} total atoms"
            )));
        }
        while index < lines.len() && !lines[index].trim().eq_ignore_ascii_case("@<TRIPOS>BOND") {
            index += 1;
        }
        if index < lines.len() {
            index += 1;
            while index < lines.len() && !lines[index].trim_start().starts_with("@<TRIPOS>") {
                let fields = lines[index].split_whitespace().collect::<Vec<_>>();
                if fields.len() >= 4 {
                    let from = fields[1]
                        .parse::<i64>()
                        .ok()
                        .and_then(|id| id_map.get(&id).copied());
                    let to = fields[2]
                        .parse::<i64>()
                        .ok()
                        .and_then(|id| id_map.get(&id).copied());
                    if let (Some(from), Some(to)) = (from, to)
                        && from != to
                    {
                        molecule.bonds.push(Bond {
                            from,
                            to,
                            order: bond_order(fields[3]),
                            stereo: 0,
                        });
                    }
                }
                index += 1;
            }
        }
        if molecule.atoms.is_empty() {
            warnings.push("MOL2 molecule without valid atoms was omitted".into());
        } else {
            if molecules.len() >= MAX_MOL2_MOLECULES {
                return Err(Error::LimitExceeded(format!(
                    "MOL2 exceeds {MAX_MOL2_MOLECULES} molecules"
                )));
            }
            molecules.push(molecule);
        }
    }
    Ok((molecules, warnings))
}

fn bond_order(value: &str) -> u8 {
    match value.to_ascii_lowercase().as_str() {
        "1" | "un" | "du" | "am" => 1,
        "2" => 2,
        "3" => 3,
        "ar" => 4,
        _ => 1,
    }
}

fn clean_element(value: &str) -> String {
    let value = value.split('.').next().unwrap_or(value);
    let mut chars = value
        .chars()
        .filter(|character| character.is_ascii_alphabetic());
    let Some(first) = chars.next() else {
        return "X".into();
    };
    let mut result = first.to_ascii_uppercase().to_string();
    if let Some(second) = chars.next() {
        result.push(second.to_ascii_lowercase());
    }
    result
}
