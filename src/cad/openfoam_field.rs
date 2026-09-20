//! Bounded OpenFOAM vol/surface field previews.
//!
//! This parser summarizes standalone OpenFOAM field files without opening the
//! case, running a solver, evaluating boundary conditions, or displaying the
//! internal value list. It accepts ASCII FoamFile dictionaries and supports
//! uniform and nonuniform scalar/vector fields.

use std::path::Path;

use crate::convert::{ConvertOptions, PageConsumer, read_limited_file};
use crate::document::html::{HtmlBlock, render_blocks_to_pages};
use crate::error::{Error, Result};
use crate::table::{TableAlign, TableData};

const MAX_FIELD_BYTES: u64 = 128 * 1024 * 1024;
const MAX_FIELD_TOKENS: usize = 20_000_000;
const MAX_FIELD_ENTRIES: usize = 1_000_000;
const MAX_FIELD_PATCHES: usize = 100_000;
const MAX_FIELD_STRING_BYTES: usize = 512 * 1024;

pub(crate) fn looks_like_prefix(bytes: &[u8]) -> bool {
    let text = String::from_utf8_lossy(bytes);
    text.contains("FoamFile")
        && text.contains("internalField")
        && (text.contains("volScalarField")
            || text.contains("volVectorField")
            || text.contains("surfaceScalarField")
            || text.contains("surfaceVectorField"))
}

struct FieldPageSink<'a> {
    inner: &'a mut dyn PageConsumer,
    warnings: &'a [String],
}

impl PageConsumer for FieldPageSink<'_> {
    fn consume(&mut self, mut page: crate::ir::Page) -> Result<()> {
        page.source_format = "openfoam-field".into();
        if page.title.is_empty() {
            page.title = "OpenFOAM field".into();
        }
        page.description =
            "OpenFOAM field metadata and bounded value statistics are rendered inertly; solver and boundary operations are never performed".into();
        for warning in self.warnings {
            page.warn(warning.clone());
        }
        self.inner.consume(page)
    }
}

pub(crate) fn convert(
    path: &Path,
    options: &ConvertOptions,
    sink: &mut dyn PageConsumer,
) -> Result<Vec<String>> {
    let bytes = read_limited_file(
        path,
        options.max_input_bytes.min(MAX_FIELD_BYTES),
        "OpenFOAM field input",
    )?;
    let (table, metadata, warnings) = parse(&bytes)?;
    let blocks = vec![
        HtmlBlock::Heading {
            level: 1,
            text: "OpenFOAM field".into(),
        },
        HtmlBlock::Paragraph { text: metadata },
        HtmlBlock::Table(table),
    ];
    let mut page_sink = FieldPageSink {
        inner: sink,
        warnings: &warnings,
    };
    render_blocks_to_pages(&blocks, &mut page_sink, options)?;
    Ok(warnings)
}

#[derive(Clone, Debug)]
struct FieldStats {
    class: String,
    object: String,
    location: String,
    format: String,
    internal: String,
    value_type: String,
    entries: usize,
    minimum: Option<f64>,
    maximum: Option<f64>,
    patches: usize,
}

