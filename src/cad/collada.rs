//! Bounded COLLADA 1.4/1.5 geometry preview.
//!
//! The adapter extracts position sources and triangle/polylist primitives from
//! a `.dae` XML package, converts them to an in-memory OBJ stream, and reuses
//! the existing shaded mesh renderer. Materials, animations, controllers,
//! external references, and node transforms are intentionally inert.

use std::collections::HashMap;
use std::io::Cursor;
use std::path::Path;

use quick_xml::Reader;
use quick_xml::events::Event;

use crate::convert::{ConvertOptions, PageConsumer, read_limited_file};
use crate::error::{Error, Result};
use crate::ooxml::{attribute, local_name};

const MAX_COLLADA_INPUT_BYTES: u64 = 64 * 1024 * 1024;
const MAX_COLLADA_XML_EVENTS: usize = 1_000_000;
const MAX_COLLADA_XML_DEPTH: usize = 256;
const MAX_COLLADA_SOURCES: usize = 100_000;
const MAX_COLLADA_VALUES: usize = 10_000_000;
const MAX_COLLADA_VERTICES: usize = 2_000_000;
const MAX_COLLADA_FACES: usize = 500_000;

#[derive(Default)]
struct SourceData {
    values: Vec<f64>,
    stride: usize,
}

#[derive(Default)]
struct Primitive {
    kind: String,
    inputs: Vec<(String, String, usize)>,
    indices: Vec<usize>,
    counts: Vec<usize>,
}

struct WarningSink<'a> {
    inner: &'a mut dyn PageConsumer,
    warnings: &'a [String],
}

impl PageConsumer for WarningSink<'_> {
    fn consume(&mut self, mut page: crate::ir::Page) -> Result<()> {
        page.source_format = "collada".into();
        for warning in self.warnings {
            page.warn(warning.clone());
        }
        self.inner.consume(page)
    }
}

pub(crate) fn looks_like_prefix(bytes: &[u8]) -> bool {
    let text = String::from_utf8_lossy(bytes).to_ascii_lowercase();
    text.contains("<collada") && text.contains("library_geometries")
}

pub(crate) fn convert(
    path: &Path,
    options: &ConvertOptions,
    sink: &mut dyn PageConsumer,
) -> Result<Vec<String>> {
    let bytes = read_limited_file(
        path,
        options.max_input_bytes.min(MAX_COLLADA_INPUT_BYTES),
        "COLLADA input",
    )?;
    let (obj, mut warnings) = parse_collada(&bytes, options.max_xml_events)?;
    let mut warning_sink = WarningSink {
        inner: sink,
        warnings: &warnings,
    };
    let obj_warnings = crate::cad::obj::convert(Cursor::new(obj), options, &mut warning_sink)?;
    warnings.extend(obj_warnings);
    dedup_warnings(&mut warnings);
    Ok(warnings)
}

