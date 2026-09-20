//! Bounded glTF 2.0 / GLB triangle-mesh preview.
//!
//! The importer resolves the default scene, applies node transforms, reads
//! bounded local/data-URI/GLB buffers and renders triangle/line primitives via
//! the shaded OBJ mesh path. Materials, textures, skins, animations and sparse
//! accessors are not evaluated.

use std::collections::HashSet;
use std::fmt::Write as _;
use std::io::Cursor;
use std::path::Path;

use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use serde_json::Value;

use crate::cad::obj;
use crate::convert::{ConvertOptions, PageConsumer, read_limited_file};
use crate::error::{Error, Result};
use crate::ir::{Node, Page};

const MAX_GLTF_BUFFERS: usize = 64;
const MAX_GLTF_BUFFER_VIEWS: usize = 100_000;
const MAX_GLTF_ACCESSORS: usize = 100_000;
const MAX_GLTF_MESHES: usize = 100_000;
const MAX_GLTF_PRIMITIVES: usize = 200_000;
const MAX_GLTF_NODES: usize = 100_000;
const MAX_GLTF_SCENES: usize = 10_000;
const MAX_GLTF_BUFFER_BYTES: u64 = 128 * 1024 * 1024;
const MAX_GLTF_TOTAL_BUFFER_BYTES: u64 = 256 * 1024 * 1024;
const MAX_GLTF_VERTICES: usize = 1_000_000;
const MAX_GLTF_TRIANGLES: usize = 200_000;
const MAX_GLTF_LINES: usize = 500_000;
const MAX_GLTF_OBJ_BYTES: u64 = 256 * 1024 * 1024;
const MAX_GLTF_NODE_DEPTH: usize = 128;
const GLTF_PREVIEW_UNITS_PER_METER: f64 = 1_000.0;
const MAX_GLTF_PREVIEW_COORDINATE: f64 = 1.0e12;

#[derive(Clone, Copy, Debug, Default)]
struct Vec3 {
    x: f64,
    y: f64,
    z: f64,
}

struct PrimitiveMesh {
    positions: Vec<Vec3>,
    triangles: Vec<[usize; 3]>,
    lines: Vec<[usize; 2]>,
}

struct GltfPageSink<'a> {
    inner: &'a mut dyn PageConsumer,
}

impl PageConsumer for GltfPageSink<'_> {
    fn consume(&mut self, mut page: Page) -> Result<()> {
        page.source_format = "gltf".into();
        page.title = "glTF 2.0 3D Model".into();
        page.description = "glTF mesh geometry rendered with an isometric shaded preview; meters are scaled to millimeter preview units, while materials and animation are not evaluated".into();
        for node in &mut page.nodes {
            relabel_obj_semantics(node);
        }
        self.inner.consume(page)
    }
}

fn relabel_obj_semantics(node: &mut Node) {
    match node {
        Node::Path { meta, .. } | Node::Text { meta, .. } | Node::Image { meta, .. } => {
            if meta.semantic_role.starts_with("obj:") {
                meta.semantic_role.replace_range(..4, "gltf:");
            }
        }
        Node::Group { nodes, meta, .. } => {
            if meta.semantic_role.starts_with("obj:") {
                meta.semantic_role.replace_range(..4, "gltf:");
            }
            for child in nodes {
                relabel_obj_semantics(child);
            }
        }
    }
}

pub(crate) fn convert(
    path: &Path,
    options: &ConvertOptions,
    sink: &mut dyn PageConsumer,
) -> Result<Vec<String>> {
    let source = read_limited_file(path, options.max_input_bytes, "glTF input")?;
    let is_glb = source.starts_with(b"glTF");
    let (json_bytes, glb_binary, mut warnings) = if is_glb {
        let parts = parse_glb(&source)?;
        (parts.json, parts.binary, parts.warnings)
    } else {
        (source.as_slice(), None, Vec::new())
    };
    let root: Value = serde_json::from_slice(json_bytes)
        .map_err(|error| Error::InvalidInput(format!("glTF JSON is invalid: {error}")))?;
    if root
        .get("asset")
        .and_then(|asset| asset.get("version"))
        .and_then(Value::as_str)
        != Some("2.0")
    {
        return Err(Error::Unsupported(
            "only glTF 2.0 assets are supported".into(),
        ));
    }
    if let Some(required) = root.get("extensionsRequired").and_then(Value::as_array)
        && !required.is_empty()
    {
        let extensions = required
            .iter()
            .filter_map(Value::as_str)
            .take(8)
            .collect::<Vec<_>>()
            .join(", ");
        return Err(Error::Unsupported(format!(
            "glTF requires unsupported extensions: {extensions}"
        )));
    }
    if root
        .get("extensionsUsed")
        .and_then(Value::as_array)
        .is_some_and(|extensions| !extensions.is_empty())
    {
        push_warning_once(
            &mut warnings,
            "glTF optional extensions are ignored unless they are required, in which case the asset is rejected",
        );
    }
    if root
        .get("materials")
        .and_then(Value::as_array)
        .is_some_and(|materials| !materials.is_empty())
        || root
            .get("textures")
            .and_then(Value::as_array)
            .is_some_and(|textures| !textures.is_empty())
    {
        push_warning_once(
            &mut warnings,
            "glTF materials, textures, vertex colors and alpha modes are not applied; geometry uses neutral mesh shading",
        );
    }
    if root
        .get("animations")
        .and_then(Value::as_array)
        .is_some_and(|animations| !animations.is_empty())
        || root
            .get("skins")
            .and_then(Value::as_array)
            .is_some_and(|skins| !skins.is_empty())
    {
        push_warning_once(&mut warnings, "glTF skins and animations are not evaluated");
    }

    let base_dir = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let base_dir = std::fs::canonicalize(base_dir)?;
    let buffers = load_buffers(&root, glb_binary, &base_dir, options)?;
    let buffer_views = parse_buffer_views(&root)?;
    let accessors = parse_accessors(&root)?;
    let meshes = parse_meshes(&root, &buffers, &buffer_views, &accessors, &mut warnings)?;
    let mut obj_source = String::new();
    let mut instance_counts = InstanceCounts::default();
    render_scene(
        &root,
        &meshes,
        &mut obj_source,
        &mut instance_counts,
        &mut warnings,
    )?;
    if instance_counts.vertices == 0
        || (instance_counts.triangles == 0 && instance_counts.lines == 0)
    {
        return Err(Error::Unsupported(
            "glTF asset contains no supported triangle or line geometry in its active scene".into(),
        ));
    }
    let mut render_options = options.clone();
    render_options.max_input_bytes = MAX_GLTF_OBJ_BYTES;
    let mut page_sink = GltfPageSink { inner: sink };
    warnings.extend(obj::convert(
        Cursor::new(obj_source.into_bytes()),
        &render_options,
        &mut page_sink,
    )?);
    Ok(warnings)
}

