//! STEP (ISO 10303-21) Part 21 CAD exchange file parser and 3D axonometric projection renderer.
//!
//! Clean-room, zero-dependency parser for standard mechanical STEP models (AP203/AP214/AP242).
//! Extracts 3D vertices, cartesian points, directional vectors, straight lines, circles,
//! and edge curves, rendering them as high-precision isometric engineering wireframe diagrams.

pub mod writer;

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read};

use crate::convert::{ConvertOptions, PageConsumer};
use crate::error::{Error, Result};
use crate::ir::{
    IDENTITY, LineCap, LineJoin, Node, Page, Paint, SourceMeta, Stroke, TextAnchor, TextRun,
};

const TARGET_PAGE_LONG_EDGE: f64 = 1200.0;
const MIN_PAGE_DIMENSION: f64 = 400.0;
const MAX_STEP_ENTITIES: usize = 500_000;

#[derive(Clone, Copy, Debug)]
struct Point3D {
    x: f64,
    y: f64,
    z: f64,
}

#[derive(Clone, Debug)]
#[allow(dead_code)]
enum StepEntity {
    CartesianPoint(Point3D),
    Direction(Point3D),
    Vector {
        dir_id: u64,
        length: f64,
    },
    Line {
        point_id: u64,
        dir_id: u64,
    },
    VertexPoint {
        point_id: u64,
    },
    EdgeCurve {
        start_v: u64,
        end_v: u64,
        curve_id: u64,
    },
    Circle {
        axis_id: u64,
        radius: f64,
    },
    Axis2Placement3D {
        location_id: u64,
        axis_id: Option<u64>,
        ref_dir_id: Option<u64>,
    },
    Polyline(Vec<u64>),
    Ellipse {
        axis_id: u64,
        semi_axis_1: f64,
        semi_axis_2: f64,
    },
    /// Raw (unresolved) `B_SPLINE_CURVE` + `B_SPLINE_CURVE_WITH_KNOTS` (+
    /// optional `RATIONAL_B_SPLINE_CURVE`) complex entity. Control points
    /// are resolved and the curve tessellated later, once every entity in
    /// the file has been parsed, so forward references work like every
    /// other curve type here.
    BSplineCurve {
        degree: usize,
        control_point_ids: Vec<u64>,
        knot_multiplicities: Vec<u64>,
        knot_values: Vec<f64>,
        weights: Option<Vec<f64>>,
    },
    Other,
}