fn parse(bytes: &[u8]) -> Result<(TableData, String, Vec<String>)> {
    if bytes.len() as u64 > MAX_FIELD_BYTES {
        return Err(Error::LimitExceeded(format!(
            "OpenFOAM field exceeds {MAX_FIELD_BYTES} bytes"
        )));
    }
    let text = std::str::from_utf8(bytes).map_err(|error| {
        Error::InvalidInput(format!("OpenFOAM field must be ASCII/UTF-8: {error}"))
    })?;
    let tokens = tokenize(text)?;
    let mut cursor = Cursor {
        tokens: &tokens,
        index: 0,
    };
    let mut stats = FieldStats {
        class: String::new(),
        object: String::new(),
        location: String::new(),
        format: String::new(),
        internal: String::new(),
        value_type: String::new(),
        entries: 0,
        minimum: None,
        maximum: None,
        patches: 0,
    };
    cursor.expect("FoamFile")?;
    cursor.expect("{")?;
    let mut header_depth = 1usize;
    while header_depth > 0 {
        let token = cursor.next().ok_or_else(|| {
            Error::InvalidInput("OpenFOAM field FoamFile header is truncated".into())
        })?;
        match token {
            "{" => header_depth += 1,
            "}" => header_depth -= 1,
            "class" | "object" | "location" | "format" => {
                let key = token;
                let value = cursor.next().ok_or_else(|| {
                    Error::InvalidInput(format!("OpenFOAM field header {key} is missing"))
                })?;
                match key {
                    "class" => stats.class = value.to_owned(),
                    "object" => stats.object = value.to_owned(),
                    "location" => stats.location = value.to_owned(),
                    "format" => stats.format = value.to_owned(),
                    _ => {}
                }
            }
            _ => {}
        }
    }
    if !stats.format.is_empty() && !stats.format.eq_ignore_ascii_case("ascii") {
        return Err(Error::Unsupported(format!(
            "OpenFOAM field format '{}' is unsupported; ASCII is required",
            stats.format
        )));
    }
    while let Some(token) = cursor.next() {
        match token {
            "internalField" => {
                parse_internal_field(&mut cursor, &mut stats)?;
            }
            "boundaryField" => {
                stats.patches = parse_patch_count(&mut cursor)?;
            }
            _ => {}
        }
    }
    if stats.class.is_empty() {
        return Err(Error::InvalidInput(
            "OpenFOAM field header has no class".into(),
        ));
    }
    if stats.internal.is_empty() {
        return Err(Error::InvalidInput(
            "OpenFOAM field has no internalField".into(),
        ));
    }
    let minimum = stats.minimum.map(format_number);
    let maximum = stats.maximum.map(format_number);
    let rows = vec![vec![
        truncate(&stats.object),
        truncate(&stats.class),
        truncate(&stats.internal),
        stats.entries.to_string(),
        minimum.unwrap_or_else(|| "—".into()),
        maximum.unwrap_or_else(|| "—".into()),
        stats.patches.to_string(),
    ]];
    let object_meta = if stats.object.is_empty() {
        "—".into()
    } else {
        truncate(&stats.object)
    };
    let location_meta = if stats.location.is_empty() {
        "—".into()
    } else {
        truncate(&stats.location)
    };
    let metadata = format!(
        "Class: {}\nObject: {}\nLocation: {}\nInternal field: {}\nEntries: {}\nBoundary patches: {}",
        truncate(&stats.class),
        object_meta,
        location_meta,
        stats.value_type,
        stats.entries,
        stats.patches
    );
    let warnings = vec![
        "OpenFOAM field values, dimensions, boundary condition dictionaries, patch values, directives and case paths are omitted; solver, filesystem traversal, command and network operations are never performed".into(),
        "Uniform/nonuniform scalar or vector value statistics are computed locally without retaining the value list".into(),
    ];
    Ok((
        TableData {
            headers: vec![
                "Obj".into(),
                "Cls".into(),
                "Mode".into(),
                "N".into(),
                "Min".into(),
                "Max".into(),
                "Pch".into(),
            ],
            rows,
            alignments: vec![TableAlign::Left; 7],
            raw_source: String::new(),
        },
        metadata,
        warnings,
    ))
}

