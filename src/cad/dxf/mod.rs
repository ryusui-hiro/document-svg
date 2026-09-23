//! DXF document conversion to Page IR.

pub(crate) mod binary;
pub mod geometry;
pub(crate) mod polyline;
pub mod reader;
pub mod types;
pub mod writer;

use std::collections::{HashMap, HashSet};
use std::f64::consts::PI;
use std::io::BufRead;

use crate::convert::{ConvertOptions, PageConsumer};
use crate::error::Result;
use crate::ir::{
    IDENTITY, LineCap, LineJoin, Node, Page, Paint, SourceMeta, Stroke, TextAnchor, TextRun,
};

use self::geometry::{BBox, arc_to_svg_path, circle_path, deg_to_rad, fmt_coord};
use self::reader::parse_dxf;
use self::types::{DxfDocument, Entity, HatchBoundary, Layer};

const MAX_BLOCK_RECURSION: usize = 16;
const TARGET_PAGE_LONG_EDGE: f64 = 1200.0;
const MIN_PAGE_DIMENSION: f64 = 400.0;

pub(crate) fn convert<R: BufRead>(
    reader: R,
    _options: &ConvertOptions,
    sink: &mut dyn PageConsumer,
) -> Result<Vec<String>> {
    let doc = parse_dxf(reader)?;
    let mut warnings = Vec::new();

    let page = render_dxf_to_page(&doc, &mut warnings)?;
    sink.consume(page)?;

    Ok(warnings)
}

/// Renders a parsed DXF document into an IR [`Page`].
pub fn render_dxf_to_page(doc: &DxfDocument, warnings: &mut Vec<String>) -> Result<Page> {
    // 1. Compute overall bounding box of visible entities
    let mut bbox = BBox::new();
    let mut visited_blocks = HashSet::new();

    for entity in &doc.entities {
        if is_layer_visible(doc, entity.layer()) {
            accumulate_entity_bbox(entity, doc, &mut bbox, &mut visited_blocks, 0);
        }
    }

    if !bbox.is_valid() {
        // Fallback bounding box if document has no visible geometry
        bbox.min_x = 0.0;
        bbox.min_y = 0.0;
        bbox.max_x = 800.0;
        bbox.max_y = 600.0;
        warnings.push(
            "DXF document contains no valid 2D visible bounds; using default viewport".into(),
        );
    }

    let raw_w = bbox.width().max(1.0);
    let raw_h = bbox.height().max(1.0);

    // Add 5% margin around content
    let margin = (raw_w.max(raw_h) * 0.05).max(10.0);
    let content_w = raw_w + 2.0 * margin;
    let content_h = raw_h + 2.0 * margin;

    // Normalize to target page dimension (preserving aspect ratio)
    let scale = (TARGET_PAGE_LONG_EDGE / content_w.max(content_h)).clamp(0.001, 10.0);
    let page_w = (content_w * scale).max(MIN_PAGE_DIMENSION);
    let page_h = (content_h * scale).max(MIN_PAGE_DIMENSION);

    let mut page = Page::new(1, page_w, page_h, "dxf");
    page.title = "CAD Drawing".into();

    // Coordinate mapping helper:
    // Transforms CAD (x, y) [Y up] to Page (X, Y) [Y down]
    let map_pt = |x: f64, y: f64| -> (f64, f64) {
        let sx = (x - bbox.min_x + margin) * scale;
        let sy = (bbox.max_y - y + margin) * scale;
        (sx, sy)
    };

    // 2. Group entities by layer
    let mut layer_entities: HashMap<String, Vec<&Entity>> = HashMap::new();
    for entity in &doc.entities {
        if is_layer_visible(doc, entity.layer()) {
            layer_entities
                .entry(entity.layer().to_string())
                .or_default()
                .push(entity);
        }
    }

    // Sort layers for deterministic output ordering
    let mut layer_names: Vec<String> = layer_entities.keys().cloned().collect();
    layer_names.sort();

    for layer_name in layer_names {
        let entities = &layer_entities[&layer_name];
        let layer_info = doc.find_layer(&layer_name).cloned().unwrap_or_default();
        let mut layer_nodes = Vec::new();

        for entity in entities {
            render_entity(
                entity,
                doc,
                &layer_info,
                &map_pt,
                scale,
                &mut layer_nodes,
                &mut HashSet::new(),
                0,
                warnings,
            );
        }

        if !layer_nodes.is_empty() {
            let meta = SourceMeta {
                semantic_role: format!("cad:layer:{layer_name}"),
                ..Default::default()
            };
            let group = Node::Group {
                id: format!("layer-{}", sanitize_id(&layer_name)),
                nodes: layer_nodes,
                transform: IDENTITY,
                opacity: 1.0,
                clip_id: None,
                meta,
            };
            page.nodes.push(group);
        }
    }

    Ok(page)
}

