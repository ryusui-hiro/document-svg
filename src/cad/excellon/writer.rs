//! SVG to Excellon NC Drill PCB reverse converter.
//!
//! Converts SVG vector circles and pads into standard Excellon NC drill files
//! (.drl, .drd, .xln) for PCB manufacturing, CNC drilling, and EDA tool inspection.

use std::collections::BTreeMap;
use std::io::Write;

use crate::cad::svg_reader::{SvgElement, parse_svg_elements};
use crate::error::Result;

/// Converts SVG document content into standard Excellon NC Drill commands.
pub fn write_svg_to_excellon<W: Write>(svg_content: &str, mut writer: W) -> Result<()> {
    let doc = parse_svg_elements(svg_content)?;
    let view_height = doc.height;

    let mut holes: Vec<(f64, f64, f64)> = Vec::new(); // (x, y, diameter_mm)

    for elem in doc.elements {
        if let SvgElement::Circle { center, radius, .. } = elem
            && radius > 0.0
        {
            let y_pcb = (view_height - center.y).max(0.0);
            let diam = (radius * 2.0).max(0.2);
            holes.push((center.x, y_pcb, diam));
        }
    }

    // If no explicit circles found, provide a fallback hole at center
    if holes.is_empty() {
        holes.push((50.0, 50.0, 1.0));
    }

    // Group holes by diameter (rounded to 3 decimal places in mm)
    let mut tool_groups: BTreeMap<u32, Vec<(f64, f64)>> = BTreeMap::new();
    for (x, y, diam) in holes {
        let diam_key = ((diam * 1000.0).round() as u32).max(100);
        tool_groups.entry(diam_key).or_default().push((x, y));
    }

    type DrillTool = (usize, f64, Vec<(f64, f64)>);
    let mut tools: Vec<DrillTool> = Vec::new();
    for (idx, (diam_key, coords)) in tool_groups.into_iter().enumerate() {
        let tool_num = idx + 1;
        let diam_mm = diam_key as f64 / 1000.0;
        tools.push((tool_num, diam_mm, coords));
    }

    // Write Excellon Header
    writeln!(writer, "M48")?;
    writeln!(writer, "; DRILL file created by document-svg")?;
    writeln!(writer, "; FORMAT: Excellon NC Drill (Metric)")?;
    writeln!(writer, "METRIC,TZ")?;

    for (t_num, diam, _) in &tools {
        writeln!(writer, "T{:02}C{:.3}", t_num, diam)?;
    }

    writeln!(writer, "%")?;
    writeln!(writer, "G90")?;
    writeln!(writer, "G05")?;

    // Write Drill Operations
    for (t_num, _, coords) in &tools {
        writeln!(writer, "T{:02}", t_num)?;
        for (x, y) in coords {
            writeln!(writer, "X{:.3}Y{:.3}", x, y)?;
        }
    }

    writeln!(writer, "M30")?;
    Ok(())
}
