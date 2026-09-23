//! Autodesk DWG Release 11/12 (AC1009) binary entity decoder.
//!
//! DWG is Autodesk's proprietary CAD database format and its specification is
//! not published by Autodesk. This is a clean-room decoder for the AC1009
//! variant only, written from the *factual* on-disk layout (byte offsets,
//! field sizes, conditional-field bit flags) as documented by two independent
//! public, freely available descriptions of the format: the CC0-licensed
//! Kaitai Struct definition at
//! <https://github.com/michal-josef-spacek/CAD-Format-DWG-AC1009> and the
//! long-standing reverse-engineering notes at
//! <http://www.iwriteiam.nl/DWG12.html>. No code from either source, nor from
//! any DWG-reading implementation (including the GPL-licensed LibreDWG), was
//! copied; only the format's structure was consulted, and every field is
//! re-derived and re-validated here in original Rust.
//!
//! # Scope
//!
//! Only the AC1009 (R11/R12) six-byte version header is accepted; other DWG
//! versions fall back to the bounded header-only preview in
//! [`crate::document::dwg`]. Within a recognized AC1009 file, the
//! model-space `entities` section and, when present, the `block_entities`
//! (block definitions) and BLOCK/LAYER tables are decoded (the `extra_entities`
//! region and the STYLE/LTYPE/VIEW tables are not read). LINE, POINT, CIRCLE,
//! ARC, SOLID, TRACE, TEXT, the classic POLYLINE/VERTEX/SEQEND group, and
//! 3DFACE are turned into geometry. INSERT is expanded: its `block_index` is
//! resolved through the BLOCK table to a name, matched against a block
//! definition decoded from `block_entities`, and drawn with the INSERT's own
//! translation/scale/rotation and the block's base point — including nested
//! INSERTs, reusing the DXF reader's existing recursion-depth and cycle
//! protection. DIMENSION is drawn the same way, at the identity transform:
//! AutoCAD always caches a fully world-placed picture of a dimension
//! (extension lines, dimension line, arrowheads, text) as an anonymous block
//! and references it with the same kind of `block_index`, so no dimension
//! geometry is computed here. ATTDEF and ATTRIB are drawn like TEXT, using
//! their default/displayed value; either is skipped (like a real viewer)
//! when its own "invisible" flag is set. An INSERT or DIMENSION whose block
//! can't be resolved, or a block with no definition available, is simply not
//! drawn (not an error). VIEWPORT is drawn as its border rectangle — its
//! `width`/`height` fields are unconditional, so this is complete, real
//! geometry, not an approximation. SHAPE is *not* drawn: it references a
//! numbered glyph in an external, un-embedded `.SHX` compiled shape file, so
//! its outline simply isn't part of the DWG to reconstruct; rather than
//! folding it into the generic "unrecognized entity" bucket, it is counted
//! and named in its own warning so its presence is never silently invisible.
//! Every entity's own `entity_size` field bounds how far it is ever read or
//! skipped, so a single malformed or unrecognized entity can never misalign
//! — or run unbounded past the end of — the rest of the file.
//!
//! # Object Coordinate System (elevation and extrusion)
//!
//! LINE, POINT, CIRCLE, ARC, SOLID and the POLYLINE/VERTEX group carry an
//! elevation (Z, shared for a whole entity or per point) and, when present,
//! an extrusion/normal vector defining an arbitrary Object Coordinate
//! System (OCS) their 2D coordinates are actually expressed in — AutoCAD's
//! standard "Arbitrary Axis Algorithm" ([`arbitrary_axis`]) derives that
//! OCS's basis from just the extrusion vector. Every point this reader
//! draws is converted from its own OCS to World coordinates and only then
//! flattened to a top-down X/Y plan view (world Z dropped); for the
//! overwhelming majority of entities, which never set a non-default
//! extrusion, this is an exact no-op ([`is_default_extrusion`] fast path).
//! A CIRCLE in a tilted OCS genuinely foreshortens into an ellipse under
//! this flattening — computed exactly via a closed-form 2x2 SVD
//! ([`project_ocs_circle`]) — so it is drawn as one when needed. An ARC's
//! *position* gets the same OCS correction, but it is still drawn as a
//! circular (not elliptical) arc: this crate's shared `Entity::Ellipse`
//! rendering only draws full ellipses, not partial ones, so drawing a
//! tilted ARC as an ellipse would show the wrong shape (a full ellipse)
//! rather than a smaller, honest position-only correction. For the same
//! reason, a tilted POLYLINE's straight segments project exactly, but a
//! bulge (circular-arc) segment keeps its original bulge value between the
//! transformed endpoints rather than the foreshortened elliptical arc a
//! true projection would need. TEXT, TRACE and 3DFACE carry no extrusion
//! field in this binary format at all (3DFACE's points are already fully
//! 3D), so they are unaffected. INSERT/DIMENSION block instancing and
//! VIEWPORT keep the simplified WCS-aligned 2D transform model described
//! above; their own extrusion fields, if present, are not applied.

use std::collections::HashMap;

use crate::cad::color::aci_to_hex;
use crate::cad::dxf::render_dxf_to_page;
use crate::cad::dxf::types::{Block, DxfDocument, Entity, Layer, LwVertex};
use crate::convert::PageConsumer;
use crate::error::{Error, Result};
use crate::ir::Page;

const ENTITIES_SENTINEL_BEGIN: [u8; 16] = [
    0xC4, 0x6E, 0x68, 0x54, 0xF8, 0x6E, 0x33, 0x30, 0x63, 0x3E, 0xC1, 0x85, 0x2A, 0xDC, 0x94, 0x01,
];
const ENTITIES_SENTINEL_END: [u8; 16] = [
    0x3B, 0x91, 0x97, 0xAB, 0x07, 0x91, 0xCC, 0xCF, 0x9C, 0xC1, 0x3E, 0x7A, 0xD5, 0x23, 0x6B, 0xFE,
];
const LAYER_SENTINEL_BEGIN: [u8; 16] = [
    0x0E, 0xC4, 0x64, 0x6F, 0xBB, 0x1D, 0xD3, 0x8B, 0x00, 0x49, 0xC2, 0xEF, 0x18, 0xEA, 0x6F, 0xFB,
];
const LAYER_SENTINEL_END: [u8; 16] = [
    0xF1, 0x3B, 0x9B, 0x90, 0x44, 0xE2, 0x2C, 0x74, 0xFF, 0xB6, 0x3D, 0x10, 0xE7, 0x15, 0x90, 0x04,
];
const BLOCK_ENTITIES_SENTINEL_BEGIN: [u8; 16] = [
    0x72, 0x2B, 0x7D, 0xEC, 0x3E, 0x8C, 0x88, 0x6C, 0x7A, 0x72, 0x0A, 0xFD, 0xC8, 0x6C, 0x84, 0x26,
];
const BLOCK_ENTITIES_SENTINEL_END: [u8; 16] = [
    0x8D, 0xD4, 0x82, 0x13, 0xC1, 0x73, 0x77, 0x93, 0x85, 0x8D, 0xF5, 0x02, 0x37, 0x93, 0x7B, 0xD9,
];
const BLOCK_TABLE_SENTINEL_BEGIN: [u8; 16] = [
    0xDB, 0xEF, 0xB3, 0xF0, 0xC7, 0x3E, 0x6D, 0xA6, 0xC9, 0xB6, 0x24, 0x5C, 0x4C, 0x6F, 0x32, 0xCB,
];
const BLOCK_TABLE_SENTINEL_END: [u8; 16] = [
    0x24, 0x10, 0x4C, 0x0F, 0x38, 0xC1, 0x92, 0x59, 0x36, 0x49, 0xDB, 0xA3, 0xB3, 0x90, 0xCD, 0x34,
];

/// Minimum plausible AC1009 layer record: 1-byte flags, 32-byte name,
/// `used`/`color`/`linetype_index` (2 bytes each), 2-byte CRC.
const LAYER_RECORD_MIN_BYTES: usize = 41;
/// Minimum plausible AC1009 BLOCK table record: 1-byte flags, 32-byte name,
/// `used` (2), `begin_address_in_block_table_raw` (4), `block_entity` (2),
/// 1-byte flags2, 1-byte `u1`, 2-byte CRC.
const BLOCK_RECORD_MIN_BYTES: usize = 45;
const MAX_ENTITIES: usize = 500_000;
const MAX_LAYERS: usize = 100_000;
const MAX_BLOCKS: usize = 50_000;

/// Decodes an AC1009 DWG's model-space entities directly into a [`Page`] and
/// hands it to `sink`, reusing the DXF renderer for layout, coloring and
/// layer visibility so DWG and DXF output stay visually consistent.
///
/// Returns `Ok(None)` when `bytes` does not validate as a well-formed AC1009
/// file (wrong/absent version header, truncated header, or a sentinel that
/// does not match) so the caller can fall back to the bounded header-only
/// preview instead of guessing. Returns `Err` only for a genuine bounded
/// resource limit (too many entities or layers) so that case is reported to
/// the user rather than silently downgraded.
pub(crate) fn convert(bytes: &[u8], sink: &mut dyn PageConsumer) -> Result<Option<Vec<String>>> {
    let Some(header) = read_header(bytes) else {
        return Ok(None);
    };
    let Some(content) = entities_section(bytes, &header) else {
        return Ok(None);
    };

    let layers = match read_layer_table(bytes, &header) {
        Ok(rows) => rows,
        Err(LayerTableError::Limit(msg)) => return Err(Error::LimitExceeded(msg)),
        Err(LayerTableError::NotFound) => Vec::new(),
    };
    let block_names = match read_block_table(bytes, &header) {
        Ok(names) => names,
        Err(BlockTableError::Limit(msg)) => return Err(Error::LimitExceeded(msg)),
        Err(BlockTableError::NotFound) => Vec::new(),
    };

    let mut doc = DxfDocument::default();
    for row in &layers {
        doc.layers.insert(
            row.name.clone(),
            Layer {
                name: row.name.clone(),
                color_hex: aci_to_hex(row.color_aci),
                linetype: "CONTINUOUS".into(),
                is_off: row.is_off,
                is_frozen: row.is_frozen,
                line_weight: None,
            },
        );
    }

    let mut warnings = Vec::new();

    // Block definitions (referenced by INSERT) live in a separate section;
    // decode it first, when present, so INSERT expansion below can find them.
    if let Some(block_content) = block_entities_section(bytes, &header) {
        let mut block_state = WalkState::new();
        walk_entity_stream(
            block_content,
            &layers,
            &block_names,
            &mut block_state,
            &mut warnings,
        )?;
        doc.blocks.extend(block_state.blocks);
        if !block_state.top_level.is_empty() {
            warnings.push(format!(
                "{} entit{suffix} in the DWG block-definition section were outside any BLOCK/ENDBLK pair and were not used",
                block_state.top_level.len(),
                suffix = if block_state.top_level.len() == 1 { "y" } else { "ies" }
            ));
        }
    }

    let mut main_state = WalkState::new();
    walk_entity_stream(
        content,
        &layers,
        &block_names,
        &mut main_state,
        &mut warnings,
    )?;
    doc.entities = main_state.top_level;
    doc.blocks.extend(main_state.blocks);

    let mut page: Page = render_dxf_to_page(&doc, &mut warnings)?;
    page.source_format = "dwg".into();
    page.title = "AutoCAD DWG (R11/R12) drawing".into();
    sink.consume(page)?;
    Ok(Some(warnings))
}

struct DwgHeader {
    entities_start: i32,
    entities_end: i32,
    block_entities_start: i32,
    block_entities_size: u32,
    table_block_item_size: u16,
    table_block_items: u16,
    table_block_begin: u32,
    table_layer_item_size: u16,
    table_layer_items: u16,
    table_layer_begin: u32,
}

fn read_header(bytes: &[u8]) -> Option<DwgHeader> {
    if bytes.len() < 94 || !bytes[..6].eq(b"AC1009") {
        return None;
    }
    let s4 = |off: usize| -> Option<i32> {
        Some(i32::from_le_bytes(bytes[off..off + 4].try_into().ok()?))
    };
    let u4 = |off: usize| -> Option<u32> {
        Some(u32::from_le_bytes(bytes[off..off + 4].try_into().ok()?))
    };
    let entities_start = s4(20)?;
    let entities_end = s4(24)?;
    let block_entities_start = s4(28)?;
    // The top byte of this raw field is a separate (unused here) counter;
    // only the low 24 bits are the actual byte size.
    let block_entities_size = u4(32)? & 0x00ff_ffff;
    let table_block_item_size = u16::from_le_bytes(bytes[44..46].try_into().ok()?);
    let table_block_items = u16::from_le_bytes(bytes[46..48].try_into().ok()?);
    let table_block_begin = u32::from_le_bytes(bytes[50..54].try_into().ok()?);
    let table_layer_item_size = u16::from_le_bytes(bytes[54..56].try_into().ok()?);
    let table_layer_items = u16::from_le_bytes(bytes[56..58].try_into().ok()?);
    let table_layer_begin = u32::from_le_bytes(bytes[60..64].try_into().ok()?);
    Some(DwgHeader {
        entities_start,
        entities_end,
        block_entities_start,
        block_entities_size,
        table_block_item_size,
        table_block_items,
        table_block_begin,
        table_layer_item_size,
        table_layer_items,
        table_layer_begin,
    })
}

