//! A bounded IFC-SPF previewer for IFC4 tessellated meshes and common extrusions.
//!
//! IFC is an information model, not just a mesh. This importer deliberately
//! reads supported tessellated geometry, selected rectangular/circular/polygon
//! profiles, and local placements needed for a 3D preview; it does not evaluate
//! IFC booleans, general parametric solids, materials, georeferencing, or project
//! semantics.

use std::collections::{HashMap, HashSet};
use std::fmt::Write as FmtWrite;
use std::fs::File;
use std::io::{BufReader, Cursor, Read};
use std::path::Path;

use quick_xml::NsReader;
use quick_xml::events::Event;
use quick_xml::name::ResolveResult;

use crate::convert::{ConvertOptions, PageConsumer};
use crate::error::{Error, Result};
use crate::geospatial::xml_tree::{XmlElement, XmlLimits, parse_xml_tree};
use crate::ir::Page;

const MAX_IFC_BYTES: u64 = 64 * 1024 * 1024;
const MAX_IFC_ENTITIES: usize = 500_000;
const MAX_IFC_RECORD_BYTES: usize = 8 * 1024 * 1024;
const MAX_IFC_VALUE_NODES: usize = 4_000_000;
const MAX_IFC_DEPTH: usize = 64;
const MAX_IFC_POINT_LISTS: usize = 100_000;
const MAX_IFC_POINTS: usize = 500_000;
const MAX_IFC_FACESETS: usize = 100_000;
const MAX_IFC_TRIANGLES: usize = 200_000;
const MAX_IFC_EXPANDED_TRIANGLES: usize = 200_000;
const MAX_IFC_EXPANDED_VERTICES: usize = 1_000_000;
const MAX_IFC_OBJ_BYTES: usize = 64 * 1024 * 1024;
const MAX_IFC_POLYGON_VERTICES: usize = 4_096;
const MAX_IFC_CONCAVE_POLYGON_VERTICES: usize = 256;
const MAX_IFC_POLYGON_EDGE_CHECKS: usize = 20_000_000;
const MAX_IFC_MAPPING_DEPTH: usize = 32;
const MAX_IFC_POLYLINE_VERTICES: usize = 4_096;
const MAX_IFC_POLYLINE_VERTICES_TOTAL: usize = 500_000;
const MAX_IFC_GENERATED_PROFILE_VERTICES: usize = 500_000;
const IFC_CIRCLE_PROFILE_SEGMENTS: usize = 64;
const MAX_IFCZIP_ENTRIES: usize = 10_000;
const MAX_IFCZIP_NAME_BYTES: usize = 8 * 1024 * 1024;
const MAX_IFC_XML_NODES: usize = 1_000_000;
const MAX_IFC_XML_TEXT_BYTES: usize = 64 * 1024 * 1024;

#[derive(Clone, Copy, Debug, Default)]
struct Vec3 {
    x: f64,
    y: f64,
    z: f64,
}

impl Vec3 {
    fn dot(self, other: Self) -> f64 {
        self.x * other.x + self.y * other.y + self.z * other.z
    }

    fn cross(self, other: Self) -> Self {
        Self {
            x: self.y * other.z - self.z * other.y,
            y: self.z * other.x - self.x * other.z,
            z: self.x * other.y - self.y * other.x,
        }
    }

    fn scale(self, value: f64) -> Self {
        Self {
            x: self.x * value,
            y: self.y * value,
            z: self.z * value,
        }
    }

    fn sub(self, other: Self) -> Self {
        Self {
            x: self.x - other.x,
            y: self.y - other.y,
            z: self.z - other.z,
        }
    }

    fn normalized(self) -> Option<Self> {
        let length = self.dot(self).sqrt();
        (length.is_finite() && length > 1e-12).then(|| self.scale(1.0 / length))
    }
}

#[derive(Clone, Debug)]
enum Value {
    Ref(u64),
    Number(f64),
    List(Vec<Value>),
    Other,
}

#[derive(Clone, Debug)]
struct FaceSet {
    point_list: u64,
    faces: Vec<[usize; 3]>,
    pn_index: Option<Vec<usize>>,
}

#[derive(Clone, Copy, Debug)]
struct AxisPlacement {
    location: u64,
    axis: Option<u64>,
    ref_direction: Option<u64>,
}

#[derive(Clone, Copy, Debug)]
struct LocalPlacement {
    parent: Option<u64>,
    relative: u64,
}

#[derive(Clone, Copy, Debug)]
struct Product {
    placement: Option<u64>,
    representation: Option<u64>,
}

#[derive(Clone, Copy, Debug)]
struct RepresentationMap {
    origin: u64,
    representation: u64,
}

#[derive(Clone, Copy, Debug)]
struct MappedItem {
    source: u64,
    target: u64,
}

#[derive(Clone, Copy, Debug)]
struct TransformationOperator {
    axis1: Option<u64>,
    axis2: Option<u64>,
    origin: u64,
    scale: f64,
    axis3: Option<u64>,
}

#[derive(Clone, Copy, Debug)]
struct AxisPlacement2D {
    location: u64,
    ref_direction: Option<u64>,
}

#[derive(Clone, Copy, Debug)]
enum Profile {
    Rectangle {
        position: Option<u64>,
        x_dim: f64,
        y_dim: f64,
    },
    Circle {
        position: Option<u64>,
        radius: f64,
    },
    ArbitraryClosed {
        outer_curve: u64,
    },
}

#[derive(Clone, Copy, Debug)]
struct ExtrudedAreaSolid {
    swept_area: u64,
    position: Option<u64>,
    direction: u64,
    depth: f64,
}

#[derive(Clone, Copy, Debug)]
struct Matrix4([[f64; 4]; 4]);

impl Matrix4 {
    const IDENTITY: Self = Self([
        [1.0, 0.0, 0.0, 0.0],
        [0.0, 1.0, 0.0, 0.0],
        [0.0, 0.0, 1.0, 0.0],
        [0.0, 0.0, 0.0, 1.0],
    ]);

    fn from_axes(x: Vec3, y: Vec3, z: Vec3, origin: Vec3) -> Self {
        Self([
            [x.x, y.x, z.x, origin.x],
            [x.y, y.y, z.y, origin.y],
            [x.z, y.z, z.z, origin.z],
            [0.0, 0.0, 0.0, 1.0],
        ])
    }

    fn multiply(self, rhs: Self) -> Self {
        let mut output = [[0.0; 4]; 4];
        for (row, row_values) in output.iter_mut().enumerate() {
            for (column, value) in row_values.iter_mut().enumerate() {
                *value = (0..4)
                    .map(|index| self.0[row][index] * rhs.0[index][column])
                    .sum();
            }
        }
        Self(output)
    }

    fn transform(self, point: Vec3) -> Vec3 {
        Vec3 {
            x: self.0[0][0] * point.x
                + self.0[0][1] * point.y
                + self.0[0][2] * point.z
                + self.0[0][3],
            y: self.0[1][0] * point.x
                + self.0[1][1] * point.y
                + self.0[1][2] * point.z
                + self.0[1][3],
            z: self.0[2][0] * point.x
                + self.0[2][1] * point.y
                + self.0[2][2] * point.z
                + self.0[2][3],
        }
    }

    fn inverse_rigid(self) -> Self {
        let mut output = Self::IDENTITY.0;
        for (row, values) in output.iter_mut().enumerate().take(3) {
            for (column, value) in values.iter_mut().enumerate().take(3) {
                *value = self.0[column][row];
            }
            values[3] = -(0..3)
                .map(|column| self.0[column][row] * self.0[column][3])
                .sum::<f64>();
        }
        Self(output)
    }
}

struct IfcPageSink<'a> {
    inner: &'a mut dyn PageConsumer,
    source_format: &'static str,
}

struct IfcZipPageSink<'a> {
    inner: &'a mut dyn PageConsumer,
    related_entries_omitted: bool,
}

impl PageConsumer for IfcZipPageSink<'_> {
    fn consume(&mut self, mut page: Page) -> Result<()> {
        page.source_format = "ifczip".into();
        if self.related_entries_omitted {
            page.warn("IfcZIP sidecar files were not extracted or loaded");
        }
        self.inner.consume(page)
    }
}

impl PageConsumer for IfcPageSink<'_> {
    fn consume(&mut self, mut page: Page) -> Result<()> {
        page.source_format = self.source_format.into();
        page.title = "IFC tessellated building model".into();
        self.inner.consume(page)
    }
}

type PolygonSetData = (u64, Vec<u64>, Option<Vec<usize>>);
type TriangleMesh = (Vec<Vec3>, Vec<[usize; 3]>);

#[derive(Default)]
struct Model {
    points: HashMap<u64, Vec3>,
    point_dimensions: HashMap<u64, usize>,
    directions: HashMap<u64, Vec3>,
    direction_dimensions: HashMap<u64, usize>,
    point_lists: HashMap<u64, Vec<Vec3>>,
    face_sets: HashMap<u64, FaceSet>,
    polygon_sets: HashMap<u64, PolygonSetData>,
    polygon_faces: HashMap<u64, Vec<usize>>,
    axes: HashMap<u64, AxisPlacement>,
    axes_2d: HashMap<u64, AxisPlacement2D>,
    placements: HashMap<u64, LocalPlacement>,
    representations: HashMap<u64, Vec<u64>>,
    product_shapes: HashMap<u64, Vec<u64>>,
    representation_maps: HashMap<u64, RepresentationMap>,
    mapped_items: HashMap<u64, MappedItem>,
    transformation_operators: HashMap<u64, TransformationOperator>,
    unsupported_transforms: HashSet<u64>,
    profiles: HashMap<u64, Profile>,
    unsupported_profiles: HashSet<u64>,
    polylines: HashMap<u64, Vec<u64>>,
    extruded_solids: HashMap<u64, ExtrudedAreaSolid>,
    generated_points: HashMap<u64, Vec<Vec3>>,
    products: Vec<Product>,
}

pub(crate) fn looks_like_ifc_prefix(bytes: &[u8]) -> bool {
    let bytes = &bytes[..bytes.len().min(4096)];
    let mut cursor = 0usize;
    let mut has_step_header = false;
    while cursor < bytes.len() {
        if bytes[cursor] == b'/' && bytes.get(cursor + 1) == Some(&b'*') {
            cursor += 2;
            while cursor + 1 < bytes.len() && !(bytes[cursor] == b'*' && bytes[cursor + 1] == b'/')
            {
                cursor += 1;
            }
            if cursor + 1 >= bytes.len() {
                return false;
            }
            cursor += 2;
            continue;
        }
        if bytes[cursor] == b'\'' {
            cursor += 1;
            while cursor < bytes.len() {
                if bytes[cursor] == b'\'' {
                    if bytes.get(cursor + 1) == Some(&b'\'') {
                        cursor += 2;
                        continue;
                    }
                    cursor += 1;
                    break;
                }
                cursor += 1;
            }
            continue;
        }
        if bytes
            .get(cursor..cursor + b"ISO-10303-21;".len())
            .is_some_and(|window| window.eq_ignore_ascii_case(b"ISO-10303-21;"))
        {
            has_step_header = true;
        }
        if !starts_word(bytes, cursor, b"FILE_SCHEMA") {
            cursor += 1;
            continue;
        }
        let section_start = cursor + b"FILE_SCHEMA".len();
        let mut end = section_start;
        let mut in_string = false;
        while end < bytes.len() {
            if bytes[end] == b'\'' {
                if in_string && bytes.get(end + 1) == Some(&b'\'') {
                    end += 2;
                    continue;
                }
                in_string = !in_string;
            } else if bytes[end] == b';' && !in_string {
                let schema =
                    String::from_utf8_lossy(&bytes[section_start..end]).to_ascii_uppercase();
                return has_step_header
                    && schema
                        .split('\'')
                        .any(|token| token.trim_start().starts_with("IFC"));
            }
            end += 1;
        }
        return false;
    }
    false
}

pub(crate) fn looks_like_ifcxml_prefix(bytes: &[u8]) -> bool {
    let mut reader = NsReader::from_reader(Cursor::new(&bytes[..bytes.len().min(64 * 1024)]));
    reader.config_mut().trim_text(true);
    let mut buffer = Vec::new();
    loop {
        match reader.read_resolved_event_into(&mut buffer) {
            Ok((namespace, Event::Start(element))) | Ok((namespace, Event::Empty(element))) => {
                let Some(namespace) = (match namespace {
                    ResolveResult::Bound(namespace) => Some(namespace),
                    _ => None,
                }) else {
                    return false;
                };
                let qualified_name = element.name();
                let local_name = crate::ooxml::local_name(qualified_name.as_ref());
                let namespace = namespace.as_ref();
                return local_name.eq_ignore_ascii_case(b"ifcXML")
                    && (namespace.starts_with(b"http://www.buildingsmart-tech.org/ifcXML/")
                        || namespace.starts_with(b"http://www.iai-tech.org/ifcXML/"));
            }
            Ok((_, Event::DocType(_))) => buffer.clear(),
            Err(_) => return false,
            Ok((_, Event::Eof)) => return false,
            _ => buffer.clear(),
        }
        buffer.clear();
    }
}

pub(crate) fn convert<R: Read>(
    input: R,
    options: &ConvertOptions,
    sink: &mut dyn PageConsumer,
) -> Result<Vec<String>> {
    convert_with_expected_xml(input, options, sink, false)
}

pub(crate) fn convert_ifcxml<R: Read>(
    input: R,
    options: &ConvertOptions,
    sink: &mut dyn PageConsumer,
) -> Result<Vec<String>> {
    convert_with_expected_xml(input, options, sink, true)
}