struct GlbParts<'a> {
    json: &'a [u8],
    binary: Option<&'a [u8]>,
    warnings: Vec<String>,
}

fn parse_glb(bytes: &[u8]) -> Result<GlbParts<'_>> {
    if bytes.len() < 12 || &bytes[..4] != b"glTF" {
        return Err(Error::InvalidInput("invalid GLB header".into()));
    }
    let version = read_u32_le(bytes, 4)?;
    if version != 2 {
        return Err(Error::Unsupported(format!(
            "GLB version {version} is unsupported; version 2 is required"
        )));
    }
    let declared_length = usize::try_from(read_u32_le(bytes, 8)?)
        .map_err(|_| Error::LimitExceeded("GLB length does not fit in memory".into()))?;
    if declared_length != bytes.len() {
        return Err(Error::InvalidInput(format!(
            "GLB declares {declared_length} bytes but contains {}",
            bytes.len()
        )));
    }
    let mut json = None;
    let mut binary = None;
    let mut warnings = Vec::new();
    let mut offset = 12usize;
    let mut chunks = 0usize;
    while offset < bytes.len() {
        chunks += 1;
        if chunks > 16 {
            return Err(Error::LimitExceeded("GLB contains too many chunks".into()));
        }
        let header_end = offset
            .checked_add(8)
            .ok_or_else(|| Error::InvalidInput("GLB chunk offset overflowed".into()))?;
        if header_end > bytes.len() {
            return Err(Error::InvalidInput("truncated GLB chunk header".into()));
        }
        let chunk_length = usize::try_from(read_u32_le(bytes, offset)?)
            .map_err(|_| Error::LimitExceeded("GLB chunk does not fit in memory".into()))?;
        let chunk_type = read_u32_le(bytes, offset + 4)?;
        let chunk_end = header_end
            .checked_add(chunk_length)
            .ok_or_else(|| Error::InvalidInput("GLB chunk length overflowed".into()))?;
        if chunk_end > bytes.len() || chunk_length % 4 != 0 {
            return Err(Error::InvalidInput(
                "invalid GLB chunk bounds or alignment".into(),
            ));
        }
        let chunk = &bytes[header_end..chunk_end];
        match chunk_type {
            0x4E4F534A => {
                if json.is_some() || chunks != 1 {
                    return Err(Error::InvalidInput(
                        "GLB JSON chunk must appear exactly once and first".into(),
                    ));
                }
                json = Some(chunk);
            }
            0x004E4942 => {
                if binary.replace(chunk).is_some() {
                    return Err(Error::InvalidInput(
                        "GLB contains duplicate BIN chunks".into(),
                    ));
                }
            }
            _ => push_warning_once(&mut warnings, "unknown GLB chunks were ignored"),
        }
        offset = chunk_end;
    }
    let json = json.ok_or_else(|| Error::InvalidInput("GLB has no JSON chunk".into()))?;
    let json = trim_json_padding(json);
    Ok(GlbParts {
        json,
        binary,
        warnings,
    })
}

fn trim_json_padding(mut bytes: &[u8]) -> &[u8] {
    while bytes.last().is_some_and(|byte| byte.is_ascii_whitespace()) {
        bytes = &bytes[..bytes.len() - 1];
    }
    bytes
}

fn read_u32_le(bytes: &[u8], offset: usize) -> Result<u32> {
    let end = offset
        .checked_add(4)
        .ok_or_else(|| Error::InvalidInput("binary glTF offset overflowed".into()))?;
    let value = bytes
        .get(offset..end)
        .ok_or_else(|| Error::InvalidInput("truncated binary glTF integer".into()))?;
    Ok(u32::from_le_bytes(
        value.try_into().expect("four-byte range"),
    ))
}

fn load_buffers(
    root: &Value,
    glb_binary: Option<&[u8]>,
    base_dir: &Path,
    options: &ConvertOptions,
) -> Result<Vec<Vec<u8>>> {
    let entries = root
        .get("buffers")
        .and_then(Value::as_array)
        .ok_or_else(|| Error::InvalidInput("glTF has no buffer array".into()))?;
    if entries.len() > MAX_GLTF_BUFFERS {
        return Err(Error::LimitExceeded(format!(
            "glTF has more than {MAX_GLTF_BUFFERS} buffers"
        )));
    }
    let per_buffer_limit = options.max_zip_entry_bytes.min(MAX_GLTF_BUFFER_BYTES);
    let total_limit = options.max_input_bytes.min(MAX_GLTF_TOTAL_BUFFER_BYTES);
    let mut total_bytes = 0u64;
    let mut buffers = Vec::with_capacity(entries.len());
    for (index, entry) in entries.iter().enumerate() {
        let expected = usize_value(entry.get("byteLength"), "buffer.byteLength")?;
        if expected as u64 > per_buffer_limit {
            return Err(Error::LimitExceeded(format!(
                "glTF buffer {index} exceeds the per-buffer limit of {per_buffer_limit} bytes"
            )));
        }
        total_bytes = total_bytes.saturating_add(expected as u64);
        if total_bytes > total_limit {
            return Err(Error::LimitExceeded(format!(
                "glTF buffers exceed the total {total_limit}-byte limit"
            )));
        }
        let bytes = if let Some(uri) = entry.get("uri").and_then(Value::as_str) {
            if uri.starts_with("data:") {
                decode_buffer_data_uri(uri, per_buffer_limit)?
            } else {
                let resource = crate::local_resource::resolve_relative_file(base_dir, uri)
                    .ok_or_else(|| {
                        Error::InvalidInput(format!(
                            "glTF buffer URI '{uri}' is external or escapes the asset directory"
                        ))
                    })?;
                read_limited_file(&resource, per_buffer_limit, "glTF buffer")?
            }
        } else if index == 0 {
            glb_binary
                .ok_or_else(|| {
                    Error::InvalidInput(
                        "glTF buffer without a URI requires the GLB BIN chunk".into(),
                    )
                })?
                .to_vec()
        } else {
            return Err(Error::InvalidInput(format!(
                "glTF buffer {index} has no URI and cannot use the single GLB BIN chunk"
            )));
        };
        if bytes.len() < expected {
            return Err(Error::InvalidInput(format!(
                "glTF buffer {index} declares {expected} bytes but contains {}",
                bytes.len()
            )));
        }
        buffers.push(bytes[..expected].to_vec());
    }
    Ok(buffers)
}

