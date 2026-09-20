//! Zero-allocation SVG path tokenizer and vector geometry extractor.
//!
//! Provides a streaming interface for extracting 2D vector primitives
//! (lines, rectangles, circles, polylines, paths, text) from SVG markup
//! with minimal heap allocation.

use quick_xml::Reader;
use quick_xml::events::Event;

pub use super::color::{ParsedStyle, parse_color, parse_color_hex, parse_style};
pub use super::geometry::{
    Point2D, Transform2D, parse_transform, sample_cubic_bezier, sample_elliptical_arc,
    sample_quad_bezier,
};
use crate::error::Result;

/// Extracted SVG 2D vector element.
#[derive(Clone, Debug, PartialEq)]
pub enum SvgElement {
    Line {
        p1: Point2D,
        p2: Point2D,
        stroke_color: Option<u32>,
        stroke_width: f64,
        layer: String,
    },
    Rect {
        x: f64,
        y: f64,
        width: f64,
        height: f64,
        stroke_color: Option<u32>,
        fill_color: Option<u32>,
        layer: String,
    },
    Circle {
        center: Point2D,
        radius: f64,
        stroke_color: Option<u32>,
        fill_color: Option<u32>,
        layer: String,
    },
    Polyline {
        points: Vec<Point2D>,
        is_closed: bool,
        stroke_color: Option<u32>,
        fill_color: Option<u32>,
        layer: String,
    },
    Text {
        pos: Point2D,
        font_size: f64,
        color: Option<u32>,
        layer: String,
        content: String,
    },
}

/// Zero-allocation tokenizer for SVG path `d` attribute data.
pub struct SvgPathTokenizer<'a> {
    remaining: &'a str,
}

impl<'a> SvgPathTokenizer<'a> {
    pub fn new(d: &'a str) -> Self {
        Self { remaining: d }
    }
}

/// Token parsed from an SVG path data string.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum PathToken<'a> {
    Command(char),
    Number(f64),
    Raw(&'a str),
}

impl<'a> Iterator for SvgPathTokenizer<'a> {
    type Item = PathToken<'a>;

    fn next(&mut self) -> Option<Self::Item> {
        let bytes = self.remaining.as_bytes();
        let mut i = 0;

        // Skip whitespace and commas
        while i < bytes.len() && (bytes[i].is_ascii_whitespace() || bytes[i] == b',') {
            i += 1;
        }
        if i >= bytes.len() {
            self.remaining = "";
            return None;
        }

        let b = bytes[i];
        // If it's an ASCII alphabetic command character
        if b.is_ascii_alphabetic() {
            let cmd = b as char;
            self.remaining = &self.remaining[i + 1..];
            return Some(PathToken::Command(cmd));
        }

        // Parse number (may start with '+', '-', or '.')
        let start = i;
        if b == b'+' || b == b'-' {
            i += 1;
        }
        let mut has_digits = false;
        while i < bytes.len() && bytes[i].is_ascii_digit() {
            has_digits = true;
            i += 1;
        }
        if i < bytes.len() && bytes[i] == b'.' {
            i += 1;
            while i < bytes.len() && bytes[i].is_ascii_digit() {
                has_digits = true;
                i += 1;
            }
        }
        // Exponent part (e.g. 1e-4, 2.5E+2)
        if has_digits && i < bytes.len() && (bytes[i] == b'e' || bytes[i] == b'E') {
            let exp_start = i;
            i += 1;
            if i < bytes.len() && (bytes[i] == b'+' || bytes[i] == b'-') {
                i += 1;
            }
            let exp_digits_start = i;
            while i < bytes.len() && bytes[i].is_ascii_digit() {
                i += 1;
            }
            if i == exp_digits_start {
                // Invalid exponent, roll back
                i = exp_start;
            }
        }

        if has_digits {
            let slice = &self.remaining[start..i];
            self.remaining = &self.remaining[i..];
            if let Ok(num) = slice.parse::<f64>() {
                return Some(PathToken::Number(num));
            }
            return Some(PathToken::Raw(slice));
        }

        // Fallback: advance 1 byte
        let slice = &self.remaining[start..start + 1];
        self.remaining = &self.remaining[start + 1..];
        Some(PathToken::Raw(slice))
    }
}

