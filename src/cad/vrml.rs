//! Bounded VRML97 text mesh preview.
//!
//! This reader handles the geometry-bearing `Coordinate`/`IndexedFaceSet` and
//! `IndexedLineSet` subset of VRML97.  It deliberately treats transforms,
//! appearances, scripts, routes, and external URLs as inert metadata, then
//! delegates validated triangles/lines to the existing shaded OBJ renderer.

use std::io::{Cursor, Read};

use crate::cad::obj;
use crate::convert::{ConvertOptions, PageConsumer};
use crate::error::{Error, Result};

const MAX_VRML_TOKENS: usize = 20_000_000;
const MAX_VRML_POINTS: usize = 2_000_000;
const MAX_VRML_FACES: usize = 500_000;
const MAX_VRML_LINES: usize = 500_000;
const MAX_VRML_INDEX_REFERENCES: usize = 8_000_000;
const MAX_VRML_GENERATED_OBJ_BYTES: usize = 256 * 1024 * 1024;
const MAX_VRML_BRACKET_DEPTH: usize = 128;

#[derive(Clone, Debug)]
enum Token {
    Word(String),
    Number(f64),
    Open,
    Close,
}

pub(crate) fn looks_like_prefix(prefix: &[u8]) -> bool {
    let Ok(text) = std::str::from_utf8(prefix) else {
        return false;
    };
    let trimmed = text.trim_start();
    trimmed.starts_with("#VRML V2.0") || trimmed.starts_with("#VRML V1.0")
}

pub(crate) fn convert<R: Read>(
    input: R,
    options: &ConvertOptions,
    sink: &mut dyn PageConsumer,
) -> Result<Vec<String>> {
    let mut bytes = Vec::new();
    Read::take(input, options.max_input_bytes.saturating_add(1)).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > options.max_input_bytes {
        return Err(Error::LimitExceeded(format!(
            "VRML input exceeds maximum limit of {}",
            options.max_input_bytes
        )));
    }
    let text = String::from_utf8(bytes)
        .map_err(|error| Error::InvalidInput(format!("VRML text is not UTF-8: {error}")))?;
    let (obj_text, mut warnings) = parse(&text)?;
    if obj_text.is_empty() {
        return Err(Error::InvalidInput(
            "VRML contains no supported IndexedFaceSet or IndexedLineSet geometry".into(),
        ));
    }
    let lower = text.to_ascii_lowercase();
    if lower.contains("externproto")
        || lower.contains("script")
        || lower.contains("url")
        || lower.contains("inline")
        || lower.contains("route")
    {
        warnings.push(
            "VRML scripts, routes, transforms, and external URLs were ignored; no active content was executed".into(),
        );
    }
    let mut renderer_warnings = obj::convert(Cursor::new(obj_text), options, sink)?;
    warnings.append(&mut renderer_warnings);
    Ok(warnings)
}