fn convert_with_expected_xml<R: Read>(
    input: R,
    options: &ConvertOptions,
    sink: &mut dyn PageConsumer,
    require_ifcxml: bool,
) -> Result<Vec<String>> {
    let max_bytes = options.max_input_bytes.min(MAX_IFC_BYTES);
    let mut bytes = Vec::new();
    Read::take(input, max_bytes.saturating_add(1)).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > max_bytes {
        return Err(Error::LimitExceeded(format!(
            "IFC input exceeds maximum limit of {max_bytes} bytes"
        )));
    }
    let is_ifcxml = looks_like_ifcxml_prefix(&bytes);
    if require_ifcxml && !is_ifcxml {
        return Err(Error::InvalidInput(
            "input does not contain a supported buildingSMART IFCXML root".into(),
        ));
    }
    let (mut model, mut unsupported_items) = if is_ifcxml {
        parse_ifcxml_model(&bytes, options)?
    } else {
        let text = String::from_utf8_lossy(&bytes);
        if !looks_like_ifc_prefix(text.as_bytes()) {
            return Err(Error::InvalidInput(
                "input does not contain an IFC STEP Physical File or supported IFCXML root".into(),
            ));
        }
        parse_model(&text)?
    };
    if model.face_sets.is_empty()
        && model.polygon_sets.is_empty()
        && model.extruded_solids.is_empty()
    {
        return Err(Error::Unsupported(
            "IFC model contains no supported IFC4 tessellated face sets".into(),
        ));
    }

    let mut face_sets = std::mem::take(&mut model.face_sets);
    let mut total_triangles = face_sets
        .values()
        .map(|face_set| face_set.faces.len())
        .sum::<usize>();
    let mut polygon_edge_checks = 0usize;
    for (id, (point_list, face_ids, pn_index)) in std::mem::take(&mut model.polygon_sets) {
        let mut faces = Vec::new();
        let points = model.point_lists.get(&point_list).ok_or_else(|| {
            Error::InvalidInput(format!(
                "IFC polygon face set #{id} references missing point list #{point_list}"
            ))
        })?;
        for face_id in face_ids {
            let Some(indices) = model.polygon_faces.get(&face_id) else {
                return Err(Error::InvalidInput(format!(
                    "IFC polygon face set #{id} references missing indexed face #{face_id}"
                )));
            };
            if indices.len() < 3 {
                return Err(Error::InvalidInput(format!(
                    "IFC indexed polygon face #{face_id} has fewer than three vertices"
                )));
            }
            let resolved = map_polygon_indices(indices, pn_index.as_deref(), points.len())?;
            let Some(triangles) =
                triangulate_planar_polygon(&resolved, points, &mut polygon_edge_checks)?
            else {
                unsupported_items = true;
                continue;
            };
            total_triangles = total_triangles
                .checked_add(triangles.len())
                .ok_or_else(|| {
                    Error::LimitExceeded("IFC source triangle count overflowed".into())
                })?;
            if total_triangles > MAX_IFC_TRIANGLES {
                return Err(Error::LimitExceeded(format!(
                    "IFC source triangles exceed {MAX_IFC_TRIANGLES}"
                )));
            }
            faces.extend(triangles);
            if faces.len() > MAX_IFC_TRIANGLES {
                return Err(Error::LimitExceeded(format!(
                    "IFC polygon triangulation exceeds {MAX_IFC_TRIANGLES} triangles"
                )));
            }
        }
        face_sets.insert(
            id,
            FaceSet {
                point_list,
                faces,
                pn_index,
            },
        );
    }

    let mut generated_profile_vertices = 0usize;
    let extruded_solid_ids = model.extruded_solids.keys().copied().collect::<Vec<_>>();
    for id in extruded_solid_ids {
        let solid = model.extruded_solids[&id];
        let remaining_vertices =
            MAX_IFC_GENERATED_PROFILE_VERTICES.saturating_sub(generated_profile_vertices);
        let Some((points, faces)) = build_extruded_mesh(
            id,
            solid,
            &model,
            &mut polygon_edge_checks,
            remaining_vertices,
        )?
        else {
            unsupported_items = true;
            continue;
        };
        generated_profile_vertices = generated_profile_vertices
            .checked_add(points.len())
            .ok_or_else(|| {
                Error::LimitExceeded("IFC generated profile vertex count overflowed".into())
            })?;
        if generated_profile_vertices > MAX_IFC_GENERATED_PROFILE_VERTICES {
            return Err(Error::LimitExceeded(format!(
                "IFC generated profile vertices exceed {MAX_IFC_GENERATED_PROFILE_VERTICES}"
            )));
        }
        total_triangles = total_triangles
            .checked_add(faces.len())
            .ok_or_else(|| Error::LimitExceeded("IFC source triangle count overflowed".into()))?;
        if total_triangles > MAX_IFC_TRIANGLES {
            return Err(Error::LimitExceeded(format!(
                "IFC source and extruded-solid triangles exceed {MAX_IFC_TRIANGLES}"
            )));
        }
        model.generated_points.insert(id, points);
        face_sets.insert(
            id,
            FaceSet {
                point_list: id,
                faces,
                pn_index: None,
            },
        );
    }

    let mut warnings = Vec::new();
    let mut object = String::new();
    let mut emitted_sets = HashSet::new();
    let mut expanded_vertices = 0usize;
    let mut expanded_triangles = 0usize;
    let mut has_linked_geometry_items = false;
    for product in &model.products {
        let Some(representation) = product.representation else {
            continue;
        };
        if product
            .placement
            .is_some_and(|placement| !model.placements.contains_key(&placement))
        {
            let warning = "IFC grid or non-local product placements were omitted";
            if !warnings.iter().any(|existing| existing == warning) {
                warnings.push(warning.into());
            }
            continue;
        }
        let transform = match product.placement {
            Some(placement) => resolve_placement(placement, &model, &mut HashSet::new(), 0)?,
            None => Matrix4::IDENTITY,
        };
        let Some(shape_ids) = model.product_shapes.get(&representation) else {
            continue;
        };
        for shape_id in shape_ids {
            let items = model
                .representations
                .get(shape_id)
                .map(Vec::as_slice)
                .unwrap_or(&[]);
            has_linked_geometry_items |= !items.is_empty();
            append_representation_items(
                items,
                transform,
                0,
                &mut HashSet::new(),
                &model,
                &face_sets,
                &mut object,
                &mut expanded_vertices,
                &mut expanded_triangles,
                &mut emitted_sets,
                &mut unsupported_items,
            )?;
        }
    }

    if emitted_sets.is_empty() && !has_linked_geometry_items {
        // A geometry-only export can contain tessellations without product
        // representation links. Such files are still useful as mesh previews.
        for (id, face_set) in &face_sets {
            let point_list = model
                .point_lists
                .get(&face_set.point_list)
                .or_else(|| model.generated_points.get(id))
                .ok_or_else(|| {
                    Error::InvalidInput(format!(
                        "IFC face set #{id} references missing point list #{}",
                        face_set.point_list
                    ))
                })?;
            append_mesh(
                &mut object,
                point_list,
                face_set,
                Matrix4::IDENTITY,
                &mut expanded_vertices,
                &mut expanded_triangles,
            )?;
        }
        warnings.push(
            "IFC supported mesh geometry is not linked to product representations; local coordinates were previewed directly".into(),
        );
    } else if !emitted_sets.is_empty() && emitted_sets.len() < face_sets.len() {
        warnings.push("unreferenced IFC tessellated face sets were omitted".into());
    }

    if unsupported_items {
        warnings.push(
            "Some IFC parametric or unsupported mapped/swept geometry, polygon holes, self-intersecting/non-planar faces, and concave polygons over 256 vertices were omitted; boolean solids are not evaluated".into(),
        );
    }
    warnings.push(
        "IFC materials, object properties, units, georeferencing, and project hierarchy are not displayed; mesh geometry is shaded with a neutral preview style".into(),
    );
    if expanded_triangles == 0 {
        return Err(Error::Unsupported(
            "IFC model has no supported, non-degenerate triangular faces".into(),
        ));
    }
    if object.len() > MAX_IFC_OBJ_BYTES || object.len() as u64 > options.max_input_bytes {
        return Err(Error::LimitExceeded(format!(
            "IFC mesh intermediate exceeds {MAX_IFC_OBJ_BYTES} bytes"
        )));
    }
    let mut page_sink = IfcPageSink {
        inner: sink,
        source_format: if is_ifcxml { "ifcxml" } else { "ifc" },
    };
    warnings.extend(crate::cad::obj::convert(
        Cursor::new(object),
        options,
        &mut page_sink,
    )?);
    Ok(warnings)
}

pub(crate) fn convert_ifczip(
    path: &Path,
    options: &ConvertOptions,
    sink: &mut dyn PageConsumer,
) -> Result<Vec<String>> {
    let input_limit = options.max_input_bytes.min(MAX_IFC_BYTES);
    let metadata = std::fs::metadata(path)?;
    if metadata.len() > input_limit {
        return Err(Error::LimitExceeded(format!(
            "IfcZIP input exceeds maximum limit of {input_limit} bytes"
        )));
    }
    let mut archive = zip::ZipArchive::new(BufReader::new(File::open(path)?))
        .map_err(|error| Error::InvalidInput(format!("invalid IfcZIP archive: {error}")))?;
    if archive.len() > MAX_IFCZIP_ENTRIES {
        return Err(Error::LimitExceeded(format!(
            "IfcZIP contains more than {MAX_IFCZIP_ENTRIES} entries"
        )));
    }
    let entry_limit = input_limit
        .min(options.max_zip_entry_bytes)
        .min(MAX_IFC_BYTES);
    let mut names_bytes = 0usize;
    let mut total_uncompressed = 0u64;
    let mut ifc_index = None;
    let mut ifc_count = 0usize;
    let mut related_entries = 0usize;
    for index in 0..archive.len() {
        let entry = archive.by_index(index)?;
        names_bytes = names_bytes
            .checked_add(entry.name().len())
            .ok_or_else(|| Error::LimitExceeded("IfcZIP entry-name bytes overflowed".into()))?;
        if names_bytes > MAX_IFCZIP_NAME_BYTES {
            return Err(Error::LimitExceeded(format!(
                "IfcZIP entry names exceed {MAX_IFCZIP_NAME_BYTES} bytes"
            )));
        }
        total_uncompressed = total_uncompressed
            .checked_add(entry.size())
            .ok_or_else(|| Error::LimitExceeded("IfcZIP expanded size overflowed".into()))?;
        if total_uncompressed > input_limit {
            return Err(Error::LimitExceeded(format!(
                "IfcZIP declared expanded size exceeds {input_limit} bytes"
            )));
        }
        let name = entry.name();
        if !entry.is_dir()
            && !name.contains('/')
            && !name.contains('\\')
            && name.to_ascii_lowercase().ends_with(".ifc")
        {
            ifc_count += 1;
            ifc_index = Some(index);
        } else if !entry.is_dir() {
            related_entries += 1;
        }
    }
    if ifc_count != 1 {
        return Err(Error::InvalidInput(format!(
            "IfcZIP must contain exactly one root-level .ifc model; found {ifc_count}"
        )));
    }
    let index = ifc_index
        .ok_or_else(|| Error::InvalidInput("IfcZIP has no root-level .ifc model".into()))?;
    let mut entry = archive.by_index(index)?;
    if entry.size() > entry_limit {
        return Err(Error::LimitExceeded(format!(
            "IfcZIP model exceeds maximum entry size of {entry_limit} bytes"
        )));
    }
    let declared_size = entry.size();
    let mut model = Vec::with_capacity(declared_size.min(entry_limit) as usize);
    Read::take(&mut entry, entry_limit.saturating_add(1)).read_to_end(&mut model)?;
    if model.len() as u64 != declared_size || model.len() as u64 > entry_limit {
        return Err(Error::InvalidInput(
            "IfcZIP model size did not match its bounded ZIP directory entry".into(),
        ));
    }

    let mut page_sink = IfcZipPageSink {
        inner: sink,
        related_entries_omitted: related_entries > 0,
    };
    let mut warnings = convert(Cursor::new(model), options, &mut page_sink)?;
    if related_entries > 0 {
        warnings.push(format!(
            "{related_entries} IfcZIP sidecar file(s) were not extracted or loaded"
        ));
    }
    Ok(warnings)
}