fn parse_internal_field(cursor: &mut Cursor<'_>, stats: &mut FieldStats) -> Result<()> {
    let kind = cursor
        .next()
        .ok_or_else(|| Error::InvalidInput("OpenFOAM internalField kind is missing".into()))?;
    match kind {
        "uniform" => {
            stats.internal = "uniform".into();
            if stats.class.contains("Vector") {
                let vector = parse_vector(cursor)?;
                stats.value_type = "uniform vector magnitude".into();
                stats.entries = 1;
                stats.minimum = Some(vector);
                stats.maximum = Some(vector);
            } else {
                let value = parse_number(cursor.next().ok_or_else(|| {
                    Error::InvalidInput("OpenFOAM uniform scalar is missing".into())
                })?)?;
                stats.value_type = "uniform scalar".into();
                stats.entries = 1;
                stats.minimum = Some(value);
                stats.maximum = Some(value);
            }
        }
        "nonuniform" => {
            stats.internal = "nonuniform".into();
            let _list = cursor.next();
            let _less = cursor.next();
            let value_type = cursor.next().unwrap_or("scalar");
            let _greater = cursor.next();
            let count = cursor
                .next()
                .ok_or_else(|| {
                    Error::InvalidInput("OpenFOAM nonuniform field count is missing".into())
                })?
                .parse::<usize>()
                .map_err(|_| {
                    Error::InvalidInput("OpenFOAM nonuniform field count is invalid".into())
                })?;
            if count > MAX_FIELD_ENTRIES {
                return Err(Error::LimitExceeded(format!(
                    "OpenFOAM field entries exceed {MAX_FIELD_ENTRIES}"
                )));
            }
            cursor.expect("(")?;
            stats.entries = count;
            stats.value_type = format!("nonuniform {value_type}");
            for _ in 0..count {
                let value = if value_type.eq_ignore_ascii_case("vector")
                    || stats.class.contains("Vector")
                {
                    parse_vector(cursor)?
                } else {
                    parse_number(cursor.next().ok_or_else(|| {
                        Error::InvalidInput("OpenFOAM nonuniform value is missing".into())
                    })?)?
                };
                stats.minimum = Some(stats.minimum.map_or(value, |current| current.min(value)));
                stats.maximum = Some(stats.maximum.map_or(value, |current| current.max(value)));
            }
            cursor.expect(")")?;
        }
        _ => {
            return Err(Error::Unsupported(format!(
                "OpenFOAM internalField kind '{kind}' is unsupported"
            )));
        }
    }
    Ok(())
}

fn parse_patch_count(cursor: &mut Cursor<'_>) -> Result<usize> {
    cursor.expect("{")?;
    let mut depth = 1usize;
    let mut patches = 0usize;
    while depth > 0 {
        let token = cursor
            .next()
            .ok_or_else(|| Error::InvalidInput("OpenFOAM boundaryField is truncated".into()))?;
        match token {
            "{" => depth += 1,
            "}" => depth -= 1,
            _ if depth == 1 => {
                patches = patches.saturating_add(1);
                if patches > MAX_FIELD_PATCHES {
                    return Err(Error::LimitExceeded(format!(
                        "OpenFOAM field patches exceed {MAX_FIELD_PATCHES}"
                    )));
                }
            }
            _ => {}
        }
    }
    Ok(patches)
}

struct Cursor<'a> {
    tokens: &'a [String],
    index: usize,
}

impl<'a> Cursor<'a> {
    fn next(&mut self) -> Option<&'a str> {
        let token = self.tokens.get(self.index)?;
        self.index += 1;
        Some(token)
    }

    fn expect(&mut self, expected: &str) -> Result<()> {
        let actual = self
            .next()
            .ok_or_else(|| Error::InvalidInput(format!("OpenFOAM field expected '{expected}'")))?;
        if actual != expected {
            return Err(Error::InvalidInput(format!(
                "OpenFOAM field expected '{expected}', found '{actual}'"
            )));
        }
        Ok(())
    }
}

