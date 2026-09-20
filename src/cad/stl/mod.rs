//! 3D STL (Stereolithography) cross-section slicer and isometric projection renderer.
//!
//! Parses both ASCII and binary STL files, calculates triangle-plane intersections
//! at specified or midpoint Z-elevations, and outputs clean 2D vector slice contours (Slice-to-SVG).

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
const MAX_STL_TRIANGLES: usize = 1_000_000;

#[derive(Clone, Copy, Debug)]
struct Vec3 {
    x: f64,
    y: f64,
    z: f64,
}

#[derive(Clone, Copy, Debug)]
struct Triangle {
    v1: Vec3,
    v2: Vec3,
    v3: Vec3,
}

pub(crate) fn convert<R: Read>(
    input: R,
    options: &ConvertOptions,
    sink: &mut dyn PageConsumer,
) -> Result<Vec<String>> {
    let mut warnings = Vec::new();
    let mut raw_bytes = Vec::new();
    Read::take(input, options.max_input_bytes.saturating_add(1)).read_to_end(&mut raw_bytes)?;

    if raw_bytes.len() as u64 > options.max_input_bytes {
        return Err(Error::LimitExceeded(format!(
            "STL file size exceeds maximum limit of {}",
            options.max_input_bytes
        )));
    }

    let triangles = parse_stl(&raw_bytes)?;
    if triangles.is_empty() {
        warnings.push("STL file contains no valid triangles".into());
    }

    let mut min_x = f64::INFINITY;
    let mut min_y = f64::INFINITY;
    let mut min_z = f64::INFINITY;
    let mut max_x = f64::NEG_INFINITY;
    let mut max_y = f64::NEG_INFINITY;
    let mut max_z = f64::NEG_INFINITY;

    for tri in &triangles {
        for v in [tri.v1, tri.v2, tri.v3] {
            if v.x < min_x {
                min_x = v.x;
            }
            if v.x > max_x {
                max_x = v.x;
            }
            if v.y < min_y {
                min_y = v.y;
            }
            if v.y > max_y {
                max_y = v.y;
            }
            if v.z < min_z {
                min_z = v.z;
            }
            if v.z > max_z {
                max_z = v.z;
            }
        }
    }

    if min_x >= max_x || min_y >= max_y {
        min_x = 0.0;
        min_y = 0.0;
        max_x = 100.0;
        max_y = 100.0;
        min_z = 0.0;
        max_z = 10.0;
    }

    // Slice at midpoint Z plane
    let slice_z = (min_z + max_z) / 2.0;
    let segments = slice_mesh(&triangles, slice_z);

    let raw_w = (max_x - min_x).max(1.0);
    let raw_h = (max_y - min_y).max(1.0);
    let margin = (raw_w.max(raw_h) * 0.1).max(10.0);
    let content_w = raw_w + 2.0 * margin;
    let content_h = raw_h + 2.0 * margin;

    let scale = (TARGET_PAGE_LONG_EDGE / content_w.max(content_h)).clamp(0.01, 100.0);
    let page_w = (content_w * scale).max(MIN_PAGE_DIMENSION);
    let page_h = (content_h * scale).max(MIN_PAGE_DIMENSION);

    let mut page = Page::new(1, page_w, page_h, "stl");
    page.title = format!("STL Cross-Section Slice (Z={:.2}mm)", slice_z);

    let map_pt = |x: f64, y: f64| -> (f64, f64) {
        let sx = (x - min_x + margin) * scale;
        let sy = (max_y - y + margin) * scale; // Y inverted for SVG
        (sx, sy)
    };

    // Dark engineering blueprint background
    page.nodes.push(Node::Path {
        id: "slice-background".into(),
        d: format!(
            "M 0 0 L {} 0 L {} {} L 0 {} Z",
            fmt_coord(page_w),
            fmt_coord(page_w),
            fmt_coord(page_h),
            fmt_coord(page_h)
        ),
        fill_rule: "nonzero".into(),
        fill: Paint::solid("#0f172a"), // Slate 900
        stroke: Stroke::default(),
        transform: IDENTITY,
        clip_id: None,
        meta: SourceMeta {
            semantic_role: "stl:background".into(),
            ..Default::default()
        },
    });

    let contours = link_segments(segments);
    let mut contour_nodes = Vec::new();

    let stroke_slice = Stroke {
        paint: Paint::solid("#38bdf8"), // Sky blue
        width: 1.75,
        line_cap: LineCap::Round,
        line_join: LineJoin::Round,
        miter_limit: 4.0,
        dash_array: Vec::new(),
        dash_offset: 0.0,
    };

    for poly in contours {
        if poly.len() >= 2 {
            let mut d = String::new();
            for (idx, pt) in poly.iter().enumerate() {
                let p = map_pt(pt.0, pt.1);
                if idx == 0 {
                    d.push_str(&format!("M {} {}", fmt_coord(p.0), fmt_coord(p.1)));
                } else {
                    d.push_str(&format!(" L {} {}", fmt_coord(p.0), fmt_coord(p.1)));
                }
            }
            d.push_str(" Z");
            contour_nodes.push(Node::Path {
                id: String::new(),
                d,
                fill_rule: "evenodd".into(),
                fill: Paint::solid("#0284c7"), // Cyan 600
                stroke: stroke_slice.clone(),
                transform: IDENTITY,
                clip_id: None,
                meta: SourceMeta::default(),
            });
        }
    }

    if !contour_nodes.is_empty() {
        page.nodes.push(Node::Group {
            id: "slice-contours".into(),
            nodes: contour_nodes,
            transform: IDENTITY,
            opacity: 0.85,
            clip_id: None,
            meta: SourceMeta {
                semantic_role: "stl:slice".into(),
                ..Default::default()
            },
        });
    }

    // Annotation text (Slice height & triangle count)
    page.nodes.push(Node::Text {
        id: "slice-label".into(),
        x: 20.0,
        y: page_h - 20.0,
        runs: vec![TextRun {
            text: format!(
                "STL Slicer | Z-Height: {:.2} mm | Triangles: {}",
                slice_z,
                triangles.len()
            ),
            font_family: "monospace, monospace".into(),
            font_size: 14.0,
            bold: true,
            italic: false,
            fill: Paint::solid("#94a3b8"),
            baseline_shift: 0.0,
            glyph_x_offsets: Vec::new(),
            target_advance: None,
        }],
        anchor: TextAnchor::Start,
        transform: IDENTITY,
        opacity: 1.0,
        stroke: Stroke::default(),
        clip_id: None,
        meta: SourceMeta::default(),
    });

    sink.consume(page)?;
    Ok(warnings)
}