fn append_mesh(
    output: &mut String,
    points: &[Vec3],
    faces: &FaceSet,
    transform: Matrix4,
    expanded_vertices: &mut usize,
    expanded_triangles: &mut usize,
) -> Result<()> {
    let base = *expanded_vertices;
    *expanded_vertices = expanded_vertices
        .checked_add(points.len())
        .ok_or_else(|| Error::LimitExceeded("IFC expanded vertex count overflowed".into()))?;
    if *expanded_vertices > MAX_IFC_EXPANDED_VERTICES {
        return Err(Error::LimitExceeded(format!(
            "IFC expanded vertices exceed {MAX_IFC_EXPANDED_VERTICES}"
        )));
    }
    for point in points {
        let transformed = transform.transform(*point);
        if ![transformed.x, transformed.y, transformed.z]
            .iter()
            .all(|value| value.is_finite() && value.abs() <= 1e12)
        {
            return Err(Error::InvalidInput(
                "IFC transformed vertex coordinate is non-finite or outside ±1e12".into(),
            ));
        }
        writeln!(
            output,
            "v {} {} {}",
            transformed.x, transformed.y, transformed.z
        )
        .map_err(|_| Error::InvalidInput("could not write IFC mesh vertex".into()))?;
        check_obj_limit(output)?;
    }
    for [a, b, c] in &faces.faces {
        let mut indices = [*a, *b, *c];
        if let Some(pn_index) = &faces.pn_index {
            for index in &mut indices {
                *index = *pn_index.get(*index - 1).ok_or_else(|| {
                    Error::InvalidInput("IFC PnIndex is shorter than CoordIndex references".into())
                })?;
            }
        }
        if indices
            .iter()
            .any(|index| *index == 0 || *index > points.len())
        {
            return Err(Error::InvalidInput(
                "IFC triangle index is outside its Cartesian point list".into(),
            ));
        }
        *expanded_triangles = expanded_triangles
            .checked_add(1)
            .ok_or_else(|| Error::LimitExceeded("IFC expanded triangle count overflowed".into()))?;
        if *expanded_triangles > MAX_IFC_EXPANDED_TRIANGLES {
            return Err(Error::LimitExceeded(format!(
                "IFC expanded triangles exceed {MAX_IFC_EXPANDED_TRIANGLES}"
            )));
        }
        writeln!(
            output,
            "f {} {} {}",
            base + indices[0],
            base + indices[1],
            base + indices[2]
        )
        .map_err(|_| Error::InvalidInput("could not write IFC mesh face".into()))?;
        check_obj_limit(output)?;
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn append_representation_items(
    items: &[u64],
    transform: Matrix4,
    depth: usize,
    active_mappings: &mut HashSet<u64>,
    model: &Model,
    face_sets: &HashMap<u64, FaceSet>,
    output: &mut String,
    expanded_vertices: &mut usize,
    expanded_triangles: &mut usize,
    emitted_sets: &mut HashSet<u64>,
    unsupported_items: &mut bool,
) -> Result<()> {
    if depth > MAX_IFC_MAPPING_DEPTH {
        *unsupported_items = true;
        return Ok(());
    }
    for item_id in items {
        if let Some(face_set) = face_sets.get(item_id) {
            let points = model
                .point_lists
                .get(&face_set.point_list)
                .or_else(|| model.generated_points.get(item_id))
                .ok_or_else(|| {
                    Error::InvalidInput(format!(
                        "IFC face set #{item_id} references missing point list #{}",
                        face_set.point_list
                    ))
                })?;
            append_mesh(
                output,
                points,
                face_set,
                transform,
                expanded_vertices,
                expanded_triangles,
            )?;
            emitted_sets.insert(*item_id);
            continue;
        }
        let Some(mapped_item) = model.mapped_items.get(item_id) else {
            *unsupported_items = true;
            continue;
        };
        if model.unsupported_transforms.contains(&mapped_item.target) {
            *unsupported_items = true;
            continue;
        }
        if !active_mappings.insert(*item_id) {
            return Err(Error::InvalidInput(format!(
                "IFC mapped representation cycle detected at item #{item_id}"
            )));
        }
        let representation_map = model
            .representation_maps
            .get(&mapped_item.source)
            .ok_or_else(|| {
                Error::InvalidInput(format!(
                    "IFC mapped item #{item_id} references missing representation map #{}",
                    mapped_item.source
                ))
            })?;
        let target = transformation_operator_matrix(mapped_item.target, model)?;
        let origin = axis_placement_matrix(representation_map.origin, model)?;
        let origin_placement = model.axes.get(&representation_map.origin).ok_or_else(|| {
            Error::InvalidInput(format!(
                "IFC representation map #{} references missing MappingOrigin #{}",
                mapped_item.source, representation_map.origin
            ))
        })?;
        if model.point_dimensions.get(&origin_placement.location) != Some(&3) {
            return Err(Error::InvalidInput(format!(
                "IFC representation map #{}, MappingOrigin is not three-dimensional",
                mapped_item.source
            )));
        }
        let mapped_transform = transform.multiply(target).multiply(origin.inverse_rigid());
        let mapped_items = model
            .representations
            .get(&representation_map.representation)
            .ok_or_else(|| {
                Error::InvalidInput(format!(
                    "IFC representation map #{} references missing mapped representation #{}",
                    mapped_item.source, representation_map.representation
                ))
            })?;
        append_representation_items(
            mapped_items,
            mapped_transform,
            depth + 1,
            active_mappings,
            model,
            face_sets,
            output,
            expanded_vertices,
            expanded_triangles,
            emitted_sets,
            unsupported_items,
        )?;
        active_mappings.remove(item_id);
    }
    Ok(())
}

fn transformation_operator_matrix(id: u64, model: &Model) -> Result<Matrix4> {
    let operator = model.transformation_operators.get(&id).ok_or_else(|| {
        Error::InvalidInput(format!(
            "IFC mapping target #{id} is not a supported 3D transformation operator"
        ))
    })?;
    if model.point_dimensions.get(&operator.origin) != Some(&3) {
        return Err(Error::InvalidInput(format!(
            "IFC transformation operator #{id} LocalOrigin is not three-dimensional"
        )));
    }
    if !operator.scale.is_finite() || operator.scale <= 0.0 || operator.scale > 1_000_000.0 {
        return Err(Error::InvalidInput(format!(
            "IFC transformation operator #{id} has invalid or excessive scale"
        )));
    }
    let origin = *model.points.get(&operator.origin).ok_or_else(|| {
        Error::InvalidInput(format!(
            "IFC transformation operator #{id} references missing LocalOrigin #{}",
            operator.origin
        ))
    })?;
    let z_hint = direction_from_model(operator.axis3, model)?.unwrap_or(Vec3 {
        x: 0.0,
        y: 0.0,
        z: 1.0,
    });
    let z = z_hint.normalized().ok_or_else(|| {
        Error::InvalidInput(format!(
            "IFC transformation operator #{id} has an invalid Axis3"
        ))
    })?;
    let default_x = if (z.x - 1.0).abs() < 1e-12 && z.y.abs() < 1e-12 && z.z.abs() < 1e-12 {
        Vec3 {
            x: 0.0,
            y: 1.0,
            z: 0.0,
        }
    } else {
        Vec3 {
            x: 1.0,
            y: 0.0,
            z: 0.0,
        }
    };
    let x_hint = direction_from_model(operator.axis1, model)?.unwrap_or(default_x);
    let x = x_hint
        .sub(z.scale(x_hint.dot(z)))
        .normalized()
        .ok_or_else(|| {
            Error::InvalidInput(format!(
                "IFC transformation operator #{id} Axis1 is parallel to Axis3"
            ))
        })?;
    let y_hint = direction_from_model(operator.axis2, model)?.unwrap_or(Vec3 {
        x: 0.0,
        y: 1.0,
        z: 0.0,
    });
    let mut y = y_hint
        .sub(z.scale(y_hint.dot(z)))
        .sub(x.scale(y_hint.dot(x)))
        .normalized();
    // If Axis2 is omitted and the default happens to be parallel to the
    // derived X axis, use the right-handed completion of the same basis.
    if y.is_none() && operator.axis2.is_none() {
        y = z.cross(x).normalized();
    }
    let y = y.ok_or_else(|| {
        Error::InvalidInput(format!(
            "IFC transformation operator #{id} Axis2 is parallel to its derived axes"
        ))
    })?;
    Ok(Matrix4::from_axes(
        x.scale(operator.scale),
        y.scale(operator.scale),
        z.scale(operator.scale),
        origin,
    ))
}

fn direction_from_model(id: Option<u64>, model: &Model) -> Result<Option<Vec3>> {
    let Some(id) = id else {
        return Ok(None);
    };
    if model.direction_dimensions.get(&id) != Some(&3) {
        return Err(Error::InvalidInput(format!(
            "IFC transformation direction #{id} is not three-dimensional"
        )));
    }
    model
        .directions
        .get(&id)
        .copied()
        .map(Some)
        .ok_or_else(|| Error::InvalidInput(format!("IFC direction #{id} is missing")))
}

fn check_obj_limit(output: &str) -> Result<()> {
    if output.len() > MAX_IFC_OBJ_BYTES {
        return Err(Error::LimitExceeded(format!(
            "IFC mesh intermediate exceeds {MAX_IFC_OBJ_BYTES} bytes"
        )));
    }
    Ok(())
}

fn map_polygon_indices(
    indices: &[usize],
    pn_index: Option<&[usize]>,
    point_count: usize,
) -> Result<Vec<usize>> {
    if indices.len() > MAX_IFC_POLYGON_VERTICES {
        return Err(Error::LimitExceeded(format!(
            "IFC polygon face exceeds {MAX_IFC_POLYGON_VERTICES} vertices"
        )));
    }
    let mut mapped = Vec::with_capacity(indices.len());
    for &index in indices {
        if index == 0 {
            return Err(Error::InvalidInput(
                "IFC polygon index uses zero; IFC point indices are one-based".into(),
            ));
        }
        let coordinate_index = if let Some(pn_index) = pn_index {
            *pn_index.get(index - 1).ok_or_else(|| {
                Error::InvalidInput("IFC PnIndex is shorter than a polygon coordinate index".into())
            })?
        } else {
            index
        };
        if coordinate_index == 0 || coordinate_index > point_count {
            return Err(Error::InvalidInput(
                "IFC polygon index is outside its Cartesian point list".into(),
            ));
        }
        mapped.push(coordinate_index);
    }
    Ok(mapped)
}

fn triangulate_planar_polygon(
    indices: &[usize],
    points: &[Vec3],
    edge_checks: &mut usize,
) -> Result<Option<Vec<[usize; 3]>>> {
    let count = indices.len();
    if count < 3 {
        return Ok(None);
    }
    let vertices = indices
        .iter()
        .map(|index| points[index - 1])
        .collect::<Vec<_>>();
    let origin = vertices[0];
    let mut normal = Vec3::default();
    for index in 0..count {
        let a = vertices[index].sub(origin);
        let b = vertices[(index + 1) % count].sub(origin);
        normal = Vec3 {
            x: normal.x + a.y * b.z - a.z * b.y,
            y: normal.y + a.z * b.x - a.x * b.z,
            z: normal.z + a.x * b.y - a.y * b.x,
        };
    }
    let Some(normal) = normal.normalized() else {
        return Ok(None);
    };
    let scale = vertices
        .iter()
        .map(|point| point.sub(origin).dot(point.sub(origin)).sqrt())
        .fold(0.0_f64, f64::max)
        .max(1.0);
    if vertices
        .iter()
        .any(|point| normal.dot(point.sub(origin)).abs() > scale * 1e-8)
    {
        return Ok(None);
    }

    let drop_axis = if normal.x.abs() >= normal.y.abs() && normal.x.abs() >= normal.z.abs() {
        0
    } else if normal.y.abs() >= normal.z.abs() {
        1
    } else {
        2
    };
    let project = |point: Vec3| match drop_axis {
        0 => (point.y, point.z),
        1 => (point.x, point.z),
        _ => (point.x, point.y),
    };
    let projected = vertices.iter().copied().map(project).collect::<Vec<_>>();
    let projected_origin = projected[0];
    let area_twice = (1..count - 1)
        .map(|index| orientation2(projected_origin, projected[index], projected[index + 1]))
        .sum::<f64>();
    if !area_twice.is_finite() || area_twice.abs() <= scale * scale * 1e-12 {
        return Ok(None);
    }
    let orientation = area_twice.signum();
    let epsilon = scale * scale * 1e-12;
    for first in 0..count {
        let a = projected[first];
        let b = projected[(first + 1) % count];
        for second in first + 1..count {
            if second == first || second == (first + 1) % count || (second + 1) % count == first {
                continue;
            }
            charge_polygon_checks(edge_checks, 1)?;
            let c = projected[second];
            let d = projected[(second + 1) % count];
            if segments_intersect(a, b, c, d, epsilon) {
                return Ok(None);
            }
        }
    }

    let mut is_convex = true;
    for index in 0..count {
        charge_polygon_checks(edge_checks, 1)?;
        let turn = orientation2(
            projected[index],
            projected[(index + 1) % count],
            projected[(index + 2) % count],
        );
        if orientation * turn < -epsilon {
            is_convex = false;
            break;
        }
    }
    if is_convex {
        let mut triangles = Vec::with_capacity(count - 2);
        for index in 1..count - 1 {
            triangles.push([indices[0], indices[index], indices[index + 1]]);
        }
        return Ok(Some(triangles));
    }
    if count > MAX_IFC_CONCAVE_POLYGON_VERTICES {
        return Ok(None);
    }

    // Ear clipping preserves simple concave polygons. The vertex and global
    // edge-check caps bound the worst-case cubic search.
    let mut remaining = (0..count).collect::<Vec<_>>();
    let mut triangles = Vec::with_capacity(count - 2);
    while remaining.len() > 3 {
        let mut ear = None;
        for cursor in 0..remaining.len() {
            let previous = remaining[(cursor + remaining.len() - 1) % remaining.len()];
            let current = remaining[cursor];
            let next = remaining[(cursor + 1) % remaining.len()];
            let a = projected[previous];
            let b = projected[current];
            let c = projected[next];
            if orientation * orientation2(a, b, c) <= epsilon {
                continue;
            }
            let mut contains_vertex = false;
            for candidate in &remaining {
                if *candidate == previous || *candidate == current || *candidate == next {
                    continue;
                }
                charge_polygon_checks(edge_checks, 1)?;
                if point_in_or_on_triangle(projected[*candidate], a, b, c, orientation, epsilon) {
                    contains_vertex = true;
                    break;
                }
            }
            if !contains_vertex {
                ear = Some((cursor, previous, current, next));
                break;
            }
        }
        let Some((cursor, previous, current, next)) = ear else {
            return Ok(None);
        };
        triangles.push([indices[previous], indices[current], indices[next]]);
        remaining.remove(cursor);
    }
    triangles.push([
        indices[remaining[0]],
        indices[remaining[1]],
        indices[remaining[2]],
    ]);
    Ok(Some(triangles))
}

fn charge_polygon_checks(checks: &mut usize, amount: usize) -> Result<()> {
    *checks = checks
        .checked_add(amount)
        .ok_or_else(|| Error::LimitExceeded("IFC polygon edge-check count overflowed".into()))?;
    if *checks > MAX_IFC_POLYGON_EDGE_CHECKS {
        return Err(Error::LimitExceeded(format!(
            "IFC polygon validation exceeds {MAX_IFC_POLYGON_EDGE_CHECKS} edge checks"
        )));
    }
    Ok(())
}

fn orientation2(a: (f64, f64), b: (f64, f64), c: (f64, f64)) -> f64 {
    (b.0 - a.0) * (c.1 - a.1) - (b.1 - a.1) * (c.0 - a.0)
}

fn on_segment(a: (f64, f64), b: (f64, f64), point: (f64, f64), epsilon: f64) -> bool {
    point.0 >= a.0.min(b.0) - epsilon
        && point.0 <= a.0.max(b.0) + epsilon
        && point.1 >= a.1.min(b.1) - epsilon
        && point.1 <= a.1.max(b.1) + epsilon
}

fn segments_intersect(
    a: (f64, f64),
    b: (f64, f64),
    c: (f64, f64),
    d: (f64, f64),
    epsilon: f64,
) -> bool {
    let ab_c = orientation2(a, b, c);
    let ab_d = orientation2(a, b, d);
    let cd_a = orientation2(c, d, a);
    let cd_b = orientation2(c, d, b);
    if ab_c * ab_d < -epsilon * epsilon && cd_a * cd_b < -epsilon * epsilon {
        return true;
    }
    (ab_c.abs() <= epsilon && on_segment(a, b, c, epsilon.sqrt()))
        || (ab_d.abs() <= epsilon && on_segment(a, b, d, epsilon.sqrt()))
        || (cd_a.abs() <= epsilon && on_segment(c, d, a, epsilon.sqrt()))
        || (cd_b.abs() <= epsilon && on_segment(c, d, b, epsilon.sqrt()))
}

fn point_in_or_on_triangle(
    point: (f64, f64),
    a: (f64, f64),
    b: (f64, f64),
    c: (f64, f64),
    orientation: f64,
    epsilon: f64,
) -> bool {
    orientation * orientation2(a, b, point) >= -epsilon
        && orientation * orientation2(b, c, point) >= -epsilon
        && orientation * orientation2(c, a, point) >= -epsilon
}

fn resolve_placement(
    id: u64,
    model: &Model,
    active: &mut HashSet<u64>,
    depth: usize,
) -> Result<Matrix4> {
    if depth > MAX_IFC_DEPTH {
        return Err(Error::LimitExceeded(format!(
            "IFC local placement chain exceeds {MAX_IFC_DEPTH}"
        )));
    }
    if !active.insert(id) {
        return Err(Error::InvalidInput(format!(
            "IFC local placement cycle detected at #{id}"
        )));
    }
    let placement = model.placements.get(&id).ok_or_else(|| {
        Error::InvalidInput(format!(
            "IFC product references missing local placement #{id}"
        ))
    })?;
    let relative = axis_placement_matrix(placement.relative, model)?;
    let output = if let Some(parent) = placement.parent {
        resolve_placement(parent, model, active, depth + 1)?.multiply(relative)
    } else {
        relative
    };
    active.remove(&id);
    Ok(output)
}

fn axis_placement_matrix(id: u64, model: &Model) -> Result<Matrix4> {
    let axis = model.axes.get(&id).ok_or_else(|| {
        Error::InvalidInput(format!(
            "IFC placement references missing Axis2Placement3D #{id}"
        ))
    })?;
    let origin = *model.points.get(&axis.location).ok_or_else(|| {
        Error::InvalidInput(format!(
            "IFC axis placement references missing Cartesian point #{}",
            axis.location
        ))
    })?;
    if model.point_dimensions.get(&axis.location) != Some(&3) {
        return Err(Error::InvalidInput(format!(
            "IFC Axis2Placement3D #{id} location is not three-dimensional"
        )));
    }
    for direction_id in [axis.axis, axis.ref_direction].into_iter().flatten() {
        if model.direction_dimensions.get(&direction_id) != Some(&3) {
            return Err(Error::InvalidInput(format!(
                "IFC Axis2Placement3D #{id} direction #{direction_id} is not three-dimensional"
            )));
        }
    }
    let z = match axis.axis {
        Some(id) => model
            .directions
            .get(&id)
            .copied()
            .and_then(Vec3::normalized),
        None => Some(Vec3 {
            x: 0.0,
            y: 0.0,
            z: 1.0,
        }),
    }
    .ok_or_else(|| Error::InvalidInput("IFC Axis direction is invalid".into()))?;
    let x_hint = match axis.ref_direction {
        Some(id) => model.directions.get(&id).copied(),
        None => Some(Vec3 {
            x: 1.0,
            y: 0.0,
            z: 0.0,
        }),
    }
    .ok_or_else(|| Error::InvalidInput("IFC RefDirection is missing".into()))?;
    let projected = x_hint.sub(z.scale(x_hint.dot(z)));
    let x = projected
        .normalized()
        .ok_or_else(|| Error::InvalidInput("IFC Axis and RefDirection are parallel".into()))?;
    let y = z
        .cross(x)
        .normalized()
        .ok_or_else(|| Error::InvalidInput("IFC placement basis is degenerate".into()))?;
    Ok(Matrix4::from_axes(x, y, z, origin))
}

fn axis_placement_2d_matrix(id: u64, model: &Model) -> Result<Matrix4> {
    let placement = model.axes_2d.get(&id).ok_or_else(|| {
        Error::InvalidInput(format!("IFC profile references missing 2D placement #{id}"))
    })?;
    if model.point_dimensions.get(&placement.location) != Some(&2) {
        return Err(Error::InvalidInput(format!(
            "IFC 2D placement #{id} location is not two-dimensional"
        )));
    }
    let origin = *model.points.get(&placement.location).ok_or_else(|| {
        Error::InvalidInput(format!(
            "IFC 2D placement #{id} references missing Cartesian point #{}",
            placement.location
        ))
    })?;
    let x_hint = if let Some(direction_id) = placement.ref_direction {
        if model.direction_dimensions.get(&direction_id) != Some(&2) {
            return Err(Error::InvalidInput(format!(
                "IFC 2D placement #{id} direction #{direction_id} is not two-dimensional"
            )));
        }
        *model.directions.get(&direction_id).ok_or_else(|| {
            Error::InvalidInput(format!(
                "IFC 2D placement references missing direction #{direction_id}"
            ))
        })?
    } else {
        Vec3 {
            x: 1.0,
            y: 0.0,
            z: 0.0,
        }
    };
    let x = Vec3 {
        x: x_hint.x,
        y: x_hint.y,
        z: 0.0,
    }
    .normalized()
    .ok_or_else(|| {
        Error::InvalidInput(format!(
            "IFC 2D placement #{id} has an invalid RefDirection"
        ))
    })?;
    let y = Vec3 {
        x: -x.y,
        y: x.x,
        z: 0.0,
    };
    Ok(Matrix4::from_axes(
        x,
        y,
        Vec3 {
            x: 0.0,
            y: 0.0,
            z: 1.0,
        },
        origin,
    ))
}

fn build_extruded_mesh(
    id: u64,
    solid: ExtrudedAreaSolid,
    model: &Model,
    edge_checks: &mut usize,
    remaining_vertices: usize,
) -> Result<Option<TriangleMesh>> {
    let Some(profile) = model.profiles.get(&solid.swept_area) else {
        return Ok(None);
    };
    if model.unsupported_profiles.contains(&solid.swept_area) {
        return Ok(None);
    }
    let (mut coordinates, profile_position) = match *profile {
        Profile::Rectangle {
            position,
            x_dim,
            y_dim,
        } => {
            if !positive_finite_length(x_dim) || !positive_finite_length(y_dim) {
                return Err(Error::InvalidInput(format!(
                    "IFC rectangle profile #{} has invalid dimensions",
                    solid.swept_area
                )));
            }
            (
                vec![
                    Vec3 {
                        x: -x_dim / 2.0,
                        y: -y_dim / 2.0,
                        z: 0.0,
                    },
                    Vec3 {
                        x: x_dim / 2.0,
                        y: -y_dim / 2.0,
                        z: 0.0,
                    },
                    Vec3 {
                        x: x_dim / 2.0,
                        y: y_dim / 2.0,
                        z: 0.0,
                    },
                    Vec3 {
                        x: -x_dim / 2.0,
                        y: y_dim / 2.0,
                        z: 0.0,
                    },
                ],
                position,
            )
        }
        Profile::Circle { position, radius } => {
            if !positive_finite_length(radius) {
                return Err(Error::InvalidInput(format!(
                    "IFC circle profile #{} has invalid radius",
                    solid.swept_area
                )));
            }
            let points = (0..IFC_CIRCLE_PROFILE_SEGMENTS)
                .map(|index| {
                    let angle =
                        std::f64::consts::TAU * index as f64 / IFC_CIRCLE_PROFILE_SEGMENTS as f64;
                    Vec3 {
                        x: radius * angle.cos(),
                        y: radius * angle.sin(),
                        z: 0.0,
                    }
                })
                .collect();
            (points, position)
        }
        Profile::ArbitraryClosed { outer_curve } => {
            let Some(point_ids) = model.polylines.get(&outer_curve) else {
                return Ok(None);
            };
            if point_ids.len() < 4 || point_ids.len() > MAX_IFC_POLYLINE_VERTICES + 1 {
                return Ok(None);
            }
            let mut points = Vec::with_capacity(point_ids.len());
            for point_id in point_ids {
                if model.point_dimensions.get(point_id) != Some(&2) {
                    return Ok(None);
                }
                let Some(point) = model.points.get(point_id).copied() else {
                    return Err(Error::InvalidInput(format!(
                        "IFC profile polyline #{outer_curve} references missing point #{point_id}"
                    )));
                };
                points.push(point);
            }
            let first = points[0];
            let last = *points.last().unwrap();
            let scale = points
                .iter()
                .map(|point| point.sub(first).dot(point.sub(first)).sqrt())
                .fold(0.0_f64, f64::max)
                .max(1.0);
            if first.sub(last).dot(first.sub(last)) > scale * scale * 1e-20 {
                // IFC polyline closure is explicit: its first and last points
                // must coincide for a profile boundary.
                return Ok(None);
            }
            points.pop();
            (points, None)
        }
    };
    if coordinates.len() < 3 || coordinates.len() > MAX_IFC_POLYLINE_VERTICES {
        return Ok(None);
    }
    let vertex_count = coordinates
        .len()
        .checked_mul(2)
        .ok_or_else(|| Error::LimitExceeded("IFC extruded solid vertex count overflowed".into()))?;
    if vertex_count > remaining_vertices {
        return Err(Error::LimitExceeded(format!(
            "IFC generated profile vertices exceed {MAX_IFC_GENERATED_PROFILE_VERTICES}"
        )));
    }
    if let Some(position_id) = profile_position {
        let position = axis_placement_2d_matrix(position_id, model)?;
        for point in &mut coordinates {
            *point = position.transform(*point);
        }
    }
    let area_twice = (0..coordinates.len())
        .map(|index| {
            let current = coordinates[index];
            let next = coordinates[(index + 1) % coordinates.len()];
            current.x * next.y - next.x * current.y
        })
        .sum::<f64>();
    if !area_twice.is_finite() || area_twice.abs() <= 1e-12 {
        return Ok(None);
    }
    if area_twice < 0.0 {
        coordinates.reverse();
    }
    let indices = (1..=coordinates.len()).collect::<Vec<_>>();
    let Some(cap_triangles) = triangulate_planar_polygon(&indices, &coordinates, edge_checks)?
    else {
        return Ok(None);
    };

    if !positive_finite_length(solid.depth) {
        return Err(Error::InvalidInput(format!(
            "IFC extruded solid #{id} has invalid depth"
        )));
    }
    if model.direction_dimensions.get(&solid.direction) != Some(&3) {
        return Err(Error::InvalidInput(format!(
            "IFC extruded solid #{id} direction is not three-dimensional"
        )));
    }
    let direction = model
        .directions
        .get(&solid.direction)
        .copied()
        .ok_or_else(|| {
            Error::InvalidInput(format!(
                "IFC extruded solid #{id} references missing direction #{}",
                solid.direction
            ))
        })?;
    let direction = direction.normalized().ok_or_else(|| {
        Error::InvalidInput(format!(
            "IFC extruded solid #{id} has a zero extrusion direction"
        ))
    })?;
    if direction.z.abs() <= 1e-12 {
        return Err(Error::InvalidInput(format!(
            "IFC extruded solid #{id} direction is perpendicular to its profile normal"
        )));
    }
    let solid_position = match solid.position {
        Some(position_id) => axis_placement_matrix(position_id, model)?,
        None => Matrix4::IDENTITY,
    };
    let mut vertices = Vec::with_capacity(vertex_count);
    for point in &coordinates {
        vertices.push(solid_position.transform(*point));
    }
    for point in &coordinates {
        let top = Vec3 {
            x: point.x + direction.x * solid.depth,
            y: point.y + direction.y * solid.depth,
            z: point.z + direction.z * solid.depth,
        };
        vertices.push(solid_position.transform(top));
    }
    if vertices.iter().any(|point| {
        ![point.x, point.y, point.z]
            .iter()
            .all(|value| value.is_finite() && value.abs() <= 1e12)
    }) {
        return Err(Error::InvalidInput(format!(
            "IFC extruded solid #{id} coordinates are non-finite or outside ±1e12"
        )));
    }

    let count = coordinates.len();
    let mut faces = Vec::with_capacity(cap_triangles.len() * 2 + count * 2);
    for triangle in cap_triangles {
        faces.push([triangle[2], triangle[1], triangle[0]]);
        faces.push([
            triangle[0] + count,
            triangle[1] + count,
            triangle[2] + count,
        ]);
    }
    for index in 0..count {
        let next = (index + 1) % count;
        let a = index + 1;
        let b = next + 1;
        faces.push([a, b, b + count]);
        faces.push([a, b + count, a + count]);
    }
    Ok(Some((vertices, faces)))
}

fn positive_finite_length(value: f64) -> bool {
    value.is_finite() && value > 0.0 && value <= 1e12
}

fn parse_model(input: &str) -> Result<(Model, bool)> {
    let data_start = find_data_start(input.as_bytes())?;
    let bytes = input.as_bytes();
    let mut cursor = data_start;
    let mut model = Model::default();
    let mut value_nodes = 0usize;
    let mut entity_count = 0usize;
    let mut entity_ids = HashSet::new();
    let mut total_points = 0usize;
    let mut total_triangles = 0usize;
    let mut total_polyline_points = 0usize;
    let mut unsupported_items = false;
    while cursor < bytes.len() {
        skip_space_and_comments(bytes, &mut cursor)?;
        if cursor >= bytes.len() || starts_word(bytes, cursor, b"ENDSEC") {
            break;
        }
        if bytes[cursor] != b'#' {
            // Blank lexical adornments and optional header-like data are not
            // entity instances; skip one token while looking for the next #.
            cursor += 1;
            continue;
        }
        entity_count += 1;
        if entity_count > MAX_IFC_ENTITIES {
            return Err(Error::LimitExceeded(format!(
                "IFC entity count exceeds {MAX_IFC_ENTITIES}"
            )));
        }
        let (id, name, fields, next) = parse_entity(bytes, cursor, &mut value_nodes)?;
        cursor = next;
        if !entity_ids.insert(id) {
            return Err(Error::InvalidInput(format!(
                "IFC file repeats instance identifier #{id}"
            )));
        }
        match name.as_str() {
            "IFCCARTESIANPOINT" => {
                if let Some(point) = point_from_list(fields.first())? {
                    let dimension = match fields.first() {
                        Some(Value::List(values)) => values.len(),
                        _ => 0,
                    };
                    model.points.insert(id, point);
                    model.point_dimensions.insert(id, dimension);
                }
            }
            "IFCDIRECTION" => {
                if let Some(point) = point_from_list(fields.first())? {
                    let dimension = match fields.first() {
                        Some(Value::List(values)) => values.len(),
                        _ => 0,
                    };
                    model.directions.insert(id, point);
                    model.direction_dimensions.insert(id, dimension);
                }
            }
            "IFCCARTESIANPOINTLIST3D" => {
                let Some(Value::List(raw_points)) = fields.first() else {
                    return Err(Error::InvalidInput(format!(
                        "IFC Cartesian point list #{id} is malformed"
                    )));
                };
                let mut points = Vec::with_capacity(raw_points.len());
                for (index, value) in raw_points.iter().enumerate() {
                    let point = point_from_list(Some(value))?.ok_or_else(|| {
                        Error::InvalidInput(format!(
                            "IFC point list #{id} point {} is malformed",
                            index + 1
                        ))
                    })?;
                    let Value::List(ordinates) = value else {
                        unreachable!()
                    };
                    if ordinates.len() != 3 {
                        return Err(Error::InvalidInput(format!(
                            "IFC point list #{id} point {} is not three-dimensional",
                            index + 1
                        )));
                    }
                    points.push(point);
                }
                total_points = total_points
                    .checked_add(points.len())
                    .ok_or_else(|| Error::LimitExceeded("IFC point count overflowed".into()))?;
                if total_points > MAX_IFC_POINTS || model.point_lists.len() >= MAX_IFC_POINT_LISTS {
                    return Err(Error::LimitExceeded(format!(
                        "IFC point lists exceed {MAX_IFC_POINTS} points or {MAX_IFC_POINT_LISTS} lists"
                    )));
                }
                model.point_lists.insert(id, points);
            }
            "IFCTRIANGULATEDFACESET" => {
                if model.face_sets.len() + model.polygon_sets.len() >= MAX_IFC_FACESETS {
                    return Err(Error::LimitExceeded(format!(
                        "IFC face set count exceeds {MAX_IFC_FACESETS}"
                    )));
                }
                let point_list = field_ref(&fields, 0).ok_or_else(|| {
                    Error::InvalidInput(format!(
                        "IFC triangulated face set #{id} has no coordinates"
                    ))
                })?;
                let faces = face_index_list(fields.get(3), id)?;
                total_triangles = total_triangles
                    .checked_add(faces.len())
                    .ok_or_else(|| Error::LimitExceeded("IFC triangle count overflowed".into()))?;
                if total_triangles > MAX_IFC_TRIANGLES {
                    return Err(Error::LimitExceeded(format!(
                        "IFC triangles exceed {MAX_IFC_TRIANGLES}"
                    )));
                }
                let pn_index = optional_index_list(fields.get(4), id)?;
                model.face_sets.insert(
                    id,
                    FaceSet {
                        point_list,
                        faces,
                        pn_index,
                    },
                );
            }
            "IFCPOLYGONALFACESET" => {
                if model.face_sets.len() + model.polygon_sets.len() >= MAX_IFC_FACESETS {
                    return Err(Error::LimitExceeded(format!(
                        "IFC face set count exceeds {MAX_IFC_FACESETS}"
                    )));
                }
                let point_list = field_ref(&fields, 0).ok_or_else(|| {
                    Error::InvalidInput(format!("IFC polygonal face set #{id} has no coordinates"))
                })?;
                let faces = refs_from_list(fields.get(2), "IFC polygon face list")?;
                let pn_index = optional_index_list(fields.get(3), id)?;
                model.polygon_sets.insert(id, (point_list, faces, pn_index));
            }
            "IFCINDEXEDPOLYGONALFACE" => {
                let indices = integer_list(fields.first(), "IFC indexed polygon face")?;
                model.polygon_faces.insert(id, indices);
            }
            "IFCINDEXEDPOLYGONALFACEWITHVOIDS" => {
                let indices = integer_list(fields.first(), "IFC indexed polygon face with voids")?;
                model.polygon_faces.insert(id, indices);
                unsupported_items = true;
            }
            "IFCLOCALPLACEMENT" => {
                let relative = field_ref(&fields, 1).ok_or_else(|| {
                    Error::InvalidInput(format!(
                        "IFC local placement #{id} has no relative placement"
                    ))
                })?;
                model.placements.insert(
                    id,
                    LocalPlacement {
                        parent: field_ref(&fields, 0),
                        relative,
                    },
                );
            }
            "IFCAXIS2PLACEMENT3D" => {
                let location = field_ref(&fields, 0).ok_or_else(|| {
                    Error::InvalidInput(format!("IFC axis placement #{id} has no location"))
                })?;
                model.axes.insert(
                    id,
                    AxisPlacement {
                        location,
                        axis: field_ref(&fields, 1),
                        ref_direction: field_ref(&fields, 2),
                    },
                );
            }
            "IFCAXIS2PLACEMENT2D" => {
                let location = field_ref(&fields, 0).ok_or_else(|| {
                    Error::InvalidInput(format!("IFC 2D axis placement #{id} has no location"))
                })?;
                model.axes_2d.insert(
                    id,
                    AxisPlacement2D {
                        location,
                        ref_direction: optional_ref(&fields, 1, "IFC 2D RefDirection")?,
                    },
                );
            }
            "IFCPOLYLINE" => {
                if model.polylines.len() >= MAX_IFC_POINT_LISTS {
                    return Err(Error::LimitExceeded(format!(
                        "IFC polyline count exceeds {MAX_IFC_POINT_LISTS}"
                    )));
                }
                let point_ids = refs_from_list(fields.first(), "IFC polyline point list")?;
                if point_ids.len() < 2 || point_ids.len() > MAX_IFC_POLYLINE_VERTICES + 1 {
                    return Err(Error::LimitExceeded(format!(
                        "IFC polyline #{id} has an invalid point count"
                    )));
                }
                total_polyline_points = total_polyline_points
                    .checked_add(point_ids.len())
                    .ok_or_else(|| {
                        Error::LimitExceeded("IFC polyline point count overflowed".into())
                    })?;
                if total_polyline_points > MAX_IFC_POLYLINE_VERTICES_TOTAL {
                    return Err(Error::LimitExceeded(format!(
                        "IFC polyline point references exceed {MAX_IFC_POLYLINE_VERTICES_TOTAL}"
                    )));
                }
                model.polylines.insert(id, point_ids);
            }
            "IFCRECTANGLEPROFILEDEF" => {
                if model.profiles.len() + model.unsupported_profiles.len() >= MAX_IFC_FACESETS {
                    return Err(Error::LimitExceeded(format!(
                        "IFC profile count exceeds {MAX_IFC_FACESETS}"
                    )));
                }
                let x_dim = required_number(&fields, 3, "IFC rectangle XDim")?;
                let y_dim = required_number(&fields, 4, "IFC rectangle YDim")?;
                model.profiles.insert(
                    id,
                    Profile::Rectangle {
                        position: optional_ref(&fields, 2, "IFC profile Position")?,
                        x_dim,
                        y_dim,
                    },
                );
            }
            "IFCCIRCLEPROFILEDEF" => {
                if model.profiles.len() + model.unsupported_profiles.len() >= MAX_IFC_FACESETS {
                    return Err(Error::LimitExceeded(format!(
                        "IFC profile count exceeds {MAX_IFC_FACESETS}"
                    )));
                }
                model.profiles.insert(
                    id,
                    Profile::Circle {
                        position: optional_ref(&fields, 2, "IFC profile Position")?,
                        radius: required_number(&fields, 3, "IFC circle Radius")?,
                    },
                );
            }
            "IFCARBITRARYCLOSEDPROFILEDEF" => {
                if model.profiles.len() + model.unsupported_profiles.len() >= MAX_IFC_FACESETS {
                    return Err(Error::LimitExceeded(format!(
                        "IFC profile count exceeds {MAX_IFC_FACESETS}"
                    )));
                }
                let outer_curve = field_ref(&fields, 2).ok_or_else(|| {
                    Error::InvalidInput(format!(
                        "IFC arbitrary closed profile #{id} has no OuterCurve"
                    ))
                })?;
                model
                    .profiles
                    .insert(id, Profile::ArbitraryClosed { outer_curve });
            }
            "IFCARBITRARYPROFILEDEFWITHVOIDS" => {
                if model.profiles.len() + model.unsupported_profiles.len() >= MAX_IFC_FACESETS {
                    return Err(Error::LimitExceeded(format!(
                        "IFC profile count exceeds {MAX_IFC_FACESETS}"
                    )));
                }
                model.unsupported_profiles.insert(id);
                unsupported_items = true;
            }
            "IFCEXTRUDEDAREASOLID" => {
                if model.extruded_solids.len() >= MAX_IFC_FACESETS {
                    return Err(Error::LimitExceeded(format!(
                        "IFC extruded solid count exceeds {MAX_IFC_FACESETS}"
                    )));
                }
                let swept_area = field_ref(&fields, 0).ok_or_else(|| {
                    Error::InvalidInput(format!("IFC extruded solid #{id} has no SweptArea"))
                })?;
                let direction = field_ref(&fields, 2).ok_or_else(|| {
                    Error::InvalidInput(format!(
                        "IFC extruded solid #{id} has no ExtrudedDirection"
                    ))
                })?;
                let depth = required_number(&fields, 3, "IFC extrusion Depth")?;
                model.extruded_solids.insert(
                    id,
                    ExtrudedAreaSolid {
                        swept_area,
                        position: optional_ref(&fields, 1, "IFC swept-solid Position")?,
                        direction,
                        depth,
                    },
                );
            }
            "IFCREPRESENTATIONMAP" => {
                let origin = field_ref(&fields, 0).ok_or_else(|| {
                    Error::InvalidInput(format!(
                        "IFC representation map #{id} has no MappingOrigin"
                    ))
                })?;
                let representation = field_ref(&fields, 1).ok_or_else(|| {
                    Error::InvalidInput(format!(
                        "IFC representation map #{id} has no MappedRepresentation"
                    ))
                })?;
                model.representation_maps.insert(
                    id,
                    RepresentationMap {
                        origin,
                        representation,
                    },
                );
            }
            "IFCMAPPEDITEM" => {
                let source = field_ref(&fields, 0).ok_or_else(|| {
                    Error::InvalidInput(format!("IFC mapped item #{id} has no MappingSource"))
                })?;
                let target = field_ref(&fields, 1).ok_or_else(|| {
                    Error::InvalidInput(format!("IFC mapped item #{id} has no MappingTarget"))
                })?;
                model.mapped_items.insert(id, MappedItem { source, target });
            }
            "IFCCARTESIANTRANSFORMATIONOPERATOR3D" => {
                if fields.len() != 5 {
                    return Err(Error::InvalidInput(format!(
                        "IFC 3D transformation operator #{id} has {} attributes, expected 5",
                        fields.len()
                    )));
                }
                let origin = field_ref(&fields, 2).ok_or_else(|| {
                    Error::InvalidInput(format!(
                        "IFC transformation operator #{id} has no LocalOrigin"
                    ))
                })?;
                let scale = optional_number(&fields, 3, "IFC transformation Scale")?.unwrap_or(1.0);
                model.transformation_operators.insert(
                    id,
                    TransformationOperator {
                        axis1: optional_ref(&fields, 0, "IFC transformation Axis1")?,
                        axis2: optional_ref(&fields, 1, "IFC transformation Axis2")?,
                        origin,
                        scale,
                        axis3: optional_ref(&fields, 4, "IFC transformation Axis3")?,
                    },
                );
            }
            "IFCCARTESIANTRANSFORMATIONOPERATOR3DNONUNIFORM"
            | "IFCCARTESIANTRANSFORMATIONOPERATOR2D"
            | "IFCCARTESIANTRANSFORMATIONOPERATOR2DNONUNIFORM" => {
                model.unsupported_transforms.insert(id);
            }
            "IFCPRODUCTDEFINITIONSHAPE" => {
                model.product_shapes.insert(
                    id,
                    refs_from_list(fields.get(2), "IFC product representation list")?,
                );
            }
            "IFCSHAPEREPRESENTATION" | "IFCREPRESENTATION" => {
                model.representations.insert(
                    id,
                    refs_from_list(fields.get(3), "IFC representation item list")?,
                );
            }
            other
                if other.starts_with("IFC")
                    && fields.len() >= 7
                    && field_ref(&fields, 6).is_some() =>
            {
                model.products.push(Product {
                    placement: field_ref(&fields, 5),
                    representation: field_ref(&fields, 6),
                });
            }
            other
                if other.starts_with("IFC")
                    && (other.contains("SOLID")
                        || other.contains("BREP")
                        || other.contains("BOOLEAN")
                        || other.contains("CSG")
                        || other.contains("SURFACE")
                        || other.contains("MAPPEDITEM")
                        || other.contains("EXTRUDED")) =>
            {
                unsupported_items = true;
            }
            _ => {}
        }
    }
    if model.points.len() > MAX_IFC_POINTS {
        return Err(Error::LimitExceeded(format!(
            "IFC Cartesian points exceed {MAX_IFC_POINTS}"
        )));
    }
    Ok((model, unsupported_items))
}

struct IfcXmlIndex<'a> {
    nodes: Vec<&'a XmlElement>,
    node_ids: HashMap<usize, u64>,
    nodes_by_id: HashMap<u64, &'a XmlElement>,
    external_ids: HashMap<String, u64>,
}

impl<'a> IfcXmlIndex<'a> {
    fn new(root: &'a XmlElement) -> Result<Self> {
        let mut nodes = Vec::new();
        collect_xml_nodes(root, &mut nodes);
        if nodes.len() > MAX_IFC_XML_NODES {
            return Err(Error::LimitExceeded(format!(
                "IFCXML node count exceeds {MAX_IFC_XML_NODES}"
            )));
        }
        let mut index = Self {
            nodes,
            node_ids: HashMap::new(),
            nodes_by_id: HashMap::new(),
            external_ids: HashMap::new(),
        };
        let mut next_id = 1u64;
        for node in &index.nodes {
            let xml_id = xml_attribute(node, "id");
            if xml_id.is_none() && !is_ifcxml_indexed_node(node) {
                continue;
            }
            let id = next_id;
            next_id = next_id
                .checked_add(1)
                .ok_or_else(|| Error::LimitExceeded("IFCXML internal ID overflowed".into()))?;
            let pointer = *node as *const XmlElement as usize;
            index.node_ids.insert(pointer, id);
            index.nodes_by_id.insert(id, node);
            if let Some(xml_id) = xml_id {
                let key = normalize_ifcxml_reference(xml_id);
                if index.external_ids.insert(key.clone(), id).is_some() {
                    return Err(Error::InvalidInput(format!(
                        "IFCXML repeats internal XML ID '{key}'"
                    )));
                }
            }
        }
        Ok(index)
    }

    fn id(&self, node: &XmlElement) -> Option<u64> {
        self.node_ids
            .get(&(node as *const XmlElement as usize))
            .copied()
    }

    fn node_for_reference(&self, reference: &str) -> Option<&'a XmlElement> {
        let key = normalize_ifcxml_reference(reference);
        let id = self.external_ids.get(&key)?;
        self.nodes_by_id.get(id).copied()
    }

    fn resolve_value(&self, node: &'a XmlElement) -> Option<&'a XmlElement> {
        if let Some(reference) = xml_attribute(node, "href").or_else(|| xml_attribute(node, "ref"))
        {
            return self.node_for_reference(reference);
        }
        if xml_attribute(node, "nil").is_some_and(|value| value.eq_ignore_ascii_case("true")) {
            return None;
        }
        if xml_entity_type(node).is_some() || is_ifcxml_geometry_value(node) {
            return Some(node);
        }
        node.children
            .iter()
            .find_map(|child| self.resolve_value(child))
    }

    fn field(&self, node: &'a XmlElement, name: &str) -> Option<&'a XmlElement> {
        let child = node.children.iter().find(|child| child.name == name)?;
        self.resolve_value(child)
    }

    fn field_id(&self, node: &'a XmlElement, name: &str) -> Option<u64> {
        self.id(self.field(node, name)?)
    }

    fn field_list(&self, node: &'a XmlElement, name: &str) -> Vec<u64> {
        let Some(container) = node.children.iter().find(|child| child.name == name) else {
            return Vec::new();
        };
        let mut output = Vec::new();
        self.collect_list_values(container, &mut output);
        output
    }

    fn collect_list_values(&self, node: &'a XmlElement, output: &mut Vec<u64>) {
        if let Some(reference) = xml_attribute(node, "href").or_else(|| xml_attribute(node, "ref"))
        {
            if let Some(target) = self.node_for_reference(reference)
                && let Some(id) = self.id(target)
            {
                output.push(id);
            }
            return;
        }
        if xml_attribute(node, "nil").is_some_and(|value| value.eq_ignore_ascii_case("true")) {
            return;
        }
        if xml_entity_type(node).is_some()
            && let Some(id) = self.id(node)
        {
            output.push(id);
            return;
        }
        for child in &node.children {
            self.collect_list_values(child, output);
        }
    }
}

fn collect_xml_nodes<'a>(node: &'a XmlElement, output: &mut Vec<&'a XmlElement>) {
    output.push(node);
    for child in &node.children {
        collect_xml_nodes(child, output);
    }
}

fn normalize_ifcxml_reference(reference: &str) -> String {
    reference.trim().trim_start_matches('#').to_owned()
}

fn xml_attribute<'a>(node: &'a XmlElement, name: &str) -> Option<&'a str> {
    node.attribute(name).or_else(|| {
        node.attributes
            .iter()
            .find(|(key, _)| key.rsplit(':').next() == Some(name))
            .map(|(_, value)| value.as_str())
    })
}

