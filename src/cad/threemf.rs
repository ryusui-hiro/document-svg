//! 3D Manufacturing Format (.3mf) parser and Lambertian-shaded vector polygon renderer.
//!
//! Reads the primary 3MF ZIP model part, expands selected build items and component
//! transforms, converts declared units to millimeters, and renders a shaded isometric SVG.

use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::Read;
use std::path::Path;

use quick_xml::Reader;
use quick_xml::events::Event;
use zip::ZipArchive;

use crate::convert::{ConvertOptions, PageConsumer};
use crate::error::{Error, Result};
use crate::ir::{IDENTITY, LineCap, LineJoin, Node, Page, Paint, SourceMeta, Stroke};
use crate::ooxml::{attribute, local_name};

const TARGET_PAGE_LONG_EDGE: f64 = 1200.0;
const MIN_PAGE_DIMENSION: f64 = 400.0;
const MAX_3MF_RELATIONSHIP_BYTES: u64 = 4 * 1024 * 1024;
const MAX_3MF_ARCHIVE_ENTRIES: usize = 100_000;
const MAX_3MF_VERTICES: usize = 1_000_000;
const MAX_3MF_FACES: usize = 200_000;
const MAX_3MF_OBJECTS: usize = 100_000;
const MAX_3MF_COMPONENTS: usize = 100_000;
const MAX_3MF_BUILD_ITEMS: usize = 100_000;
const MAX_3MF_EXPANDED_VERTICES: usize = 1_000_000;
const MAX_3MF_EXPANDED_FACES: usize = 200_000;
const MAX_3MF_EXPANSION_VISITS: usize = 1_000_000;
const MAX_3MF_EXPANSION_DEPTH: usize = 64;
const MAX_3MF_COORDINATE: f64 = 1.0e12;

#[derive(Clone, Copy, Debug)]
struct Vec3 {
    x: f64,
    y: f64,
    z: f64,
}

impl Vec3 {
    fn dot(&self, other: &Vec3) -> f64 {
        self.x * other.x + self.y * other.y + self.z * other.z
    }

    fn sub(&self, other: &Vec3) -> Vec3 {
        Vec3 {
            x: self.x - other.x,
            y: self.y - other.y,
            z: self.z - other.z,
        }
    }

    fn cross(&self, other: &Vec3) -> Vec3 {
        Vec3 {
            x: self.y * other.z - self.z * other.y,
            y: self.z * other.x - self.x * other.z,
            z: self.x * other.y - self.y * other.x,
        }
    }

    fn normalize(&self) -> Vec3 {
        let len = (self.x * self.x + self.y * self.y + self.z * self.z).sqrt();
        if len > 1e-9 {
            Vec3 {
                x: self.x / len,
                y: self.y / len,
                z: self.z / len,
            }
        } else {
            Vec3 {
                x: 0.0,
                y: 0.0,
                z: 1.0,
            }
        }
    }
}

#[derive(Clone, Debug)]
struct TriangleFace {
    v0: usize,
    v1: usize,
    v2: usize,
    normal: Vec3,
    center_depth: f64,
}

#[derive(Clone, Copy, Debug)]
struct Transform3D([f64; 12]);

impl Transform3D {
    const IDENTITY: Self = Self([1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0]);

    fn apply(self, point: Vec3) -> Vec3 {
        let m = self.0;
        Vec3 {
            x: point.x * m[0] + point.y * m[3] + point.z * m[6] + m[9],
            y: point.x * m[1] + point.y * m[4] + point.z * m[7] + m[10],
            z: point.x * m[2] + point.y * m[5] + point.z * m[8] + m[11],
        }
    }

    // 3MF stores row-major matrices for row-vector application. `self` is applied
    // first and `parent` second, so a child component uses child * parent.
    fn then(self, parent: Self) -> Self {
        let mut result = [0.0; 12];
        for row in 0..3 {
            for column in 0..3 {
                result[row * 3 + column] = (0..3)
                    .map(|inner| self.0[row * 3 + inner] * parent.0[inner * 3 + column])
                    .sum();
            }
        }
        for column in 0..3 {
            result[9 + column] = (0..3)
                .map(|inner| self.0[9 + inner] * parent.0[inner * 3 + column])
                .sum::<f64>()
                + parent.0[9 + column];
        }
        Self(result)
    }

    fn determinant(self) -> f64 {
        let m = self.0;
        m[0] * (m[4] * m[8] - m[5] * m[7]) - m[1] * (m[3] * m[8] - m[5] * m[6])
            + m[2] * (m[3] * m[7] - m[4] * m[6])
    }
}

#[derive(Default)]
struct ThreeMfObject {
    object_type: String,
    vertices: Vec<Vec3>,
    faces: Vec<[usize; 3]>,
    components: Vec<ThreeMfComponent>,
}

#[derive(Clone, Copy)]
struct ThreeMfComponent {
    object_id: u32,
    transform: Transform3D,
}

#[derive(Clone, Copy)]
struct ThreeMfBuildItem {
    object_id: u32,
    transform: Transform3D,
}

