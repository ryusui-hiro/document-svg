//! Bounded mmCIF/PDBx atom-coordinate preview.
//!
//! Extracts the atom_site loop from text CIF files and renders each model with
//! the existing molecule renderer. Chemistry perception, symmetry, and other
//! crystallographic metadata remain inert.

use std::collections::BTreeMap;
use std::path::Path;

use crate::convert::{ConvertOptions, PageConsumer, read_limited_file};
use crate::document::molfile::{Atom, Molecule, render_molecule};
use crate::error::{Error, Result};

const MAX_CIF_INPUT_BYTES: u64 = 64 * 1024 * 1024;
const MAX_CIF_LINES: usize = 1_000_000;
const MAX_CIF_LINE_BYTES: usize = 1024 * 1024;
const MAX_CIF_TOKENS: usize = 5_000_000;
const MAX_CIF_MODELS: usize = 1_000;
const MAX_CIF_ATOMS_PER_MODEL: usize = 100_000;
const MAX_CIF_TOTAL_ATOMS: usize = 500_000;
const MAX_CIF_COORDINATE: f64 = 1_000_000.0;

pub(crate) fn looks_like_prefix(bytes: &[u8]) -> bool {
    String::from_utf8_lossy(bytes).lines().take(64).any(|line| {
        line.trim_start()
            .to_ascii_lowercase()
            .starts_with("_atom_site.")
    })
}

pub(crate) fn convert(
    path: &Path,
    options: &ConvertOptions,
    sink: &mut dyn PageConsumer,
) -> Result<Vec<String>> {
    let bytes = read_limited_file(
        path,
        options.max_input_bytes.min(MAX_CIF_INPUT_BYTES),
        "CIF input",
    )?;
    let text = std::str::from_utf8(&bytes).map_err(|error| {
        Error::InvalidInput(format!("CIF input is not UTF-8 or ASCII: {error}"))
    })?;
    let (models, mut warnings) = parse_cif(text)?;
    if models.is_empty() {
        return Err(Error::InvalidInput(
            "CIF contains no atom_site coordinates".into(),
        ));
    }
    if models.len() > options.max_pages {
        return Err(Error::LimitExceeded(format!(
            "CIF contains {} models but max_pages is {}",
            models.len(),
            options.max_pages
        )));
    }
    for (index, model) in models.values().enumerate() {
        let page = render_molecule(model, index + 1, "cif")?;
        warnings.extend(page.warnings.iter().cloned());
        sink.consume(page)?;
    }
    warnings.sort();
    warnings.dedup();
    Ok(warnings)
}

fn parse_cif(text: &str) -> Result<(BTreeMap<usize, Molecule>, Vec<String>)> {
    let lines = text.lines().collect::<Vec<_>>();
    let mut models = BTreeMap::<usize, Molecule>::new();
    let mut warnings = Vec::new();
    let mut line_index = 0usize;
    let mut token_count = 0usize;
    let mut total_atoms = 0usize;
    let mut title = String::new();
    while line_index < lines.len() {
        if line_index >= MAX_CIF_LINES {
            return Err(Error::LimitExceeded(format!(
                "CIF exceeds {MAX_CIF_LINES} lines"
            )));
        }
        let trimmed = lines[line_index].trim();
        if lines[line_index].len() > MAX_CIF_LINE_BYTES {
            return Err(Error::LimitExceeded(format!(
                "CIF line exceeds {MAX_CIF_LINE_BYTES} bytes"
            )));
        }
        if trimmed.to_ascii_lowercase().starts_with("data_") && title.is_empty() {
            title = trimmed[5..].trim().chars().take(160).collect();
        }
        if trimmed.eq_ignore_ascii_case("loop_") {
            line_index += 1;
            let mut columns = Vec::new();
            while line_index < lines.len() && lines[line_index].trim_start().starts_with('_') {
                columns.push(
                    lines[line_index]
                        .split_whitespace()
                        .next()
                        .unwrap_or_default()
                        .to_owned(),
                );
                line_index += 1;
            }
            if columns.is_empty() {
                continue;
            }
            let atom_loop = columns
                .iter()
                .any(|column| column.to_ascii_lowercase().starts_with("_atom_site."));
            let mut row_tokens = Vec::<String>::new();
            while line_index < lines.len() {
                let candidate = lines[line_index].trim();
                if is_cif_control(candidate) && row_tokens.is_empty() {
                    break;
                }
                if candidate.is_empty() || candidate.starts_with('#') {
                    line_index += 1;
                    continue;
                }
                let tokens = tokenize_cif_line(lines[line_index]);
                token_count = token_count.saturating_add(tokens.len());
                if token_count > MAX_CIF_TOKENS {
                    return Err(Error::LimitExceeded(format!(
                        "CIF exceeds {MAX_CIF_TOKENS} tokens"
                    )));
                }
                row_tokens.extend(tokens);
                while row_tokens.len() >= columns.len() {
                    let row = row_tokens.drain(..columns.len()).collect::<Vec<_>>();
                    if atom_loop {
                        process_atom_row(
                            &columns,
                            &row,
                            &mut models,
                            &title,
                            &mut warnings,
                            &mut total_atoms,
                        )?;
                    }
                }
                line_index += 1;
            }
            if atom_loop && !row_tokens.is_empty() {
                push_warning_once(
                    &mut warnings,
                    "CIF atom_site loop ended with an incomplete row",
                );
            }
            continue;
        }
        line_index += 1;
    }
    Ok((models, warnings))
}