fn parse_stl(bytes: &[u8]) -> Result<Vec<Triangle>> {
    let raw = match bytes.strip_prefix(b"\xEF\xBB\xBF") {
        Some(rest) => rest,
        None => bytes,
    };

    // Check if exact binary STL: header of 80 bytes + 4-byte count + 50 bytes per triangle
    if raw.len() >= 84 {
        let count_bytes: [u8; 4] = raw[80..84].try_into().unwrap_or([0, 0, 0, 0]);
        let tri_count = u32::from_le_bytes(count_bytes) as usize;
        if raw.len() == 84 + tri_count * 50 {
            return parse_binary_stl(raw);
        }
    }

    let leading = raw
        .iter()
        .skip_while(|b| b.is_ascii_whitespace())
        .take(16)
        .copied()
        .collect::<Vec<_>>();
    let starts_solid = leading.starts_with(b"solid") || leading.starts_with(b"SOLID");
    let has_facet = raw
        .windows(5)
        .take(4096)
        .any(|w| w.eq_ignore_ascii_case(b"facet"));

    if starts_solid && has_facet {
        parse_ascii_stl(raw)
    } else {
        parse_binary_stl(raw)
    }
}

fn parse_ascii_stl(bytes: &[u8]) -> Result<Vec<Triangle>> {
    let mut triangles = Vec::new();
    let mut reader = BufReader::new(bytes);
    let mut vertices = Vec::with_capacity(3);
    let mut line_buf = Vec::new();

    loop {
        line_buf.clear();
        let bytes_read = reader.read_until(b'\n', &mut line_buf)?;
        if bytes_read == 0 {
            break;
        }
        let line = match std::str::from_utf8(&line_buf) {
            Ok(s) => s.to_string(),
            Err(_) => line_buf.iter().map(|&b| b as char).collect::<String>(),
        };
        let trimmed = line.trim();
        if trimmed.starts_with("vertex") {
            let mut fields = trimmed.split_whitespace();
            let token = fields.next();
            if token != Some("vertex") {
                continue;
            }
            let parsed = (
                fields
                    .next()
                    .and_then(crate::cad::dxf::geometry::parse_cad_float),
                fields
                    .next()
                    .and_then(crate::cad::dxf::geometry::parse_cad_float),
                fields
                    .next()
                    .and_then(crate::cad::dxf::geometry::parse_cad_float),
            );
            if let (Some(x), Some(y), Some(z)) = parsed {
                vertices.push(Vec3 { x, y, z });
                if vertices.len() == 3 {
                    triangles.push(Triangle {
                        v1: vertices[0],
                        v2: vertices[1],
                        v3: vertices[2],
                    });
                    vertices.clear();
                    if triangles.len() >= MAX_STL_TRIANGLES {
                        break;
                    }
                }
            }
        }
    }

    Ok(triangles)
}