pub(crate) fn convert<R: Read>(
    input: R,
    options: &ConvertOptions,
    sink: &mut dyn PageConsumer,
) -> Result<Vec<String>> {
    let mut warnings = Vec::new();
    let mut reader = BufReader::new(input);
    let mut raw_line = Vec::new();

    let mut entities: HashMap<u64, StepEntity> = HashMap::new();
    let mut byte_count: u64 = 0;
    let mut in_data = false;
    let mut entity_statement = String::new();

    let mut in_comment = false;

    loop {
        raw_line.clear();
        let bytes_read = reader.read_until(b'\n', &mut raw_line)?;
        if bytes_read == 0 {
            break;
        }
        byte_count += bytes_read as u64;
        if byte_count > options.max_input_bytes {
            return Err(Error::LimitExceeded(format!(
                "STEP file size exceeds maximum limit of {}",
                options.max_input_bytes
            )));
        }

        let line = match std::str::from_utf8(&raw_line) {
            Ok(s) => s.to_string(),
            Err(_) => raw_line.iter().map(|&b| b as char).collect(),
        };

        let mut cleaned = String::new();
        let mut chars = line.chars().peekable();
        while let Some(c) = chars.next() {
            if in_comment {
                if c == '*' && chars.peek() == Some(&'/') {
                    chars.next();
                    in_comment = false;
                }
            } else if c == '/' && chars.peek() == Some(&'*') {
                chars.next();
                in_comment = true;
            } else {
                cleaned.push(c);
            }
        }

        let trimmed = cleaned.trim();
        if trimmed.is_empty() {
            continue;
        }

        let upper = trimmed.to_ascii_uppercase();
        if upper.starts_with("DATA") && (upper.contains(';') || upper.len() == 4) {
            in_data = true;
            continue;
        } else if upper.starts_with("ENDSEC") {
            if in_data {
                break;
            }
            continue;
        }

        if !in_data {
            continue;
        }

        // STEP statements end with ';'
        entity_statement.push_str(trimmed);
        if entity_statement.ends_with(';') {
            parse_step_statement(&entity_statement, &mut entities);
            entity_statement.clear();
            if entities.len() > MAX_STEP_ENTITIES {
                return Err(Error::LimitExceeded(format!(
                    "STEP entity count exceeds maximum allowed limit of {MAX_STEP_ENTITIES}"
                )));
            }
        } else {
            entity_statement.push(' ');
        }
    }

    if entities.is_empty() {
        warnings.push("STEP file contains no valid geometry entities".into());
    }

    // Collect 3D segments from EDGE_CURVE or DIRECT CARTESIAN POINTS
    let mut segments: Vec<(Point3D, Point3D)> = Vec::new();
    let mut referenced_curves: std::collections::HashSet<u64> = std::collections::HashSet::new();

    for entity in entities.values() {
        if let StepEntity::EdgeCurve {
            start_v,
            end_v,
            curve_id,
        } = entity
        {
            referenced_curves.insert(*curve_id);
            let p1 = resolve_vertex_point(*start_v, &entities);
            let p2 = resolve_vertex_point(*end_v, &entities);
            if let Some(StepEntity::Circle { axis_id, radius }) = entities.get(curve_id) {
                sample_step_circle(*axis_id, *radius, p1, p2, &entities, &mut segments);
            } else if let Some(StepEntity::Ellipse {
                axis_id,
                semi_axis_1,
                semi_axis_2,
            }) = entities.get(curve_id)
            {
                sample_step_ellipse(
                    *axis_id,
                    *semi_axis_1,
                    *semi_axis_2,
                    p1,
                    p2,
                    &entities,
                    &mut segments,
                );
            } else if let Some(bspline @ StepEntity::BSplineCurve { .. }) = entities.get(curve_id) {
                push_bspline_segments(bspline, &entities, &mut segments);
            } else if let Some(StepEntity::Polyline(pt_ids)) = entities.get(curve_id) {
                for w in pt_ids.windows(2) {
                    let pa = resolve_cartesian_point(w[0], &entities);
                    let pb = resolve_cartesian_point(w[1], &entities);
                    if let (Some(a), Some(b)) = (pa, pb) {
                        segments.push((a, b));
                    }
                }
            } else if let (Some(pt1), Some(pt2)) = (p1, p2) {
                let dist_sq =
                    (pt1.x - pt2.x).powi(2) + (pt1.y - pt2.y).powi(2) + (pt1.z - pt2.z).powi(2);
                if dist_sq > 1e-12 {
                    segments.push((pt1, pt2));
                }
            }
        }
    }

    // Include standalone circles, ellipses, B-spline curves and polylines if any exist
    for (id, entity) in &entities {
        if let StepEntity::Circle { axis_id, radius } = entity
            && !referenced_curves.contains(id)
        {
            sample_step_circle(*axis_id, *radius, None, None, &entities, &mut segments);
        } else if let StepEntity::Ellipse {
            axis_id,
            semi_axis_1,
            semi_axis_2,
        } = entity
            && !referenced_curves.contains(id)
        {
            sample_step_ellipse(
                *axis_id,
                *semi_axis_1,
                *semi_axis_2,
                None,
                None,
                &entities,
                &mut segments,
            );
        } else if matches!(entity, StepEntity::BSplineCurve { .. })
            && !referenced_curves.contains(id)
        {
            push_bspline_segments(entity, &entities, &mut segments);
        } else if let StepEntity::Polyline(pt_ids) = entity
            && !referenced_curves.contains(id)
        {
            for w in pt_ids.windows(2) {
                let pa = resolve_cartesian_point(w[0], &entities);
                let pb = resolve_cartesian_point(w[1], &entities);
                if let (Some(a), Some(b)) = (pa, pb) {
                    segments.push((a, b));
                }
            }
        }
    }

    // Fallback: If no EDGE_CURVE was present, attempt connecting cartesian points
    if segments.is_empty() {
        let mut pts: Vec<Point3D> = Vec::new();
        for entity in entities.values() {
            if let StepEntity::CartesianPoint(p) = entity {
                pts.push(*p);
            }
        }
        for w in pts.windows(2) {
            segments.push((w[0], w[1]));
        }
    }

    // Isometric projection: 30-degree isometric view
    // X_screen = (x - y) * cos(30 deg)
    // Y_screen = (x + y) * sin(30 deg) - z
    let cos30 = (30.0_f64.to_radians()).cos();
    let sin30 = (30.0_f64.to_radians()).sin();

    let project = |p: Point3D| -> (f64, f64) {
        let sx = (p.x - p.y) * cos30;
        let sy = (p.x + p.y) * sin30 - p.z;
        (sx, sy)
    };

    let mut min_sx = f64::INFINITY;
    let mut min_sy = f64::INFINITY;
    let mut max_sx = f64::NEG_INFINITY;
    let mut max_sy = f64::NEG_INFINITY;

    for (p1, p2) in &segments {
        for p in [*p1, *p2] {
            let (sx, sy) = project(p);
            if sx < min_sx {
                min_sx = sx;
            }
            if sx > max_sx {
                max_sx = sx;
            }
            if sy < min_sy {
                min_sy = sy;
            }
            if sy > max_sy {
                max_sy = sy;
            }
        }
    }

    if min_sx >= max_sx || min_sy >= max_sy {
        min_sx = 0.0;
        min_sy = 0.0;
        max_sx = 100.0;
        max_sy = 100.0;
        warnings.push("STEP geometry bounding box is empty; using default canvas".into());
    }

    let raw_w = (max_sx - min_sx).max(1.0);
    let raw_h = (max_sy - min_sy).max(1.0);
    let margin = (raw_w.max(raw_h) * 0.1).max(20.0);
    let content_w = raw_w + 2.0 * margin;
    let content_h = raw_h + 2.0 * margin;

    let scale = (TARGET_PAGE_LONG_EDGE / content_w.max(content_h)).clamp(0.01, 100.0);
    let page_w = (content_w * scale).max(MIN_PAGE_DIMENSION);
    let page_h = (content_h * scale).max(MIN_PAGE_DIMENSION);

    let mut page = Page::new(1, page_w, page_h, "step");
    page.title = "STEP 3D Isometric View".into();

    let map_pt = |p: Point3D| -> (f64, f64) {
        let (sx, sy) = project(p);
        let px = (sx - min_sx + margin) * scale;
        let py = (sy - min_sy + margin) * scale;
        (px, py)
    };

    // Dark technical blueprint background
    page.nodes.push(Node::Path {
        id: "step-background".into(),
        d: format!(
            "M 0 0 L {:.3} 0 L {:.3} {:.3} L 0 {:.3} Z",
            page_w, page_w, page_h, page_h
        ),
        fill_rule: "nonzero".into(),
        fill: Paint::solid("#0c1322"), // deep blueprint navy
        stroke: Stroke::default(),
        transform: IDENTITY,
        clip_id: None,
        meta: SourceMeta {
            semantic_role: "step:background".into(),
            ..Default::default()
        },
    });

    let mut edge_nodes = Vec::new();
    for (p1, p2) in &segments {
        let (x1, y1) = map_pt(*p1);
        let (x2, y2) = map_pt(*p2);
        edge_nodes.push(Node::Path {
            id: String::new(),
            d: format!("M {:.3} {:.3} L {:.3} {:.3}", x1, y1, x2, y2),
            fill_rule: "nonzero".into(),
            fill: Paint::None,
            stroke: Stroke {
                paint: Paint::solid("#38bdf8"), // electric cyan wireframe
                width: 1.25,
                line_cap: LineCap::Round,
                line_join: LineJoin::Round,
                miter_limit: 4.0,
                dash_array: Vec::new(),
                dash_offset: 0.0,
            },
            transform: IDENTITY,
            clip_id: None,
            meta: Default::default(),
        });
    }

    page.nodes.push(Node::Group {
        id: "step-edges".into(),
        transform: IDENTITY,
        clip_id: None,
        opacity: 0.95,
        nodes: edge_nodes,
        meta: SourceMeta {
            semantic_role: "step:wireframe".into(),
            ..Default::default()
        },
    });

    // Technical title bar metadata
    let subtitle = format!(
        "STEP ISO 10303-21 | Isometric Wireframe | Edges: {}",
        segments.len()
    );
    page.nodes.push(Node::Text {
        id: "step-label".into(),
        x: 20.0,
        y: page_h - 20.0,
        anchor: TextAnchor::Start,
        transform: IDENTITY,
        clip_id: None,
        opacity: 1.0,
        stroke: Stroke::default(),
        runs: vec![TextRun {
            text: subtitle,
            font_family: "monospace, monospace".into(),
            font_size: 13.0,
            bold: true,
            italic: false,
            fill: Paint::solid("#94a3b8"),
            baseline_shift: 0.0,
            glyph_x_offsets: Vec::new(),
            target_advance: None,
        }],
        meta: Default::default(),
    });

    sink.consume(page)?;
    Ok(warnings)
}