fn process_atom_row(
    columns: &[String],
    row: &[String],
    models: &mut BTreeMap<usize, Molecule>,
    title: &str,
    warnings: &mut Vec<String>,
    total_atoms: &mut usize,
) -> Result<()> {
    let value = |name: &str| {
        columns
            .iter()
            .position(|column| column.eq_ignore_ascii_case(name))
            .and_then(|index| row.get(index))
            .map(String::as_str)
    };
    let (Some(x_text), Some(y_text), Some(z_text)) = (
        value("_atom_site.Cartn_x"),
        value("_atom_site.Cartn_y"),
        value("_atom_site.Cartn_z"),
    ) else {
        return Err(Error::InvalidInput(
            "CIF atom_site loop is missing Cartn_x/Cartn_y/Cartn_z".into(),
        ));
    };
    if is_missing(x_text) || is_missing(y_text) || is_missing(z_text) {
        push_warning_once(warnings, "CIF atoms with missing coordinates were omitted");
        return Ok(());
    }
    let x = parse_coordinate(x_text, "x")?;
    let y = parse_coordinate(y_text, "y")?;
    let _z = parse_coordinate(z_text, "z")?;
    let model_number = value("_atom_site.pdbx_PDB_model_num")
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(1);
    if model_number == 0 || model_number > MAX_CIF_MODELS {
        return Err(Error::LimitExceeded(format!(
            "CIF model exceeds {MAX_CIF_MODELS}"
        )));
    }
    let alt = value("_atom_site.label_alt_id").unwrap_or(".");
    if !matches!(alt, "." | "?" | "" | "A") {
        push_warning_once(
            warnings,
            "CIF alternate atom locations other than blank/A were omitted",
        );
        return Ok(());
    }
    let element = value("_atom_site.type_symbol")
        .filter(|value| !is_missing(value))
        .map(clean_element)
        .unwrap_or_else(|| "X".into());
    let molecule = models.entry(model_number).or_insert_with(|| Molecule {
        title: if title.is_empty() {
            format!("CIF model {model_number}")
        } else {
            format!("{title} (model {model_number})")
        },
        atoms: Vec::new(),
        bonds: Vec::new(),
        warnings: Vec::new(),
    });
    if molecule.atoms.len() >= MAX_CIF_ATOMS_PER_MODEL {
        return Err(Error::LimitExceeded(format!(
            "CIF model exceeds {MAX_CIF_ATOMS_PER_MODEL} atoms"
        )));
    }
    molecule.atoms.push(Atom {
        x,
        y,
        element,
        charge: 0,
        isotope: None,
    });
    molecule.warnings.push(
        "CIF 3D coordinates are projected onto the XY plane; bond inference is not performed"
            .into(),
    );
    *total_atoms = total_atoms.saturating_add(1);
    if *total_atoms > MAX_CIF_TOTAL_ATOMS {
        return Err(Error::LimitExceeded(format!(
            "CIF exceeds {MAX_CIF_TOTAL_ATOMS} total atoms"
        )));
    }
    Ok(())
}

fn tokenize_cif_line(line: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut quote = None::<char>;
    for character in line.chars() {
        if let Some(delimiter) = quote {
            if character == delimiter {
                quote = None;
            } else {
                current.push(character);
            }
        } else if character == '\'' || character == '"' {
            quote = Some(character);
        } else if character.is_whitespace() {
            if !current.is_empty() {
                tokens.push(std::mem::take(&mut current));
            }
        } else if character == '#' && current.is_empty() {
            break;
        } else {
            current.push(character);
        }
    }
    if !current.is_empty() {
        tokens.push(current);
    }
    tokens
}

fn is_cif_control(line: &str) -> bool {
    let lower = line.to_ascii_lowercase();
    line.starts_with('_')
        || lower == "loop_"
        || lower.starts_with("data_")
        || lower.starts_with("save_")
}

fn parse_coordinate(value: &str, label: &str) -> Result<f64> {
    let value = value.trim_matches(|character| character == '(' || character == ')');
    let coordinate = value
        .parse::<f64>()
        .map_err(|_| Error::InvalidInput(format!("CIF {label} coordinate is invalid")))?;
    if !coordinate.is_finite() || coordinate.abs() > MAX_CIF_COORDINATE {
        return Err(Error::InvalidInput(format!(
            "CIF {label} coordinate exceeds ±{MAX_CIF_COORDINATE}"
        )));
    }
    Ok(coordinate)
}

fn clean_element(value: &str) -> String {
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

fn is_missing(value: &str) -> bool {
    matches!(value.trim(), "." | "?") || value.trim().is_empty()
}

fn push_warning_once(warnings: &mut Vec<String>, warning: &str) {
    if !warnings.iter().any(|existing| existing == warning) {
        warnings.push(warning.to_owned());
    }
}
