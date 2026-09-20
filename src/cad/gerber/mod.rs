//! Gerber RS-274X (Extended Gerber) PCB layout converter to Page IR.

#![allow(clippy::collapsible_if)]

pub mod writer;

use std::collections::HashMap;
use std::f64::consts::PI;
use std::io::BufRead;

use crate::cad::dxf::geometry::{BBox, circle_path, fmt_coord, parse_cad_float};
use crate::convert::{ConvertOptions, PageConsumer};
use crate::error::{Error, Result};
use crate::ir::{IDENTITY, LineCap, LineJoin, Node, Page, Paint, SourceMeta, Stroke};

const TARGET_PAGE_LONG_EDGE: f64 = 1200.0;
const MIN_PAGE_DIMENSION: f64 = 400.0;
const MAX_GERBER_COMMANDS: usize = 1_000_000;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Polarity {
    Dark,
    Clear,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum InterpolationMode {
    Linear,
    Clockwise,
    CounterClockwise,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Aperture {
    Circle {
        diameter: f64,
        hole_diameter: Option<f64>,
    },
    Rectangle {
        width: f64,
        height: f64,
        hole_diameter: Option<f64>,
    },
    Obround {
        width: f64,
        height: f64,
        hole_diameter: Option<f64>,
    },
    Polygon {
        diameter: f64,
        vertices: usize,
        rotation: Option<f64>,
        hole_diameter: Option<f64>,
    },
}

#[derive(Clone, Debug)]
pub struct CoordinateFormat {
    pub x_int: usize,
    pub x_dec: usize,
    pub y_int: usize,
    pub y_dec: usize,
    pub suppress_leading_zeros: bool,
}

impl Default for CoordinateFormat {
    fn default() -> Self {
        Self {
            x_int: 2,
            x_dec: 4,
            y_int: 2,
            y_dec: 4,
            suppress_leading_zeros: true,
        }
    }
}

impl CoordinateFormat {
    pub fn parse_coord(&self, s: &str, is_y: bool) -> f64 {
        if s.is_empty() {
            return 0.0;
        }
        // Direct decimal support
        if s.contains('.') {
            return parse_cad_float(s).unwrap_or(0.0);
        }
        let sign = if s.starts_with('-') { -1.0 } else { 1.0 };
        let num_part = s.trim_start_matches(['+', '-']);
        let dec_digits = if is_y { self.y_dec } else { self.x_dec };
        let total_digits = if is_y {
            self.y_int + self.y_dec
        } else {
            self.x_int + self.x_dec
        };

        let padded = if self.suppress_leading_zeros {
            if num_part.len() < total_digits {
                format!("{:0>width$}", num_part, width = total_digits)
            } else {
                num_part.to_string()
            }
        } else if num_part.len() < total_digits {
            format!("{:0<width$}", num_part, width = total_digits)
        } else {
            num_part.to_string()
        };

        let raw_val: f64 = padded.parse().unwrap_or(0.0);
        sign * (raw_val / 10f64.powi(dec_digits as i32))
    }
}

pub(crate) fn convert<R: BufRead>(
    mut reader: R,
    _options: &ConvertOptions,
    sink: &mut dyn PageConsumer,
) -> Result<Vec<String>> {
    let mut warnings = Vec::new();
    let mut buffer = String::new();
    reader.read_to_string(&mut buffer)?;

    let mut apertures: HashMap<u32, Aperture> = HashMap::new();
    let mut current_aperture: Option<u32> = None;
    let mut is_metric = true; // Default mm
    let mut coord_format = CoordinateFormat::default();
    let mut current_x = 0.0;
    let mut current_y = 0.0;
    let mut current_polarity = Polarity::Dark;
    let mut interpolation_mode = InterpolationMode::Linear;
    let mut in_polygon_mode = false;
    let mut polygon_points: Vec<(f64, f64)> = Vec::new();

    let mut raw_paths: Vec<GerberElement> = Vec::new();
    let mut bbox = BBox::new();

    let mut commands_count = 0;
    let mut stop_parsing = false;

    // Gerber RS-274X: percent signs '%' delimit parameter blocks (odd segments).
    // Even segments are standard drawing commands.
    for (seg_idx, segment) in buffer.split('%').enumerate() {
        if stop_parsing {
            break;
        }
        if seg_idx % 2 == 1 {
            // Parameter block
            for param_cmd in segment.split('*') {
                let param_body = param_cmd.trim();
                if param_body.starts_with("MOMM") {
                    is_metric = true;
                } else if param_body.starts_with("MOIN") {
                    is_metric = false;
                } else if param_body.starts_with("FSLA") || param_body.starts_with("FS") {
                    parse_format_spec(param_body, &mut coord_format);
                } else if param_body.starts_with("ADD") {
                    parse_aperture_definition(param_body, &mut apertures);
                } else if param_body.starts_with("LPD") {
                    current_polarity = Polarity::Dark;
                } else if param_body.starts_with("LPC") {
                    current_polarity = Polarity::Clear;
                }
            }
            continue;
        }

        // Standard commands block
        for token in segment.split('*') {
            let trimmed = token.trim();
            if trimmed.is_empty() {
                continue;
            }
            commands_count += 1;
            if commands_count > MAX_GERBER_COMMANDS {
                return Err(Error::LimitExceeded(format!(
                    "Gerber commands exceed safety limit of {MAX_GERBER_COMMANDS}"
                )));
            }

            // Ignore comments and handle program termination
            if trimmed.starts_with("G04") || trimmed.starts_with("G4") {
                continue;
            }
            if trimmed == "M02" || trimmed == "M00" || trimmed == "M2" || trimmed == "M0" {
                stop_parsing = true;
                break;
            }

            // Standard commands
            let mut chars = trimmed.chars().peekable();
            let mut target_x = current_x;
            let mut target_y = current_y;
            let mut target_i: f64 = 0.0;
            let mut target_j: f64 = 0.0;
            let mut has_coord = false;

            while let Some(c) = chars.next() {
                match c {
                    'G' => {
                        let mut num_str = String::new();
                        while let Some(&nc) = chars.peek() {
                            if nc.is_ascii_digit() {
                                num_str.push(nc);
                                chars.next();
                            } else {
                                break;
                            }
                        }
                        match num_str.as_str() {
                            "4" | "04" => break, // G04 comment terminates remainder of command
                            "1" | "01" => interpolation_mode = InterpolationMode::Linear,
                            "2" | "02" => interpolation_mode = InterpolationMode::Clockwise,
                            "3" | "03" => interpolation_mode = InterpolationMode::CounterClockwise,
                            "36" => {
                                in_polygon_mode = true;
                                polygon_points.clear();
                                polygon_points.push((current_x, current_y));
                            }
                            "37" => {
                                in_polygon_mode = false;
                                if polygon_points.len() >= 3 {
                                    for pt in &polygon_points {
                                        bbox.update(pt.0, pt.1);
                                    }
                                    raw_paths.push(GerberElement::Polygon {
                                        points: polygon_points.clone(),
                                        polarity: current_polarity,
                                    });
                                }
                                polygon_points.clear();
                            }
                            _ => {}
                        }
                    }
                    'D' => {
                        let mut num_str = String::new();
                        while let Some(&nc) = chars.peek() {
                            if nc.is_ascii_digit() {
                                num_str.push(nc);
                                chars.next();
                            } else {
                                break;
                            }
                        }
                        if let Ok(d_code) = num_str.parse::<u32>() {
                            match d_code {
                                1 => {
                                    // Draw operation (exposure on)
                                    if in_polygon_mode {
                                        current_x = target_x;
                                        current_y = target_y;
                                        polygon_points.push((current_x, current_y));
                                    } else {
                                        let ap = current_aperture
                                            .and_then(|id| apertures.get(&id))
                                            .cloned();
                                        let width = match &ap {
                                            Some(Aperture::Circle { diameter, .. }) => *diameter,
                                            Some(Aperture::Rectangle { width, height, .. }) => {
                                                width.min(*height)
                                            }
                                            _ => 0.2, // fallback mm
                                        };
                                        bbox.update(current_x - width, current_y - width);
                                        bbox.update(current_x + width, current_y + width);
                                        bbox.update(target_x - width, target_y - width);
                                        bbox.update(target_x + width, target_y + width);

                                        if interpolation_mode == InterpolationMode::Linear
                                            || (target_i.abs() < 1e-6 && target_j.abs() < 1e-6)
                                        {
                                            raw_paths.push(GerberElement::Line {
                                                start: (current_x, current_y),
                                                end: (target_x, target_y),
                                                width,
                                                polarity: current_polarity,
                                            });
                                        } else {
                                            let clockwise =
                                                interpolation_mode == InterpolationMode::Clockwise;
                                            raw_paths.push(GerberElement::Arc {
                                                start: (current_x, current_y),
                                                end: (target_x, target_y),
                                                center_offset: (target_i, target_j),
                                                clockwise,
                                                width,
                                                polarity: current_polarity,
                                            });
                                        }
                                        current_x = target_x;
                                        current_y = target_y;
                                    }
                                }
                                2 => {
                                    // Move operation (exposure off)
                                    current_x = target_x;
                                    current_y = target_y;
                                    if in_polygon_mode {
                                        polygon_points.push((current_x, current_y));
                                    }
                                }
                                3 => {
                                    // Flash operation
                                    current_x = target_x;
                                    current_y = target_y;
                                    if let Some(ap_id) = current_aperture {
                                        if let Some(ap) = apertures.get(&ap_id) {
                                            accumulate_aperture_bbox(
                                                current_x, current_y, ap, &mut bbox,
                                            );
                                            raw_paths.push(GerberElement::Flash {
                                                x: current_x,
                                                y: current_y,
                                                aperture: ap.clone(),
                                                polarity: current_polarity,
                                            });
                                        }
                                    }
                                }
                                id if id >= 10 => {
                                    current_aperture = Some(id);
                                }
                                _ => {}
                            }
                        }
                    }
                    'X' => {
                        let mut num_str = String::new();
                        while let Some(&nc) = chars.peek() {
                            if nc.is_ascii_digit() || nc == '-' || nc == '+' || nc == '.' {
                                num_str.push(nc);
                                chars.next();
                            } else {
                                break;
                            }
                        }
                        let mut val = coord_format.parse_coord(&num_str, false);
                        if !is_metric {
                            val *= 25.4; // inches to mm
                        }
                        target_x = val;
                        has_coord = true;
                    }
                    'Y' => {
                        let mut num_str = String::new();
                        while let Some(&nc) = chars.peek() {
                            if nc.is_ascii_digit() || nc == '-' || nc == '+' || nc == '.' {
                                num_str.push(nc);
                                chars.next();
                            } else {
                                break;
                            }
                        }
                        let mut val = coord_format.parse_coord(&num_str, true);
                        if !is_metric {
                            val *= 25.4; // inches to mm
                        }
                        target_y = val;
                        has_coord = true;
                    }
                    'I' => {
                        let mut num_str = String::new();
                        while let Some(&nc) = chars.peek() {
                            if nc.is_ascii_digit() || nc == '-' || nc == '+' || nc == '.' {
                                num_str.push(nc);
                                chars.next();
                            } else {
                                break;
                            }
                        }
                        let mut val = coord_format.parse_coord(&num_str, false);
                        if !is_metric {
                            val *= 25.4;
                        }
                        target_i = val;
                    }
                    'J' => {
                        let mut num_str = String::new();
                        while let Some(&nc) = chars.peek() {
                            if nc.is_ascii_digit() || nc == '-' || nc == '+' || nc == '.' {
                                num_str.push(nc);
                                chars.next();
                            } else {
                                break;
                            }
                        }
                        let mut val = coord_format.parse_coord(&num_str, true);
                        if !is_metric {
                            val *= 25.4;
                        }
                        target_j = val;
                    }
                    _ => {}
                }
            }

            if has_coord && in_polygon_mode {
                current_x = target_x;
                current_y = target_y;
                polygon_points.push((current_x, current_y));
            }
        }
    }

    if !bbox.is_valid() {
        bbox.min_x = 0.0;
        bbox.min_y = 0.0;
        bbox.max_x = 100.0;
        bbox.max_y = 100.0;
        warnings.push("Gerber contains no renderable elements; using default viewport".into());
    }

    let raw_w = bbox.width().max(1.0);
    let raw_h = bbox.height().max(1.0);
    let margin = (raw_w.max(raw_h) * 0.05).max(5.0);
    let content_w = raw_w + 2.0 * margin;
    let content_h = raw_h + 2.0 * margin;

    let scale = (TARGET_PAGE_LONG_EDGE / content_w.max(content_h)).clamp(0.01, 100.0);
    let page_w = (content_w * scale).max(MIN_PAGE_DIMENSION);
    let page_h = (content_h * scale).max(MIN_PAGE_DIMENSION);

    let mut page = Page::new(1, page_w, page_h, "gerber");
    page.title = "PCB Layer".into();

    let map_pt = |x: f64, y: f64| -> (f64, f64) {
        let sx = (x - bbox.min_x + margin) * scale;
        let sy = (bbox.max_y - y + margin) * scale; // Y inverted for SVG
        (sx, sy)
    };

    let bg_paint = Paint::solid("#143d22");
    let bg_rect = Node::Path {
        id: "pcb-substrate".into(),
        d: format!(
            "M 0 0 L {} 0 L {} {} L 0 {} Z",
            fmt_coord(page_w),
            fmt_coord(page_w),
            fmt_coord(page_h),
            fmt_coord(page_h)
        ),
        fill_rule: "nonzero".into(),
        fill: bg_paint.clone(),
        stroke: Stroke::default(),
        transform: IDENTITY,
        clip_id: None,
        meta: SourceMeta {
            semantic_role: "pcb:substrate".into(),
            ..Default::default()
        },
    };
    page.nodes.push(bg_rect);

    let mut copper_nodes = Vec::new();
    let copper_paint = Paint::solid("#e8be38");

    let get_paint = |polarity: Polarity| -> Paint {
        match polarity {
            Polarity::Dark => copper_paint.clone(),
            Polarity::Clear => bg_paint.clone(),
        }
    };

    for elem in raw_paths {
        match elem {
            GerberElement::Line {
                start,
                end,
                width,
                polarity,
            } => {
                let p1 = map_pt(start.0, start.1);
                let p2 = map_pt(end.0, end.1);
                let stroke_w = (width * scale).clamp(0.75, 200.0);
                let d = format!(
                    "M {} {} L {} {}",
                    fmt_coord(p1.0),
                    fmt_coord(p1.1),
                    fmt_coord(p2.0),
                    fmt_coord(p2.1)
                );
                copper_nodes.push(Node::Path {
                    id: String::new(),
                    d,
                    fill_rule: "nonzero".into(),
                    fill: Paint::None,
                    stroke: Stroke {
                        paint: get_paint(polarity),
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
            GerberElement::Arc {
                start,
                end,
                center_offset,
                clockwise,
                width,
                polarity,
            } => {
                let p1 = map_pt(start.0, start.1);
                let p2 = map_pt(end.0, end.1);
                let stroke_w = (width * scale).clamp(0.75, 200.0);
                let r_unscaled = (center_offset.0.powi(2) + center_offset.1.powi(2)).sqrt();
                let r = (r_unscaled * scale).max(0.1);
                // In inverted Y coordinate space, clockwise direction reverses sweep
                let sweep = if clockwise { 0 } else { 1 };
                let d = format!(
                    "M {} {} A {} {} 0 0 {} {} {}",
                    fmt_coord(p1.0),
                    fmt_coord(p1.1),
                    fmt_coord(r),
                    fmt_coord(r),
                    sweep,
                    fmt_coord(p2.0),
                    fmt_coord(p2.1)
                );
                copper_nodes.push(Node::Path {
                    id: String::new(),
                    d,
                    fill_rule: "nonzero".into(),
                    fill: Paint::None,
                    stroke: Stroke {
                        paint: get_paint(polarity),
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
            GerberElement::Polygon { points, polarity } => {
                if points.len() >= 3 {
                    let mut d = String::new();
                    for (i, pt) in points.iter().enumerate() {
                        let (x, y) = map_pt(pt.0, pt.1);
                        if i == 0 {
                            d.push_str(&format!("M {} {}", fmt_coord(x), fmt_coord(y)));
                        } else {
                            d.push_str(&format!(" L {} {}", fmt_coord(x), fmt_coord(y)));
                        }
                    }
                    d.push_str(" Z");
                    copper_nodes.push(Node::Path {
                        id: String::new(),
                        d,
                        fill_rule: "nonzero".into(),
                        fill: get_paint(polarity),
                        stroke: Stroke::default(),
                        transform: IDENTITY,
                        clip_id: None,
                        meta: SourceMeta::default(),
                    });
                }
            }
            GerberElement::Flash {
                x,
                y,
                aperture,
                polarity,
            } => {
                let (cx, cy) = map_pt(x, y);
                let fill_paint = get_paint(polarity);
                match aperture {
                    Aperture::Circle {
                        diameter,
                        hole_diameter,
                    } => {
                        let r = (diameter / 2.0) * scale;
                        let d = circle_path(cx, cy, r);
                        copper_nodes.push(Node::Path {
                            id: String::new(),
                            d,
                            fill_rule: "nonzero".into(),
                            fill: fill_paint,
                            stroke: Stroke::default(),
                            transform: IDENTITY,
                            clip_id: None,
                            meta: SourceMeta::default(),
                        });
                        if let Some(hd) = hole_diameter {
                            let hr = (hd / 2.0) * scale;
                            let hole_d = circle_path(cx, cy, hr);
                            copper_nodes.push(Node::Path {
                                id: String::new(),
                                d: hole_d,
                                fill_rule: "nonzero".into(),
                                fill: bg_paint.clone(),
                                stroke: Stroke::default(),
                                transform: IDENTITY,
                                clip_id: None,
                                meta: SourceMeta::default(),
                            });
                        }
                    }
                    Aperture::Rectangle {
                        width,
                        height,
                        hole_diameter,
                    } => {
                        let w = width * scale;
                        let h = height * scale;
                        let x0 = cx - w / 2.0;
                        let y0 = cy - h / 2.0;
                        let d = format!(
                            "M {} {} L {} {} L {} {} L {} {} Z",
                            fmt_coord(x0),
                            fmt_coord(y0),
                            fmt_coord(x0 + w),
                            fmt_coord(y0),
                            fmt_coord(x0 + w),
                            fmt_coord(y0 + h),
                            fmt_coord(x0),
                            fmt_coord(y0 + h)
                        );
                        copper_nodes.push(Node::Path {
                            id: String::new(),
                            d,
                            fill_rule: "nonzero".into(),
                            fill: fill_paint,
                            stroke: Stroke::default(),
                            transform: IDENTITY,
                            clip_id: None,
                            meta: SourceMeta::default(),
                        });
                        if let Some(hd) = hole_diameter {
                            let hr = (hd / 2.0) * scale;
                            let hole_d = circle_path(cx, cy, hr);
                            copper_nodes.push(Node::Path {
                                id: String::new(),
                                d: hole_d,
                                fill_rule: "nonzero".into(),
                                fill: bg_paint.clone(),
                                stroke: Stroke::default(),
                                transform: IDENTITY,
                                clip_id: None,
                                meta: SourceMeta::default(),
                            });
                        }
                    }
                    Aperture::Obround {
                        width,
                        height,
                        hole_diameter,
                    } => {
                        let w = width * scale;
                        let h = height * scale;
                        let d = if (w - h).abs() < 1e-4 {
                            circle_path(cx, cy, w / 2.0)
                        } else if w > h {
                            let r = h / 2.0;
                            let dx = (w - h) / 2.0;
                            format!(
                                "M {} {} L {} {} A {} {} 0 0 1 {} {} L {} {} A {} {} 0 0 1 {} {} Z",
                                fmt_coord(cx - dx),
                                fmt_coord(cy - r),
                                fmt_coord(cx + dx),
                                fmt_coord(cy - r),
                                fmt_coord(r),
                                fmt_coord(r),
                                fmt_coord(cx + dx),
                                fmt_coord(cy + r),
                                fmt_coord(cx - dx),
                                fmt_coord(cy + r),
                                fmt_coord(r),
                                fmt_coord(r),
                                fmt_coord(cx - dx),
                                fmt_coord(cy - r),
                            )
                        } else {
                            let r = w / 2.0;
                            let dy = (h - w) / 2.0;
                            format!(
                                "M {} {} L {} {} A {} {} 0 0 1 {} {} L {} {} A {} {} 0 0 1 {} {} Z",
                                fmt_coord(cx + r),
                                fmt_coord(cy - dy),
                                fmt_coord(cx + r),
                                fmt_coord(cy + dy),
                                fmt_coord(r),
                                fmt_coord(r),
                                fmt_coord(cx - r),
                                fmt_coord(cy + dy),
                                fmt_coord(cx - r),
                                fmt_coord(cy - dy),
                                fmt_coord(r),
                                fmt_coord(r),
                                fmt_coord(cx + r),
                                fmt_coord(cy - dy),
                            )
                        };
                        copper_nodes.push(Node::Path {
                            id: String::new(),
                            d,
                            fill_rule: "nonzero".into(),
                            fill: fill_paint,
                            stroke: Stroke::default(),
                            transform: IDENTITY,
                            clip_id: None,
                            meta: SourceMeta::default(),
                        });
                        if let Some(hd) = hole_diameter {
                            let hr = (hd / 2.0) * scale;
                            let hole_d = circle_path(cx, cy, hr);
                            copper_nodes.push(Node::Path {
                                id: String::new(),
                                d: hole_d,
                                fill_rule: "nonzero".into(),
                                fill: bg_paint.clone(),
                                stroke: Stroke::default(),
                                transform: IDENTITY,
                                clip_id: None,
                                meta: SourceMeta::default(),
                            });
                        }
                    }
                    Aperture::Polygon {
                        diameter,
                        vertices,
                        rotation,
                        hole_diameter,
                    } => {
                        let r = (diameter / 2.0) * scale;
                        let mut d = String::new();
                        let count = vertices.max(3);
                        let rot_rad = rotation.unwrap_or(0.0).to_radians();
                        for i in 0..count {
                            let angle = rot_rad + (i as f64) * 2.0 * PI / (count as f64);
                            let px = cx + r * angle.cos();
                            let py = cy + r * angle.sin();
                            if i == 0 {
                                d.push_str(&format!("M {} {}", fmt_coord(px), fmt_coord(py)));
                            } else {
                                d.push_str(&format!(" L {} {}", fmt_coord(px), fmt_coord(py)));
                            }
                        }
                        d.push_str(" Z");
                        copper_nodes.push(Node::Path {
                            id: String::new(),
                            d,
                            fill_rule: "nonzero".into(),
                            fill: fill_paint,
                            stroke: Stroke::default(),
                            transform: IDENTITY,
                            clip_id: None,
                            meta: SourceMeta::default(),
                        });
                        if let Some(hd) = hole_diameter {
                            let hr = (hd / 2.0) * scale;
                            let hole_d = circle_path(cx, cy, hr);
                            copper_nodes.push(Node::Path {
                                id: String::new(),
                                d: hole_d,
                                fill_rule: "nonzero".into(),
                                fill: bg_paint.clone(),
                                stroke: Stroke::default(),
                                transform: IDENTITY,
                                clip_id: None,
                                meta: SourceMeta::default(),
                            });
                        }
                    }
                }
            }
        }
    }

    let copper_group = Node::Group {
        id: "pcb-copper-layer".into(),
        nodes: copper_nodes,
        transform: IDENTITY,
        opacity: 1.0,
        clip_id: None,
        meta: SourceMeta {
            semantic_role: "pcb:copper".into(),
            ..Default::default()
        },
    };
    page.nodes.push(copper_group);

    sink.consume(page)?;
    Ok(warnings)
}

#[derive(Clone, Debug)]
enum GerberElement {
    Line {
        start: (f64, f64),
        end: (f64, f64),
        width: f64,
        polarity: Polarity,
    },
    Arc {
        start: (f64, f64),
        end: (f64, f64),
        center_offset: (f64, f64),
        clockwise: bool,
        width: f64,
        polarity: Polarity,
    },
    Polygon {
        points: Vec<(f64, f64)>,
        polarity: Polarity,
    },
    Flash {
        x: f64,
        y: f64,
        aperture: Aperture,
        polarity: Polarity,
    },
}

fn accumulate_aperture_bbox(x: f64, y: f64, ap: &Aperture, bbox: &mut BBox) {
    match ap {
        Aperture::Circle { diameter, .. } => {
            let r = diameter / 2.0;
            bbox.update(x - r, y - r);
            bbox.update(x + r, y + r);
        }
        Aperture::Rectangle { width, height, .. } | Aperture::Obround { width, height, .. } => {
            let hw = width / 2.0;
            let hh = height / 2.0;
            bbox.update(x - hw, y - hh);
            bbox.update(x + hw, y + hh);
        }
        Aperture::Polygon { diameter, .. } => {
            let r = diameter / 2.0;
            bbox.update(x - r, y - r);
            bbox.update(x + r, y + r);
        }
    }
}

fn parse_format_spec(s: &str, fmt: &mut CoordinateFormat) {
    if s.contains('L') {
        fmt.suppress_leading_zeros = true;
    } else if s.contains('T') {
        fmt.suppress_leading_zeros = false;
    }
    if let Some(x_pos) = s.find('X') {
        let after = &s[x_pos + 1..];
        let digits: String = after.chars().take_while(|c| c.is_ascii_digit()).collect();
        if digits.len() == 2 {
            let b = digits.as_bytes();
            fmt.x_int = (b[0] - b'0') as usize;
            fmt.x_dec = (b[1] - b'0') as usize;
        }
    }
    if let Some(y_pos) = s.find('Y') {
        let after = &s[y_pos + 1..];
        let digits: String = after.chars().take_while(|c| c.is_ascii_digit()).collect();
        if digits.len() == 2 {
            let b = digits.as_bytes();
            fmt.y_int = (b[0] - b'0') as usize;
            fmt.y_dec = (b[1] - b'0') as usize;
        }
    }
}

fn parse_aperture_definition(s: &str, apertures: &mut HashMap<u32, Aperture>) {
    // Format: ADD<id><type>,<mods>
    let rest = s.trim_start_matches("ADD");
    let mut id_str = String::new();
    let mut chars = rest.chars().peekable();
    while let Some(&c) = chars.peek() {
        if c.is_ascii_digit() {
            id_str.push(c);
            chars.next();
        } else {
            break;
        }
    }
    let id: u32 = match id_str.parse() {
        Ok(v) => v,
        Err(_) => return,
    };

    let shape_type = match chars.next() {
        Some(c) => c,
        None => return,
    };
    if chars.next() != Some(',') {
        return;
    }

    let mods_str: String = chars.collect();
    let params: Vec<f64> = mods_str
        .split(['X', 'x'])
        .filter_map(parse_cad_float)
        .collect();

    let ap = match shape_type {
        'C' => {
            let diameter = params.first().copied().unwrap_or(0.1);
            let hole_diameter = params.get(1).copied();
            Aperture::Circle {
                diameter,
                hole_diameter,
            }
        }
        'R' => {
            let width = params.first().copied().unwrap_or(0.1);
            let height = params.get(1).copied().unwrap_or(width);
            let hole_diameter = params.get(2).copied();
            Aperture::Rectangle {
                width,
                height,
                hole_diameter,
            }
        }
        'O' => {
            let width = params.first().copied().unwrap_or(0.1);
            let height = params.get(1).copied().unwrap_or(width);
            let hole_diameter = params.get(2).copied();
            Aperture::Obround {
                width,
                height,
                hole_diameter,
            }
        }
        'P' => {
            let diameter = params.first().copied().unwrap_or(0.1);
            let vertices = params.get(1).copied().unwrap_or(3.0) as usize;
            let rotation = params.get(2).copied();
            let hole_diameter = params.get(3).copied();
            Aperture::Polygon {
                diameter,
                vertices,
                rotation,
                hole_diameter,
            }
        }
        _ => return,
    };

    apertures.insert(id, ap);
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
    fn parses_simple_gerber() {
        let gerber_text = r#"%FSLAX24Y24*%
%MOMM*%
%ADD10C,0.5000*%
%ADD11R,1.0000X2.0000*%
D10*
X000000Y000000D02*
X010000Y010000D01*
D11*
X020000Y020000D03*
M02*
"#;
        let mut sink = DummySink(Vec::new());
        let options = ConvertOptions::default();
        let warnings =
            convert(Cursor::new(gerber_text), &options, &mut sink).expect("convert gerber");
        assert!(warnings.is_empty());
        assert_eq!(sink.0.len(), 1);
        let page = &sink.0[0];
        assert_eq!(page.nodes.len(), 2); // substrate + copper group
    }

    #[test]
    fn parses_gerber_arc_and_polarity_and_aperture_holes() {
        let gerber_text = r#"%FSLAX24Y24*%
%MOMM*%
%ADD10C,0.5000*%
%ADD11C,2.0000X0.8000*%
D10*
X000000Y000000D02*
G02*
X010000Y010000I010000J000000D01*
G01*
D11*
X020000Y020000D03*
%LPC*%
X015000Y015000D03*
%LPD*%
M02*
"#;
        let mut sink = DummySink(Vec::new());
        let options = ConvertOptions::default();
        let warnings =
            convert(Cursor::new(gerber_text), &options, &mut sink).expect("convert gerber");
        assert!(warnings.is_empty());
        assert_eq!(sink.0.len(), 1);
        let page = &sink.0[0];
        assert_eq!(page.nodes.len(), 2);
        if let Node::Group { nodes, .. } = &page.nodes[1] {
            // Should contain arc path, donut pad (outer + hole), and clear pad
            assert!(!nodes.is_empty());
        } else {
            panic!("expected copper group");
        }
    }

    #[test]
    fn parses_gerber_obround_polygon_and_comments() {
        let gerber_text = r#"%FSLAX24Y24*%
%MOMM*%
G04 Created by CAD tool on 2024-01-01 with D01 and X100Y100*
%AMMACRO*
1,1,$1,$2,0,0,0*
%
%ADD12O,2.0000X1.0000X0.5000*%
%ADD13P,2.0000X8X22.5000X0.8000*%
D12*
X010000Y010000D03*
D13*
X020000Y020000D03*
G04 Trailing comment*
M02*
"#;
        let mut sink = DummySink(Vec::new());
        let options = ConvertOptions::default();
        let warnings =
            convert(Cursor::new(gerber_text), &options, &mut sink).expect("convert gerber");
        assert!(warnings.is_empty());
        assert_eq!(sink.0.len(), 1);
        let page = &sink.0[0];
        if let Node::Group { nodes, .. } = &page.nodes[1] {
            // Should contain obround outer + hole, and octagonal polygon outer + hole
            assert_eq!(nodes.len(), 4);
            if let Node::Path { d, .. } = &nodes[0] {
                // Obround path contains 'A' arc commands
                assert!(d.contains('A'));
            } else {
                panic!("expected path node for obround");
            }
        } else {
            panic!("expected copper group");
        }
    }
}