fn parse(text: &str) -> Result<(String, Vec<String>)> {
    let tokens = tokenize(text)?;
    let mut obj_text = String::new();
    obj_text.push_str("# VRML validated mesh\n");
    let mut warnings = Vec::new();
    let mut point_blocks = 0usize;
    let mut total_points = 0usize;
    let mut total_faces = 0usize;
    let mut total_lines = 0usize;
    let mut total_refs = 0usize;
    let mut cursor = 0usize;
    while cursor < tokens.len() {
        let Some((word, next)) = word_at(&tokens, cursor) else {
            cursor += 1;
            continue;
        };
        if !word.eq_ignore_ascii_case("point") || !is_open(tokens.get(next)) {
            cursor += 1;
            continue;
        }
        let point_end = matching_close(&tokens, next)?;
        if !is_coordinate_point(&tokens, cursor) {
            cursor = point_end + 1;
            continue;
        }
        let point_values = parse_point_values(&tokens[next + 1..point_end])?;
        if point_values.is_empty() {
            cursor = point_end + 1;
            continue;
        }
        point_blocks += 1;
        if point_blocks > MAX_VRML_POINTS {
            return Err(Error::LimitExceeded(format!(
                "VRML contains more than {MAX_VRML_POINTS} Coordinate blocks"
            )));
        }
        total_points = total_points
            .checked_add(point_values.len())
            .ok_or_else(|| Error::LimitExceeded("VRML point count overflowed".into()))?;
        if total_points > MAX_VRML_POINTS {
            return Err(Error::LimitExceeded(format!(
                "VRML exceeds {MAX_VRML_POINTS} points"
            )));
        }
        let base_index = total_points - point_values.len() + 1;
        for (x, y, z) in &point_values {
            append_obj_line(&mut obj_text, format_args!("v {x:.17} {y:.17} {z:.17}\n"))?;
        }
        let search_end = tokens
            .iter()
            .enumerate()
            .skip(point_end + 1)
            .find(|(index, token)| {
                matches!(token, Token::Word(word) if word.eq_ignore_ascii_case("point"))
                    && is_coordinate_point(&tokens, *index)
            })
            .map(|(index, _)| index)
            .unwrap_or(tokens.len());
        let Some(index_word) = find_word(&tokens, point_end + 1, search_end, "coordIndex") else {
            warnings.push("VRML Coordinate point list has no coordIndex; it was omitted".into());
            cursor = point_end + 1;
            continue;
        };
        let Some(index_open) = next_nonempty_index(&tokens, index_word + 1) else {
            return Err(Error::InvalidInput(
                "VRML coordIndex list is missing".into(),
            ));
        };
        if !is_open(tokens.get(index_open)) {
            return Err(Error::InvalidInput(
                "VRML coordIndex is not followed by '['".into(),
            ));
        }
        let index_end = matching_close(&tokens, index_open)?;
        let records = parse_indices(&tokens[index_open + 1..index_end], point_values.len())?;
        total_refs = total_refs
            .checked_add(records.iter().map(Vec::len).sum::<usize>())
            .ok_or_else(|| Error::LimitExceeded("VRML index count overflowed".into()))?;
        if total_refs > MAX_VRML_INDEX_REFERENCES {
            return Err(Error::LimitExceeded(format!(
                "VRML exceeds {MAX_VRML_INDEX_REFERENCES} index references"
            )));
        }
        let is_line_set = tokens
            .get(cursor..index_word)
            .unwrap_or_default()
            .iter()
            .any(|token| matches!(token, Token::Word(word) if word.eq_ignore_ascii_case("IndexedLineSet")));
        if is_line_set {
            for line in records {
                if line.len() < 2 {
                    continue;
                }
                total_lines += 1;
                if total_lines > MAX_VRML_LINES {
                    return Err(Error::LimitExceeded(format!(
                        "VRML exceeds {MAX_VRML_LINES} indexed lines"
                    )));
                }
                append_obj_indices(&mut obj_text, 'l', &line, base_index)?;
            }
        } else {
            for face in records {
                if face.len() < 3 {
                    continue;
                }
                total_faces += 1;
                if total_faces > MAX_VRML_FACES {
                    return Err(Error::LimitExceeded(format!(
                        "VRML exceeds {MAX_VRML_FACES} indexed faces"
                    )));
                }
                append_obj_indices(&mut obj_text, 'f', &face, base_index)?;
            }
        }
        cursor = index_end + 1;
    }
    Ok((obj_text, warnings))
}

fn tokenize(text: &str) -> Result<Vec<Token>> {
    let mut tokens = Vec::new();
    let mut chars = text.chars().peekable();
    while let Some(character) = chars.next() {
        if character.is_whitespace() || character == ',' {
            continue;
        }
        if character == '#' {
            for next in chars.by_ref() {
                if next == '\n' {
                    break;
                }
            }
            continue;
        }
        if character == '[' {
            tokens.push(Token::Open);
        } else if character == ']' {
            tokens.push(Token::Close);
        } else {
            let mut value = String::from(character);
            while let Some(next) = chars.peek().copied() {
                if next.is_whitespace() || next == ',' || next == '[' || next == ']' || next == '#'
                {
                    break;
                }
                value.push(next);
                chars.next();
            }
            if let Ok(number) = value.parse::<f64>() {
                if !number.is_finite() {
                    return Err(Error::InvalidInput(
                        "VRML contains a non-finite number".into(),
                    ));
                }
                tokens.push(Token::Number(number));
            } else {
                tokens.push(Token::Word(value));
            }
        }
        if tokens.len() > MAX_VRML_TOKENS {
            return Err(Error::LimitExceeded(format!(
                "VRML exceeds {MAX_VRML_TOKENS} tokens"
            )));
        }
    }
    Ok(tokens)
}