fn xml_entity_type(node: &XmlElement) -> Option<&str> {
    if node.name.starts_with("Ifc") {
        return Some(&node.name);
    }
    let type_name = xml_attribute(node, "type")?.rsplit(':').next()?;
    type_name.starts_with("Ifc").then_some(type_name)
}

fn is_ifcxml_geometry_value(node: &XmlElement) -> bool {
    xml_attribute(node, "Coordinates").is_some()
        || xml_attribute(node, "DirectionRatios").is_some()
        || xml_attribute(node, "CoordList").is_some()
        || node.name == "Location"
            && node
                .children
                .iter()
                .any(|child| child.name == "Coordinates")
}

fn is_ifcxml_indexed_node(node: &XmlElement) -> bool {
    xml_entity_type(node).is_some() || is_ifcxml_geometry_value(node)
}

fn parse_ifcxml_model(bytes: &[u8], options: &ConvertOptions) -> Result<(Model, bool)> {
    let limits = XmlLimits {
        max_events: options.max_xml_events,
        max_nodes: MAX_IFC_XML_NODES,
        max_depth: MAX_IFC_DEPTH,
        max_text_bytes: MAX_IFC_XML_TEXT_BYTES,
    };
    let root = parse_xml_tree(bytes, &limits, "IFCXML")?;
    let namespace = root.namespace.as_deref().unwrap_or_default();
    if !root.name.eq_ignore_ascii_case("ifcXML")
        || !(namespace.starts_with("http://www.buildingsmart-tech.org/ifcXML/")
            || namespace.starts_with("http://www.iai-tech.org/ifcXML/"))
    {
        return Err(Error::InvalidInput(
            "input does not contain a supported buildingSMART IFCXML root".into(),
        ));
    }
    let index = IfcXmlIndex::new(&root)?;
    let mut model = Model::default();
    let mut unsupported_items = false;
    let mut point_count = 0usize;
    let mut total_polyline_points = 0usize;
    let mut total_triangles = 0usize;
    let mut total_polygon_face_refs = 0usize;
    let mut total_polygon_indices = 0usize;
    let mut entity_count = 0usize;

    for node in &index.nodes {
        if xml_attribute(node, "href").is_some() || xml_attribute(node, "ref").is_some() {
            // IFCXML reference elements carry the target ID on the field
            // wrapper; they are not independent entity definitions.
            continue;
        }
        if xml_entity_type(node).is_some() {
            entity_count = entity_count.saturating_add(1);
            if entity_count > MAX_IFC_ENTITIES {
                return Err(Error::LimitExceeded(format!(
                    "IFCXML entity count exceeds {MAX_IFC_ENTITIES}"
                )));
            }
        }
        let Some(id) = index.id(node) else {
            continue;
        };
        let entity_type = xml_entity_type(node).unwrap_or("");
        let is_cartesian_point = entity_type == "IfcCartesianPoint"
            || entity_type.is_empty()
                && (xml_attribute(node, "Coordinates").is_some()
                    || node.name == "Location"
                        && node
                            .children
                            .iter()
                            .any(|child| child.name == "Coordinates"));
        if is_cartesian_point {
            if let Some(values) = xml_field_numeric_values(node, "Coordinates")? {
                let point = point_from_xml_coordinates(&values, "IFCXML Cartesian point")?;
                point_count = point_count.saturating_add(1);
                if point_count > MAX_IFC_POINTS {
                    return Err(Error::LimitExceeded(format!(
                        "IFCXML Cartesian points exceed {MAX_IFC_POINTS}"
                    )));
                }
                model.points.insert(id, point);
                model.point_dimensions.insert(id, values.len());
            }
            continue;
        }
        let is_direction = entity_type == "IfcDirection"
            || entity_type.is_empty() && xml_attribute(node, "DirectionRatios").is_some();
        if is_direction {
            if let Some(values) = xml_field_numeric_values(node, "DirectionRatios")? {
                let direction = point_from_xml_coordinates(&values, "IFCXML direction")?;
                model.directions.insert(id, direction);
                model.direction_dimensions.insert(id, values.len());
            }
            continue;
        }
        match entity_type {
            "IfcCartesianPointList3D" => {
                let Some(values) = xml_attribute_or_child_numbers(
                    node,
                    "CoordList",
                    MAX_IFC_POINTS.saturating_mul(3),
                    "IFCXML point list",
                )?
                else {
                    return Err(Error::InvalidInput(format!(
                        "IFCXML point list #{id} has no CoordList"
                    )));
                };
                if values.len() % 3 != 0 {
                    return Err(Error::InvalidInput(format!(
                        "IFCXML point list #{id} CoordList is not a sequence of XYZ triples"
                    )));
                }
                let mut points = Vec::with_capacity(values.len() / 3);
                for chunk in values.chunks_exact(3) {
                    points.push(point_from_xml_coordinates(chunk, "IFCXML 3D point list")?);
                }
                point_count = point_count.checked_add(points.len()).ok_or_else(|| {
                    Error::LimitExceeded("IFCXML source point count overflowed".into())
                })?;
                if point_count > MAX_IFC_POINTS {
                    return Err(Error::LimitExceeded(format!(
                        "IFCXML source points exceed {MAX_IFC_POINTS}"
                    )));
                }
                model.point_lists.insert(id, points);
            }
            "IfcTriangulatedFaceSet" => {
                if model.face_sets.len() + model.polygon_sets.len() >= MAX_IFC_FACESETS {
                    return Err(Error::LimitExceeded(format!(
                        "IFCXML face set count exceeds {MAX_IFC_FACESETS}"
                    )));
                }
                let point_list = index.field_id(node, "Coordinates").ok_or_else(|| {
                    Error::InvalidInput(format!(
                        "IFCXML triangulated face set #{id} has no Coordinates reference"
                    ))
                })?;
                let Some(indices) = xml_attribute_or_child_indices(
                    node,
                    "CoordIndex",
                    MAX_IFC_TRIANGLES.saturating_mul(3),
                    "IFCXML triangle index list",
                )?
                else {
                    return Err(Error::InvalidInput(format!(
                        "IFCXML triangulated face set #{id} has no CoordIndex"
                    )));
                };
                if indices.len() % 3 != 0 {
                    return Err(Error::InvalidInput(format!(
                        "IFCXML triangulated face set #{id} CoordIndex is not grouped in triples"
                    )));
                }
                let mut faces = Vec::with_capacity(indices.len() / 3);
                for chunk in indices.chunks_exact(3) {
                    faces.push([chunk[0], chunk[1], chunk[2]]);
                }
                total_triangles = total_triangles.checked_add(faces.len()).ok_or_else(|| {
                    Error::LimitExceeded("IFCXML triangle count overflowed".into())
                })?;
                if total_triangles > MAX_IFC_TRIANGLES {
                    return Err(Error::LimitExceeded(format!(
                        "IFCXML triangles exceed {MAX_IFC_TRIANGLES}"
                    )));
                }
                let pn_index = xml_optional_attribute_or_child_indices(
                    node,
                    "PnIndex",
                    MAX_IFC_POINTS,
                    "IFCXML PnIndex",
                )?;
                model.face_sets.insert(
                    id,
                    FaceSet {
                        point_list,
                        faces,
                        pn_index,
                    },
                );
            }
            "IfcPolygonalFaceSet" => {
                if model.face_sets.len() + model.polygon_sets.len() >= MAX_IFC_FACESETS {
                    return Err(Error::LimitExceeded(format!(
                        "IFCXML face set count exceeds {MAX_IFC_FACESETS}"
                    )));
                }
                let point_list = index.field_id(node, "Coordinates").ok_or_else(|| {
                    Error::InvalidInput(format!(
                        "IFCXML polygon face set #{id} has no Coordinates reference"
                    ))
                })?;
                let face_ids = index.field_list(node, "Faces");
                total_polygon_face_refs = total_polygon_face_refs
                    .checked_add(face_ids.len())
                    .ok_or_else(|| {
                        Error::LimitExceeded("IFCXML polygon face count overflowed".into())
                    })?;
                if total_polygon_face_refs > MAX_IFC_TRIANGLES {
                    return Err(Error::LimitExceeded(format!(
                        "IFCXML polygon face references exceed {MAX_IFC_TRIANGLES}"
                    )));
                }
                let pn_index = xml_optional_attribute_or_child_indices(
                    node,
                    "PnIndex",
                    MAX_IFC_POINTS,
                    "IFCXML PnIndex",
                )?;
                model
                    .polygon_sets
                    .insert(id, (point_list, face_ids, pn_index));
            }
            "IfcIndexedPolygonalFace" | "IfcIndexedPolygonalFaceWithVoids" => {
                let Some(indices) = xml_attribute_or_child_indices(
                    node,
                    "CoordIndex",
                    MAX_IFC_POLYLINE_VERTICES,
                    "IFCXML indexed face",
                )?
                else {
                    return Err(Error::InvalidInput(format!(
                        "IFCXML indexed face #{id} has no CoordIndex"
                    )));
                };
                if indices.len() > MAX_IFC_POLYLINE_VERTICES {
                    return Err(Error::LimitExceeded(format!(
                        "IFCXML indexed polygon face #{id} exceeds {MAX_IFC_POLYLINE_VERTICES} vertices"
                    )));
                }
                total_polygon_indices = total_polygon_indices
                    .checked_add(indices.len())
                    .ok_or_else(|| {
                        Error::LimitExceeded("IFCXML polygon index count overflowed".into())
                    })?;
                if total_polygon_indices > MAX_IFC_VALUE_NODES {
                    return Err(Error::LimitExceeded(format!(
                        "IFCXML polygon indices exceed {MAX_IFC_VALUE_NODES}"
                    )));
                }
                model.polygon_faces.insert(id, indices);
                if entity_type == "IfcIndexedPolygonalFaceWithVoids" {
                    unsupported_items = true;
                }
            }
            "IfcAxis2Placement3D" => {
                let location = index.field_id(node, "Location").ok_or_else(|| {
                    Error::InvalidInput(format!("IFCXML 3D axis placement #{id} has no Location"))
                })?;
                model.axes.insert(
                    id,
                    AxisPlacement {
                        location,
                        axis: index.field_id(node, "Axis"),
                        ref_direction: index.field_id(node, "RefDirection"),
                    },
                );
            }
            "IfcAxis2Placement2D" => {
                let location = index.field_id(node, "Location").ok_or_else(|| {
                    Error::InvalidInput(format!("IFCXML 2D axis placement #{id} has no Location"))
                })?;
                model.axes_2d.insert(
                    id,
                    AxisPlacement2D {
                        location,
                        ref_direction: index.field_id(node, "RefDirection"),
                    },
                );
            }
            "IfcLocalPlacement" => {
                let relative = index.field_id(node, "RelativePlacement").ok_or_else(|| {
                    Error::InvalidInput(format!(
                        "IFCXML local placement #{id} has no RelativePlacement"
                    ))
                })?;
                model.placements.insert(
                    id,
                    LocalPlacement {
                        parent: index.field_id(node, "PlacementRelTo"),
                        relative,
                    },
                );
            }
            "IfcProductDefinitionShape" => {
                model
                    .product_shapes
                    .insert(id, index.field_list(node, "Representations"));
            }
            "IfcShapeRepresentation" | "IfcRepresentation" => {
                model
                    .representations
                    .insert(id, index.field_list(node, "Items"));
            }
            "IfcPolyline" => {
                let points = index.field_list(node, "Points");
                if points.len() < 2 || points.len() > MAX_IFC_POLYLINE_VERTICES + 1 {
                    return Err(Error::LimitExceeded(format!(
                        "IFCXML polyline #{id} has an invalid point count"
                    )));
                }
                total_polyline_points = total_polyline_points
                    .checked_add(points.len())
                    .ok_or_else(|| {
                        Error::LimitExceeded("IFCXML polyline point count overflowed".into())
                    })?;
                if total_polyline_points > MAX_IFC_POLYLINE_VERTICES_TOTAL {
                    return Err(Error::LimitExceeded(format!(
                        "IFCXML polyline point references exceed {MAX_IFC_POLYLINE_VERTICES_TOTAL}"
                    )));
                }
                model.polylines.insert(id, points);
            }
            "IfcRectangleProfileDef" => {
                if model.profiles.len() + model.unsupported_profiles.len() >= MAX_IFC_FACESETS {
                    return Err(Error::LimitExceeded(format!(
                        "IFCXML profile count exceeds {MAX_IFC_FACESETS}"
                    )));
                }
                model.profiles.insert(
                    id,
                    Profile::Rectangle {
                        position: index.field_id(node, "Position"),
                        x_dim: xml_required_number_attribute(
                            node,
                            "XDim",
                            "IFCXML rectangle XDim",
                        )?,
                        y_dim: xml_required_number_attribute(
                            node,
                            "YDim",
                            "IFCXML rectangle YDim",
                        )?,
                    },
                );
            }
            "IfcCircleProfileDef" => {
                if model.profiles.len() + model.unsupported_profiles.len() >= MAX_IFC_FACESETS {
                    return Err(Error::LimitExceeded(format!(
                        "IFCXML profile count exceeds {MAX_IFC_FACESETS}"
                    )));
                }
                model.profiles.insert(
                    id,
                    Profile::Circle {
                        position: index.field_id(node, "Position"),
                        radius: xml_required_number_attribute(
                            node,
                            "Radius",
                            "IFCXML circle Radius",
                        )?,
                    },
                );
            }
            "IfcArbitraryClosedProfileDef" => {
                if model.profiles.len() + model.unsupported_profiles.len() >= MAX_IFC_FACESETS {
                    return Err(Error::LimitExceeded(format!(
                        "IFCXML profile count exceeds {MAX_IFC_FACESETS}"
                    )));
                }
                let outer_curve = index.field_id(node, "OuterCurve").ok_or_else(|| {
                    Error::InvalidInput(format!(
                        "IFCXML arbitrary closed profile #{id} has no OuterCurve"
                    ))
                })?;
                model
                    .profiles
                    .insert(id, Profile::ArbitraryClosed { outer_curve });
            }
            "IfcArbitraryProfileDefWithVoids" => {
                if model.profiles.len() + model.unsupported_profiles.len() >= MAX_IFC_FACESETS {
                    return Err(Error::LimitExceeded(format!(
                        "IFCXML profile count exceeds {MAX_IFC_FACESETS}"
                    )));
                }
                model.unsupported_profiles.insert(id);
                unsupported_items = true;
            }
            "IfcExtrudedAreaSolid" => {
                if model.extruded_solids.len() >= MAX_IFC_FACESETS {
                    return Err(Error::LimitExceeded(format!(
                        "IFCXML extruded solid count exceeds {MAX_IFC_FACESETS}"
                    )));
                }
                let swept_area = index.field_id(node, "SweptArea").ok_or_else(|| {
                    Error::InvalidInput(format!("IFCXML extrusion #{id} has no SweptArea"))
                })?;
                let direction = index.field_id(node, "ExtrudedDirection").ok_or_else(|| {
                    Error::InvalidInput(format!("IFCXML extrusion #{id} has no ExtrudedDirection"))
                })?;
                let depth = xml_required_number_attribute(node, "Depth", "IFCXML extrusion Depth")?;
                model.extruded_solids.insert(
                    id,
                    ExtrudedAreaSolid {
                        swept_area,
                        position: index.field_id(node, "Position"),
                        direction,
                        depth,
                    },
                );
            }
            "IfcRepresentationMap" => {
                let origin = index.field_id(node, "MappingOrigin").ok_or_else(|| {
                    Error::InvalidInput(format!(
                        "IFCXML representation map #{id} has no MappingOrigin"
                    ))
                })?;
                let representation =
                    index
                        .field_id(node, "MappedRepresentation")
                        .ok_or_else(|| {
                            Error::InvalidInput(format!(
                                "IFCXML representation map #{id} has no MappedRepresentation"
                            ))
                        })?;
                model.representation_maps.insert(
                    id,
                    RepresentationMap {
                        origin,
                        representation,
                    },
                );
            }
            "IfcMappedItem" => {
                let source = index.field_id(node, "MappingSource").ok_or_else(|| {
                    Error::InvalidInput(format!("IFCXML mapped item #{id} has no MappingSource"))
                })?;
                let target = index.field_id(node, "MappingTarget").ok_or_else(|| {
                    Error::InvalidInput(format!("IFCXML mapped item #{id} has no MappingTarget"))
                })?;
                model.mapped_items.insert(id, MappedItem { source, target });
            }
            "IfcCartesianTransformationOperator3D" => {
                let origin = index.field_id(node, "LocalOrigin").ok_or_else(|| {
                    Error::InvalidInput(format!(
                        "IFCXML transformation operator #{id} has no LocalOrigin"
                    ))
                })?;
                model.transformation_operators.insert(
                    id,
                    TransformationOperator {
                        axis1: index.field_id(node, "Axis1"),
                        axis2: index.field_id(node, "Axis2"),
                        origin,
                        scale: xml_optional_number_attribute(node, "Scale")?.unwrap_or(1.0),
                        axis3: index.field_id(node, "Axis3"),
                    },
                );
            }
            _ => {
                if entity_type
                    .eq_ignore_ascii_case("IfcCartesianTransformationOperator3DnonUniform")
                    || entity_type.eq_ignore_ascii_case("IfcCartesianTransformationOperator2D")
                    || entity_type
                        .eq_ignore_ascii_case("IfcCartesianTransformationOperator2DnonUniform")
                {
                    model.unsupported_transforms.insert(id);
                }
                let uppercase = entity_type.to_ascii_uppercase();
                if uppercase.contains("SOLID")
                    || uppercase.contains("BREP")
                    || uppercase.contains("BOOLEAN")
                    || uppercase.contains("CSG")
                    || uppercase.contains("SURFACE")
                    || uppercase.contains("MAPPEDITEM")
                    || uppercase.contains("EXTRUDED")
                {
                    unsupported_items = true;
                }
            }
        }

        if let Some(representation) = index.field_id(node, "Representation") {
            model.products.push(Product {
                placement: index.field_id(node, "ObjectPlacement"),
                representation: Some(representation),
            });
        }
    }
    Ok((model, unsupported_items))
}