fn resolve_vertex_point(v_id: u64, entities: &HashMap<u64, StepEntity>) -> Option<Point3D> {
    match entities.get(&v_id)? {
        StepEntity::VertexPoint { point_id } => {
            if let Some(StepEntity::CartesianPoint(p)) = entities.get(point_id) {
                Some(*p)
            } else {
                None
            }
        }
        StepEntity::CartesianPoint(p) => Some(*p),
        _ => None,
    }
}

fn resolve_cartesian_point(p_id: u64, entities: &HashMap<u64, StepEntity>) -> Option<Point3D> {
    match entities.get(&p_id)? {
        StepEntity::CartesianPoint(p) => Some(*p),
        StepEntity::VertexPoint { point_id } => resolve_cartesian_point(*point_id, entities),
        _ => None,
    }
}

fn resolve_direction(d_id: u64, entities: &HashMap<u64, StepEntity>) -> Option<Point3D> {
    match entities.get(&d_id)? {
        StepEntity::Direction(d) => Some(*d),
        StepEntity::Vector { dir_id, .. } => resolve_direction(*dir_id, entities),
        _ => None,
    }
}

fn normalize(p: Point3D) -> Point3D {
    let len = (p.x * p.x + p.y * p.y + p.z * p.z).sqrt();
    if len > 1e-9 {
        Point3D {
            x: p.x / len,
            y: p.y / len,
            z: p.z / len,
        }
    } else {
        Point3D {
            x: 0.0,
            y: 0.0,
            z: 1.0,
        }
    }
}

fn cross(a: Point3D, b: Point3D) -> Point3D {
    Point3D {
        x: a.y * b.z - a.z * b.y,
        y: a.z * b.x - a.x * b.z,
        z: a.x * b.y - a.y * b.x,
    }
}

fn dot(a: Point3D, b: Point3D) -> f64 {
    a.x * b.x + a.y * b.y + a.z * b.z
}

/// Resolves an `AXIS2_PLACEMENT_3D` into a right-handed `(center, u, v)`
/// in-plane frame, shared by circle and ellipse sampling. Falls back to the
/// world XY plane at the origin when the placement is missing or unresolved.
fn resolve_axis_frame(
    axis_id: u64,
    entities: &HashMap<u64, StepEntity>,
) -> (Point3D, Point3D, Point3D) {
    let (center, normal, ref_dir) = match entities.get(&axis_id) {
        Some(StepEntity::Axis2Placement3D {
            location_id,
            axis_id: ax_id,
            ref_dir_id,
        }) => {
            let center = resolve_cartesian_point(*location_id, entities).unwrap_or(Point3D {
                x: 0.0,
                y: 0.0,
                z: 0.0,
            });
            let normal = ax_id
                .and_then(|id| resolve_direction(id, entities))
                .unwrap_or(Point3D {
                    x: 0.0,
                    y: 0.0,
                    z: 1.0,
                });
            let ref_dir = ref_dir_id.and_then(|id| resolve_direction(id, entities));
            (center, normal, ref_dir)
        }
        _ => (
            Point3D {
                x: 0.0,
                y: 0.0,
                z: 0.0,
            },
            Point3D {
                x: 0.0,
                y: 0.0,
                z: 1.0,
            },
            None,
        ),
    };

    let n = normalize(normal);
    let u_init = if let Some(rd) = ref_dir {
        normalize(rd)
    } else if n.x.abs() < 0.9 {
        Point3D {
            x: 1.0,
            y: 0.0,
            z: 0.0,
        }
    } else {
        Point3D {
            x: 0.0,
            y: 1.0,
            z: 0.0,
        }
    };
    let d = dot(u_init, n);
    let u_ortho = Point3D {
        x: u_init.x - d * n.x,
        y: u_init.y - d * n.y,
        z: u_init.z - d * n.z,
    };
    let u = normalize(u_ortho);
    let v = cross(n, u);
    (center, u, v)
}