fn decode_buffer_data_uri(uri: &str, limit: u64) -> Result<Vec<u8>> {
    let (metadata, payload) = uri
        .split_once(',')
        .ok_or_else(|| Error::InvalidInput("invalid glTF data URI".into()))?;
    if !metadata.ends_with(";base64")
        || !(metadata.starts_with("data:application/octet-stream")
            || metadata.starts_with("data:application/gltf-buffer"))
    {
        return Err(Error::Unsupported(
            "glTF buffer data URIs must use base64 application/octet-stream or application/gltf-buffer".into(),
        ));
    }
    let encoded_limit = usize::try_from(limit.div_ceil(3).saturating_mul(4)).unwrap_or(usize::MAX);
    if payload.len() > encoded_limit.saturating_add(4) {
        return Err(Error::LimitExceeded(
            "glTF buffer data URI exceeds the decoded buffer limit".into(),
        ));
    }
    let decoded = BASE64_STANDARD
        .decode(payload)
        .map_err(|error| Error::InvalidInput(format!("invalid glTF base64 buffer: {error}")))?;
    if decoded.len() as u64 > limit {
        return Err(Error::LimitExceeded(format!(
            "decoded glTF buffer exceeds {limit} bytes"
        )));
    }
    Ok(decoded)
}

#[derive(Clone, Copy)]
struct BufferView {
    buffer: usize,
    byte_offset: usize,
    byte_length: usize,
    byte_stride: Option<usize>,
}

#[derive(Clone, Copy)]
struct Accessor {
    buffer_view: Option<usize>,
    byte_offset: usize,
    count: usize,
    component_type: u64,
    component_count: usize,
    normalized: bool,
    sparse: bool,
}

fn parse_buffer_views(root: &Value) -> Result<Vec<BufferView>> {
    let Some(entries) = root.get("bufferViews").and_then(Value::as_array) else {
        return Ok(Vec::new());
    };
    if entries.len() > MAX_GLTF_BUFFER_VIEWS {
        return Err(Error::LimitExceeded(format!(
            "glTF has more than {MAX_GLTF_BUFFER_VIEWS} buffer views"
        )));
    }
    entries
        .iter()
        .map(|entry| {
            let stride = entry
                .get("byteStride")
                .map(|value| usize_value(Some(value), "bufferView.byteStride"))
                .transpose()?;
            if stride.is_some_and(|value| !(4..=252).contains(&value) || value % 4 != 0) {
                return Err(Error::InvalidInput(
                    "glTF bufferView.byteStride must be a multiple of four from 4 to 252".into(),
                ));
            }
            Ok(BufferView {
                buffer: usize_value(entry.get("buffer"), "bufferView.buffer")?,
                byte_offset: entry
                    .get("byteOffset")
                    .map(|value| usize_value(Some(value), "bufferView.byteOffset"))
                    .transpose()?
                    .unwrap_or(0),
                byte_length: usize_value(entry.get("byteLength"), "bufferView.byteLength")?,
                byte_stride: stride,
            })
        })
        .collect()
}

fn parse_accessors(root: &Value) -> Result<Vec<Accessor>> {
    let Some(entries) = root.get("accessors").and_then(Value::as_array) else {
        return Ok(Vec::new());
    };
    if entries.len() > MAX_GLTF_ACCESSORS {
        return Err(Error::LimitExceeded(format!(
            "glTF has more than {MAX_GLTF_ACCESSORS} accessors"
        )));
    }
    entries
        .iter()
        .map(|entry| {
            let ty = entry
                .get("type")
                .and_then(Value::as_str)
                .ok_or_else(|| Error::InvalidInput("glTF accessor.type is missing".into()))?;
            let component_count = match ty {
                "SCALAR" => 1,
                "VEC2" => 2,
                "VEC3" => 3,
                "VEC4" => 4,
                "MAT2" => 4,
                "MAT3" => 9,
                "MAT4" => 16,
                _ => {
                    return Err(Error::Unsupported(format!(
                        "unsupported glTF accessor type '{ty}'"
                    )));
                }
            };
            Ok(Accessor {
                buffer_view: entry
                    .get("bufferView")
                    .map(|value| usize_value(Some(value), "accessor.bufferView"))
                    .transpose()?,
                byte_offset: entry
                    .get("byteOffset")
                    .map(|value| usize_value(Some(value), "accessor.byteOffset"))
                    .transpose()?
                    .unwrap_or(0),
                count: usize_value(entry.get("count"), "accessor.count")?,
                component_type: entry
                    .get("componentType")
                    .and_then(Value::as_u64)
                    .ok_or_else(|| {
                        Error::InvalidInput("glTF accessor.componentType is missing".into())
                    })?,
                component_count,
                normalized: entry
                    .get("normalized")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
                sparse: entry.get("sparse").is_some(),
            })
        })
        .collect()
}

fn usize_value(value: Option<&Value>, name: &str) -> Result<usize> {
    let value = value
        .and_then(Value::as_u64)
        .ok_or_else(|| Error::InvalidInput(format!("glTF {name} must be a nonnegative integer")))?;
    usize::try_from(value)
        .map_err(|_| Error::LimitExceeded(format!("glTF {name} does not fit in memory")))
}

