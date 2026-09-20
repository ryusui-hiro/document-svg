//! SVG to AutoCAD DXF reverse converter.

#![allow(clippy::collapsible_if)]

use std::collections::HashSet;
use std::io::Write;

use crate::cad::svg_reader::{SvgElement, parse_svg_elements};
use crate::error::Result;

pub fn write_svg_to_dxf<W: Write>(svg_content: &str, mut writer: W) -> Result<()> {
    let doc = parse_svg_elements(svg_content)?;
    let view_height = doc.height;

    let mut layers: HashSet<String> = HashSet::new();
    layers.insert("0".into());

    for elem in &doc.elements {
        match elem {
            SvgElement::Line { layer, .. }
            | SvgElement::Rect { layer, .. }
            | SvgElement::Circle { layer, .. }
            | SvgElement::Polyline { layer, .. }
            | SvgElement::Text { layer, .. } => {
                if !layer.is_empty() {
                    layers.insert(layer.clone());
                }
            }
        }
    }

    // Write complete standard ASCII DXF (Release 12 / AC1009 compatible for universal import)
    writeln!(
        writer,
        "  0
SECTION
  2
HEADER
  9

  1
AC1009
  9

 70
4
  0
ENDSEC"
    )?;

    // TABLES section
    writeln!(
        writer,
        "  0
SECTION
  2
TABLES
  0
TABLE
  2
LAYER"
    )?;
    for layer in &layers {
        writeln!(
            writer,
            "  0
LAYER
  2
{layer}
 70
0
 62
7
  6
CONTINUOUS"
        )?;
    }
    writeln!(
        writer,
        "  0
ENDTAB
  0
ENDSEC"
    )?;

    // BLOCKS section
    writeln!(
        writer,
        "  0
SECTION
  2
BLOCKS
  0
ENDSEC"
    )?;

    // ENTITIES section
    writeln!(
        writer,
        "  0
SECTION
  2
ENTITIES"
    )?;
    for elem in &doc.elements {
        match elem {
            SvgElement::Line {
                p1,
                p2,
                stroke_color,
                layer,
                ..
            } => {
                if !p1.x.is_finite() || !p1.y.is_finite() || !p2.x.is_finite() || !p2.y.is_finite()
                {
                    continue;
                }
                let dy1 = view_height - p1.y;
                let dy2 = view_height - p2.y;
                write!(
                    writer,
                    "  0
LINE
  8
{layer}
 10
{:.4}
 20
{:.4}
 11
{:.4}
 21
{:.4}
",
                    p1.x, dy1, p2.x, dy2
                )?;
                if let Some(tc) = stroke_color {
                    let aci = rgb_to_aci(*tc);
                    write!(writer, "  62\n{aci}\n 420\n{tc}\n")?;
                }
            }
            SvgElement::Rect {
                x,
                y,
                width,
                height,
                stroke_color,
                fill_color,
                layer,
            } => {
                if !x.is_finite()
                    || !y.is_finite()
                    || !width.is_finite()
                    || !height.is_finite()
                    || *width <= 0.0
                    || *height <= 0.0
                {
                    continue;
                }
                let y0 = view_height - y;
                let y1 = view_height - (y + height);
                let color = stroke_color.or(*fill_color);
                write!(
                    writer,
                    "  0
LWPOLYLINE
  8
{layer}
 90
4
 70
1
 10
{:.4}
 20
{:.4}
 10
{:.4}
 20
{:.4}
 10
{:.4}
 20
{:.4}
 10
{:.4}
 20
{:.4}
",
                    x,
                    y0,
                    x + width,
                    y0,
                    x + width,
                    y1,
                    x,
                    y1
                )?;
                if let Some(tc) = color {
                    let aci = rgb_to_aci(tc);
                    write!(writer, "  62\n{aci}\n 420\n{tc}\n")?;
                }
            }
            SvgElement::Circle {
                center,
                radius,
                stroke_color,
                fill_color,
                layer,
            } => {
                if !center.x.is_finite()
                    || !center.y.is_finite()
                    || !radius.is_finite()
                    || *radius <= 0.0
                {
                    continue;
                }
                let dy = view_height - center.y;
                let color = stroke_color.or(*fill_color);
                write!(
                    writer,
                    "  0
CIRCLE
  8
{layer}
 10
{:.4}
 20
{:.4}
 40
{:.4}
",
                    center.x, dy, radius
                )?;
                if let Some(tc) = color {
                    let aci = rgb_to_aci(tc);
                    write!(writer, "  62\n{aci}\n 420\n{tc}\n")?;
                }
            }
            SvgElement::Polyline {
                points,
                is_closed,
                stroke_color,
                fill_color,
                layer,
            } => {
                let valid_points: Vec<&_> = points
                    .iter()
                    .filter(|p| p.x.is_finite() && p.y.is_finite())
                    .collect();
                if valid_points.len() >= 2 {
                    let flag = if *is_closed { 1 } else { 0 };
                    let count = valid_points.len();
                    write!(
                        writer,
                        "  0
LWPOLYLINE
  8
{layer}
 90
{count}
 70
{flag}
"
                    )?;
                    for pt in valid_points {
                        let dy = view_height - pt.y;
                        write!(
                            writer,
                            " 10
{:.4}
 20
{:.4}
",
                            pt.x, dy
                        )?;
                    }
                    let color = stroke_color.or(*fill_color);
                    if let Some(tc) = color {
                        let aci = rgb_to_aci(tc);
                        write!(writer, "  62\n{aci}\n 420\n{tc}\n")?;
                    }
                }
            }
            SvgElement::Text {
                pos,
                font_size,
                color,
                layer,
                content,
            } => {
                if !pos.x.is_finite()
                    || !pos.y.is_finite()
                    || !font_size.is_finite()
                    || content.is_empty()
                {
                    continue;
                }
                let dy = view_height - pos.y;
                write!(
                    writer,
                    "  0
TEXT
  8
{layer}
 10
{:.4}
 20
{:.4}
 40
{:.4}
  1
{content}
",
                    pos.x, dy, font_size
                )?;
                if let Some(tc) = color {
                    let aci = rgb_to_aci(*tc);
                    write!(writer, "  62\n{aci}\n 420\n{tc}\n")?;
                }
            }
        }
    }
    writeln!(
        writer,
        "  0
ENDSEC
  0
EOF"
    )?;

    Ok(())
}

