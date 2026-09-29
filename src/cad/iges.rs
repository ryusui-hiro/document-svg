//! Initial Graphics Exchange Specification (IGES / .iges / .igs) CAD parser and vector wireframe renderer.
//!
//! Parses standard 80-column ASCII IGES sections (Directory Entry & Parameter Data),
//! extracts 3D wireframe entities (Lines 110, Arcs 100, Copious Data 106, and
//! Rational B-Spline Curves 126 tessellated through [`crate::cad::nurbs`]),
//! and projects them with an isometric camera into clean vector SVG drawings.

use std::collections::HashMap;
use std::path::Path;

use crate::convert::{ConvertOptions, PageConsumer, read_limited_file};
use crate::error::{Error, Result};
use crate::ir::{
    IDENTITY, LineCap, LineJoin, Node, Page, Paint, SourceMeta, Stroke, TextAnchor, TextRun,
};

const TARGET_PAGE_LONG_EDGE: f64 = 1200.0;
const MIN_PAGE_DIMENSION: f64 = 400.0;
const MAX_IGES_ENTITIES: usize = 100_000;
/// Bound on control points accepted from a single entity 126 so a crafted
/// K value cannot force a huge allocation before parameter data is validated.
const MAX_IGES_SPLINE_CONTROL_POINTS: usize = 20_000;
/// Points sampled along a tessellated B-spline curve; bounded and independent
/// of curve complexity to keep rendering deterministic and inexpensive.
const IGES_SPLINE_TESSELLATION_STEPS: usize = 64;

#[derive(Clone, Copy, Debug)]
struct Vec3 {
    x: f64,
    y: f64,
    z: f64,
}

#[derive(Clone, Debug)]
enum IgesEntity {
    Line {
        p1: Vec3,
        p2: Vec3,
    },
    Arc {
        center: Vec3,
        radius: f64,
        start: Vec3,
        end: Vec3,
    },
    Polyline {
        points: Vec<Vec3>,
    },
    /// Entity 116: a single point, drawn as a small cross marker.
    Point {
        p: Vec3,
    },
    /// Entity 212 (General Note): one text string placed at its origin.
    Text {
        origin: Vec3,
        height: f64,
        /// Rotation of the text baseline in the XY plane, radians.
        rotation: f64,
        text: String,
    },
}

/// Points sampled along a conic arc (entity 104) or per parametric spline
/// segment (entity 112); bounded like the B-spline tessellation.
const IGES_CONIC_TESSELLATION_STEPS: usize = 48;
const IGES_SPLINE_SEGMENT_STEPS: usize = 8;
const MAX_IGES_SPLINE_SEGMENTS: usize = 20_000;
/// How many chained entity-124 transformation matrices are followed for one
/// entity before giving up (a cycle or an absurd nesting depth).
const MAX_IGES_TRANSFORM_DEPTH: usize = 8;
/// Half-size of the cross drawn for an entity-116 point, in page units.
const IGES_POINT_MARKER_HALF_SIZE: f64 = 3.0;

/// An entity-124 transformation: `[R | T]`, applied as `R * p + T`.
#[derive(Clone, Copy, Debug, PartialEq)]
struct IgesTransform {
    r: [[f64; 3]; 3],
    t: [f64; 3],
}