fn sample_step_circle(
    axis_id: u64,
    radius: f64,
    start_pt: Option<Point3D>,
    end_pt: Option<Point3D>,
    entities: &HashMap<u64, StepEntity>,
    segments: &mut Vec<(Point3D, Point3D)>,
) {
    if radius <= 1e-6 {
        return;
    }
    let (center, u, v) = resolve_axis_frame(axis_id, entities);

    let get_pt = |angle: f64| -> Point3D {
        Point3D {
            x: center.x + radius * (angle.cos() * u.x + angle.sin() * v.x),
            y: center.y + radius * (angle.cos() * u.y + angle.sin() * v.y),
            z: center.z + radius * (angle.cos() * u.z + angle.sin() * v.z),
        }
    };

    let is_closed = match (start_pt, end_pt) {
        (Some(p1), Some(p2)) => {
            let dist_sq = (p1.x - p2.x).powi(2) + (p1.y - p2.y).powi(2) + (p1.z - p2.z).powi(2);
            dist_sq < 1e-8
        }
        _ => true,
    };

    if is_closed {
        let steps = 32;
        let mut prev = get_pt(0.0);
        for i in 1..=steps {
            let angle = (i as f64 / steps as f64) * std::f64::consts::TAU;
            let curr = get_pt(angle);
            segments.push((prev, curr));
            prev = curr;
        }
    } else if let (Some(p1), Some(p2)) = (start_pt, end_pt) {
        let d1 = Point3D {
            x: p1.x - center.x,
            y: p1.y - center.y,
            z: p1.z - center.z,
        };
        let d2 = Point3D {
            x: p2.x - center.x,
            y: p2.y - center.y,
            z: p2.z - center.z,
        };
        let a1 = dot(d1, v).atan2(dot(d1, u));
        let mut a2 = dot(d2, v).atan2(dot(d2, u));
        if a2 <= a1 {
            a2 += std::f64::consts::TAU;
        }
        let steps = 24;
        let mut prev = p1;
        for i in 1..=steps {
            let angle = a1 + (i as f64 / steps as f64) * (a2 - a1);
            let curr = get_pt(angle);
            segments.push((prev, curr));
            prev = curr;
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn sample_step_ellipse(
    axis_id: u64,
    semi_axis_1: f64,
    semi_axis_2: f64,
    start_pt: Option<Point3D>,
    end_pt: Option<Point3D>,
    entities: &HashMap<u64, StepEntity>,
    segments: &mut Vec<(Point3D, Point3D)>,
) {
    if !(semi_axis_1 > 1e-6 && semi_axis_2 > 1e-6) {
        return;
    }
    let (center, u, v) = resolve_axis_frame(axis_id, entities);

    let get_pt = |angle: f64| -> Point3D {
        Point3D {
            x: center.x + semi_axis_1 * angle.cos() * u.x + semi_axis_2 * angle.sin() * v.x,
            y: center.y + semi_axis_1 * angle.cos() * u.y + semi_axis_2 * angle.sin() * v.y,
            z: center.z + semi_axis_1 * angle.cos() * u.z + semi_axis_2 * angle.sin() * v.z,
        }
    };
    let angle_of = |p: Point3D| -> f64 {
        let d = Point3D {
            x: p.x - center.x,
            y: p.y - center.y,
            z: p.z - center.z,
        };
        (dot(d, v) / semi_axis_2).atan2(dot(d, u) / semi_axis_1)
    };

    let is_closed = match (start_pt, end_pt) {
        (Some(p1), Some(p2)) => {
            let dist_sq = (p1.x - p2.x).powi(2) + (p1.y - p2.y).powi(2) + (p1.z - p2.z).powi(2);
            dist_sq < 1e-8
        }
        _ => true,
    };

    if is_closed {
        let steps = 40;
        let mut prev = get_pt(0.0);
        for i in 1..=steps {
            let angle = (i as f64 / steps as f64) * std::f64::consts::TAU;
            let curr = get_pt(angle);
            segments.push((prev, curr));
            prev = curr;
        }
    } else if let (Some(p1), Some(p2)) = (start_pt, end_pt) {
        let a1 = angle_of(p1);
        let mut a2 = angle_of(p2);
        if a2 <= a1 {
            a2 += std::f64::consts::TAU;
        }
        let steps = 32;
        let mut prev = p1;
        for i in 1..=steps {
            let angle = a1 + (i as f64 / steps as f64) * (a2 - a1);
            let curr = get_pt(angle);
            segments.push((prev, curr));
            prev = curr;
        }
    }
}

/// Bounded points sampled along a STEP B-spline curve edge; independent of
/// curve degree/knot complexity to keep rendering deterministic and cheap.
const STEP_BSPLINE_TESSELLATION_STEPS: usize = 64;

/// Resolves a [`StepEntity::BSplineCurve`]'s control points, expands its
/// knot multiplicities, evaluates the resulting NURBS curve, and appends the
/// tessellated segments. Silently omits the curve (no segments pushed) when
/// control points are unresolved or the curve fails validation, matching
/// this reader's bounded, warn-or-omit contract for unsupported geometry.
fn push_bspline_segments(
    entity: &StepEntity,
    entities: &HashMap<u64, StepEntity>,
    segments: &mut Vec<(Point3D, Point3D)>,
) {
    let StepEntity::BSplineCurve {
        degree,
        control_point_ids,
        knot_multiplicities,
        knot_values,
        weights,
    } = entity
    else {
        return;
    };

    let mut control_points = Vec::with_capacity(control_point_ids.len());
    for &pid in control_point_ids {
        match resolve_cartesian_point(pid, entities) {
            Some(p) => control_points.push(crate::cad::nurbs::Point3 {
                x: p.x,
                y: p.y,
                z: p.z,
            }),
            None => return,
        }
    }
    let Some(knots) =
        crate::cad::nurbs::expand_knot_multiplicities(knot_multiplicities, knot_values)
    else {
        return;
    };
    let curve_weights = match weights {
        Some(w) if w.len() == control_points.len() => w.clone(),
        Some(_) => return,
        None => vec![1.0; control_points.len()],
    };

    let curve = crate::cad::nurbs::NurbsCurve {
        degree: *degree,
        control_points,
        weights: curve_weights,
        knots,
    };
    if !curve.is_valid() {
        return;
    }
    let points = curve.tessellate(STEP_BSPLINE_TESSELLATION_STEPS);
    for w in points.windows(2) {
        segments.push((
            Point3D {
                x: w[0].x,
                y: w[0].y,
                z: w[0].z,
            },
            Point3D {
                x: w[1].x,
                y: w[1].y,
                z: w[1].z,
            },
        ));
    }
}

fn parse_step_statement(stmt: &str, entities: &mut HashMap<u64, StepEntity>) {
    let trimmed = stmt.trim().trim_end_matches(';');
    let eq_pos = match trimmed.find('=') {
        Some(pos) => pos,
        None => return,
    };

    let id_str = trimmed[..eq_pos].trim().trim_start_matches('#');
    let id: u64 = match id_str.parse() {
        Ok(v) => v,
        Err(_) => return,
    };

    let rest = trimmed[eq_pos + 1..].trim();
    let paren_pos = match rest.find('(') {
        Some(pos) => pos,
        None => return,
    };

    let keyword = rest[..paren_pos].trim().to_ascii_uppercase();
    let args = &rest[paren_pos..];

    if keyword.is_empty() {
        // A STEP "complex entity" instance: several simple entities joined
        // without an outer keyword, e.g.
        // #50=(BOUNDED_CURVE()B_SPLINE_CURVE(3,(#51,#52,#53,#54),.UNSPECIFIED.,.F.,.F.)
        //      B_SPLINE_CURVE_WITH_KNOTS((4,4),(0.,1.),.UNSPECIFIED.)CURVE()
        //      GEOMETRIC_REPRESENTATION_ITEM()
        //      RATIONAL_B_SPLINE_CURVE((1.,1.,1.,1.))REPRESENTATION_ITEM(''));
        // is how real CAD exporters express a NURBS edge curve.
        parse_step_complex_entity(id, rest, entities);
        return;
    }

    match keyword.as_str() {
        "CARTESIAN_POINT" => {
            if let Some(coords) = extract_inner_tuple(args) {
                let nums: Vec<f64> = coords
                    .split(',')
                    .filter_map(crate::cad::dxf::geometry::parse_cad_float)
                    .collect();
                if nums.len() >= 3 {
                    entities.insert(
                        id,
                        StepEntity::CartesianPoint(Point3D {
                            x: nums[0],
                            y: nums[1],
                            z: nums[2],
                        }),
                    );
                } else if nums.len() == 2 {
                    entities.insert(
                        id,
                        StepEntity::CartesianPoint(Point3D {
                            x: nums[0],
                            y: nums[1],
                            z: 0.0,
                        }),
                    );
                }
            }
        }
        "DIRECTION" => {
            if let Some(coords) = extract_inner_tuple(args) {
                let nums: Vec<f64> = coords
                    .split(',')
                    .filter_map(crate::cad::dxf::geometry::parse_cad_float)
                    .collect();
                if nums.len() >= 3 {
                    entities.insert(
                        id,
                        StepEntity::Direction(Point3D {
                            x: nums[0],
                            y: nums[1],
                            z: nums[2],
                        }),
                    );
                }
            }
        }
        "VECTOR" => {
            let inner = args.trim_start_matches('(').trim_end_matches(')');
            let parts: Vec<&str> = inner.split(',').collect();
            if parts.len() >= 3 {
                let dir_id = parts[1].trim().trim_start_matches('#').parse().unwrap_or(0);
                let length = crate::cad::dxf::geometry::parse_cad_float(parts[2]).unwrap_or(1.0);
                entities.insert(id, StepEntity::Vector { dir_id, length });
            }
        }
        "LINE" => {
            let inner = args.trim_start_matches('(').trim_end_matches(')');
            let parts: Vec<&str> = inner.split(',').collect();
            if parts.len() >= 3 {
                let point_id = parts[1].trim().trim_start_matches('#').parse().unwrap_or(0);
                let dir_id = parts[2].trim().trim_start_matches('#').parse().unwrap_or(0);
                entities.insert(id, StepEntity::Line { point_id, dir_id });
            }
        }
        "VERTEX_POINT" => {
            let inner = args.trim_start_matches('(').trim_end_matches(')');
            let parts: Vec<&str> = inner.split(',').collect();
            if parts.len() >= 2 {
                let point_id = parts[1].trim().trim_start_matches('#').parse().unwrap_or(0);
                entities.insert(id, StepEntity::VertexPoint { point_id });
            }
        }
        "EDGE_CURVE" => {
            let inner = args.trim_start_matches('(').trim_end_matches(')');
            let parts: Vec<&str> = inner.split(',').collect();
            if parts.len() >= 4 {
                let start_v = parts[1].trim().trim_start_matches('#').parse().unwrap_or(0);
                let end_v = parts[2].trim().trim_start_matches('#').parse().unwrap_or(0);
                let curve_id = parts[3].trim().trim_start_matches('#').parse().unwrap_or(0);
                entities.insert(
                    id,
                    StepEntity::EdgeCurve {
                        start_v,
                        end_v,
                        curve_id,
                    },
                );
            }
        }
        "AXIS2_PLACEMENT_3D" => {
            let inner = args.trim_start_matches('(').trim_end_matches(')');
            let parts: Vec<&str> = inner.split(',').collect();
            if parts.len() >= 2 {
                let location_id = parts[1].trim().trim_start_matches('#').parse().unwrap_or(0);
                let axis_id = parts.get(2).and_then(|p| {
                    let s = p.trim().trim_start_matches('#');
                    s.parse().ok()
                });
                let ref_dir_id = parts.get(3).and_then(|p| {
                    let s = p.trim().trim_start_matches('#');
                    s.parse().ok()
                });
                entities.insert(
                    id,
                    StepEntity::Axis2Placement3D {
                        location_id,
                        axis_id,
                        ref_dir_id,
                    },
                );
            }
        }
        "CIRCLE" => {
            let inner = args.trim_start_matches('(').trim_end_matches(')');
            let parts: Vec<&str> = inner.split(',').collect();
            if parts.len() >= 3 {
                let axis_id = parts[1].trim().trim_start_matches('#').parse().unwrap_or(0);
                let radius = crate::cad::dxf::geometry::parse_cad_float(parts[2]).unwrap_or(0.0);
                entities.insert(id, StepEntity::Circle { axis_id, radius });
            }
        }
        "ELLIPSE" => {
            let inner = args.trim_start_matches('(').trim_end_matches(')');
            let parts: Vec<&str> = inner.split(',').collect();
            if parts.len() >= 4 {
                let axis_id = parts[1].trim().trim_start_matches('#').parse().unwrap_or(0);
                let semi_axis_1 =
                    crate::cad::dxf::geometry::parse_cad_float(parts[2]).unwrap_or(0.0);
                let semi_axis_2 =
                    crate::cad::dxf::geometry::parse_cad_float(parts[3]).unwrap_or(0.0);
                entities.insert(
                    id,
                    StepEntity::Ellipse {
                        axis_id,
                        semi_axis_1,
                        semi_axis_2,
                    },
                );
            }
        }
        "POLYLINE" => {
            if let Some(tuple_str) = extract_inner_tuple(args) {
                let point_ids: Vec<u64> = tuple_str
                    .split(',')
                    .filter_map(|s| s.trim().trim_start_matches('#').parse().ok())
                    .collect();
                if !point_ids.is_empty() {
                    entities.insert(id, StepEntity::Polyline(point_ids));
                }
            }
        }
        _ => {
            entities.insert(id, StepEntity::Other);
        }
    }
}

/// Parses a STEP complex entity instance (`#id=(A()B(...)C(...));`), looking
/// specifically for the `B_SPLINE_CURVE` / `B_SPLINE_CURVE_WITH_KNOTS` /
/// `RATIONAL_B_SPLINE_CURVE` combination that real CAD exporters use for
/// NURBS edges. Any other complex entity is recorded as [`StepEntity::Other`]
/// so it never resolves as a curve reference.
fn parse_step_complex_entity(id: u64, rest: &str, entities: &mut HashMap<u64, StepEntity>) {
    if let (Some(base_args), Some(knots_args)) = (
        find_keyword_args(rest, "B_SPLINE_CURVE"),
        find_keyword_args(rest, "B_SPLINE_CURVE_WITH_KNOTS"),
    ) {
        let base_inner = &base_args[1..base_args.len().saturating_sub(1)];
        let base_parts = split_top_level(base_inner);
        let knots_inner = &knots_args[1..knots_args.len().saturating_sub(1)];
        let knots_parts = split_top_level(knots_inner);

        if base_parts.len() >= 2 && knots_parts.len() >= 2 {
            let degree = base_parts[0].trim().parse::<i64>().ok();
            let control_point_ids: Option<Vec<u64>> = Some(
                strip_one_paren_level(base_parts[1])
                    .split(',')
                    .filter_map(|p| p.trim().trim_start_matches('#').parse().ok())
                    .collect(),
            );
            let knot_multiplicities: Option<Vec<u64>> = Some(
                strip_one_paren_level(knots_parts[0])
                    .split(',')
                    .filter_map(|p| p.trim().parse::<u64>().ok())
                    .collect(),
            );
            let knot_values: Option<Vec<f64>> = Some(
                strip_one_paren_level(knots_parts[1])
                    .split(',')
                    .filter_map(crate::cad::dxf::geometry::parse_cad_float)
                    .collect(),
            );
            let weights = find_keyword_args(rest, "RATIONAL_B_SPLINE_CURVE").and_then(|w_args| {
                let w_inner = &w_args[1..w_args.len().saturating_sub(1)];
                let w_parts = split_top_level(w_inner);
                w_parts.first().map(|p| {
                    strip_one_paren_level(p)
                        .split(',')
                        .filter_map(crate::cad::dxf::geometry::parse_cad_float)
                        .collect::<Vec<f64>>()
                })
            });

            if let (
                Some(degree),
                Some(control_point_ids),
                Some(knot_multiplicities),
                Some(knot_values),
            ) = (degree, control_point_ids, knot_multiplicities, knot_values)
                && degree >= 1
                && (degree as usize) < 10_000
                && !control_point_ids.is_empty()
                && control_point_ids.len() <= 20_000
                && knot_multiplicities.len() == knot_values.len()
                && !knot_multiplicities.is_empty()
            {
                entities.insert(
                    id,
                    StepEntity::BSplineCurve {
                        degree: degree as usize,
                        control_point_ids,
                        knot_multiplicities,
                        knot_values,
                        weights,
                    },
                );
                return;
            }
        }
    }
    entities.insert(id, StepEntity::Other);
}

/// Finds a top-level, word-bounded `KEYWORD(...)` occurrence in `text` and
/// returns its balanced-parenthesis argument string, parens included.
/// Word-boundary checking keeps `B_SPLINE_CURVE` from matching inside
/// `B_SPLINE_CURVE_WITH_KNOTS` or `RATIONAL_B_SPLINE_CURVE`.
fn find_keyword_args<'a>(text: &'a str, keyword: &str) -> Option<&'a str> {
    let bytes = text.as_bytes();
    let mut search_from = 0usize;
    while let Some(rel) = text[search_from..].find(keyword) {
        let start = search_from + rel;
        let before_ok = start == 0 || !is_step_word_byte(bytes[start - 1]);
        let after = start + keyword.len();
        if before_ok && after < bytes.len() && bytes[after] == b'(' {
            let mut depth = 0i32;
            for (off, ch) in text[after..].char_indices() {
                match ch {
                    '(' => depth += 1,
                    ')' => {
                        depth -= 1;
                        if depth == 0 {
                            return Some(&text[after..after + off + ch.len_utf8()]);
                        }
                    }
                    _ => {}
                }
            }
            return None;
        }
        search_from = start + keyword.len();
    }
    None
}

fn is_step_word_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

/// Strips exactly one leading `(` and trailing `)` from a STEP tuple
/// literal such as `(#51,#52,#53)`, leaving the interior untouched.
fn strip_one_paren_level(s: &str) -> &str {
    let s = s.trim();
    match s.strip_prefix('(') {
        Some(inner) => inner.strip_suffix(')').unwrap_or(inner),
        None => s,
    }
}

/// Splits a STEP argument-list interior (no outer parens) on top-level
/// commas, treating nested `(...)` groups as opaque.
fn split_top_level(s: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut depth = 0i32;
    let mut start = 0usize;
    for (i, ch) in s.char_indices() {
        match ch {
            '(' => depth += 1,
            ')' => depth -= 1,
            ',' if depth == 0 => {
                parts.push(&s[start..i]);
                start = i + ch.len_utf8();
            }
            _ => {}
        }
    }
    parts.push(&s[start..]);
    parts
}

fn extract_inner_tuple(args: &str) -> Option<&str> {
    let start = args.find('(')?;
    let rest = &args[start + 1..];
    let second_start = rest.find('(')?;
    let second_end = rest[second_start + 1..].find(')')?;
    Some(&rest[second_start + 1..second_start + 1 + second_end])
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    struct DummySink(Vec<Page>);
    impl PageConsumer for DummySink {
        fn consume(&mut self, page: Page) -> Result<()> {
            self.0.push(page);
            Ok(())
        }
    }

    #[test]
    fn parses_step_iso10303_cube_wireframe() {
        let step_content = r#"
ISO-10303-21;
HEADER;
FILE_DESCRIPTION(('STEP AP203 Cube sample'),'2;1');
FILE_NAME('cube.step','2026-09-10',('Engineer'),('Testing'),'','','');
FILE_SCHEMA(('CONFIG_CONTROL_DESIGN'));
ENDSEC;
DATA;
#10 = CARTESIAN_POINT('', (0.0, 0.0, 0.0));
#11 = CARTESIAN_POINT('', (50.0, 0.0, 0.0));
#12 = CARTESIAN_POINT('', (50.0, 50.0, 0.0));
#13 = CARTESIAN_POINT('', (0.0, 50.0, 0.0));
#14 = CARTESIAN_POINT('', (0.0, 0.0, 50.0));
#15 = CARTESIAN_POINT('', (50.0, 0.0, 50.0));
#16 = CARTESIAN_POINT('', (50.0, 50.0, 50.0));
#17 = CARTESIAN_POINT('', (0.0, 50.0, 50.0));

#20 = VERTEX_POINT('', #10);
#21 = VERTEX_POINT('', #11);
#22 = VERTEX_POINT('', #12);
#23 = VERTEX_POINT('', #13);
#24 = VERTEX_POINT('', #14);
#25 = VERTEX_POINT('', #15);
#26 = VERTEX_POINT('', #16);
#27 = VERTEX_POINT('', #17);

#30 = EDGE_CURVE('', #20, #21, #0, .T.);
#31 = EDGE_CURVE('', #21, #22, #0, .T.);
#32 = EDGE_CURVE('', #22, #23, #0, .T.);
#33 = EDGE_CURVE('', #23, #20, #0, .T.);
#34 = EDGE_CURVE('', #24, #25, #0, .T.);
#35 = EDGE_CURVE('', #25, #26, #0, .T.);
#36 = EDGE_CURVE('', #26, #27, #0, .T.);
#37 = EDGE_CURVE('', #27, #24, #0, .T.);
#38 = EDGE_CURVE('', #20, #24, #0, .T.);
#39 = EDGE_CURVE('', #21, #25, #0, .T.);
#40 = EDGE_CURVE('', #22, #26, #0, .T.);
#41 = EDGE_CURVE('', #23, #27, #0, .T.);
ENDSEC;
END-ISO-10303-21;
"#;
        let mut sink = DummySink(Vec::new());
        let options = ConvertOptions::default();
        let warnings =
            convert(Cursor::new(step_content), &options, &mut sink).expect("convert step");
        assert!(warnings.is_empty());
        assert_eq!(sink.0.len(), 1);
        let page = &sink.0[0];
        assert_eq!(page.nodes.len(), 3);
        if let Node::Group { nodes, .. } = &page.nodes[1] {
            assert_eq!(nodes.len(), 12);
        } else {
            panic!("expected wireframe group");
        }
    }

    #[test]
    fn find_keyword_args_respects_word_boundaries() {
        let text = "(BOUNDED_CURVE()B_SPLINE_CURVE(3,(#1,#2))B_SPLINE_CURVE_WITH_KNOTS((4,4),(0.,1.),.UNSPECIFIED.)RATIONAL_B_SPLINE_CURVE((1.,1.)))";
        assert_eq!(
            find_keyword_args(text, "B_SPLINE_CURVE"),
            Some("(3,(#1,#2))")
        );
        assert_eq!(
            find_keyword_args(text, "B_SPLINE_CURVE_WITH_KNOTS"),
            Some("((4,4),(0.,1.),.UNSPECIFIED.)")
        );
        assert_eq!(
            find_keyword_args(text, "RATIONAL_B_SPLINE_CURVE"),
            Some("((1.,1.))")
        );
        assert_eq!(find_keyword_args(text, "MISSING_KEYWORD"), None);
    }

    #[test]
    fn parses_complex_bspline_curve_entity() {
        let stmt = "#50=(BOUNDED_CURVE()B_SPLINE_CURVE(3,(#10,#11,#12,#13),.UNSPECIFIED.,.F.,.F.)B_SPLINE_CURVE_WITH_KNOTS((4,4),(0.,1.),.UNSPECIFIED.)CURVE()GEOMETRIC_REPRESENTATION_ITEM()RATIONAL_B_SPLINE_CURVE((1.,1.,1.,1.))REPRESENTATION_ITEM(''));";
        let mut entities = HashMap::new();
        parse_step_statement(stmt, &mut entities);
        match entities.get(&50) {
            Some(StepEntity::BSplineCurve {
                degree,
                control_point_ids,
                knot_multiplicities,
                knot_values,
                weights,
            }) => {
                assert_eq!(*degree, 3);
                assert_eq!(control_point_ids, &vec![10, 11, 12, 13]);
                assert_eq!(knot_multiplicities, &vec![4, 4]);
                assert_eq!(knot_values, &vec![0.0, 1.0]);
                assert_eq!(weights.as_deref(), Some([1.0, 1.0, 1.0, 1.0].as_slice()));
            }
            other => panic!("expected BSplineCurve, got {other:?}"),
        }
    }

    #[test]
    fn tessellates_step_bspline_curve_through_endpoints() {
        let mut entities = HashMap::new();
        entities.insert(
            10,
            StepEntity::CartesianPoint(Point3D {
                x: 0.0,
                y: 0.0,
                z: 0.0,
            }),
        );
        entities.insert(
            11,
            StepEntity::CartesianPoint(Point3D {
                x: 0.0,
                y: 10.0,
                z: 0.0,
            }),
        );
        entities.insert(
            12,
            StepEntity::CartesianPoint(Point3D {
                x: 10.0,
                y: 10.0,
                z: 0.0,
            }),
        );
        entities.insert(
            13,
            StepEntity::CartesianPoint(Point3D {
                x: 10.0,
                y: 0.0,
                z: 0.0,
            }),
        );
        let curve = StepEntity::BSplineCurve {
            degree: 3,
            control_point_ids: vec![10, 11, 12, 13],
            knot_multiplicities: vec![4, 4],
            knot_values: vec![0.0, 1.0],
            weights: None,
        };
        let mut segments = Vec::new();
        push_bspline_segments(&curve, &entities, &mut segments);
        assert!(segments.len() > 10);
        let first = segments.first().unwrap().0;
        let last = segments.last().unwrap().1;
        assert!(first.x.abs() < 1e-6 && first.y.abs() < 1e-6);
        assert!((last.x - 10.0).abs() < 1e-6 && last.y.abs() < 1e-6);
    }

    #[test]
    fn tessellates_step_ellipse_arc() {
        let mut entities = HashMap::new();
        entities.insert(
            100,
            StepEntity::CartesianPoint(Point3D {
                x: 0.0,
                y: 0.0,
                z: 0.0,
            }),
        );
        entities.insert(
            103,
            StepEntity::Axis2Placement3D {
                location_id: 100,
                axis_id: None,
                ref_dir_id: None,
            },
        );
        let start = Point3D {
            x: 20.0,
            y: 0.0,
            z: 0.0,
        };
        let end = Point3D {
            x: 0.0,
            y: 10.0,
            z: 0.0,
        };
        let mut segments = Vec::new();
        sample_step_ellipse(
            103,
            20.0,
            10.0,
            Some(start),
            Some(end),
            &entities,
            &mut segments,
        );
        assert!(!segments.is_empty());
        assert!((segments.first().unwrap().0.x - 20.0).abs() < 1e-6);
        let last = segments.last().unwrap().1;
        assert!(last.x.abs() < 1e-6 && (last.y - 10.0).abs() < 1e-6);
    }

    #[test]
    fn end_to_end_step_with_nurbs_and_ellipse_edges() {
        let step_content = r#"
ISO-10303-21;
HEADER;
FILE_DESCRIPTION(('NURBS + ellipse sample'),'2;1');
FILE_NAME('curves.step','2026-09-22',('Engineer'),('Testing'),'','','');
FILE_SCHEMA(('CONFIG_CONTROL_DESIGN'));
ENDSEC;
DATA;
#10 = CARTESIAN_POINT('', (0.0, 0.0, 0.0));
#11 = CARTESIAN_POINT('', (0.0, 10.0, 0.0));
#12 = CARTESIAN_POINT('', (10.0, 10.0, 0.0));
#13 = CARTESIAN_POINT('', (10.0, 0.0, 0.0));
#20 = VERTEX_POINT('', #10);
#21 = VERTEX_POINT('', #13);
#50=(
BOUNDED_CURVE()
B_SPLINE_CURVE(3,(#10,#11,#12,#13),.UNSPECIFIED.,.F.,.F.)
B_SPLINE_CURVE_WITH_KNOTS((4,4),(0.,1.),.UNSPECIFIED.)
CURVE()
GEOMETRIC_REPRESENTATION_ITEM()
RATIONAL_B_SPLINE_CURVE((1.,1.,1.,1.))
REPRESENTATION_ITEM('')
);
#60 = EDGE_CURVE('', #20, #21, #50, .T.);

#100 = CARTESIAN_POINT('', (50.0, 0.0, 0.0));
#103 = AXIS2_PLACEMENT_3D('', #100, $, $);
#104 = ELLIPSE('', #103, 20.0, 10.0);
ENDSEC;
END-ISO-10303-21;
"#;
        let mut sink = DummySink(Vec::new());
        let options = ConvertOptions::default();
        let warnings =
            convert(Cursor::new(step_content), &options, &mut sink).expect("convert step");
        assert!(warnings.is_empty());
        assert_eq!(sink.0.len(), 1);
        let page = &sink.0[0];
        if let Node::Group { nodes, .. } = &page.nodes[1] {
            // 1 tessellated NURBS edge (~64 segments) + a standalone closed
            // ellipse (40 segments), well beyond the old line-only coverage.
            assert!(nodes.len() > 60);
        } else {
            panic!("expected wireframe group");
        }
    }

    #[test]
    fn parses_step_with_non_utf8_header() {
        let step_bytes = b"ISO-10303-21;\nHEADER;\n/* Pi\xE8ce mod\xE8le 20\xB0C */\nFILE_NAME('test', '2026-01-01', ('A'), ('B'), 'C', 'D', 'E');\nENDSEC;\nDATA;\n#10 = CARTESIAN_POINT('', (0.0, 0.0, 0.0));\n#11 = CARTESIAN_POINT('', (10.0, 0.0, 0.0));\n#20 = VERTEX_POINT('', #10);\n#21 = VERTEX_POINT('', #11);\n#30 = EDGE_CURVE('', #20, #21, #0, .T.);\nENDSEC;\nEND-ISO-10303-21;\n".to_vec();

        let mut sink = DummySink(Vec::new());
        let options = ConvertOptions::default();
        let warnings = convert(Cursor::new(step_bytes), &options, &mut sink)
            .expect("convert step with non-utf8");
        assert!(warnings.is_empty());
        assert_eq!(sink.0.len(), 1);
    }
}