fn tokenize(text: &str) -> Result<Vec<String>> {
    let mut tokens = Vec::new();
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch.is_whitespace() {
            continue;
        }
        if ch == '/' && chars.peek() == Some(&'/') {
            chars.next();
            for next in chars.by_ref() {
                if next == '\n' {
                    break;
                }
            }
            continue;
        }
        if ch == '/' && chars.peek() == Some(&'*') {
            chars.next();
            let mut previous = '\0';
            loop {
                let next = chars.next().ok_or_else(|| {
                    Error::InvalidInput("unterminated OpenFOAM field comment".into())
                })?;
                if previous == '*' && next == '/' {
                    break;
                }
                previous = next;
            }
            continue;
        }
        if matches!(ch, '{' | '}' | '(' | ')' | '[' | ']' | '<' | '>' | ';') {
            tokens.push(ch.to_string());
        } else if ch == '"' || ch == '\'' {
            let quote = ch;
            let mut value = String::new();
            for next in chars.by_ref() {
                if next == quote {
                    break;
                }
                value.push(next);
                if value.len() > MAX_FIELD_STRING_BYTES {
                    return Err(Error::LimitExceeded(format!(
                        "OpenFOAM field token exceeds {MAX_FIELD_STRING_BYTES} bytes"
                    )));
                }
            }
            tokens.push(value);
        } else {
            let mut value = String::from(ch);
            while let Some(&next) = chars.peek() {
                if next.is_whitespace()
                    || matches!(next, '{' | '}' | '(' | ')' | '[' | ']' | '<' | '>' | ';')
                {
                    break;
                }
                value.push(next);
                chars.next();
                if value.len() > MAX_FIELD_STRING_BYTES {
                    return Err(Error::LimitExceeded(format!(
                        "OpenFOAM field token exceeds {MAX_FIELD_STRING_BYTES} bytes"
                    )));
                }
            }
            tokens.push(value);
        }
        if tokens.len() > MAX_FIELD_TOKENS {
            return Err(Error::LimitExceeded(format!(
                "OpenFOAM field tokens exceed {MAX_FIELD_TOKENS}"
            )));
        }
    }
    Ok(tokens)
}

fn parse_number(value: &str) -> Result<f64> {
    let value = value.trim_end_matches(';');
    let number = value
        .parse::<f64>()
        .map_err(|_| Error::InvalidInput(format!("invalid OpenFOAM field scalar '{value}'")))?;
    if !number.is_finite() || number.abs() > 1e12 {
        return Err(Error::InvalidInput(
            "OpenFOAM field scalar is non-finite or outside ±1e12".into(),
        ));
    }
    Ok(number)
}

fn parse_vector(cursor: &mut Cursor<'_>) -> Result<f64> {
    cursor.expect("(")?;
    let x = parse_number(
        cursor
            .next()
            .ok_or_else(|| Error::InvalidInput("OpenFOAM vector x is missing".into()))?,
    )?;
    let y = parse_number(
        cursor
            .next()
            .ok_or_else(|| Error::InvalidInput("OpenFOAM vector y is missing".into()))?,
    )?;
    let z = parse_number(
        cursor
            .next()
            .ok_or_else(|| Error::InvalidInput("OpenFOAM vector z is missing".into()))?,
    )?;
    cursor.expect(")")?;
    let magnitude = x.hypot(y).hypot(z);
    if !magnitude.is_finite() || magnitude > 1e12 {
        return Err(Error::InvalidInput(
            "OpenFOAM vector magnitude is non-finite or outside ±1e12".into(),
        ));
    }
    Ok(magnitude)
}

fn format_number(value: f64) -> String {
    if value == 0.0 {
        "0".into()
    } else {
        format!("{value:.6}")
    }
}

fn truncate(value: &str) -> String {
    if value.len() <= MAX_FIELD_STRING_BYTES {
        return value.to_owned();
    }
    let mut end = MAX_FIELD_STRING_BYTES;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &value[..end])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognizes_field_header() {
        assert!(looks_like_prefix(
            b"FoamFile { class volScalarField; } internalField uniform 0;"
        ));
        assert!(!looks_like_prefix(b"FoamFile { class dictionary; }"));
    }

    #[test]
    fn summarizes_scalar_and_vector_values() {
        let scalar = b"FoamFile { version 2.0; format ascii; class volScalarField; object p; } internalField nonuniform List<scalar> 2 ( 1.0 -2.0 ); boundaryField { inlet { type fixedValue; } outlet { type zeroGradient; } }";
        let (table, metadata, warnings) = parse(scalar).unwrap();
        assert!(metadata.contains("Entries: 2"));
        assert_eq!(table.rows[0][4], "-2.000000");
        assert_eq!(table.rows[0][6], "2");
        assert!(warnings.iter().any(|warning| warning.contains("never")));
        let vector = b"FoamFile { format ascii; class volVectorField; object U; } internalField uniform ( 3 4 0 ); boundaryField { inlet {} }";
        let (table, _, _) = parse(vector).unwrap();
        assert_eq!(table.rows[0][3], "1");
        assert_eq!(table.rows[0][4], "5.000000");
    }
}