/// Decomposes an SVG path `d` attribute into a list of 2D polylines (points and closed flag).
///
/// Supports all W3C SVG 1.1 path commands: M/m, L/l, H/h, V/v, C/c, S/s, Q/q, T/t, A/a, Z/z.
pub fn decompose_svg_path(d: &str) -> Vec<(Vec<Point2D>, bool)> {
    let mut polylines = Vec::new();
    let mut current_verts = Vec::new();
    let mut is_closed = false;

    let mut cur = Point2D::new(0.0, 0.0);
    let mut start = Point2D::new(0.0, 0.0);
    let mut last_cubic_ctrl: Option<Point2D> = None;
    let mut last_quad_ctrl: Option<Point2D> = None;

    let mut tokenizer = SvgPathTokenizer::new(d);
    let mut cur_cmd = 'M';

    while let Some(tok) = tokenizer.next() {
        match tok {
            PathToken::Command(c) => {
                cur_cmd = c;
            }
            PathToken::Number(first_num) => {
                // We encountered a number without an explicit command token; repeat previous command
                match cur_cmd {
                    'M' | 'm' => {
                        last_cubic_ctrl = None;
                        last_quad_ctrl = None;
                        if !current_verts.is_empty() {
                            polylines.push((std::mem::take(&mut current_verts), is_closed));
                            is_closed = false;
                        }
                        let py = if let Some(PathToken::Number(y)) = tokenizer.next() {
                            y
                        } else {
                            0.0
                        };
                        if cur_cmd == 'm' {
                            cur.x += first_num;
                            cur.y += py;
                        } else {
                            cur.x = first_num;
                            cur.y = py;
                        }
                        start = cur;
                        current_verts.push(cur);
                        cur_cmd = if cur_cmd == 'm' { 'l' } else { 'L' };
                    }
                    'L' | 'l' => {
                        last_cubic_ctrl = None;
                        last_quad_ctrl = None;
                        let py = if let Some(PathToken::Number(y)) = tokenizer.next() {
                            y
                        } else {
                            0.0
                        };
                        if cur_cmd == 'l' {
                            cur.x += first_num;
                            cur.y += py;
                        } else {
                            cur.x = first_num;
                            cur.y = py;
                        }
                        current_verts.push(cur);
                    }
                    'H' | 'h' => {
                        last_cubic_ctrl = None;
                        last_quad_ctrl = None;
                        if cur_cmd == 'h' {
                            cur.x += first_num;
                        } else {
                            cur.x = first_num;
                        }
                        current_verts.push(cur);
                    }
                    'V' | 'v' => {
                        last_cubic_ctrl = None;
                        last_quad_ctrl = None;
                        if cur_cmd == 'v' {
                            cur.y += first_num;
                        } else {
                            cur.y = first_num;
                        }
                        current_verts.push(cur);
                    }
                    'C' | 'c' => {
                        let y1 = if let Some(PathToken::Number(v)) = tokenizer.next() {
                            v
                        } else {
                            0.0
                        };
                        let x2 = if let Some(PathToken::Number(v)) = tokenizer.next() {
                            v
                        } else {
                            0.0
                        };
                        let y2 = if let Some(PathToken::Number(v)) = tokenizer.next() {
                            v
                        } else {
                            0.0
                        };
                        let x = if let Some(PathToken::Number(v)) = tokenizer.next() {
                            v
                        } else {
                            0.0
                        };
                        let y = if let Some(PathToken::Number(v)) = tokenizer.next() {
                            v
                        } else {
                            0.0
                        };

                        let (p1, p2, p3) = if cur_cmd == 'c' {
                            (
                                Point2D::new(cur.x + first_num, cur.y + y1),
                                Point2D::new(cur.x + x2, cur.y + y2),
                                Point2D::new(cur.x + x, cur.y + y),
                            )
                        } else {
                            (
                                Point2D::new(first_num, y1),
                                Point2D::new(x2, y2),
                                Point2D::new(x, y),
                            )
                        };

                        sample_cubic_bezier(cur, p1, p2, p3, 8, |pt| {
                            current_verts.push(pt);
                        });
                        cur = p3;
                        last_cubic_ctrl = Some(p2);
                        last_quad_ctrl = None;
                    }
                    'S' | 's' => {
                        let y2 = if let Some(PathToken::Number(v)) = tokenizer.next() {
                            v
                        } else {
                            0.0
                        };
                        let x = if let Some(PathToken::Number(v)) = tokenizer.next() {
                            v
                        } else {
                            0.0
                        };
                        let y = if let Some(PathToken::Number(v)) = tokenizer.next() {
                            v
                        } else {
                            0.0
                        };

                        let (p2, p3) = if cur_cmd == 's' {
                            (
                                Point2D::new(cur.x + first_num, cur.y + y2),
                                Point2D::new(cur.x + x, cur.y + y),
                            )
                        } else {
                            (Point2D::new(first_num, y2), Point2D::new(x, y))
                        };

                        let p1 = if let Some(last_ctrl) = last_cubic_ctrl {
                            Point2D::new(2.0 * cur.x - last_ctrl.x, 2.0 * cur.y - last_ctrl.y)
                        } else {
                            cur
                        };

                        sample_cubic_bezier(cur, p1, p2, p3, 8, |pt| {
                            current_verts.push(pt);
                        });
                        cur = p3;
                        last_cubic_ctrl = Some(p2);
                        last_quad_ctrl = None;
                    }
                    'Q' | 'q' => {
                        let y1 = if let Some(PathToken::Number(v)) = tokenizer.next() {
                            v
                        } else {
                            0.0
                        };
                        let x = if let Some(PathToken::Number(v)) = tokenizer.next() {
                            v
                        } else {
                            0.0
                        };
                        let y = if let Some(PathToken::Number(v)) = tokenizer.next() {
                            v
                        } else {
                            0.0
                        };

                        let (p1, p2) = if cur_cmd == 'q' {
                            (
                                Point2D::new(cur.x + first_num, cur.y + y1),
                                Point2D::new(cur.x + x, cur.y + y),
                            )
                        } else {
                            (Point2D::new(first_num, y1), Point2D::new(x, y))
                        };

                        sample_quad_bezier(cur, p1, p2, 8, |pt| {
                            current_verts.push(pt);
                        });
                        cur = p2;
                        last_quad_ctrl = Some(p1);
                        last_cubic_ctrl = None;
                    }
                    'T' | 't' => {
                        let y = if let Some(PathToken::Number(v)) = tokenizer.next() {
                            v
                        } else {
                            0.0
                        };
                        let p2 = if cur_cmd == 't' {
                            Point2D::new(cur.x + first_num, cur.y + y)
                        } else {
                            Point2D::new(first_num, y)
                        };

                        let p1 = if let Some(last_ctrl) = last_quad_ctrl {
                            Point2D::new(2.0 * cur.x - last_ctrl.x, 2.0 * cur.y - last_ctrl.y)
                        } else {
                            cur
                        };

                        sample_quad_bezier(cur, p1, p2, 8, |pt| {
                            current_verts.push(pt);
                        });
                        cur = p2;
                        last_quad_ctrl = Some(p1);
                        last_cubic_ctrl = None;
                    }
                    'A' | 'a' => {
                        let ry = if let Some(PathToken::Number(v)) = tokenizer.next() {
                            v
                        } else {
                            0.0
                        };
                        let x_rot = if let Some(PathToken::Number(v)) = tokenizer.next() {
                            v
                        } else {
                            0.0
                        };
                        let large_arc = if let Some(PathToken::Number(v)) = tokenizer.next() {
                            v
                        } else {
                            0.0
                        };
                        let sweep = if let Some(PathToken::Number(v)) = tokenizer.next() {
                            v
                        } else {
                            0.0
                        };
                        let x = if let Some(PathToken::Number(v)) = tokenizer.next() {
                            v
                        } else {
                            0.0
                        };
                        let y = if let Some(PathToken::Number(v)) = tokenizer.next() {
                            v
                        } else {
                            0.0
                        };

                        let dest = if cur_cmd == 'a' {
                            Point2D::new(cur.x + x, cur.y + y)
                        } else {
                            Point2D::new(x, y)
                        };

                        sample_elliptical_arc(
                            cur,
                            dest,
                            first_num,
                            ry,
                            x_rot,
                            large_arc != 0.0,
                            sweep != 0.0,
                            16,
                            |pt| current_verts.push(pt),
                        );
                        cur = dest;
                        last_cubic_ctrl = None;
                        last_quad_ctrl = None;
                    }
                    _ => {}
                }
            }
            PathToken::Raw(_) => {}
        }

        if cur_cmd == 'Z' || cur_cmd == 'z' {
            if !current_verts.is_empty() {
                current_verts.push(start);
                is_closed = true;
                polylines.push((std::mem::take(&mut current_verts), is_closed));
                is_closed = false;
            }
            cur = start;
            last_cubic_ctrl = None;
            last_quad_ctrl = None;
        }
    }

    if !current_verts.is_empty() {
        polylines.push((current_verts, is_closed));
    }

    polylines
}