fn xml_field_numeric_values(node: &XmlElement, name: &str) -> Result<Option<Vec<f64>>> {
    xml_attribute_or_child_numbers(node, name, 3, "IFCXML coordinate value")
}

fn xml_optional_number_attribute(node: &XmlElement, name: &str) -> Result<Option<f64>> {
    let Some(text) = xml_attribute_or_child_text(node, name) else {
        return Ok(None);
    };
    let values = parse_ifcxml_numbers(&text, 1, "IFCXML numeric attribute")?;
    if values.len() != 1 {
        return Err(Error::InvalidInput(format!(
            "IFCXML attribute '{name}' must contain one number"
        )));
    }
    Ok(values.into_iter().next())
}

fn xml_required_number_attribute(node: &XmlElement, name: &str, context: &str) -> Result<f64> {
    xml_optional_number_attribute(node, name)?
        .ok_or_else(|| Error::InvalidInput(format!("{context} is missing or not numeric")))
}

fn xml_optional_attribute_or_child_indices(
    node: &XmlElement,
    name: &str,
    max_values: usize,
    context: &str,
) -> Result<Option<Vec<usize>>> {
    let Some(text) = xml_attribute_or_child_text(node, name) else {
        return Ok(None);
    };
    parse_ifcxml_indices(&text, max_values, context).map(Some)
}