fn parse_collada(bytes: &[u8], max_events: usize) -> Result<(String, Vec<String>)> {
    let mut reader = Reader::from_reader(bytes);
    reader.config_mut().trim_text(true);
    let mut buffer = Vec::new();
    let mut stack = Vec::<Vec<u8>>::new();
    let mut sources = HashMap::<String, SourceData>::new();
    let mut primitives = Vec::<Primitive>::new();
    let mut current_source = None::<String>;
    let mut current_source_text = String::new();
    let mut current_source_stride = 3usize;
    let mut current_primitive = None::<Primitive>;
    let mut current_text = String::new();
    let mut warnings = Vec::new();
    let mut events = 0usize;
    loop {
        events = events.saturating_add(1);
        if events > max_events.min(MAX_COLLADA_XML_EVENTS) {
            return Err(Error::LimitExceeded(format!(
                "COLLADA XML exceeds {} parser events",
                max_events.min(MAX_COLLADA_XML_EVENTS)
            )));
        }
        match reader.read_event_into(&mut buffer)? {
            Event::Start(element) => {
                let qualified = element.name();
                let name = local_name(qualified.as_ref()).to_vec();
                if stack.len() >= MAX_COLLADA_XML_DEPTH {
                    return Err(Error::LimitExceeded(format!(
                        "COLLADA XML nesting exceeds {MAX_COLLADA_XML_DEPTH}"
                    )));
                }
                match name.as_slice() {
                    b"source" => {
                        current_source = attribute(&element, b"id");
                        current_source_text.clear();
                        current_source_stride = 3;
                    }
                    b"accessor" if current_source.is_some() => {
                        current_source_stride = attribute(&element, b"stride")
                            .and_then(|value| value.parse::<usize>().ok())
                            .filter(|stride| (1..=16).contains(stride))
                            .unwrap_or(3);
                    }
                    b"input" if current_primitive.is_some() => {
                        let semantic = attribute(&element, b"semantic").unwrap_or_default();
                        let source = attribute(&element, b"source")
                            .unwrap_or_default()
                            .trim_start_matches('#')
                            .to_owned();
                        let offset = attribute(&element, b"offset")
                            .and_then(|value| value.parse::<usize>().ok())
                            .unwrap_or(0);
                        if let Some(primitive) = current_primitive.as_mut() {
                            primitive.inputs.push((semantic, source, offset));
                        }
                    }
                    b"triangles" | b"polylist" => {
                        current_primitive = Some(Primitive {
                            kind: String::from_utf8_lossy(&name).into_owned(),
                            ..Default::default()
                        });
                    }
                    b"p" | b"vcount" if current_primitive.is_some() => current_text.clear(),
                    b"instance_controller" | b"animation" | b"controller" => push_warning_once(
                        &mut warnings,
                        "COLLADA controllers and animations are omitted",
                    ),
                    b"node" | b"translate" | b"rotate" | b"scale" | b"matrix" => push_warning_once(
                        &mut warnings,
                        "COLLADA node transforms are ignored; geometry is rendered in source coordinates",
                    ),
                    _ => {}
                }
                stack.push(name);
            }
            Event::Empty(element) => {
                let qualified = element.name();
                let name = local_name(qualified.as_ref());
                if name == b"input" && current_primitive.is_some() {
                    let semantic = attribute(&element, b"semantic").unwrap_or_default();
                    let source = attribute(&element, b"source")
                        .unwrap_or_default()
                        .trim_start_matches('#')
                        .to_owned();
                    let offset = attribute(&element, b"offset")
                        .and_then(|value| value.parse::<usize>().ok())
                        .unwrap_or(0);
                    if let Some(primitive) = current_primitive.as_mut() {
                        primitive.inputs.push((semantic, source, offset));
                    }
                }
            }
            Event::Text(text) => {
                if current_source.is_some() || (current_primitive.is_some() && !stack.is_empty()) {
                    current_text.push_str(&text.decode().map_err(|error| {
                        Error::InvalidInput(format!("invalid COLLADA text: {error}"))
                    })?);
                }
            }
            Event::CData(text) => {
                current_text.push_str(&text.decode().map_err(|error| {
                    Error::InvalidInput(format!("invalid COLLADA CDATA: {error}"))
                })?)
            }
            Event::End(element) => {
                let qualified = element.name();
                let name = local_name(qualified.as_ref());
                match name {
                    b"float_array" if current_source.is_some() => {
                        current_source_text.push_str(&current_text);
                        current_text.clear();
                    }
                    b"source" => {
                        if let Some(id) = current_source.take() {
                            if sources.len() >= MAX_COLLADA_SOURCES {
                                return Err(Error::LimitExceeded(format!(
                                    "COLLADA exceeds {MAX_COLLADA_SOURCES} sources"
                                )));
                            }
                            let values = parse_values(&current_source_text)?;
                            if values.len() > MAX_COLLADA_VALUES {
                                return Err(Error::LimitExceeded(format!(
                                    "COLLADA source exceeds {MAX_COLLADA_VALUES} values"
                                )));
                            }
                            sources.insert(
                                id,
                                SourceData {
                                    values,
                                    stride: current_source_stride,
                                },
                            );
                            current_source_text.clear();
                        }
                    }
                    b"vcount" if current_primitive.is_some() => {
                        if let Some(primitive) = current_primitive.as_mut() {
                            primitive.counts = current_text
                                .split_whitespace()
                                .filter_map(|value| value.parse::<usize>().ok())
                                .collect();
                        }
                        current_text.clear();
                    }
                    b"p" if current_primitive.is_some() => {
                        if let Some(primitive) = current_primitive.as_mut() {
                            primitive.indices = current_text
                                .split_whitespace()
                                .filter_map(|value| value.parse::<usize>().ok())
                                .collect();
                        }
                        current_text.clear();
                    }
                    b"triangles" | b"polylist" => {
                        if let Some(primitive) = current_primitive.take() {
                            primitives.push(primitive);
                        }
                    }
                    _ => {}
                }
                stack.pop();
            }
            Event::DocType(_) => push_warning_once(
                &mut warnings,
                "COLLADA DTD declaration was ignored; external entities were not loaded",
            ),
            Event::Eof => break,
            _ => {}
        }
        buffer.clear();
    }

    // The POSITION input is declared inside <vertices>; collect it in a second
    // lightweight pass so primitive parsing remains streaming and bounded.
    let vertices_map = parse_vertices_map(bytes, max_events)?;
    let mut obj = String::new();
    let mut vertex_count = 0usize;
    let mut face_count = 0usize;
    for primitive in primitives {
        let Some((vertex_source, vertex_offset)) = primitive
            .inputs
            .iter()
            .find(|(semantic, _, _)| semantic.eq_ignore_ascii_case("VERTEX"))
            .and_then(|(_, source, offset)| {
                vertices_map
                    .get(source)
                    .map(|position| (position.clone(), *offset))
            })
        else {
            push_warning_once(
                &mut warnings,
                "COLLADA primitive without a VERTEX/POSITION input was omitted",
            );
            continue;
        };
        let Some(source) = sources.get(&vertex_source) else {
            push_warning_once(
                &mut warnings,
                "COLLADA primitive references a missing position source",
            );
            continue;
        };
        let stride = primitive
            .inputs
            .iter()
            .map(|(_, _, offset)| offset.saturating_add(1))
            .max()
            .unwrap_or(1);
        let position_stride = source.stride.max(3);
        let mut local_indices = Vec::new();
        let mut cursor = 0usize;
        let polygon_counts = if primitive.kind == "polylist" {
            primitive.counts
        } else {
            vec![3; primitive.indices.len().checked_div(stride).unwrap_or(0) / 3]
        };
        for count in polygon_counts {
            if count < 3
                || cursor.saturating_add(count.saturating_mul(stride)) > primitive.indices.len()
            {
                push_warning_once(
                    &mut warnings,
                    "COLLADA primitive had an incomplete polygon and was omitted",
                );
                break;
            }
            for index in 0..count {
                let raw = primitive.indices[cursor + index * stride + vertex_offset];
                let base = raw.checked_mul(position_stride).ok_or_else(|| {
                    Error::LimitExceeded("COLLADA position index overflowed".into())
                })?;
                if base + 2 >= source.values.len() {
                    return Err(Error::InvalidInput(
                        "COLLADA position index is out of range".into(),
                    ));
                }
                obj.push_str(&format!(
                    "v {} {} {}\n",
                    source.values[base],
                    source.values[base + 1],
                    source.values[base + 2]
                ));
                vertex_count = vertex_count.saturating_add(1);
                local_indices.push(vertex_count);
                if vertex_count > MAX_COLLADA_VERTICES {
                    return Err(Error::LimitExceeded(format!(
                        "COLLADA exceeds {MAX_COLLADA_VERTICES} generated vertices"
                    )));
                }
            }
            // The polygon vector above is populated through local_indices; use
            // a fresh slice so fan triangulation is deterministic.
            let start = local_indices.len().saturating_sub(count);
            let indices = &local_indices[start..];
            for fan in 1..indices.len().saturating_sub(1) {
                obj.push_str(&format!(
                    "f {} {} {}\n",
                    indices[0],
                    indices[fan],
                    indices[fan + 1]
                ));
                face_count = face_count.saturating_add(1);
                if face_count > MAX_COLLADA_FACES {
                    return Err(Error::LimitExceeded(format!(
                        "COLLADA exceeds {MAX_COLLADA_FACES} generated faces"
                    )));
                }
            }
            cursor = cursor.saturating_add(count * stride);
        }
    }
    if face_count == 0 {
        return Err(Error::InvalidInput(
            "COLLADA contains no supported triangle or polygon faces".into(),
        ));
    }
    push_warning_once(
        &mut warnings,
        "COLLADA materials, animations, controllers, and node transforms are not reconstructed",
    );
    Ok((obj, warnings))
}

