//! Bounded Protein Data Bank coordinate previews.
//!
//! This reader consumes fixed-column ATOM/HETATM coordinates and optional
//! CONECT records from legacy PDB text files. Each MODEL becomes an SVG page;
//! coordinates are projected onto XY and chemical interpretation is deliberately
//! limited to the element labels supplied by the file.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use crate::convert::{ConvertOptions, PageConsumer, read_limited_file};
use crate::document::molfile::{Atom, Bond, Molecule, render_molecule};
use crate::error::{Error, Result};

const MAX_PDB_INPUT_BYTES: u64 = 64 * 1024 * 1024;
const MAX_PDB_LINES: usize = 1_000_000;
const MAX_PDB_LINE_BYTES: usize = 1024 * 1024;
const MAX_PDB_MODELS: usize = 1_000;
const MAX_PDB_ATOMS_PER_MODEL: usize = 100_000;
const MAX_PDB_TOTAL_ATOMS: usize = 500_000;
const MAX_PDB_BONDS_PER_MODEL: usize = 200_000;
const MAX_PDB_COORDINATE: f64 = 1_000_000.0;

struct ModelBuilder {
    number: usize,
    title: String,
    atoms: Vec<Atom>,
    serials: HashMap<String, usize>,
    conect: Vec<(String, String)>,
    warnings: Vec<String>,
    has_nonzero_z: bool,
    skipped_altlocs: usize,
}

impl ModelBuilder {
    fn new(number: usize, title: String) -> Self {
        Self {
            number,
            title,
            atoms: Vec::new(),
            serials: HashMap::new(),
            conect: Vec::new(),
            warnings: Vec::new(),
            has_nonzero_z: false,
            skipped_altlocs: 0,
        }
    }

    fn finish(self) -> Result<Molecule> {
        let mut bonds = Vec::new();
        let mut seen = HashSet::new();
        for (from_serial, to_serial) in self.conect {
            let Some(&from) = self.serials.get(&from_serial) else {
                continue;
            };
            let Some(&to) = self.serials.get(&to_serial) else {
                continue;
            };
            if from == to {
                continue;
            }
            let key = (from.min(to), from.max(to));
            if seen.insert(key) {
                if bonds.len() >= MAX_PDB_BONDS_PER_MODEL {
                    return Err(Error::LimitExceeded(format!(
                        "PDB model exceeds {MAX_PDB_BONDS_PER_MODEL} CONECT bonds"
                    )));
                }
                bonds.push(Bond {
                    from,
                    to,
                    order: 1,
                    stereo: 0,
                });
            }
        }
        let mut warnings = self.warnings;
        if self.has_nonzero_z {
            warnings.push("PDB 3D coordinates are projected onto the XY plane".into());
        }
        if self.skipped_altlocs > 0 {
            warnings.push(format!(
                "PDB alternate atom locations other than blank/A were omitted ({})",
                self.skipped_altlocs
            ));
        }
        if !self.atoms.is_empty() && bonds.is_empty() {
            warnings.push("PDB CONECT records were absent; atoms are shown without bonds".into());
        }
        Ok(Molecule {
            title: if self.title.is_empty() {
                format!("PDB model {}", self.number)
            } else if self.number > 1 {
                format!("{} (model {})", self.title, self.number)
            } else {
                self.title
            },
            atoms: self.atoms,
            bonds,
            warnings,
        })
    }
}

pub(crate) fn looks_like_prefix(bytes: &[u8]) -> bool {
    let text = String::from_utf8_lossy(bytes);
    text.lines().take(32).any(|line| {
        let record = line.get(..6).unwrap_or(line).trim();
        matches!(record, "ATOM" | "HETATM" | "MODEL" | "HEADER")
    })
}

