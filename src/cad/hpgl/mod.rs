//! HP-GL / HP-GL/2 plotter vector converter to Page IR.

#![allow(
    clippy::collapsible_if,
    clippy::type_complexity,
    clippy::too_many_arguments
)]

pub mod writer;

use std::f64::consts::PI;
use std::io::BufRead;

use crate::cad::dxf::geometry::{BBox, circle_path, fmt_coord};
use crate::convert::{ConvertOptions, PageConsumer};
use crate::error::{Error, Result};
use crate::ir::{
    IDENTITY, LineCap, LineJoin, Node, Page, Paint, SourceMeta, Stroke, TextAnchor, TextRun,
};

const TARGET_PAGE_LONG_EDGE: f64 = 1200.0;
const MIN_PAGE_DIMENSION: f64 = 400.0;
const MAX_HPGL_COMMANDS: usize = 1_000_000;

// Standard 8-pen HP-GL plotter palette
const HPGL_PEN_COLORS: [&str; 9] = [
    "#000000", // 0: No pen / default black
    "#000000", // 1: Black
    "#e02020", // 2: Red
    "#20a020", // 3: Green
    "#e0d020", // 4: Yellow
    "#2040e0", // 5: Blue
    "#d020d0", // 6: Magenta
    "#20c0d0", // 7: Cyan
    "#e08020", // 8: Orange
];