fn parse_vertices_map(bytes: &[u8], max_events: usize) -> Result<HashMap<String, String>> {
    let mut reader = Reader::from_reader(Cursor::new(bytes));
    reader.config_mut().trim_text(true);
    let mut buffer = Vec::new();
    let mut current = None::<String>;
    let mut map = HashMap::new();
    let mut events = 0usize;
    loop {
        events = events.saturating_add(1);
        if events > max_events.min(MAX_COLLADA_XML_EVENTS) {
            return Err(Error::LimitExceeded(format!(
                "COLLADA XML exceeds {} parser events",
                max_events.min(MAX_COLLADA_XML_EVENTS)
            )));
        }
        match reader.read_event_into(&mut buffer)? {
            Event::Start(element) => {
                let qualified = element.name();
                let name = local_name(qualified.as_ref());
                if name == b"vertices" {
                    current = attribute(&element, b"id");
                } else if name == b"input"
                    && current.is_some()
                    && attribute(&element, b"semantic").as_deref() == Some("POSITION")
                    && let Some(source) = attribute(&element, b"source")
                {
                    map.insert(
                        current.clone().expect("vertices id"),
                        source.trim_start_matches('#').to_owned(),
                    );
                }
            }
            Event::End(element) => {
                let qualified = element.name();
                if local_name(qualified.as_ref()) == b"vertices" {
                    current = None;
                }
            }
            Event::Empty(element) => {
                let qualified = element.name();
                if local_name(qualified.as_ref()) == b"input"
                    && current.is_some()
                    && attribute(&element, b"semantic").as_deref() == Some("POSITION")
                    && let Some(source) = attribute(&element, b"source")
                {
                    map.insert(
                        current.clone().expect("vertices id"),
                        source.trim_start_matches('#').to_owned(),
                    );
                }
            }
            Event::Eof => break,
            _ => {}
        }
        buffer.clear();
    }
    Ok(map)
}

fn parse_values(text: &str) -> Result<Vec<f64>> {
    text.split_whitespace()
        .map(|value| {
            value
                .parse::<f64>()
                .ok()
                .filter(|value| value.is_finite())
                .ok_or_else(|| {
                    Error::InvalidInput("COLLADA float_array contains an invalid number".into())
                })
        })
        .collect()
}

fn push_warning_once(warnings: &mut Vec<String>, warning: &str) {
    if !warnings.iter().any(|existing| existing == warning) {
        warnings.push(warning.to_owned());
    }
}

fn dedup_warnings(warnings: &mut Vec<String>) {
    warnings.sort();
    warnings.dedup();
}