impl IgesTransform {
    const IDENTITY: Self = Self {
        r: [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
        t: [0.0; 3],
    };

    fn is_identity(&self) -> bool {
        *self == Self::IDENTITY
    }

    fn apply(&self, p: Vec3) -> Vec3 {
        let v = [p.x, p.y, p.z];
        let mut out = [0.0; 3];
        for (i, row) in self.r.iter().enumerate() {
            out[i] = row[0] * v[0] + row[1] * v[1] + row[2] * v[2] + self.t[i];
        }
        Vec3 {
            x: out[0],
            y: out[1],
            z: out[2],
        }
    }

    /// `self ∘ inner`: applies `inner` first, then `self`.
    fn then(&self, inner: &Self) -> Self {
        let mut r = [[0.0; 3]; 3];
        let mut t = [0.0; 3];
        for i in 0..3 {
            for (j, cell) in r[i].iter_mut().enumerate() {
                *cell = (0..3).map(|k| self.r[i][k] * inner.r[k][j]).sum();
            }
            t[i] = (0..3).map(|k| self.r[i][k] * inner.t[k]).sum::<f64>() + self.t[i];
        }
        Self { r, t }
    }
}

pub(crate) fn convert(
    path: &Path,
    options: &ConvertOptions,
    sink: &mut dyn PageConsumer,
) -> Result<Vec<String>> {
    let bytes = read_limited_file(path, options.max_input_bytes, "IGES file")?;
    let text = match String::from_utf8(bytes) {
        Ok(s) => s,
        Err(e) => e.into_bytes().iter().map(|&b| b as char).collect(),
    };

    let entities = parse_iges(&text)?;
    if entities.is_empty() {
        return Err(Error::InvalidInput(
            "no wireframe entities found in IGES file".into(),
        ));
    }

    // Isometric projection
    let cos_y = (std::f64::consts::PI / 4.0).cos();
    let sin_y = (std::f64::consts::PI / 4.0).sin();
    let angle_x = (35.264f64).to_radians();
    let cos_x = angle_x.cos();
    let sin_x = angle_x.sin();

    let project = |p: Vec3| -> (f64, f64) {
        let x1 = p.x * cos_y + p.z * sin_y;
        let y1 = p.y;
        let z1 = -p.x * sin_y + p.z * cos_y;

        let x2 = x1;
        let y2 = y1 * cos_x - z1 * sin_x;
        (x2, -y2)
    };

    let mut min_x = f64::INFINITY;
    let mut max_x = f64::NEG_INFINITY;
    let mut min_y = f64::INFINITY;
    let mut max_y = f64::NEG_INFINITY;

    let update_bounds =
        |pt: (f64, f64), min_x: &mut f64, max_x: &mut f64, min_y: &mut f64, max_y: &mut f64| {
            *min_x = min_x.min(pt.0);
            *max_x = max_x.max(pt.0);
            *min_y = min_y.min(pt.1);
            *max_y = max_y.max(pt.1);
        };

    for ent in &entities {
        match ent {
            IgesEntity::Line { p1, p2 } => {
                update_bounds(project(*p1), &mut min_x, &mut max_x, &mut min_y, &mut max_y);
                update_bounds(project(*p2), &mut min_x, &mut max_x, &mut min_y, &mut max_y);
            }
            IgesEntity::Arc {
                center,
                radius,
                start,
                end,
            } => {
                let is_closed = (start.x - end.x).hypot(start.y - end.y) < 1e-6;
                let (a_start, a_end) = if is_closed {
                    (0.0, std::f64::consts::TAU)
                } else {
                    let a1 = (start.y - center.y).atan2(start.x - center.x);
                    let mut a2 = (end.y - center.y).atan2(end.x - center.x);
                    if a2 <= a1 {
                        a2 += std::f64::consts::TAU;
                    }
                    (a1, a2)
                };
                let steps = 24;
                for step in 0..=steps {
                    let a = a_start + (step as f64 / steps as f64) * (a_end - a_start);
                    let pt = Vec3 {
                        x: center.x + radius * a.cos(),
                        y: center.y + radius * a.sin(),
                        z: center.z,
                    };
                    update_bounds(project(pt), &mut min_x, &mut max_x, &mut min_y, &mut max_y);
                }
            }
            IgesEntity::Polyline { points } => {
                for pt in points {
                    update_bounds(project(*pt), &mut min_x, &mut max_x, &mut min_y, &mut max_y);
                }
            }
            IgesEntity::Point { p } => {
                update_bounds(project(*p), &mut min_x, &mut max_x, &mut min_y, &mut max_y);
            }
            IgesEntity::Text { origin, .. } => {
                update_bounds(
                    project(*origin),
                    &mut min_x,
                    &mut max_x,
                    &mut min_y,
                    &mut max_y,
                );
            }
        }
    }

    let dx = (max_x - min_x).max(1e-4);
    let dy = (max_y - min_y).max(1e-4);
    let scale = (TARGET_PAGE_LONG_EDGE - 100.0) / dx.max(dy);
    let margin = 50.0;
    let width = (dx * scale + margin * 2.0).max(MIN_PAGE_DIMENSION);
    let height = (dy * scale + margin * 2.0).max(MIN_PAGE_DIMENSION);

    let mut page = Page::new(1, width, height, "iges-cad");

    for (i, ent) in entities.iter().enumerate() {
        let d = match ent {
            IgesEntity::Line { p1, p2 } => {
                let s1 = project(*p1);
                let s2 = project(*p2);
                let x1 = (s1.0 - min_x) * scale + margin;
                let y1 = (s1.1 - min_y) * scale + margin;
                let x2 = (s2.0 - min_x) * scale + margin;
                let y2 = (s2.1 - min_y) * scale + margin;
                format!("M {x1:.2},{y1:.2} L {x2:.2},{y2:.2}")
            }
            IgesEntity::Arc {
                center,
                radius,
                start,
                end,
            } => {
                let is_closed = (start.x - end.x).hypot(start.y - end.y) < 1e-6;
                let (a_start, a_end) = if is_closed {
                    (0.0, std::f64::consts::TAU)
                } else {
                    let a1 = (start.y - center.y).atan2(start.x - center.x);
                    let mut a2 = (end.y - center.y).atan2(end.x - center.x);
                    if a2 <= a1 {
                        a2 += std::f64::consts::TAU;
                    }
                    (a1, a2)
                };
                let steps = 24;
                let mut path_str = String::new();
                for step in 0..=steps {
                    let a = a_start + (step as f64 / steps as f64) * (a_end - a_start);
                    let pt = Vec3 {
                        x: center.x + radius * a.cos(),
                        y: center.y + radius * a.sin(),
                        z: center.z,
                    };
                    let sp = project(pt);
                    let x = (sp.0 - min_x) * scale + margin;
                    let y = (sp.1 - min_y) * scale + margin;
                    if step == 0 {
                        path_str.push_str(&format!("M {x:.2},{y:.2}"));
                    } else {
                        path_str.push_str(&format!(" L {x:.2},{y:.2}"));
                    }
                }
                if is_closed {
                    path_str.push_str(" Z");
                }
                path_str
            }
            IgesEntity::Polyline { points } => {
                let mut path_str = String::new();
                for (step, pt) in points.iter().enumerate() {
                    let sp = project(*pt);
                    let x = (sp.0 - min_x) * scale + margin;
                    let y = (sp.1 - min_y) * scale + margin;
                    if step == 0 {
                        path_str.push_str(&format!("M {x:.2},{y:.2}"));
                    } else {
                        path_str.push_str(&format!(" L {x:.2},{y:.2}"));
                    }
                }
                path_str
            }
            IgesEntity::Point { p } => {
                let sp = project(*p);
                let x = (sp.0 - min_x) * scale + margin;
                let y = (sp.1 - min_y) * scale + margin;
                let h = IGES_POINT_MARKER_HALF_SIZE;
                format!(
                    "M {:.2},{:.2} L {:.2},{:.2} M {:.2},{:.2} L {:.2},{:.2}",
                    x - h,
                    y,
                    x + h,
                    y,
                    x,
                    y - h,
                    x,
                    y + h
                )
            }
            IgesEntity::Text {
                origin,
                height,
                rotation,
                text,
            } => {
                let sp = project(*origin);
                let x = (sp.0 - min_x) * scale + margin;
                let y = (sp.1 - min_y) * scale + margin;
                let font_size = (height * scale).clamp(6.0, 100.0);
                // The page's Y axis points down, so a counter-clockwise
                // model rotation becomes a clockwise page rotation.
                let (sin, cos) = (-rotation).sin_cos();
                let transform = if rotation.abs() > 1e-9 {
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
                page.nodes.push(Node::Text {
                    id: format!("iges_{i}"),
                    x,
                    y,
                    runs: vec![TextRun {
                        text: text.clone(),
                        font_family: "sans-serif, Arial, 'Hiragino Sans'".into(),
                        font_size,
                        bold: false,
                        italic: false,
                        fill: Paint::solid("#2563eb"),
                        baseline_shift: 0.0,
                        glyph_x_offsets: Vec::new(),
                        target_advance: None,
                    }],
                    anchor: TextAnchor::Start,
                    transform,
                    opacity: 1.0,
                    stroke: Stroke::default(),
                    clip_id: None,
                    meta: SourceMeta {
                        kind: "iges:note".into(),
                        ..Default::default()
                    },
                });
                String::new()
            }
        };

        if !d.is_empty() {
            page.nodes.push(Node::Path {
                id: format!("iges_{i}"),
                d,
                fill_rule: "evenodd".into(),
                fill: Paint::None,
                stroke: Stroke {
                    paint: Paint::solid("#2563eb"),
                    width: 1.0,
                    line_cap: LineCap::Round,
                    line_join: LineJoin::Round,
                    ..Default::default()
                },
                transform: IDENTITY,
                clip_id: None,
                meta: SourceMeta {
                    kind: "iges:wireframe".into(),
                    ..Default::default()
                },
            });
        }
    }

    sink.consume(page)?;
    Ok(Vec::new())
}

/// One directory-entry record (two 80-column `D` lines).
struct IgesDirEntry {
    /// Sequence number of the record's first `D` line (what pointers in
    /// other records, such as field 7, refer to).
    seq: usize,
    entity_type: usize,
    pd_pointer: usize,
    /// Field 7: sequence number of an entity-124 transformation matrix
    /// record, or 0.
    transform_pointer: usize,
    /// Field 15 (second line): entity form number.
    form: i64,
}

fn parse_iges(text: &str) -> Result<Vec<IgesEntity>> {
    let mut dir_entries: Vec<IgesDirEntry> = Vec::new();
    let mut pd_lines: HashMap<usize, String> = HashMap::new();

    fn field(line: &str, index: usize) -> &str {
        let start = (index - 1) * 8;
        line.get(start..start + 8).unwrap_or("").trim()
    }

    for line in text.lines() {
        if line.len() < 73 {
            continue;
        }
        let section = &line[72..73];
        let seq_str = line[73..].trim();
        let seq_num = seq_str.parse::<usize>().unwrap_or(0);

        match section {
            "D" => {
                if seq_num % 2 == 1 {
                    if let (Ok(entity_type), Ok(pd_pointer)) = (
                        field(line, 1).parse::<usize>(),
                        field(line, 2).parse::<usize>(),
                    ) {
                        dir_entries.push(IgesDirEntry {
                            seq: seq_num,
                            entity_type,
                            pd_pointer,
                            transform_pointer: field(line, 7).parse().unwrap_or(0),
                            form: 0,
                        });
                    }
                } else if let Some(entry) = dir_entries.last_mut()
                    && entry.seq + 1 == seq_num
                {
                    entry.form = field(line, 15).parse().unwrap_or(0);
                }
            }
            "P" => {
                let data = line[..72].to_string();
                pd_lines.insert(seq_num, data);
            }
            _ => {}
        }
    }

    // Raw parameter text of one record, joined across its continuation lines.
    let pd_text = |pd_pointer: usize| -> String {
        let mut pd_data = String::new();
        let mut cur = pd_pointer;
        while let Some(line) = pd_lines.get(&cur) {
            pd_data.push_str(line);
            if line.contains(';') {
                break;
            }
            cur += 1;
        }
        pd_data
    };
    let numeric_params = |pd_data: &str| -> Vec<f64> {
        let trimmed = pd_data
            .trim()
            .trim_end_matches(';')
            .replace(['D', 'd'], "E");
        trimmed
            .split(',')
            .map(|s| {
                let clean = s.trim().trim_end_matches(';');
                if clean.is_empty() {
                    0.0
                } else {
                    crate::cad::dxf::geometry::parse_cad_float(clean).unwrap_or(0.0)
                }
            })
            .collect()
    };

    // Entity 124 records first, keyed by their DE sequence number, so any
    // entity's field-7 pointer (and a matrix's own pointer, for nesting)
    // can be resolved regardless of file order.
    let mut matrices: HashMap<usize, (IgesTransform, usize)> = HashMap::new();
    for entry in dir_entries.iter().filter(|e| e.entity_type == 124) {
        let params = numeric_params(&pd_text(entry.pd_pointer));
        if params.len() >= 13 && params[1..13].iter().all(|v| v.is_finite()) {
            let m = &params[1..13];
            let transform = IgesTransform {
                r: [[m[0], m[1], m[2]], [m[4], m[5], m[6]], [m[8], m[9], m[10]]],
                t: [m[3], m[7], m[11]],
            };
            matrices.insert(entry.seq, (transform, entry.transform_pointer));
        }
    }
    let resolve_transform = |mut pointer: usize| -> IgesTransform {
        let mut total = IgesTransform::IDENTITY;
        let mut depth = 0;
        while pointer != 0 && depth < MAX_IGES_TRANSFORM_DEPTH {
            let Some((m, parent)) = matrices.get(&pointer) else {
                break;
            };
            // An outer matrix applies after the inner one already folded in.
            total = m.then(&total);
            pointer = *parent;
            depth += 1;
        }
        total
    };

    let mut entities = Vec::new();

    for entry in &dir_entries {
        if entities.len() >= MAX_IGES_ENTITIES {
            break;
        }
        let etype = entry.entity_type;
        let pd_data = pd_text(entry.pd_pointer);
        let transform = resolve_transform(entry.transform_pointer);
        let before = entities.len();

        if etype == 212 {
            parse_iges_212(&split_iges_params_raw(&pd_data), &mut entities);
            for ent in &mut entities[before..] {
                apply_iges_transform(ent, &transform);
            }
            continue;
        }

        let params = numeric_params(&pd_data);

        match etype {
            110 => {
                // Line: 110, X1, Y1, Z1, X2, Y2, Z2
                if params.len() >= 7 {
                    entities.push(IgesEntity::Line {
                        p1: Vec3 {
                            x: params[1],
                            y: params[2],
                            z: params[3],
                        },
                        p2: Vec3 {
                            x: params[4],
                            y: params[5],
                            z: params[6],
                        },
                    });
                }
            }
            100 => {
                // Circular Arc: 100, ZT, X1, Y1, X2, Y2, X3, Y3
                if params.len() >= 7 {
                    let z = params[1];
                    let cx = params[2];
                    let cy = params[3];
                    let sx = params[4];
                    let sy = params[5];
                    let ex = if params.len() >= 8 { params[6] } else { sx };
                    let ey = if params.len() >= 9 { params[7] } else { sy };
                    let r = ((sx - cx).powi(2) + (sy - cy).powi(2)).sqrt();
                    entities.push(IgesEntity::Arc {
                        center: Vec3 { x: cx, y: cy, z },
                        radius: r,
                        start: Vec3 { x: sx, y: sy, z },
                        end: Vec3 { x: ex, y: ey, z },
                    });
                }
            }
            104 => {
                if let Some(points) = parse_iges_104(&params, entry.form) {
                    entities.push(IgesEntity::Polyline { points });
                }
            }
            106 => {
                // Copious Data: 106, IP, N, ...
                if params.len() >= 6 {
                    let ip = params[1].round() as i32;
                    let mut pts = Vec::new();
                    if ip == 2 {
                        // Form 2: Planar curve, shared Z: 106, 2, N, ZT, X1, Y1, X2, Y2, ...
                        let z = params[3];
                        let mut idx = 4;
                        while idx + 1 < params.len() {
                            pts.push(Vec3 {
                                x: params[idx],
                                y: params[idx + 1],
                                z,
                            });
                            idx += 2;
                        }
                    } else {
                        // Form 1: Data points in R^3: 106, 1, N, X1, Y1, Z1, ...
                        let mut idx = 3;
                        while idx + 2 < params.len() {
                            pts.push(Vec3 {
                                x: params[idx],
                                y: params[idx + 1],
                                z: params[idx + 2],
                            });
                            idx += 3;
                        }
                    }
                    if pts.len() >= 2 {
                        entities.push(IgesEntity::Polyline { points: pts });
                    }
                }
            }
            112 => {
                if let Some(points) = parse_iges_112(&params) {
                    entities.push(IgesEntity::Polyline { points });
                }
            }
            116 => {
                // Point: 116, X, Y, Z, PTR (display symbol subfigure, unused)
                if params.len() >= 4 && params[1..4].iter().all(|v| v.is_finite()) {
                    entities.push(IgesEntity::Point {
                        p: Vec3 {
                            x: params[1],
                            y: params[2],
                            z: params[3],
                        },
                    });
                }
            }
            126 => {
                // Rational B-Spline Curve:
                // 126, K, M, PROP1, PROP2, PROP3, PROP4,
                //      T(-M)..T(N+M),        (K+M+2 knots)
                //      W(0)..W(K),           (K+1 weights)
                //      X(0),Y(0),Z(0)..X(K),Y(K),Z(K),  (3*(K+1) coords)
                //      V(0), V(1)            (parameter domain)
                //      [, XNORM, YNORM, ZNORM]          (optional, ignored)
                if let Some(curve) = parse_iges_126(&params) {
                    let (v0, v1) = curve.domain;
                    let pts = curve
                        .curve
                        .tessellate_domain(v0, v1, IGES_SPLINE_TESSELLATION_STEPS);
                    if pts.len() >= 2 {
                        entities.push(IgesEntity::Polyline {
                            points: pts
                                .into_iter()
                                .map(|p| Vec3 {
                                    x: p.x,
                                    y: p.y,
                                    z: p.z,
                                })
                                .collect(),
                        });
                    }
                }
            }
            _ => {}
        }

        for ent in &mut entities[before..] {
            apply_iges_transform(ent, &transform);
        }
    }

    Ok(entities)
}

/// Moves an entity from its definition space into model space through its
/// directory entry's (possibly chained) entity-124 matrix. A circular arc
/// is defined in a Z = ZT plane; under a rotation it generally leaves the
/// XY-parallel planes this reader's arc drawing assumes, so it is
/// tessellated into a polyline first. Identity transforms are a no-op.
fn apply_iges_transform(entity: &mut IgesEntity, m: &IgesTransform) {
    if m.is_identity() {
        return;
    }
    match entity {
        IgesEntity::Line { p1, p2 } => {
            *p1 = m.apply(*p1);
            *p2 = m.apply(*p2);
        }
        IgesEntity::Arc {
            center,
            radius,
            start,
            end,
        } => {
            let points = tessellate_iges_arc(*center, *radius, *start, *end)
                .into_iter()
                .map(|p| m.apply(p))
                .collect();
            *entity = IgesEntity::Polyline { points };
        }
        IgesEntity::Polyline { points } => {
            for p in points.iter_mut() {
                *p = m.apply(*p);
            }
        }
        IgesEntity::Point { p } => *p = m.apply(*p),
        IgesEntity::Text {
            origin, rotation, ..
        } => {
            // Carry the baseline direction through the rotation part only.
            let (sin, cos) = rotation.sin_cos();
            let dir = m.apply(Vec3 {
                x: cos,
                y: sin,
                z: 0.0,
            });
            let base = m.apply(Vec3 {
                x: 0.0,
                y: 0.0,
                z: 0.0,
            });
            *rotation = (dir.y - base.y).atan2(dir.x - base.x);
            *origin = m.apply(*origin);
        }
    }
}

/// Samples an entity-100 arc (counter-clockwise from `start` to `end`
/// about `center`, full circle when they coincide) into points.
fn tessellate_iges_arc(center: Vec3, radius: f64, start: Vec3, end: Vec3) -> Vec<Vec3> {
    let is_closed = (start.x - end.x).hypot(start.y - end.y) < 1e-6;
    let (a_start, a_end) = if is_closed {
        (0.0, std::f64::consts::TAU)
    } else {
        let a1 = (start.y - center.y).atan2(start.x - center.x);
        let mut a2 = (end.y - center.y).atan2(end.x - center.x);
        if a2 <= a1 {
            a2 += std::f64::consts::TAU;
        }
        (a1, a2)
    };
    let steps = 24;
    (0..=steps)
        .map(|step| {
            let a = a_start + (step as f64 / steps as f64) * (a_end - a_start);
            Vec3 {
                x: center.x + radius * a.cos(),
                y: center.y + radius * a.sin(),
                z: center.z,
            }
        })
        .collect()
}

/// Entity 104 (Conic Arc): `A x² + B xy + C y² + D x + E y + F = 0` in the
/// Z = ZT definition plane, from `(X1, Y1)` to `(X2, Y2)`. IGES requires
/// the conic in standard position (`B = D = E = 0`); the form number says
/// ellipse (1), hyperbola (2) or parabola (3), and form 0 is classified
/// from the coefficients. An ellipse is sampled by angle (counter-clockwise
/// from start to end); a hyperbola or parabola is sampled along whichever
/// axis its endpoints span more, solving the conic for the other
/// coordinate and keeping the root continuous with the previous sample.
/// Returns `None` for truncated, non-finite or degenerate data.
fn parse_iges_104(params: &[f64], form: i64) -> Option<Vec<Vec3>> {
    if params.len() < 12 || !params[1..12].iter().all(|v| v.is_finite()) {
        return None;
    }
    let (a, b, c, d, e, f) = (
        params[1], params[2], params[3], params[4], params[5], params[6],
    );
    let z = params[7];
    let (x1, y1, x2, y2) = (params[8], params[9], params[10], params[11]);
    let kind = match form {
        1 => 1,
        2 => 2,
        3 => 3,
        _ => {
            let disc = b * b - 4.0 * a * c;
            if disc.abs() < 1e-12 {
                3
            } else if disc < 0.0 {
                1
            } else {
                2
            }
        }
    };
    let steps = IGES_CONIC_TESSELLATION_STEPS;
    if kind == 1 {
        if !(a > 0.0 && c > 0.0 && f < 0.0) {
            return None;
        }
        let ra = (-f / a).sqrt();
        let rb = (-f / c).sqrt();
        let a1 = (y1 / rb).atan2(x1 / ra);
        let mut a2 = (y2 / rb).atan2(x2 / ra);
        let full = (x1 - x2).hypot(y1 - y2) < 1e-9;
        if full {
            a2 = a1 + std::f64::consts::TAU;
        } else if a2 <= a1 {
            a2 += std::f64::consts::TAU;
        }
        return Some(
            (0..=steps)
                .map(|i| {
                    let t = a1 + (i as f64 / steps as f64) * (a2 - a1);
                    Vec3 {
                        x: ra * t.cos(),
                        y: rb * t.sin(),
                        z,
                    }
                })
                .collect(),
        );
    }
    // Hyperbola / parabola: walk the coordinate with the larger endpoint
    // span and solve the conic for the other one.
    let along_x = (x2 - x1).abs() >= (y2 - y1).abs();
    if (along_x && (x2 - x1).abs() < 1e-12) || (!along_x && (y2 - y1).abs() < 1e-12) {
        return None;
    }
    let mut points = Vec::with_capacity(steps + 1);
    let mut prev_other = if along_x { y1 } else { x1 };
    for i in 0..=steps {
        let s = i as f64 / steps as f64;
        let (known, expected) = if along_x {
            (x1 + s * (x2 - x1), y1 + s * (y2 - y1))
        } else {
            (y1 + s * (y2 - y1), x1 + s * (x2 - x1))
        };
        // Quadratic in the unknown coordinate: qa u² + qb u + qc = 0.
        let (qa, qb, qc) = if along_x {
            (c, b * known + e, a * known * known + d * known + f)
        } else {
            (a, b * known + d, c * known * known + e * known + f)
        };
        let other = if qa.abs() < 1e-12 {
            if qb.abs() < 1e-12 {
                return None;
            }
            -qc / qb
        } else {
            let disc = qb * qb - 4.0 * qa * qc;
            if disc < -1e-9 {
                return None;
            }
            let root = disc.max(0.0).sqrt();
            let u1 = (-qb + root) / (2.0 * qa);
            let u2 = (-qb - root) / (2.0 * qa);
            let target = if i == 0 { expected } else { prev_other };
            if (u1 - target).abs() <= (u2 - target).abs() {
                u1
            } else {
                u2
            }
        };
        prev_other = other;
        let (x, y) = if along_x {
            (known, other)
        } else {
            (other, known)
        };
        points.push(Vec3 { x, y, z });
    }
    // Snap the ends onto the declared endpoints.
    if let Some(first) = points.first_mut() {
        *first = Vec3 { x: x1, y: y1, z };
    }
    if let Some(last) = points.last_mut() {
        *last = Vec3 { x: x2, y: y2, z };
    }
    Some(points)
}

/// Entity 112 (Parametric Spline Curve): `112, CTYPE, H, NDIM, N,
/// T(1..N+1), [AX BX CX DX AY BY CY DY AZ BZ CZ DZ] × N, [terminating
/// point values × 12]`. Each polynomial segment `i` is
/// `A + B s + C s² + D s³` for `s = t - T(i)`, `0 ≤ s ≤ T(i+1) - T(i)`, and
/// is sampled at a bounded number of steps. Returns `None` for truncated or
/// non-finite data.
fn parse_iges_112(params: &[f64]) -> Option<Vec<Vec3>> {
    if params.len() < 5 {
        return None;
    }
    let n = params[4];
    if !n.is_finite() || n < 1.0 || n > MAX_IGES_SPLINE_SEGMENTS as f64 {
        return None;
    }
    let n = n.round() as usize;
    let breaks_start = 5;
    let coeff_start = breaks_start + n + 1;
    let coeff_end = coeff_start + 12 * n;
    if params.len() < coeff_end
        || !params[breaks_start..coeff_end]
            .iter()
            .all(|v| v.is_finite())
    {
        return None;
    }
    let breaks = &params[breaks_start..coeff_start];
    let mut points = Vec::with_capacity(n * IGES_SPLINE_SEGMENT_STEPS + 1);
    for seg in 0..n {
        let span = breaks[seg + 1] - breaks[seg];
        let c = &params[coeff_start + 12 * seg..coeff_start + 12 * seg + 12];
        let eval = |s: f64| Vec3 {
            x: c[0] + c[1] * s + c[2] * s * s + c[3] * s * s * s,
            y: c[4] + c[5] * s + c[6] * s * s + c[7] * s * s * s,
            z: c[8] + c[9] * s + c[10] * s * s + c[11] * s * s * s,
        };
        let first = if seg == 0 { 0 } else { 1 };
        for i in first..=IGES_SPLINE_SEGMENT_STEPS {
            let s = span * (i as f64 / IGES_SPLINE_SEGMENT_STEPS as f64);
            points.push(eval(s));
        }
    }
    (points.len() >= 2).then_some(points)
}

/// Splits one record's parameter text into raw string fields, honouring
/// Hollerith constants (`nHtext`, whose `text` may itself contain the `,`
/// and `;` delimiters). The terminating `;` ends the list.
fn split_iges_params_raw(pd_data: &str) -> Vec<String> {
    let chars: Vec<char> = pd_data.chars().collect();
    let mut fields = Vec::new();
    let mut current = String::new();
    let mut i = 0;
    while i < chars.len() {
        let ch = chars[i];
        // A Hollerith field starts with digits immediately followed by 'H'.
        if current.trim().is_empty() && ch.is_ascii_digit() {
            let mut j = i;
            while j < chars.len() && chars[j].is_ascii_digit() {
                j += 1;
            }
            if j < chars.len() && chars[j] == 'H' {
                let count: usize = chars[i..j].iter().collect::<String>().parse().unwrap_or(0);
                let start = j + 1;
                let end = (start + count).min(chars.len());
                fields.push(chars[start..end].iter().collect::<String>());
                current.clear();
                i = end;
                // Skip to the delimiter that follows the Hollerith text.
                while i < chars.len() && chars[i] != ',' && chars[i] != ';' {
                    i += 1;
                }
                if i < chars.len() && chars[i] == ';' {
                    return fields;
                }
                i += 1;
                continue;
            }
        }
        match ch {
            ',' => {
                fields.push(current.trim().to_string());
                current.clear();
            }
            ';' => {
                fields.push(current.trim().to_string());
                return fields;
            }
            _ => current.push(ch),
        }
        i += 1;
    }
    if !current.trim().is_empty() {
        fields.push(current.trim().to_string());
    }
    fields
}

/// Entity 212 (General Note): `212, NS, [NC, WT, HT, FC, SL, A, M, VH, XS,
/// YS, ZS, TEXT] × NS`. Each string becomes one [`IgesEntity::Text`] at its
/// own origin, using its box height and rotation angle; the font code,
/// slant, mirror and vertical/horizontal flags are not applied.
fn parse_iges_212(fields: &[String], entities: &mut Vec<IgesEntity>) {
    let num = |s: &str| -> Option<f64> {
        crate::cad::dxf::geometry::parse_cad_float(&s.replace(['D', 'd'], "E"))
    };
    let Some(count) = fields.get(1).and_then(|s| num(s)) else {
        return;
    };
    if !count.is_finite() || count < 1.0 {
        return;
    }
    let count = (count.round() as usize).min(MAX_IGES_ENTITIES);
    for k in 0..count {
        let base = 2 + k * 12;
        let Some(text) = fields.get(base + 11) else {
            break;
        };
        if text.is_empty() {
            continue;
        }
        let height = fields.get(base + 2).and_then(|s| num(s)).unwrap_or(0.0);
        let rotation = fields.get(base + 5).and_then(|s| num(s)).unwrap_or(0.0);
        let x = fields.get(base + 8).and_then(|s| num(s)).unwrap_or(0.0);
        let y = fields.get(base + 9).and_then(|s| num(s)).unwrap_or(0.0);
        let z = fields.get(base + 10).and_then(|s| num(s)).unwrap_or(0.0);
        if ![height, rotation, x, y, z].iter().all(|v| v.is_finite()) {
            continue;
        }
        entities.push(IgesEntity::Text {
            origin: Vec3 { x, y, z },
            height: if height > 0.0 { height } else { 1.0 },
            rotation,
            text: text.clone(),
        });
    }
}

struct Iges126Curve {
    curve: crate::cad::nurbs::NurbsCurve,
    domain: (f64, f64),
}

/// Parses the IGES entity 126 parameter list (`params[0]` is the literal
/// entity type code `126.0`, matching the convention used elsewhere in this
/// file) into a [`crate::cad::nurbs::NurbsCurve`] plus its declared `V(0)`/`V(1)`
/// parameter domain. Returns `None` for truncated data or a control-point
/// count outside the bounded range this reader accepts.
fn parse_iges_126(params: &[f64]) -> Option<Iges126Curve> {
    if params.len() < 7 {
        return None;
    }
    let k = params[1];
    let m = params[2];
    if !k.is_finite()
        || !m.is_finite()
        || k < 1.0
        || m < 1.0
        || k > MAX_IGES_SPLINE_CONTROL_POINTS as f64 - 1.0
    {
        return None;
    }
    let k = k.round() as usize;
    let m = m.round() as usize;
    if m > k {
        return None;
    }
    let num_cp = k + 1;
    let num_knots = k + m + 2;

    let knots_start = 7;
    let weights_start = knots_start + num_knots;
    let cp_start = weights_start + num_cp;
    let cp_end = cp_start + 3 * num_cp;
    if params.len() < cp_end + 2 {
        return None;
    }

    let knots = params[knots_start..weights_start].to_vec();
    let weights = params[weights_start..cp_start].to_vec();
    let control_points = (0..num_cp)
        .map(|i| {
            let base = cp_start + i * 3;
            crate::cad::nurbs::Point3 {
                x: params[base],
                y: params[base + 1],
                z: params[base + 2],
            }
        })
        .collect();
    let domain = (params[cp_end], params[cp_end + 1]);

    let curve = crate::cad::nurbs::NurbsCurve {
        degree: m,
        control_points,
        weights,
        knots,
    };
    if !curve.is_valid() {
        return None;
    }
    Some(Iges126Curve { curve, domain })
}

/// Converts SVG elements to standard ANSI IGES 5.3 ASCII CAD file (.igs / .iges).
pub fn write_svg_to_iges<W: std::io::Write>(svg_content: &str, mut writer: W) -> Result<()> {
    let doc = crate::cad::svg_reader::parse_svg_elements(svg_content)?;

    let mut iges_entities: Vec<IgesEntity> = Vec::new();

    for elem in doc.elements {
        match elem {
            crate::cad::svg_reader::SvgElement::Line { p1, p2, .. } => {
                iges_entities.push(IgesEntity::Line {
                    p1: Vec3 {
                        x: p1.x,
                        y: p1.y,
                        z: 0.0,
                    },
                    p2: Vec3 {
                        x: p2.x,
                        y: p2.y,
                        z: 0.0,
                    },
                });
            }
            crate::cad::svg_reader::SvgElement::Circle { center, radius, .. } => {
                let p = Vec3 {
                    x: center.x + radius,
                    y: center.y,
                    z: 0.0,
                };
                iges_entities.push(IgesEntity::Arc {
                    center: Vec3 {
                        x: center.x,
                        y: center.y,
                        z: 0.0,
                    },
                    radius,
                    start: p,
                    end: p,
                });
            }
            crate::cad::svg_reader::SvgElement::Rect {
                x,
                y,
                width,
                height,
                ..
            } => {
                iges_entities.push(IgesEntity::Polyline {
                    points: vec![
                        Vec3 { x, y, z: 0.0 },
                        Vec3 {
                            x: x + width,
                            y,
                            z: 0.0,
                        },
                        Vec3 {
                            x: x + width,
                            y: y + height,
                            z: 0.0,
                        },
                        Vec3 {
                            x,
                            y: y + height,
                            z: 0.0,
                        },
                        Vec3 { x, y, z: 0.0 },
                    ],
                });
            }
            crate::cad::svg_reader::SvgElement::Polyline {
                points, is_closed, ..
            } => {
                if points.len() >= 2 {
                    let mut pts: Vec<Vec3> = points
                        .into_iter()
                        .map(|p| Vec3 {
                            x: p.x,
                            y: p.y,
                            z: 0.0,
                        })
                        .collect();
                    if is_closed
                        && pts.first().map(|p| (p.x, p.y)) != pts.last().map(|p| (p.x, p.y))
                    {
                        let first = pts[0];
                        pts.push(first);
                    }
                    iges_entities.push(IgesEntity::Polyline { points: pts });
                }
            }
            _ => {}
        }
    }

    if iges_entities.is_empty() {
        iges_entities.push(IgesEntity::Line {
            p1: Vec3 {
                x: 0.0,
                y: 0.0,
                z: 0.0,
            },
            p2: Vec3 {
                x: 100.0,
                y: 100.0,
                z: 0.0,
            },
        });
    }

    // Format fixed 80-column records: 72 chars data + 1 char section + 7 chars line index
    let format_record = |data: &str, section: char, index: usize| -> String {
        format!("{:<72}{}{:>7}\n", data, section, index)
    };

    let mut s_count = 0;
    let mut g_count = 0;
    let mut d_count = 0;
    let mut p_count = 0;

    // Start Section
    s_count += 1;
    write!(
        writer,
        "{}",
        format_record("document-svg exported IGES 5.3 CAD file", 'S', s_count)
    )?;

    // Global Section
    g_count += 1;
    write!(
        writer,
        "{}",
        format_record(
            "1H,,1H;,4HDOC1,13Hdocument-svg,13Hdocument-svg,16,38,6,308,15,4HDOC1,1.0,1,",
            'G',
            g_count
        )
    )?;
    g_count += 1;
    write!(
        writer,
        "{}",
        format_record(
            "2HMM,1,0.0,15H20260911.230000,0.001,0.0,8HAuthor,12HOrganization,11,0;",
            'G',
            g_count
        )
    )?;

    // Prepare DE and PD records
    let mut de_records = Vec::new();
    let mut pd_records = Vec::new();

    for ent in &iges_entities {
        let pd_ptr = p_count + 1;
        match ent {
            IgesEntity::Line { p1, p2 } => {
                let param_str = format!(
                    "110,{:.4},{:.4},{:.4},{:.4},{:.4},{:.4};",
                    p1.x, p1.y, p1.z, p2.x, p2.y, p2.z
                );
                p_count += 1;
                pd_records.push(format_record(&param_str, 'P', p_count));

                d_count += 1;
                de_records.push(format_record(
                    &format!(
                        "     110{:>8}       0       1       0       0       0       000000001",
                        pd_ptr
                    ),
                    'D',
                    d_count,
                ));
                d_count += 1;
                de_records.push(format_record(
                    "     110       0       0       1       0                               0",
                    'D',
                    d_count,
                ));
            }
            IgesEntity::Arc {
                center, start, end, ..
            } => {
                let param_str = format!(
                    "100,{:.4},{:.4},{:.4},{:.4},{:.4},{:.4},{:.4};",
                    center.z, center.x, center.y, start.x, start.y, end.x, end.y
                );
                p_count += 1;
                pd_records.push(format_record(&param_str, 'P', p_count));

                d_count += 1;
                de_records.push(format_record(
                    &format!(
                        "     100{:>8}       0       1       0       0       0       000000001",
                        pd_ptr
                    ),
                    'D',
                    d_count,
                ));
                d_count += 1;
                de_records.push(format_record(
                    "     100       0       0       1       0                               0",
                    'D',
                    d_count,
                ));
            }
            IgesEntity::Polyline { points } => {
                let mut param_str = format!("106,1,{}", points.len());
                for pt in points {
                    param_str.push_str(&format!(",{:.4},{:.4},{:.4}", pt.x, pt.y, pt.z));
                }
                param_str.push(';');

                let chunks: Vec<String> = param_str
                    .as_bytes()
                    .chunks(64)
                    .map(|c| String::from_utf8_lossy(c).to_string())
                    .collect();

                let lines_in_pd = chunks.len();
                for chunk in chunks {
                    p_count += 1;
                    pd_records.push(format_record(&chunk, 'P', p_count));
                }

                d_count += 1;
                de_records.push(format_record(
                    &format!(
                        "     106{:>8}       0       1       0       0       0       000000001",
                        pd_ptr
                    ),
                    'D',
                    d_count,
                ));
                d_count += 1;
                de_records.push(format_record(
                    &format!(
                        "     106       0       0{:>8}       0                               0",
                        lines_in_pd
                    ),
                    'D',
                    d_count,
                ));
            }
            // The SVG-to-IGES writer only produces wireframe entities; points
            // and notes are read-side only.
            IgesEntity::Point { .. } | IgesEntity::Text { .. } => {}
        }
    }

    // Write Directory Entry Section
    for rec in de_records {
        write!(writer, "{rec}")?;
    }

    // Write Parameter Data Section
    for rec in pd_records {
        write!(writer, "{rec}")?;
    }

    // Terminate Section (T)
    let term_str = format!(
        "S{:>7}G{:>7}D{:>7}P{:>7}",
        s_count, g_count, d_count, p_count
    );
    write!(writer, "{}", format_record(&term_str, 'T', 1))?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_and_writes_iges_roundtrip() {
        let mut page = Page::new(1, 100.0, 100.0, "test");
        page.nodes.push(Node::Path {
            id: String::new(),
            d: "M 10,20 L 50,60".to_string(),
            fill_rule: String::new(),
            fill: Paint::None,
            stroke: Stroke::default(),
            transform: IDENTITY,
            clip_id: None,
            meta: SourceMeta::default(),
        });
        let mut svg_bytes = Vec::new();
        crate::svg::write_page(&page, &mut svg_bytes, crate::svg::SvgOptions::default()).unwrap();
        let svg_str = String::from_utf8(svg_bytes).unwrap();
        let mut iges_bytes = Vec::new();
        write_svg_to_iges(&svg_str, &mut iges_bytes).expect("write iges");
        assert!(!iges_bytes.is_empty());

        let iges_str = String::from_utf8(iges_bytes).expect("valid utf8 iges");
        let entities = parse_iges(&iges_str).expect("parse iges");
        assert!(!entities.is_empty());
    }

    #[test]
    fn parses_iges_with_non_utf8_start_section() {
        let mut page = Page::new(1, 400.0, 400.0, "test");
        page.nodes.push(Node::Path {
            id: String::new(),
            d: "M 0,0 L 100,100".to_string(),
            fill_rule: String::new(),
            fill: Paint::None,
            stroke: Stroke::default(),
            transform: IDENTITY,
            clip_id: None,
            meta: SourceMeta::default(),
        });

        let mut svg_bytes = Vec::new();
        crate::svg::write_page(&page, &mut svg_bytes, crate::svg::SvgOptions::default()).unwrap();
        let svg_str = String::from_utf8(svg_bytes).unwrap();
        let mut iges_bytes = Vec::new();
        write_svg_to_iges(&svg_str, &mut iges_bytes).expect("write iges");

        // Inject non-UTF8 Latin-1 byte (0xB0 for °) in Start section
        if let Some(pos) = iges_bytes.iter().position(|&b| b == b'e') {
            iges_bytes[pos] = 0xB0;
        }

        let text = match String::from_utf8(iges_bytes.clone()) {
            Ok(s) => s,
            Err(e) => e.into_bytes().iter().map(|&b| b as char).collect(),
        };
        let entities = parse_iges(&text).expect("parse non-utf8 iges");
        assert!(!entities.is_empty());
    }

    #[test]
    fn parses_entity_126_rational_b_spline_curve() {
        // Degree-1 (piecewise linear), 3-control-point clamped B-spline
        // through (0,0,0) -> (10,0,0) -> (10,10,0), unit weights.
        let param_str = "126,2,1,0,0,1,0,0,0,1,2,2,1,1,1,0,0,0,10,0,0,10,10,0,0,2;".to_string();
        let de_line1 = format!("{:<72}D{:>7}\n", format!("{:<8}{:<8}", "126", "1"), 1);
        let de_line2 = format!("{:<72}D{:>7}\n", "126", 2);
        let p_line = format!("{:<72}P{:>7}\n", param_str, 1);
        let text = format!("{de_line1}{de_line2}{p_line}");

        let entities = parse_iges(&text).expect("parse iges with entity 126");
        assert_eq!(entities.len(), 1);
        match &entities[0] {
            IgesEntity::Polyline { points } => {
                assert!(points.len() >= 2);
                let first = points.first().unwrap();
                let last = points.last().unwrap();
                assert!((first.x).abs() < 1e-6 && (first.y).abs() < 1e-6);
                assert!((last.x - 10.0).abs() < 1e-6 && (last.y - 10.0).abs() < 1e-6);
            }
            other => panic!("expected tessellated polyline, got {other:?}"),
        }
    }

    /// Builds a minimal IGES text from `(entity_type, form, transform_pointer,
    /// parameter_text)` records; DE sequence numbers are 1, 3, 5, ... in order.
    fn iges_text(records: &[(usize, i64, usize, &str)]) -> String {
        let mut out = String::new();
        let mut d_lines = String::new();
        let mut p_lines = String::new();
        let mut pd_seq = 0;
        for (i, (etype, form, xform, params)) in records.iter().enumerate() {
            let de_seq = 2 * i + 1;
            pd_seq += 1;
            let line1 = format!(
                "{:>8}{:>8}{:>8}{:>8}{:>8}{:>8}{:>8}{:>8}{:>8}",
                etype, pd_seq, 0, 0, 0, 0, xform, 0, "00000000"
            );
            d_lines.push_str(&format!("{line1:<72}D{de_seq:>7}\n"));
            let line2 = format!("{:>8}{:>8}{:>8}{:>8}{:>8}", etype, 0, 0, 1, form);
            d_lines.push_str(&format!("{:<72}D{:>7}\n", line2, de_seq + 1));
            // Long parameter text continues on following P lines, 64
            // columns each, like a real file.
            let chunks: Vec<String> = params
                .as_bytes()
                .chunks(64)
                .map(|c| String::from_utf8_lossy(c).into_owned())
                .collect();
            for (k, chunk) in chunks.iter().enumerate() {
                p_lines.push_str(&format!("{:<72}P{:>7}\n", chunk, pd_seq + k));
            }
            pd_seq += chunks.len() - 1;
        }
        out.push_str(&d_lines);
        out.push_str(&p_lines);
        out
    }

    #[test]
    fn parses_entity_104_ellipse_arc_by_angle() {
        // x²/16 + y²/4 = 1  ->  A=1/16, C=1/4, F=-1, quarter arc (4,0) -> (0,2).
        let text = iges_text(&[(104, 1, 0, "104,0.0625,0,0.25,0,0,-1,0,4,0,0,2;")]);
        let entities = parse_iges(&text).expect("parse");
        assert_eq!(entities.len(), 1);
        let IgesEntity::Polyline { points } = &entities[0] else {
            panic!("expected polyline, got {:?}", entities[0]);
        };
        assert_eq!(points.len(), IGES_CONIC_TESSELLATION_STEPS + 1);
        assert!((points[0].x - 4.0).abs() < 1e-9 && points[0].y.abs() < 1e-9);
        let last = points.last().unwrap();
        assert!(last.x.abs() < 1e-9 && (last.y - 2.0).abs() < 1e-9);
        // Every sample satisfies the ellipse equation.
        for p in points {
            assert!((p.x * p.x / 16.0 + p.y * p.y / 4.0 - 1.0).abs() < 1e-9);
        }
    }

    #[test]
    fn parses_entity_104_parabola_by_walking_its_wider_axis() {
        // y = x²  ->  A=1, E=-1, from (-2,4) to (2,4): sampled along x.
        let text = iges_text(&[(104, 3, 0, "104,1,0,0,0,-1,0,0,-2,4,2,4;")]);
        let entities = parse_iges(&text).expect("parse");
        let IgesEntity::Polyline { points } = &entities[0] else {
            panic!("expected polyline");
        };
        for p in points {
            assert!((p.y - p.x * p.x).abs() < 1e-9, "{p:?} is off the parabola");
        }
        assert!((points[0].x + 2.0).abs() < 1e-9);
        assert!((points.last().unwrap().x - 2.0).abs() < 1e-9);
    }

    #[test]
    fn parses_entity_112_parametric_spline_segments() {
        // Two linear segments: x = s on [0,1] then x = 1 + s on [1,2]; y = 0.
        let params = "112,3,1,3,2,0,1,2,\
0,1,0,0,0,0,0,0,0,0,0,0,\
1,1,0,0,0,0,0,0,0,0,0,0,\
2,0,0,0,0,0,0,0,0,0,0,0;";
        let text = iges_text(&[(112, 0, 0, params)]);
        let entities = parse_iges(&text).expect("parse");
        let IgesEntity::Polyline { points } = &entities[0] else {
            panic!("expected polyline");
        };
        assert_eq!(points.len(), 2 * IGES_SPLINE_SEGMENT_STEPS + 1);
        assert!(points[0].x.abs() < 1e-9);
        assert!((points.last().unwrap().x - 2.0).abs() < 1e-9);
        assert!(points.windows(2).all(|w| w[1].x > w[0].x));
    }

    #[test]
    fn parses_entity_116_point_and_212_general_note_with_hollerith_text() {
        let text = iges_text(&[
            (116, 0, 0, "116,1,2,3,0;"),
            (
                212,
                0,
                0,
                "212,1,8,10,2.5,1,1.5708,0.5,0,0,7,8,0,8HA,B;C DE;",
            ),
        ]);
        let entities = parse_iges(&text).expect("parse");
        assert_eq!(entities.len(), 2);
        assert!(
            matches!(&entities[0], IgesEntity::Point { p } if p.x == 1.0 && p.y == 2.0 && p.z == 3.0)
        );
        match &entities[1] {
            IgesEntity::Text {
                origin,
                height,
                rotation,
                text,
            } => {
                assert_eq!(text, "A,B;C DE");
                assert_eq!((origin.x, origin.y, origin.z), (7.0, 8.0, 0.0));
                assert_eq!(*height, 2.5);
                assert_eq!(*rotation, 0.5);
            }
            other => panic!("expected text, got {other:?}"),
        }
    }

    #[test]
    fn applies_entity_124_transformation_matrix_including_chained_parents() {
        // DE 1: outer matrix, translate +100 in X.
        // DE 3: inner matrix, rotate 90° about Z, parent = DE 1.
        // DE 5: line (0,0,0)-(10,0,0) placed through DE 3.
        // DE 7: arc in the XY plane placed through DE 1 -> becomes a polyline.
        let text = iges_text(&[
            (124, 0, 0, "124,1,0,0,100,0,1,0,0,0,0,1,0;"),
            (124, 0, 1, "124,0,-1,0,0,1,0,0,0,0,0,1,0;"),
            (110, 0, 3, "110,0,0,0,10,0,0;"),
            (100, 0, 1, "100,0,0,0,5,0,5,0;"),
        ]);
        let entities = parse_iges(&text).expect("parse");
        assert_eq!(entities.len(), 2);
        match &entities[0] {
            IgesEntity::Line { p1, p2 } => {
                assert!((p1.x - 100.0).abs() < 1e-9 && p1.y.abs() < 1e-9);
                assert!((p2.x - 100.0).abs() < 1e-9 && (p2.y - 10.0).abs() < 1e-9);
            }
            other => panic!("expected line, got {other:?}"),
        }
        match &entities[1] {
            IgesEntity::Polyline { points } => {
                assert!(
                    points
                        .iter()
                        .all(|p| ((p.x - 100.0).hypot(p.y) - 5.0).abs() < 1e-9)
                );
            }
            other => panic!("expected tessellated arc, got {other:?}"),
        }
    }

    #[test]
    fn split_iges_params_raw_keeps_delimiters_inside_hollerith_strings() {
        let fields = split_iges_params_raw("212,1,3HA,B,5Hx;y,z,7;");
        assert_eq!(fields, vec!["212", "1", "A,B", "x;y,z", "7"]);
    }

    #[test]
    fn rejects_entity_126_with_truncated_parameter_data() {
        // K=2, M=1 declared but the parameter list is cut short; must not
        // panic on out-of-bounds indexing and must simply skip the entity.
        let param_str = "126,2,1,0,0,1,0,0,0,1,2,2;".to_string();
        let de_line1 = format!("{:<72}D{:>7}\n", format!("{:<8}{:<8}", "126", "1"), 1);
        let de_line2 = format!("{:<72}D{:>7}\n", "126", 2);
        let p_line = format!("{:<72}P{:>7}\n", param_str, 1);
        let text = format!("{de_line1}{de_line2}{p_line}");

        let entities = parse_iges(&text).expect("parse without panicking");
        assert!(entities.is_empty());
    }
}
