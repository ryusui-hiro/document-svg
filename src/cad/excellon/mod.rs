//! Excellon NC Drill file parser and PCB drill-hole SVG renderer.
//!
//! Visualizes PCB plated through-holes and tooling holes with accurate drill diameters
//! aligned with standard Gerber RS-274X coordinates.

pub mod writer;

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read};

use crate::convert::{ConvertOptions, PageConsumer};
use crate::error::{Error, Result};
use crate::ir::{IDENTITY, LineCap, LineJoin, Node, Page, Paint, SourceMeta, Stroke};

const TARGET_PAGE_LONG_EDGE: f64 = 1200.0;
const MIN_PAGE_DIMENSION: f64 = 400.0;
const MAX_EXCELLON_HOLES: usize = 1_000_000;

pub(crate) fn convert<R: Read>(
    input: R,
    options: &ConvertOptions,
    sink: &mut dyn PageConsumer,
) -> Result<Vec<String>> {
    let mut warnings = Vec::new();
    let mut reader = BufReader::new(input);

    let mut is_metric = true; // default metric, or detect INCH
    let mut tools: HashMap<usize, f64> = HashMap::new(); // tool_num -> diameter_mm
    let mut current_tool = 1;
    let mut holes: Vec<(f64, f64, f64)> = Vec::new(); // (x_mm, y_mm, diameter_mm)

    let mut min_x = f64::INFINITY;
    let mut min_y = f64::INFINITY;
    let mut max_x = f64::NEG_INFINITY;
    let mut max_y = f64::NEG_INFINITY;

    let mut cur_x = 0.0;
    let mut cur_y = 0.0;
    let mut byte_count: u64 = 0;
    let mut line_buf = Vec::new();

    loop {
        line_buf.clear();
        let bytes_read = reader.read_until(b'\n', &mut line_buf)?;
        if bytes_read == 0 {
            break;
        }
        byte_count += bytes_read as u64;
        if byte_count > options.max_input_bytes {
            return Err(Error::LimitExceeded(format!(
                "Excellon drill input exceeds maximum bytes limit of {}",
                options.max_input_bytes
            )));
        }

        let line = match std::str::from_utf8(&line_buf) {
            Ok(s) => s.to_string(),
            Err(_) => line_buf.iter().map(|&b| b as char).collect::<String>(),
        };

        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with(';') {
            continue;
        }

        // Header and mode commands
        if trimmed.starts_with("INCH") || trimmed.contains("M72") {
            is_metric = false;
            continue;
        } else if trimmed.starts_with("METRIC") || trimmed.contains("M71") {
            is_metric = true;
            continue;
        } else if trimmed == "M30" || trimmed == "M00" {
            break;
        }

        // Tool definition: e.g. T01C0.032 or T1C0.8 or T01C0.032F100*
        if trimmed.starts_with('T') && trimmed.contains('C') {
            if let Some(c_pos) = trimmed.find('C') {
                let tool_str = &trimmed[1..c_pos];
                let diam_raw = trimmed[c_pos + 1..].trim_end_matches('*').trim();
                let num_part = diam_raw
                    .chars()
                    .take_while(|c| c.is_ascii_digit() || *c == '.' || *c == '-' || *c == '+')
                    .collect::<String>();
                if let (Ok(num), Some(diam)) = (
                    tool_str.parse::<usize>(),
                    crate::cad::dxf::geometry::parse_cad_float(&num_part),
                ) {
                    let diam_mm = if is_metric { diam } else { diam * 25.4 };
                    tools.insert(num, diam_mm);
                }
            }
            continue;
        }

        // Tool selection: e.g. T01
        if trimmed.starts_with('T') && !trimmed.contains('C') {
            let num_str = trimmed.trim_start_matches('T').trim_end_matches('*');
            if let Ok(num) = num_str.parse::<usize>() {
                current_tool = num;
            }
            continue;
        }

        // Coordinate parse: X...Y...
        if trimmed.contains('X') || trimmed.contains('Y') {
            let (px, py) = parse_coords(trimmed, is_metric, cur_x, cur_y);
            cur_x = px;
            cur_y = py;

            let diam_mm = tools.get(&current_tool).copied().unwrap_or(0.8);
            let r = diam_mm / 2.0;

            if px - r < min_x {
                min_x = px - r;
            }
            if px + r > max_x {
                max_x = px + r;
            }
            if py - r < min_y {
                min_y = py - r;
            }
            if py + r > max_y {
                max_y = py + r;
            }

            holes.push((px, py, diam_mm));
            if holes.len() >= MAX_EXCELLON_HOLES {
                warnings.push("Excellon holes limit reached; subsequent holes truncated".into());
                break;
            }
        }
    }

    if min_x >= max_x || min_y >= max_y {
        min_x = 0.0;
        min_y = 0.0;
        max_x = 50.0;
        max_y = 50.0;
        warnings.push(
            "Excellon file contains no valid drill coordinates; using default canvas bounds".into(),
        );
    }

    let raw_w = (max_x - min_x).max(1.0);
    let raw_h = (max_y - min_y).max(1.0);
    let margin = (raw_w.max(raw_h) * 0.1).max(5.0);
    let content_w = raw_w + 2.0 * margin;
    let content_h = raw_h + 2.0 * margin;

    let scale = (TARGET_PAGE_LONG_EDGE / content_w.max(content_h)).clamp(0.01, 100.0);
    let page_w = (content_w * scale).max(MIN_PAGE_DIMENSION);
    let page_h = (content_h * scale).max(MIN_PAGE_DIMENSION);

    let mut page = Page::new(1, page_w, page_h, "excellon");
    page.title = "PCB Drill Holes".into();

    let map_pt = |x: f64, y: f64| -> (f64, f64) {
        let sx = (x - min_x + margin) * scale;
        let sy = (max_y - y + margin) * scale; // Y inverted for SVG
        (sx, sy)
    };

    // Substrate background
    page.nodes.push(Node::Path {
        id: "pcb-substrate".into(),
        d: format!(
            "M 0 0 L {} 0 L {} {} L 0 {} Z",
            fmt_coord(page_w),
            fmt_coord(page_w),
            fmt_coord(page_h),
            fmt_coord(page_h)
        ),
        fill_rule: "nonzero".into(),
        fill: Paint::solid("#143d22"), // PCB solder mask green
        stroke: Stroke::default(),
        transform: IDENTITY,
        clip_id: None,
        meta: SourceMeta {
            semantic_role: "pcb:substrate".into(),
            ..Default::default()
        },
    });

    let mut hole_nodes = Vec::new();
    let pad_gold = Paint::solid("#e8be38");
    let hole_dark = Paint::solid("#0c2014");

    for (hx, hy, diam_mm) in holes {
        let p = map_pt(hx, hy);
        let hole_r = (diam_mm / 2.0) * scale;
        let pad_r = hole_r * 1.5;

        // Plated annular copper ring around hole
        hole_nodes.push(Node::Path {
            id: String::new(),
            d: circle_path(p.0, p.1, pad_r),
            fill_rule: "nonzero".into(),
            fill: pad_gold.clone(),
            stroke: Stroke::default(),
            transform: IDENTITY,
            clip_id: None,
            meta: SourceMeta::default(),
        });

        // Drilled physical hole cutout
        hole_nodes.push(Node::Path {
            id: String::new(),
            d: circle_path(p.0, p.1, hole_r),
            fill_rule: "nonzero".into(),
            fill: hole_dark.clone(),
            stroke: Stroke {
                paint: Paint::solid("#000000"),
                width: 0.5,
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

    if !hole_nodes.is_empty() {
        page.nodes.push(Node::Group {
            id: "drill-holes".into(),
            nodes: hole_nodes,
            transform: IDENTITY,
            opacity: 1.0,
            clip_id: None,
            meta: SourceMeta {
                semantic_role: "pcb:drill-holes".into(),
                ..Default::default()
            },
        });
    }

    sink.consume(page)?;
    Ok(warnings)
}

fn parse_coords(s: &str, is_metric: bool, default_x: f64, default_y: f64) -> (f64, f64) {
    let mut cur_x = default_x;
    let mut cur_y = default_y;

    let s_clean = s.trim_end_matches('*');

    if let Some(x_pos) = s_clean.find('X') {
        let rest = &s_clean[x_pos + 1..];
        let end = rest.find(['Y', 'M', 'G']).unwrap_or(rest.len());
        let val_str = &rest[..end];
        if let Ok(v) = parse_val(val_str, is_metric) {
            cur_x = v;
        }
    }

    if let Some(y_pos) = s_clean.find('Y') {
        let rest = &s_clean[y_pos + 1..];
        let end = rest.find(['X', 'M', 'G']).unwrap_or(rest.len());
        let val_str = &rest[..end];
        if let Ok(v) = parse_val(val_str, is_metric) {
            cur_y = v;
        }
    }

    (cur_x, cur_y)
}

fn parse_val(val_str: &str, is_metric: bool) -> std::result::Result<f64, ()> {
    let clean = val_str.trim();
    if clean.contains('.') {
        let v: f64 = crate::cad::dxf::geometry::parse_cad_float(clean).ok_or(())?;
        Ok(if is_metric { v } else { v * 25.4 })
    } else {
        let signed = clean.strip_prefix('+').unwrap_or(clean);
        let int_val: f64 = signed.parse().map_err(|_| ())?;
        // Standard Excellon: 2:4 inch (factor 10000) or 3:3 mm (factor 1000)
        let divisor = if is_metric { 1000.0 } else { 10000.0 };
        let unit_factor = if is_metric { 1.0 } else { 25.4 };
        Ok((int_val / divisor) * unit_factor)
    }
}

fn circle_path(cx: f64, cy: f64, r: f64) -> String {
    format!(
        "M {} {} A {} {} 0 1 0 {} {} A {} {} 0 1 0 {} {} Z",
        fmt_coord(cx - r),
        fmt_coord(cy),
        fmt_coord(r),
        fmt_coord(r),
        fmt_coord(cx + r),
        fmt_coord(cy),
        fmt_coord(r),
        fmt_coord(r),
        fmt_coord(cx - r),
        fmt_coord(cy)
    )
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
    fn parses_simple_excellon() {
        let drl = r#"M48
METRIC
T01C0.800
T02C1.500
%
T01
X10.0Y10.0
X20.0Y10.0
T02
X15.0Y25.0
M30
"#;
        let mut sink = DummySink(Vec::new());
        let options = ConvertOptions::default();
        let warnings = convert(Cursor::new(drl), &options, &mut sink).expect("convert excellon");
        assert!(warnings.is_empty());
        assert_eq!(sink.0.len(), 1);
        let page = &sink.0[0];
        assert_eq!(page.source_format, "excellon");
        assert!(page.nodes.len() >= 2);
    }

    #[test]
    fn parses_excellon_with_non_utf8_comments_and_feed_rates() {
        let mut drl_bytes = Vec::new();
        drl_bytes.extend_from_slice(b"; Bohrl\xF6cher f\xFCr Leiterplatte (Drill holes \xB0C)\n");
        drl_bytes.extend_from_slice(b"M48\n");
        drl_bytes.extend_from_slice(b"METRIC\n");
        drl_bytes.extend_from_slice(b"T01C0.8F100S50*\n"); // Tool with feed rate & star
        drl_bytes.extend_from_slice(b"%\n");
        drl_bytes.extend_from_slice(b"T01\n");
        drl_bytes.extend_from_slice(b"X.5Y.5\n"); // Omitted leading zero
        drl_bytes.extend_from_slice(b"X+10.5Y+20.5\n"); // Explicit plus sign
        drl_bytes.extend_from_slice(b"M30\n");

        let mut sink = DummySink(Vec::new());
        let options = ConvertOptions::default();
        let warnings =
            convert(Cursor::new(drl_bytes), &options, &mut sink).expect("convert excellon");
        assert!(warnings.is_empty());
        assert_eq!(sink.0.len(), 1);
        let page = &sink.0[0];
        assert_eq!(page.source_format, "excellon");
    }
}
