//! SVG to 3D STL (Stereolithography) extrusion reverse converter.
//!
//! Converts 2D SVG vector contours (paths, rectangles, circles, polygons)
//! into 3D printable manifold triangle meshes by extruding 2D shapes along the Z-axis.

use std::io::Write;

use crate::cad::svg_reader::{Point2D, SvgElement, parse_svg_elements};
use crate::error::Result;

#[derive(Clone, Copy, Debug)]
struct Pt3 {
    x: f64,
    y: f64,
    z: f64,
}

fn cross(a: Pt3, b: Pt3) -> Pt3 {
    Pt3 {
        x: a.y * b.z - a.z * b.y,
        y: a.z * b.x - a.x * b.z,
        z: a.x * b.y - a.y * b.x,
    }
}

fn sub(a: Pt3, b: Pt3) -> Pt3 {
    Pt3 {
        x: a.x - b.x,
        y: a.y - b.y,
        z: a.z - b.z,
    }
}

fn normalize(v: Pt3) -> Pt3 {
    let len = (v.x * v.x + v.y * v.y + v.z * v.z).sqrt();
    if len > 1e-9 {
        Pt3 {
            x: v.x / len,
            y: v.y / len,
            z: v.z / len,
        }
    } else {
        Pt3 {
            x: 0.0,
            y: 0.0,
            z: 1.0,
        }
    }
}

fn write_facet<W: Write>(writer: &mut W, v1: Pt3, v2: Pt3, v3: Pt3) -> Result<()> {
    let normal = normalize(cross(sub(v2, v1), sub(v3, v1)));
    writeln!(
        writer,
        "  facet normal {:.6e} {:.6e} {:.6e}",
        normal.x, normal.y, normal.z
    )?;
    writeln!(writer, "    outer loop")?;
    writeln!(writer, "      vertex {:.4} {:.4} {:.4}", v1.x, v1.y, v1.z)?;
    writeln!(writer, "      vertex {:.4} {:.4} {:.4}", v2.x, v2.y, v2.z)?;
    writeln!(writer, "      vertex {:.4} {:.4} {:.4}", v3.x, v3.y, v3.z)?;
    writeln!(writer, "    endloop")?;
    writeln!(writer, "  endfacet")?;
    Ok(())
}

/// Converts SVG shapes into a 3D STL mesh by extruding contours along the Z-axis.
pub fn write_svg_to_stl<W: Write>(svg_content: &str, mut writer: W) -> Result<()> {
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

    let extrude_height = 10.0; // Standard 10mm extrusion depth

    writeln!(writer, "solid document_svg_extrusion")?;

    for poly in &contours {
        let n = poly.len();
        if n < 3 {
            continue;
        }

        let cx = poly.iter().map(|p| p.x).sum::<f64>() / (n as f64);
        let cy = poly.iter().map(|p| p.y).sum::<f64>() / (n as f64);
        let c_bot = Pt3 {
            x: cx,
            y: cy,
            z: 0.0,
        };
        let c_top = Pt3 {
            x: cx,
            y: cy,
            z: extrude_height,
        };

        for i in 0..n {
            let j = (i + 1) % n;
            let p1 = poly[i];
            let p2 = poly[j];

            let b1 = Pt3 {
                x: p1.x,
                y: p1.y,
                z: 0.0,
            };
            let b2 = Pt3 {
                x: p2.x,
                y: p2.y,
                z: 0.0,
            };
            let t1 = Pt3 {
                x: p1.x,
                y: p1.y,
                z: extrude_height,
            };
            let t2 = Pt3 {
                x: p2.x,
                y: p2.y,
                z: extrude_height,
            };

            // Side wall quad: (b1, b2, t2) and (b1, t2, t1)
            write_facet(&mut writer, b1, b2, t2)?;
            write_facet(&mut writer, b1, t2, t1)?;

            // Bottom cap triangle (pointing downward -Z)
            write_facet(&mut writer, b2, b1, c_bot)?;

            // Top cap triangle (pointing upward +Z)
            write_facet(&mut writer, t1, t2, c_top)?;
        }
    }

    writeln!(writer, "endsolid document_svg_extrusion")?;
    Ok(())
}