/// Returns the bounded, sentinel-validated `real_entities` byte range.
fn entities_section<'a>(bytes: &'a [u8], header: &DwgHeader) -> Option<&'a [u8]> {
    if header.entities_start < 16 || header.entities_end < header.entities_start {
        return None;
    }
    let start = header.entities_start as usize;
    let end = header.entities_end as usize;
    if start < 16 || end > bytes.len() || end.checked_add(16)? > bytes.len() {
        return None;
    }
    if bytes.get(start - 16..start)? != ENTITIES_SENTINEL_BEGIN {
        return None;
    }
    if bytes.get(end..end + 16)? != ENTITIES_SENTINEL_END {
        return None;
    }
    bytes.get(start..end)
}

/// Returns the bounded, sentinel-validated block-definition `real_entities`
/// byte range (`entities_block` in the format), or `None` when the size is
/// zero, the layout is out of bounds, or a sentinel doesn't match — in which
/// case block definitions are simply unavailable and INSERT references stay
/// unexpanded, rather than failing the whole conversion.
fn block_entities_section<'a>(bytes: &'a [u8], header: &DwgHeader) -> Option<&'a [u8]> {
    if header.block_entities_start < 16 || header.block_entities_size == 0 {
        return None;
    }
    let start = header.block_entities_start as usize;
    let len = header.block_entities_size as usize;
    if bytes.get(start - 16..start)? != BLOCK_ENTITIES_SENTINEL_BEGIN {
        return None;
    }
    let end = start.checked_add(len)?;
    if end.checked_add(16)? > bytes.len() {
        return None;
    }
    if bytes.get(end..end + 16)? != BLOCK_ENTITIES_SENTINEL_END {
        return None;
    }
    bytes.get(start..end)
}

struct DwgLayerRow {
    name: String,
    color_aci: i16,
    is_off: bool,
    is_frozen: bool,
}

enum LayerTableError {
    NotFound,
    Limit(String),
}

fn read_layer_table(
    bytes: &[u8],
    header: &DwgHeader,
) -> std::result::Result<Vec<DwgLayerRow>, LayerTableError> {
    let items = header.table_layer_items as usize;
    let item_size = header.table_layer_item_size as usize;
    let begin = header.table_layer_begin as usize;
    if items == 0 {
        return Ok(Vec::new());
    }
    if items > MAX_LAYERS {
        return Err(LayerTableError::Limit(format!(
            "DWG layer table exceeds maximum supported entries ({MAX_LAYERS})"
        )));
    }
    if item_size < LAYER_RECORD_MIN_BYTES || begin < 16 {
        return Err(LayerTableError::NotFound);
    }
    let sentinel_begin_pos = begin - 16;
    if bytes.get(sentinel_begin_pos..begin) != Some(&LAYER_SENTINEL_BEGIN[..]) {
        return Err(LayerTableError::NotFound);
    }
    let Some(content_len) = items.checked_mul(item_size) else {
        return Err(LayerTableError::NotFound);
    };
    let Some(end) = begin.checked_add(content_len) else {
        return Err(LayerTableError::NotFound);
    };
    if bytes.get(end..end + 16) != Some(&LAYER_SENTINEL_END[..]) {
        return Err(LayerTableError::NotFound);
    }

    let mut out = Vec::with_capacity(items);
    for i in 0..items {
        let rec_start = begin + i * item_size;
        let rec = &bytes[rec_start..rec_start + item_size];
        let flag_byte = rec[0];
        let name = decode_dwg_text(&rec[1..33]);
        let color = i16::from_le_bytes([rec[35], rec[36]]);
        out.push(DwgLayerRow {
            name: if name.is_empty() {
                format!("LAYER_{i}")
            } else {
                name
            },
            color_aci: color,
            is_off: color < 0,
            is_frozen: bit(flag_byte, 8),
        });
    }
    Ok(out)
}

enum BlockTableError {
    NotFound,
    Limit(String),
}

/// Reads the BLOCK table's fixed-width, 32-byte-name records into a
/// `block_index -> name` lookup (0-based, table order), used to resolve
/// `INSERT`'s numeric `block_index` field to the name that keys
/// [`DxfDocument::blocks`]. Errors the same way [`read_layer_table`] does:
/// `NotFound` means "not available", not "corrupt file".
fn read_block_table(
    bytes: &[u8],
    header: &DwgHeader,
) -> std::result::Result<Vec<String>, BlockTableError> {
    let items = header.table_block_items as usize;
    let item_size = header.table_block_item_size as usize;
    let begin = header.table_block_begin as usize;
    if items == 0 {
        return Ok(Vec::new());
    }
    if items > MAX_LAYERS {
        return Err(BlockTableError::Limit(format!(
            "DWG block table exceeds maximum supported entries ({MAX_LAYERS})"
        )));
    }
    if item_size < BLOCK_RECORD_MIN_BYTES || begin < 16 {
        return Err(BlockTableError::NotFound);
    }
    let sentinel_begin_pos = begin - 16;
    if bytes.get(sentinel_begin_pos..begin) != Some(&BLOCK_TABLE_SENTINEL_BEGIN[..]) {
        return Err(BlockTableError::NotFound);
    }
    let Some(content_len) = items.checked_mul(item_size) else {
        return Err(BlockTableError::NotFound);
    };
    let Some(end) = begin.checked_add(content_len) else {
        return Err(BlockTableError::NotFound);
    };
    if bytes.get(end..end + 16) != Some(&BLOCK_TABLE_SENTINEL_END[..]) {
        return Err(BlockTableError::NotFound);
    }

    let mut out = Vec::with_capacity(items);
    for i in 0..items {
        let rec_start = begin + i * item_size;
        let rec = &bytes[rec_start..rec_start + item_size];
        let name = decode_dwg_text(&rec[1..33]);
        out.push(if name.is_empty() {
            format!("BLOCK_{i}")
        } else {
            name
        });
    }
    Ok(out)
}

/// Decodes a DWG name/text byte field: trims at the first NUL, then decodes
/// as UTF-8 or falls back to Windows-1252 for legacy non-ASCII drawings.
fn decode_dwg_text(raw: &[u8]) -> String {
    let trimmed = match raw.iter().position(|&b| b == 0) {
        Some(i) => &raw[..i],
        None => raw,
    };
    match std::str::from_utf8(trimmed) {
        Ok(s) => s.trim().to_string(),
        Err(_) => {
            let (text, _, _) = encoding_rs::WINDOWS_1252.decode(trimmed);
            text.trim().to_string()
        }
    }
}

/// Tests the `n`-th (1-based) bit of `byte` in the declaration order used
/// throughout the AC1009 format's flag bytes: the first-declared flag is the
/// most significant bit, the last-declared is the least significant.
fn bit(byte: u8, n: u32) -> bool {
    (byte >> (8 - n)) & 1 == 1
}

type Vec3 = (f64, f64, f64);

fn vec3_normalize(v: Vec3) -> Vec3 {
    let len = (v.0 * v.0 + v.1 * v.1 + v.2 * v.2).sqrt();
    if len > 1e-10 {
        (v.0 / len, v.1 / len, v.2 / len)
    } else {
        // Degenerate/zero extrusion vector in the file; fall back to the
        // WCS-aligned default rather than propagating a NaN.
        (0.0, 0.0, 1.0)
    }
}

fn vec3_cross(a: Vec3, b: Vec3) -> Vec3 {
    (
        a.1 * b.2 - a.2 * b.1,
        a.2 * b.0 - a.0 * b.2,
        a.0 * b.1 - a.1 * b.0,
    )
}

/// True when `v` is close enough to the default WCS-aligned `(0,0,1)`
/// extrusion that the OCS-to-WCS transform is the identity. Used both as a
/// fast path and to keep untouched entities free of floating-point noise.
fn is_default_extrusion(v: Vec3) -> bool {
    v.0.abs() < 1e-9 && v.1.abs() < 1e-9 && (v.2 - 1.0).abs() < 1e-9
}

/// AutoCAD's "Arbitrary Axis Algorithm": derives an Object Coordinate
/// System's X/Y/Z basis (as WCS vectors) from just an entity's extrusion
/// (normal) vector — the same construction AutoCAD itself uses, since a
/// single normal vector alone doesn't pin down a rotation about it.
fn arbitrary_axis(extrusion: Vec3) -> (Vec3, Vec3, Vec3) {
    let n = vec3_normalize(extrusion);
    const LIMIT: f64 = 1.0 / 64.0;
    let world_ref = if n.0.abs() < LIMIT && n.1.abs() < LIMIT {
        (0.0, 1.0, 0.0)
    } else {
        (0.0, 0.0, 1.0)
    };
    let ax = vec3_normalize(vec3_cross(world_ref, n));
    let ay = vec3_cross(n, ax);
    (ax, ay, n)
}

/// Converts one Object Coordinate System point `(x, y, z)` under
/// `extrusion` (`None` meaning the WCS-aligned default) to its position in
/// World coordinates, keeping only (X, Y): every DWG/DXF entity this
/// reader draws is a flat X/Y plan view, so world Z is computed (it
/// affects X/Y whenever the OCS is tilted) and then dropped.
fn ocs_point_to_world_xy(x: f64, y: f64, z: f64, extrusion: Option<Vec3>) -> (f64, f64) {
    let extrusion = match extrusion {
        Some(v) => v,
        None => return (x, y),
    };
    if is_default_extrusion(extrusion) {
        return (x, y);
    }
    let (ax, ay, n) = arbitrary_axis(extrusion);
    let wx = x * ax.0 + y * ay.0 + z * n.0;
    let wy = x * ax.1 + y * ay.1 + z * n.1;
    (wx, wy)
}

/// Reads a `point_3d` extrusion vector (3 little-endian `f64`s).
fn read_extrusion(cur: &mut Cursor) -> Option<Vec3> {
    Some((cur.f64()?, cur.f64()?, cur.f64()?))
}

/// Projects a CIRCLE of the given `radius`, defined in an OCS with basis
/// vectors `ax`/`ay` (from [`arbitrary_axis`]), onto the flat WCS X/Y plan
/// view. A circle in a tilted OCS generally foreshortens into an ellipse
/// when flattened this way; this returns the projected major-axis vector
/// and axis ratio via a closed-form 2x2 SVD of the `[ax_xy | ay_xy]`
/// basis (the two columns being the WCS X/Y components of the OCS's own
/// X/Y unit axes). When the OCS plane is parallel to WCS X/Y (untilted,
/// only rotated/mirrored about Z), this naturally reduces to
/// `axis_ratio == 1.0` — a true circle, no foreshortening.
fn project_ocs_circle(radius: f64, ax: Vec3, ay: Vec3) -> ((f64, f64), f64) {
    // M = [ax_xy | ay_xy]; M^T*M is symmetric, its eigenvalues are the
    // squared singular values (semi-axis lengths, before scaling by
    // `radius`) and its eigenvectors are M's input-space (V) directions.
    let e = ax.0 * ax.0 + ax.1 * ax.1;
    let f = ax.0 * ay.0 + ax.1 * ay.1;
    let g = ay.0 * ay.0 + ay.1 * ay.1;
    let mid = (e + g) / 2.0;
    let half_diff = ((e - g) * (e - g) / 4.0 + f * f).max(0.0).sqrt();
    let sigma1 = (mid + half_diff).max(0.0).sqrt();
    let sigma2 = (mid - half_diff).max(0.0).sqrt();

    // Eigenvector of [[e,f],[f,g]] for the larger eigenvalue (mid+half_diff).
    let v1 = if f.abs() > 1e-12 {
        let (vx, vy) = (f, mid + half_diff - e);
        let len = (vx * vx + vy * vy).sqrt();
        if len > 1e-12 {
            (vx / len, vy / len)
        } else {
            (1.0, 0.0)
        }
    } else if e >= g {
        (1.0, 0.0)
    } else {
        (0.0, 1.0)
    };

    // Output-space (major axis) direction: u1 = M*v1, normalized.
    let u1_raw = (ax.0 * v1.0 + ay.0 * v1.1, ax.1 * v1.0 + ay.1 * v1.1);
    let u1_len = (u1_raw.0 * u1_raw.0 + u1_raw.1 * u1_raw.1).sqrt();
    let u1 = if u1_len > 1e-12 {
        (u1_raw.0 / u1_len, u1_raw.1 / u1_len)
    } else {
        (1.0, 0.0)
    };

    let major_axis = (radius * sigma1 * u1.0, radius * sigma1 * u1.1);
    let axis_ratio = if sigma1 > 1e-12 {
        (sigma2 / sigma1).clamp(0.0, 1.0)
    } else {
        1.0
    };
    (major_axis, axis_ratio)
}

/// A bounds-checked little-endian cursor over one entity's own byte range.
/// Every read fails safely (`None`) instead of panicking on truncated data;
/// callers always resynchronize to the next entity via its own
/// `entity_size` regardless of how far this cursor got.
struct Cursor<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Cursor<'a> {
    fn new(buf: &'a [u8]) -> Self {
        Self { buf, pos: 0 }
    }

    fn bytes(&mut self, n: usize) -> Option<&'a [u8]> {
        let end = self.pos.checked_add(n)?;
        let s = self.buf.get(self.pos..end)?;
        self.pos = end;
        Some(s)
    }

    fn skip(&mut self, n: usize) -> Option<()> {
        self.bytes(n).map(|_| ())
    }

    fn u8(&mut self) -> Option<u8> {
        self.bytes(1).map(|b| b[0])
    }

    fn i8(&mut self) -> Option<i8> {
        self.u8().map(|b| b as i8)
    }

    fn u16(&mut self) -> Option<u16> {
        self.bytes(2).map(|b| u16::from_le_bytes([b[0], b[1]]))
    }

    fn i16(&mut self) -> Option<i16> {
        self.u16().map(|v| v as i16)
    }

    fn f64(&mut self) -> Option<f64> {
        self.bytes(8)
            .map(|b| f64::from_le_bytes(b.try_into().unwrap()))
    }
}

