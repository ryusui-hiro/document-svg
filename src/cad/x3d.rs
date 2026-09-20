//! Bounded X3D XML mesh preview.
//!
//! Extracts Coordinate points and IndexedFaceSet/IndexedLineSet indices from
//! X3D XML and reuses the shaded OBJ renderer. Scene transforms, materials,
//! textures, normals, animations, and external URLs are not evaluated.

use std::io::Cursor;
use std::path::Path;

use quick_xml::Reader;
use quick_xml::events::Event;

use crate::convert::{ConvertOptions, PageConsumer, read_limited_file};
use crate::error::{Error, Result};
use crate::ooxml::{attribute, local_name};

const MAX_X3D_INPUT_BYTES: u64 = 64 * 1024 * 1024;
const MAX_X3D_XML_EVENTS: usize = 1_000_000;
const MAX_X3D_XML_DEPTH: usize = 256;
const MAX_X3D_VERTICES: usize = 2_000_000;
const MAX_X3D_FACES: usize = 500_000;
const MAX_X3D_LINES: usize = 500_000;

struct WarningSink<'a> {
    inner: &'a mut dyn PageConsumer,
    warnings: &'a [String],
}

impl PageConsumer for WarningSink<'_> {
    fn consume(&mut self, mut page: crate::ir::Page) -> Result<()> {
        page.source_format = "x3d".into();
        for warning in self.warnings {
            page.warn(warning.clone());
        }
        self.inner.consume(page)
    }
}

pub(crate) fn looks_like_prefix(bytes: &[u8]) -> bool {
    let text = String::from_utf8_lossy(bytes).to_ascii_lowercase();
    text.contains("<x3d") && (text.contains("indexedfaceset") || text.contains("coordinate"))
}

pub(crate) fn convert(
    path: &Path,
    options: &ConvertOptions,
    sink: &mut dyn PageConsumer,
) -> Result<Vec<String>> {
    let bytes = read_limited_file(
        path,
        options.max_input_bytes.min(MAX_X3D_INPUT_BYTES),
        "X3D input",
    )?;
    let (obj, mut warnings) = parse_x3d(&bytes, options.max_xml_events)?;
    let mut warning_sink = WarningSink {
        inner: sink,
        warnings: &warnings,
    };
    let obj_warnings = crate::cad::obj::convert(Cursor::new(obj), options, &mut warning_sink)?;
    warnings.extend(obj_warnings);
    warnings.sort();
    warnings.dedup();
    Ok(warnings)
}

fn parse_x3d(bytes: &[u8], max_events: usize) -> Result<(String, Vec<String>)> {
    let mut reader = Reader::from_reader(Cursor::new(bytes));
    reader.config_mut().trim_text(true);
    let mut buffer = Vec::new();
    let mut stack = Vec::<Vec<u8>>::new();
    let mut coordinates = Vec::<[f64; 3]>::new();
    let mut faces = Vec::<Vec<usize>>::new();
    let mut lines = Vec::<Vec<usize>>::new();
    let mut warnings = Vec::new();
    let mut events = 0usize;
    loop {
        events = events.saturating_add(1);
        if events > max_events.min(MAX_X3D_XML_EVENTS) {
            return Err(Error::LimitExceeded(format!(
                "X3D XML exceeds {} parser events",
                max_events.min(MAX_X3D_XML_EVENTS)
            )));
        }
        match reader.read_event_into(&mut buffer)? {
            Event::Start(element) => {
                if stack.len() >= MAX_X3D_XML_DEPTH {
                    return Err(Error::LimitExceeded(format!(
                        "X3D XML nesting exceeds {MAX_X3D_XML_DEPTH}"
                    )));
                }
                stack.push(process_element(
                    &element,
                    &mut coordinates,
                    &mut faces,
                    &mut lines,
                    &mut warnings,
                )?);
            }
            Event::Empty(element) => {
                process_element(
                    &element,
                    &mut coordinates,
                    &mut faces,
                    &mut lines,
                    &mut warnings,
                )?;
            }
            Event::DocType(_) => push_warning_once(
                &mut warnings,
                "X3D DTD declaration was ignored; external entities were not loaded",
            ),
            Event::End(_) => {
                stack.pop();
            }
            Event::Eof => break,
            _ => {}
        }
        buffer.clear();
    }
    if faces.is_empty() && lines.is_empty() {
        return Err(Error::InvalidInput(
            "X3D contains no supported IndexedFaceSet or IndexedLineSet geometry".into(),
        ));
    }
    if coordinates.is_empty() {
        return Err(Error::InvalidInput(
            "X3D contains no Coordinate points".into(),
        ));
    }
    let mut obj = String::new();
    for [x, y, z] in &coordinates {
        obj.push_str(&format!("v {x} {y} {z}\n"));
    }
    for face in &faces {
        if face.len() < 3 || face.iter().any(|index| *index >= coordinates.len()) {
            push_warning_once(&mut warnings, "X3D faces with invalid indices were omitted");
            continue;
        }
        obj.push('f');
        for index in face {
            obj.push_str(&format!(" {}", index + 1));
        }
        obj.push('\n');
    }
    for line in &lines {
        if line.len() < 2 || line.iter().any(|index| *index >= coordinates.len()) {
            push_warning_once(&mut warnings, "X3D lines with invalid indices were omitted");
            continue;
        }
        obj.push('l');
        for index in line {
            obj.push_str(&format!(" {}", index + 1));
        }
        obj.push('\n');
    }
    Ok((obj, warnings))
}