pub(crate) fn convert<R: BufRead>(
    mut reader: R,
    _options: &ConvertOptions,
    sink: &mut dyn PageConsumer,
) -> Result<Vec<String>> {
    let mut warnings = Vec::new();
    let mut buffer = String::new();
    reader.read_to_string(&mut buffer)?;

    // Strip common PCL wrapper escapes: \x1b%-12345X, \x1b%0B, \x1b%1B, \x1bE
    let mut clean_buffer = String::with_capacity(buffer.len());
    let mut chars_iter = buffer.chars().peekable();
    while let Some(c) = chars_iter.next() {
        if c == '\x1b' {
            if let Some(&next_c) = chars_iter.peek() {
                if next_c == '%' {
                    chars_iter.next();
                    while let Some(&sub_c) = chars_iter.peek() {
                        chars_iter.next();
                        if sub_c.is_ascii_alphabetic() {
                            break;
                        }
                    }
                } else if next_c == 'E' {
                    chars_iter.next();
                }
            }
        } else {
            clean_buffer.push(c);
        }
    }

    let mut current_x = 0.0;
    let mut current_y = 0.0;
    let mut pen_down = false;
    let mut current_pen: usize = 1;
    let mut pen_width_mm = 0.35;
    let mut is_relative = false;

    let mut current_subpath: Vec<(f64, f64)> = Vec::new();
    let mut paths_by_pen: Vec<Vec<(Vec<(f64, f64)>, f64)>> = vec![Vec::new(); 9];
    let mut circles: Vec<(f64, f64, f64, usize, f64)> = Vec::new(); // cx, cy, r, pen, width
    let mut labels: Vec<(f64, f64, String, usize)> = Vec::new(); // x, y, text, pen
    let mut fill_rects: Vec<(f64, f64, f64, f64, usize)> = Vec::new(); // x1, y1, x2, y2, pen
    let mut bbox = BBox::new();

    let mut commands_count = 0;

    // Tokens separated by ';' or newlines
    let tokens: Vec<&str> = clean_buffer
        .split([';', '\n', '\r'])
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect();

    for token in tokens {
        commands_count += 1;
        if commands_count > MAX_HPGL_COMMANDS {
            return Err(Error::LimitExceeded(format!(
                "HP-GL commands exceed safety limit of {MAX_HPGL_COMMANDS}"
            )));
        }

        if token.len() < 2 {
            continue;
        }

        let cmd = &token[0..2].to_ascii_uppercase();
        let args_str = token[2..].trim();

        let parse_numbers = || -> Vec<f64> {
            args_str
                .split([',', ' ', '\t'])
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .filter_map(crate::cad::dxf::geometry::parse_cad_float)
                .collect()
        };

        match cmd.as_str() {
            "IN" => {
                // Initialize
                current_x = 0.0;
                current_y = 0.0;
                pen_down = false;
                current_pen = 1;
                is_relative = false;
            }
            "SP" => {
                // Select Pen
                let nums = parse_numbers();
                let pen_id = nums.first().map(|v| *v as usize).unwrap_or(0);
                if pen_id < paths_by_pen.len() {
                    current_pen = pen_id;
                }
            }
            "PW" => {
                // Pen Width (in mm)
                let nums = parse_numbers();
                if let Some(&w) = nums.first() {
                    if w > 0.0 {
                        pen_width_mm = w;
                    }
                }
            }
            "PA" => {
                // Plot Absolute
                is_relative = false;
                let nums = parse_numbers();
                process_points(
                    &nums,
                    is_relative,
                    &mut current_x,
                    &mut current_y,
                    pen_down,
                    &mut current_subpath,
                    &mut paths_by_pen[current_pen],
                    pen_width_mm,
                    &mut bbox,
                );
            }
            "PR" => {
                // Plot Relative
                is_relative = true;
                let nums = parse_numbers();
                process_points(
                    &nums,
                    is_relative,
                    &mut current_x,
                    &mut current_y,
                    pen_down,
                    &mut current_subpath,
                    &mut paths_by_pen[current_pen],
                    pen_width_mm,
                    &mut bbox,
                );
            }
            "PU" => {
                // Pen Up
                pen_down = false;
                if !current_subpath.is_empty() {
                    paths_by_pen[current_pen].push((current_subpath.clone(), pen_width_mm));
                    current_subpath.clear();
                }
                let nums = parse_numbers();
                if !nums.is_empty() {
                    process_points(
                        &nums,
                        is_relative,
                        &mut current_x,
                        &mut current_y,
                        pen_down,
                        &mut current_subpath,
                        &mut paths_by_pen[current_pen],
                        pen_width_mm,
                        &mut bbox,
                    );
                }
            }
            "PD" => {
                // Pen Down
                pen_down = true;
                if current_subpath.is_empty() {
                    current_subpath.push((current_x, current_y));
                    bbox.update(current_x, current_y);
                }
                let nums = parse_numbers();
                if !nums.is_empty() {
                    process_points(
                        &nums,
                        is_relative,
                        &mut current_x,
                        &mut current_y,
                        pen_down,
                        &mut current_subpath,
                        &mut paths_by_pen[current_pen],
                        pen_width_mm,
                        &mut bbox,
                    );
                }
            }
            "CI" => {
                // Circle at current position with radius r
                let nums = parse_numbers();
                if let Some(&r) = nums.first() {
                    if r > 0.0 {
                        bbox.update(current_x - r, current_y - r);
                        bbox.update(current_x + r, current_y + r);
                        circles.push((current_x, current_y, r, current_pen, pen_width_mm));
                    }
                }
            }
            "AA" | "AR" => {
                // Arc Absolute: AA cx, cy, sweep_angle or Arc Relative: AR dx, dy, sweep_angle
                let nums = parse_numbers();
                if nums.len() >= 3 {
                    let (cx, cy) = if cmd.as_str() == "AA" {
                        (nums[0], nums[1])
                    } else {
                        (current_x + nums[0], current_y + nums[1])
                    };
                    let sweep_deg = nums[2];
                    let dx = current_x - cx;
                    let dy = current_y - cy;
                    let r = (dx * dx + dy * dy).sqrt();
                    let start_angle = dy.atan2(dx);
                    let sweep_rad = sweep_deg * PI / 180.0;
                    let steps = (sweep_deg.abs() / 5.0).ceil().clamp(8.0, 72.0) as usize;

                    if pen_down && current_subpath.is_empty() {
                        current_subpath.push((current_x, current_y));
                    }

                    for step in 1..=steps {
                        let t = start_angle + sweep_rad * (step as f64 / steps as f64);
                        let px = cx + r * t.cos();
                        let py = cy + r * t.sin();
                        current_x = px;
                        current_y = py;
                        bbox.update(px, py);
                        if pen_down {
                            current_subpath.push((px, py));
                        }
                    }
                }
            }
            "LB" => {
                let label_text = args_str.trim_end_matches('\x03').to_string();
                if !label_text.is_empty() {
                    bbox.update(current_x, current_y);
                    labels.push((current_x, current_y, label_text, current_pen));
                }
            }
            "EA" | "ER" => {
                let nums = parse_numbers();
                if nums.len() >= 2 {
                    let (x2, y2) = if cmd.as_str() == "EA" {
                        (nums[0], nums[1])
                    } else {
                        (current_x + nums[0], current_y + nums[1])
                    };
                    let rect_pts = vec![
                        (current_x, current_y),
                        (x2, current_y),
                        (x2, y2),
                        (current_x, y2),
                        (current_x, current_y),
                    ];
                    bbox.update(current_x, current_y);
                    bbox.update(x2, y2);
                    paths_by_pen[current_pen].push((rect_pts, pen_width_mm));
                }
            }
            "RA" | "RR" => {
                let nums = parse_numbers();
                if nums.len() >= 2 {
                    let (x2, y2) = if cmd.as_str() == "RA" {
                        (nums[0], nums[1])
                    } else {
                        (current_x + nums[0], current_y + nums[1])
                    };
                    let min_x = current_x.min(x2);
                    let min_y = current_y.min(y2);
                    let max_x = current_x.max(x2);
                    let max_y = current_y.max(y2);
                    bbox.update(min_x, min_y);
                    bbox.update(max_x, max_y);
                    fill_rects.push((min_x, min_y, max_x, max_y, current_pen));
                }
            }
            _ => {
                // Other unsupported HP-GL commands ignored
            }
        }
    }

    if !current_subpath.is_empty() {
        paths_by_pen[current_pen].push((current_subpath, pen_width_mm));
    }

    if !bbox.is_valid() {
        bbox.min_x = 0.0;
        bbox.min_y = 0.0;
        bbox.max_x = 1000.0;
        bbox.max_y = 1000.0;
        warnings.push("HP-GL drawing contains no visible paths; using default bounds".into());
    }

    let raw_w = bbox.width().max(1.0);
    let raw_h = bbox.height().max(1.0);
    let margin = (raw_w.max(raw_h) * 0.05).max(10.0);
    let content_w = raw_w + 2.0 * margin;
    let content_h = raw_h + 2.0 * margin;

    let scale = (TARGET_PAGE_LONG_EDGE / content_w.max(content_h)).clamp(0.0001, 100.0);
    let page_w = (content_w * scale).max(MIN_PAGE_DIMENSION);
    let page_h = (content_h * scale).max(MIN_PAGE_DIMENSION);

    let mut page = Page::new(1, page_w, page_h, "hpgl");
    page.title = "HP-GL Plot".into();

    let map_pt = |x: f64, y: f64| -> (f64, f64) {
        let sx = (x - bbox.min_x + margin) * scale;
        let sy = (bbox.max_y - y + margin) * scale; // Y inverted for SVG
        (sx, sy)
    };

    for (pen_idx, paths) in paths_by_pen.iter().enumerate() {
        if paths.is_empty() {
            continue;
        }
        let color_hex = HPGL_PEN_COLORS[pen_idx.min(HPGL_PEN_COLORS.len() - 1)];
        let mut pen_nodes = Vec::new();

        for (subpath, pw_mm) in paths {
            if subpath.len() >= 2 {
                let stroke_w = (pw_mm * scale * 2.8346).clamp(0.75, 10.0);
                let mut d = String::new();
                for (i, pt) in subpath.iter().enumerate() {
                    let p = map_pt(pt.0, pt.1);
                    if i == 0 {
                        d.push_str(&format!("M {} {}", fmt_coord(p.0), fmt_coord(p.1)));
                    } else {
                        d.push_str(&format!(" L {} {}", fmt_coord(p.0), fmt_coord(p.1)));
                    }
                }
                pen_nodes.push(Node::Path {
                    id: String::new(),
                    d,
                    fill_rule: "nonzero".into(),
                    fill: Paint::None,
                    stroke: Stroke {
                        paint: Paint::solid(color_hex),
                        width: stroke_w,
                        line_cap: LineCap::Round,
                        line_join: LineJoin::Round,
                        miter_limit: 4.0,
                        dash_array: Vec::new(),
                        dash_offset: 0.0,
                    },
                    transform: IDENTITY,
                    clip_id: None,
                    meta: SourceMeta::default(),
                });
            }
        }

        if !pen_nodes.is_empty() {
            page.nodes.push(Node::Group {
                id: format!("pen-{pen_idx}"),
                nodes: pen_nodes,
                transform: IDENTITY,
                opacity: 1.0,
                clip_id: None,
                meta: SourceMeta {
                    semantic_role: format!("hpgl:pen:{pen_idx}"),
                    ..Default::default()
                },
            });
        }
    }

    for (cx, cy, r, pen_idx, pw_mm) in circles {
        let color_hex = HPGL_PEN_COLORS[pen_idx.min(HPGL_PEN_COLORS.len() - 1)];
        let p = map_pt(cx, cy);
        let radius = r * scale;
        let stroke_w = (pw_mm * scale * 2.8346).clamp(0.75, 10.0);
        let d = circle_path(p.0, p.1, radius);
        page.nodes.push(Node::Path {
            id: String::new(),
            d,
            fill_rule: "nonzero".into(),
            fill: Paint::None,
            stroke: Stroke {
                paint: Paint::solid(color_hex),
                width: stroke_w,
                line_cap: LineCap::Round,
                line_join: LineJoin::Round,
                miter_limit: 4.0,
                dash_array: Vec::new(),
                dash_offset: 0.0,
            },
            transform: IDENTITY,
            clip_id: None,
            meta: SourceMeta::default(),
        });
    }

    for (min_x, min_y, max_x, max_y, pen_idx) in fill_rects {
        let color_hex = HPGL_PEN_COLORS[pen_idx.min(HPGL_PEN_COLORS.len() - 1)];
        let p1 = map_pt(min_x, min_y);
        let p2 = map_pt(max_x, max_y);
        let left = p1.0.min(p2.0);
        let right = p1.0.max(p2.0);
        let top = p1.1.min(p2.1);
        let bottom = p1.1.max(p2.1);
        let d = format!(
            "M {} {} H {} V {} H {} Z",
            fmt_coord(left),
            fmt_coord(top),
            fmt_coord(right),
            fmt_coord(bottom),
            fmt_coord(left)
        );
        page.nodes.push(Node::Path {
            id: String::new(),
            d,
            fill_rule: "nonzero".into(),
            fill: Paint::solid(color_hex),
            stroke: Stroke::default(),
            transform: IDENTITY,
            clip_id: None,
            meta: SourceMeta::default(),
        });
    }

    for (lx, ly, text, pen_idx) in labels {
        let color_hex = HPGL_PEN_COLORS[pen_idx.min(HPGL_PEN_COLORS.len() - 1)];
        let p = map_pt(lx, ly);
        page.nodes.push(Node::Text {
            id: String::new(),
            runs: vec![TextRun {
                text,
                font_family: "sans-serif".into(),
                font_size: 12.0,
                bold: false,
                italic: false,
                fill: Paint::solid(color_hex),
                baseline_shift: 0.0,
                glyph_x_offsets: Vec::new(),
                target_advance: None,
            }],
            x: p.0,
            y: p.1,
            anchor: TextAnchor::Start,
            transform: IDENTITY,
            opacity: 1.0,
            stroke: Stroke::default(),
            clip_id: None,
            meta: SourceMeta::default(),
        });
    }

    sink.consume(page)?;
    Ok(warnings)
}