fn xml_attribute_or_child_indices(
    node: &XmlElement,
    name: &str,
    max_values: usize,
    context: &str,
) -> Result<Option<Vec<usize>>> {
    xml_optional_attribute_or_child_indices(node, name, max_values, context)
}

fn xml_attribute_or_child_numbers(
    node: &XmlElement,
    name: &str,
    max_values: usize,
    context: &str,
) -> Result<Option<Vec<f64>>> {
    let Some(text) = xml_attribute_or_child_text(node, name) else {
        return Ok(None);
    };
    parse_ifcxml_numbers(&text, max_values, context).map(Some)
}

fn xml_attribute_or_child_text(node: &XmlElement, name: &str) -> Option<String> {
    if let Some(value) = xml_attribute(node, name) {
        return Some(value.to_owned());
    }
    let child = node.children.iter().find(|child| child.name == name)?;
    let mut output = String::new();
    collect_xml_text(child, &mut output);
    (!output.trim().is_empty()).then_some(output)
}

fn collect_xml_text(node: &XmlElement, output: &mut String) {
    if !node.text.is_empty() {
        output.push(' ');
        output.push_str(&node.text);
    }
    for child in &node.children {
        collect_xml_text(child, output);
    }
}

fn parse_ifcxml_numbers(text: &str, max_values: usize, context: &str) -> Result<Vec<f64>> {
    let mut values = Vec::with_capacity(text.len().min(max_values.saturating_mul(4)) / 2);
    for token in text.split_whitespace() {
        if values.len() >= max_values {
            return Err(Error::LimitExceeded(format!(
                "{context} exceeds {max_values} numeric values"
            )));
        }
        // buildingSMART IFCXML examples serialize decimal fractions with a
        // comma while separating list values with XML whitespace.
        let normalized = token.replace(',', ".");
        let value = normalized
            .parse::<f64>()
            .ok()
            .filter(|value| value.is_finite())
            .ok_or_else(|| Error::InvalidInput(format!("{context} contains an invalid number")))?;
        values.push(value);
    }
    Ok(values)
}