pub(crate) fn convert(
    path: &Path,
    options: &ConvertOptions,
    sink: &mut dyn PageConsumer,
) -> Result<Vec<String>> {
    let bytes = read_limited_file(
        path,
        options.max_input_bytes.min(MAX_PDB_INPUT_BYTES),
        "PDB input",
    )?;
    let text = std::str::from_utf8(&bytes).map_err(|error| {
        Error::InvalidInput(format!("PDB input is not UTF-8 or ASCII: {error}"))
    })?;
    let models = parse_models(text)?;
    if models.is_empty() {
        return Err(Error::InvalidInput(
            "PDB input contains no ATOM or HETATM records".into(),
        ));
    }
    if options.max_pages == 0 {
        return Err(Error::LimitExceeded(
            "PDB conversion requires at least one page; max_pages is zero".into(),
        ));
    }
    if models.len() > options.max_pages {
        return Err(Error::LimitExceeded(format!(
            "PDB contains {} models but max_pages is {}",
            models.len(),
            options.max_pages
        )));
    }
    let mut warnings = Vec::new();
    for (index, model) in models.iter().enumerate() {
        let page = render_molecule(model, index + 1, "pdb")?;
        warnings.extend(page.warnings.iter().cloned());
        sink.consume(page)?;
    }
    warnings.sort();
    warnings.dedup();
    Ok(warnings)
}

fn parse_models(text: &str) -> Result<Vec<Molecule>> {
    let mut models = Vec::new();
    let mut current = None::<ModelBuilder>;
    let mut next_model = 1usize;
    let mut total_atoms = 0usize;
    let mut line_count = 0usize;
    let mut global_title = String::new();
    for line in text.lines() {
        line_count = line_count.saturating_add(1);
        if line_count > MAX_PDB_LINES {
            return Err(Error::LimitExceeded(format!(
                "PDB input exceeds {MAX_PDB_LINES} lines"
            )));
        }
        if line.len() > MAX_PDB_LINE_BYTES {
            return Err(Error::LimitExceeded(format!(
                "PDB line exceeds {MAX_PDB_LINE_BYTES} bytes"
            )));
        }
        let record = line.get(..6).unwrap_or(line).trim();
        match record {
            "HEADER" => {
                if global_title.is_empty() {
                    global_title = clean_field(
                        line.get(10..50)
                            .or_else(|| line.get(10..))
                            .unwrap_or_default(),
                    );
                }
            }
            "TITLE" => {
                let part = clean_field(
                    line.get(10..80)
                        .or_else(|| line.get(10..))
                        .unwrap_or_default(),
                );
                if !part.is_empty() {
                    if !global_title.is_empty() {
                        global_title.push(' ');
                    }
                    global_title.push_str(&part);
                }
            }
            "MODEL" => {
                if let Some(builder) = current.take() {
                    models.push(builder.finish()?);
                }
                if models.len() >= MAX_PDB_MODELS {
                    return Err(Error::LimitExceeded(format!(
                        "PDB exceeds {MAX_PDB_MODELS} models"
                    )));
                }
                let number = parse_optional_usize(line.get(10..14).unwrap_or_default())
                    .unwrap_or(next_model);
                next_model = number.saturating_add(1);
                current = Some(ModelBuilder::new(number, global_title.clone()));
            }
            "ENDMDL" => {
                if let Some(builder) = current.take() {
                    models.push(builder.finish()?);
                }
            }
            "ATOM" | "HETATM" => {
                if current.is_none() {
                    current = Some(ModelBuilder::new(next_model, global_title.clone()));
                    next_model = next_model.saturating_add(1);
                }
                let builder = current.as_mut().expect("PDB model initialized");
                if builder.atoms.len() >= MAX_PDB_ATOMS_PER_MODEL {
                    return Err(Error::LimitExceeded(format!(
                        "PDB model exceeds {MAX_PDB_ATOMS_PER_MODEL} atoms"
                    )));
                }
                let alt = line.as_bytes().get(16).copied().unwrap_or(b' ');
                if alt != b' ' && alt != b'A' {
                    builder.skipped_altlocs = builder.skipped_altlocs.saturating_add(1);
                    continue;
                }
                let serial = fixed_field(line, 6, 11, "atom serial")?;
                let x = parse_coordinate(fixed_field(line, 30, 38, "x coordinate")?, "x")?;
                let y = parse_coordinate(fixed_field(line, 38, 46, "y coordinate")?, "y")?;
                let z = parse_coordinate(fixed_field(line, 46, 54, "z coordinate")?, "z")?;
                let atom_name = line.get(12..16).unwrap_or_default();
                let element = element_name(line.get(76..78).unwrap_or_default(), atom_name);
                let index = builder.atoms.len();
                builder.serials.entry(serial).or_insert(index);
                builder.has_nonzero_z |= z.abs() > 1e-6;
                builder.atoms.push(Atom {
                    x,
                    y,
                    element,
                    charge: 0,
                    isotope: None,
                });
                total_atoms = total_atoms.saturating_add(1);
                if total_atoms > MAX_PDB_TOTAL_ATOMS {
                    return Err(Error::LimitExceeded(format!(
                        "PDB exceeds {MAX_PDB_TOTAL_ATOMS} total atoms"
                    )));
                }
            }
            "CONECT" => {
                if let Some(builder) = current.as_mut() {
                    let source = fixed_field(line, 6, 11, "CONECT source")?;
                    for start in [11usize, 16, 21, 26, 31] {
                        if start >= line.len() {
                            break;
                        }
                        let target = line
                            .get(start..start.saturating_add(5))
                            .unwrap_or_default()
                            .trim();
                        if !target.is_empty() {
                            builder.conect.push((source.clone(), target.to_owned()));
                        }
                    }
                }
            }
            "END" => break,
            _ => {}
        }
    }
    if let Some(builder) = current.take() {
        models.push(builder.finish()?);
    }
    if models.len() > MAX_PDB_MODELS {
        return Err(Error::LimitExceeded(format!(
            "PDB exceeds {MAX_PDB_MODELS} models"
        )));
    }
    Ok(models)
}