/// High-level parsed SVG vector document data.
#[derive(Clone, Debug, Default)]
pub struct SvgVectorDocument {
    pub width: f64,
    pub height: f64,
    pub elements: Vec<SvgElement>,
}

/// Type alias for [`SvgVectorDocument`].
pub type SvgDocument = SvgVectorDocument;

/// Parses standard SVG XML content into a structured collection of 2D vector elements.
///
/// Applies `<g>` layer nesting and active 2D affine transformations (`transform="translate(...) rotate(...) matrix(...)"`).
pub fn parse_svg_elements(svg_content: &str) -> Result<SvgVectorDocument> {
    let mut reader = Reader::from_str(svg_content);
    reader.config_mut().trim_text(true);

    let mut doc = SvgVectorDocument {
        width: 800.0,
        height: 600.0,
        elements: Vec::new(),
    };

    let mut current_layer = "0".to_string();
    let mut layer_stack = Vec::new();
    let mut current_transform = Transform2D::identity();
    let mut transform_stack = Vec::new();
    let mut buf = Vec::new();

    let mut current_text_state: Option<(Point2D, f64, Option<u32>, String, String, Transform2D)> =
        None;

    while let Ok(event) = reader.read_event_into(&mut buf) {
        match event {
            Event::Start(e) | Event::Empty(e) => {
                let name = e.name().as_ref().to_vec();
                let tag = String::from_utf8_lossy(&name).to_ascii_lowercase();

                if tag == "svg" {
                    for attr in e.attributes().flatten() {
                        let k = String::from_utf8_lossy(attr.key.as_ref()).to_ascii_lowercase();
                        let v = String::from_utf8_lossy(&attr.value);
                        if k == "viewbox" {
                            let parts: Vec<f64> =
                                v.split([' ', ',']).filter_map(|s| s.parse().ok()).collect();
                            if parts.len() >= 4 && parts[2] > 0.0 && parts[3] > 0.0 {
                                doc.width = parts[2];
                                doc.height = parts[3];
                            }
                        } else if k == "width"
                            && let Ok(w) = v
                                .trim_end_matches("pt")
                                .trim_end_matches("px")
                                .parse::<f64>()
                            && w > 0.0
                        {
                            doc.width = w;
                        } else if k == "height"
                            && let Ok(h) = v
                                .trim_end_matches("pt")
                                .trim_end_matches("px")
                                .parse::<f64>()
                            && h > 0.0
                        {
                            doc.height = h;
                        }
                    }
                } else if tag == "g" {
                    let mut layer_name = current_layer.clone();
                    let mut g_transform = current_transform;
                    for attr in e.attributes().flatten() {
                        let k = String::from_utf8_lossy(attr.key.as_ref()).to_ascii_lowercase();
                        let v = String::from_utf8_lossy(&attr.value);
                        if k == "id" {
                            let sanitized = v.trim_start_matches("layer-");
                            if !sanitized.is_empty() {
                                layer_name = sanitized.to_string();
                            }
                        } else if k == "transform" {
                            let tf = parse_transform(&v);
                            g_transform = current_transform.multiply(&tf);
                        }
                    }
                    layer_stack.push(current_layer.clone());
                    current_layer = layer_name;
                    transform_stack.push(current_transform);
                    current_transform = g_transform;
                } else if tag == "line" {
                    let mut p1 = Point2D::default();
                    let mut p2 = Point2D::default();
                    let mut stroke = None;
                    let mut width = 1.0;
                    let mut elem_tf = Transform2D::identity();
                    for attr in e.attributes().flatten() {
                        let k = String::from_utf8_lossy(attr.key.as_ref()).to_ascii_lowercase();
                        let v = String::from_utf8_lossy(&attr.value);
                        match k.as_str() {
                            "x1" => p1.x = v.parse().unwrap_or(0.0),
                            "y1" => p1.y = v.parse().unwrap_or(0.0),
                            "x2" => p2.x = v.parse().unwrap_or(0.0),
                            "y2" => p2.y = v.parse().unwrap_or(0.0),
                            "stroke" => stroke = parse_color(&v),
                            "stroke-width" => {
                                width = v
                                    .trim_end_matches("px")
                                    .trim_end_matches("pt")
                                    .parse()
                                    .unwrap_or(1.0)
                            }
                            "style" => {
                                let st = parse_style(&v);
                                if let Some(s) = st.stroke {
                                    stroke = s;
                                }
                                if let Some(w) = st.stroke_width {
                                    width = w;
                                }
                            }
                            "transform" => elem_tf = parse_transform(&v),
                            _ => {}
                        }
                    }
                    let tf = current_transform.multiply(&elem_tf);
                    doc.elements.push(SvgElement::Line {
                        p1: tf.apply(p1),
                        p2: tf.apply(p2),
                        stroke_color: stroke,
                        stroke_width: width,
                        layer: current_layer.clone(),
                    });
                } else if tag == "rect" {
                    let mut x = 0.0;
                    let mut y = 0.0;
                    let mut width = 0.0;
                    let mut height = 0.0;
                    let mut stroke = None;
                    let mut fill = None;
                    let mut elem_tf = Transform2D::identity();
                    for attr in e.attributes().flatten() {
                        let k = String::from_utf8_lossy(attr.key.as_ref()).to_ascii_lowercase();
                        let v = String::from_utf8_lossy(&attr.value);
                        match k.as_str() {
                            "x" => x = v.parse().unwrap_or(0.0),
                            "y" => y = v.parse().unwrap_or(0.0),
                            "width" => width = v.parse().unwrap_or(0.0),
                            "height" => height = v.parse().unwrap_or(0.0),
                            "stroke" => stroke = parse_color(&v),
                            "fill" => fill = parse_color(&v),
                            "style" => {
                                let st = parse_style(&v);
                                if let Some(s) = st.stroke {
                                    stroke = s;
                                }
                                if let Some(f) = st.fill {
                                    fill = f;
                                }
                            }
                            "transform" => elem_tf = parse_transform(&v),
                            _ => {}
                        }
                    }
                    if width > 0.0 && height > 0.0 {
                        let tf = current_transform.multiply(&elem_tf);
                        if tf.b.abs() < 1e-9 && tf.c.abs() < 1e-9 && tf.a > 0.0 && tf.d > 0.0 {
                            let origin = tf.apply(Point2D::new(x, y));
                            doc.elements.push(SvgElement::Rect {
                                x: origin.x,
                                y: origin.y,
                                width: width * tf.a,
                                height: height * tf.d,
                                stroke_color: stroke,
                                fill_color: fill,
                                layer: current_layer.clone(),
                            });
                        } else {
                            // Rotated or skewed rectangle: convert to polyline
                            let p0 = tf.apply(Point2D::new(x, y));
                            let p1 = tf.apply(Point2D::new(x + width, y));
                            let p2 = tf.apply(Point2D::new(x + width, y + height));
                            let p3 = tf.apply(Point2D::new(x, y + height));
                            doc.elements.push(SvgElement::Polyline {
                                points: vec![p0, p1, p2, p3, p0],
                                is_closed: true,
                                stroke_color: stroke,
                                fill_color: fill,
                                layer: current_layer.clone(),
                            });
                        }
                    }
                } else if tag == "circle" {
                    let mut center = Point2D::default();
                    let mut radius = 0.0;
                    let mut stroke = None;
                    let mut fill = None;
                    let mut elem_tf = Transform2D::identity();
                    for attr in e.attributes().flatten() {
                        let k = String::from_utf8_lossy(attr.key.as_ref()).to_ascii_lowercase();
                        let v = String::from_utf8_lossy(&attr.value);
                        match k.as_str() {
                            "cx" => center.x = v.parse().unwrap_or(0.0),
                            "cy" => center.y = v.parse().unwrap_or(0.0),
                            "r" => radius = v.parse().unwrap_or(0.0),
                            "stroke" => stroke = parse_color(&v),
                            "fill" => fill = parse_color(&v),
                            "style" => {
                                let st = parse_style(&v);
                                if let Some(s) = st.stroke {
                                    stroke = s;
                                }
                                if let Some(f) = st.fill {
                                    fill = f;
                                }
                            }
                            "transform" => elem_tf = parse_transform(&v),
                            _ => {}
                        }
                    }
                    if radius > 0.0 {
                        let tf = current_transform.multiply(&elem_tf);
                        let center_trans = tf.apply(center);
                        if tf.b.abs() < 1e-9
                            && tf.c.abs() < 1e-9
                            && (tf.a.abs() - tf.d.abs()).abs() < 1e-6
                        {
                            doc.elements.push(SvgElement::Circle {
                                center: center_trans,
                                radius: radius * tf.a.abs(),
                                stroke_color: stroke,
                                fill_color: fill,
                                layer: current_layer.clone(),
                            });
                        } else {
                            // Non-uniformly scaled or sheared circle: sample to polyline
                            let mut pts = Vec::with_capacity(33);
                            for i in 0..=32 {
                                let angle = (i as f64 / 32.0) * std::f64::consts::PI * 2.0;
                                let px = center.x + radius * angle.cos();
                                let py = center.y + radius * angle.sin();
                                pts.push(tf.apply(Point2D::new(px, py)));
                            }
                            doc.elements.push(SvgElement::Polyline {
                                points: pts,
                                is_closed: true,
                                stroke_color: stroke,
                                fill_color: fill,
                                layer: current_layer.clone(),
                            });
                        }
                    }
                } else if tag == "polyline" || tag == "polygon" {
                    let is_poly = tag == "polygon";
                    let mut points = Vec::new();
                    let mut stroke = None;
                    let mut fill = None;
                    let mut elem_tf = Transform2D::identity();
                    for attr in e.attributes().flatten() {
                        let k = String::from_utf8_lossy(attr.key.as_ref()).to_ascii_lowercase();
                        let v = String::from_utf8_lossy(&attr.value);
                        if k == "points" {
                            let coords: Vec<f64> =
                                v.split([' ', ',']).filter_map(|s| s.parse().ok()).collect();
                            points = coords
                                .chunks_exact(2)
                                .map(|c| Point2D::new(c[0], c[1]))
                                .collect();
                        } else if k == "stroke" {
                            stroke = parse_color(&v);
                        } else if k == "fill" {
                            fill = parse_color(&v);
                        } else if k == "style" {
                            let st = parse_style(&v);
                            if let Some(s) = st.stroke {
                                stroke = s;
                            }
                            if let Some(f) = st.fill {
                                fill = f;
                            }
                        } else if k == "transform" {
                            elem_tf = parse_transform(&v);
                        }
                    }
                    if !points.is_empty() {
                        let tf = current_transform.multiply(&elem_tf);
                        let transformed_pts: Vec<Point2D> =
                            points.into_iter().map(|p| tf.apply(p)).collect();
                        doc.elements.push(SvgElement::Polyline {
                            points: transformed_pts,
                            is_closed: is_poly,
                            stroke_color: stroke,
                            fill_color: fill,
                            layer: current_layer.clone(),
                        });
                    }
                } else if tag == "path" {
                    let mut d_str = String::new();
                    let mut stroke = None;
                    let mut fill = None;
                    let mut elem_tf = Transform2D::identity();
                    for attr in e.attributes().flatten() {
                        let k = String::from_utf8_lossy(attr.key.as_ref()).to_ascii_lowercase();
                        let v = String::from_utf8_lossy(&attr.value);
                        if k == "d" {
                            d_str = v.to_string();
                        } else if k == "stroke" {
                            stroke = parse_color(&v);
                        } else if k == "fill" {
                            fill = parse_color(&v);
                        } else if k == "style" {
                            let st = parse_style(&v);
                            if let Some(s) = st.stroke {
                                stroke = s;
                            }
                            if let Some(f) = st.fill {
                                fill = f;
                            }
                        } else if k == "transform" {
                            elem_tf = parse_transform(&v);
                        }
                    }
                    if !d_str.is_empty() {
                        let tf = current_transform.multiply(&elem_tf);
                        let polylines = decompose_svg_path(&d_str);
                        for (pts, is_closed) in polylines {
                            if !pts.is_empty() {
                                let transformed_pts =
                                    pts.into_iter().map(|p| tf.apply(p)).collect();
                                doc.elements.push(SvgElement::Polyline {
                                    points: transformed_pts,
                                    is_closed,
                                    stroke_color: stroke,
                                    fill_color: fill,
                                    layer: current_layer.clone(),
                                });
                            }
                        }
                    }
                } else if tag == "text" {
                    let mut x = 0.0;
                    let mut y = 0.0;
                    let mut size = 12.0;
                    let mut fill = None;
                    let mut elem_tf = Transform2D::identity();
                    for attr in e.attributes().flatten() {
                        let k = String::from_utf8_lossy(attr.key.as_ref()).to_ascii_lowercase();
                        let v = String::from_utf8_lossy(&attr.value);
                        if k == "x" {
                            x = v.parse().unwrap_or(0.0);
                        } else if k == "y" {
                            y = v.parse().unwrap_or(0.0);
                        } else if k == "font-size" {
                            size = v
                                .trim_end_matches("px")
                                .trim_end_matches("pt")
                                .parse()
                                .unwrap_or(12.0);
                        } else if k == "fill" {
                            fill = parse_color(&v);
                        } else if k == "style" {
                            let st = parse_style(&v);
                            if let Some(f) = st.fill {
                                fill = f;
                            }
                            if let Some(sz) = st.font_size {
                                size = sz;
                            }
                        } else if k == "transform" {
                            elem_tf = parse_transform(&v);
                        }
                    }
                    let tf = current_transform.multiply(&elem_tf);
                    current_text_state = Some((
                        Point2D::new(x, y),
                        size,
                        fill,
                        current_layer.clone(),
                        String::new(),
                        tf,
                    ));
                }
            }
            Event::Text(e) => {
                if let Some((_, _, _, _, ref mut text, _)) = current_text_state {
                    let text_content = String::from_utf8_lossy(&e);
                    text.push_str(&text_content);
                }
            }
            Event::End(e) => {
                let name = e.name().as_ref().to_vec();
                let tag = String::from_utf8_lossy(&name).to_ascii_lowercase();
                if tag == "g" {
                    if let Some(prev) = layer_stack.pop() {
                        current_layer = prev;
                    }
                    if let Some(prev_tf) = transform_stack.pop() {
                        current_transform = prev_tf;
                    }
                } else if tag == "text"
                    && let Some((pos, font_size, color, layer, text, tf)) =
                        current_text_state.take()
                {
                    let trimmed = text.trim();
                    if !trimmed.is_empty() {
                        doc.elements.push(SvgElement::Text {
                            pos: tf.apply(pos),
                            font_size,
                            color,
                            layer,
                            content: trimmed.to_string(),
                        });
                    }
                }
            }
            Event::Eof => break,
            _ => {}
        }
        buf.clear();
    }

    Ok(doc)
}