fn parse_ifcxml_indices(text: &str, max_values: usize, context: &str) -> Result<Vec<usize>> {
    parse_ifcxml_numbers(text, max_values, context)?
        .into_iter()
        .map(|value| {
            if value >= 1.0 && value <= usize::MAX as f64 && value.fract() == 0.0 {
                Ok(value as usize)
            } else {
                Err(Error::InvalidInput(format!(
                    "{context} contains a non-positive or non-integral index"
                )))
            }
        })
        .collect()
}

fn point_from_xml_coordinates(values: &[f64], context: &str) -> Result<Vec3> {
    if values.len() != 2 && values.len() != 3 {
        return Err(Error::InvalidInput(format!(
            "{context} must have two or three coordinates"
        )));
    }
    if values.iter().any(|value| value.abs() > 1e12) {
        return Err(Error::InvalidInput(format!("{context} is outside ±1e12")));
    }
    Ok(Vec3 {
        x: values[0],
        y: values[1],
        z: values.get(2).copied().unwrap_or(0.0),
    })
}

fn parse_entity(
    bytes: &[u8],
    start: usize,
    value_nodes: &mut usize,
) -> Result<(u64, String, Vec<Value>, usize)> {
    let mut cursor = start + 1;
    let id_start = cursor;
    while bytes.get(cursor).is_some_and(u8::is_ascii_digit) {
        cursor += 1;
    }
    let id = std::str::from_utf8(&bytes[id_start..cursor])
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|id| *id != 0)
        .ok_or_else(|| {
            Error::InvalidInput("IFC entity has an invalid instance identifier".into())
        })?;
    skip_space_and_comments(bytes, &mut cursor)?;
    if bytes.get(cursor) != Some(&b'=') {
        return Err(Error::InvalidInput(format!(
            "IFC entity #{id} is missing '='"
        )));
    }
    cursor += 1;
    skip_space_and_comments(bytes, &mut cursor)?;
    let type_start = cursor;
    while bytes.get(cursor).is_some_and(u8::is_ascii_alphanumeric) {
        cursor += 1;
    }
    if type_start == cursor {
        return Err(Error::InvalidInput(format!(
            "IFC entity #{id} has no type name"
        )));
    }
    let name = std::str::from_utf8(&bytes[type_start..cursor])
        .map_err(|_| Error::InvalidInput("IFC entity type is not ASCII".into()))?
        .to_ascii_uppercase();
    skip_space_and_comments(bytes, &mut cursor)?;
    if bytes.get(cursor) != Some(&b'(') {
        return Err(Error::InvalidInput(format!(
            "IFC entity #{id} has no argument list"
        )));
    }
    let open = cursor;
    let close = find_matching_paren(bytes, open)?;
    if close - start > MAX_IFC_RECORD_BYTES {
        return Err(Error::LimitExceeded(format!(
            "IFC entity #{id} exceeds {MAX_IFC_RECORD_BYTES} bytes"
        )));
    }
    let mut parser = ValueParser {
        bytes: &bytes[open + 1..close],
        cursor: 0,
        nodes: value_nodes,
    };
    let fields = parser.parse_fields()?;
    cursor = close + 1;
    skip_space_and_comments(bytes, &mut cursor)?;
    if bytes.get(cursor) != Some(&b';') {
        return Err(Error::InvalidInput(format!(
            "IFC entity #{id} is missing ';'"
        )));
    }
    Ok((id, name, fields, cursor + 1))
}

fn find_matching_paren(bytes: &[u8], open: usize) -> Result<usize> {
    let mut depth = 0usize;
    let mut cursor = open;
    let mut in_string = false;
    let mut in_comment = false;
    while cursor < bytes.len() {
        let byte = bytes[cursor];
        if in_comment {
            if byte == b'*' && bytes.get(cursor + 1) == Some(&b'/') {
                in_comment = false;
                cursor += 2;
                continue;
            }
        } else if in_string {
            if byte == b'\'' {
                if bytes.get(cursor + 1) == Some(&b'\'') {
                    cursor += 2;
                    continue;
                }
                in_string = false;
            }
        } else if byte == b'/' && bytes.get(cursor + 1) == Some(&b'*') {
            in_comment = true;
            cursor += 2;
            continue;
        } else if byte == b'\'' {
            in_string = true;
        } else if byte == b'(' {
            depth += 1;
            if depth > MAX_IFC_DEPTH {
                return Err(Error::LimitExceeded(format!(
                    "IFC argument nesting exceeds {MAX_IFC_DEPTH}"
                )));
            }
        } else if byte == b')' {
            depth = depth.saturating_sub(1);
            if depth == 0 {
                return Ok(cursor);
            }
        }
        cursor += 1;
    }
    Err(Error::InvalidInput(
        "IFC entity argument list is unterminated".into(),
    ))
}

struct ValueParser<'a, 'n> {
    bytes: &'a [u8],
    cursor: usize,
    nodes: &'n mut usize,
}

impl ValueParser<'_, '_> {
    fn parse_fields(&mut self) -> Result<Vec<Value>> {
        let mut fields = Vec::new();
        loop {
            self.skip_space_and_comments()?;
            if self.cursor >= self.bytes.len() {
                return Err(Error::InvalidInput(
                    "IFC argument list ended unexpectedly".into(),
                ));
            }
            fields.push(self.parse_value(0)?);
            self.skip_space_and_comments()?;
            match self.bytes.get(self.cursor) {
                Some(b',') => self.cursor += 1,
                None => return Ok(fields),
                _ => {
                    return Err(Error::InvalidInput(
                        "invalid IFC entity argument separator".into(),
                    ));
                }
            }
        }
    }

    fn parse_value(&mut self, depth: usize) -> Result<Value> {
        self.bump_node()?;
        self.skip_space_and_comments()?;
        if depth > MAX_IFC_DEPTH {
            return Err(Error::LimitExceeded(format!(
                "IFC argument nesting exceeds {MAX_IFC_DEPTH}"
            )));
        }
        match self.bytes.get(self.cursor).copied() {
            Some(b'(') => {
                self.cursor += 1;
                let mut values = Vec::new();
                loop {
                    self.skip_space_and_comments()?;
                    if self.bytes.get(self.cursor) == Some(&b')') {
                        self.cursor += 1;
                        break;
                    }
                    values.push(self.parse_value(depth + 1)?);
                    self.skip_space_and_comments()?;
                    match self.bytes.get(self.cursor) {
                        Some(b',') => self.cursor += 1,
                        Some(b')') => {}
                        _ => {
                            return Err(Error::InvalidInput("invalid IFC list separator".into()));
                        }
                    }
                }
                Ok(Value::List(values))
            }
            Some(b'#') => {
                self.cursor += 1;
                let start = self.cursor;
                while self.bytes.get(self.cursor).is_some_and(u8::is_ascii_digit) {
                    self.cursor += 1;
                }
                let value = std::str::from_utf8(&self.bytes[start..self.cursor])
                    .ok()
                    .and_then(|value| value.parse::<u64>().ok())
                    .filter(|value| *value > 0)
                    .ok_or_else(|| Error::InvalidInput("invalid IFC reference token".into()))?;
                Ok(Value::Ref(value))
            }
            Some(b'\'') => {
                self.cursor += 1;
                while let Some(byte) = self.bytes.get(self.cursor).copied() {
                    self.cursor += 1;
                    if byte == b'\'' {
                        if self.bytes.get(self.cursor) == Some(&b'\'') {
                            self.cursor += 1;
                            continue;
                        }
                        return Ok(Value::Other);
                    }
                }
                Err(Error::InvalidInput("unterminated IFC quoted string".into()))
            }
            Some(b'.') => {
                self.cursor += 1;
                let start = self.cursor;
                while self.cursor < self.bytes.len() && self.bytes[self.cursor] != b'.' {
                    self.cursor += 1;
                }
                if self.cursor >= self.bytes.len() {
                    return Err(Error::InvalidInput("unterminated IFC enum value".into()));
                }
                let _value = String::from_utf8_lossy(&self.bytes[start..self.cursor]);
                self.cursor += 1;
                Ok(Value::Other)
            }
            Some(b'$' | b'*') => {
                self.cursor += 1;
                Ok(Value::Other)
            }
            Some(_) => {
                let start = self.cursor;
                while self.cursor < self.bytes.len()
                    && !matches!(self.bytes[self.cursor], b',' | b')' | b'(')
                    && !self.bytes[self.cursor].is_ascii_whitespace()
                {
                    self.cursor += 1;
                }
                if start == self.cursor {
                    return Err(Error::InvalidInput("empty IFC value token".into()));
                }
                let token = std::str::from_utf8(&self.bytes[start..self.cursor]).unwrap_or("");
                if let Some(number) = token.parse::<f64>().ok().filter(|value| value.is_finite()) {
                    Ok(Value::Number(number))
                } else if self.bytes.get(self.cursor) == Some(&b'(') {
                    // STEP typed parameters, such as IFCLENGTHMEASURE(25.4),
                    // can appear inside otherwise ignored IFC metadata.
                    self.cursor += 1;
                    loop {
                        self.skip_space_and_comments()?;
                        if self.bytes.get(self.cursor) == Some(&b')') {
                            self.cursor += 1;
                            break;
                        }
                        let _ = self.parse_value(depth + 1)?;
                        self.skip_space_and_comments()?;
                        match self.bytes.get(self.cursor) {
                            Some(b',') => self.cursor += 1,
                            Some(b')') => {}
                            _ => {
                                return Err(Error::InvalidInput(
                                    "invalid IFC typed parameter separator".into(),
                                ));
                            }
                        }
                    }
                    Ok(Value::Other)
                } else {
                    Ok(Value::Other)
                }
            }
            None => Err(Error::InvalidInput("missing IFC value".into())),
        }
    }

    fn bump_node(&mut self) -> Result<()> {
        *self.nodes = self
            .nodes
            .checked_add(1)
            .ok_or_else(|| Error::LimitExceeded("IFC value node count overflowed".into()))?;
        if *self.nodes > MAX_IFC_VALUE_NODES {
            return Err(Error::LimitExceeded(format!(
                "IFC parsed value count exceeds {MAX_IFC_VALUE_NODES}"
            )));
        }
        Ok(())
    }

    fn skip_space_and_comments(&mut self) -> Result<()> {
        skip_space_and_comments(self.bytes, &mut self.cursor)
    }
}

fn skip_space_and_comments(bytes: &[u8], cursor: &mut usize) -> Result<()> {
    loop {
        while bytes.get(*cursor).is_some_and(u8::is_ascii_whitespace) {
            *cursor += 1;
        }
        if bytes.get(*cursor) == Some(&b'/') && bytes.get(*cursor + 1) == Some(&b'*') {
            *cursor += 2;
            let start = *cursor;
            while *cursor + 1 < bytes.len()
                && !(bytes[*cursor] == b'*' && bytes[*cursor + 1] == b'/')
            {
                *cursor += 1;
            }
            if *cursor + 1 >= bytes.len() {
                return Err(Error::InvalidInput("unterminated IFC comment".into()));
            }
            *cursor += 2;
            if *cursor - start > MAX_IFC_RECORD_BYTES {
                return Err(Error::LimitExceeded(format!(
                    "IFC comment exceeds {MAX_IFC_RECORD_BYTES} bytes"
                )));
            }
            continue;
        }
        return Ok(());
    }
}

fn find_data_start(bytes: &[u8]) -> Result<usize> {
    let mut cursor = 0usize;
    let mut in_string = false;
    let mut in_comment = false;
    while cursor < bytes.len() {
        if in_comment {
            if bytes[cursor] == b'*' && bytes.get(cursor + 1) == Some(&b'/') {
                in_comment = false;
                cursor += 2;
                continue;
            }
        } else if in_string {
            if bytes[cursor] == b'\'' {
                if bytes.get(cursor + 1) == Some(&b'\'') {
                    cursor += 2;
                    continue;
                }
                in_string = false;
            }
        } else if bytes[cursor] == b'/' && bytes.get(cursor + 1) == Some(&b'*') {
            in_comment = true;
            cursor += 2;
            continue;
        } else if bytes[cursor] == b'\'' {
            in_string = true;
        } else if starts_word(bytes, cursor, b"DATA") {
            let mut end = cursor + 4;
            let mut parens = 0usize;
            let mut quote = false;
            while end < bytes.len() {
                match bytes[end] {
                    b'\'' => quote = !quote,
                    b'(' if !quote => parens += 1,
                    b')' if !quote => parens = parens.saturating_sub(1),
                    b';' if !quote && parens == 0 => return Ok(end + 1),
                    _ => {}
                }
                end += 1;
            }
            return Err(Error::InvalidInput(
                "IFC DATA section is unterminated".into(),
            ));
        }
        cursor += 1;
    }
    Err(Error::InvalidInput("IFC file has no DATA section".into()))
}

fn starts_word(bytes: &[u8], cursor: usize, word: &[u8]) -> bool {
    bytes
        .get(cursor..cursor + word.len())
        .is_some_and(|candidate| candidate.eq_ignore_ascii_case(word))
        && (cursor == 0 || !bytes[cursor - 1].is_ascii_alphanumeric())
        && bytes
            .get(cursor + word.len())
            .is_none_or(|byte| !byte.is_ascii_alphanumeric())
}