fn read_float_vec3_accessor(
    accessor_index: usize,
    buffers: &[Vec<u8>],
    views: &[BufferView],
    accessors: &[Accessor],
) -> Result<Vec<Vec3>> {
    let accessor = *accessors.get(accessor_index).ok_or_else(|| {
        Error::InvalidInput(format!(
            "glTF POSITION accessor {accessor_index} is missing"
        ))
    })?;
    if accessor.component_type != 5126 || accessor.component_count != 3 || accessor.normalized {
        return Err(Error::Unsupported(
            "glTF POSITION accessors must be non-normalized FLOAT VEC3 values".into(),
        ));
    }
    if accessor.sparse {
        return Err(Error::Unsupported(
            "sparse glTF POSITION accessors are unsupported".into(),
        ));
    }
    if accessor.count > MAX_GLTF_VERTICES {
        return Err(Error::LimitExceeded(format!(
            "glTF POSITION accessor exceeds {MAX_GLTF_VERTICES} vertices"
        )));
    }
    let data = accessor_data(accessor, 12, buffers, views, "POSITION")?;
    let mut positions = Vec::with_capacity(accessor.count);
    for index in 0..accessor.count {
        let offset = index * data.stride;
        let x = read_f32(data.bytes, offset)? as f64;
        let y = read_f32(data.bytes, offset + 4)? as f64;
        let z = read_f32(data.bytes, offset + 8)? as f64;
        if !x.is_finite()
            || !y.is_finite()
            || !z.is_finite()
            || [x, y, z].iter().any(|v| v.abs() > 1.0e12)
        {
            return Err(Error::InvalidInput(format!(
                "glTF POSITION accessor contains an invalid coordinate at element {index}"
            )));
        }
        positions.push(Vec3 { x, y, z });
    }
    Ok(positions)
}

struct AccessorBytes<'a> {
    bytes: &'a [u8],
    stride: usize,
}

fn accessor_data<'a>(
    accessor: Accessor,
    element_bytes: usize,
    buffers: &'a [Vec<u8>],
    views: &[BufferView],
    label: &str,
) -> Result<AccessorBytes<'a>> {
    let view_index = accessor.buffer_view.ok_or_else(|| {
        Error::Unsupported(format!(
            "glTF {label} accessor without a bufferView is unsupported"
        ))
    })?;
    let view = *views
        .get(view_index)
        .ok_or_else(|| Error::InvalidInput(format!("glTF {label} bufferView index is invalid")))?;
    let buffer = buffers
        .get(view.buffer)
        .ok_or_else(|| Error::InvalidInput(format!("glTF {label} buffer index is invalid")))?;
    let stride = view.byte_stride.unwrap_or(element_bytes);
    if stride < element_bytes {
        return Err(Error::InvalidInput(format!(
            "glTF {label} byteStride is smaller than the element"
        )));
    }
    let last_element_offset = accessor
        .count
        .saturating_sub(1)
        .checked_mul(stride)
        .and_then(|value| value.checked_add(accessor.byte_offset))
        .ok_or_else(|| Error::LimitExceeded(format!("glTF {label} accessor offset overflowed")))?;
    let end_in_view = last_element_offset
        .checked_add(if accessor.count == 0 {
            0
        } else {
            element_bytes
        })
        .ok_or_else(|| Error::LimitExceeded(format!("glTF {label} accessor extent overflowed")))?;
    let view_end = view
        .byte_offset
        .checked_add(view.byte_length)
        .ok_or_else(|| Error::LimitExceeded("glTF bufferView extent overflowed".into()))?;
    let absolute_start = view
        .byte_offset
        .checked_add(accessor.byte_offset)
        .ok_or_else(|| Error::LimitExceeded("glTF accessor offset overflowed".into()))?;
    let absolute_end = view
        .byte_offset
        .checked_add(end_in_view)
        .ok_or_else(|| Error::LimitExceeded("glTF accessor extent overflowed".into()))?;
    if end_in_view > view.byte_length || view_end > buffer.len() || absolute_end > buffer.len() {
        return Err(Error::InvalidInput(format!(
            "glTF {label} accessor exceeds its bufferView"
        )));
    }
    Ok(AccessorBytes {
        bytes: &buffer[absolute_start..absolute_end],
        stride,
    })
}

fn read_f32(bytes: &[u8], offset: usize) -> Result<f32> {
    let end = offset
        .checked_add(4)
        .ok_or_else(|| Error::InvalidInput("glTF float offset overflowed".into()))?;
    let value = bytes
        .get(offset..end)
        .ok_or_else(|| Error::InvalidInput("truncated glTF float".into()))?;
    Ok(f32::from_le_bytes(
        value.try_into().expect("four-byte range"),
    ))
}

fn read_indices_accessor(
    accessor_index: usize,
    buffers: &[Vec<u8>],
    views: &[BufferView],
    accessors: &[Accessor],
) -> Result<Vec<usize>> {
    let accessor = *accessors.get(accessor_index).ok_or_else(|| {
        Error::InvalidInput(format!("glTF indices accessor {accessor_index} is missing"))
    })?;
    if accessor.component_count != 1 || accessor.normalized || accessor.sparse {
        return Err(Error::Unsupported(
            "glTF indices must use non-normalized, non-sparse SCALAR accessors".into(),
        ));
    }
    let component_bytes = match accessor.component_type {
        5121 => 1,
        5123 => 2,
        5125 => 4,
        _ => {
            return Err(Error::Unsupported(
                "glTF indices must use UNSIGNED_BYTE, UNSIGNED_SHORT, or UNSIGNED_INT".into(),
            ));
        }
    };
    if accessor.count
        > MAX_GLTF_TRIANGLES
            .saturating_mul(3)
            .max(MAX_GLTF_LINES.saturating_mul(2))
    {
        return Err(Error::LimitExceeded(
            "glTF primitive index count exceeds its limit".into(),
        ));
    }
    let data = accessor_data(accessor, component_bytes, buffers, views, "indices")?;
    if data.stride != component_bytes {
        return Err(Error::Unsupported(
            "strided glTF index accessors are unsupported".into(),
        ));
    }
    let mut indices = Vec::with_capacity(accessor.count);
    for index in 0..accessor.count {
        let offset = index * component_bytes;
        let value = match component_bytes {
            1 => usize::from(data.bytes[offset]),
            2 => usize::from(u16::from_le_bytes(
                data.bytes[offset..offset + 2]
                    .try_into()
                    .expect("two-byte range"),
            )),
            _ => usize::try_from(u32::from_le_bytes(
                data.bytes[offset..offset + 4]
                    .try_into()
                    .expect("four-byte range"),
            ))
            .map_err(|_| Error::LimitExceeded("glTF index does not fit in memory".into()))?,
        };
        indices.push(value);
    }
    Ok(indices)
}