pub(crate) fn convert(
    path: &Path,
    options: &ConvertOptions,
    sink: &mut dyn PageConsumer,
) -> Result<Vec<String>> {
    let file = File::open(path)?;
    let mut archive = ZipArchive::new(file)
        .map_err(|e| Error::InvalidInput(format!("failed to open 3MF zip archive: {e}")))?;
    if archive.len() > MAX_3MF_ARCHIVE_ENTRIES {
        return Err(Error::LimitExceeded(format!(
            "3MF package exceeds {MAX_3MF_ARCHIVE_ENTRIES} archive entries"
        )));
    }

    let model_path = locate_model_part(&mut archive, options.max_xml_events)?;
    let mut model_part = archive.by_name(&model_path).map_err(|error| {
        Error::InvalidInput(format!("3MF model part '{model_path}' is missing: {error}"))
    })?;
    let model_size = model_part.size();
    if model_size > options.max_zip_entry_bytes {
        return Err(Error::LimitExceeded(format!(
            "3MF model exceeds maximum entry size of {} bytes",
            options.max_zip_entry_bytes
        )));
    }
    let model_bytes = read_entry_limited(
        &mut model_part,
        &model_path,
        options.max_zip_entry_bytes,
        model_size,
    )?;
    let model_xml = String::from_utf8(model_bytes)
        .map_err(|error| Error::InvalidInput(format!("3MF model XML is not UTF-8: {error}")))?;

    let (vertices, mut faces, warnings) = parse_3mf_model(&model_xml, options.max_xml_events)?;
    if vertices.is_empty() || faces.is_empty() {
        return Err(Error::InvalidInput(
            "3MF model contains no vertices or faces".into(),
        ));
    }

    // Camera transform: isometric projection (X=35.264°, Y=45°)
    let cos_y = (std::f64::consts::PI / 4.0).cos();
    let sin_y = (std::f64::consts::PI / 4.0).sin();
    let angle_x = (35.264f64).to_radians();
    let cos_x = angle_x.cos();
    let sin_x = angle_x.sin();

    let project_point = |p: Vec3| -> Vec3 {
        let x1 = p.x * cos_y + p.z * sin_y;
        let y1 = p.y;
        let z1 = -p.x * sin_y + p.z * cos_y;

        let x2 = x1;
        let y2 = y1 * cos_x - z1 * sin_x;
        let z2 = y1 * sin_x + z1 * cos_x;
        Vec3 {
            x: x2,
            y: -y2,
            z: z2,
        }
    };

    let projected_vertices: Vec<Vec3> = vertices.iter().map(|&v| project_point(v)).collect();

    // Compute bounds
    let mut min_x = f64::INFINITY;
    let mut max_x = f64::NEG_INFINITY;
    let mut min_y = f64::INFINITY;
    let mut max_y = f64::NEG_INFINITY;

    for p in &projected_vertices {
        min_x = min_x.min(p.x);
        max_x = max_x.max(p.x);
        min_y = min_y.min(p.y);
        max_y = max_y.max(p.y);
    }

    let dx = (max_x - min_x).max(1e-4);
    let dy = (max_y - min_y).max(1e-4);
    let scale = (TARGET_PAGE_LONG_EDGE - 100.0) / dx.max(dy);
    let margin = 50.0;
    let width = (dx * scale + margin * 2.0).max(MIN_PAGE_DIMENSION);
    let height = (dy * scale + margin * 2.0).max(MIN_PAGE_DIMENSION);

    let light_dir = Vec3 {
        x: 0.5,
        y: -0.8,
        z: 0.6,
    }
    .normalize();
    let ambient = 0.25;

    for face in &mut faces {
        if face.v0 < vertices.len() && face.v1 < vertices.len() && face.v2 < vertices.len() {
            let v0 = vertices[face.v0];
            let v1 = vertices[face.v1];
            let v2 = vertices[face.v2];
            let edge1 = v1.sub(&v0);
            let edge2 = v2.sub(&v0);
            face.normal = edge1.cross(&edge2).normalize();

            let p0 = projected_vertices[face.v0];
            let p1 = projected_vertices[face.v1];
            let p2 = projected_vertices[face.v2];
            face.center_depth = (p0.z + p1.z + p2.z) / 3.0;
        }
    }

    // Sort faces by depth
    faces.sort_by(|a, b| {
        a.center_depth
            .partial_cmp(&b.center_depth)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    let mut page = Page::new(1, width, height, "3mf-model");

    for (i, face) in faces.iter().enumerate() {
        if face.v0 >= projected_vertices.len()
            || face.v1 >= projected_vertices.len()
            || face.v2 >= projected_vertices.len()
        {
            continue;
        }
        let p0 = projected_vertices[face.v0];
        let p1 = projected_vertices[face.v1];
        let p2 = projected_vertices[face.v2];

        let s0 = (
            (p0.x - min_x) * scale + margin,
            (p0.y - min_y) * scale + margin,
        );
        let s1 = (
            (p1.x - min_x) * scale + margin,
            (p1.y - min_y) * scale + margin,
        );
        let s2 = (
            (p2.x - min_x) * scale + margin,
            (p2.y - min_y) * scale + margin,
        );

        let d = format!(
            "M {:.3},{:.3} L {:.3},{:.3} L {:.3},{:.3} Z",
            s0.0, s0.1, s1.0, s1.1, s2.0, s2.1
        );

        let diffuse = face.normal.dot(&light_dir).abs();
        let intensity = (ambient + (1.0 - ambient) * diffuse).clamp(0.0, 1.0);
        let r = (intensity * 100.0) as u8;
        let g = (intensity * 180.0) as u8;
        let b = (intensity * 230.0) as u8;
        let color = format!("#{:02X}{:02X}{:02X}", r, g, b);

        page.nodes.push(Node::Path {
            id: format!("face_{i}"),
            d,
            fill_rule: "nonzero".into(),
            fill: Paint::solid(color),
            stroke: Stroke {
                paint: Paint::solid("#1e293b"),
                width: 0.5,
                line_cap: LineCap::Round,
                line_join: LineJoin::Round,
                ..Default::default()
            },
            transform: IDENTITY,
            clip_id: None,
            meta: SourceMeta {
                kind: "3mf:triangle".into(),
                ..Default::default()
            },
        });
    }

    sink.consume(page)?;
    Ok(warnings)
}

fn parse_3mf_model(
    xml: &str,
    max_xml_events: usize,
) -> Result<(Vec<Vec3>, Vec<TriangleFace>, Vec<String>)> {
    let mut reader = Reader::from_reader(xml.as_bytes());
    reader.config_mut().trim_text(true);
    let mut buf = Vec::new();
    let mut model_seen = false;
    let mut has_recommended_extensions = false;
    let mut unit_scale = 1.0;
    let mut current_object: Option<(u32, ThreeMfObject)> = None;
    let mut mesh_vertex_base = 0usize;
    let mut objects = HashMap::<u32, ThreeMfObject>::new();
    let mut object_order = Vec::new();
    let mut build_items = Vec::<ThreeMfBuildItem>::new();
    let mut build_seen = false;
    let mut in_build = false;
    let mut component_count = 0usize;
    let mut raw_vertex_count = 0usize;
    let mut raw_face_count = 0usize;
    let mut has_property_data = false;
    let mut stack = Vec::<String>::new();
    let mut event_count = 0usize;

    loop {
        event_count = event_count.saturating_add(1);
        if event_count > max_xml_events {
            return Err(Error::LimitExceeded(format!(
                "3MF model exceeds {max_xml_events} XML events"
            )));
        }
        let event = reader.read_event_into(&mut buf)?;
        let is_empty = matches!(&event, Event::Empty(_));
        match event {
            Event::Start(ref e) | Event::Empty(ref e) => {
                let qname = e.name();
                let name = local_name(qname.as_ref());
                if matches!(
                    name,
                    b"basematerials"
                        | b"colorgroup"
                        | b"texture2d"
                        | b"texture2dgroup"
                        | b"composite"
                        | b"multiproperties"
                ) || (name == b"object"
                    && (attribute(e, b"pid").is_some() || attribute(e, b"pindex").is_some()))
                    || (name == b"triangle"
                        && [b"pid".as_slice(), b"p1", b"p2", b"p3", b"p4", b"p5"]
                            .iter()
                            .any(|key| attribute(e, key).is_some()))
                {
                    has_property_data = true;
                }
                match name {
                    b"model" => {
                        if model_seen {
                            return Err(Error::InvalidInput(
                                "3MF model part contains multiple model roots".into(),
                            ));
                        }
                        model_seen = true;
                        unit_scale = parse_3mf_unit(attribute(e, b"unit").as_deref())?;
                        if attribute(e, b"requiredextensions")
                            .is_some_and(|extensions| !extensions.trim().is_empty())
                        {
                            return Err(Error::Unsupported(
                                "3MF model requires extensions that this previewer does not implement".into(),
                            ));
                        }
                        has_recommended_extensions = attribute(e, b"recommendedextensions")
                            .is_some_and(|extensions| !extensions.trim().is_empty());
                    }
                    b"object" => {
                        if current_object.is_some() {
                            return Err(Error::InvalidInput(
                                "3MF object resources cannot be nested".into(),
                            ));
                        }
                        if objects.len() >= MAX_3MF_OBJECTS {
                            return Err(Error::LimitExceeded(format!(
                                "3MF object count exceeds {MAX_3MF_OBJECTS}"
                            )));
                        }
                        let id = required_3mf_resource_id(e, b"id", "object id")?;
                        if objects.contains_key(&id) {
                            return Err(Error::InvalidInput(format!(
                                "3MF resource id {id} is defined more than once"
                            )));
                        }
                        let object_type = attribute(e, b"type").unwrap_or_else(|| "model".into());
                        if !matches!(
                            object_type.as_str(),
                            "model" | "solidsupport" | "support" | "surface" | "other"
                        ) {
                            return Err(Error::InvalidInput(format!(
                                "invalid 3MF object type '{object_type}'"
                            )));
                        }
                        current_object = Some((
                            id,
                            ThreeMfObject {
                                object_type,
                                ..Default::default()
                            },
                        ));
                        mesh_vertex_base = 0;
                    }
                    b"vertices" => {
                        let (_, object) = current_object.as_mut().ok_or_else(|| {
                            Error::InvalidInput("3MF vertices are outside an object".into())
                        })?;
                        mesh_vertex_base = object.vertices.len();
                    }
                    b"vertex" => {
                        let (_, object) = current_object.as_mut().ok_or_else(|| {
                            Error::InvalidInput("3MF vertex is outside an object".into())
                        })?;
                        if raw_vertex_count >= MAX_3MF_VERTICES {
                            return Err(Error::LimitExceeded(format!(
                                "3MF vertex count exceeds {MAX_3MF_VERTICES}"
                            )));
                        }
                        raw_vertex_count += 1;
                        let x = required_f64_attribute(e, b"x", "vertex x")?;
                        let y = required_f64_attribute(e, b"y", "vertex y")?;
                        let z = required_f64_attribute(e, b"z", "vertex z")?;
                        if [x, y, z]
                            .iter()
                            .any(|value| !value.is_finite() || value.abs() > MAX_3MF_COORDINATE)
                        {
                            return Err(Error::InvalidInput(
                                "3MF vertex has a non-finite or out-of-range coordinate".into(),
                            ));
                        }
                        object.vertices.push(Vec3 { x, y, z });
                    }
                    b"triangle" => {
                        let (_, object) = current_object.as_mut().ok_or_else(|| {
                            Error::InvalidInput("3MF triangle is outside an object".into())
                        })?;
                        if raw_face_count >= MAX_3MF_FACES {
                            return Err(Error::LimitExceeded(format!(
                                "3MF triangle count exceeds {MAX_3MF_FACES}"
                            )));
                        }
                        raw_face_count += 1;
                        let local_vertex_count = object
                            .vertices
                            .len()
                            .checked_sub(mesh_vertex_base)
                            .ok_or_else(|| {
                                Error::InvalidInput("invalid 3MF mesh vertex range".into())
                            })?;
                        let v0 =
                            resolve_triangle_index(e, b"v1", mesh_vertex_base, local_vertex_count)?;
                        let v1 =
                            resolve_triangle_index(e, b"v2", mesh_vertex_base, local_vertex_count)?;
                        let v2 =
                            resolve_triangle_index(e, b"v3", mesh_vertex_base, local_vertex_count)?;
                        object.faces.push([v0, v1, v2]);
                    }
                    b"component" => {
                        let (_, object) = current_object.as_mut().ok_or_else(|| {
                            Error::InvalidInput("3MF component is outside an object".into())
                        })?;
                        if component_count >= MAX_3MF_COMPONENTS {
                            return Err(Error::LimitExceeded(format!(
                                "3MF component count exceeds {MAX_3MF_COMPONENTS}"
                            )));
                        }
                        component_count += 1;
                        object.components.push(ThreeMfComponent {
                            object_id: required_3mf_resource_id(
                                e,
                                b"objectid",
                                "component objectid",
                            )?,
                            transform: parse_3mf_transform(attribute(e, b"transform").as_deref())?,
                        });
                    }
                    b"build" => {
                        if build_seen {
                            return Err(Error::InvalidInput(
                                "3MF model contains multiple build sections".into(),
                            ));
                        }
                        build_seen = true;
                        in_build = true;
                    }
                    b"item" if in_build => {
                        if build_items.len() >= MAX_3MF_BUILD_ITEMS {
                            return Err(Error::LimitExceeded(format!(
                                "3MF build item count exceeds {MAX_3MF_BUILD_ITEMS}"
                            )));
                        }
                        build_items.push(ThreeMfBuildItem {
                            object_id: required_3mf_resource_id(
                                e,
                                b"objectid",
                                "build item objectid",
                            )?,
                            transform: parse_3mf_transform(attribute(e, b"transform").as_deref())?,
                        });
                    }
                    _ => {}
                }
                if !is_empty {
                    stack.push(String::from_utf8_lossy(name).into_owned());
                } else if name == b"object" {
                    finish_3mf_object(&mut current_object, &mut objects, &mut object_order)?;
                } else if name == b"build" {
                    in_build = false;
                }
            }
            Event::End(ref e) => {
                let name = String::from_utf8_lossy(local_name(e.name().as_ref())).into_owned();
                if name == "object" {
                    finish_3mf_object(&mut current_object, &mut objects, &mut object_order)?;
                } else if name == "build" {
                    in_build = false;
                }
                if stack.pop().as_deref() != Some(name.as_str()) {
                    return Err(Error::InvalidInput(format!(
                        "mismatched 3MF model end tag '{name}'"
                    )));
                }
            }
            Event::DocType(_) => {
                return Err(Error::InvalidInput(
                    "3MF model document type declarations are not supported".into(),
                ));
            }
            Event::Eof => break,
            _ => {}
        }
        buf.clear();
    }

    if current_object.is_some() || !stack.is_empty() {
        return Err(Error::InvalidInput("incomplete 3MF model XML".into()));
    }
    if !model_seen {
        return Err(Error::InvalidInput(
            "3MF model part has no model root".into(),
        ));
    }

    let mut warnings = Vec::new();
    if has_recommended_extensions {
        warnings.push(
            "3MF model recommends extensions that are not interpreted by this previewer".into(),
        );
    }
    if has_property_data {
        warnings.push("3MF material and property colors are not rendered in this preview".into());
    }
    if !build_seen {
        warnings.push(
            "3MF model has no build section; previewing top-level object resources as a compatibility fallback".into(),
        );
        let referenced = objects
            .values()
            .flat_map(|object| {
                object
                    .components
                    .iter()
                    .map(|component| component.object_id)
            })
            .collect::<HashSet<_>>();
        build_items = object_order
            .iter()
            .copied()
            .filter(|id| !referenced.contains(id))
            .map(|object_id| ThreeMfBuildItem {
                object_id,
                transform: Transform3D::IDENTITY,
            })
            .collect();
        if build_items.is_empty() {
            build_items = object_order
                .iter()
                .copied()
                .map(|object_id| ThreeMfBuildItem {
                    object_id,
                    transform: Transform3D::IDENTITY,
                })
                .collect();
        }
    } else if build_items.is_empty() {
        return Err(Error::InvalidInput(
            "3MF build section contains no items to preview".into(),
        ));
    }

    let mut vertices = Vec::new();
    let mut faces = Vec::new();
    let mut active = HashSet::new();
    let mut visits = 0usize;
    for item in build_items {
        expand_3mf_object(
            item.object_id,
            item.transform,
            unit_scale,
            0,
            &objects,
            &mut active,
            &mut visits,
            &mut vertices,
            &mut faces,
        )?;
    }

    Ok((vertices, faces, warnings))
}

fn finish_3mf_object(
    current: &mut Option<(u32, ThreeMfObject)>,
    objects: &mut HashMap<u32, ThreeMfObject>,
    order: &mut Vec<u32>,
) -> Result<()> {
    let (id, object) = current
        .take()
        .ok_or_else(|| Error::InvalidInput("3MF object close has no open object".into()))?;
    if objects.insert(id, object).is_some() {
        return Err(Error::InvalidInput(format!(
            "3MF resource id {id} is defined more than once"
        )));
    }
    order.push(id);
    Ok(())
}

fn parse_3mf_unit(unit: Option<&str>) -> Result<f64> {
    match unit.unwrap_or("millimeter") {
        "micron" => Ok(0.001),
        "millimeter" => Ok(1.0),
        "centimeter" => Ok(10.0),
        "inch" => Ok(25.4),
        "foot" => Ok(304.8),
        "meter" => Ok(1000.0),
        other => Err(Error::InvalidInput(format!(
            "unsupported 3MF model unit '{other}'"
        ))),
    }
}

fn required_3mf_resource_id(
    element: &quick_xml::events::BytesStart<'_>,
    name: &[u8],
    label: &str,
) -> Result<u32> {
    attribute(element, name)
        .and_then(|value| value.parse::<u32>().ok())
        .ok_or_else(|| Error::InvalidInput(format!("3MF {label} is missing or invalid")))
}

fn parse_3mf_transform(value: Option<&str>) -> Result<Transform3D> {
    let Some(value) = value else {
        return Ok(Transform3D::IDENTITY);
    };
    let values = value
        .split_whitespace()
        .map(|part| part.parse::<f64>())
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(|_| Error::InvalidInput("3MF transform contains an invalid number".into()))?;
    let values: [f64; 12] = values
        .try_into()
        .map_err(|_| Error::InvalidInput("3MF transform must contain exactly 12 numbers".into()))?;
    if values
        .iter()
        .any(|value| !value.is_finite() || value.abs() > MAX_3MF_COORDINATE)
    {
        return Err(Error::InvalidInput(
            "3MF transform contains a non-finite or out-of-range value".into(),
        ));
    }
    Ok(Transform3D(values))
}

#[allow(clippy::too_many_arguments)]
fn expand_3mf_object(
    object_id: u32,
    transform: Transform3D,
    unit_scale: f64,
    depth: usize,
    objects: &HashMap<u32, ThreeMfObject>,
    active: &mut HashSet<u32>,
    visits: &mut usize,
    vertices: &mut Vec<Vec3>,
    faces: &mut Vec<TriangleFace>,
) -> Result<()> {
    if depth > MAX_3MF_EXPANSION_DEPTH {
        return Err(Error::LimitExceeded(format!(
            "3MF component nesting exceeds {MAX_3MF_EXPANSION_DEPTH}"
        )));
    }
    *visits = visits.saturating_add(1);
    if *visits > MAX_3MF_EXPANSION_VISITS {
        return Err(Error::LimitExceeded(format!(
            "3MF object expansion exceeds {MAX_3MF_EXPANSION_VISITS} visits"
        )));
    }
    if !active.insert(object_id) {
        return Err(Error::InvalidInput(format!(
            "3MF component reference cycle includes object {object_id}"
        )));
    }
    let object = objects.get(&object_id).ok_or_else(|| {
        Error::InvalidInput(format!("3MF object reference {object_id} is not defined"))
    })?;
    if object.object_type == "other" {
        return Err(Error::InvalidInput(format!(
            "3MF build or component reference must not target object type 'other' (id {object_id})"
        )));
    }
    let determinant = transform.determinant();
    if !determinant.is_finite() {
        return Err(Error::InvalidInput(
            "3MF composed transform is numerically out of range".into(),
        ));
    }
    let vertex_base = vertices.len();
    if object.vertices.len() > MAX_3MF_EXPANDED_VERTICES.saturating_sub(vertex_base) {
        return Err(Error::LimitExceeded(format!(
            "3MF expanded vertex count exceeds {MAX_3MF_EXPANDED_VERTICES}"
        )));
    }
    for point in &object.vertices {
        let transformed = transform.apply(*point);
        let transformed = Vec3 {
            x: transformed.x * unit_scale,
            y: transformed.y * unit_scale,
            z: transformed.z * unit_scale,
        };
        if [transformed.x, transformed.y, transformed.z]
            .iter()
            .any(|value| !value.is_finite() || value.abs() > MAX_3MF_COORDINATE)
        {
            return Err(Error::InvalidInput(
                "3MF transformed vertex is non-finite or out of range".into(),
            ));
        }
        vertices.push(transformed);
    }
    if object.faces.len() > MAX_3MF_EXPANDED_FACES.saturating_sub(faces.len()) {
        return Err(Error::LimitExceeded(format!(
            "3MF expanded triangle count exceeds {MAX_3MF_EXPANDED_FACES}"
        )));
    }
    let mirrored = determinant < 0.0;
    for [v0, v1, v2] in &object.faces {
        let mut indices = [*v0, *v1, *v2];
        if mirrored {
            indices.swap(1, 2);
        }
        faces.push(TriangleFace {
            v0: vertex_base + indices[0],
            v1: vertex_base + indices[1],
            v2: vertex_base + indices[2],
            normal: Vec3 {
                x: 0.0,
                y: 0.0,
                z: 1.0,
            },
            center_depth: 0.0,
        });
    }
    for component in &object.components {
        expand_3mf_object(
            component.object_id,
            component.transform.then(transform),
            unit_scale,
            depth + 1,
            objects,
            active,
            visits,
            vertices,
            faces,
        )?;
    }
    active.remove(&object_id);
    Ok(())
}

fn locate_model_part(archive: &mut ZipArchive<File>, max_xml_events: usize) -> Result<String> {
    let relationship_bytes = match archive.by_name("_rels/.rels") {
        Ok(mut entry) => {
            let size = entry.size();
            if size > MAX_3MF_RELATIONSHIP_BYTES {
                return Err(Error::LimitExceeded(format!(
                    "3MF root relationships exceed {MAX_3MF_RELATIONSHIP_BYTES} bytes"
                )));
            }
            read_entry_limited(&mut entry, "_rels/.rels", MAX_3MF_RELATIONSHIP_BYTES, size)?
        }
        Err(zip::result::ZipError::FileNotFound) => return Ok("3D/3dmodel.model".into()),
        Err(error) => {
            return Err(Error::InvalidInput(format!(
                "failed to read 3MF root relationships: {error}"
            )));
        }
    };
    Ok(
        read_model_relationship(&relationship_bytes, max_xml_events)?
            .unwrap_or_else(|| "3D/3dmodel.model".into()),
    )
}

fn read_model_relationship(bytes: &[u8], max_xml_events: usize) -> Result<Option<String>> {
    let xml = std::str::from_utf8(bytes).map_err(|error| {
        Error::InvalidInput(format!("3MF root relationships are not UTF-8: {error}"))
    })?;
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(true);
    let mut buf = Vec::new();
    let mut event_count = 0usize;

    loop {
        event_count = event_count.saturating_add(1);
        if event_count > max_xml_events {
            return Err(Error::LimitExceeded(format!(
                "3MF root relationships exceed {max_xml_events} XML events"
            )));
        }
        match reader.read_event_into(&mut buf)? {
            Event::Start(ref e) | Event::Empty(ref e)
                if local_name(e.name().as_ref()) == b"Relationship" =>
            {
                let relationship_type = attribute(e, b"Type").unwrap_or_default();
                if relationship_type.ends_with("/3dmodel") {
                    if attribute(e, b"TargetMode")
                        .is_some_and(|mode| mode.eq_ignore_ascii_case("External"))
                    {
                        return Err(Error::InvalidInput(
                            "3MF start-model relationship must be package-internal".into(),
                        ));
                    }
                    let target = attribute(e, b"Target").ok_or_else(|| {
                        Error::InvalidInput("3MF start-model relationship has no target".into())
                    })?;
                    return normalize_3mf_target(&target).map(Some);
                }
            }
            Event::Eof => break,
            _ => {}
        }
        buf.clear();
    }
    Ok(None)
}

fn normalize_3mf_target(target: &str) -> Result<String> {
    if target.contains(['?', '#', ':']) {
        return Err(Error::InvalidInput(
            "3MF start-model target must be a package part path".into(),
        ));
    }
    let normalized = target.trim_start_matches('/').replace('\\', "/");
    let mut components = Vec::new();
    for component in normalized.split('/') {
        match component {
            "" | "." => {}
            ".." => {
                if components.pop().is_none() {
                    return Err(Error::InvalidInput(
                        "3MF start-model target escapes the package root".into(),
                    ));
                }
            }
            value => components.push(value),
        }
    }
    if components.is_empty() {
        return Err(Error::InvalidInput(
            "3MF start-model target is empty".into(),
        ));
    }
    Ok(components.join("/"))
}

fn read_entry_limited<R: Read>(
    reader: R,
    name: &str,
    limit: u64,
    size_hint: u64,
) -> Result<Vec<u8>> {
    let capacity = usize::try_from(size_hint.min(limit).min(8 * 1024 * 1024)).unwrap_or(0);
    let mut bytes = Vec::with_capacity(capacity);
    reader
        .take(limit.saturating_add(1))
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > limit {
        return Err(Error::LimitExceeded(format!(
            "3MF ZIP entry {name} expanded beyond {limit} bytes"
        )));
    }
    Ok(bytes)
}

fn required_f64_attribute(
    element: &quick_xml::events::BytesStart<'_>,
    name: &[u8],
    label: &str,
) -> Result<f64> {
    attribute(element, name)
        .and_then(|value| value.parse::<f64>().ok())
        .ok_or_else(|| Error::InvalidInput(format!("3MF {label} is missing or invalid")))
}

fn resolve_triangle_index(
    element: &quick_xml::events::BytesStart<'_>,
    name: &[u8],
    vertex_base: usize,
    local_vertex_count: usize,
) -> Result<usize> {
    let value = attribute(element, name)
        .and_then(|value| value.parse::<usize>().ok())
        .ok_or_else(|| Error::InvalidInput("3MF triangle has a missing or invalid index".into()))?;
    if value >= local_vertex_count {
        return Err(Error::InvalidInput(format!(
            "3MF triangle refers to vertex {value}, but its mesh has {local_vertex_count} vertices"
        )));
    }
    vertex_base
        .checked_add(value)
        .ok_or_else(|| Error::LimitExceeded("3MF vertex index overflowed".into()))
}

/// Converts SVG shapes into a 3D Manufacturing Format (.3mf) model package.
pub fn write_svg_to_threemf<W: std::io::Write + std::io::Seek>(
    svg_content: &str,
    writer: W,
) -> Result<()> {
    let doc = crate::cad::svg_reader::parse_svg_elements(svg_content)?;

    let mut contours: Vec<Vec<crate::cad::svg_reader::Point2D>> = Vec::new();

    for elem in doc.elements {
        match elem {
            crate::cad::svg_reader::SvgElement::Rect {
                x,
                y,
                width,
                height,
                ..
            } => {
                contours.push(vec![
                    crate::cad::svg_reader::Point2D::new(x, y),
                    crate::cad::svg_reader::Point2D::new(x + width, y),
                    crate::cad::svg_reader::Point2D::new(x + width, y + height),
                    crate::cad::svg_reader::Point2D::new(x, y + height),
                ]);
            }
            crate::cad::svg_reader::SvgElement::Circle { center, radius, .. } => {
                let segments = crate::cad::DEFAULT_CIRCLE_SEGMENTS;
                let mut pts = Vec::with_capacity(segments);
                for i in 0..segments {
                    let theta = (i as f64) * std::f64::consts::TAU / (segments as f64);
                    pts.push(crate::cad::svg_reader::Point2D::new(
                        center.x + radius * theta.cos(),
                        center.y + radius * theta.sin(),
                    ));
                }
                contours.push(pts);
            }
            crate::cad::svg_reader::SvgElement::Polyline { points, .. } => {
                if points.len() >= 3 {
                    contours.push(points);
                }
            }
            _ => {}
        }
    }

    if contours.is_empty() {
        contours.push(vec![
            crate::cad::svg_reader::Point2D::new(0.0, 0.0),
            crate::cad::svg_reader::Point2D::new(100.0, 0.0),
            crate::cad::svg_reader::Point2D::new(100.0, 100.0),
            crate::cad::svg_reader::Point2D::new(0.0, 100.0),
        ]);
    }

    let extrusion_height = 20.0;
    let mut vertices: Vec<Vec3> = Vec::new();
    let mut triangles: Vec<(usize, usize, usize)> = Vec::new();

    for contour in contours {
        let n = contour.len();
        if n < 3 {
            continue;
        }
        let base_idx = vertices.len();

        // Bottom vertices (z = 0)
        for pt in &contour {
            vertices.push(Vec3 {
                x: pt.x,
                y: pt.y,
                z: 0.0,
            });
        }
        // Top vertices (z = extrusion_height)
        for pt in &contour {
            vertices.push(Vec3 {
                x: pt.x,
                y: pt.y,
                z: extrusion_height,
            });
        }

        // Bottom face (fan triangulation, normal pointing down)
        for i in 1..n - 1 {
            triangles.push((base_idx, base_idx + i + 1, base_idx + i));
        }

        // Top face (fan triangulation, normal pointing up)
        for i in 1..n - 1 {
            triangles.push((base_idx + n, base_idx + n + i, base_idx + n + i + 1));
        }

        // Side faces (2 triangles per segment)
        for i in 0..n {
            let next = (i + 1) % n;
            let b0 = base_idx + i;
            let b1 = base_idx + next;
            let t0 = base_idx + n + i;
            let t1 = base_idx + n + next;

            triangles.push((b0, b1, t1));
            triangles.push((b0, t1, t0));
        }
    }

    // Build 3D/3dmodel.model XML
    let mut model_xml = String::new();
    model_xml.push_str(r#"<?xml version="1.0" encoding="UTF-8"?>
<model unit="millimeter" xml:lang="en-US" xmlns="http://schemas.microsoft.com/3dmanufacturing/core/2015/02">
  <resources>
    <object id="1" type="model">
      <mesh>
        <vertices>
"#);
    for v in &vertices {
        model_xml.push_str(&format!(
            r#"          <vertex x="{:.4}" y="{:.4}" z="{:.4}"/>"#,
            v.x, v.y, v.z
        ));
        model_xml.push('\n');
    }
    model_xml.push_str(
        r#"        </vertices>
        <triangles>
"#,
    );
    for (v1, v2, v3) in &triangles {
        model_xml.push_str(&format!(
            r#"          <triangle v1="{v1}" v2="{v2}" v3="{v3}"/>"#
        ));
        model_xml.push('\n');
    }
    model_xml.push_str(
        r#"        </triangles>
      </mesh>
    </object>
  </resources>
  <build>
    <item objectid="1"/>
  </build>
</model>"#,
    );

    // Build OPC ZIP Package
    let mut zip = zip::ZipWriter::new(writer);
    let zip_options = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);

    // [Content_Types].xml
    zip.start_file("[Content_Types].xml", zip_options)
        .map_err(|e| Error::InvalidInput(format!("failed to write [Content_Types].xml: {e}")))?;
    std::io::Write::write_all(
        &mut zip,
        r#"<?xml version="1.0" encoding="UTF-8"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
  <Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>
  <Default Extension="model" ContentType="application/vnd.ms-package.3dmanufacturing-3dmodel+xml"/>
</Types>"#
            .as_bytes(),
    )?;

    // _rels/.rels
    zip.start_file("_rels/.rels", zip_options)
        .map_err(|e| Error::InvalidInput(format!("failed to write _rels/.rels: {e}")))?;
    std::io::Write::write_all(&mut zip, r#"<?xml version="1.0" encoding="UTF-8"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
  <Relationship Target="/3D/3dmodel.model" Id="rel0" Type="http://schemas.microsoft.com/3dmanufacturing/2013/01/3dmodel"/>
</Relationships>"#.as_bytes())?;

    // 3D/3dmodel.model
    zip.start_file("3D/3dmodel.model", zip_options)
        .map_err(|e| Error::InvalidInput(format!("failed to write 3D/3dmodel.model: {e}")))?;
    std::io::Write::write_all(&mut zip, model_xml.as_bytes())?;

    zip.finish()
        .map_err(|e| Error::InvalidInput(format!("failed to finish 3MF zip package: {e}")))?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selects_build_objects_applies_item_transform_and_converts_units() {
        let xml = r#"<model unit="centimeter"><resources>
<object id="1"><mesh><vertices><vertex x="0" y="0" z="0"/><vertex x="1" y="0" z="0"/><vertex x="0" y="1" z="0"/></vertices><triangles><triangle v1="0" v2="1" v3="2"/></triangles></mesh></object>
<object id="2"><mesh><vertices><vertex x="1" y="2" z="3"/><vertex x="2" y="2" z="3"/><vertex x="1" y="3" z="3"/></vertices><triangles><triangle v1="0" v2="1" v3="2"/></triangles></mesh></object>
</resources><build><item objectid="2" transform="2 0 0 0 1 0 0 0 1 5 6 7"/></build></model>"#;
        let (vertices, faces, warnings) = parse_3mf_model(xml, 10_000).unwrap();
        assert_eq!(vertices.len(), 3);
        assert_eq!(faces.len(), 1);
        assert!(warnings.is_empty());
        assert_eq!(
            (vertices[0].x, vertices[0].y, vertices[0].z),
            (70.0, 80.0, 100.0)
        );
        assert_eq!(
            (vertices[1].x, vertices[1].y, vertices[1].z),
            (90.0, 80.0, 100.0)
        );
        assert_eq!(
            (vertices[2].x, vertices[2].y, vertices[2].z),
            (70.0, 90.0, 100.0)
        );
    }

    #[test]
    fn composes_component_transform_before_build_transform() {
        let xml = r#"<model unit="millimeter"><resources>
<object id="1"><mesh><vertices><vertex x="1" y="2" z="3"/><vertex x="2" y="2" z="3"/><vertex x="1" y="3" z="3"/></vertices><triangles><triangle v1="0" v2="1" v3="2"/></triangles></mesh></object>
<object id="2"><components><component objectid="1" transform="1 0 0 0 1 0 0 0 1 10 0 0"/></components></object>
</resources><build><item objectid="2" transform="2 0 0 0 1 0 0 0 1 0 20 0"/></build></model>"#;
        let (vertices, faces, warnings) = parse_3mf_model(xml, 10_000).unwrap();
        assert_eq!(vertices.len(), 3);
        assert_eq!(faces.len(), 1);
        assert!(warnings.is_empty());
        assert_eq!(
            (vertices[0].x, vertices[0].y, vertices[0].z),
            (22.0, 22.0, 3.0)
        );
        assert_eq!(
            (vertices[1].x, vertices[1].y, vertices[1].z),
            (24.0, 22.0, 3.0)
        );
        assert_eq!(
            (vertices[2].x, vertices[2].y, vertices[2].z),
            (22.0, 23.0, 3.0)
        );
    }

    #[test]
    fn rejects_3mf_component_cycles_and_unknown_units() {
        let cycle = r#"<model><resources><object id="1"><components><component objectid="2"/></components></object><object id="2"><components><component objectid="1"/></components></object></resources><build><item objectid="1"/></build></model>"#;
        assert!(matches!(
            parse_3mf_model(cycle, 10_000),
            Err(Error::InvalidInput(_))
        ));
        assert!(matches!(
            parse_3mf_model("<model unit=\"parsec\"><resources/><build/></model>", 100),
            Err(Error::InvalidInput(_))
        ));
        assert!(matches!(
            parse_3mf_model(
                "<model requiredextensions=\"material\"><resources/><build/></model>",
                100
            ),
            Err(Error::Unsupported(_))
        ));
        let other_object = r#"<model><resources><object id="1" type="other"><mesh><vertices><vertex x="0" y="0" z="0"/><vertex x="1" y="0" z="0"/><vertex x="0" y="1" z="0"/></vertices><triangles><triangle v1="0" v2="1" v3="2"/></triangles></mesh></object></resources><build><item objectid="1"/></build></model>"#;
        assert!(matches!(
            parse_3mf_model(other_object, 10_000),
            Err(Error::InvalidInput(_))
        ));
    }

    #[test]
    fn preserves_a_legacy_no_build_preview_with_an_explicit_warning() {
        let xml = r#"<model><resources><object id="1"><mesh><vertices><vertex x="0" y="0" z="0"/><vertex x="1" y="0" z="0"/><vertex x="0" y="1" z="0"/></vertices><triangles><triangle v1="0" v2="1" v3="2"/></triangles></mesh></object></resources></model>"#;
        let (vertices, faces, warnings) = parse_3mf_model(xml, 10_000).unwrap();
        assert_eq!(vertices.len(), 3);
        assert_eq!(faces.len(), 1);
        assert!(
            warnings
                .iter()
                .any(|warning| warning.contains("compatibility fallback"))
        );
    }
}
