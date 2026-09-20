//! SVG to HP-GL / HP-GL/2 plotter commands reverse converter.
//!
//! Converts SVG vector artwork (lines, rectangles, circles, polyline paths) into standard
//! HP-GL pen-plotter instructions (IN, DF, SP, PA, PU, PD, CI, AA).

use std::io::Write;

use crate::cad::svg_reader::{SvgElement, parse_svg_elements};
use crate::error::Result;

/// Converts SVG document content to standard HP-GL plotter commands.
pub fn write_svg_to_hpgl<W: Write>(svg_content: &str, mut writer: W) -> Result<()> {
    let doc = parse_svg_elements(svg_content)?;
    let view_height = doc.height;

    let mut polylines: Vec<(Vec<(f64, f64)>, usize, f64)> = Vec::new(); // pts, pen, width
    let mut circles: Vec<(f64, f64, f64, usize)> = Vec::new(); // cx, cy, r, pen

    for elem in doc.elements {
        match elem {
            SvgElement::Line {
                p1,
                p2,
                stroke_color,
                stroke_width,
                ..
            } => {
                let pen = color_opt_to_pen(stroke_color);
                polylines.push((
                    vec![(p1.x, view_height - p1.y), (p2.x, view_height - p2.y)],
                    pen,
                    stroke_width,
                ));
            }
            SvgElement::Circle {
                center,
                radius,
                stroke_color,
                fill_color,
                ..
            } => {
                let pen = color_opt_to_pen(stroke_color.or(fill_color));
                circles.push((center.x, view_height - center.y, radius, pen));
            }
            SvgElement::Rect {
                x,
                y,
                width,
                height,
                stroke_color,
                fill_color,
                ..
            } => {
                let pen = color_opt_to_pen(stroke_color.or(fill_color));
                let y0 = view_height - y;
                let y1 = view_height - (y + height);
                polylines.push((
                    vec![(x, y0), (x + width, y0), (x + width, y1), (x, y1), (x, y0)],
                    pen,
                    0.35,
                ));
            }
            SvgElement::Polyline {
                points,
                is_closed,
                stroke_color,
                fill_color,
                ..
            } => {
                if points.len() >= 2 {
                    let pen = color_opt_to_pen(stroke_color.or(fill_color));
                    let mut pts: Vec<(f64, f64)> = points
                        .into_iter()
                        .map(|p| (p.x, view_height - p.y))
                        .collect();
                    if is_closed && pts.first() != pts.last() {
                        if let Some(&first) = pts.first() {
                            pts.push(first);
                        }
                    }
                    polylines.push((pts, pen, 0.35));
                }
            }
            SvgElement::Text { .. } => {}
        }
    }

    // Write standard HP-GL plotter initialization
    // IN = Initialize, DF = Default values, IP = Input P1 and P2 coordinates
    write!(writer, "IN;DF;SP1;")?;

    let mut current_pen = 1;

    for (pts, pen, width) in polylines {
        if pts.len() < 2 {
            continue;
        }
        if pen != current_pen {
            write!(writer, "SP{pen};PW{width:.2};")?;
            current_pen = pen;
        }
        let start = pts[0];
        // PA = Plot Absolute, PU = Pen Up, PD = Pen Down
        write!(writer, "PA{:.0},{:.0};PD", start.0 * 10.0, start.1 * 10.0)?;
        for pt in &pts[1..] {
            write!(writer, "{:.0},{:.0},", pt.0 * 10.0, pt.1 * 10.0)?;
        }
        write!(writer, ";PU;")?;
    }

    for (cx, cy, r, pen) in circles {
        if pen != current_pen {
            write!(writer, "SP{pen};")?;
            current_pen = pen;
        }
        // CI = Circle (radius in plotter units)
        write!(
            writer,
            "PA{:.0},{:.0};CI{:.0};",
            cx * 10.0,
            cy * 10.0,
            r * 10.0
        )?;
    }

    // Park pen (SP0) and end
    writeln!(writer, "SP0;")?;
    Ok(())
}

fn color_opt_to_pen(color: Option<u32>) -> usize {
    let Some(c) = color else {
        return 1;
    };
    let r = ((c >> 16) & 0xff) as i32;
    let g = ((c >> 8) & 0xff) as i32;
    let b = (c & 0xff) as i32;

    let palette = [
        (1, 0, 0, 0),     // Black
        (2, 255, 0, 0),   // Red
        (3, 0, 255, 0),   // Green
        (4, 255, 255, 0), // Yellow
        (5, 0, 0, 255),   // Blue
        (6, 255, 0, 255), // Magenta
        (7, 0, 255, 255), // Cyan
        (8, 255, 165, 0), // Orange
    ];

    let mut best_pen = 1;
    let mut min_dist = i32::MAX;
    for (pen, pr, pg, pb) in palette {
        let dist = (r - pr).pow(2) + (g - pg).pow(2) + (b - pb).pow(2);
        if dist < min_dist {
            min_dist = dist;
            best_pen = pen;
        }
    }
    best_pen
}