fn rgb_to_aci(rgb: u32) -> u16 {
    let r = ((rgb >> 16) & 0xff) as i32;
    let g = ((rgb >> 8) & 0xff) as i32;
    let b = (rgb & 0xff) as i32;

    let palette = [
        (1, 255, 0, 0),     // Red
        (2, 255, 255, 0),   // Yellow
        (3, 0, 255, 0),     // Green
        (4, 0, 255, 255),   // Cyan
        (5, 0, 0, 255),     // Blue
        (6, 255, 0, 255),   // Magenta
        (7, 255, 255, 255), // White
        (8, 128, 128, 128), // Dark Gray
        (9, 192, 192, 192), // Light Gray
    ];

    let mut best_idx = 7;
    let mut best_dist = i32::MAX;
    for (idx, cr, cg, cb) in palette {
        let dist = (r - cr).pow(2) + (g - cg).pow(2) + (b - cb).pow(2);
        if dist < best_dist {
            best_dist = dist;
            best_idx = idx;
        }
    }
    best_idx
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converts_simple_svg_to_dxf() {
        let svg = r##"<svg width="800" height="600" viewBox="0 0 800 600" xmlns="http://www.w3.org/2000/svg">
  <g id="layer-Walls">
    <line x1="100" y1="100" x2="500" y2="100" stroke="#ff0000" />
    <circle cx="300" cy="300" r="50" stroke="#00ff00" />
  </g>
</svg>"##;
        let mut out = Vec::new();
        write_svg_to_dxf(svg, &mut out).expect("write svg to dxf");
        let dxf = String::from_utf8(out).expect("utf8 dxf");

        assert!(dxf.contains(
            "SECTION
  2
HEADER"
        ));
        assert!(dxf.contains(
            "SECTION
  2
TABLES"
        ));
        assert!(dxf.contains(
            "LAYER
  2
Walls"
        ));
        assert!(dxf.contains(
            "LINE
  8
Walls"
        ));
        assert!(dxf.contains(
            "CIRCLE
  8
Walls"
        ));
        assert!(dxf.contains("EOF"));
    }

    #[test]
    fn converts_svg_with_curves_and_text_to_dxf() {
        let svg = r##"<svg width="1000" height="800" viewBox="0 0 1000 800" xmlns="http://www.w3.org/2000/svg">
  <g id="layer-TextAndCurves">
    <text x="50" y="100" font-size="16" fill="#112233">CAD Floor Plan</text>
    <path d="M 100 200 C 150 250 250 250 300 200 S 450 150 500 200 Q 600 300 700 200 T 900 200 Z" stroke="#336699" />
  </g>
</svg>"##;
        let mut out = Vec::new();
        write_svg_to_dxf(svg, &mut out).expect("write svg with curves and text");
        let dxf = String::from_utf8(out).expect("utf8 dxf");

        assert!(dxf.contains(
            "LAYER
  2
TextAndCurves"
        ));
        assert!(dxf.contains(
            "TEXT
  8
TextAndCurves"
        ));
        assert!(dxf.contains("CAD Floor Plan"));
        assert!(dxf.contains(
            "LWPOLYLINE
  8
TextAndCurves"
        ));
    }
}
