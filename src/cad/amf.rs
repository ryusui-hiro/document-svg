//! Bounded ISO/ASTM 52915 Additive Manufacturing File Format (AMF) preview.
//!
//! AMF is an XML mesh format. This adapter extracts object-local vertices and
//! volume triangles into an in-memory OBJ stream for the existing shaded mesh
//! renderer. Materials, textures, metadata, lattices, slices and external
//! references remain inert.

use std::fmt::Write as _;
use std::io::Cursor;
use std::path::Path;

use crate::convert::{ConvertOptions, PageConsumer, read_limited_file};
use crate::error::{Error, Result};
use crate::geospatial::xml_tree::{XmlElement, XmlLimits, parse_xml_tree};

const MAX_AMF_INPUT_BYTES: u64 = 128 * 1024 * 1024;
const MAX_AMF_EVENTS: usize = 500_000;
const MAX_AMF_NODES: usize = 300_000;
const MAX_AMF_DEPTH: usize = 96;
const MAX_AMF_TEXT_BYTES: usize = 24 * 1024 * 1024;
const MAX_AMF_VERTICES: usize = 2_000_000;
const MAX_AMF_FACES: usize = 200_000;
const MAX_AMF_OBJECTS: usize = 100_000;

struct AmfWarningSink<'a> {
    inner: &'a mut dyn PageConsumer,
    warnings: &'a [String],
}

impl PageConsumer for AmfWarningSink<'_> {
    fn consume(&mut self, mut page: crate::ir::Page) -> Result<()> {
        page.source_format = "amf".into();
        if page.title.is_empty() {
            page.title = "AMF mesh".into();
        }
        page.description = "AMF mesh geometry is rendered as a bounded shaded preview; materials, textures and manufacturing metadata are inert".into();
        for warning in self.warnings {
            page.warn(warning.clone());
        }
        self.inner.consume(page)
    }
}

pub(crate) fn looks_like_prefix(bytes: &[u8]) -> bool {
    let text = String::from_utf8_lossy(bytes).to_ascii_lowercase();
    text.contains("<amf") && (text.contains("<object") || text.contains("<mesh"))
}

pub(crate) fn convert(
    path: &Path,
    options: &ConvertOptions,
    sink: &mut dyn PageConsumer,
) -> Result<Vec<String>> {
    let bytes = read_limited_file(
        path,
        options.max_input_bytes.min(MAX_AMF_INPUT_BYTES),
        "AMF input",
    )?;
    convert_bytes(&bytes, options, sink)
}