fn parse_meshes(
    root: &Value,
    buffers: &[Vec<u8>],
    views: &[BufferView],
    accessors: &[Accessor],
    warnings: &mut Vec<String>,
) -> Result<Vec<Vec<PrimitiveMesh>>> {
    let Some(meshes) = root.get("meshes").and_then(Value::as_array) else {
        return Err(Error::Unsupported("glTF asset has no mesh array".into()));
    };
    if meshes.len() > MAX_GLTF_MESHES {
        return Err(Error::LimitExceeded(format!(
            "glTF has more than {MAX_GLTF_MESHES} meshes"
        )));
    }
    let mut total_vertices = 0usize;
    let mut total_triangles = 0usize;
    let mut total_lines = 0usize;
    let mut total_primitives = 0usize;
    let mut parsed_meshes = Vec::with_capacity(meshes.len());
    for (mesh_index, mesh) in meshes.iter().enumerate() {
        let primitives = mesh
            .get("primitives")
            .and_then(Value::as_array)
            .ok_or_else(|| {
                Error::InvalidInput(format!("glTF mesh {mesh_index} has no primitives array"))
            })?;
        total_primitives = total_primitives
            .checked_add(primitives.len())
            .ok_or_else(|| Error::LimitExceeded("glTF primitive count overflowed".into()))?;
        if total_primitives > MAX_GLTF_PRIMITIVES {
            return Err(Error::LimitExceeded(format!(
                "glTF has more than {MAX_GLTF_PRIMITIVES} primitives"
            )));
        }
        let mut parsed_primitives = Vec::new();
        for (primitive_index, primitive) in primitives.iter().enumerate() {
            if primitive.get("targets").is_some() {
                push_warning_once(warnings, "glTF morph targets are not applied");
            }
            if primitive
                .get("attributes")
                .and_then(Value::as_object)
                .is_some_and(|attributes| attributes.len() > 1)
            {
                push_warning_once(
                    warnings,
                    "glTF normals, texture coordinates, vertex colors and other vertex attributes are not applied",
                );
            }
            let Some(position_index) = primitive
                .get("attributes")
                .and_then(|attributes| attributes.get("POSITION"))
                .and_then(Value::as_u64)
                .and_then(|value| usize::try_from(value).ok())
            else {
                push_warning_once(warnings, "glTF primitives without POSITION are skipped");
                continue;
            };
            let positions =
                match read_float_vec3_accessor(position_index, buffers, views, accessors) {
                    Ok(positions) => positions,
                    Err(Error::Unsupported(message)) => {
                        push_warning_once(warnings, &format!("glTF primitive skipped: {message}"));
                        continue;
                    }
                    Err(error) => {
                        return Err(Error::InvalidInput(format!(
                            "glTF mesh {mesh_index} primitive {primitive_index}: {error}"
                        )));
                    }
                };
            let indices = if let Some(index) = primitive.get("indices") {
                let index = usize_value(Some(index), "primitive.indices")?;
                match read_indices_accessor(index, buffers, views, accessors) {
                    Ok(indices) => indices,
                    Err(Error::Unsupported(message)) => {
                        push_warning_once(warnings, &format!("glTF primitive skipped: {message}"));
                        continue;
                    }
                    Err(error) => {
                        return Err(Error::InvalidInput(format!(
                            "glTF mesh {mesh_index} primitive {primitive_index}: {error}"
                        )));
                    }
                }
            } else {
                (0..positions.len()).collect()
            };
            if indices.iter().any(|index| *index >= positions.len()) {
                return Err(Error::InvalidInput(format!(
                    "glTF mesh {mesh_index} primitive {primitive_index} has an out-of-range index"
                )));
            }
            let mode = primitive
                .get("mode")
                .map(|value| {
                    value.as_u64().ok_or_else(|| {
                        Error::InvalidInput(
                            "glTF primitive.mode must be a nonnegative integer".into(),
                        )
                    })
                })
                .transpose()?
                .unwrap_or(4);
            let (triangles, lines) = match mode {
                4 => {
                    if indices.len() % 3 != 0 {
                        return Err(Error::InvalidInput(
                            "glTF TRIANGLES index count is not divisible by three".into(),
                        ));
                    }
                    (
                        indices
                            .chunks_exact(3)
                            .map(|triple| [triple[0], triple[1], triple[2]])
                            .collect(),
                        Vec::new(),
                    )
                }
                5 => (triangle_strip(&indices), Vec::new()),
                6 => (triangle_fan(&indices), Vec::new()),
                1 => {
                    if indices.len() % 2 != 0 {
                        return Err(Error::InvalidInput(
                            "glTF LINES index count is not divisible by two".into(),
                        ));
                    }
                    (
                        Vec::new(),
                        indices
                            .chunks_exact(2)
                            .map(|pair| [pair[0], pair[1]])
                            .collect(),
                    )
                }
                2 => (Vec::new(), line_loop(&indices)),
                3 => (
                    Vec::new(),
                    indices.windows(2).map(|pair| [pair[0], pair[1]]).collect(),
                ),
                0 => {
                    push_warning_once(warnings, "glTF POINTS primitives are not rendered");
                    continue;
                }
                _ => {
                    push_warning_once(warnings, "glTF primitive with an unknown mode was skipped");
                    continue;
                }
            };
            total_vertices = total_vertices
                .checked_add(positions.len())
                .ok_or_else(|| Error::LimitExceeded("glTF vertex count overflowed".into()))?;
            total_triangles = total_triangles
                .checked_add(triangles.len())
                .ok_or_else(|| Error::LimitExceeded("glTF triangle count overflowed".into()))?;
            total_lines = total_lines
                .checked_add(lines.len())
                .ok_or_else(|| Error::LimitExceeded("glTF line count overflowed".into()))?;
            if total_vertices > MAX_GLTF_VERTICES
                || total_triangles > MAX_GLTF_TRIANGLES
                || total_lines > MAX_GLTF_LINES
            {
                return Err(Error::LimitExceeded(
                    "glTF mesh geometry exceeds configured limits".into(),
                ));
            }
            parsed_primitives.push(PrimitiveMesh {
                positions,
                triangles,
                lines,
            });
        }
        parsed_meshes.push(parsed_primitives);
    }
    Ok(parsed_meshes)
}

