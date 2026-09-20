//! Wavefront OBJ 3D model parser and Lambertian-shaded vector polygon renderer.
//!
//! Parses standard 3D mesh definitions (`v` vertices, `vn` normals, `f` polygonal faces),
//! calculates isometric/perspective camera transforms, sorts faces by painter's depth algorithm,
//! and renders shaded 2D SVG vector faces with directional lighting.

pub mod writer;

use std::io::{BufRead, BufReader, Read};

use crate::convert::{ConvertOptions, PageConsumer};
use crate::error::{Error, Result};
use crate::ir::{IDENTITY, LineCap, LineJoin, Node, Page, Paint, SourceMeta, Stroke};

const TARGET_PAGE_LONG_EDGE: f64 = 1200.0;
const MIN_PAGE_DIMENSION: f64 = 400.0;
const MAX_OBJ_FACES: usize = 200_000;

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

pub(crate) fn convert<R: Read>(
    input: R,
    options: &ConvertOptions,
    sink: &mut dyn PageConsumer,
) -> Result<Vec<String>> {
    let mut warnings = Vec::new();
    let mut byte_count = 0u64;
    let mut reader = BufReader::new(input);
    let mut line_buf = Vec::new();
    let mut vertices = Vec::<Vec3>::new();
    let mut faces = Vec::<Vec<usize>>::new();
    let mut wireframe_lines = Vec::<Vec<usize>>::new();

    loop {
        line_buf.clear();
        let bytes_read = reader.read_until(b'\n', &mut line_buf)?;
        if bytes_read == 0 {
            break;
        }
        byte_count += bytes_read as u64;
        if byte_count > options.max_input_bytes {
            return Err(Error::LimitExceeded(format!(
                "OBJ file size exceeds maximum limit of {}",
                options.max_input_bytes
            )));
        }

        let line = match std::str::from_utf8(&line_buf) {
            Ok(s) => s.to_string(),
            Err(_) => line_buf.iter().map(|&b| b as char).collect::<String>(),
        };

        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }

        let mut parts = trimmed.split_whitespace();
        let cmd = match parts.next() {
            Some(c) => c,
            None => continue,
        };

        if cmd == "v" {
            let nums: Vec<f64> = parts
                .filter_map(crate::cad::dxf::geometry::parse_cad_float)
                .collect();
            if nums.len() >= 3 {
                vertices.push(Vec3 {
                    x: nums[0],
                    y: nums[1],
                    z: nums[2],
                });
            }
        } else if cmd == "f" {
            let mut face_v = Vec::new();
            for part in parts {
                // Vertex index format can be v, v/vt, or v/vt/vn
                let v_str = match part.find('/') {
                    Some(slash) => &part[..slash],
                    None => part,
                };
                if let Ok(idx) = v_str.parse::<isize>() {
                    let zero_based = if idx > 0 {
                        usize::try_from(idx - 1).ok()
                    } else if idx < 0 {
                        let rel = vertices.len() as isize + idx;
                        if rel >= 0 {
                            usize::try_from(rel).ok()
                        } else {
                            None
                        }
                    } else {
                        None
                    };
                    if let Some(zb) = zero_based {
                        face_v.push(zb);
                    }
                }
            }
            if face_v.len() >= 3 {
                faces.push(face_v);
                if faces.len() > MAX_OBJ_FACES {
                    return Err(Error::LimitExceeded(format!(
                        "OBJ face count exceeds limit of {MAX_OBJ_FACES}"
                    )));
                }
            }
        } else if cmd == "l" {
            let mut line_v = Vec::new();
            for part in parts {
                let v_str = match part.find('/') {
                    Some(slash) => &part[..slash],
                    None => part,
                };
                if let Ok(idx) = v_str.parse::<isize>() {
                    let zero_based = if idx > 0 {
                        usize::try_from(idx - 1).ok()
                    } else if idx < 0 {
                        let rel = vertices.len() as isize + idx;
                        if rel >= 0 {
                            usize::try_from(rel).ok()
                        } else {
                            None
                        }
                    } else {
                        None
                    };
                    if let Some(zb) = zero_based {
                        line_v.push(zb);
                    }
                }
            }
            if line_v.len() >= 2 {
                wireframe_lines.push(line_v);
            }
        }
    }

    if vertices.is_empty() {
        warnings.push("OBJ file contains no vertex definitions".into());
    }

    // Camera: Standard 3D axonometric isometric view
    let cos30 = (30.0_f64.to_radians()).cos();
    let sin30 = (30.0_f64.to_radians()).sin();

    // Directional light vector from top-left-front
    let light_dir = Vec3 {
        x: -0.5,
        y: 0.5,
        z: 0.707,
    }
    .normalize();

    let project = |p: Vec3| -> (f64, f64, f64) {
        let sx = (p.x - p.y) * cos30;
        let sy = (p.x + p.y) * sin30 - p.z;
        let depth = p.x + p.y + p.z; // for painter's sort
        (sx, sy, depth)
    };

    let mut min_sx = f64::INFINITY;
    let mut min_sy = f64::INFINITY;
    let mut max_sx = f64::NEG_INFINITY;
    let mut max_sy = f64::NEG_INFINITY;

    for v in &vertices {
        let (sx, sy, _) = project(*v);
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

    if min_sx >= max_sx || min_sy >= max_sy {
        min_sx = 0.0;
        min_sy = 0.0;
        max_sx = 100.0;
        max_sy = 100.0;
        warnings.push("OBJ model has zero or invalid bounding volume".into());
    }

    let raw_w = (max_sx - min_sx).max(1e-9);
    let raw_h = (max_sy - min_sy).max(1e-9);
    // Keep the margin proportional to the model. A fixed 20-unit floor made
    // small IFC/meter-based meshes occupy only a few percent of the page.
    let margin = (raw_w.max(raw_h) * 0.1).max(0.01);
    let content_w = raw_w + 2.0 * margin;
    let content_h = raw_h + 2.0 * margin;

    let scale = (TARGET_PAGE_LONG_EDGE / content_w.max(content_h)).clamp(0.01, 10_000.0);
    let page_w = (content_w * scale).max(MIN_PAGE_DIMENSION);
    let page_h = (content_h * scale).max(MIN_PAGE_DIMENSION);

    let mut page = Page::new(1, page_w, page_h, "obj");
    page.title = "Wavefront OBJ 3D Model".into();

    let map_pt = |p: Vec3| -> (f64, f64) {
        let (sx, sy, _) = project(p);
        let px = (sx - min_sx + margin) * scale;
        let py = (sy - min_sy + margin) * scale;
        (px, py)
    };

    // Dark studio background
    page.nodes.push(Node::Path {
        id: "obj-background".into(),
        d: format!(
            "M 0 0 L {:.3} 0 L {:.3} {:.3} L 0 {:.3} Z",
            page_w, page_w, page_h, page_h
        ),
        fill_rule: "nonzero".into(),
        fill: Paint::solid("#0f172a"), // dark studio slate
        stroke: Stroke::default(),
        transform: IDENTITY,
        clip_id: None,
        meta: SourceMeta {
            semantic_role: "obj:background".into(),
            ..Default::default()
        },
    });

    // Face sorting structure for painter's algorithm
    struct RenderFace {
        points: Vec<(f64, f64)>,
        avg_depth: f64,
        shade_color: String,
    }

    let mut render_faces = Vec::new();

    for face in &faces {
        if face.len() < 3 {
            continue;
        }

        // Validate vertex indices
        let mut valid = true;
        for &idx in face {
            if idx >= vertices.len() {
                valid = false;
                break;
            }
        }
        if !valid {
            continue;
        }

        let p0 = vertices[face[0]];
        let p1 = vertices[face[1]];
        let p2 = vertices[face[2]];

        let v_norm = (p1.sub(&p0)).cross(&p2.sub(&p0)).normalize();
        let intensity = (v_norm.dot(&light_dir).abs() * 0.7 + 0.3).clamp(0.15, 1.0);

        // Slate cyan lighting
        let r = (45.0 * intensity) as u8;
        let g = (140.0 * intensity) as u8;
        let b = (230.0 * intensity) as u8;
        let shade_color = format!("#{r:02x}{g:02x}{b:02x}");

        let mut pts = Vec::with_capacity(face.len());
        let mut depth_sum = 0.0;
        for &idx in face {
            pts.push(map_pt(vertices[idx]));
            let (_, _, d) = project(vertices[idx]);
            depth_sum += d;
        }

        render_faces.push(RenderFace {
            points: pts,
            avg_depth: depth_sum / (face.len() as f64),
            shade_color,
        });
    }

    // Sort back-to-front
    render_faces.sort_by(|a, b| {
        a.avg_depth
            .partial_cmp(&b.avg_depth)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    let mut face_nodes = Vec::new();
    for rf in render_faces {
        let mut d = format!("M {:.3} {:.3}", rf.points[0].0, rf.points[0].1);
        for pt in &rf.points[1..] {
            d.push_str(&format!(" L {:.3} {:.3}", pt.0, pt.1));
        }
        d.push_str(" Z");

        face_nodes.push(Node::Path {
            id: String::new(),
            d,
            fill_rule: "nonzero".into(),
            fill: Paint::solid(rf.shade_color),
            stroke: Stroke {
                paint: Paint::solid("#38bdf8"),
                width: 0.5,
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
        id: "obj-mesh".into(),
        transform: IDENTITY,
        clip_id: None,
        opacity: 1.0,
        nodes: face_nodes,
        meta: SourceMeta {
            semantic_role: "obj:mesh".into(),
            ..Default::default()
        },
    });

    let mut line_nodes = Vec::new();
    for line in &wireframe_lines {
        if line.len() < 2 {
            continue;
        }
        let mut d = String::new();
        for (i, &idx) in line.iter().enumerate() {
            if idx < vertices.len() {
                let pt = map_pt(vertices[idx]);
                if i == 0 {
                    d.push_str(&format!("M {:.3} {:.3}", pt.0, pt.1));
                } else {
                    d.push_str(&format!(" L {:.3} {:.3}", pt.0, pt.1));
                }
            }
        }
        if !d.is_empty() {
            line_nodes.push(Node::Path {
                id: String::new(),
                d,
                fill_rule: "nonzero".into(),
                fill: Paint::None,
                stroke: Stroke {
                    paint: Paint::solid("#38bdf8"),
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
    }

    if !line_nodes.is_empty() {
        page.nodes.push(Node::Group {
            id: "obj-lines".into(),
            transform: IDENTITY,
            clip_id: None,
            opacity: 1.0,
            nodes: line_nodes,
            meta: SourceMeta {
                semantic_role: "obj:wireframe".into(),
                ..Default::default()
            },
        });
    }

    sink.consume(page)?;
    Ok(warnings)
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
    fn parses_obj_pyramid_model() {
        let obj_data = r#"
# Sample OBJ pyramid
v 0.0 0.0 0.0
v 50.0 0.0 0.0
v 50.0 50.0 0.0
v 0.0 50.0 0.0
v 25.0 25.0 40.0

f 1 2 3 4
f 1 2 5
f 2 3 5
f 3 4 5
f 4 1 5
"#;
        let mut sink = DummySink(Vec::new());
        let options = ConvertOptions::default();
        let warnings = convert(Cursor::new(obj_data), &options, &mut sink).expect("convert obj");
        assert!(warnings.is_empty());
        assert_eq!(sink.0.len(), 1);
        let page = &sink.0[0];
        assert_eq!(page.nodes.len(), 2);
        if let Node::Group { nodes, .. } = &page.nodes[1] {
            assert_eq!(nodes.len(), 5); // 5 shaded faces
        } else {
            panic!("expected mesh group");
        }
    }

    #[test]
    fn parses_obj_with_non_utf8_comments_and_relative_indices() {
        // Contains negative relative indices (e.g. -3 -2 -1) and non-UTF8 byte in comment (0xB0 for °)
        let mut obj_bytes = Vec::new();
        obj_bytes.extend_from_slice(b"# Temperature 25\xB0C test mesh\n");
        obj_bytes.extend_from_slice(b"v 0 0 0\n");
        obj_bytes.extend_from_slice(b"v 10 0 0\n");
        obj_bytes.extend_from_slice(b"v 0 10 0\n");
        obj_bytes.extend_from_slice(b"f -3 -2 -1\n"); // references the last 3 vertices

        let mut sink = DummySink(Vec::new());
        let options = ConvertOptions::default();
        let warnings = convert(Cursor::new(obj_bytes), &options, &mut sink).expect("convert obj");
        assert!(warnings.is_empty());
        assert_eq!(sink.0.len(), 1);
        let page = &sink.0[0];
        if let Node::Group { nodes, .. } = &page.nodes[1] {
            assert_eq!(nodes.len(), 1); // 1 shaded triangle face
        } else {
            panic!("expected mesh group");
        }
    }
}