pub(crate) fn convert_bytes(
    bytes: &[u8],
    options: &ConvertOptions,
    sink: &mut dyn PageConsumer,
) -> Result<Vec<String>> {
    let root = parse_xml_tree(
        bytes,
        &XmlLimits {
            max_events: options.max_xml_events.min(MAX_AMF_EVENTS),
            max_nodes: MAX_AMF_NODES,
            max_depth: MAX_AMF_DEPTH,
            max_text_bytes: MAX_AMF_TEXT_BYTES,
        },
        "AMF",
    )?;
    if !root.name.eq_ignore_ascii_case("amf") {
        return Err(Error::InvalidInput(
            "AMF document must have an amf root".into(),
        ));
    }
    let mut obj = String::new();
    let mut total_vertices = 0usize;
    let mut total_faces = 0usize;
    let objects = descendants_named(&root, "object");
    if objects.len() > MAX_AMF_OBJECTS {
        return Err(Error::LimitExceeded(format!(
            "AMF objects exceed {MAX_AMF_OBJECTS}"
        )));
    }
    for (object_index, object) in objects.iter().enumerate() {
        let vertices = descendants_named(object, "vertex");
        let base_index = total_vertices;
        for vertex in &vertices {
            let Some(coordinates) = descendants_named(vertex, "coordinates").first().copied()
            else {
                continue;
            };
            let x = first_number(coordinates, "x")?;
            let y = first_number(coordinates, "y")?;
            let z = first_number(coordinates, "z")?;
            writeln!(&mut obj, "v {x} {y} {z}")
                .map_err(|_| Error::InvalidInput("AMF OBJ buffer write failed".into()))?;
            total_vertices = total_vertices.saturating_add(1);
            if total_vertices > MAX_AMF_VERTICES {
                return Err(Error::LimitExceeded(format!(
                    "AMF vertices exceed {MAX_AMF_VERTICES}"
                )));
            }
        }
        let label = attr_local(object, "id").unwrap_or("object");
        writeln!(&mut obj, "o amf_{}_{}", sanitize_label(label), object_index)
            .map_err(|_| Error::InvalidInput("AMF OBJ buffer write failed".into()))?;
        for volume in descendants_named(object, "volume") {
            for triangle in descendants_named(volume, "triangle") {
                let v1 = first_index(triangle, "v1")?;
                let v2 = first_index(triangle, "v2")?;
                let v3 = first_index(triangle, "v3")?;
                if v1 >= vertices.len() || v2 >= vertices.len() || v3 >= vertices.len() {
                    return Err(Error::InvalidInput(
                        "AMF triangle references a vertex outside its object".into(),
                    ));
                }
                writeln!(
                    &mut obj,
                    "f {} {} {}",
                    base_index + v1 + 1,
                    base_index + v2 + 1,
                    base_index + v3 + 1
                )
                .map_err(|_| Error::InvalidInput("AMF OBJ buffer write failed".into()))?;
                total_faces = total_faces.saturating_add(1);
                if total_faces > MAX_AMF_FACES {
                    return Err(Error::LimitExceeded(format!(
                        "AMF triangles exceed {MAX_AMF_FACES}"
                    )));
                }
            }
        }
    }
    if total_faces == 0 {
        return Err(Error::InvalidInput(
            "AMF contains no renderable volume triangles".into(),
        ));
    }
    let unit = attr_local(&root, "unit").unwrap_or("millimeter");
    let mut warnings = vec![
        format!(
            "AMF object/triangle mesh rendered as a shaded preview; declared unit {unit:?} is retained as metadata and not converted"
        ),
        "AMF materials, textures, metadata, lattices, slices, color, and external references are omitted; no manufacturing operation runs".into(),
    ];
    if !descendants_named(&root, "material").is_empty()
        || !descendants_named(&root, "texture").is_empty()
    {
        warnings.push(
            "AMF material/texture definitions were present but not applied to the shaded preview"
                .into(),
        );
    }
    let mut warning_sink = AmfWarningSink {
        inner: sink,
        warnings: &warnings,
    };
    let obj_warnings = crate::cad::obj::convert(Cursor::new(obj), options, &mut warning_sink)?;
    warnings.extend(obj_warnings);
    warnings.sort();
    warnings.dedup();
    Ok(warnings)
}

fn first_number(element: &XmlElement, name: &str) -> Result<f64> {
    let text = descendants_named(element, name)
        .first()
        .map(|node| node.text.trim())
        .ok_or_else(|| Error::InvalidInput(format!("AMF coordinates missing {name}")))?;
    text.parse::<f64>()
        .map_err(|error| Error::InvalidInput(format!("invalid AMF coordinate {name}: {error}")))
}

fn first_index(element: &XmlElement, name: &str) -> Result<usize> {
    let text = descendants_named(element, name)
        .first()
        .map(|node| node.text.trim())
        .ok_or_else(|| Error::InvalidInput(format!("AMF triangle missing {name}")))?;
    text.parse::<usize>()
        .map_err(|error| Error::InvalidInput(format!("invalid AMF triangle index {name}: {error}")))
}

fn attr_local<'a>(element: &'a XmlElement, name: &str) -> Option<&'a str> {
    element.attributes.iter().find_map(|(key, value)| {
        key.rsplit(':')
            .next()
            .filter(|local| local.eq_ignore_ascii_case(name))
            .map(|_| value.as_str())
    })
}

fn descendants_named<'a>(element: &'a XmlElement, name: &str) -> Vec<&'a XmlElement> {
    let mut result = Vec::new();
    for child in &element.children {
        if child.name.eq_ignore_ascii_case(name) {
            result.push(child);
        }
        result.extend(descendants_named(child, name));
    }
    result
}

fn sanitize_label(label: &str) -> String {
    let mut result = String::new();
    for character in label.chars().take(128) {
        if character.is_ascii_alphanumeric() || character == '_' || character == '-' {
            result.push(character);
        } else {
            result.push('_');
        }
    }
    if result.is_empty() {
        "object".into()
    } else {
        result
    }
}