fn triangle_strip(indices: &[usize]) -> Vec<[usize; 3]> {
    (2..indices.len())
        .filter_map(|index| {
            let triangle = if index % 2 == 0 {
                [indices[index - 2], indices[index - 1], indices[index]]
            } else {
                [indices[index - 1], indices[index - 2], indices[index]]
            };
            (triangle[0] != triangle[1] && triangle[1] != triangle[2] && triangle[0] != triangle[2])
                .then_some(triangle)
        })
        .collect()
}

fn triangle_fan(indices: &[usize]) -> Vec<[usize; 3]> {
    (2..indices.len())
        .filter_map(|index| {
            let triangle = [indices[0], indices[index - 1], indices[index]];
            (triangle[0] != triangle[1] && triangle[1] != triangle[2] && triangle[0] != triangle[2])
                .then_some(triangle)
        })
        .collect()
}

fn line_loop(indices: &[usize]) -> Vec<[usize; 2]> {
    if indices.len() < 2 {
        return Vec::new();
    }
    let mut lines = indices
        .windows(2)
        .map(|pair| [pair[0], pair[1]])
        .collect::<Vec<_>>();
    lines.push([*indices.last().unwrap_or(&indices[0]), indices[0]]);
    lines
}

#[derive(Default)]
struct InstanceCounts {
    nodes: usize,
    vertices: usize,
    triangles: usize,
    lines: usize,
}

fn render_scene(
    root: &Value,
    meshes: &[Vec<PrimitiveMesh>],
    output: &mut String,
    counts: &mut InstanceCounts,
    warnings: &mut Vec<String>,
) -> Result<()> {
    let nodes = root.get("nodes").and_then(Value::as_array);
    let Some(nodes) = nodes else {
        push_warning_once(
            warnings,
            "glTF has no node scene graph; meshes are instantiated once at identity",
        );
        for mesh in meshes {
            for primitive in mesh {
                append_primitive(primitive, identity_matrix(), output, counts)?;
            }
        }
        return Ok(());
    };
    if nodes.len() > MAX_GLTF_NODES {
        return Err(Error::LimitExceeded(format!(
            "glTF has more than {MAX_GLTF_NODES} nodes"
        )));
    }
    let roots = if let Some(scenes) = root.get("scenes").and_then(Value::as_array) {
        if scenes.len() > MAX_GLTF_SCENES {
            return Err(Error::LimitExceeded(format!(
                "glTF has more than {MAX_GLTF_SCENES} scenes"
            )));
        }
        let scene_index = root.get("scene").and_then(Value::as_u64).unwrap_or(0);
        let scene_index = usize::try_from(scene_index)
            .map_err(|_| Error::InvalidInput("glTF default scene index is invalid".into()))?;
        let scene = scenes.get(scene_index).ok_or_else(|| {
            Error::InvalidInput("glTF default scene index is outside the scenes array".into())
        })?;
        let roots = scene
            .get("nodes")
            .and_then(Value::as_array)
            .map(|nodes| {
                nodes
                    .iter()
                    .map(|node| usize_value(Some(node), "scene.nodes[]"))
                    .collect::<Result<Vec<_>>>()
            })
            .transpose()?
            .unwrap_or_default();
        if root.get("scene").is_none() {
            push_warning_once(
                warnings,
                "glTF has no explicit default scene; the first scene was selected",
            );
        }
        if scenes.len() > 1 {
            push_warning_once(
                warnings,
                "only the default glTF scene is rendered; other scenes are omitted",
            );
        }
        roots
    } else {
        let mut child_nodes = HashSet::new();
        for node in nodes {
            if let Some(children) = node.get("children").and_then(Value::as_array) {
                for child in children {
                    child_nodes.insert(usize_value(Some(child), "node.children[]")?);
                }
            }
        }
        let roots = (0..nodes.len())
            .filter(|index| !child_nodes.contains(index))
            .collect();
        push_warning_once(
            warnings,
            "glTF has no scenes array; root nodes are used as the preview scene",
        );
        roots
    };
    if roots.is_empty() {
        push_warning_once(
            warnings,
            "glTF scene has no roots; mesh definitions are used at identity",
        );
        for mesh in meshes {
            for primitive in mesh {
                append_primitive(primitive, identity_matrix(), output, counts)?;
            }
        }
        return Ok(());
    }
    let mut context = GltfSceneContext {
        nodes,
        meshes,
        output,
        counts,
        warnings,
        active: HashSet::new(),
    };
    for node in roots {
        visit_node(node, identity_matrix(), 0, &mut context)?;
    }
    Ok(())
}

struct GltfSceneContext<'a> {
    nodes: &'a [Value],
    meshes: &'a [Vec<PrimitiveMesh>],
    output: &'a mut String,
    counts: &'a mut InstanceCounts,
    warnings: &'a mut Vec<String>,
    active: HashSet<usize>,
}

fn visit_node(
    node_index: usize,
    parent: [f64; 16],
    depth: usize,
    context: &mut GltfSceneContext<'_>,
) -> Result<()> {
    if depth > MAX_GLTF_NODE_DEPTH {
        return Err(Error::LimitExceeded(format!(
            "glTF node hierarchy exceeds depth {MAX_GLTF_NODE_DEPTH}"
        )));
    }
    if !context.active.insert(node_index) {
        return Err(Error::InvalidInput(
            "glTF node graph contains a cycle".into(),
        ));
    }
    context.counts.nodes = context.counts.nodes.saturating_add(1);
    if context.counts.nodes > MAX_GLTF_NODES.saturating_mul(4) {
        return Err(Error::LimitExceeded(
            "glTF scene instancing exceeds the node visit limit".into(),
        ));
    }
    let (local, mesh_index, has_skin, children) = {
        let node = context.nodes.get(node_index).ok_or_else(|| {
            Error::InvalidInput(format!("glTF node index {node_index} is invalid"))
        })?;
        let children = node
            .get("children")
            .and_then(Value::as_array)
            .map(|children| {
                children
                    .iter()
                    .map(|child| usize_value(Some(child), "node.children[]"))
                    .collect::<Result<Vec<_>>>()
            })
            .transpose()?
            .unwrap_or_default();
        let mesh_index = node
            .get("mesh")
            .map(|mesh| usize_value(Some(mesh), "node.mesh"))
            .transpose()?;
        (
            node_transform(node)?,
            mesh_index,
            node.get("skin").is_some(),
            children,
        )
    };
    let world = multiply_matrix(parent, local);
    if let Some(mesh_index) = mesh_index {
        let mesh = context.meshes.get(mesh_index).ok_or_else(|| {
            Error::InvalidInput(format!("glTF node references missing mesh {mesh_index}"))
        })?;
        for primitive in mesh {
            append_primitive(primitive, world, context.output, context.counts)?;
        }
    }
    if has_skin {
        push_warning_once(context.warnings, "glTF skin deformation is not applied");
    }
    for child in children {
        visit_node(child, world, depth + 1, context)?;
    }
    context.active.remove(&node_index);
    Ok(())
}

