//! Expansion of the classic (pre-R14) `POLYLINE`/`VERTEX`/`SEQEND` group into
//! drawable entities, shared by the DXF reader and the DWG (AC1009) decoder.
//!
//! A classic POLYLINE is not always a single polyline: its `70` flags select
//! between a plain 2D/3D polyline, a *spline-fit* polyline (whose vertex list
//! interleaves the original frame control points with the fit points AutoCAD
//! computed from them), an `M x N` *polygon mesh* (a grid of vertices drawn
//! as its row and column polylines), and a *polyface mesh* (a list of
//! location vertices followed by face records that index into them). Both
//! readers decode the raw flags/counts/indices into a [`ClassicPolyline`]
//! and let [`expand_classic_polyline`] turn it into ordinary entities, so
//! DXF and DWG draw the same shapes for the same data.

use super::types::{Entity, LwVertex};

/// DXF `POLYLINE` group-70 bits (identical to the AC1009 `polyline_flags`
/// byte, whose bits are declared MSB-first in the same order).
pub(crate) const PL_CLOSED: u16 = 1;
pub(crate) const PL_SPLINE_FIT: u16 = 4;
pub(crate) const PL_POLYGON_MESH: u16 = 16;
pub(crate) const PL_MESH_CLOSED_N: u16 = 32;
pub(crate) const PL_POLYFACE_MESH: u16 = 64;

/// DXF `VERTEX` group-70 bits (identical to the AC1009 `vertex_extra_flag`
/// byte).
pub(crate) const VX_SPLINE_FRAME_CONTROL_POINT: u8 = 16;

/// One decoded `VERTEX` record.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct ClassicVertex {
    pub x: f64,
    pub y: f64,
    pub bulge: f64,
    /// Raw group-70 flags (`VX_*`).
    pub flags: u8,
    /// Polyface-mesh face record: 1-based indices into the location
    /// vertices (`71`..`74`); a negative index marks the edge that starts at
    /// that corner as invisible, `0` means "no fourth corner". `None` for a
    /// location vertex.
    pub face: Option<[i16; 4]>,
}

/// One decoded `POLYLINE` header plus its `VERTEX` records.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct ClassicPolyline {
    /// Raw group-70 flags (`PL_*`).
    pub flags: u16,
    /// Polygon-mesh vertex counts (`71`, `72`); `0` when absent.
    pub m_count: usize,
    pub n_count: usize,
    pub vertices: Vec<ClassicVertex>,
    pub layer: String,
    pub color: Option<String>,
    pub line_weight: Option<f64>,
    pub linetype: Option<String>,
}

impl ClassicPolyline {
    fn lw(&self, vertices: Vec<LwVertex>, is_closed: bool) -> Entity {
        Entity::LwPolyline {
            vertices,
            is_closed,
            layer: self.layer.clone(),
            color: self.color.clone(),
            line_weight: self.line_weight,
            linetype: self.linetype.clone(),
        }
    }

    fn line(&self, start: (f64, f64), end: (f64, f64)) -> Entity {
        Entity::Line {
            start,
            end,
            layer: self.layer.clone(),
            color: self.color.clone(),
            line_weight: self.line_weight,
            linetype: self.linetype.clone(),
        }
    }
}

/// Turns a decoded classic POLYLINE group into drawable entities, appended
/// to `out`. Nothing is appended for a group with no usable vertices.
///
/// * Polygon mesh (`16`): with `m_count * n_count` vertices in row-major
///   order, each of the `M` rows and `N` columns becomes its own polyline,
///   closed in the M direction by flag `1` and in the N direction by `32`.
///   If the counts don't match the vertex list the group is drawn as a
///   plain polyline instead, so nothing is lost.
/// * Polyface mesh (`64`): every face record's visible edges become lines
///   between the location vertices it indexes; face records with no valid
///   index and location vertices no face references draw nothing.
/// * Spline-fit (`4`): the frame control points (vertex flag `16`) are
///   dropped and only the fit vertices are drawn — exactly what AutoCAD
///   shows with `SPLFRAME` off. If a file marks every vertex as a frame
///   point the whole list is drawn as-is rather than vanishing.
/// * Anything else (plain, curve-fit, 3D polyline): one polyline through all
///   vertices, keeping bulges, closed by flag `1`.
pub(crate) fn expand_classic_polyline(polyline: ClassicPolyline, out: &mut Vec<Entity>) {
    if polyline.vertices.is_empty() {
        return;
    }
    if polyline.flags & PL_POLYFACE_MESH != 0 {
        expand_polyface(&polyline, out);
        return;
    }
    if polyline.flags & PL_POLYGON_MESH != 0 && expand_polygon_mesh(&polyline, out) {
        return;
    }
    let closed = polyline.flags & PL_CLOSED != 0;
    let drawn: Vec<&ClassicVertex> = if polyline.flags & PL_SPLINE_FIT != 0 {
        let fit: Vec<&ClassicVertex> = polyline
            .vertices
            .iter()
            .filter(|v| v.flags & VX_SPLINE_FRAME_CONTROL_POINT == 0)
            .collect();
        if fit.is_empty() {
            polyline.vertices.iter().collect()
        } else {
            fit
        }
    } else {
        polyline.vertices.iter().collect()
    };
    let vertices = drawn
        .into_iter()
        .map(|v| LwVertex {
            x: v.x,
            y: v.y,
            bulge: v.bulge,
        })
        .collect();
    out.push(polyline.lw(vertices, closed));
}

