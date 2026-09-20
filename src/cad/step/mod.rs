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

    // Include standalone circles and polylines if any exist
    for (id, entity) in &entities {
        if let StepEntity::Circle { axis_id, radius } = entity
            && !referenced_curves.contains(id)
        {
            sample_step_circle(*axis_id, *radius, None, None, &entities, &mut segments);
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