fn is_layer_visible(doc: &DxfDocument, layer_name: &str) -> bool {
    if let Some(layer) = doc.find_layer(layer_name) {
        !layer.is_off && !layer.is_frozen
    } else {
        true
    }
}

fn sanitize_id(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

fn accumulate_entity_bbox(
    entity: &Entity,
    doc: &DxfDocument,
    bbox: &mut BBox,
    visited_blocks: &mut HashSet<String>,
    depth: usize,
) {
    match entity {
        Entity::Line { start, end, .. } => {
            bbox.update(start.0, start.1);
            bbox.update(end.0, end.1);
        }
        Entity::Point { pt, .. } => {
            bbox.update(pt.0, pt.1);
        }
        Entity::Circle { center, radius, .. } => {
            bbox.update(center.0 - radius, center.1 - radius);
            bbox.update(center.0 + radius, center.1 + radius);
        }
        Entity::Arc {
            center,
            radius,
            start_deg,
            end_deg,
            ..
        } => {
            // Include start and end points
            let s_rad = deg_to_rad(*start_deg);
            let e_rad = deg_to_rad(*end_deg);
            bbox.update(
                center.0 + radius * s_rad.cos(),
                center.1 + radius * s_rad.sin(),
            );
            bbox.update(
                center.0 + radius * e_rad.cos(),
                center.1 + radius * e_rad.sin(),
            );

            // Check quadrant extremes (0, 90, 180, 270 deg)
            let mut sweep = end_deg - start_deg;
            while sweep <= 0.0 {
                sweep += 360.0;
            }
            for quad in [0.0, 90.0, 180.0, 270.0] {
                let mut d = quad - start_deg;
                while d < 0.0 {
                    d += 360.0;
                }
                while d >= 360.0 {
                    d -= 360.0;
                }
                if d <= sweep {
                    let q_rad = deg_to_rad(quad);
                    bbox.update(
                        center.0 + radius * q_rad.cos(),
                        center.1 + radius * q_rad.sin(),
                    );
                }
            }
        }
        Entity::Ellipse {
            center, major_axis, ..
        } => {
            let r = (major_axis.0 * major_axis.0 + major_axis.1 * major_axis.1).sqrt();
            bbox.update(center.0 - r, center.1 - r);
            bbox.update(center.0 + r, center.1 + r);
        }
        Entity::LwPolyline { vertices, .. } => {
            for v in vertices {
                bbox.update(v.x, v.y);
            }
        }
        Entity::Spline { control_points, .. } => {
            for pt in control_points {
                bbox.update(pt.0, pt.1);
            }
        }
        Entity::Solid { points, .. } => {
            for pt in points {
                bbox.update(pt.0, pt.1);
            }
        }
        Entity::Text { insert, height, .. } | Entity::MText { insert, height, .. } => {
            bbox.update(insert.0, insert.1);
            bbox.update(insert.0 + height * 2.0, insert.1 + height);
        }
        Entity::Insert {
            block_name,
            insert,
            scale,
            rotation_deg,
            ..
        } => {
            if depth < MAX_BLOCK_RECURSION
                && !visited_blocks.contains(block_name)
                && let Some(block) = doc.blocks.get(block_name)
            {
                visited_blocks.insert(block_name.clone());
                let mut block_bbox = BBox::new();
                for b_ent in &block.entities {
                    accumulate_entity_bbox(b_ent, doc, &mut block_bbox, visited_blocks, depth + 1);
                }
                visited_blocks.remove(block_name);

                if block_bbox.is_valid() {
                    let rad = deg_to_rad(*rotation_deg);
                    let cos = rad.cos();
                    let sin = rad.sin();
                    for corner in [
                        (
                            block_bbox.min_x - block.base_point.0,
                            block_bbox.min_y - block.base_point.1,
                        ),
                        (
                            block_bbox.max_x - block.base_point.0,
                            block_bbox.min_y - block.base_point.1,
                        ),
                        (
                            block_bbox.max_x - block.base_point.0,
                            block_bbox.max_y - block.base_point.1,
                        ),
                        (
                            block_bbox.min_x - block.base_point.0,
                            block_bbox.max_y - block.base_point.1,
                        ),
                    ] {
                        let sx = corner.0 * scale.0;
                        let sy = corner.1 * scale.1;
                        let rx = sx * cos - sy * sin + insert.0;
                        let ry = sx * sin + sy * cos + insert.1;
                        bbox.update(rx, ry);
                    }
                }
            }
        }
        Entity::Hatch { .. } => {}
        Entity::Dimension {
            insert, block_name, ..
        } => {
            bbox.update(insert.0, insert.1);
            if let Some(name) = block_name
                && depth < MAX_BLOCK_RECURSION
                && !visited_blocks.contains(name)
                && let Some(block) = doc.find_block(name)
            {
                visited_blocks.insert(name.clone());
                for b_ent in &block.entities {
                    accumulate_entity_bbox(b_ent, doc, bbox, visited_blocks, depth + 1);
                }
                visited_blocks.remove(name);
            }
        }
        Entity::Leader { vertices, .. } => {
            for pt in vertices {
                bbox.update(pt.0, pt.1);
            }
        }
    }
}

fn resolve_color(ent_color: &Option<String>, layer: &Layer) -> String {
    ent_color
        .as_ref()
        .cloned()
        .unwrap_or_else(|| layer.color_hex.clone())
}

fn resolve_stroke_width(ent_lw: &Option<f64>, layer: &Layer, scale: f64) -> f64 {
    let raw_mm = ent_lw.or(layer.line_weight).unwrap_or(0.25); // default 0.25 mm
    // Convert mm to points (1 mm = 72 / 25.4 pt ~ 2.8346 pt)
    let pt = raw_mm * 72.0 / 25.4;
    // Stroke width scaled to display units, clamped to stay legible
    (pt * scale).clamp(0.75, 8.0)
}

fn resolve_dash_array(
    ent_lt: &Option<String>,
    layer: &Layer,
    doc: &DxfDocument,
    scale: f64,
) -> Vec<f64> {
    let lt_name = ent_lt
        .as_ref()
        .unwrap_or(&layer.linetype)
        .to_ascii_uppercase();

    if lt_name == "CONTINUOUS" || lt_name.is_empty() {
        return Vec::new();
    }

    if let Some(ltype) = doc.linetypes.get(&lt_name)
        && !ltype.pattern.is_empty()
    {
        let mut dashes = Vec::new();
        for seg in &ltype.pattern {
            let len = (seg.abs() * scale).max(1.0);
            dashes.push(len);
        }
        return dashes;
    }

    // Built-in standard fallbacks for common CAD linetypes
    if lt_name.contains("DASH") || lt_name.contains("HIDDEN") {
        vec![6.0, 3.0]
    } else if lt_name.contains("DOT") {
        vec![1.5, 3.0]
    } else if lt_name.contains("CENTER") {
        vec![12.0, 3.0, 3.0, 3.0]
    } else if lt_name.contains("PHANTOM") {
        vec![12.0, 3.0, 3.0, 3.0, 3.0, 3.0]
    } else {
        Vec::new()
    }
}

/// Maps a `TEXT` entity's DXF horizontal (`72`) and vertical (`73`)
/// justification onto an SVG text anchor and a baseline offset (in page
/// units, Y down) from the entity's alignment point.
///
/// Horizontal: Left -> start; Center, Middle, Aligned and Fit -> middle;
/// Right -> end. Vertical: Baseline sits on the point; Bottom puts the
/// descender bottom there; Middle centres the cap height on it; Top hangs
/// the cap height below it. The 0.7 cap-height and 0.2 descent ratios are
/// the usual sans-serif proportions, since no CAD font metrics are known.
fn text_alignment(h_align: u8, v_align: u8, font_size: f64) -> (TextAnchor, f64) {
    let anchor = match h_align {
        1 | 3 | 4 | 5 => TextAnchor::Middle,
        2 => TextAnchor::End,
        _ => TextAnchor::Start,
    };
    let baseline_dy = match v_align {
        1 => -0.2 * font_size,
        2 => 0.35 * font_size,
        3 => 0.7 * font_size,
        _ => 0.0,
    };
    (anchor, baseline_dy)
}

fn make_stroke(color_hex: &str, width: f64, dash_array: Vec<f64>) -> Stroke {
    Stroke {
        paint: Paint::solid(color_hex),
        width,
        line_cap: LineCap::Round,
        line_join: LineJoin::Round,
        miter_limit: 4.0,
        dash_array,
        dash_offset: 0.0,
    }
}

#[allow(clippy::too_many_arguments, clippy::only_used_in_recursion)]
fn render_entity(
    entity: &Entity,
    doc: &DxfDocument,
    layer: &Layer,
    map_pt: &dyn Fn(f64, f64) -> (f64, f64),
    scale: f64,
    nodes: &mut Vec<Node>,
    visited_blocks: &mut HashSet<String>,
    depth: usize,
    warnings: &mut Vec<String>,
) {
    let color_hex = resolve_color(
        match entity {
            Entity::Line { color, .. }
            | Entity::Point { color, .. }
            | Entity::Circle { color, .. }
            | Entity::Arc { color, .. }
            | Entity::Ellipse { color, .. }
            | Entity::LwPolyline { color, .. }
            | Entity::Spline { color, .. }
            | Entity::Solid { color, .. }
            | Entity::Text { color, .. }
            | Entity::MText { color, .. }
            | Entity::Insert { color, .. }
            | Entity::Hatch { color, .. }
            | Entity::Dimension { color, .. }
            | Entity::Leader { color, .. } => color,
        },
        layer,
    );

    let stroke_width = resolve_stroke_width(
        match entity {
            Entity::Line { line_weight, .. }
            | Entity::Circle { line_weight, .. }
            | Entity::Arc { line_weight, .. }
            | Entity::Ellipse { line_weight, .. }
            | Entity::LwPolyline { line_weight, .. }
            | Entity::Spline { line_weight, .. }
            | Entity::Insert { line_weight, .. } => line_weight,
            _ => &None,
        },
        layer,
        scale,
    );

    let dash_array = resolve_dash_array(
        match entity {
            Entity::Line { linetype, .. }
            | Entity::Circle { linetype, .. }
            | Entity::Arc { linetype, .. }
            | Entity::Ellipse { linetype, .. }
            | Entity::LwPolyline { linetype, .. } => linetype,
            _ => &None,
        },
        layer,
        doc,
        scale,
    );

    match entity {
        Entity::Line { start, end, .. } => {
            let (x1, y1) = map_pt(start.0, start.1);
            let (x2, y2) = map_pt(end.0, end.1);
            let d = format!(
                "M {} {} L {} {}",
                fmt_coord(x1),
                fmt_coord(y1),
                fmt_coord(x2),
                fmt_coord(y2)
            );
            nodes.push(Node::Path {
                id: String::new(),
                d,
                fill_rule: "nonzero".into(),
                fill: Paint::None,
                stroke: make_stroke(&color_hex, stroke_width, dash_array),
                transform: IDENTITY,
                clip_id: None,
                meta: SourceMeta::default(),
            });
        }
        Entity::Point { pt, .. } => {
            let (x, y) = map_pt(pt.0, pt.1);
            let r = (stroke_width * 0.75).max(1.5);
            let d = circle_path(x, y, r);
            nodes.push(Node::Path {
                id: String::new(),
                d,
                fill_rule: "nonzero".into(),
                fill: Paint::solid(color_hex),
                stroke: Stroke::default(),
                transform: IDENTITY,
                clip_id: None,
                meta: SourceMeta::default(),
            });
        }
        Entity::Circle { center, radius, .. } => {
            let (cx, cy) = map_pt(center.0, center.1);
            let r = radius * scale;
            let d = circle_path(cx, cy, r);
            nodes.push(Node::Path {
                id: String::new(),
                d,
                fill_rule: "nonzero".into(),
                fill: Paint::None,
                stroke: make_stroke(&color_hex, stroke_width, dash_array),
                transform: IDENTITY,
                clip_id: None,
                meta: SourceMeta::default(),
            });
        }
        Entity::Arc {
            center,
            radius,
            start_deg,
            end_deg,
            ..
        } => {
            let (cx, cy) = map_pt(center.0, center.1);
            let r = radius * scale;
            let d = arc_to_svg_path(cx, cy, r, *start_deg, *end_deg, true);
            if !d.is_empty() {
                nodes.push(Node::Path {
                    id: String::new(),
                    d,
                    fill_rule: "nonzero".into(),
                    fill: Paint::None,
                    stroke: make_stroke(&color_hex, stroke_width, dash_array),
                    transform: IDENTITY,
                    clip_id: None,
                    meta: SourceMeta::default(),
                });
            }
        }
        Entity::Ellipse {
            center,
            major_axis,
            axis_ratio,
            ..
        } => {
            let (cx, cy) = map_pt(center.0, center.1);
            let a = (major_axis.0 * major_axis.0 + major_axis.1 * major_axis.1).sqrt() * scale;
            let b = (a * axis_ratio).max(0.1);
            // Invert angle for SVG display space
            let angle_rad = -major_axis.1.atan2(major_axis.0);
            let angle_deg = angle_rad * 180.0 / PI;

            let d = format!(
                "M {} {} A {} {} {} 1 0 {} {} A {} {} {} 1 0 {} {} Z",
                fmt_coord(cx - a),
                fmt_coord(cy),
                fmt_coord(a),
                fmt_coord(b),
                fmt_coord(angle_deg),
                fmt_coord(cx + a),
                fmt_coord(cy),
                fmt_coord(a),
                fmt_coord(b),
                fmt_coord(angle_deg),
                fmt_coord(cx - a),
                fmt_coord(cy),
            );
            nodes.push(Node::Path {
                id: String::new(),
                d,
                fill_rule: "nonzero".into(),
                fill: Paint::None,
                stroke: make_stroke(&color_hex, stroke_width, dash_array),
                transform: IDENTITY,
                clip_id: None,
                meta: SourceMeta::default(),
            });
        }
        Entity::LwPolyline {
            vertices,
            is_closed,
            ..
        } => {
            if vertices.len() >= 2 {
                let mut d = String::new();
                for i in 0..vertices.len() {
                    let v = &vertices[i];
                    let (x, y) = map_pt(v.x, v.y);
                    if i == 0 {
                        d.push_str(&format!("M {} {}", fmt_coord(x), fmt_coord(y)));
                    } else {
                        let prev = &vertices[i - 1];
                        let (px, py) = map_pt(prev.x, prev.y);
                        if prev.bulge.abs() > 1e-9 {
                            let arc_cmd =
                                geometry::bulge_to_svg_arc(px, py, x, y, prev.bulge, true);
                            d.push(' ');
                            d.push_str(&arc_cmd);
                        } else {
                            d.push_str(&format!(" L {} {}", fmt_coord(x), fmt_coord(y)));
                        }
                    }
                }
                if *is_closed && !vertices.is_empty() {
                    let last = vertices.last().unwrap();
                    let first = &vertices[0];
                    let (fx, fy) = map_pt(first.x, first.y);
                    let (lx, ly) = map_pt(last.x, last.y);
                    if last.bulge.abs() > 1e-9 {
                        let arc_cmd = geometry::bulge_to_svg_arc(lx, ly, fx, fy, last.bulge, true);
                        d.push(' ');
                        d.push_str(&arc_cmd);
                    }
                    d.push_str(" Z");
                }
                nodes.push(Node::Path {
                    id: String::new(),
                    d,
                    fill_rule: "nonzero".into(),
                    fill: Paint::None,
                    stroke: make_stroke(&color_hex, stroke_width, dash_array),
                    transform: IDENTITY,
                    clip_id: None,
                    meta: SourceMeta::default(),
                });
            }
        }
        Entity::Spline {
            control_points,
            is_closed,
            ..
        } => {
            if control_points.len() >= 2 {
                let mut d = String::new();
                for (i, pt) in control_points.iter().enumerate() {
                    let (x, y) = map_pt(pt.0, pt.1);
                    if i == 0 {
                        d.push_str(&format!("M {} {}", fmt_coord(x), fmt_coord(y)));
                    } else {
                        d.push_str(&format!(" L {} {}", fmt_coord(x), fmt_coord(y)));
                    }
                }
                if *is_closed {
                    d.push_str(" Z");
                }
                nodes.push(Node::Path {
                    id: String::new(),
                    d,
                    fill_rule: "nonzero".into(),
                    fill: Paint::None,
                    stroke: make_stroke(&color_hex, stroke_width, dash_array),
                    transform: IDENTITY,
                    clip_id: None,
                    meta: SourceMeta::default(),
                });
            }
        }
        Entity::Solid { points, .. } => {
            let p0 = map_pt(points[0].0, points[0].1);
            let p1 = map_pt(points[1].0, points[1].1);
            let p2 = map_pt(points[2].0, points[2].1);
            let p3 = map_pt(points[3].0, points[3].1);
            // DXF SOLID vertex ordering is 0, 1, 3, 2 for non-self-intersecting quadrilateral
            let d = format!(
                "M {} {} L {} {} L {} {} L {} {} Z",
                fmt_coord(p0.0),
                fmt_coord(p0.1),
                fmt_coord(p1.0),
                fmt_coord(p1.1),
                fmt_coord(p3.0),
                fmt_coord(p3.1),
                fmt_coord(p2.0),
                fmt_coord(p2.1),
            );
            nodes.push(Node::Path {
                id: String::new(),
                d,
                fill_rule: "nonzero".into(),
                fill: Paint::solid(color_hex),
                stroke: Stroke::default(),
                transform: IDENTITY,
                clip_id: None,
                meta: SourceMeta::default(),
            });
        }
        Entity::Text {
            text,
            insert,
            height,
            rotation_deg,
            h_align,
            v_align,
            ..
        } => {
            if !text.is_empty() {
                let (x, y) = map_pt(insert.0, insert.1);
                let font_size = (height * scale).clamp(6.0, 100.0);
                let (anchor, baseline_dy) = text_alignment(*h_align, *v_align, font_size);
                // Negative rotation because Y is flipped
                let rad = deg_to_rad(-*rotation_deg);
                let transform = if rotation_deg.abs() > 1e-4 {
                    let cos = rad.cos();
                    let sin = rad.sin();
                    // Rotate around the alignment point (x, y); the baseline
                    // origin below is offset from it along the (pre-rotation)
                    // vertical, so the offset turns with the text.
                    [
                        cos,
                        sin,
                        -sin,
                        cos,
                        x * (1.0 - cos) + y * sin,
                        y * (1.0 - cos) - x * sin,
                    ]
                } else {
                    IDENTITY
                };
                let run = TextRun {
                    text: clean_dxf_text(text),
                    font_family: "sans-serif, Arial, 'Hiragino Sans'".into(),
                    font_size,
                    bold: false,
                    italic: false,
                    fill: Paint::solid(color_hex),
                    baseline_shift: 0.0,
                    glyph_x_offsets: Vec::new(),
                    target_advance: None,
                };
                nodes.push(Node::Text {
                    id: String::new(),
                    x,
                    y: y + baseline_dy,
                    runs: vec![run],
                    anchor,
                    transform,
                    opacity: 1.0,
                    stroke: Stroke::default(),
                    clip_id: None,
                    meta: SourceMeta::default(),
                });
            }
        }
        Entity::MText {
            text,
            insert,
            height,
            rotation_deg,
            ..
        } => {
            if !text.is_empty() {
                let (x, y) = map_pt(insert.0, insert.1);
                let font_size = (height * scale).clamp(6.0, 100.0);
                // Negative rotation because Y is flipped
                let rad = deg_to_rad(-*rotation_deg);
                let transform = if rotation_deg.abs() > 1e-4 {
                    let cos = rad.cos();
                    let sin = rad.sin();
                    // Rotate around (x, y)
                    [
                        cos,
                        sin,
                        -sin,
                        cos,
                        x * (1.0 - cos) + y * sin,
                        y * (1.0 - cos) - x * sin,
                    ]
                } else {
                    IDENTITY
                };
                let run = TextRun {
                    text: clean_dxf_text(text),
                    font_family: "sans-serif, Arial, 'Hiragino Sans'".into(),
                    font_size,
                    bold: false,
                    italic: false,
                    fill: Paint::solid(color_hex),
                    baseline_shift: 0.0,
                    glyph_x_offsets: Vec::new(),
                    target_advance: None,
                };
                nodes.push(Node::Text {
                    id: String::new(),
                    x,
                    y,
                    runs: vec![run],
                    anchor: TextAnchor::Start,
                    transform,
                    opacity: 1.0,
                    stroke: Stroke::default(),
                    clip_id: None,
                    meta: SourceMeta::default(),
                });
            }
        }
        Entity::Insert {
            block_name,
            insert,
            scale: blk_scale,
            rotation_deg,
            ..
        } => {
            if depth < MAX_BLOCK_RECURSION
                && !visited_blocks.contains(block_name)
                && let Some(block) = doc.find_block(block_name)
            {
                visited_blocks.insert(block_name.clone());
                let rad = deg_to_rad(*rotation_deg);
                let cos = rad.cos();
                let sin = rad.sin();

                let block_map_pt = |bx: f64, by: f64| -> (f64, f64) {
                    let rel_x = (bx - block.base_point.0) * blk_scale.0;
                    let rel_y = (by - block.base_point.1) * blk_scale.1;
                    let rx = rel_x * cos - rel_y * sin + insert.0;
                    let ry = rel_x * sin + rel_y * cos + insert.1;
                    map_pt(rx, ry)
                };

                for b_entity in &block.entities {
                    render_entity(
                        b_entity,
                        doc,
                        layer,
                        &block_map_pt,
                        scale * blk_scale.0.abs().max(0.1),
                        nodes,
                        visited_blocks,
                        depth + 1,
                        warnings,
                    );
                }
                visited_blocks.remove(block_name);
            }
        }
        Entity::Dimension {
            block_name,
            text,
            insert,
            color,
            ..
        } => {
            let mut drawn_block = false;
            if let Some(name) = block_name
                && depth < MAX_BLOCK_RECURSION
                && !visited_blocks.contains(name)
                && let Some(block) = doc.find_block(name)
            {
                visited_blocks.insert(name.clone());
                let block_map_pt = |bx: f64, by: f64| -> (f64, f64) { map_pt(bx, by) };
                for b_entity in &block.entities {
                    render_entity(
                        b_entity,
                        doc,
                        layer,
                        &block_map_pt,
                        scale,
                        nodes,
                        visited_blocks,
                        depth + 1,
                        warnings,
                    );
                }
                visited_blocks.remove(name);
                drawn_block = true;
            }
            if !drawn_block && !text.is_empty() && text != "<>" {
                let (x, y) = map_pt(insert.0, insert.1);
                let color_hex = resolve_color(color, layer);
                let run = TextRun {
                    text: clean_dxf_text(text),
                    font_family: "sans-serif, Arial, 'Hiragino Sans'".into(),
                    font_size: 9.0,
                    bold: false,
                    italic: false,
                    fill: Paint::solid(color_hex),
                    baseline_shift: 0.0,
                    glyph_x_offsets: Vec::new(),
                    target_advance: None,
                };
                nodes.push(Node::Text {
                    id: String::new(),
                    x,
                    y,
                    runs: vec![run],
                    anchor: TextAnchor::Middle,
                    transform: IDENTITY,
                    opacity: 1.0,
                    stroke: Stroke::default(),
                    clip_id: None,
                    meta: SourceMeta::default(),
                });
            }
        }
        Entity::Leader {
            vertices, color, ..
        } => {
            if vertices.len() >= 2 {
                let stroke_w = resolve_stroke_width(&None, layer, scale);
                let color_hex = resolve_color(color, layer);
                let mut d = String::new();
                for (i, pt) in vertices.iter().enumerate() {
                    let p = map_pt(pt.0, pt.1);
                    if i == 0 {
                        d.push_str(&format!("M {} {}", fmt_coord(p.0), fmt_coord(p.1)));
                    } else {
                        d.push_str(&format!(" L {} {}", fmt_coord(p.0), fmt_coord(p.1)));
                    }
                }
                nodes.push(Node::Path {
                    id: String::new(),
                    d,
                    fill_rule: "nonzero".into(),
                    fill: Paint::None,
                    stroke: Stroke {
                        paint: Paint::solid(color_hex),
                        width: stroke_w,
                        line_cap: LineCap::Round,
                        line_join: LineJoin::Round,
                        miter_limit: 4.0,
                        dash_array: Vec::new(),
                        dash_offset: 0.0,
                    },
                    transform: IDENTITY,
                    clip_id: None,
                    meta: SourceMeta::default(),
                });
            }
        }
        Entity::Hatch {
            boundaries,
            is_solid,
            color,
            ..
        } => {
            let color_hex = resolve_color(color, layer);
            let stroke_w = resolve_stroke_width(&None, layer, scale);
            for b in boundaries {
                match b {
                    HatchBoundary::Polyline {
                        vertices,
                        is_closed,
                    } => {
                        if vertices.len() >= 2 {
                            let mut d = String::new();
                            for (i, v) in vertices.iter().enumerate() {
                                let p = map_pt(v.x, v.y);
                                if i == 0 {
                                    d.push_str(&format!("M {} {}", fmt_coord(p.0), fmt_coord(p.1)));
                                } else {
                                    d.push_str(&format!(
                                        " L {} {}",
                                        fmt_coord(p.0),
                                        fmt_coord(p.1)
                                    ));
                                }
                            }
                            if *is_closed {
                                d.push_str(" Z");
                            }
                            nodes.push(Node::Path {
                                id: String::new(),
                                d,
                                fill_rule: "nonzero".into(),
                                fill: if *is_solid {
                                    Paint::solid(&color_hex)
                                } else {
                                    Paint::None
                                },
                                stroke: Stroke {
                                    paint: Paint::solid(&color_hex),
                                    width: stroke_w,
                                    line_cap: LineCap::Round,
                                    line_join: LineJoin::Round,
                                    miter_limit: 4.0,
                                    dash_array: Vec::new(),
                                    dash_offset: 0.0,
                                },
                                transform: IDENTITY,
                                clip_id: None,
                                meta: SourceMeta::default(),
                            });
                        }
                    }
                    HatchBoundary::Edges(edges) => {
                        for edge in edges {
                            render_entity(
                                edge,
                                doc,
                                layer,
                                map_pt,
                                scale,
                                nodes,
                                visited_blocks,
                                depth + 1,
                                warnings,
                            );
                        }
                    }
                }
            }
        }
    }
}

/// Cleans DXF MTEXT formatting codes (e.g. `\P`, `\A1;`, `\C1;`, `%%c`, `%%d`, `%%p`).
fn clean_dxf_text(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();

    while let Some(c) = chars.next() {
        if c == '%' && chars.peek() == Some(&'%') {
            chars.next();
            if let Some(code) = chars.next() {
                match code {
                    'd' | 'D' => out.push('°'), // degree symbol
                    'p' | 'P' => out.push('±'), // plus-minus
                    'c' | 'C' => out.push('Ø'), // diameter symbol
                    other => {
                        out.push('%');
                        out.push('%');
                        out.push(other);
                    }
                }
                continue;
            }
        } else if c == '\\'
            && let Some(&next_c) = chars.peek()
        {
            let upper = next_c.to_ascii_uppercase();
            if upper == 'P' {
                chars.next();
                out.push('\n');
                continue;
            } else if next_c == '~' {
                chars.next();
                out.push(' ');
                continue;
            } else if next_c == '{' || next_c == '}' || next_c == '\\' {
                chars.next();
                out.push(next_c);
                continue;
            } else if matches!(upper, 'L' | 'O' | 'K') {
                // Formatting toggles for underline, overline, strikethrough
                chars.next();
                continue;
            } else if matches!(upper, 'A' | 'C' | 'H' | 'W' | 'T' | 'Q' | 'F' | 'S') {
                // Skip until ';'
                chars.next();
                for inner in chars.by_ref() {
                    if inner == ';' {
                        break;
                    }
                }
                continue;
            }
        }
        if c != '{' && c != '}' {
            out.push(c);
        }
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    struct DummySink(pub Vec<Page>);
    impl PageConsumer for DummySink {
        fn consume(&mut self, page: Page) -> Result<()> {
            self.0.push(page);
            Ok(())
        }
    }

    #[test]
    fn converts_simple_dxf_to_page() {
        let dxf_content = r#"  0
SECTION
  2
ENTITIES
  0
LINE
  8
Walls
 10
0.0
 20
0.0
 11
500.0
 21
300.0
  0
CIRCLE
  8
Columns
 10
250.0
 20
150.0
 40
50.0
  0
ENDSEC
  0
EOF
"#;
        let options = ConvertOptions::default();
        let mut sink = DummySink(Vec::new());
        let warnings = convert(Cursor::new(dxf_content), &options, &mut sink).expect("convert DXF");
        assert!(warnings.is_empty() || warnings.iter().all(|w| !w.is_empty()));
        assert_eq!(sink.0.len(), 1);
        let page = &sink.0[0];
        assert!(page.width >= 400.0);
        assert!(page.height >= 400.0);
        // Groups for layer "Walls" and "Columns"
        assert!(!page.nodes.is_empty());
    }

    #[test]
    fn cleans_dxf_text_formatting() {
        assert_eq!(clean_dxf_text(r"\A1;Hello\PWorld"), "Hello\nWorld");
        assert_eq!(clean_dxf_text("%%c50.0"), "Ø50.0");
        assert_eq!(clean_dxf_text("45%%d"), "45°");
        assert_eq!(clean_dxf_text(r"%%p0.05"), "±0.05");
        assert_eq!(clean_dxf_text(r"{\fArial;Sample Text}"), "Sample Text");
        assert_eq!(
            clean_dxf_text(r"\LUnderline\l and \ONormal\o"),
            "Underline and Normal"
        );
        assert_eq!(clean_dxf_text(r"Item\~1"), "Item 1");
    }
}