/// Fields shared by (almost) every entity type once its mandatory
/// `entity_mode`/`entity_size`/`entity_layer_index`/`entity_common` header
/// has been read: the paperspace/extended-data/color/linetype/thickness/
/// handle/viewport chain that precedes the entity-specific geometry.
struct CommonPrefix {
    /// Raw `entity_common` high byte (`flag2_1`..`flag2_8`, MSB-first).
    common_hi: u8,
    /// Raw `entity_common` low byte (`flag3_1`..`flag3_8`, MSB-first).
    common_lo: u8,
    color_aci: Option<i16>,
    /// The entity's shared elevation (Z in its own OCS), when this entity
    /// type stores one and it was present; `0.0` otherwise. Combined with
    /// an extrusion vector via [`ocs_point_to_world_xy`] to place a planar
    /// entity anywhere in 3D. LINE/POINT/3DFACE store Z per point instead
    /// (`use_elevation = false`), so this is always `0.0` for them.
    elevation: f64,
}

/// Reads the common prefix in the order used by most entity types:
/// `entity_common`, then (if paperspace) `extra_flag`/`eed`, then color,
/// linetype, thickness, elevation, handle, viewport.
///
/// `use_elevation` selects whether this entity type stores a shared
/// elevation (Z) scalar here at all — LINE/POINT/3DFACE store Z per point
/// instead, so callers pass `false` for them.
fn read_common_prefix(cur: &mut Cursor, mode: u8, use_elevation: bool) -> Option<CommonPrefix> {
    let common_hi = cur.u8()?;
    let common_lo = cur.u8()?;
    let has_pspace = bit(mode, 2);
    let mut has_viewport = false;
    if has_pspace {
        let extra = cur.u8()?;
        has_viewport = bit(extra, 6);
        if bit(extra, 7) {
            let len = cur.u16()? as usize;
            cur.skip(len)?;
        }
    }
    let color_aci = if bit(mode, 8) {
        Some(cur.i8()? as i16)
    } else {
        None
    };
    if bit(mode, 7) {
        cur.skip(2)?; // entity_linetype_index
    }
    if bit(mode, 5) {
        cur.skip(8)?; // entity_thickness
    }
    let elevation = if use_elevation && bit(mode, 6) {
        cur.f64()?
    } else {
        0.0
    };
    if bit(mode, 3) {
        let len = cur.u8()? as usize;
        cur.skip(len)?; // handling_id
    }
    if has_pspace && has_viewport {
        cur.skip(2)?;
    }
    Some(CommonPrefix {
        common_hi,
        common_lo,
        color_aci,
        elevation,
    })
}

/// Reads the common prefix in the alternate order the format spec uses for
/// TRACE and VERTEX: color/linetype/thickness/elevation come first, then
/// (if paperspace) `extra_flag`/`eed`, then handle, then viewport. See
/// [`read_common_prefix`] for the meaning of `use_elevation`.
fn read_common_prefix_late(
    cur: &mut Cursor,
    mode: u8,
    use_elevation: bool,
) -> Option<CommonPrefix> {
    let common_hi = cur.u8()?;
    let common_lo = cur.u8()?;
    let color_aci = if bit(mode, 8) {
        Some(cur.i8()? as i16)
    } else {
        None
    };
    if bit(mode, 7) {
        cur.skip(2)?;
    }
    if bit(mode, 5) {
        cur.skip(8)?;
    }
    let elevation = if use_elevation && bit(mode, 6) {
        cur.f64()?
    } else {
        0.0
    };
    let has_pspace = bit(mode, 2);
    let mut has_viewport = false;
    if has_pspace {
        let extra = cur.u8()?;
        has_viewport = bit(extra, 6);
        if bit(extra, 7) {
            let len = cur.u16()? as usize;
            cur.skip(len)?;
        }
    }
    if bit(mode, 3) {
        let len = cur.u8()? as usize;
        cur.skip(len)?;
    }
    if has_pspace && has_viewport {
        cur.skip(2)?;
    }
    Some(CommonPrefix {
        common_hi,
        common_lo,
        color_aci,
        elevation,
    })
}

struct InsertPrefix {
    insert_flags: u8,
    color_aci: Option<i16>,
}

/// Reads INSERT's own prefix, which uses a 1-byte `entity_insert_flags` in
/// place of the usual 2-byte `entity_common`, and reads elevation before
/// thickness (the opposite order from [`read_common_prefix`]).
fn read_insert_prefix(cur: &mut Cursor, mode: u8) -> Option<InsertPrefix> {
    let insert_flags = cur.u8()?;
    let has_pspace = bit(mode, 2);
    let mut has_viewport = false;
    if has_pspace {
        let extra = cur.u8()?;
        has_viewport = bit(extra, 6);
        if bit(extra, 7) {
            let len = cur.u16()? as usize;
            cur.skip(len)?;
        }
    }
    let color_aci = if bit(mode, 8) {
        Some(cur.i8()? as i16)
    } else {
        None
    };
    if bit(mode, 7) {
        cur.skip(2)?; // entity_linetype_index
    }
    if bit(mode, 6) {
        cur.skip(8)?; // entity_elevation (unused, flat XY projection)
    }
    if bit(mode, 5) {
        cur.skip(8)?; // entity_thickness
    }
    if bit(mode, 3) {
        let len = cur.u8()? as usize;
        cur.skip(len)?;
    }
    if has_pspace && has_viewport {
        cur.skip(2)?;
    }
    Some(InsertPrefix {
        insert_flags,
        color_aci,
    })
}

struct OpenPolyline {
    layer: String,
    color: Option<String>,
    closed: bool,
    vertices: Vec<LwVertex>,
    /// The POLYLINE's own shared elevation/extrusion, applied to every
    /// VERTEX belonging to it (VERTEX has no OCS fields of its own).
    elevation: f64,
    extrusion: Option<Vec3>,
}

/// A BLOCK/ENDBLK definition currently being accumulated while walking an
/// entity stream; entities decoded between the two go here instead of the
/// surrounding top-level entity list.
struct OpenBlock {
    name: String,
    base_point: (f64, f64),
    entities: Vec<Entity>,
}

fn block_name_for_index(block_names: &[String], index: i16) -> Option<String> {
    if index >= 0 {
        block_names.get(index as usize).cloned()
    } else {
        None
    }
}

fn layer_name(layers: &[DwgLayerRow], index: i16) -> String {
    if index >= 0
        && let Some(row) = layers.get(index as usize)
    {
        return row.name.clone();
    }
    "0".into()
}

/// Accumulates decoded entities while walking one entity stream (the
/// model-space `entities` section or the `block_entities` section): a
/// top-level list, a `name -> Block` map built from BLOCK/ENDBLK pairs, and
/// the in-progress POLYLINE/BLOCK accumulators. Entities decoded while a
/// BLOCK is open go into that block's own entity list instead of the
/// top-level list, so the same walker works for either section.
struct WalkState {
    top_level: Vec<Entity>,
    blocks: HashMap<String, Block>,
    open_polyline: Option<OpenPolyline>,
    open_block: Option<OpenBlock>,
}

impl WalkState {
    fn new() -> Self {
        Self {
            top_level: Vec::new(),
            blocks: HashMap::new(),
            open_polyline: None,
            open_block: None,
        }
    }
}

/// Pushes a finished POLYLINE (if any vertices were accumulated) into
/// whichever entity list is current: the open BLOCK's, or the top level.
fn finalize_open_polyline(state: &mut WalkState) {
    let Some(open) = state.open_polyline.take() else {
        return;
    };
    if open.vertices.is_empty() {
        return;
    }
    let entity = Entity::LwPolyline {
        vertices: open.vertices,
        is_closed: open.closed,
        layer: open.layer,
        color: open.color,
        line_weight: None,
        linetype: None,
    };
    match state.open_block.as_mut() {
        Some(ob) => ob.entities.push(entity),
        None => state.top_level.push(entity),
    }
}

/// Moves the open BLOCK's accumulated entities into `state.blocks`, keyed
/// by its name. Bounded by [`MAX_BLOCKS`]; past that, further block
/// definitions are simply dropped rather than growing unbounded.
fn finalize_open_block(state: &mut WalkState) {
    let Some(open) = state.open_block.take() else {
        return;
    };
    if state.blocks.len() < MAX_BLOCKS {
        state.blocks.insert(
            open.name.clone(),
            Block {
                name: open.name,
                base_point: open.base_point,
                entities: open.entities,
            },
        );
    }
}

/// Walks one bounded entity stream (`content`), decoding entities into
/// `state`. Called once for the model-space `entities` section and, when
/// present, once more for the `block_entities` section — both use the
/// identical on-disk `real_entities` record format.
fn walk_entity_stream(
    content: &[u8],
    layers: &[DwgLayerRow],
    block_names: &[String],
    state: &mut WalkState,
    warnings: &mut Vec<String>,
) -> Result<()> {
    let mut pos = 0usize;
    let mut entity_count = 0usize;
    let mut unresolved_insert_count = 0usize;
    let mut unresolved_dimension_count = 0usize;
    let mut shape_count = 0usize;
    let mut malformed_count = 0usize;

    while pos < content.len() {
        if pos + 4 > content.len() {
            warnings.push(format!(
                "DWG entity stream has {} trailing byte(s) that do not form a complete entity record; ignored",
                content.len() - pos
            ));
            break;
        }
        entity_count += 1;
        if entity_count > MAX_ENTITIES {
            return Err(Error::LimitExceeded(format!(
                "DWG entity count exceeds maximum supported entries ({MAX_ENTITIES})"
            )));
        }

        let entity_type = content[pos] as i8;
        let mode = content[pos + 1];
        let size = i16::from_le_bytes([content[pos + 2], content[pos + 3]]);

        if size < 4 {
            warnings.push(
                "DWG entity record has an invalid size field; stopped reading entities".into(),
            );
            break;
        }
        let Some(record_end) = pos.checked_add(size as usize) else {
            warnings.push("DWG entity record size overflowed; stopped reading entities".into());
            break;
        };
        if record_end > content.len() {
            warnings.push(
                "DWG entity record extends past the entities section; stopped reading entities"
                    .into(),
            );
            break;
        }

        if entity_type == 18 {
            // JUMP: no layer index, nothing to draw; already bounded by size.
            pos = record_end;
            continue;
        }

        let body_start = pos + 4;
        if body_start + 2 > record_end {
            malformed_count += 1;
            pos = record_end;
            continue;
        }
        let layer_index = i16::from_le_bytes([content[body_start], content[body_start + 1]]);
        let layer = layer_name(layers, layer_index);
        let mut cur = Cursor::new(&content[body_start + 2..record_end]);

        match entity_type {
            12 => {
                // BLOCK_BEGIN: opens a named block accumulator. A polyline
                // or block already open (malformed input) is finalized
                // first rather than silently dropped.
                finalize_open_polyline(state);
                if let Some(name) = decode_block_begin(&mut cur, mode) {
                    finalize_open_block(state);
                    state.open_block = Some(OpenBlock {
                        name: name.0,
                        base_point: name.1,
                        entities: Vec::new(),
                    });
                } else {
                    malformed_count += 1;
                }
            }
            13 => {
                // BLOCK_END: close the open block, if any. Its own body
                // (there isn't one beyond the common prefix) is never needed.
                finalize_open_polyline(state);
                finalize_open_block(state);
            }
            _ => {
                let target: &mut Vec<Entity> = match state.open_block.as_mut() {
                    Some(ob) => &mut ob.entities,
                    None => &mut state.top_level,
                };
                let outcome = decode_entity(
                    entity_type,
                    mode,
                    &mut cur,
                    &layer,
                    target,
                    &mut state.open_polyline,
                    block_names,
                );
                match outcome {
                    DecodeOutcome::Ok | DecodeOutcome::Insert => {}
                    DecodeOutcome::Malformed => malformed_count += 1,
                    DecodeOutcome::InsertUnresolved => unresolved_insert_count += 1,
                    DecodeOutcome::DimensionUnresolved => unresolved_dimension_count += 1,
                    DecodeOutcome::ShapeSkipped => shape_count += 1,
                    DecodeOutcome::Skipped => {}
                }
            }
        }

        pos = record_end;
    }

    finalize_open_polyline(state);
    finalize_open_block(state);

    if unresolved_insert_count > 0 {
        warnings.push(format!(
            "{unresolved_insert_count} INSERT entit{suffix} referenced a block index this reader could not resolve to a name",
            suffix = if unresolved_insert_count == 1 { "y" } else { "ies" }
        ));
    }
    if unresolved_dimension_count > 0 {
        warnings.push(format!(
            "{unresolved_dimension_count} DIMENSION entit{suffix} referenced a block index this reader could not resolve to a name, so its dimension geometry was not drawn",
            suffix = if unresolved_dimension_count == 1 { "y" } else { "ies" }
        ));
    }
    if shape_count > 0 {
        warnings.push(format!(
            "{shape_count} SHAPE entit{suffix} reference{suffix2} a glyph in an external .SHX file that is not embedded in the drawing, so {pronoun} outline was not drawn",
            suffix = if shape_count == 1 { "y" } else { "ies" },
            suffix2 = if shape_count == 1 { "s" } else { "" },
            pronoun = if shape_count == 1 { "its" } else { "their" }
        ));
    }
    if malformed_count > 0 {
        warnings.push(format!(
            "{malformed_count} DWG entity record(s) had unexpected internal layout and were skipped"
        ));
    }

    Ok(())
}