fn expand_polygon_mesh(polyline: &ClassicPolyline, out: &mut Vec<Entity>) -> bool {
    let (m, n) = (polyline.m_count, polyline.n_count);
    if m < 2 && n < 2 {
        return false;
    }
    let Some(total) = m.checked_mul(n) else {
        return false;
    };
    if m == 0 || n == 0 || total != polyline.vertices.len() {
        return false;
    }
    let closed_m = polyline.flags & PL_CLOSED != 0;
    let closed_n = polyline.flags & PL_MESH_CLOSED_N != 0;
    let at = |i: usize, j: usize| {
        let v = &polyline.vertices[i * n + j];
        LwVertex {
            x: v.x,
            y: v.y,
            bulge: 0.0,
        }
    };
    if n >= 2 {
        for i in 0..m {
            let row = (0..n).map(|j| at(i, j)).collect();
            out.push(polyline.lw(row, closed_n && n > 2));
        }
    }
    if m >= 2 {
        for j in 0..n {
            let column = (0..m).map(|i| at(i, j)).collect();
            out.push(polyline.lw(column, closed_m && m > 2));
        }
    }
    true
}

fn expand_polyface(polyline: &ClassicPolyline, out: &mut Vec<Entity>) {
    let locations: Vec<(f64, f64)> = polyline
        .vertices
        .iter()
        .filter(|v| v.face.is_none())
        .map(|v| (v.x, v.y))
        .collect();
    for face in polyline.vertices.iter().filter_map(|v| v.face) {
        // Keep the corners in order, dropping "no corner" zeros, and
        // remember which edges the sign bit hides.
        let corners: Vec<(usize, bool)> = face
            .iter()
            .filter(|&&idx| idx != 0)
            .filter_map(|&idx| {
                let i = idx.unsigned_abs() as usize;
                (i >= 1 && i <= locations.len()).then_some((i - 1, idx < 0))
            })
            .collect();
        if corners.len() < 2 {
            continue;
        }
        for (k, &(from, invisible)) in corners.iter().enumerate() {
            if invisible {
                continue;
            }
            let to = corners[(k + 1) % corners.len()].0;
            if corners.len() == 2 && k == 1 {
                break; // a two-corner "face" is a single edge, not a back-and-forth pair
            }
            out.push(polyline.line(locations[from], locations[to]));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vx(x: f64, y: f64) -> ClassicVertex {
        ClassicVertex {
            x,
            y,
            ..Default::default()
        }
    }

    fn face(idx: [i16; 4]) -> ClassicVertex {
        ClassicVertex {
            face: Some(idx),
            ..Default::default()
        }
    }

    fn expand(p: ClassicPolyline) -> Vec<Entity> {
        let mut out = Vec::new();
        expand_classic_polyline(p, &mut out);
        out
    }

    #[test]
    fn plain_polyline_keeps_every_vertex_and_bulge() {
        let mut a = vx(0.0, 0.0);
        a.bulge = 1.0;
        let out = expand(ClassicPolyline {
            flags: PL_CLOSED,
            vertices: vec![a, vx(10.0, 0.0), vx(10.0, 10.0)],
            ..Default::default()
        });
        assert_eq!(out.len(), 1);
        match &out[0] {
            Entity::LwPolyline {
                vertices,
                is_closed,
                ..
            } => {
                assert!(is_closed);
                assert_eq!(vertices.len(), 3);
                assert_eq!(vertices[0].bulge, 1.0);
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn spline_fit_polyline_drops_frame_control_points() {
        let mut frame = vx(0.0, 100.0);
        frame.flags = VX_SPLINE_FRAME_CONTROL_POINT;
        let mut fit = vx(1.0, 1.0);
        fit.flags = 8;
        let out = expand(ClassicPolyline {
            flags: PL_SPLINE_FIT,
            vertices: vec![frame, fit, vx(2.0, 2.0)],
            ..Default::default()
        });
        match &out[0] {
            Entity::LwPolyline { vertices, .. } => {
                assert_eq!(vertices.len(), 2);
                assert_eq!((vertices[0].x, vertices[0].y), (1.0, 1.0));
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn spline_fit_polyline_with_only_frame_points_still_draws_them() {
        let mut frame = vx(0.0, 0.0);
        frame.flags = VX_SPLINE_FRAME_CONTROL_POINT;
        let out = expand(ClassicPolyline {
            flags: PL_SPLINE_FIT,
            vertices: vec![frame, frame],
            ..Default::default()
        });
        assert_eq!(out.len(), 1);
    }

    #[test]
    fn polygon_mesh_draws_rows_and_columns() {
        // 2 x 3 grid -> 2 rows + 3 columns.
        let vertices = (0..2)
            .flat_map(|i| (0..3).map(move |j| vx(j as f64, i as f64)))
            .collect();
        let out = expand(ClassicPolyline {
            flags: PL_POLYGON_MESH | PL_MESH_CLOSED_N,
            m_count: 2,
            n_count: 3,
            vertices,
            ..Default::default()
        });
        assert_eq!(out.len(), 5);
        let closed_rows = out
            .iter()
            .filter(|e| matches!(e, Entity::LwPolyline { is_closed: true, vertices, .. } if vertices.len() == 3))
            .count();
        assert_eq!(closed_rows, 2);
        let open_columns = out
            .iter()
            .filter(|e| matches!(e, Entity::LwPolyline { is_closed: false, vertices, .. } if vertices.len() == 2))
            .count();
        assert_eq!(open_columns, 3);
    }

    #[test]
    fn polygon_mesh_with_mismatched_counts_falls_back_to_a_plain_polyline() {
        let out = expand(ClassicPolyline {
            flags: PL_POLYGON_MESH,
            m_count: 4,
            n_count: 4,
            vertices: vec![vx(0.0, 0.0), vx(1.0, 0.0), vx(1.0, 1.0)],
            ..Default::default()
        });
        assert_eq!(out.len(), 1);
        assert!(matches!(&out[0], Entity::LwPolyline { vertices, .. } if vertices.len() == 3));
    }

    #[test]
    fn polyface_mesh_draws_visible_face_edges_only() {
        let out = expand(ClassicPolyline {
            flags: PL_POLYFACE_MESH,
            vertices: vec![
                vx(0.0, 0.0),
                vx(10.0, 0.0),
                vx(10.0, 10.0),
                vx(0.0, 10.0),
                // Quad with the edge from corner 3 (index 3 -> 4) hidden.
                face([1, 2, -3, 4]),
                // Triangle referencing an out-of-range corner: only the two
                // edges among valid corners are drawn.
                face([1, 3, 99, 0]),
            ],
            ..Default::default()
        });
        let lines: Vec<_> = out
            .iter()
            .filter_map(|e| match e {
                Entity::Line { start, end, .. } => Some((*start, *end)),
                _ => None,
            })
            .collect();
        assert_eq!(lines.len(), 3 + 1);
        assert!(lines.contains(&((0.0, 0.0), (10.0, 0.0))));
        assert!(lines.contains(&((10.0, 0.0), (10.0, 10.0))));
        assert!(!lines.contains(&((10.0, 10.0), (0.0, 10.0))));
        assert!(lines.contains(&((0.0, 10.0), (0.0, 0.0))));
        assert!(lines.contains(&((0.0, 0.0), (10.0, 10.0))));
    }

    #[test]
    fn curve_fit_polyline_keeps_extra_vertices() {
        let mut extra = vx(5.0, 5.0);
        extra.flags = 1;
        let out = expand(ClassicPolyline {
            flags: 2, // curve-fit
            vertices: vec![vx(0.0, 0.0), extra, vx(10.0, 0.0)],
            ..Default::default()
        });
        assert!(matches!(&out[0], Entity::LwPolyline { vertices, .. } if vertices.len() == 3));
    }
}