fn fixed_field(line: &str, start: usize, end: usize, label: &str) -> Result<String> {
    let value = line
        .get(start..end.min(line.len()))
        .ok_or_else(|| Error::InvalidInput(format!("PDB {label} field is missing")))?
        .trim();
    if value.is_empty() {
        return Err(Error::InvalidInput(format!("PDB {label} field is empty")));
    }
    Ok(value.to_owned())
}

fn parse_coordinate(value: String, label: &str) -> Result<f64> {
    let parsed = value
        .parse::<f64>()
        .map_err(|_| Error::InvalidInput(format!("PDB {label} coordinate is invalid")))?;
    if !parsed.is_finite() || parsed.abs() > MAX_PDB_COORDINATE {
        return Err(Error::InvalidInput(format!(
            "PDB {label} coordinate exceeds ±{MAX_PDB_COORDINATE}"
        )));
    }
    Ok(parsed)
}

fn element_name(field: &str, atom_name: &str) -> String {
    let candidate = if field.trim().is_empty() {
        atom_name
            .chars()
            .filter(|character| character.is_ascii_alphabetic())
            .take(2)
            .collect::<String>()
    } else {
        field
            .trim()
            .chars()
            .filter(|c| c.is_ascii_alphabetic())
            .take(2)
            .collect()
    };
    let mut chars = candidate.chars();
    let Some(first) = chars.next() else {
        return "X".into();
    };
    let mut output = first.to_ascii_uppercase().to_string();
    if let Some(second) = chars.next() {
        output.push(second.to_ascii_lowercase());
    }
    output
}

fn clean_field(value: &str) -> String {
    value
        .trim()
        .chars()
        .filter(|character| !character.is_control())
        .take(240)
        .collect()
}

fn parse_optional_usize(value: &str) -> Option<usize> {
    value.trim().parse::<usize>().ok()
}

#[cfg(test)]
mod tests {
    use super::{looks_like_prefix, parse_models};

    #[test]
    fn recognizes_pdb_records_without_broad_text_sniffing() {
        assert!(looks_like_prefix(
            b"HEADER    SAMPLE\nATOM      1  CA  ALA A   1       0.000   0.000   0.000\n"
        ));
        assert!(!looks_like_prefix(b"This prose mentions ATOM as a word.\n"));
    }

    #[test]
    fn parses_fixed_columns_and_model_title() {
        let text = "HEADER    SAMPLE PDB PREVIEW\nTITLE     Two model test\nMODEL        1\nATOM      1  CA  ALA A   1       0.000   0.000   0.000  1.00 10.00           C  \nENDMDL\n";
        let models = parse_models(text).unwrap();
        assert_eq!(models.len(), 1);
        assert!(
            models[0].title.contains("SAMPLE PDB PREVIEW"),
            "title={:?}",
            models[0].title
        );
        assert_eq!(models[0].atoms.len(), 1);
    }
}
