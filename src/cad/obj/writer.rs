//! SVG to 3D Wavefront OBJ extrusion reverse converter.
//!
//! Converts 2D SVG vector shapes into 3D polygonal Wavefront OBJ models
//! with extruded side walls and triangulated top/bottom caps.

use std::io::Write;

use crate::cad::svg_reader::{Point2D, SvgElement, parse_svg_elements};
use crate::error::Result;

#[derive(Clone, Copy, Debug)]
struct Pt3 {
    x: f64,
    y: f64,
    z: f64,
}

/// Converts SVG shapes into a 3D Wavefront OBJ model by extruding contours along the Z-axis.
pub fn write_svg_to_obj<W: Write>(svg_content: &str, mut writer: W) -> Result<()> {
    let doc = parse_svg_elements(svg_content)?;

    let mut contours: Vec<Vec<Point2D>> = Vec::new();

    for elem in doc.elements {
        match elem {
            SvgElement::Rect {
                x,
                y,
                width,
                height,
                ..
            } => {
                contours.push(vec![
                    Point2D::new(x, y),
                    Point2D::new(x + width, y),
                    Point2D::new(x + width, y + height),
                    Point2D::new(x, y + height),
                ]);
            }
            SvgElement::Circle { center, radius, .. } => {
                let segments = crate::cad::DEFAULT_CIRCLE_SEGMENTS;
                let mut pts = Vec::with_capacity(segments);
                for i in 0..segments {
                    let theta = (i as f64) * std::f64::consts::TAU / (segments as f64);
                    pts.push(Point2D::new(
                        center.x + radius * theta.cos(),
                        center.y + radius * theta.sin(),
                    ));
                }
                contours.push(pts);
            }
            SvgElement::Polyline { points, .. } => {
                if points.len() >= 3 {
                    contours.push(points);
                }
            }
            _ => {}
        }
    }

    if contours.is_empty() {
        contours.push(vec![
            Point2D::new(0.0, 0.0),
            Point2D::new(100.0, 0.0),
            Point2D::new(100.0, 100.0),
            Point2D::new(0.0, 100.0),
        ]);
    }

    let extrude_height = 10.0;
    let mut vertices: Vec<Pt3> = Vec::new();
    let mut faces: Vec<[usize; 3]> = Vec::new(); // 1-based vertex indices

    for poly in &contours {
        let n = poly.len();
        if n < 3 {
            continue;
        }

        let base_idx = vertices.len() + 1; // 1-based start index

        // Bottom vertices: 0..n
        for p in poly {
            vertices.push(Pt3 {
                x: p.x,
                y: p.y,
                z: 0.0,
            });
        }
        // Top vertices: n..2n
        for p in poly {
            vertices.push(Pt3 {
                x: p.x,
                y: p.y,
                z: extrude_height,
            });
        }
        // Centroids
        let cx = poly.iter().map(|p| p.x).sum::<f64>() / (n as f64);
        let cy = poly.iter().map(|p| p.y).sum::<f64>() / (n as f64);
        let c_bot_idx = base_idx + 2 * n;
        vertices.push(Pt3 {
            x: cx,
            y: cy,
            z: 0.0,
        });
        let c_top_idx = base_idx + 2 * n + 1;
        vertices.push(Pt3 {
            x: cx,
            y: cy,
            z: extrude_height,
        });

        for i in 0..n {
            let j = (i + 1) % n;
            let b1 = base_idx + i;
            let b2 = base_idx + j;
            let t1 = base_idx + n + i;
            let t2 = base_idx + n + j;

            // Side wall quad split into 2 triangles
            faces.push([b1, b2, t2]);
            faces.push([b1, t2, t1]);

            // Bottom cap
            faces.push([b2, b1, c_bot_idx]);

            // Top cap
            faces.push([t1, t2, c_top_idx]);
        }
    }

    writeln!(
        writer,
        "# Wavefront OBJ extruded 3D model created by document-svg"
    )?;
    writeln!(writer, "o ExtrudedSVG")?;

    for v in &vertices {
        writeln!(writer, "v {:.4} {:.4} {:.4}", v.x, v.y, v.z)?;
    }

    for f in &faces {
        writeln!(writer, "f {} {} {}", f[0], f[1], f[2])?;
    }

    Ok(())
}