fn node_transform(node: &Value) -> Result<[f64; 16]> {
    if let Some(matrix) = node.get("matrix") {
        if node.get("translation").is_some()
            || node.get("rotation").is_some()
            || node.get("scale").is_some()
        {
            return Err(Error::InvalidInput(
                "glTF node cannot define both matrix and TRS transforms".into(),
            ));
        }
        let values = matrix
            .as_array()
            .ok_or_else(|| Error::InvalidInput("glTF node.matrix must be an array".into()))?;
        if values.len() != 16 {
            return Err(Error::InvalidInput(
                "glTF node.matrix must contain 16 values".into(),
            ));
        }
        let mut result = [0.0; 16];
        for (index, value) in values.iter().enumerate() {
            result[index] = finite_number(value, "node.matrix")?;
        }
        return Ok(result);
    }
    let translation = vec3_property(node.get("translation"), [0.0, 0.0, 0.0], "node.translation")?;
    let scale = vec3_property(node.get("scale"), [1.0, 1.0, 1.0], "node.scale")?;
    let rotation = quat_property(node.get("rotation"))?;
    let [x, y, z, w] = rotation;
    let xx = x * x;
    let yy = y * y;
    let zz = z * z;
    let xy = x * y;
    let xz = x * z;
    let yz = y * z;
    let wx = w * x;
    let wy = w * y;
    let wz = w * z;
    let mut matrix = [
        1.0 - 2.0 * (yy + zz),
        2.0 * (xy + wz),
        2.0 * (xz - wy),
        0.0,
        2.0 * (xy - wz),
        1.0 - 2.0 * (xx + zz),
        2.0 * (yz + wx),
        0.0,
        2.0 * (xz + wy),
        2.0 * (yz - wx),
        1.0 - 2.0 * (xx + yy),
        0.0,
        translation[0],
        translation[1],
        translation[2],
        1.0,
    ];
    for row in 0..3 {
        matrix[row] *= scale[0];
        matrix[4 + row] *= scale[1];
        matrix[8 + row] *= scale[2];
    }
    Ok(matrix)
}

fn vec3_property(value: Option<&Value>, default: [f64; 3], label: &str) -> Result<[f64; 3]> {
    let Some(value) = value else {
        return Ok(default);
    };
    let values = value
        .as_array()
        .ok_or_else(|| Error::InvalidInput(format!("glTF {label} must be an array")))?;
    if values.len() != 3 {
        return Err(Error::InvalidInput(format!(
            "glTF {label} must contain three values"
        )));
    }
    Ok([
        finite_number(&values[0], label)?,
        finite_number(&values[1], label)?,
        finite_number(&values[2], label)?,
    ])
}

fn quat_property(value: Option<&Value>) -> Result<[f64; 4]> {
    let Some(value) = value else {
        return Ok([0.0, 0.0, 0.0, 1.0]);
    };
    let values = value
        .as_array()
        .ok_or_else(|| Error::InvalidInput("glTF node.rotation must be an array".into()))?;
    if values.len() != 4 {
        return Err(Error::InvalidInput(
            "glTF node.rotation must contain four values".into(),
        ));
    }
    let mut quat = [
        finite_number(&values[0], "node.rotation")?,
        finite_number(&values[1], "node.rotation")?,
        finite_number(&values[2], "node.rotation")?,
        finite_number(&values[3], "node.rotation")?,
    ];
    let norm = quat.iter().map(|value| value * value).sum::<f64>().sqrt();
    if norm < f64::EPSILON {
        return Err(Error::InvalidInput(
            "glTF node.rotation quaternion has zero length".into(),
        ));
    }
    for value in &mut quat {
        *value /= norm;
    }
    Ok(quat)
}

fn finite_number(value: &Value, label: &str) -> Result<f64> {
    let number = value
        .as_f64()
        .ok_or_else(|| Error::InvalidInput(format!("glTF {label} contains a non-number")))?;
    if !number.is_finite() || number.abs() > 1.0e12 {
        return Err(Error::InvalidInput(format!(
            "glTF {label} contains an invalid number"
        )));
    }
    Ok(number)
}

fn identity_matrix() -> [f64; 16] {
    [
        1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0,
    ]
}

fn multiply_matrix(left: [f64; 16], right: [f64; 16]) -> [f64; 16] {
    let mut result = [0.0; 16];
    for column in 0..4 {
        for row in 0..4 {
            for inner in 0..4 {
                result[column * 4 + row] += left[inner * 4 + row] * right[column * 4 + inner];
            }
        }
    }
    result
}

fn transform_position(matrix: [f64; 16], point: Vec3) -> Result<Vec3> {
    let x = matrix[0] * point.x + matrix[4] * point.y + matrix[8] * point.z + matrix[12];
    let y = matrix[1] * point.x + matrix[5] * point.y + matrix[9] * point.z + matrix[13];
    let z = matrix[2] * point.x + matrix[6] * point.y + matrix[10] * point.z + matrix[14];
    let w = matrix[3] * point.x + matrix[7] * point.y + matrix[11] * point.z + matrix[15];
    if !w.is_finite() || w.abs() < f64::EPSILON {
        return Err(Error::InvalidInput(
            "glTF node transform produced an invalid homogeneous coordinate".into(),
        ));
    }
    let result = Vec3 {
        x: x / w,
        y: y / w,
        z: z / w,
    };
    if [result.x, result.y, result.z]
        .iter()
        .any(|value| !value.is_finite() || value.abs() > 1.0e12)
    {
        return Err(Error::InvalidInput(
            "glTF node transform produced an invalid position".into(),
        ));
    }
    Ok(result)
}

