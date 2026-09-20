//! SVG to CAE Simulation mesh reverse converter (Gmsh .msh & VTK .vtk).
//!
//! Converts 2D SVG vector lines and polygons into standard scientific FEA meshes
//! (Gmsh 2.2 ASCII format) and scientific field visualization datasets (VTK Legacy ASCII POLYDATA).

use std::io::Write;

use crate::cad::svg_reader::{Point2D, SvgElement, parse_svg_elements};
use crate::error::Result;

fn extract_simulation_polys(svg_content: &str) -> Result<Vec<Vec<Point2D>>> {
    let doc = parse_svg_elements(svg_content)?;
    let mut polys: Vec<Vec<Point2D>> = Vec::new();

    for elem in doc.elements {
        match elem {
            SvgElement::Line { p1, p2, .. } => {
                polys.push(vec![p1, p2]);
            }
            SvgElement::Rect {
                x,
                y,
                width,
                height,
                ..
            } => {
                polys.push(vec![
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
                polys.push(pts);
            }
            SvgElement::Polyline { points, .. } => {
                if points.len() >= 2 {
                    polys.push(points);
                }
            }
            SvgElement::Text { .. } => {}
        }
    }

    if polys.is_empty() {
        polys.push(vec![
            Point2D::new(0.0, 0.0),
            Point2D::new(100.0, 0.0),
            Point2D::new(100.0, 100.0),
            Point2D::new(0.0, 100.0),
        ]);
    }

    Ok(polys)
}

/// Converts SVG elements to Gmsh 2.2 ASCII finite element mesh (.msh).
pub fn write_svg_to_gmsh<W: Write>(svg_content: &str, mut writer: W) -> Result<()> {
    let polys = extract_simulation_polys(svg_content)?;

    let mut nodes: Vec<Point2D> = Vec::new();
    let mut elements: Vec<(usize, Vec<usize>)> = Vec::new(); // (element_type, node_indices_1_based)

    for poly in &polys {
        let base_idx = nodes.len() + 1;
        for p in poly {
            nodes.push(*p);
        }
        let n = poly.len();
        if n == 2 {
            // Type 1: 2-node line
            elements.push((1, vec![base_idx, base_idx + 1]));
        } else if n == 3 {
            // Type 2: 3-node triangle
            elements.push((2, vec![base_idx, base_idx + 1, base_idx + 2]));
        } else if n >= 4 {
            // Fan triangulation into 3-node triangles
            for i in 1..n - 1 {
                elements.push((2, vec![base_idx, base_idx + i, base_idx + i + 1]));
            }
        }
    }

    writeln!(writer, "$MeshFormat")?;
    writeln!(writer, "2.2 0 8")?;
    writeln!(writer, "$EndMeshFormat")?;

    writeln!(writer, "$Nodes")?;
    writeln!(writer, "{}", nodes.len())?;
    for (idx, pt) in nodes.iter().enumerate() {
        writeln!(writer, "{} {:.4} {:.4} 0.0", idx + 1, pt.x, pt.y)?;
    }
    writeln!(writer, "$EndNodes")?;

    writeln!(writer, "$Elements")?;
    writeln!(writer, "{}", elements.len())?;
    for (idx, (elm_type, elm_nodes)) in elements.iter().enumerate() {
        write!(writer, "{} {} 2 1 1", idx + 1, elm_type)?;
        for node_id in elm_nodes {
            write!(writer, " {}", node_id)?;
        }
        writeln!(writer)?;
    }
    writeln!(writer, "$EndElements")?;

    Ok(())
}

/// Converts SVG elements to VTK Legacy ASCII POLYDATA dataset (.vtk).
pub fn write_svg_to_vtk<W: Write>(svg_content: &str, mut writer: W) -> Result<()> {
    let polys = extract_simulation_polys(svg_content)?;

    let mut points: Vec<Point2D> = Vec::new();
    let mut poly_cells: Vec<Vec<usize>> = Vec::new(); // 0-based indices

    for poly in &polys {
        let base_idx = points.len();
        for p in poly {
            points.push(*p);
        }
        let n = poly.len();
        if n >= 3 {
            let idxs: Vec<usize> = (0..n).map(|i| base_idx + i).collect();
            poly_cells.push(idxs);
        } else if n == 2 {
            poly_cells.push(vec![base_idx, base_idx + 1]);
        }
    }

    writeln!(writer, "# vtk DataFile Version 3.0")?;
    writeln!(writer, "document-svg exported polygonal dataset")?;
    writeln!(writer, "ASCII")?;
    writeln!(writer, "DATASET POLYDATA")?;

    writeln!(writer, "POINTS {} float", points.len())?;
    for p in &points {
        writeln!(writer, "{:.4} {:.4} 0.0", p.x, p.y)?;
    }

    let total_size: usize = poly_cells.iter().map(|c| c.len() + 1).sum();
    writeln!(writer, "POLYGONS {} {}", poly_cells.len(), total_size)?;
    for cell in &poly_cells {
        write!(writer, "{}", cell.len())?;
        for idx in cell {
            write!(writer, " {}", idx)?;
        }
        writeln!(writer)?;
    }

    // Add sample scalar field data
    writeln!(writer, "POINT_DATA {}", points.len())?;
    writeln!(writer, "SCALARS elevation float 1")?;
    writeln!(writer, "LOOKUP_TABLE default")?;
    for p in &points {
        let val = (p.x * 0.5 + p.y * 0.5).max(0.0);
        writeln!(writer, "{:.4}", val)?;
    }

    Ok(())
}

/// Converts SVG elements to Stanford ASCII PLY dataset (.ply).
pub fn write_svg_to_ply<W: Write>(svg_content: &str, mut writer: W) -> Result<()> {
    let polys = extract_simulation_polys(svg_content)?;

    let mut vertices: Vec<(f64, f64, f64)> = Vec::new();
    let mut faces: Vec<Vec<usize>> = Vec::new();

    let extrude_z = 5.0;

    for poly in &polys {
        let n = poly.len();
        if n < 3 {
            continue;
        }
        let base_idx = vertices.len();

        // Bottom vertices (z = 0.0)
        for p in poly {
            vertices.push((p.x, p.y, 0.0));
        }
        // Top vertices (z = extrude_z)
        for p in poly {
            vertices.push((p.x, p.y, extrude_z));
        }

        // Bottom face (reversed order for outward normal)
        let bottom_face: Vec<usize> = (0..n).rev().map(|i| base_idx + i).collect();
        faces.push(bottom_face);

        // Top face
        let top_face: Vec<usize> = (0..n).map(|i| base_idx + n + i).collect();
        faces.push(top_face);

        // Side quad faces
        for i in 0..n {
            let next = (i + 1) % n;
            let b0 = base_idx + i;
            let b1 = base_idx + next;
            let t1 = base_idx + n + next;
            let t0 = base_idx + n + i;
            faces.push(vec![b0, b1, t1, t0]);
        }
    }

    writeln!(writer, "ply")?;
    writeln!(writer, "format ascii 1.0")?;
    writeln!(writer, "comment Exported by document-svg")?;
    writeln!(writer, "element vertex {}", vertices.len())?;
    writeln!(writer, "property float x")?;
    writeln!(writer, "property float y")?;
    writeln!(writer, "property float z")?;
    writeln!(writer, "element face {}", faces.len())?;
    writeln!(writer, "property list uchar int vertex_indices")?;
    writeln!(writer, "end_header")?;

    for (x, y, z) in &vertices {
        writeln!(writer, "{:.4} {:.4} {:.4}", x, y, z)?;
    }

    for f in &faces {
        write!(writer, "{}", f.len())?;
        for idx in f {
            write!(writer, " {}", idx)?;
        }
        writeln!(writer)?;
    }

    Ok(())
}