fn process_element(
    element: &quick_xml::events::BytesStart<'_>,
    coordinates: &mut Vec<[f64; 3]>,
    faces: &mut Vec<Vec<usize>>,
    lines: &mut Vec<Vec<usize>>,
    warnings: &mut Vec<String>,
) -> Result<Vec<u8>> {
    let qualified = element.name();
    let name = local_name(qualified.as_ref());
    match name {
        b"Coordinate" => {
            if let Some(point) = attribute(element, b"point") {
                append_points(&point, coordinates)?;
                if coordinates.len() > MAX_X3D_VERTICES {
                    return Err(Error::LimitExceeded(format!(
                        "X3D exceeds {MAX_X3D_VERTICES} vertices"
                    )));
                }
            }
        }
        b"IndexedFaceSet" => {
            if let Some(index) = attribute(element, b"coordIndex") {
                append_indexed(&index, faces, MAX_X3D_FACES)?;
            }
        }
        b"IndexedLineSet" => {
            if let Some(index) = attribute(element, b"coordIndex") {
                append_indexed(&index, lines, MAX_X3D_LINES)?;
            }
        }
        b"Transform"
        | b"Material"
        | b"Appearance"
        | b"ImageTexture"
        | b"Normal"
        | b"Color"
        | b"OrientationInterpolator"
        | b"PositionInterpolator" => push_warning_once(
            warnings,
            "X3D transforms, materials, textures, normals, colors, and animation are not reconstructed",
        ),
        _ => {}
    }
    Ok(name.to_vec())
}

fn append_points(text: &str, output: &mut Vec<[f64; 3]>) -> Result<()> {
    let values = text
        .split(|character: char| character.is_ascii_whitespace() || character == ',')
        .filter(|value| !value.is_empty())
        .map(|value| {
            value
                .parse::<f64>()
                .ok()
                .filter(|number| number.is_finite())
                .ok_or_else(|| Error::InvalidInput("X3D point contains an invalid number".into()))
        })
        .collect::<Result<Vec<_>>>()?;
    if values.len() % 3 != 0 {
        return Err(Error::InvalidInput(
            "X3D Coordinate point list is not divisible by three".into(),
        ));
    }
    for chunk in values.chunks_exact(3) {
        output.push([chunk[0], chunk[1], chunk[2]]);
    }
    Ok(())
}

fn append_indexed(text: &str, output: &mut Vec<Vec<usize>>, limit: usize) -> Result<()> {
    let mut current = Vec::new();
    for value in text.split(|character: char| character.is_ascii_whitespace() || character == ',') {
        if value.is_empty() {
            continue;
        }
        let index = value.parse::<isize>().map_err(|_| {
            Error::InvalidInput("X3D index list contains an invalid integer".into())
        })?;
        if index < 0 {
            if current.len() >= 2 {
                output.push(std::mem::take(&mut current));
                if output.len() > limit {
                    return Err(Error::LimitExceeded(format!(
                        "X3D exceeds {limit} indexed primitives"
                    )));
                }
            } else {
                current.clear();
            }
        } else {
            current.push(index as usize);
        }
    }
    if current.len() >= 2 {
        output.push(current);
        if output.len() > limit {
            return Err(Error::LimitExceeded(format!(
                "X3D exceeds {limit} indexed primitives"
            )));
        }
    }
    Ok(())
}

fn push_warning_once(warnings: &mut Vec<String>, warning: &str) {
    if !warnings.iter().any(|existing| existing == warning) {
        warnings.push(warning.to_owned());
    }
}