fn append_primitive(
    primitive: &PrimitiveMesh,
    transform: [f64; 16],
    output: &mut String,
    counts: &mut InstanceCounts,
) -> Result<()> {
    let base = counts.vertices;
    for position in &primitive.positions {
        let mut transformed = transform_position(transform, *position)?;
        // glTF is Y-up; the OBJ renderer uses Z as its vertical axis.
        transformed = Vec3 {
            x: transformed.x * GLTF_PREVIEW_UNITS_PER_METER,
            y: -transformed.z * GLTF_PREVIEW_UNITS_PER_METER,
            z: transformed.y * GLTF_PREVIEW_UNITS_PER_METER,
        };
        if [transformed.x, transformed.y, transformed.z]
            .iter()
            .any(|value| !value.is_finite() || value.abs() > MAX_GLTF_PREVIEW_COORDINATE)
        {
            return Err(Error::LimitExceeded(
                "glTF preview coordinates exceed the supported range after meter scaling".into(),
            ));
        }
        append_obj(
            output,
            format_args!(
                "v {:.9} {:.9} {:.9}\n",
                transformed.x, transformed.y, transformed.z
            ),
        )?;
        counts.vertices += 1;
        if counts.vertices > MAX_GLTF_VERTICES {
            return Err(Error::LimitExceeded(format!(
                "glTF scene exceeds {MAX_GLTF_VERTICES} expanded vertices"
            )));
        }
    }
    for triangle in &primitive.triangles {
        if counts.triangles >= MAX_GLTF_TRIANGLES {
            return Err(Error::LimitExceeded(format!(
                "glTF scene exceeds {MAX_GLTF_TRIANGLES} triangles"
            )));
        }
        let a = base
            .checked_add(triangle[0])
            .and_then(|value| value.checked_add(1))
            .ok_or_else(|| Error::LimitExceeded("glTF OBJ face index overflowed".into()))?;
        let b = base
            .checked_add(triangle[1])
            .and_then(|value| value.checked_add(1))
            .ok_or_else(|| Error::LimitExceeded("glTF OBJ face index overflowed".into()))?;
        let c = base
            .checked_add(triangle[2])
            .and_then(|value| value.checked_add(1))
            .ok_or_else(|| Error::LimitExceeded("glTF OBJ face index overflowed".into()))?;
        append_obj(output, format_args!("f {a} {b} {c}\n"))?;
        counts.triangles += 1;
    }
    for line in &primitive.lines {
        if counts.lines >= MAX_GLTF_LINES {
            return Err(Error::LimitExceeded(format!(
                "glTF scene exceeds {MAX_GLTF_LINES} lines"
            )));
        }
        let a = base
            .checked_add(line[0])
            .and_then(|value| value.checked_add(1))
            .ok_or_else(|| Error::LimitExceeded("glTF OBJ line index overflowed".into()))?;
        let b = base
            .checked_add(line[1])
            .and_then(|value| value.checked_add(1))
            .ok_or_else(|| Error::LimitExceeded("glTF OBJ line index overflowed".into()))?;
        append_obj(output, format_args!("l {a} {b}\n"))?;
        counts.lines += 1;
    }
    Ok(())
}

fn append_obj(output: &mut String, args: std::fmt::Arguments<'_>) -> Result<()> {
    output
        .write_fmt(args)
        .map_err(|_| Error::InvalidInput("failed to format glTF preview geometry".into()))?;
    if output.len() as u64 > MAX_GLTF_OBJ_BYTES {
        return Err(Error::LimitExceeded(format!(
            "generated glTF mesh exceeds {MAX_GLTF_OBJ_BYTES} bytes"
        )));
    }
    Ok(())
}

fn push_warning_once(warnings: &mut Vec<String>, warning: &str) {
    if !warnings.iter().any(|existing| existing == warning) {
        warnings.push(warning.to_owned());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn composes_gltf_translation_rotation_and_scale_in_spec_order() {
        let quarter_turn = std::f64::consts::FRAC_1_SQRT_2;
        let node = serde_json::json!({
            "translation": [1.0, 2.0, 3.0],
            "rotation": [0.0, 0.0, quarter_turn, quarter_turn],
            "scale": [2.0, 2.0, 2.0]
        });
        let transformed = transform_position(
            node_transform(&node).unwrap(),
            Vec3 {
                x: 1.0,
                y: 0.0,
                z: 0.0,
            },
        )
        .unwrap();

        assert!((transformed.x - 1.0).abs() < 1e-9);
        assert!((transformed.y - 4.0).abs() < 1e-9);
        assert!((transformed.z - 3.0).abs() < 1e-9);
    }

    #[test]
    fn multiplies_parent_and_child_node_transforms() {
        let parent = node_transform(&serde_json::json!({"translation":[10.0,0.0,0.0]})).unwrap();
        let child = node_transform(&serde_json::json!({"translation":[2.0,0.0,0.0]})).unwrap();
        let transformed =
            transform_position(multiply_matrix(parent, child), Vec3::default()).unwrap();

        assert_eq!(transformed.x, 12.0);
        assert_eq!(transformed.y, 0.0);
        assert_eq!(transformed.z, 0.0);
    }

    #[test]
    fn triangle_strip_and_fan_expand_without_degenerate_faces() {
        assert_eq!(triangle_strip(&[0, 1, 2, 3]), vec![[0, 1, 2], [2, 1, 3]]);
        assert_eq!(triangle_fan(&[0, 1, 2, 3]), vec![[0, 1, 2], [0, 2, 3]]);
        assert_eq!(triangle_strip(&[0, 1, 1, 2, 3]).len(), 1);
    }

    #[test]
    fn glb_parser_validates_declared_length_and_json_order() {
        let bytes = b"glTF\x02\0\0\0\x10\0\0\0";
        assert!(matches!(parse_glb(bytes), Err(Error::InvalidInput(_))));
    }

    #[test]
    fn decodes_only_bounded_gltf_buffer_data_uris() {
        let payload = [1u8, 2, 3, 4];
        let uri = format!(
            "data:application/octet-stream;base64,{}",
            BASE64_STANDARD.encode(payload)
        );
        assert_eq!(decode_buffer_data_uri(&uri, 4).unwrap(), payload);
        assert!(matches!(
            decode_buffer_data_uri(&uri, 3),
            Err(Error::LimitExceeded(_))
        ));
        assert!(matches!(
            decode_buffer_data_uri("data:image/png;base64,AAAA", 100),
            Err(Error::Unsupported(_))
        ));
    }
}