/// Decodes a BLOCK_BEGIN entity's own prefix, base point, and (when
/// present) name. Returns `None` on truncated data or a missing/empty name
/// — a block this reader cannot name can never be matched by an INSERT's
/// resolved block name, so it isn't worth keeping.
fn decode_block_begin(cur: &mut Cursor, mode: u8) -> Option<(String, (f64, f64))> {
    let pre = read_common_prefix(cur, mode, true)?;
    let base_point = (cur.f64()?, cur.f64()?);
    if bit(pre.common_hi, 7) {
        // xref_pname: length-prefixed, precedes the name field.
        let len = cur.u16()? as usize;
        cur.skip(len)?;
    }
    if !bit(pre.common_hi, 6) {
        return None;
    }
    let len = cur.u16()? as usize;
    let raw = cur.bytes(len)?;
    let name = decode_dwg_text(raw);
    if name.is_empty() {
        return None;
    }
    Some((name, base_point))
}

enum DecodeOutcome {
    Ok,
    Malformed,
    Insert,
    InsertUnresolved,
    DimensionUnresolved,
    ShapeSkipped,
    Skipped,
}

#[allow(clippy::too_many_arguments)]
fn decode_entity(
    entity_type: i8,
    mode: u8,
    cur: &mut Cursor,
    layer: &str,
    entities_out: &mut Vec<Entity>,
    open_polyline: &mut Option<OpenPolyline>,
    block_names: &[String],
) -> DecodeOutcome {
    match entity_type {
        1 => {
            // LINE: x1,y1,[z1],x2,y2,[z2],[extrusion] — Z and extrusion are
            // per-entity (one OCS for both endpoints), Z itself per-point.
            let Some(pre) = read_common_prefix(cur, mode, false) else {
                return DecodeOutcome::Malformed;
            };
            let (Some(x1), Some(y1)) = (cur.f64(), cur.f64()) else {
                return DecodeOutcome::Malformed;
            };
            let z1 = if !bit(mode, 6) {
                match cur.f64() {
                    Some(v) => v,
                    None => return DecodeOutcome::Malformed,
                }
            } else {
                0.0
            };
            let (Some(x2), Some(y2)) = (cur.f64(), cur.f64()) else {
                return DecodeOutcome::Malformed;
            };
            let z2 = if !bit(mode, 6) {
                match cur.f64() {
                    Some(v) => v,
                    None => return DecodeOutcome::Malformed,
                }
            } else {
                0.0
            };
            let extrusion = if bit(pre.common_hi, 8) {
                match read_extrusion(cur) {
                    Some(e) => Some(e),
                    None => return DecodeOutcome::Malformed,
                }
            } else {
                None
            };
            entities_out.push(Entity::Line {
                start: ocs_point_to_world_xy(x1, y1, z1, extrusion),
                end: ocs_point_to_world_xy(x2, y2, z2, extrusion),
                layer: layer.into(),
                color: pre.color_aci.map(aci_to_hex),
                line_weight: None,
                linetype: None,
            });
            DecodeOutcome::Ok
        }
        2 => {
            // POINT: x,y,[z],[extrusion] — Z is per-point, not shared.
            let Some(pre) = read_common_prefix(cur, mode, false) else {
                return DecodeOutcome::Malformed;
            };
            let (Some(x), Some(y)) = (cur.f64(), cur.f64()) else {
                return DecodeOutcome::Malformed;
            };
            let z = if !bit(mode, 6) {
                match cur.f64() {
                    Some(v) => v,
                    None => return DecodeOutcome::Malformed,
                }
            } else {
                0.0
            };
            let extrusion = if bit(pre.common_hi, 8) {
                match read_extrusion(cur) {
                    Some(e) => Some(e),
                    None => return DecodeOutcome::Malformed,
                }
            } else {
                None
            };
            entities_out.push(Entity::Point {
                pt: ocs_point_to_world_xy(x, y, z, extrusion),
                layer: layer.into(),
                color: pre.color_aci.map(aci_to_hex),
            });
            DecodeOutcome::Ok
        }
        3 => {
            // CIRCLE: center(x,y), radius, [extrusion]. A tilted OCS
            // foreshortens the circle into an ellipse when flattened.
            let Some(pre) = read_common_prefix(cur, mode, true) else {
                return DecodeOutcome::Malformed;
            };
            let (Some(cx), Some(cy), Some(radius)) = (cur.f64(), cur.f64(), cur.f64()) else {
                return DecodeOutcome::Malformed;
            };
            let extrusion = if bit(pre.common_hi, 8) {
                match read_extrusion(cur) {
                    Some(e) => Some(e),
                    None => return DecodeOutcome::Malformed,
                }
            } else {
                None
            };
            let center = ocs_point_to_world_xy(cx, cy, pre.elevation, extrusion);
            match extrusion.filter(|&e| !is_default_extrusion(e)) {
                Some(e) => {
                    let (ax, ay, _n) = arbitrary_axis(e);
                    let (major_axis, axis_ratio) = project_ocs_circle(radius, ax, ay);
                    entities_out.push(Entity::Ellipse {
                        center,
                        major_axis,
                        axis_ratio,
                        start_param: 0.0,
                        end_param: std::f64::consts::TAU,
                        layer: layer.into(),
                        color: pre.color_aci.map(aci_to_hex),
                        line_weight: None,
                        linetype: None,
                    });
                }
                None => {
                    entities_out.push(Entity::Circle {
                        center,
                        radius,
                        layer: layer.into(),
                        color: pre.color_aci.map(aci_to_hex),
                        line_weight: None,
                        linetype: None,
                    });
                }
            }
            DecodeOutcome::Ok
        }
        8 => {
            // ARC: center(x,y), radius, angle_from, angle_to (radians),
            // [extrusion]. Only the center's OCS position is corrected;
            // the arc is still drawn as a circular (not elliptical) arc,
            // since this crate's Ellipse rendering only draws full
            // ellipses, not partial ones. See the module docs.
            let Some(pre) = read_common_prefix(cur, mode, true) else {
                return DecodeOutcome::Malformed;
            };
            let (Some(cx), Some(cy), Some(radius), Some(a1), Some(a2)) =
                (cur.f64(), cur.f64(), cur.f64(), cur.f64(), cur.f64())
            else {
                return DecodeOutcome::Malformed;
            };
            let extrusion = if bit(pre.common_hi, 8) {
                match read_extrusion(cur) {
                    Some(e) => Some(e),
                    None => return DecodeOutcome::Malformed,
                }
            } else {
                None
            };
            let center = ocs_point_to_world_xy(cx, cy, pre.elevation, extrusion);
            entities_out.push(Entity::Arc {
                center,
                radius,
                start_deg: a1.to_degrees(),
                end_deg: a2.to_degrees(),
                layer: layer.into(),
                color: pre.color_aci.map(aci_to_hex),
                line_weight: None,
                linetype: None,
            });
            DecodeOutcome::Ok
        }
        11 => {
            // SOLID: four 2D corners in file order (0,1,3,2 winding is
            // applied by the shared DXF renderer, not here), [extrusion].
            let Some(pre) = read_common_prefix(cur, mode, true) else {
                return DecodeOutcome::Malformed;
            };
            let Some(points) = read_four_points_2d(cur) else {
                return DecodeOutcome::Malformed;
            };
            let extrusion = if bit(pre.common_hi, 8) {
                match read_extrusion(cur) {
                    Some(e) => Some(e),
                    None => return DecodeOutcome::Malformed,
                }
            } else {
                None
            };
            let points = points.map(|(x, y)| ocs_point_to_world_xy(x, y, pre.elevation, extrusion));
            entities_out.push(Entity::Solid {
                points,
                layer: layer.into(),
                color: pre.color_aci.map(aci_to_hex),
            });
            DecodeOutcome::Ok
        }
        9 => {
            // TRACE: same geometry as SOLID, alternate field order.
            let Some(pre) = read_common_prefix_late(cur, mode, true) else {
                return DecodeOutcome::Malformed;
            };
            let Some(points) = read_four_points_2d(cur) else {
                return DecodeOutcome::Malformed;
            };
            entities_out.push(Entity::Solid {
                points,
                layer: layer.into(),
                color: pre.color_aci.map(aci_to_hex),
            });
            DecodeOutcome::Ok
        }
        7 => {
            // TEXT: insert(x,y), height, length-prefixed string, [angle].
            let Some(pre) = read_common_prefix(cur, mode, true) else {
                return DecodeOutcome::Malformed;
            };
            let (Some(x), Some(y), Some(height)) = (cur.f64(), cur.f64(), cur.f64()) else {
                return DecodeOutcome::Malformed;
            };
            let Some(len) = cur.u16() else {
                return DecodeOutcome::Malformed;
            };
            let Some(raw) = cur.bytes(len as usize) else {
                return DecodeOutcome::Malformed;
            };
            let text = decode_dwg_text(raw);
            let rotation_deg = if bit(pre.common_hi, 8) {
                cur.f64().map(f64::to_degrees).unwrap_or(0.0)
            } else {
                0.0
            };
            if !text.is_empty() {
                entities_out.push(Entity::Text {
                    text,
                    insert: (x, y),
                    height,
                    rotation_deg,
                    layer: layer.into(),
                    color: pre.color_aci.map(aci_to_hex),
                    h_align: 0,
                    v_align: 0,
                });
            }
            DecodeOutcome::Ok
        }
        15 => {
            // ATTDEF: attribute definition. Drawn like TEXT using its
            // default value (ATTDEF/1); the prompt and tag are metadata,
            // not drawn. Rotation is gated by flag2_7 here, not flag2_8
            // as in TEXT.
            let Some(pre) = read_common_prefix(cur, mode, true) else {
                return DecodeOutcome::Malformed;
            };
            let (Some(x), Some(y), Some(height)) = (cur.f64(), cur.f64(), cur.f64()) else {
                return DecodeOutcome::Malformed;
            };
            let Some(default_len) = cur.u16() else {
                return DecodeOutcome::Malformed;
            };
            let Some(raw) = cur.bytes(default_len as usize) else {
                return DecodeOutcome::Malformed;
            };
            let text = decode_dwg_text(raw);
            if skip_len_prefixed_string(cur).is_none() // prompt
                || skip_len_prefixed_string(cur).is_none()
            // tag
            {
                return DecodeOutcome::Malformed;
            }
            let Some(flags) = cur.u8() else {
                return DecodeOutcome::Malformed;
            };
            if bit(flags, 6) {
                // "invisible" (attdef_flags' 6th declared bit): a real
                // viewer would never show this, so neither do we.
                return DecodeOutcome::Ok;
            }
            let rotation_deg = if bit(pre.common_hi, 7) {
                cur.f64().map(f64::to_degrees).unwrap_or(0.0)
            } else {
                0.0
            };
            if !text.is_empty() {
                entities_out.push(Entity::Text {
                    text,
                    insert: (x, y),
                    height,
                    rotation_deg,
                    layer: layer.into(),
                    color: pre.color_aci.map(aci_to_hex),
                    h_align: 0,
                    v_align: 0,
                });
            }
            DecodeOutcome::Ok
        }
        16 => {
            // ATTRIB: attribute instance attached to an INSERT, drawn with
            // its own displayed value (ATTRIB/1) at its own already-final
            // position — not relative to the owning INSERT's transform.
            let Some(pre) = read_common_prefix(cur, mode, true) else {
                return DecodeOutcome::Malformed;
            };
            let (Some(x), Some(y), Some(height)) = (cur.f64(), cur.f64(), cur.f64()) else {
                return DecodeOutcome::Malformed;
            };
            let Some(value_len) = cur.u16() else {
                return DecodeOutcome::Malformed;
            };
            let Some(raw) = cur.bytes(value_len as usize) else {
                return DecodeOutcome::Malformed;
            };
            let text = decode_dwg_text(raw);
            if skip_len_prefixed_string(cur).is_none() {
                return DecodeOutcome::Malformed; // tag
            }
            let Some(flags) = cur.u8() else {
                return DecodeOutcome::Malformed;
            };
            if bit(flags, 8) {
                // "invisible" (attr_flags' last declared bit, the LSB).
                return DecodeOutcome::Ok;
            }
            let rotation_deg = if bit(pre.common_hi, 7) {
                cur.f64().map(f64::to_degrees).unwrap_or(0.0)
            } else {
                0.0
            };
            if !text.is_empty() {
                entities_out.push(Entity::Text {
                    text,
                    insert: (x, y),
                    height,
                    rotation_deg,
                    layer: layer.into(),
                    color: pre.color_aci.map(aci_to_hex),
                    h_align: 0,
                    v_align: 0,
                });
            }
            DecodeOutcome::Ok
        }
        19 => {
            // POLYLINE: opens a vertex accumulator closed by SEQEND. Its
            // shared elevation/extrusion (note: extrusion is gated by
            // flag2_5 here, not flag2_8 as in most other entity types)
            // apply to every VERTEX in the group, since VERTEX carries
            // neither itself.
            let Some(pre) = read_common_prefix(cur, mode, true) else {
                return DecodeOutcome::Malformed;
            };
            let closed = if bit(pre.common_hi, 8) {
                match cur.u8() {
                    Some(flags) => bit(flags, 8),
                    None => return DecodeOutcome::Malformed,
                }
            } else {
                false
            };
            if bit(pre.common_hi, 7) && cur.f64().is_none() {
                return DecodeOutcome::Malformed; // start_width
            }
            if bit(pre.common_hi, 6) && cur.f64().is_none() {
                return DecodeOutcome::Malformed; // end_width
            }
            let extrusion = if bit(pre.common_hi, 5) {
                match read_extrusion(cur) {
                    Some(e) => Some(e),
                    None => return DecodeOutcome::Malformed,
                }
            } else {
                None
            };
            if let Some(previous) = open_polyline.take()
                && !previous.vertices.is_empty()
            {
                entities_out.push(Entity::LwPolyline {
                    vertices: previous.vertices,
                    is_closed: previous.closed,
                    layer: previous.layer,
                    color: previous.color,
                    line_weight: None,
                    linetype: None,
                });
            }
            *open_polyline = Some(OpenPolyline {
                layer: layer.into(),
                color: pre.color_aci.map(aci_to_hex),
                closed,
                vertices: Vec::new(),
                elevation: pre.elevation,
                extrusion,
            });
            DecodeOutcome::Ok
        }
        20 => {
            // VERTEX: belongs to the currently open POLYLINE, if any; its
            // (x, y) is placed through the POLYLINE's own elevation and
            // extrusion (VERTEX carries neither itself). A tilted OCS
            // still leaves bulge (circular-arc) segments drawn as circular
            // arcs between the transformed endpoints rather than the
            // foreshortened elliptical arcs a true projection would need
            // — see the module docs.
            let Some(pre) = read_common_prefix_late(cur, mode, true) else {
                return DecodeOutcome::Malformed;
            };
            let has_xy = !bit(pre.common_lo, 2);
            let xy = if has_xy {
                match (cur.f64(), cur.f64()) {
                    (Some(x), Some(y)) => Some((x, y)),
                    _ => return DecodeOutcome::Malformed,
                }
            } else {
                None
            };
            if bit(pre.common_hi, 8) && cur.f64().is_none() {
                return DecodeOutcome::Malformed; // start_width
            }
            if bit(pre.common_hi, 7) && cur.f64().is_none() {
                return DecodeOutcome::Malformed; // end_width
            }
            let bulge = if bit(pre.common_hi, 6) {
                match cur.f64() {
                    Some(b) => b,
                    None => return DecodeOutcome::Malformed,
                }
            } else {
                0.0
            };
            if let (Some((x, y)), Some(accum)) = (xy, open_polyline.as_mut()) {
                let (wx, wy) = ocs_point_to_world_xy(x, y, accum.elevation, accum.extrusion);
                accum.vertices.push(LwVertex {
                    x: wx,
                    y: wy,
                    bulge,
                });
            }
            DecodeOutcome::Ok
        }
        17 => {
            // SEQEND: finalize the open POLYLINE, if any. Its own body
            // (an address field) is never needed.
            if let Some(open) = open_polyline.take()
                && !open.vertices.is_empty()
            {
                entities_out.push(Entity::LwPolyline {
                    vertices: open.vertices,
                    is_closed: open.closed,
                    layer: open.layer,
                    color: open.color,
                    line_weight: None,
                    linetype: None,
                });
            }
            DecodeOutcome::Ok
        }
        22 => {
            // 3DFACE: four points, Z per-point (like LINE/POINT).
            let Some(pre) = read_common_prefix(cur, mode, false) else {
                return DecodeOutcome::Malformed;
            };
            let has_elevation = bit(mode, 6);
            let mut xs = [0.0f64; 4];
            let mut ys = [0.0f64; 4];
            for i in 0..4 {
                let (Some(x), Some(y)) = (cur.f64(), cur.f64()) else {
                    return DecodeOutcome::Malformed;
                };
                if !has_elevation && cur.f64().is_none() {
                    return DecodeOutcome::Malformed;
                }
                xs[i] = x;
                ys[i] = y;
            }
            entities_out.push(Entity::Solid {
                points: [
                    (xs[0], ys[0]),
                    (xs[1], ys[1]),
                    (xs[2], ys[2]),
                    (xs[3], ys[3]),
                ],
                layer: layer.into(),
                color: pre.color_aci.map(aci_to_hex),
            });
            DecodeOutcome::Ok
        }
        14 => {
            // INSERT: block reference. `block_index` is a 0-based index
            // into the BLOCK table, resolved to a name here so the shared
            // DXF renderer can look it up in `DxfDocument::blocks`.
            let Some(pre) = read_insert_prefix(cur, mode) else {
                return DecodeOutcome::Malformed;
            };
            let Some(block_index) = cur.i16() else {
                return DecodeOutcome::Malformed;
            };
            let (Some(x), Some(y)) = (cur.f64(), cur.f64()) else {
                return DecodeOutcome::Malformed;
            };
            let flags = pre.insert_flags;
            let x_scale = if bit(flags, 8) {
                match cur.f64() {
                    Some(v) => v,
                    None => return DecodeOutcome::Malformed,
                }
            } else {
                1.0
            };
            let y_scale = if bit(flags, 7) {
                match cur.f64() {
                    Some(v) => v,
                    None => return DecodeOutcome::Malformed,
                }
            } else {
                1.0
            };
            let rotation_deg = if bit(flags, 6) {
                match cur.f64() {
                    Some(v) => v.to_degrees(),
                    None => return DecodeOutcome::Malformed,
                }
            } else {
                0.0
            };
            match block_name_for_index(block_names, block_index) {
                Some(block_name) => {
                    entities_out.push(Entity::Insert {
                        block_name,
                        insert: (x, y),
                        scale: (x_scale, y_scale),
                        rotation_deg,
                        layer: layer.into(),
                        color: pre.color_aci.map(aci_to_hex),
                        line_weight: None,
                    });
                    DecodeOutcome::Insert
                }
                None => DecodeOutcome::InsertUnresolved,
            }
        }
        23 => {
            // DIMENSION: AutoCAD always generates and caches a fully
            // world-placed anonymous block (extension lines, dimension
            // line, arrowheads, text) for every dimension, referenced here
            // by the same kind of `block_index` INSERT uses. Unlike a
            // normal INSERT, that block's own entity coordinates already
            // are the final placement, so it's drawn at the identity
            // transform rather than computing dimension geometry here.
            let Some(pre) = read_common_prefix(cur, mode, true) else {
                return DecodeOutcome::Malformed;
            };
            let Some(block_index) = cur.i16() else {
                return DecodeOutcome::Malformed;
            };
            match block_name_for_index(block_names, block_index) {
                Some(block_name) => {
                    entities_out.push(Entity::Insert {
                        block_name,
                        insert: (0.0, 0.0),
                        scale: (1.0, 1.0),
                        rotation_deg: 0.0,
                        layer: layer.into(),
                        color: pre.color_aci.map(aci_to_hex),
                        line_weight: None,
                    });
                    DecodeOutcome::Insert
                }
                None => DecodeOutcome::DimensionUnresolved,
            }
        }
        24 => {
            // VIEWPORT: a paper-space viewport border. `width`/`height`
            // are unconditional (no per-field presence flags), so this is
            // fully real, complete geometry — drawn as its rectangle.
            let Some(pre) = read_common_prefix(cur, mode, true) else {
                return DecodeOutcome::Malformed;
            };
            let (Some(x), Some(y), Some(_z), Some(width), Some(height)) =
                (cur.f64(), cur.f64(), cur.f64(), cur.f64(), cur.f64())
            else {
                return DecodeOutcome::Malformed;
            };
            if width.is_finite() && height.is_finite() && width > 0.0 && height > 0.0 {
                let hw = width / 2.0;
                let hh = height / 2.0;
                entities_out.push(Entity::LwPolyline {
                    vertices: vec![
                        LwVertex {
                            x: x - hw,
                            y: y - hh,
                            bulge: 0.0,
                        },
                        LwVertex {
                            x: x + hw,
                            y: y - hh,
                            bulge: 0.0,
                        },
                        LwVertex {
                            x: x + hw,
                            y: y + hh,
                            bulge: 0.0,
                        },
                        LwVertex {
                            x: x - hw,
                            y: y + hh,
                            bulge: 0.0,
                        },
                    ],
                    is_closed: true,
                    layer: layer.into(),
                    color: pre.color_aci.map(aci_to_hex),
                    line_weight: None,
                    linetype: None,
                });
            }
            DecodeOutcome::Ok
        }
        4 => {
            // SHAPE: references a glyph by numeric index into an external,
            // un-embedded .SHX compiled shape file — the actual outline
            // isn't part of the DWG and can't be reconstructed here.
            // Counted and reported distinctly rather than folded into the
            // generic "unrecognized entity" bucket, so its presence is
            // never silently invisible.
            DecodeOutcome::ShapeSkipped
        }
        // Any other unrecognized entity type: safely skipped via its own
        // `entity_size`.
        _ => DecodeOutcome::Skipped,
    }
}