fn point_from_list(value: Option<&Value>) -> Result<Option<Vec3>> {
    let Some(Value::List(values)) = value else {
        return Ok(None);
    };
    if values.len() != 2 && values.len() != 3 {
        return Err(Error::InvalidInput(
            "IFC Cartesian coordinate must have two or three ordinates".into(),
        ));
    }
    let mut coords = [0.0; 3];
    for (index, value) in values.iter().enumerate() {
        let Value::Number(number) = value else {
            return Err(Error::InvalidInput(
                "IFC Cartesian coordinate contains a non-numeric value".into(),
            ));
        };
        if number.abs() > 1e12 {
            return Err(Error::InvalidInput(
                "IFC coordinate is outside ±1e12".into(),
            ));
        }
        coords[index] = *number;
    }
    Ok(Some(Vec3 {
        x: coords[0],
        y: coords[1],
        z: coords[2],
    }))
}

fn field_ref(fields: &[Value], index: usize) -> Option<u64> {
    match fields.get(index) {
        Some(Value::Ref(id)) => Some(*id),
        _ => None,
    }
}

fn optional_ref(fields: &[Value], index: usize, context: &str) -> Result<Option<u64>> {
    match fields.get(index) {
        None | Some(Value::Other) => Ok(None),
        Some(Value::Ref(id)) => Ok(Some(*id)),
        _ => Err(Error::InvalidInput(format!(
            "{context} must be a reference or unset"
        ))),
    }
}

fn optional_number(fields: &[Value], index: usize, context: &str) -> Result<Option<f64>> {
    match fields.get(index) {
        None | Some(Value::Other) => Ok(None),
        Some(Value::Number(number)) => Ok(Some(*number)),
        _ => Err(Error::InvalidInput(format!(
            "{context} must be numeric or unset"
        ))),
    }
}

fn required_number(fields: &[Value], index: usize, context: &str) -> Result<f64> {
    match fields.get(index) {
        Some(Value::Number(number)) => Ok(*number),
        _ => Err(Error::InvalidInput(format!(
            "{context} is missing or not numeric"
        ))),
    }
}

fn refs_from_list(value: Option<&Value>, context: &str) -> Result<Vec<u64>> {
    let Some(Value::List(values)) = value else {
        return Ok(Vec::new());
    };
    values
        .iter()
        .map(|value| match value {
            Value::Ref(id) => Ok(*id),
            _ => Err(Error::InvalidInput(format!(
                "{context} contains a non-reference"
            ))),
        })
        .collect()
}

fn face_index_list(value: Option<&Value>, id: u64) -> Result<Vec<[usize; 3]>> {
    let Some(Value::List(values)) = value else {
        return Err(Error::InvalidInput(format!(
            "IFC triangulated face set #{id} has no CoordIndex"
        )));
    };
    values
        .iter()
        .map(|value| {
            let Value::List(indices) = value else {
                return Err(Error::InvalidInput(format!(
                    "IFC triangulated face set #{id} has a malformed triangle"
                )));
            };
            if indices.len() != 3 {
                return Err(Error::InvalidInput(format!(
                    "IFC triangulated face set #{id} triangle must have exactly three indices"
                )));
            }
            let mut output = [0; 3];
            for (index, value) in indices.iter().enumerate() {
                output[index] = positive_index(value, id)?;
            }
            Ok(output)
        })
        .collect()
}

fn integer_list(value: Option<&Value>, context: &str) -> Result<Vec<usize>> {
    let Some(Value::List(values)) = value else {
        return Err(Error::InvalidInput(format!("{context} has no index list")));
    };
    values
        .iter()
        .map(|value| match value {
            Value::Number(number)
                if *number >= 1.0 && *number <= usize::MAX as f64 && number.fract() == 0.0 =>
            {
                Ok(*number as usize)
            }
            _ => Err(Error::InvalidInput(format!(
                "{context} has an invalid positive index"
            ))),
        })
        .collect()
}

fn optional_index_list(value: Option<&Value>, id: u64) -> Result<Option<Vec<usize>>> {
    match value {
        None | Some(Value::Other) => Ok(None),
        Some(Value::List(_)) => {
            integer_list(value, &format!("IFC face set #{id} PnIndex")).map(Some)
        }
        _ => Err(Error::InvalidInput(format!(
            "IFC face set #{id} PnIndex has an invalid value"
        ))),
    }
}

fn positive_index(value: &Value, id: u64) -> Result<usize> {
    match value {
        Value::Number(number)
            if *number >= 1.0 && *number <= usize::MAX as f64 && number.fract() == 0.0 =>
        {
            Ok(*number as usize)
        }
        _ => Err(Error::InvalidInput(format!(
            "IFC face set #{id} has an invalid positive coordinate index"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TRIANGULATED: &str = "ISO-10303-21;\nHEADER;\nFILE_SCHEMA(('IFC4'));\nENDSEC;\nDATA;\n#1=IFCCARTESIANPOINTLIST3D(((0.,0.,0.),(1.,0.,0.),(0.,1.,0.)),$);\n#2=IFCTRIANGULATEDFACESET(#1,$,.F.,((1,2,3)),$);\nENDSEC;\nEND-ISO-10303-21;";

    #[test]
    fn parses_one_based_triangle_indices_and_point_lists() {
        let (model, _) = parse_model(TRIANGULATED).unwrap();
        assert_eq!(model.point_lists.get(&1).unwrap().len(), 3);
        assert_eq!(model.face_sets.get(&2).unwrap().faces, vec![[1, 2, 3]]);
    }

    #[test]
    fn honors_indexed_polygonal_face_sets() {
        let text = "ISO-10303-21;HEADER;FILE_SCHEMA(('IFC4'));ENDSEC;DATA;#1=IFCCARTESIANPOINTLIST3D(((0.,0.,0.),(1.,0.,0.),(1.,1.,0.),(0.,1.,0.)),$);#2=IFCINDEXEDPOLYGONALFACE((1,2,3,4));#3=IFCPOLYGONALFACESET(#1,.F.,(#2),$);ENDSEC;END-ISO-10303-21;";
        let (model, _) = parse_model(text).unwrap();
        let mut faces = model.face_sets;
        let (point_list, face_ids, pn_index) = model.polygon_sets.get(&3).unwrap();
        assert_eq!(face_ids, &vec![2]);
        assert_eq!(pn_index, &None);
        assert_eq!(model.polygon_faces.get(&2).unwrap(), &vec![1, 2, 3, 4]);
        faces.insert(
            3,
            FaceSet {
                point_list: *point_list,
                faces: vec![[1, 2, 3], [1, 3, 4]],
                pn_index: None,
            },
        );
        assert_eq!(faces.get(&3).unwrap().faces.len(), 2);
    }

    #[test]
    fn composes_mapped_representation_target_and_product_placement() {
        let input = include_str!("../../tests/fixtures/sample_mapped.ifc");
        let (model, unsupported) = parse_model(input).unwrap();
        assert!(!unsupported);
        assert_eq!(model.mapped_items.len(), 2);

        let product = resolve_placement(20, &model, &mut HashSet::new(), 0).unwrap();
        let mapping_origin = axis_placement_matrix(5, &model).unwrap();
        let target = transformation_operator_matrix(11, &model).unwrap();
        let transform = product
            .multiply(target)
            .multiply(mapping_origin.inverse_rigid());
        let local = model.point_lists.get(&1).unwrap()[0];
        let first = transform.transform(local);
        assert!((first.x - 15.0).abs() < 1e-9);
        assert!((first.y - 18.0).abs() < 1e-9);
        assert!(first.z.abs() < 1e-9);

        let second_target = transformation_operator_matrix(14, &model).unwrap();
        let second = product
            .multiply(second_target)
            .multiply(mapping_origin.inverse_rigid())
            .transform(local);
        assert!((second.x - 24.0).abs() < 1e-9);
        assert!((second.y - 20.0).abs() < 1e-9);
        assert!(second.z.abs() < 1e-9);
    }

    #[test]
    fn tessellates_bounded_ifc_extruded_rectangle_circle_and_polyline_profiles() {
        let input = include_str!("../../tests/fixtures/sample_extruded.ifc");
        let (model, unsupported) = parse_model(input).unwrap();
        assert!(!unsupported);
        assert_eq!(model.extruded_solids.len(), 3);

        let mut checks = 0;
        let rectangle = build_extruded_mesh(
            19,
            model.extruded_solids[&19],
            &model,
            &mut checks,
            MAX_IFC_GENERATED_PROFILE_VERTICES,
        )
        .unwrap()
        .unwrap();
        assert_eq!(rectangle.0.len(), 8);
        assert_eq!(rectangle.1.len(), 12);
        assert!(rectangle.0.iter().any(|point| {
            (point.x + 2.0).abs() < 1e-9 && (point.y + 1.0).abs() < 1e-9 && point.z.abs() < 1e-9
        }));
        assert!(rectangle.0.iter().any(|point| {
            (point.x - 2.0).abs() < 1e-9
                && (point.y - 1.0).abs() < 1e-9
                && (point.z - 3.0).abs() < 1e-9
        }));

        let circle = build_extruded_mesh(
            22,
            model.extruded_solids[&22],
            &model,
            &mut checks,
            MAX_IFC_GENERATED_PROFILE_VERTICES,
        )
        .unwrap()
        .unwrap();
        assert_eq!(circle.0.len(), IFC_CIRCLE_PROFILE_SEGMENTS * 2);
        assert_eq!(circle.1.len(), IFC_CIRCLE_PROFILE_SEGMENTS * 4 - 4);
        assert!(
            circle
                .0
                .iter()
                .all(|point| (point.z >= -1e-9) && (point.z <= 2.0 + 1e-9))
        );
        assert!(
            (circle
                .0
                .iter()
                .map(|point| point.x)
                .fold(f64::INFINITY, f64::min)
                - 13.5)
                .abs()
                < 1e-9
        );

        let arbitrary = build_extruded_mesh(
            25,
            model.extruded_solids[&25],
            &model,
            &mut checks,
            MAX_IFC_GENERATED_PROFILE_VERTICES,
        )
        .unwrap()
        .unwrap();
        assert_eq!(arbitrary.0.len(), 12);
        assert_eq!(arbitrary.1.len(), 20);
        assert!(arbitrary.0.iter().all(|point| point.y >= 4.0 - 1e-9));
    }

    #[test]
    fn resolves_ifcxml_internal_href_ids_and_decimal_comma_coordinates() {
        let input = include_bytes!("../../tests/fixtures/sample_ifcxml.ifcxml");
        let (model, unsupported) = parse_ifcxml_model(input, &ConvertOptions::default()).unwrap();
        assert!(!unsupported);
        assert_eq!(model.products.len(), 1);
        let product_placement = model.products[0].placement.unwrap();
        assert!(model.placements[&product_placement].parent.is_some());
        assert_eq!(model.mapped_items.len(), 1);
        assert_eq!(model.representation_maps.len(), 1);
        assert_eq!(model.transformation_operators.len(), 1);
        assert!(
            model
                .point_lists
                .values()
                .flatten()
                .any(|point| (point.x - 1.5).abs() < 1e-9)
        );

        let mapped_item = model.mapped_items.values().next().unwrap();
        let representation_map = model.representation_maps[&mapped_item.source];
        let target = transformation_operator_matrix(mapped_item.target, &model).unwrap();
        let origin = axis_placement_matrix(representation_map.origin, &model).unwrap();
        let product = resolve_placement(product_placement, &model, &mut HashSet::new(), 0).unwrap();
        let representation = model.representations[&representation_map.representation]
            .first()
            .unwrap();
        let face_set = &model.face_sets[representation];
        let source_point = model.point_lists[&face_set.point_list][0];
        let rendered = product
            .multiply(target)
            .multiply(origin.inverse_rigid())
            .transform(source_point);
        assert!((rendered.x - 6010.0).abs() < 1e-9);
        assert!((rendered.y - 20.0).abs() < 1e-9);
        assert!(rendered.z.abs() < 1e-9);
    }

    #[test]
    fn fan_and_ear_clipping_preserve_convex_and_concave_polygons() {
        let square = [
            Vec3 {
                x: 0.0,
                y: 0.0,
                z: 0.0,
            },
            Vec3 {
                x: 2.0,
                y: 0.0,
                z: 0.0,
            },
            Vec3 {
                x: 2.0,
                y: 2.0,
                z: 0.0,
            },
            Vec3 {
                x: 0.0,
                y: 2.0,
                z: 0.0,
            },
        ];
        let mut checks = 0;
        let triangles = triangulate_planar_polygon(&[1, 2, 3, 4], &square, &mut checks)
            .unwrap()
            .unwrap();
        assert_eq!(triangles, vec![[1, 2, 3], [1, 3, 4]]);

        let concave = [
            Vec3 {
                x: 0.0,
                y: 0.0,
                z: 0.0,
            },
            Vec3 {
                x: 2.0,
                y: 0.0,
                z: 0.0,
            },
            Vec3 {
                x: 2.0,
                y: 1.0,
                z: 0.0,
            },
            Vec3 {
                x: 1.0,
                y: 1.0,
                z: 0.0,
            },
            Vec3 {
                x: 1.0,
                y: 2.0,
                z: 0.0,
            },
            Vec3 {
                x: 0.0,
                y: 2.0,
                z: 0.0,
            },
        ];
        let triangles = triangulate_planar_polygon(&[1, 2, 3, 4, 5, 6], &concave, &mut checks)
            .unwrap()
            .unwrap();
        assert_eq!(triangles.len(), 4);
        let area = triangles
            .iter()
            .map(|[a, b, c]| {
                let a = concave[*a - 1];
                let b = concave[*b - 1];
                let c = concave[*c - 1];
                ((b.x - a.x) * (c.y - a.y) - (b.y - a.y) * (c.x - a.x)).abs() / 2.0
            })
            .sum::<f64>();
        assert!((area - 3.0).abs() < 1e-9);
    }

    #[test]
    fn applies_one_based_pn_index_for_polygon_coordinates() {
        assert_eq!(
            map_polygon_indices(&[1, 2, 3], Some(&[3, 4, 5]), 5).unwrap(),
            vec![3, 4, 5]
        );
        assert!(map_polygon_indices(&[0, 2, 3], None, 5).is_err());
    }

    #[test]
    fn resolves_parent_translation_and_rotated_axis_placements() {
        let text = "ISO-10303-21;HEADER;FILE_SCHEMA(('IFC4'));ENDSEC;DATA;#1=IFCCARTESIANPOINT((10.,0.,0.));#2=IFCDIRECTION((0.,0.,1.));#3=IFCDIRECTION((0.,1.,0.));#4=IFCAXIS2PLACEMENT3D(#1,#2,#3);#5=IFCLOCALPLACEMENT($,#4);#6=IFCCARTESIANPOINT((2.,0.,0.));#7=IFCAXIS2PLACEMENT3D(#6,$,$);#8=IFCLOCALPLACEMENT(#5,#7);ENDSEC;END-ISO-10303-21;";
        let (model, _) = parse_model(text).unwrap();
        let matrix = resolve_placement(8, &model, &mut HashSet::new(), 0).unwrap();
        let result = matrix.transform(Vec3 {
            x: 0.0,
            y: 0.0,
            z: 0.0,
        });
        assert!((result.x - 10.0).abs() < 1e-9);
        assert!((result.y - 2.0).abs() < 1e-9);
    }

    #[test]
    fn detects_ifc_signature_without_misreading_step_cad_header() {
        assert!(looks_like_ifc_prefix(
            b"ISO-10303-21;\nHEADER; FILE_SCHEMA(('IFC4'));"
        ));
        assert!(!looks_like_ifc_prefix(
            b"ISO-10303-21;\nHEADER; FILE_DESCRIPTION(('ordinary file'));"
        ));
        assert!(!looks_like_ifc_prefix(
            b"ISO-10303-21;\nHEADER; FILE_SCHEMA(('AP242'));"
        ));
        assert!(!looks_like_ifc_prefix(
            b"/* ISO-10303-21; FILE_SCHEMA(('IFC4')) */ HEADER; DATA;"
        ));
    }
}
