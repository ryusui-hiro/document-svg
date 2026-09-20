//! G-code toolpath parser and SVG vector renderer.
//!
//! Visualizes CNC milling, laser cutting, and 3D printing G-code programs
//! with distinguished rapid motions (dashed blue) and cutting feeds (solid orange).

pub mod writer;

use std::io::{BufRead, BufReader, Read};

use crate::convert::{ConvertOptions, PageConsumer};
use crate::error::{Error, Result};
use crate::ir::{IDENTITY, LineCap, LineJoin, Node, Page, Paint, SourceMeta, Stroke};

const TARGET_PAGE_LONG_EDGE: f64 = 1200.0;
const MIN_PAGE_DIMENSION: f64 = 400.0;
const MAX_GCODE_MOVES: usize = 1_000_000;

#[derive(Clone, Debug)]
#[allow(dead_code)]
enum ToolpathMove {
    Rapid {
        from: (f64, f64),
        to: (f64, f64),
    },
    CutLine {
        from: (f64, f64),
        to: (f64, f64),
        z: f64,
        feed: f64,
    },
    CutArc {
        from: (f64, f64),
        to: (f64, f64),
        cx: f64,
        cy: f64,
        clockwise: bool,
        z: f64,
    },
}

pub(crate) fn convert<R: Read>(
    input: R,
    options: &ConvertOptions,
    sink: &mut dyn PageConsumer,
) -> Result<Vec<String>> {
    let mut warnings = Vec::new();
    let mut reader = BufReader::new(input);

    let mut is_relative = false;
    let mut unit_scale = 1.0; // G21: 1.0 (mm), G20: 25.4 (inches)
    let mut active_mode = 0; // 0: G00 rapid, 1: G01 cut, 2: G02 CW arc, 3: G03 CCW arc
    let mut cur_x = 0.0;
    let mut cur_y = 0.0;
    let mut cur_z = 0.0;
    let mut cur_feed = 1000.0;

    let mut min_x = f64::INFINITY;
    let mut min_y = f64::INFINITY;
    let mut max_x = f64::NEG_INFINITY;
    let mut max_y = f64::NEG_INFINITY;

    let mut moves: Vec<ToolpathMove> = Vec::new();
    let mut byte_count: u64 = 0;

    let update_bbox =
        |x: f64, y: f64, min_x: &mut f64, min_y: &mut f64, max_x: &mut f64, max_y: &mut f64| {
            if x < *min_x {
                *min_x = x;
            }
            if x > *max_x {
                *max_x = x;
            }
            if y < *min_y {
                *min_y = y;
            }
            if y > *max_y {
                *max_y = y;
            }
        };

    update_bbox(0.0, 0.0, &mut min_x, &mut min_y, &mut max_x, &mut max_y);

    let mut raw_line = Vec::new();

    loop {
        raw_line.clear();
        let bytes_read = reader.read_until(b'\n', &mut raw_line)?;
        if bytes_read == 0 {
            break;
        }
        byte_count += bytes_read as u64;
        if byte_count > options.max_input_bytes {
            return Err(Error::LimitExceeded(format!(
                "G-code input exceeds maximum bytes limit of {}",
                options.max_input_bytes
            )));
        }

        let line = match std::str::from_utf8(&raw_line) {
            Ok(s) => s.to_string(),
            Err(_) => raw_line.iter().map(|&b| b as char).collect(),
        };

        let cleaned = strip_comments(&line);
        let trimmed = cleaned.trim();
        if trimmed.is_empty() {
            continue;
        }

        let tokens = tokenize_gcode(trimmed);
        let mut target_x = None;
        let mut target_y = None;
        let mut target_z = None;
        let mut arc_i = None;
        let mut arc_j = None;
        let mut arc_r = None;
        let mut is_g92 = false;

        for (letter, val) in tokens {
            match letter {
                'G' => {
                    let code = val.round() as i32;
                    match code {
                        0 => active_mode = 0,
                        1 => active_mode = 1,
                        2 => active_mode = 2,
                        3 => active_mode = 3,
                        20 => unit_scale = 25.4,
                        21 => unit_scale = 1.0,
                        90 => is_relative = false,
                        91 => is_relative = true,
                        92 => is_g92 = true,
                        _ => {}
                    }
                }
                'X' => target_x = Some(val * unit_scale),
                'Y' => target_y = Some(val * unit_scale),
                'Z' => target_z = Some(val * unit_scale),
                'I' => arc_i = Some(val * unit_scale),
                'J' => arc_j = Some(val * unit_scale),
                'R' => arc_r = Some(val * unit_scale),
                'F' => cur_feed = val * unit_scale,
                _ => {}
            }
        }

        if is_g92 {
            if let Some(x) = target_x {
                cur_x = x;
            }
            if let Some(y) = target_y {
                cur_y = y;
            }
            if let Some(z) = target_z {
                cur_z = z;
            }
            continue;
        }

        let next_x = if let Some(x) = target_x {
            if is_relative { cur_x + x } else { x }
        } else {
            cur_x
        };

        let next_y = if let Some(y) = target_y {
            if is_relative { cur_y + y } else { y }
        } else {
            cur_y
        };

        let next_z = if let Some(z) = target_z {
            if is_relative { cur_z + z } else { z }
        } else {
            cur_z
        };

        let is_arc = (active_mode == 2 || active_mode == 3)
            && (arc_i.is_some() || arc_j.is_some() || arc_r.is_some());
        let xy_moved = (next_x - cur_x).abs() > 1e-7 || (next_y - cur_y).abs() > 1e-7;

        if is_arc {
            let clockwise = active_mode == 2;
            let (cx, cy) = if let Some(r) = arc_r {
                let dx = next_x - cur_x;
                let dy = next_y - cur_y;
                let d_sq = dx * dx + dy * dy;
                let r_abs = r.abs();
                let h_sq = (r_abs * r_abs - d_sq / 4.0).max(0.0);
                let h = h_sq.sqrt();
                let mx = (cur_x + next_x) / 2.0;
                let my = (cur_y + next_y) / 2.0;
                let sign = if (clockwise && r > 0.0) || (!clockwise && r < 0.0) {
                    -1.0
                } else {
                    1.0
                };
                let d = d_sq.sqrt().max(1e-9);
                (mx + sign * h * (-dy / d), my + sign * h * (dx / d))
            } else {
                let i = arc_i.unwrap_or(0.0);
                let j = arc_j.unwrap_or(0.0);
                (cur_x + i, cur_y + j)
            };

            let r = ((cur_x - cx).powi(2) + (cur_y - cy).powi(2)).sqrt();
            update_bbox(
                cx - r,
                cy - r,
                &mut min_x,
                &mut min_y,
                &mut max_x,
                &mut max_y,
            );
            update_bbox(
                cx + r,
                cy + r,
                &mut min_x,
                &mut min_y,
                &mut max_x,
                &mut max_y,
            );

            moves.push(ToolpathMove::CutArc {
                from: (cur_x, cur_y),
                to: (next_x, next_y),
                cx,
                cy,
                clockwise,
                z: next_z,
            });
        } else if xy_moved {
            update_bbox(
                next_x, next_y, &mut min_x, &mut min_y, &mut max_x, &mut max_y,
            );
            match active_mode {
                0 => {
                    moves.push(ToolpathMove::Rapid {
                        from: (cur_x, cur_y),
                        to: (next_x, next_y),
                    });
                }
                1 => {
                    moves.push(ToolpathMove::CutLine {
                        from: (cur_x, cur_y),
                        to: (next_x, next_y),
                        z: next_z,
                        feed: cur_feed,
                    });
                }
                _ => {}
            }
        }

        cur_x = next_x;
        cur_y = next_y;
        cur_z = next_z;

        if moves.len() >= MAX_GCODE_MOVES {
            warnings.push("G-code moves limit reached; subsequent moves truncated".into());
            break;
        }
    }

    if min_x >= max_x || min_y >= max_y {
        min_x = 0.0;
        min_y = 0.0;
        max_x = 100.0;
        max_y = 100.0;
        warnings.push(
            "G-code contains no planar XY tool movements; using default canvas bounds".into(),
        );
    }

    let raw_w = (max_x - min_x).max(1.0);
    let raw_h = (max_y - min_y).max(1.0);
    let margin = (raw_w.max(raw_h) * 0.05).max(10.0);
    let content_w = raw_w + 2.0 * margin;
    let content_h = raw_h + 2.0 * margin;

    let scale = (TARGET_PAGE_LONG_EDGE / content_w.max(content_h)).clamp(0.001, 100.0);
    let page_w = (content_w * scale).max(MIN_PAGE_DIMENSION);
    let page_h = (content_h * scale).max(MIN_PAGE_DIMENSION);

    let mut page = Page::new(1, page_w, page_h, "gcode");
    page.title = "CNC Toolpath".into();

    let map_pt = |x: f64, y: f64| -> (f64, f64) {
        let sx = (x - min_x + margin) * scale;
        let sy = (max_y - y + margin) * scale; // Y inverted for SVG
        (sx, sy)
    };

    // Dark CAM simulator substrate background
    page.nodes.push(Node::Path {
        id: "cam-workspace".into(),
        d: format!(
            "M 0 0 L {} 0 L {} {} L 0 {} Z",
            fmt_coord(page_w),
            fmt_coord(page_w),
            fmt_coord(page_h),
            fmt_coord(page_h)
        ),
        fill_rule: "nonzero".into(),
        fill: Paint::solid("#18181b"),
        stroke: Stroke::default(),
        transform: IDENTITY,
        clip_id: None,
        meta: SourceMeta {
            semantic_role: "cam:workspace".into(),
            ..Default::default()
        },
    });

    let mut rapid_nodes = Vec::new();
    let mut cut_nodes = Vec::new();

    let rapid_stroke = Stroke {
        paint: Paint::solid("#38bdf8"), // Sky blue
        width: 1.0,
        line_cap: LineCap::Round,
        line_join: LineJoin::Round,
        miter_limit: 4.0,
        dash_array: vec![4.0, 4.0],
        dash_offset: 0.0,
    };

    let cut_stroke = Stroke {
        paint: Paint::solid("#f97316"), // Bright orange
        width: 1.75,
        line_cap: LineCap::Round,
        line_join: LineJoin::Round,
        miter_limit: 4.0,
        dash_array: Vec::new(),
        dash_offset: 0.0,
    };

    for m in moves {
        match m {
            ToolpathMove::Rapid { from, to } => {
                let p1 = map_pt(from.0, from.1);
                let p2 = map_pt(to.0, to.1);
                let d = format!(
                    "M {} {} L {} {}",
                    fmt_coord(p1.0),
                    fmt_coord(p1.1),
                    fmt_coord(p2.0),
                    fmt_coord(p2.1)
                );
                rapid_nodes.push(Node::Path {
                    id: String::new(),
                    d,
                    fill_rule: "nonzero".into(),
                    fill: Paint::None,
                    stroke: rapid_stroke.clone(),
                    transform: IDENTITY,
                    clip_id: None,
                    meta: SourceMeta::default(),
                });
            }
            ToolpathMove::CutLine { from, to, .. } => {
                let p1 = map_pt(from.0, from.1);
                let p2 = map_pt(to.0, to.1);
                let d = format!(
                    "M {} {} L {} {}",
                    fmt_coord(p1.0),
                    fmt_coord(p1.1),
                    fmt_coord(p2.0),
                    fmt_coord(p2.1)
                );
                cut_nodes.push(Node::Path {
                    id: String::new(),
                    d,
                    fill_rule: "nonzero".into(),
                    fill: Paint::None,
                    stroke: cut_stroke.clone(),
                    transform: IDENTITY,
                    clip_id: None,
                    meta: SourceMeta::default(),
                });
            }
            ToolpathMove::CutArc {
                from,
                to,
                cx,
                cy,
                clockwise,
                ..
            } => {
                let p1 = map_pt(from.0, from.1);
                let p2 = map_pt(to.0, to.1);
                let r = ((from.0 - cx).powi(2) + (from.1 - cy).powi(2)).sqrt() * scale;
                // SVG Y is inverted, so CW in CNC (sweep 1) becomes CCW in SVG (sweep 0)
                let sweep = if clockwise { 0 } else { 1 };
                let dist = ((from.0 - to.0).powi(2) + (from.1 - to.1).powi(2)).sqrt();

                let d = if dist < 1e-5 && r > 1e-5 {
                    // Full 360-degree circle: two 180-degree arcs to avoid degenerate SVG arc
                    let opp_x = 2.0 * cx - from.0;
                    let opp_y = 2.0 * cy - from.1;
                    let p_mid = map_pt(opp_x, opp_y);
                    format!(
                        "M {} {} A {} {} 0 0 {} {} {} A {} {} 0 0 {} {} {}",
                        fmt_coord(p1.0),
                        fmt_coord(p1.1),
                        fmt_coord(r),
                        fmt_coord(r),
                        sweep,
                        fmt_coord(p_mid.0),
                        fmt_coord(p_mid.1),
                        fmt_coord(r),
                        fmt_coord(r),
                        sweep,
                        fmt_coord(p1.0),
                        fmt_coord(p1.1)
                    )
                } else {
                    let theta_1 = (from.1 - cy).atan2(from.0 - cx);
                    let theta_2 = (to.1 - cy).atan2(to.0 - cx);
                    let mut delta = if clockwise {
                        theta_1 - theta_2
                    } else {
                        theta_2 - theta_1
                    };
                    while delta < 0.0 {
                        delta += 2.0 * std::f64::consts::PI;
                    }
                    while delta >= 2.0 * std::f64::consts::PI {
                        delta -= 2.0 * std::f64::consts::PI;
                    }
                    let large_arc = if delta > std::f64::consts::PI { 1 } else { 0 };
                    format!(
                        "M {} {} A {} {} 0 {} {} {} {}",
                        fmt_coord(p1.0),
                        fmt_coord(p1.1),
                        fmt_coord(r),
                        fmt_coord(r),
                        large_arc,
                        sweep,
                        fmt_coord(p2.0),
                        fmt_coord(p2.1)
                    )
                };
                cut_nodes.push(Node::Path {
                    id: String::new(),
                    d,
                    fill_rule: "nonzero".into(),
                    fill: Paint::None,
                    stroke: cut_stroke.clone(),
                    transform: IDENTITY,
                    clip_id: None,
                    meta: SourceMeta::default(),
                });
            }
        }
    }

    if !rapid_nodes.is_empty() {
        page.nodes.push(Node::Group {
            id: "rapid-traverse".into(),
            nodes: rapid_nodes,
            transform: IDENTITY,
            opacity: 0.75,
            clip_id: None,
            meta: SourceMeta {
                semantic_role: "gcode:rapid".into(),
                ..Default::default()
            },
        });
    }

    if !cut_nodes.is_empty() {
        page.nodes.push(Node::Group {
            id: "cutting-toolpath".into(),
            nodes: cut_nodes,
            transform: IDENTITY,
            opacity: 1.0,
            clip_id: None,
            meta: SourceMeta {
                semantic_role: "gcode:cut".into(),
                ..Default::default()
            },
        });
    }

    sink.consume(page)?;
    Ok(warnings)
}