fn read_four_points_2d(cur: &mut Cursor) -> Option<[(f64, f64); 4]> {
    let mut pts = [(0.0f64, 0.0f64); 4];
    for pt in &mut pts {
        *pt = (cur.f64()?, cur.f64()?);
    }
    Some(pts)
}

/// Skips one `u16`-length-prefixed string field without decoding it, used
/// to step over ATTDEF/ATTRIB's prompt/tag fields on the way to the
/// optional fields (rotation, etc.) that follow them.
fn skip_len_prefixed_string(cur: &mut Cursor) -> Option<()> {
    let len = cur.u16()? as usize;
    cur.skip(len)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::Node;

    struct DummySink(Vec<Page>);
    impl PageConsumer for DummySink {
        fn consume(&mut self, page: Page) -> Result<()> {
            self.0.push(page);
            Ok(())
        }
    }

    fn le16(v: i16) -> [u8; 2] {
        v.to_le_bytes()
    }
    fn le32(v: i32) -> [u8; 4] {
        v.to_le_bytes()
    }
    fn le64(v: f64) -> [u8; 8] {
        v.to_le_bytes()
    }

    /// Builds a minimal, well-formed AC1009 byte stream: fixed-offset
    /// header fields, an entities section (LINE + CIRCLE + open/close
    /// POLYLINE with two VERTEX records), and a one-row LAYER table.
    fn build_sample_dwg() -> Vec<u8> {
        // --- entities section content ---
        let mut ent = Vec::new();
        // LINE: mode byte with has_color set (bit8) and has_elevation set
        // (bit6, so Z is omitted): 1000_0100 = 0x84.
        {
            let mut rec = Vec::new();
            rec.push(1i8 as u8); // entity_type = LINE
            let mode: u8 = 0b1000_0100; // has_color, has_elevation
            let layer_index: i16 = 0;
            let mut body = Vec::new();
            body.extend_from_slice(&le16(layer_index));
            body.extend_from_slice(&[0u8, 0u8]); // entity_common (no flags set)
            body.push(3u8 as i8 as u8); // entity_color (ACI 3)
            body.extend_from_slice(&le64(1.0)); // x1
            body.extend_from_slice(&le64(2.0)); // y1
            body.extend_from_slice(&le64(11.0)); // x2
            body.extend_from_slice(&le64(12.0)); // y2
            let total_size = 4 + body.len(); // type+mode+size(2) + body
            rec.push(mode);
            rec.extend_from_slice(&le16(total_size as i16));
            rec.extend_from_slice(&body);
            ent.extend_from_slice(&rec);
        }
        // CIRCLE: mode with has_color: 1000_0000.
        {
            let mut rec = Vec::new();
            rec.push(3i8 as u8); // CIRCLE
            let mode: u8 = 0b1000_0000;
            let mut body = Vec::new();
            body.extend_from_slice(&le16(0)); // layer_index
            body.extend_from_slice(&[0u8, 0u8]); // entity_common
            body.push(5u8 as i8 as u8); // color ACI 5
            body.extend_from_slice(&le64(50.0)); // cx
            body.extend_from_slice(&le64(50.0)); // cy
            body.extend_from_slice(&le64(25.0)); // radius
            let total_size = 4 + body.len();
            rec.push(mode);
            rec.extend_from_slice(&le16(total_size as i16));
            rec.extend_from_slice(&body);
            ent.extend_from_slice(&rec);
        }
        // POLYLINE (closed): entity_common flag2_8 set (closed flag byte
        // present) -> common_hi = 0x01 (flag2_8 is the LSB of the byte).
        {
            let mut rec = Vec::new();
            rec.push(19i8 as u8); // POLYLINE
            let mode: u8 = 0;
            let mut body = Vec::new();
            body.extend_from_slice(&le16(0)); // layer_index
            body.push(0x01); // common_hi: flag2_8 set
            body.push(0x00); // common_lo
            body.push(0b1000_0001u8); // polyline_flags: closed (LSB) set
            let total_size = 4 + body.len();
            rec.push(mode);
            rec.extend_from_slice(&le16(total_size as i16));
            rec.extend_from_slice(&body);
            ent.extend_from_slice(&rec);
        }
        // VERTEX x2 (plain x/y, no width/bulge).
        for (vx, vy) in [(0.0, 0.0), (10.0, 0.0)] {
            let mut rec = Vec::new();
            rec.push(20i8 as u8); // VERTEX
            let mode: u8 = 0;
            let mut body = Vec::new();
            body.extend_from_slice(&le16(0)); // layer_index
            body.push(0x00); // common_hi
            body.push(0x00); // common_lo (flag3_2 clear -> xy present)
            body.extend_from_slice(&le64(vx));
            body.extend_from_slice(&le64(vy));
            let total_size = 4 + body.len();
            rec.push(mode);
            rec.extend_from_slice(&le16(total_size as i16));
            rec.extend_from_slice(&body);
            ent.extend_from_slice(&rec);
        }
        // SEQEND (minimal body: just an address, doesn't matter).
        {
            let mut rec = Vec::new();
            rec.push(17i8 as u8); // SEQEND
            let mode: u8 = 0;
            let mut body = Vec::new();
            body.extend_from_slice(&le16(0)); // layer_index
            body.push(0x00);
            body.push(0x00);
            body.extend_from_slice(&le32(0)); // begin_addr
            let total_size = 4 + body.len();
            rec.push(mode);
            rec.extend_from_slice(&le16(total_size as i16));
            rec.extend_from_slice(&body);
            ent.extend_from_slice(&rec);
        }

        let mut entities_section = Vec::new();
        entities_section.extend_from_slice(&ENTITIES_SENTINEL_BEGIN);
        entities_section.extend_from_slice(&ent);
        entities_section.extend_from_slice(&ENTITIES_SENTINEL_END);

        // --- layer table (one row) ---
        let mut layer_rec = Vec::new();
        layer_rec.push(0u8); // flag: not frozen
        let mut name = vec![0u8; 32];
        name[..1].copy_from_slice(b"0");
        layer_rec.extend_from_slice(&name);
        layer_rec.extend_from_slice(&le16(0)); // used
        layer_rec.extend_from_slice(&le16(7)); // color ACI 7
        layer_rec.extend_from_slice(&le16(0)); // linetype_index
        layer_rec.extend_from_slice(&le16(0)); // crc16
        assert_eq!(layer_rec.len(), LAYER_RECORD_MIN_BYTES);

        let mut layer_section = Vec::new();
        layer_section.extend_from_slice(&LAYER_SENTINEL_BEGIN);
        layer_section.extend_from_slice(&layer_rec);
        layer_section.extend_from_slice(&LAYER_SENTINEL_END);

        // --- assemble whole file ---
        let mut file = vec![0u8; 94];
        file[..6].copy_from_slice(b"AC1009");

        let entities_start: i32 = 94;
        let entities_content_start = entities_start as usize + 16;
        let entities_content_end = entities_content_start + ent.len();
        file[20..24].copy_from_slice(&le32(entities_content_start as i32));
        file[24..28].copy_from_slice(&le32(entities_content_end as i32));

        file.extend_from_slice(&entities_section);

        let layer_begin = file.len() as u32 + 16;
        file[54..56].copy_from_slice(&le16(LAYER_RECORD_MIN_BYTES as i16));
        file[56..58].copy_from_slice(&le16(1));
        file[60..64].copy_from_slice(&layer_begin.to_le_bytes());

        file.extend_from_slice(&layer_section);
        file
    }

    #[test]
    fn decodes_line_circle_and_closed_polyline_from_a_synthetic_ac1009_file() {
        let bytes = build_sample_dwg();
        let mut sink = DummySink(Vec::new());
        let warnings = convert(&bytes, &mut sink)
            .expect("convert")
            .expect("recognized as AC1009");
        assert!(warnings.is_empty(), "unexpected warnings: {warnings:?}");
        assert_eq!(sink.0.len(), 1);
        let page = &sink.0[0];
        assert!(!page.nodes.is_empty());
    }

    #[test]
    fn rejects_non_ac1009_version_header() {
        let mut bytes = vec![0u8; 200];
        bytes[..6].copy_from_slice(b"AC1015");
        let mut sink = DummySink(Vec::new());
        let result = convert(&bytes, &mut sink).expect("no hard error");
        assert!(result.is_none());
        assert!(sink.0.is_empty());
    }

    #[test]
    fn rejects_truncated_header() {
        let bytes = vec![0u8; 10];
        let mut sink = DummySink(Vec::new());
        let result = convert(&bytes, &mut sink).expect("no hard error");
        assert!(result.is_none());
    }

    #[test]
    fn rejects_mismatched_entities_sentinel() {
        let mut bytes = build_sample_dwg();
        // Corrupt one byte of the entities sentinel_begin.
        bytes[94] ^= 0xFF;
        let mut sink = DummySink(Vec::new());
        let result = convert(&bytes, &mut sink).expect("no hard error");
        assert!(result.is_none());
    }

    #[test]
    fn survives_a_truncated_entity_record_without_panicking() {
        let mut bytes = build_sample_dwg();
        // Shrink the recorded entities_end so the section is cut mid-entity;
        // this must not panic even though the declared size no longer fits.
        let entities_end = i32::from_le_bytes(bytes[24..28].try_into().unwrap());
        let new_end = entities_end - 5;
        bytes[24..28].copy_from_slice(&new_end.to_le_bytes());
        // Sentinel_end must follow immediately after the new (shorter) end.
        let sentinel_end_pos = new_end as usize;
        let tail: Vec<u8> = bytes[(entities_end as usize)..(entities_end as usize + 16)].to_vec();
        bytes.splice(sentinel_end_pos..sentinel_end_pos, tail);
        let mut sink = DummySink(Vec::new());
        // Either a clean recognition-failure or a successful bounded parse
        // is acceptable; a panic is not.
        let _ = convert(&bytes, &mut sink);
    }

    #[test]
    fn hides_solid_entities_on_a_frozen_layer() {
        // Layer 0: visible SOLID quad. Layer 1 ("HIDDEN"): frozen, a TRACE
        // and a second SOLID that must not reach the rendered page.
        let mut ent = Vec::new();
        {
            let mut rec = vec![11u8]; // SOLID
            let mode: u8 = 0;
            let mut body = Vec::new();
            body.extend_from_slice(&le16(0)); // layer 0
            body.extend_from_slice(&[0, 0]);
            for (x, y) in [(0.0, 0.0), (10.0, 0.0), (10.0, 10.0), (0.0, 10.0)] {
                body.extend_from_slice(&le64(x));
                body.extend_from_slice(&le64(y));
            }
            rec.push(mode);
            rec.extend_from_slice(&le16((4 + body.len()) as i16));
            rec.extend_from_slice(&body);
            ent.extend_from_slice(&rec);
        }
        {
            let mut rec = vec![9u8]; // TRACE
            let mode: u8 = 0;
            let mut body = Vec::new();
            body.extend_from_slice(&le16(1)); // layer 1 (frozen)
            body.extend_from_slice(&[0, 0]);
            for (x, y) in [(20.0, 0.0), (30.0, 0.0), (30.0, 10.0), (20.0, 10.0)] {
                body.extend_from_slice(&le64(x));
                body.extend_from_slice(&le64(y));
            }
            rec.push(mode);
            rec.extend_from_slice(&le16((4 + body.len()) as i16));
            rec.extend_from_slice(&body);
            ent.extend_from_slice(&rec);
        }

        let mut entities_section = Vec::new();
        entities_section.extend_from_slice(&ENTITIES_SENTINEL_BEGIN);
        entities_section.extend_from_slice(&ent);
        entities_section.extend_from_slice(&ENTITIES_SENTINEL_END);

        let mut layer_section = Vec::new();
        layer_section.extend_from_slice(&LAYER_SENTINEL_BEGIN);
        for (name, color, frozen) in [("0", 7i16, false), ("HIDDEN", 3i16, true)] {
            let mut rec = Vec::new();
            rec.push(if frozen { 0x01 } else { 0x00 });
            let mut name_bytes = vec![0u8; 32];
            name_bytes[..name.len()].copy_from_slice(name.as_bytes());
            rec.extend_from_slice(&name_bytes);
            rec.extend_from_slice(&le16(0)); // used
            rec.extend_from_slice(&le16(color));
            rec.extend_from_slice(&le16(0)); // linetype_index
            rec.extend_from_slice(&le16(0)); // crc16
            assert_eq!(rec.len(), LAYER_RECORD_MIN_BYTES);
            layer_section.extend_from_slice(&rec);
        }
        layer_section.extend_from_slice(&LAYER_SENTINEL_END);

        let mut file = vec![0u8; 94];
        file[..6].copy_from_slice(b"AC1009");
        let entities_content_start = 94 + 16;
        let entities_content_end = entities_content_start + ent.len();
        file[20..24].copy_from_slice(&le32(entities_content_start as i32));
        file[24..28].copy_from_slice(&le32(entities_content_end as i32));
        file.extend_from_slice(&entities_section);

        let layer_begin = file.len() as u32 + 16;
        file[54..56].copy_from_slice(&le16(LAYER_RECORD_MIN_BYTES as i16));
        file[56..58].copy_from_slice(&le16(2));
        file[60..64].copy_from_slice(&layer_begin.to_le_bytes());
        file.extend_from_slice(&layer_section);

        let mut sink = DummySink(Vec::new());
        convert(&file, &mut sink)
            .expect("convert")
            .expect("recognized as AC1009");
        assert_eq!(sink.0.len(), 1);
        let page = &sink.0[0];
        // Only the layer-0 SOLID should have been rendered; the frozen
        // layer's TRACE must not appear anywhere in the node tree.
        assert_eq!(count_paths(&page.nodes), 1);
    }

    #[test]
    fn expands_insert_block_reference_to_match_equivalent_direct_geometry() {
        // Baseline: a LINE drawn directly at the position/scale an INSERT
        // of a one-line block would produce, with no blocks involved.
        let baseline_line = {
            let mut body = Vec::new();
            body.extend_from_slice(&le16(0)); // layer_index
            body.extend_from_slice(&[0, 0]); // entity_common
            body.extend_from_slice(&le64(100.0));
            body.extend_from_slice(&le64(200.0));
            body.extend_from_slice(&le64(120.0));
            body.extend_from_slice(&le64(200.0));
            let mode: u8 = 0b0000_0100; // has_elevation -> Z omitted
            let mut rec = vec![1u8, mode];
            rec.extend_from_slice(&le16((4 + body.len()) as i16));
            rec.extend_from_slice(&body);
            rec
        };
        let baseline_svg = render_main_entities_only(&baseline_line);

        // INSERT-based: a "SQUARE" block containing one LINE from (0,0) to
        // (10,0), inserted at (100,200) with a 2x/2x scale — geometrically
        // equivalent to the baseline LINE above.
        let block_begin = {
            let mut body = Vec::new();
            body.extend_from_slice(&le16(0)); // layer_index
            body.extend_from_slice(&[0x04, 0x00]); // entity_common: flag2_6 (name)
            body.extend_from_slice(&le64(0.0)); // base_point x
            body.extend_from_slice(&le64(0.0)); // base_point y
            let name = b"SQUARE";
            body.extend_from_slice(&le16(name.len() as i16));
            body.extend_from_slice(name);
            let mut rec = vec![12u8, 0u8]; // BLOCK_BEGIN, mode=0
            rec.extend_from_slice(&le16((4 + body.len()) as i16));
            rec.extend_from_slice(&body);
            rec
        };
        let block_line = {
            let mut body = Vec::new();
            body.extend_from_slice(&le16(0)); // layer_index
            body.extend_from_slice(&[0, 0]);
            body.extend_from_slice(&le64(0.0));
            body.extend_from_slice(&le64(0.0));
            body.extend_from_slice(&le64(10.0));
            body.extend_from_slice(&le64(0.0));
            let mode: u8 = 0b0000_0100; // has_elevation -> Z omitted
            let mut rec = vec![1u8, mode];
            rec.extend_from_slice(&le16((4 + body.len()) as i16));
            rec.extend_from_slice(&body);
            rec
        };
        let block_end = {
            let body = le16(0); // layer_index only
            let mut rec = vec![13u8, 0u8];
            rec.extend_from_slice(&le16((4 + body.len()) as i16));
            rec.extend_from_slice(&body);
            rec
        };
        let mut block_ent = Vec::new();
        block_ent.extend_from_slice(&block_begin);
        block_ent.extend_from_slice(&block_line);
        block_ent.extend_from_slice(&block_end);

        let insert_rec = {
            let mut body = Vec::new();
            body.extend_from_slice(&le16(0)); // layer_index
            body.push(0b0000_0011); // insert_flags: has_x_scale, has_y_scale
            body.extend_from_slice(&le16(0)); // block_index 0 -> "SQUARE"
            body.extend_from_slice(&le64(100.0)); // x
            body.extend_from_slice(&le64(200.0)); // y
            body.extend_from_slice(&le64(2.0)); // x_scale
            body.extend_from_slice(&le64(2.0)); // y_scale
            let mut rec = vec![14u8, 0u8]; // INSERT, mode=0
            rec.extend_from_slice(&le16((4 + body.len()) as i16));
            rec.extend_from_slice(&body);
            rec
        };

        let mut file = vec![0u8; 94];
        file[..6].copy_from_slice(b"AC1009");

        let entities_content_start = 94 + 16;
        let entities_content_end = entities_content_start + insert_rec.len();
        file[20..24].copy_from_slice(&le32(entities_content_start as i32));
        file[24..28].copy_from_slice(&le32(entities_content_end as i32));
        file.extend_from_slice(&ENTITIES_SENTINEL_BEGIN);
        file.extend_from_slice(&insert_rec);
        file.extend_from_slice(&ENTITIES_SENTINEL_END);

        let block_entities_start = file.len() as i32 + 16;
        file[28..32].copy_from_slice(&le32(block_entities_start));
        file[32..36].copy_from_slice(&(block_ent.len() as u32).to_le_bytes());
        file.extend_from_slice(&BLOCK_ENTITIES_SENTINEL_BEGIN);
        file.extend_from_slice(&block_ent);
        file.extend_from_slice(&BLOCK_ENTITIES_SENTINEL_END);

        let table_block_begin = file.len() as u32 + 16;
        file[44..46].copy_from_slice(&le16(BLOCK_RECORD_MIN_BYTES as i16));
        file[46..48].copy_from_slice(&le16(1));
        file[50..54].copy_from_slice(&table_block_begin.to_le_bytes());
        {
            let mut rec = vec![0u8]; // flag
            let mut name_bytes = vec![0u8; 32];
            name_bytes[..6].copy_from_slice(b"SQUARE");
            rec.extend_from_slice(&name_bytes);
            rec.extend_from_slice(&[0, 0]); // used
            rec.extend_from_slice(&[0, 0, 0, 0]); // begin_address_in_block_table_raw
            rec.extend_from_slice(&[0, 0]); // block_entity
            rec.push(0); // flag2
            rec.push(0); // u1
            rec.extend_from_slice(&[0, 0]); // crc16
            assert_eq!(rec.len(), BLOCK_RECORD_MIN_BYTES);
            file.extend_from_slice(&BLOCK_TABLE_SENTINEL_BEGIN);
            file.extend_from_slice(&rec);
            file.extend_from_slice(&BLOCK_TABLE_SENTINEL_END);
        }

        let layer_begin = file.len() as u32 + 16;
        file[54..56].copy_from_slice(&le16(LAYER_RECORD_MIN_BYTES as i16));
        file[56..58].copy_from_slice(&le16(1));
        file[60..64].copy_from_slice(&layer_begin.to_le_bytes());
        {
            let mut rec = vec![0u8];
            let mut name_bytes = vec![0u8; 32];
            name_bytes[..1].copy_from_slice(b"0");
            rec.extend_from_slice(&name_bytes);
            rec.extend_from_slice(&le16(0));
            rec.extend_from_slice(&le16(7));
            rec.extend_from_slice(&le16(0));
            rec.extend_from_slice(&le16(0));
            assert_eq!(rec.len(), LAYER_RECORD_MIN_BYTES);
            file.extend_from_slice(&LAYER_SENTINEL_BEGIN);
            file.extend_from_slice(&rec);
            file.extend_from_slice(&LAYER_SENTINEL_END);
        }

        let mut sink = DummySink(Vec::new());
        let warnings = convert(&file, &mut sink)
            .expect("convert")
            .expect("recognized as AC1009");
        assert!(warnings.is_empty(), "unexpected warnings: {warnings:?}");
        assert_eq!(sink.0.len(), 1);
        let insert_svg = svg_of(&sink.0[0]);

        assert_eq!(
            path_data(&insert_svg),
            path_data(&baseline_svg),
            "INSERT expansion should draw the same geometry as the equivalent direct LINE"
        );
    }

    /// Renders a minimal AC1009 file containing only the given pre-built
    /// entity record(s) in the main `entities` section (no blocks, one
    /// default layer), and returns the resulting page's serialized SVG.
    fn render_main_entities_only(entity_records: &[u8]) -> String {
        let mut file = vec![0u8; 94];
        file[..6].copy_from_slice(b"AC1009");
        let entities_content_start = 94 + 16;
        let entities_content_end = entities_content_start + entity_records.len();
        file[20..24].copy_from_slice(&le32(entities_content_start as i32));
        file[24..28].copy_from_slice(&le32(entities_content_end as i32));
        file.extend_from_slice(&ENTITIES_SENTINEL_BEGIN);
        file.extend_from_slice(entity_records);
        file.extend_from_slice(&ENTITIES_SENTINEL_END);

        let layer_begin = file.len() as u32 + 16;
        file[54..56].copy_from_slice(&le16(LAYER_RECORD_MIN_BYTES as i16));
        file[56..58].copy_from_slice(&le16(1));
        file[60..64].copy_from_slice(&layer_begin.to_le_bytes());
        let mut rec = vec![0u8];
        let mut name_bytes = vec![0u8; 32];
        name_bytes[..1].copy_from_slice(b"0");
        rec.extend_from_slice(&name_bytes);
        rec.extend_from_slice(&le16(0));
        rec.extend_from_slice(&le16(7));
        rec.extend_from_slice(&le16(0));
        rec.extend_from_slice(&le16(0));
        file.extend_from_slice(&LAYER_SENTINEL_BEGIN);
        file.extend_from_slice(&rec);
        file.extend_from_slice(&LAYER_SENTINEL_END);

        let mut sink = DummySink(Vec::new());
        convert(&file, &mut sink)
            .expect("convert")
            .expect("recognized as AC1009");
        svg_of(&sink.0[0])
    }

    fn svg_of(page: &Page) -> String {
        let mut bytes = Vec::new();
        crate::svg::write_page(page, &mut bytes, crate::svg::SvgOptions::default()).unwrap();
        String::from_utf8(bytes).unwrap()
    }

    /// Extracts every `d="..."` path attribute value from an SVG string,
    /// ignoring ids and other attributes so two structurally-equivalent
    /// renders compare equal regardless of node ordering/naming. Matches on
    /// `" d="` (with the leading space) rather than bare `d="`, since every
    /// element's `id="..."` attribute would otherwise also match (`id="`
    /// ends in `d="`).
    fn path_data(svg: &str) -> Vec<&str> {
        let mut out = Vec::new();
        let mut rest = svg;
        while let Some(start) = rest.find(" d=\"") {
            let after = &rest[start + 4..];
            let Some(end) = after.find('"') else { break };
            out.push(&after[..end]);
            rest = &after[end + 1..];
        }
        out
    }

    fn count_paths(nodes: &[Node]) -> usize {
        nodes
            .iter()
            .map(|n| match n {
                Node::Path { .. } => 1,
                Node::Group { nodes, .. } => count_paths(nodes),
                _ => 0,
            })
            .sum()
    }

    #[test]
    fn draws_visible_attdef_and_attrib_text_and_hides_invisible_ones() {
        fn attdef(default: &str, prompt: &str, tag: &str, invisible: bool) -> Vec<u8> {
            let mut body = Vec::new();
            body.extend_from_slice(&le16(0)); // layer_index
            body.extend_from_slice(&[0, 0]); // entity_common (no rotation)
            body.extend_from_slice(&le64(10.0));
            body.extend_from_slice(&le64(20.0));
            body.extend_from_slice(&le64(5.0)); // height
            body.extend_from_slice(&le16(default.len() as i16));
            body.extend_from_slice(default.as_bytes());
            body.extend_from_slice(&le16(prompt.len() as i16));
            body.extend_from_slice(prompt.as_bytes());
            body.extend_from_slice(&le16(tag.len() as i16));
            body.extend_from_slice(tag.as_bytes());
            body.push(if invisible { 0x04 } else { 0x00 }); // attdef_flags
            let mut rec = vec![15u8, 0u8];
            rec.extend_from_slice(&le16((4 + body.len()) as i16));
            rec.extend_from_slice(&body);
            rec
        }
        fn attrib(value: &str, tag: &str, invisible: bool) -> Vec<u8> {
            let mut body = Vec::new();
            body.extend_from_slice(&le16(0)); // layer_index
            body.extend_from_slice(&[0, 0]); // entity_common (no rotation)
            body.extend_from_slice(&le64(30.0));
            body.extend_from_slice(&le64(40.0));
            body.extend_from_slice(&le64(5.0)); // height
            body.extend_from_slice(&le16(value.len() as i16));
            body.extend_from_slice(value.as_bytes());
            body.extend_from_slice(&le16(tag.len() as i16));
            body.extend_from_slice(tag.as_bytes());
            body.push(if invisible { 0x01 } else { 0x00 }); // attr_flags
            let mut rec = vec![16u8, 0u8];
            rec.extend_from_slice(&le16((4 + body.len()) as i16));
            rec.extend_from_slice(&body);
            rec
        }

        let mut ent = Vec::new();
        ent.extend_from_slice(&attdef("VAL1", "Enter value", "TAG1", false));
        ent.extend_from_slice(&attdef("HIDDEN_DEF", "prompt", "TAG2", true));
        ent.extend_from_slice(&attrib("ATTR1", "TAG3", false));
        ent.extend_from_slice(&attrib("HIDDEN_ATTR", "TAG4", true));

        let svg = render_main_entities_only(&ent);
        assert!(svg.contains("VAL1"), "{svg}");
        assert!(svg.contains("ATTR1"), "{svg}");
        assert!(!svg.contains("HIDDEN_DEF"), "{svg}");
        assert!(!svg.contains("HIDDEN_ATTR"), "{svg}");
    }

    #[test]
    fn expands_dimension_block_reference_at_identity_transform() {
        // Baseline: the dimension block's LINE drawn directly.
        let baseline_line = {
            let mut body = Vec::new();
            body.extend_from_slice(&le16(0));
            body.extend_from_slice(&[0, 0]);
            body.extend_from_slice(&le64(0.0));
            body.extend_from_slice(&le64(0.0));
            body.extend_from_slice(&le64(30.0));
            body.extend_from_slice(&le64(0.0));
            let mode: u8 = 0b0000_0100; // has_elevation -> Z omitted
            let mut rec = vec![1u8, mode];
            rec.extend_from_slice(&le16((4 + body.len()) as i16));
            rec.extend_from_slice(&body);
            rec
        };
        let baseline_svg = render_main_entities_only(&baseline_line);

        // DIMENSION-based: a "DIMBLK" block (AutoCAD's cached dimension
        // picture) containing the same LINE in already-final coordinates,
        // referenced by a DIMENSION entity.
        let block_begin = {
            let mut body = Vec::new();
            body.extend_from_slice(&le16(0));
            body.extend_from_slice(&[0x04, 0x00]); // flag2_6: name present
            body.extend_from_slice(&le64(0.0));
            body.extend_from_slice(&le64(0.0));
            let name = b"DIMBLK";
            body.extend_from_slice(&le16(name.len() as i16));
            body.extend_from_slice(name);
            let mut rec = vec![12u8, 0u8];
            rec.extend_from_slice(&le16((4 + body.len()) as i16));
            rec.extend_from_slice(&body);
            rec
        };
        let block_end = {
            let body = le16(0);
            let mut rec = vec![13u8, 0u8];
            rec.extend_from_slice(&le16((4 + body.len()) as i16));
            rec.extend_from_slice(&body);
            rec
        };
        let mut block_ent = Vec::new();
        block_ent.extend_from_slice(&block_begin);
        block_ent.extend_from_slice(&baseline_line);
        block_ent.extend_from_slice(&block_end);

        let dim_rec = {
            let mut body = Vec::new();
            body.extend_from_slice(&le16(0)); // layer_index
            body.extend_from_slice(&[0, 0]); // entity_common
            body.extend_from_slice(&le16(0)); // block_index -> "DIMBLK"
            let mut rec = vec![23u8, 0u8];
            rec.extend_from_slice(&le16((4 + body.len()) as i16));
            rec.extend_from_slice(&body);
            rec
        };

        let mut file = vec![0u8; 94];
        file[..6].copy_from_slice(b"AC1009");
        let entities_content_start = 94 + 16;
        let entities_content_end = entities_content_start + dim_rec.len();
        file[20..24].copy_from_slice(&le32(entities_content_start as i32));
        file[24..28].copy_from_slice(&le32(entities_content_end as i32));
        file.extend_from_slice(&ENTITIES_SENTINEL_BEGIN);
        file.extend_from_slice(&dim_rec);
        file.extend_from_slice(&ENTITIES_SENTINEL_END);

        let block_entities_start = file.len() as i32 + 16;
        file[28..32].copy_from_slice(&le32(block_entities_start));
        file[32..36].copy_from_slice(&(block_ent.len() as u32).to_le_bytes());
        file.extend_from_slice(&BLOCK_ENTITIES_SENTINEL_BEGIN);
        file.extend_from_slice(&block_ent);
        file.extend_from_slice(&BLOCK_ENTITIES_SENTINEL_END);

        let table_block_begin = file.len() as u32 + 16;
        file[44..46].copy_from_slice(&le16(BLOCK_RECORD_MIN_BYTES as i16));
        file[46..48].copy_from_slice(&le16(1));
        file[50..54].copy_from_slice(&table_block_begin.to_le_bytes());
        {
            let mut rec = vec![0u8];
            let mut name_bytes = vec![0u8; 32];
            name_bytes[..6].copy_from_slice(b"DIMBLK");
            rec.extend_from_slice(&name_bytes);
            rec.extend_from_slice(&[0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
            assert_eq!(rec.len(), BLOCK_RECORD_MIN_BYTES);
            file.extend_from_slice(&BLOCK_TABLE_SENTINEL_BEGIN);
            file.extend_from_slice(&rec);
            file.extend_from_slice(&BLOCK_TABLE_SENTINEL_END);
        }

        let layer_begin = file.len() as u32 + 16;
        file[54..56].copy_from_slice(&le16(LAYER_RECORD_MIN_BYTES as i16));
        file[56..58].copy_from_slice(&le16(1));
        file[60..64].copy_from_slice(&layer_begin.to_le_bytes());
        {
            let mut rec = vec![0u8];
            let mut name_bytes = vec![0u8; 32];
            name_bytes[..1].copy_from_slice(b"0");
            rec.extend_from_slice(&name_bytes);
            rec.extend_from_slice(&le16(0));
            rec.extend_from_slice(&le16(7));
            rec.extend_from_slice(&le16(0));
            rec.extend_from_slice(&le16(0));
            assert_eq!(rec.len(), LAYER_RECORD_MIN_BYTES);
            file.extend_from_slice(&LAYER_SENTINEL_BEGIN);
            file.extend_from_slice(&rec);
            file.extend_from_slice(&LAYER_SENTINEL_END);
        }

        let mut sink = DummySink(Vec::new());
        let warnings = convert(&file, &mut sink)
            .expect("convert")
            .expect("recognized as AC1009");
        assert!(warnings.is_empty(), "unexpected warnings: {warnings:?}");
        assert_eq!(sink.0.len(), 1);
        let dim_svg = svg_of(&sink.0[0]);

        assert_eq!(
            path_data(&dim_svg),
            path_data(&baseline_svg),
            "DIMENSION expansion should draw the same geometry as its cached block, at identity transform"
        );
    }

    #[test]
    fn draws_viewport_as_its_border_rectangle() {
        let rec = {
            let mut body = Vec::new();
            body.extend_from_slice(&le16(0)); // layer_index
            body.extend_from_slice(&[0, 0]); // entity_common
            body.extend_from_slice(&le64(50.0)); // x (center)
            body.extend_from_slice(&le64(100.0)); // y (center)
            body.extend_from_slice(&le64(0.0)); // z
            body.extend_from_slice(&le64(40.0)); // width
            body.extend_from_slice(&le64(20.0)); // height
            body.extend_from_slice(&le16(1)); // id
            let mut rec = vec![24u8, 0u8];
            rec.extend_from_slice(&le16((4 + body.len()) as i16));
            rec.extend_from_slice(&body);
            rec
        };

        let file = build_single_entity_file(&rec);
        let mut sink = DummySink(Vec::new());
        let warnings = convert(&file, &mut sink)
            .expect("convert")
            .expect("recognized as AC1009");
        assert!(warnings.is_empty(), "unexpected warnings: {warnings:?}");
        let page = &sink.0[0];
        assert_eq!(count_paths(&page.nodes), 1);
        let svg = svg_of(page);
        let d = path_data(&svg);
        assert_eq!(d.len(), 1);
        // A closed rectangle: 4 distinct corners plus the closing "Z".
        assert!(d[0].ends_with('Z'), "{}", d[0]);
        assert_eq!(d[0].matches('L').count(), 3);
    }

    #[test]
    fn counts_shape_entities_separately_and_draws_nothing_for_them() {
        let rec = {
            let mut body = Vec::new();
            body.extend_from_slice(&le16(0)); // layer_index
            body.extend_from_slice(&[0, 0]); // entity_common
            body.extend_from_slice(&le64(1.0)); // x
            body.extend_from_slice(&le64(2.0)); // y
            body.extend_from_slice(&le64(3.0)); // height
            body.push(7); // item_num
            let mut rec = vec![4u8, 0u8];
            rec.extend_from_slice(&le16((4 + body.len()) as i16));
            rec.extend_from_slice(&body);
            rec
        };

        let file = build_single_entity_file(&rec);
        let mut sink = DummySink(Vec::new());
        let warnings = convert(&file, &mut sink)
            .expect("convert")
            .expect("recognized as AC1009");
        assert_eq!(sink.0.len(), 1);
        assert_eq!(count_paths(&sink.0[0].nodes), 0);
        assert!(
            warnings
                .iter()
                .any(|w| w.contains("SHAPE") && w.contains(".SHX")),
            "expected a SHAPE-specific warning, got: {warnings:?}"
        );
    }

    /// Builds a minimal well-formed AC1009 file whose main `entities`
    /// section contains exactly one pre-built entity record, with a single
    /// default layer (mirrors [`render_main_entities_only`], but returns
    /// the raw bytes instead of rendering, for tests that need the
    /// conversion's returned warnings too).
    fn build_single_entity_file(entity_record: &[u8]) -> Vec<u8> {
        let mut file = vec![0u8; 94];
        file[..6].copy_from_slice(b"AC1009");
        let entities_content_start = 94 + 16;
        let entities_content_end = entities_content_start + entity_record.len();
        file[20..24].copy_from_slice(&le32(entities_content_start as i32));
        file[24..28].copy_from_slice(&le32(entities_content_end as i32));
        file.extend_from_slice(&ENTITIES_SENTINEL_BEGIN);
        file.extend_from_slice(entity_record);
        file.extend_from_slice(&ENTITIES_SENTINEL_END);

        let layer_begin = file.len() as u32 + 16;
        file[54..56].copy_from_slice(&le16(LAYER_RECORD_MIN_BYTES as i16));
        file[56..58].copy_from_slice(&le16(1));
        file[60..64].copy_from_slice(&layer_begin.to_le_bytes());
        let mut rec = vec![0u8];
        let mut name_bytes = vec![0u8; 32];
        name_bytes[..1].copy_from_slice(b"0");
        rec.extend_from_slice(&name_bytes);
        rec.extend_from_slice(&le16(0));
        rec.extend_from_slice(&le16(7));
        rec.extend_from_slice(&le16(0));
        rec.extend_from_slice(&le16(0));
        file.extend_from_slice(&LAYER_SENTINEL_BEGIN);
        file.extend_from_slice(&rec);
        file.extend_from_slice(&LAYER_SENTINEL_END);
        file
    }

    #[test]
    fn arbitrary_axis_reduces_to_identity_for_default_extrusion() {
        let (ax, ay, n) = arbitrary_axis((0.0, 0.0, 1.0));
        assert!((ax.0 - 1.0).abs() < 1e-9 && ax.1.abs() < 1e-9 && ax.2.abs() < 1e-9);
        assert!(ay.0.abs() < 1e-9 && (ay.1 - 1.0).abs() < 1e-9 && ay.2.abs() < 1e-9);
        assert!(n.0.abs() < 1e-9 && n.1.abs() < 1e-9 && (n.2 - 1.0).abs() < 1e-9);
    }

    #[test]
    fn ocs_point_to_world_xy_is_identity_for_default_or_absent_extrusion() {
        assert_eq!(ocs_point_to_world_xy(3.0, 4.0, 5.0, None), (3.0, 4.0));
        let (x, y) = ocs_point_to_world_xy(3.0, 4.0, 5.0, Some((0.0, 0.0, 1.0)));
        assert!((x - 3.0).abs() < 1e-9 && (y - 4.0).abs() < 1e-9);
    }

    #[test]
    fn project_ocs_circle_collapses_an_edge_on_view_to_a_degenerate_ellipse() {
        // Extrusion (1,0,0): the OCS plane is the WCS Y-Z plane, so a
        // circle viewed top-down (world Z dropped) flattens to a line
        // segment of length 2*radius: axis_ratio == 0.
        let (ax, ay, _n) = arbitrary_axis((1.0, 0.0, 0.0));
        let (major_axis, axis_ratio) = project_ocs_circle(10.0, ax, ay);
        assert!(major_axis.0.abs() < 1e-9);
        assert!((major_axis.1.abs() - 10.0).abs() < 1e-9);
        assert!(axis_ratio.abs() < 1e-9);
    }

    #[test]
    fn project_ocs_circle_stays_a_true_circle_when_only_flipped_about_z() {
        // Extrusion (0,0,-1): still parallel to WCS X/Y, just mirrored —
        // no foreshortening, so axis_ratio must stay exactly 1.
        let (ax, ay, _n) = arbitrary_axis((0.0, 0.0, -1.0));
        let (_major_axis, axis_ratio) = project_ocs_circle(7.0, ax, ay);
        assert!((axis_ratio - 1.0).abs() < 1e-9);
    }

    #[test]
    fn line_applies_ocs_extrusion_to_place_points_in_world_space() {
        // Extrusion (0,1,0): OCS X/Y/Z basis works out to ax=(-1,0,0),
        // ay=(0,0,1), n=(0,1,0) — so world (X,Y) = (-ocs_x, ocs_z),
        // dropping world Z (= ocs_y). Two points sharing ocs (x=3,y=0)
        // but differing only in ocs z (0 and 7) must land at the same
        // world X but a world Y that tracks ocs z.
        let mut body = Vec::new();
        body.extend_from_slice(&le16(0)); // layer_index
        body.extend_from_slice(&[0x01, 0x00]); // common_hi: flag2_8 (extrusion)
        body.extend_from_slice(&le64(3.0)); // x1
        body.extend_from_slice(&le64(0.0)); // y1
        body.extend_from_slice(&le64(0.0)); // z1
        body.extend_from_slice(&le64(3.0)); // x2
        body.extend_from_slice(&le64(0.0)); // y2
        body.extend_from_slice(&le64(7.0)); // z2
        body.extend_from_slice(&le64(0.0)); // extrusion.x
        body.extend_from_slice(&le64(1.0)); // extrusion.y
        body.extend_from_slice(&le64(0.0)); // extrusion.z
        let mode: u8 = 0; // has_elevation = false -> Z present per point
        let mut rec = vec![1u8, mode];
        rec.extend_from_slice(&le16((4 + body.len()) as i16));
        rec.extend_from_slice(&body);

        let svg = render_main_entities_only(&rec);
        let d = path_data(&svg);
        assert_eq!(d.len(), 1);
        let nums = path_numbers(d[0]);
        assert_eq!(nums.len(), 4);
        let (x1, _y1, x2, y2) = (nums[0], nums[1], nums[2], nums[3]);
        assert!(
            (x1 - x2).abs() < 1e-6,
            "expected equal world X, got {x1} vs {x2}"
        );
        assert!(
            y2.abs() > 1.0,
            "expected a non-trivial world Y span from ocs z, got {y2}"
        );
    }

    #[test]
    fn circle_with_tilted_extrusion_draws_as_a_foreshortened_ellipse() {
        let rec = circle_record(50.0, 10.0, Some((1.0, 1.0, 1.0)));
        let svg = render_main_entities_only(&rec);
        let (rx, ry) = first_arc_radii(&svg);
        // A genuinely tilted OCS must foreshorten the circle: unequal
        // radii on the rendered ellipse's two arcs.
        assert!(
            (rx - ry).abs() > rx * 0.05,
            "expected an ellipse (rx != ry), got rx={rx} ry={ry}"
        );
    }

    #[test]
    fn circle_with_z_flip_extrusion_still_draws_as_a_true_circle() {
        let rec = circle_record(50.0, 10.0, Some((0.0, 0.0, -1.0)));
        let svg = render_main_entities_only(&rec);
        let (rx, ry) = first_arc_radii(&svg);
        assert!(
            (rx - ry).abs() < rx * 1e-6,
            "expected a true circle (rx == ry), got rx={rx} ry={ry}"
        );
    }

    fn circle_record(radius: f64, elevation: f64, extrusion: Option<(f64, f64, f64)>) -> Vec<u8> {
        let mut body = Vec::new();
        body.extend_from_slice(&le16(0)); // layer_index
        let common_hi: u8 = if extrusion.is_some() { 0x01 } else { 0x00 };
        body.extend_from_slice(&[common_hi, 0x00]);
        let mode: u8 = if elevation != 0.0 { 0b0000_0100 } else { 0 };
        if elevation != 0.0 {
            body.extend_from_slice(&le64(elevation));
        }
        body.extend_from_slice(&le64(0.0)); // cx
        body.extend_from_slice(&le64(0.0)); // cy
        body.extend_from_slice(&le64(radius));
        if let Some((ex, ey, ez)) = extrusion {
            body.extend_from_slice(&le64(ex));
            body.extend_from_slice(&le64(ey));
            body.extend_from_slice(&le64(ez));
        }
        let mut rec = vec![3u8, mode];
        rec.extend_from_slice(&le16((4 + body.len()) as i16));
        rec.extend_from_slice(&body);
        rec
    }

    /// Parses every plain number out of an SVG path's `d` attribute value
    /// (as produced by this crate's DXF/DWG path builders: space-separated
    /// `M`/`L`/`A` commands with no commas).
    fn path_numbers(d: &str) -> Vec<f64> {
        d.split(|c: char| c.is_alphabetic())
            .flat_map(|chunk| chunk.split_whitespace())
            .filter_map(|s| s.parse().ok())
            .collect()
    }

    /// Extracts `(rx, ry)` from the first `A rx ry ...` command in an SVG
    /// document's first path (works for both `circle_path`'s and the
    /// Ellipse renderer's two-arc full-circle/ellipse path syntax).
    fn first_arc_radii(svg: &str) -> (f64, f64) {
        let d = path_data(svg);
        assert_eq!(d.len(), 1, "expected exactly one path, got {d:?}");
        let after_a = d[0].split(" A ").nth(1).expect("no A command in path");
        let mut nums = after_a.split_whitespace();
        let rx: f64 = nums.next().unwrap().parse().unwrap();
        let ry: f64 = nums.next().unwrap().parse().unwrap();
        (rx, ry)
    }

    #[test]
    fn bit_reads_declaration_order_msb_first() {
        assert!(bit(0b1000_0000, 1));
        assert!(!bit(0b1000_0000, 2));
        assert!(bit(0b0000_0001, 8));
        assert!(!bit(0b0000_0001, 7));
    }
}