fn process_points(
    nums: &[f64],
    is_relative: bool,
    cur_x: &mut f64,
    cur_y: &mut f64,
    pen_down: bool,
    subpath: &mut Vec<(f64, f64)>,
    finished_paths: &mut Vec<(Vec<(f64, f64)>, f64)>,
    pen_width: f64,
    bbox: &mut BBox,
) {
    let mut i = 0;
    while i + 1 < nums.len() {
        let x = nums[i];
        let y = nums[i + 1];
        if is_relative {
            *cur_x += x;
            *cur_y += y;
        } else {
            *cur_x = x;
            *cur_y = y;
        }
        bbox.update(*cur_x, *cur_y);
        if pen_down {
            subpath.push((*cur_x, *cur_y));
        } else if !subpath.is_empty() {
            finished_paths.push((subpath.clone(), pen_width));
            subpath.clear();
        }
        i += 2;
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
    fn parses_simple_hpgl() {
        let hpgl_text = "IN;SP1;PU0,0;PD1000,0,1000,1000,0,1000,0,0;PU;SP2;CI500;SP0;";
        let mut sink = DummySink(Vec::new());
        let options = ConvertOptions::default();
        let warnings = convert(Cursor::new(hpgl_text), &options, &mut sink).expect("convert hpgl");
        assert!(warnings.is_empty());
        assert_eq!(sink.0.len(), 1);
        let page = &sink.0[0];
        assert!(!page.nodes.is_empty());
    }

    #[test]
    fn parses_hpgl_labels_rectangles_and_arcs() {
        let hpgl_text = "IN;SP1;PA500,500;AA500,500,90;EA1000,1000;RA200,200;LBPLOT TITLE\x03;SP0;";
        let mut sink = DummySink(Vec::new());
        let options = ConvertOptions::default();
        let warnings =
            convert(Cursor::new(hpgl_text), &options, &mut sink).expect("convert hpgl advanced");
        assert!(warnings.is_empty());
        assert_eq!(sink.0.len(), 1);
        let page = &sink.0[0];
        // Ensure at least one Node::Text exists with "PLOT TITLE"
        let has_text = page.nodes.iter().any(|node| match node {
            Node::Text { runs, .. } => runs.iter().any(|r| r.text == "PLOT TITLE"),
            _ => false,
        });
        assert!(has_text, "expected text node for LB");
    }

    #[test]
    fn parses_hpgl_with_pcl_escapes_ar_arc_and_omitted_leading_zeros() {
        // PCL escape prefix \x1b%-12345X and \x1b%0B, followed by HPGL commands with AR and .5
        let hpgl_text =
            "\x1b%-12345X@PJL\r\n\x1b%0BIN;SP1;PA.5,.5;PD10.5,10.5;AR5,0,45;PU;SP0;\x1b%0A";
        let mut sink = DummySink(Vec::new());
        let options = ConvertOptions::default();
        let warnings =
            convert(Cursor::new(hpgl_text), &options, &mut sink).expect("convert hpgl with pcl");
        assert!(warnings.is_empty());
        assert_eq!(sink.0.len(), 1);
        let page = &sink.0[0];
        assert!(!page.nodes.is_empty());
    }
}