fn parse_point_values(tokens: &[Token]) -> Result<Vec<(f64, f64, f64)>> {
    let numbers = tokens
        .iter()
        .filter_map(|token| match token {
            Token::Number(value) => Some(*value),
            _ => None,
        })
        .collect::<Vec<_>>();
    if numbers.len() % 3 != 0 {
        return Err(Error::InvalidInput(
            "VRML Coordinate point list is not divisible by three".into(),
        ));
    }
    Ok(numbers
        .chunks_exact(3)
        .map(|chunk| (chunk[0], chunk[1], chunk[2]))
        .collect())
}

fn parse_indices(tokens: &[Token], point_count: usize) -> Result<Vec<Vec<usize>>> {
    let mut records = Vec::new();
    let mut current = Vec::new();
    for token in tokens {
        let Token::Number(value) = token else {
            continue;
        };
        if *value == -1.0 {
            if current.len() >= 2 {
                records.push(std::mem::take(&mut current));
            } else {
                current.clear();
            }
            continue;
        }
        if *value < 0.0 || value.fract() != 0.0 {
            return Err(Error::InvalidInput(format!(
                "VRML coordIndex contains invalid index {value}"
            )));
        }
        let index = *value as usize;
        if index >= point_count {
            return Err(Error::InvalidInput(format!(
                "VRML coordIndex {index} exceeds {point_count} points"
            )));
        }
        current.push(index);
    }
    if current.len() >= 2 {
        records.push(current);
    }
    Ok(records)
}

fn append_obj_indices(
    output: &mut String,
    kind: char,
    indices: &[usize],
    base: usize,
) -> Result<()> {
    output.push(kind);
    for index in indices {
        output.push_str(&format!(" {}", base + index));
    }
    output.push('\n');
    if output.len() > MAX_VRML_GENERATED_OBJ_BYTES {
        return Err(Error::LimitExceeded(format!(
            "VRML generated mesh exceeds {MAX_VRML_GENERATED_OBJ_BYTES} bytes"
        )));
    }
    Ok(())
}

fn append_obj_line(output: &mut String, line: std::fmt::Arguments<'_>) -> Result<()> {
    use std::fmt::Write;
    output
        .write_fmt(line)
        .map_err(|_| Error::InvalidInput("VRML mesh output failed".into()))?;
    if output.len() > MAX_VRML_GENERATED_OBJ_BYTES {
        return Err(Error::LimitExceeded(format!(
            "VRML generated mesh exceeds {MAX_VRML_GENERATED_OBJ_BYTES} bytes"
        )));
    }
    Ok(())
}

fn word_at(tokens: &[Token], index: usize) -> Option<(&str, usize)> {
    match tokens.get(index) {
        Some(Token::Word(word)) => Some((word, index + 1)),
        _ => None,
    }
}

fn is_coordinate_point(tokens: &[Token], point_word: usize) -> bool {
    point_word >= 2
        && matches!(tokens.get(point_word - 1), Some(Token::Open))
        && matches!(tokens.get(point_word - 2), Some(Token::Word(word)) if word.eq_ignore_ascii_case("Coordinate"))
}

fn find_word(tokens: &[Token], start: usize, end: usize, wanted: &str) -> Option<usize> {
    tokens
        .iter()
        .enumerate()
        .skip(start)
        .take(end.saturating_sub(start))
        .find_map(|(index, token)| match token {
            Token::Word(word) if word.eq_ignore_ascii_case(wanted) => Some(index),
            _ => None,
        })
}

fn next_nonempty_index(tokens: &[Token], index: usize) -> Option<usize> {
    (index < tokens.len()).then_some(index)
}

fn is_open(token: Option<&Token>) -> bool {
    matches!(token, Some(Token::Open))
}

fn matching_close(tokens: &[Token], open: usize) -> Result<usize> {
    let mut depth = 0usize;
    for (index, token) in tokens.iter().enumerate().skip(open) {
        match token {
            Token::Open => {
                depth += 1;
                if depth > MAX_VRML_BRACKET_DEPTH {
                    return Err(Error::LimitExceeded(format!(
                        "VRML bracket nesting exceeds {MAX_VRML_BRACKET_DEPTH}"
                    )));
                }
            }
            Token::Close => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    return Ok(index);
                }
            }
            _ => {}
        }
    }
    Err(Error::InvalidInput(
        "VRML bracket list is not terminated".into(),
    ))
}