fn strip_comments(line: &str) -> String {
    let mut out = String::new();
    let mut in_paren = false;
    for c in line.chars() {
        if c == ';' {
            break;
        } else if c == '(' {
            in_paren = true;
        } else if c == ')' {
            in_paren = false;
        } else if !in_paren {
            out.push(c);
        }
    }
    out
}

fn tokenize_gcode(s: &str) -> Vec<(char, f64)> {
    let mut tokens = Vec::new();
    let mut chars = s.chars().peekable();

    while let Some(&c) = chars.peek() {
        if c.is_ascii_alphabetic() {
            let letter = c.to_ascii_uppercase();
            chars.next();
            let mut num_str = String::new();
            while let Some(&nc) = chars.peek() {
                if nc.is_ascii_digit() || nc == '.' || nc == '-' || nc == '+' {
                    num_str.push(nc);
                    chars.next();
                } else if nc.is_ascii_whitespace() {
                    chars.next();
                } else {
                    break;
                }
            }
            if let Some(val) = crate::cad::dxf::geometry::parse_cad_float(&num_str) {
                tokens.push((letter, val));
            }
        } else {
            chars.next();
        }
    }

    tokens
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
    fn parses_simple_gcode() {
        let gcode = r#"(Header comment)
G21 G90
G00 X0.0 Y0.0 Z5.0
M03 S1200
G01 Z-1.0 F300
G01 X50.0 Y0.0 F1200
G02 X100.0 Y50.0 I0.0 J50.0
G01 X100.0 Y100.0
M05
G00 Z10.0
G00 X0.0 Y0.0
M02
"#;
        let mut sink = DummySink(Vec::new());
        let options = ConvertOptions::default();
        let warnings = convert(Cursor::new(gcode), &options, &mut sink).expect("convert gcode");
        assert!(warnings.is_empty());
        assert_eq!(sink.0.len(), 1);
        let page = &sink.0[0];
        assert_eq!(page.source_format, "gcode");
        assert!(page.nodes.len() >= 2); // workspace substrate + cutting group
    }

    #[test]
    fn parses_gcode_with_non_utf8_comments() {
        // G-code containing Latin-1 characters in parentheses comment: 0xB0 (°), 0xE9 (é)
        let gcode_bytes =
            b"(Temperature: 210\xB0C, Pi\xE8ce)\nG21 G90\nG01 X10.0 Y20.0 F500\n".to_vec();
        let mut sink = DummySink(Vec::new());
        let options = ConvertOptions::default();
        let warnings = convert(Cursor::new(gcode_bytes), &options, &mut sink)
            .expect("convert gcode with Latin-1");
        assert!(warnings.is_empty());
        assert_eq!(sink.0.len(), 1);
    }
}