fn parse_binary_stl(bytes: &[u8]) -> Result<Vec<Triangle>> {
    if bytes.len() < 84 {
        return Ok(Vec::new());
    }

    let count_bytes: [u8; 4] = bytes[80..84].try_into().unwrap_or([0, 0, 0, 0]);
    let tri_count = (u32::from_le_bytes(count_bytes) as usize).min(MAX_STL_TRIANGLES);

    let mut triangles = Vec::with_capacity(tri_count.min(100_000));
    let mut offset = 84;

    for _ in 0..tri_count {
        if offset + 50 > bytes.len() {
            break;
        }

        let read_f32 = |pos: usize| -> f64 {
            let slice: [u8; 4] = bytes[pos..pos + 4].try_into().unwrap_or([0, 0, 0, 0]);
            f32::from_le_bytes(slice) as f64
        };

        // Skip normal (12 bytes)
        let v1 = Vec3 {
            x: read_f32(offset + 12),
            y: read_f32(offset + 16),
            z: read_f32(offset + 20),
        };
        let v2 = Vec3 {
            x: read_f32(offset + 24),
            y: read_f32(offset + 28),
            z: read_f32(offset + 32),
        };
        let v3 = Vec3 {
            x: read_f32(offset + 36),
            y: read_f32(offset + 40),
            z: read_f32(offset + 44),
        };

        triangles.push(Triangle { v1, v2, v3 });
        offset += 50;
    }

    Ok(triangles)
}

/// Slices a 3D triangle mesh with a horizontal plane Z = slice_z.
fn slice_mesh(triangles: &[Triangle], slice_z: f64) -> Vec<((f64, f64), (f64, f64))> {
    let mut segments = Vec::new();

    for tri in triangles {
        let edges = [(tri.v1, tri.v2), (tri.v2, tri.v3), (tri.v3, tri.v1)];
        let mut pts = Vec::new();

        for (a, b) in edges {
            let z_min = a.z.min(b.z);
            let z_max = a.z.max(b.z);

            if slice_z >= z_min && slice_z <= z_max && (b.z - a.z).abs() > 1e-9 {
                let t = (slice_z - a.z) / (b.z - a.z);
                if (0.0..=1.0).contains(&t) {
                    let px = a.x + t * (b.x - a.x);
                    let py = a.y + t * (b.y - a.y);
                    pts.push((px, py));
                }
            }
        }

        if pts.len() == 2
            && ((pts[0].0 - pts[1].0).abs() > 1e-7 || (pts[0].1 - pts[1].1).abs() > 1e-7)
        {
            segments.push((pts[0], pts[1]));
        }
    }

    segments
}