/// Extracts the embedded diagram or DSL source from the root `<svg>` element `content` attribute,
/// if present. Decodes XML entities safely.
pub fn extract_embedded_source(svg_bytes: &[u8]) -> Option<String> {
    let mut reader = Reader::from_reader(svg_bytes);
    reader.config_mut().trim_text(false);
    let mut buffer = Vec::new();

    loop {
        match reader.read_event_into(&mut buffer).ok()? {
            Event::Start(start) | Event::Empty(start) => {
                let name = start.name();
                if name.as_ref() == b"svg" {
                    for attr in start.attributes().flatten() {
                        if attr.key.as_ref() == b"content" {
                            let val_str = std::str::from_utf8(&attr.value).ok()?;
                            let unescaped = quick_xml::escape::unescape(val_str).ok()?;
                            if !unescaped.trim().is_empty() {
                                return Some(unescaped.into_owned());
                            }
                        }
                    }
                    return None;
                }
            }
            Event::Eof => return None,
            _ => {}
        }
        buffer.clear();
    }
}

/// Extracts the original source-format tag from the root SVG element.
pub fn extract_source_format(svg_bytes: &[u8]) -> Option<String> {
    let mut reader = Reader::from_reader(svg_bytes);
    reader.config_mut().trim_text(false);
    let mut buffer = Vec::new();
    loop {
        match reader.read_event_into(&mut buffer).ok()? {
            Event::Start(start) | Event::Empty(start) if start.name().as_ref() == b"svg" => {
                for attr in start.attributes().flatten() {
                    if attr.key.as_ref() == b"data-source-format" {
                        let value = std::str::from_utf8(&attr.value).ok()?;
                        return quick_xml::escape::unescape(value)
                            .ok()
                            .map(|value| value.into_owned());
                    }
                }
                return None;
            }
            Event::Eof => return None,
            _ => {}
        }
        buffer.clear();
    }
}