/// Connects loose line segments into closed/continuous 2D polylines.
fn link_segments(segments: Vec<((f64, f64), (f64, f64))>) -> Vec<Vec<(f64, f64)>> {
    let mut polylines = Vec::new();
    const EPS: f64 = 1e-4;
    const EPS_SCALE: f64 = 1.0 / EPS;

    if segments.is_empty() {
        return polylines;
    }

    fn endpoint_key(p: (f64, f64)) -> (i64, i64) {
        if !p.0.is_finite() || !p.1.is_finite() {
            return (i64::MIN, i64::MIN);
        }
        (
            ((p.0 * EPS_SCALE).round() as i64),
            ((p.1 * EPS_SCALE).round() as i64),
        )
    }

    let mut point_to_segments: HashMap<(i64, i64), Vec<(usize, bool)>> =
        HashMap::with_capacity(segments.len() * 2);
    let mut used = vec![false; segments.len()];
    let mut remaining = segments.len();

    for (idx, &(p1, p2)) in segments.iter().enumerate() {
        point_to_segments
            .entry(endpoint_key(p1))
            .or_default()
            .push((idx, false));
        point_to_segments
            .entry(endpoint_key(p2))
            .or_default()
            .push((idx, true));
    }

    while remaining > 0 {
        let start_idx = used.iter().position(|&used| !used);
        let Some(start_idx) = start_idx else { break };

        let (p1, p2) = segments[start_idx];
        used[start_idx] = true;
        remaining -= 1;
        let mut poly = vec![p1, p2];

        let mut extended = true;
        while extended {
            extended = false;
            let last_pt = *poly.last().unwrap();
            let key = endpoint_key(last_pt);

            let Some(bucket) = point_to_segments.get_mut(&key) else {
                continue;
            };

            while let Some((seg_idx, reversed)) = bucket.pop() {
                if used[seg_idx] {
                    continue;
                }

                let (s1, s2) = segments[seg_idx];
                let next_pt = if reversed { s1 } else { s2 };
                poly.push(next_pt);
                used[seg_idx] = true;
                remaining -= 1;
                extended = true;
                break;
            }
        }

        polylines.push(poly);
    }

    polylines
}

fn fmt_coord(v: f64) -> String {
    if !v.is_finite() {
        return "0".to_string();
    }
    let rounded = (v * 1000.0).round() / 1000.0;
    if rounded.fract().abs() < 1e-6 {
        format!("{rounded:.0}")
    } else {
        format!("{rounded}")
    }
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
    fn parses_and_slices_ascii_stl() {
        let stl_text = r#"solid cube
  facet normal 0 0 1
    outer loop
      vertex 0 0 10
      vertex 50 0 10
      vertex 50 50 10
    endloop
  endfacet
  facet normal 0 0 1
    outer loop
      vertex 0 0 10
      vertex 50 50 10
      vertex 0 50 10
    endloop
  endfacet
  facet normal 0 -1 0
    outer loop
      vertex 0 0 0
      vertex 50 0 0
      vertex 50 0 10
    endloop
  endfacet
  facet normal 0 -1 0
    outer loop
      vertex 0 0 0
      vertex 50 0 10
      vertex 0 0 10
    endloop
  endfacet
endsolid cube
"#;
        let mut sink = DummySink(Vec::new());
        let options = ConvertOptions::default();
        let warnings = convert(Cursor::new(stl_text), &options, &mut sink).expect("convert stl");
        assert!(warnings.is_empty());
        assert_eq!(sink.0.len(), 1);
        let page = &sink.0[0];
        assert_eq!(page.source_format, "stl");
        assert!(page.nodes.len() >= 2);
    }

    #[test]
    fn links_segments_without_front_remove_regression() {
        let segments = vec![
            ((0.0, 0.0), (1.0, 0.0)),
            ((1.0, 0.0), (1.0, 1.0)),
            ((1.0, 1.0), (0.0, 1.0)),
            ((0.0, 1.0), (0.0, 0.0)),
        ];
        let polylines = link_segments(segments);
        assert_eq!(polylines.len(), 1);
        assert_eq!(polylines[0].len(), 5);
        assert_eq!(polylines[0][0], polylines[0][4]);
    }

    #[test]
    fn links_disconnected_segments_in_batch() {
        let segments = vec![
            ((0.0, 0.0), (1.0, 0.0)),
            ((2.0, 0.0), (3.0, 0.0)),
            ((1.0, 0.0), (1.0, 1.0)),
            ((2.0, 1.0), (3.0, 1.0)),
        ];
        let polylines = link_segments(segments);
        assert_eq!(polylines.len(), 3);
        let mut lengths: Vec<_> = polylines.iter().map(|poly| poly.len()).collect();
        lengths.sort_unstable();
        assert_eq!(lengths, vec![2, 2, 3]);
    }

    #[test]
    fn parses_stl_with_bom_and_non_utf8_solid_name() {
        let mut stl_bytes = Vec::new();
        // UTF-8 BOM
        stl_bytes.extend_from_slice(b"\xEF\xBB\xBF");
        // solid name with non-UTF8 byte (0xE4 for German ä in Gehäuse)
        stl_bytes.extend_from_slice(b"solid Geh\xE4use\n");
        stl_bytes.extend_from_slice(b"  facet normal 0 0 1\n");
        stl_bytes.extend_from_slice(b"    outer loop\n");
        stl_bytes.extend_from_slice(b"      vertex 0 0 0\n");
        stl_bytes.extend_from_slice(b"      vertex 10 0 0\n");
        stl_bytes.extend_from_slice(b"      vertex 0 10 0\n");
        stl_bytes.extend_from_slice(b"    endloop\n");
        stl_bytes.extend_from_slice(b"  endfacet\n");
        stl_bytes.extend_from_slice(b"endsolid Geh\xE4use\n");

        let mut sink = DummySink(Vec::new());
        let options = ConvertOptions::default();
        let warnings = convert(Cursor::new(stl_bytes), &options, &mut sink).expect("convert stl");
        assert!(warnings.is_empty());
        assert_eq!(sink.0.len(), 1);
    }

    #[test]
    fn parses_binary_stl_with_solid_header_text() {
        // Binary STL where 80-byte header starts with "solid " (very common from CAD tools)
        let mut bin_stl = vec![0u8; 84 + 50]; // 1 triangle
        let header = b"solid generated by CAD exporter";
        bin_stl[..header.len()].copy_from_slice(header);
        // triangle count = 1
        bin_stl[80..84].copy_from_slice(&1u32.to_le_bytes());
        // Normal = (0, 0, 1)
        bin_stl[92..96].copy_from_slice(&1.0f32.to_le_bytes()); // normal.z
        // Vertex 1: (0, 0, 5)
        bin_stl[104..108].copy_from_slice(&5.0f32.to_le_bytes());
        // Vertex 2: (10, 0, 5)
        bin_stl[108..112].copy_from_slice(&10.0f32.to_le_bytes());
        bin_stl[116..120].copy_from_slice(&5.0f32.to_le_bytes());
        // Vertex 3: (0, 10, 5)
        bin_stl[124..128].copy_from_slice(&10.0f32.to_le_bytes());
        bin_stl[128..132].copy_from_slice(&5.0f32.to_le_bytes());

        let mut sink = DummySink(Vec::new());
        let options = ConvertOptions::default();
        let warnings =
            convert(Cursor::new(bin_stl), &options, &mut sink).expect("convert binary stl");
        assert!(warnings.is_empty());
        assert_eq!(sink.0.len(), 1);
    }
}
