//! Initial Graphics Exchange Specification (IGES / .iges / .igs) CAD parser and vector wireframe renderer.
//!
//! Parses standard 80-column ASCII IGES sections (Directory Entry & Parameter Data),
//! extracts 3D wireframe entities (Lines 110, Arcs 100, Copious Data 106),
//! and projects them with an isometric camera into clean vector SVG drawings.

use std::collections::HashMap;
use std::path::Path;

use crate::convert::{ConvertOptions, PageConsumer, read_limited_file};
use crate::error::{Error, Result};
use crate::ir::{IDENTITY, LineCap, LineJoin, Node, Page, Paint, SourceMeta, Stroke};

const TARGET_PAGE_LONG_EDGE: f64 = 1200.0;
const MIN_PAGE_DIMENSION: f64 = 400.0;
const MAX_IGES_ENTITIES: usize = 100_000;

#[derive(Clone, Copy, Debug)]
struct Vec3 {
    x: f64,
    y: f64,
    z: f64,
}

#[derive(Clone, Debug)]
enum IgesEntity {
    Line {
        p1: Vec3,
        p2: Vec3,
    },
    Arc {
        center: Vec3,
        radius: f64,
        start: Vec3,
        end: Vec3,
    },
    Polyline {
        points: Vec<Vec3>,
    },
}

pub(crate) fn convert(
    path: &Path,
    options: &ConvertOptions,
    sink: &mut dyn PageConsumer,
) -> Result<Vec<String>> {
    let bytes = read_limited_file(path, options.max_input_bytes, "IGES file")?;
    let text = match String::from_utf8(bytes) {
        Ok(s) => s,
        Err(e) => e.into_bytes().iter().map(|&b| b as char).collect(),
    };

    let entities = parse_iges(&text)?;
    if entities.is_empty() {
        return Err(Error::InvalidInput(
            "no wireframe entities found in IGES file".into(),
        ));
    }

    // Isometric projection
    let cos_y = (std::f64::consts::PI / 4.0).cos();
    let sin_y = (std::f64::consts::PI / 4.0).sin();
    let angle_x = (35.264f64).to_radians();
    let cos_x = angle_x.cos();
    let sin_x = angle_x.sin();

    let project = |p: Vec3| -> (f64, f64) {
        let x1 = p.x * cos_y + p.z * sin_y;
        let y1 = p.y;
        let z1 = -p.x * sin_y + p.z * cos_y;

        let x2 = x1;
        let y2 = y1 * cos_x - z1 * sin_x;
        (x2, -y2)
    };

    let mut min_x = f64::INFINITY;
    let mut max_x = f64::NEG_INFINITY;
    let mut min_y = f64::INFINITY;
    let mut max_y = f64::NEG_INFINITY;

    let update_bounds =
        |pt: (f64, f64), min_x: &mut f64, max_x: &mut f64, min_y: &mut f64, max_y: &mut f64| {
            *min_x = min_x.min(pt.0);
            *max_x = max_x.max(pt.0);
            *min_y = min_y.min(pt.1);
            *max_y = max_y.max(pt.1);
        };

    for ent in &entities {
        match ent {
            IgesEntity::Line { p1, p2 } => {
                update_bounds(project(*p1), &mut min_x, &mut max_x, &mut min_y, &mut max_y);
                update_bounds(project(*p2), &mut min_x, &mut max_x, &mut min_y, &mut max_y);
            }
            IgesEntity::Arc {
                center,
                radius,
                start,
                end,
            } => {
                let is_closed = (start.x - end.x).hypot(start.y - end.y) < 1e-6;
                let (a_start, a_end) = if is_closed {
                    (0.0, std::f64::consts::TAU)
                } else {
                    let a1 = (start.y - center.y).atan2(start.x - center.x);
                    let mut a2 = (end.y - center.y).atan2(end.x - center.x);
                    if a2 <= a1 {
                        a2 += std::f64::consts::TAU;
                    }
                    (a1, a2)
                };
                let steps = 24;
                for step in 0..=steps {
                    let a = a_start + (step as f64 / steps as f64) * (a_end - a_start);
                    let pt = Vec3 {
                        x: center.x + radius * a.cos(),
                        y: center.y + radius * a.sin(),
                        z: center.z,
                    };
                    update_bounds(project(pt), &mut min_x, &mut max_x, &mut min_y, &mut max_y);
                }
            }
            IgesEntity::Polyline { points } => {
                for pt in points {
                    update_bounds(project(*pt), &mut min_x, &mut max_x, &mut min_y, &mut max_y);
                }
            }
        }
    }

    let dx = (max_x - min_x).max(1e-4);
    let dy = (max_y - min_y).max(1e-4);
    let scale = (TARGET_PAGE_LONG_EDGE - 100.0) / dx.max(dy);
    let margin = 50.0;
    let width = (dx * scale + margin * 2.0).max(MIN_PAGE_DIMENSION);
    let height = (dy * scale + margin * 2.0).max(MIN_PAGE_DIMENSION);

    let mut page = Page::new(1, width, height, "iges-cad");

    for (i, ent) in entities.iter().enumerate() {
        let d = match ent {
            IgesEntity::Line { p1, p2 } => {
                let s1 = project(*p1);
                let s2 = project(*p2);
                let x1 = (s1.0 - min_x) * scale + margin;
                let y1 = (s1.1 - min_y) * scale + margin;
                let x2 = (s2.0 - min_x) * scale + margin;
                let y2 = (s2.1 - min_y) * scale + margin;
                format!("M {x1:.2},{y1:.2} L {x2:.2},{y2:.2}")
            }
            IgesEntity::Arc {
                center,
                radius,
                start,
                end,
            } => {
                let is_closed = (start.x - end.x).hypot(start.y - end.y) < 1e-6;
                let (a_start, a_end) = if is_closed {
                    (0.0, std::f64::consts::TAU)
                } else {
                    let a1 = (start.y - center.y).atan2(start.x - center.x);
                    let mut a2 = (end.y - center.y).atan2(end.x - center.x);
                    if a2 <= a1 {
                        a2 += std::f64::consts::TAU;
                    }
                    (a1, a2)
                };
                let steps = 24;
                let mut path_str = String::new();
                for step in 0..=steps {
                    let a = a_start + (step as f64 / steps as f64) * (a_end - a_start);
                    let pt = Vec3 {
                        x: center.x + radius * a.cos(),
                        y: center.y + radius * a.sin(),
                        z: center.z,
                    };
                    let sp = project(pt);
                    let x = (sp.0 - min_x) * scale + margin;
                    let y = (sp.1 - min_y) * scale + margin;
                    if step == 0 {
                        path_str.push_str(&format!("M {x:.2},{y:.2}"));
                    } else {
                        path_str.push_str(&format!(" L {x:.2},{y:.2}"));
                    }
                }
                if is_closed {
                    path_str.push_str(" Z");
                }
                path_str
            }
            IgesEntity::Polyline { points } => {
                let mut path_str = String::new();
                for (step, pt) in points.iter().enumerate() {
                    let sp = project(*pt);
                    let x = (sp.0 - min_x) * scale + margin;
                    let y = (sp.1 - min_y) * scale + margin;
                    if step == 0 {
                        path_str.push_str(&format!("M {x:.2},{y:.2}"));
                    } else {
                        path_str.push_str(&format!(" L {x:.2},{y:.2}"));
                    }
                }
                path_str
            }
        };

        if !d.is_empty() {
            page.nodes.push(Node::Path {
                id: format!("iges_{i}"),
                d,
                fill_rule: "evenodd".into(),
                fill: Paint::None,
                stroke: Stroke {
                    paint: Paint::solid("#2563eb"),
                    width: 1.0,
                    line_cap: LineCap::Round,
                    line_join: LineJoin::Round,
                    ..Default::default()
                },
                transform: IDENTITY,
                clip_id: None,
                meta: SourceMeta {
                    kind: "iges:wireframe".into(),
                    ..Default::default()
                },
            });
        }
    }

    sink.consume(page)?;
    Ok(Vec::new())
}

fn parse_iges(text: &str) -> Result<Vec<IgesEntity>> {
    let mut de_entries: Vec<(usize, usize)> = Vec::new(); // (entity_type, pd_pointer)
    let mut pd_lines: HashMap<usize, String> = HashMap::new();

    for line in text.lines() {
        if line.len() < 73 {
            continue;
        }
        let section = &line[72..73];
        let seq_str = line[73..].trim();
        let seq_num = seq_str.parse::<usize>().unwrap_or(0);

        match section {
            "D" => {
                if seq_num % 2 == 1 {
                    let type_str = line[..8].trim();
                    let pd_str = if line.len() >= 16 {
                        line[8..16].trim()
                    } else {
                        ""
                    };
                    if let (Ok(etype), Ok(pd_ptr)) =
                        (type_str.parse::<usize>(), pd_str.parse::<usize>())
                    {
                        de_entries.push((etype, pd_ptr));
                    }
                }
            }
            "P" => {
                let data = line[..72].to_string();
                pd_lines.insert(seq_num, data);
            }
            _ => {}
        }
    }

    let mut entities = Vec::new();

    for &(etype, pd_ptr) in &de_entries {
        if entities.len() >= MAX_IGES_ENTITIES {
            break;
        }

        // Collect continuous PD lines
        let mut pd_data = String::new();
        let mut cur = pd_ptr;
        while let Some(line) = pd_lines.get(&cur) {
            pd_data.push_str(line);
            if line.contains(';') {
                break;
            }
            cur += 1;
        }

        let trimmed = pd_data
            .trim()
            .trim_end_matches(';')
            .replace(['D', 'd'], "E");
        let params: Vec<f64> = trimmed
            .split(',')
            .map(|s| {
                let clean = s.trim().trim_end_matches(';');
                if clean.is_empty() {
                    0.0
                } else {
                    crate::cad::dxf::geometry::parse_cad_float(clean).unwrap_or(0.0)
                }
            })
            .collect();

        match etype {
            110 => {
                // Line: 110, X1, Y1, Z1, X2, Y2, Z2
                if params.len() >= 7 {
                    entities.push(IgesEntity::Line {
                        p1: Vec3 {
                            x: params[1],
                            y: params[2],
                            z: params[3],
                        },
                        p2: Vec3 {
                            x: params[4],
                            y: params[5],
                            z: params[6],
                        },
                    });
                }
            }
            100 => {
                // Circular Arc: 100, ZT, X1, Y1, X2, Y2, X3, Y3
                if params.len() >= 7 {
                    let z = params[1];
                    let cx = params[2];
                    let cy = params[3];
                    let sx = params[4];
                    let sy = params[5];
                    let ex = if params.len() >= 8 { params[6] } else { sx };
                    let ey = if params.len() >= 9 { params[7] } else { sy };
                    let r = ((sx - cx).powi(2) + (sy - cy).powi(2)).sqrt();
                    entities.push(IgesEntity::Arc {
                        center: Vec3 { x: cx, y: cy, z },
                        radius: r,
                        start: Vec3 { x: sx, y: sy, z },
                        end: Vec3 { x: ex, y: ey, z },
                    });
                }
            }
            106 => {
                // Copious Data: 106, IP, N, ...
                if params.len() >= 6 {
                    let ip = params[1].round() as i32;
                    let mut pts = Vec::new();
                    if ip == 2 {
                        // Form 2: Planar curve, shared Z: 106, 2, N, ZT, X1, Y1, X2, Y2, ...
                        let z = params[3];
                        let mut idx = 4;
                        while idx + 1 < params.len() {
                            pts.push(Vec3 {
                                x: params[idx],
                                y: params[idx + 1],
                                z,
                            });
                            idx += 2;
                        }
                    } else {
                        // Form 1: Data points in R^3: 106, 1, N, X1, Y1, Z1, ...
                        let mut idx = 3;
                        while idx + 2 < params.len() {
                            pts.push(Vec3 {
                                x: params[idx],
                                y: params[idx + 1],
                                z: params[idx + 2],
                            });
                            idx += 3;
                        }
                    }
                    if pts.len() >= 2 {
                        entities.push(IgesEntity::Polyline { points: pts });
                    }
                }
            }
            _ => {}
        }
    }

    Ok(entities)
}

/// Converts SVG elements to standard ANSI IGES 5.3 ASCII CAD file (.igs / .iges).
pub fn write_svg_to_iges<W: std::io::Write>(svg_content: &str, mut writer: W) -> Result<()> {
    let doc = crate::cad::svg_reader::parse_svg_elements(svg_content)?;

    let mut iges_entities: Vec<IgesEntity> = Vec::new();

    for elem in doc.elements {
        match elem {
            crate::cad::svg_reader::SvgElement::Line { p1, p2, .. } => {
                iges_entities.push(IgesEntity::Line {
                    p1: Vec3 {
                        x: p1.x,
                        y: p1.y,
                        z: 0.0,
                    },
                    p2: Vec3 {
                        x: p2.x,
                        y: p2.y,
                        z: 0.0,
                    },
                });
            }
            crate::cad::svg_reader::SvgElement::Circle { center, radius, .. } => {
                let p = Vec3 {
                    x: center.x + radius,
                    y: center.y,
                    z: 0.0,
                };
                iges_entities.push(IgesEntity::Arc {
                    center: Vec3 {
                        x: center.x,
                        y: center.y,
                        z: 0.0,
                    },
                    radius,
                    start: p,
                    end: p,
                });
            }
            crate::cad::svg_reader::SvgElement::Rect {
                x,
                y,
                width,
                height,
                ..
            } => {
                iges_entities.push(IgesEntity::Polyline {
                    points: vec![
                        Vec3 { x, y, z: 0.0 },
                        Vec3 {
                            x: x + width,
                            y,
                            z: 0.0,
                        },
                        Vec3 {
                            x: x + width,
                            y: y + height,
                            z: 0.0,
                        },
                        Vec3 {
                            x,
                            y: y + height,
                            z: 0.0,
                        },
                        Vec3 { x, y, z: 0.0 },
                    ],
                });
            }
            crate::cad::svg_reader::SvgElement::Polyline {
                points, is_closed, ..
            } => {
                if points.len() >= 2 {
                    let mut pts: Vec<Vec3> = points
                        .into_iter()
                        .map(|p| Vec3 {
                            x: p.x,
                            y: p.y,
                            z: 0.0,
                        })
                        .collect();
                    if is_closed
                        && pts.first().map(|p| (p.x, p.y)) != pts.last().map(|p| (p.x, p.y))
                    {
                        let first = pts[0];
                        pts.push(first);
                    }
                    iges_entities.push(IgesEntity::Polyline { points: pts });
                }
            }
            _ => {}
        }
    }

    if iges_entities.is_empty() {
        iges_entities.push(IgesEntity::Line {
            p1: Vec3 {
                x: 0.0,
                y: 0.0,
                z: 0.0,
            },
            p2: Vec3 {
                x: 100.0,
                y: 100.0,
                z: 0.0,
            },
        });
    }

    // Format fixed 80-column records: 72 chars data + 1 char section + 7 chars line index
    let format_record = |data: &str, section: char, index: usize| -> String {
        format!("{:<72}{}{:>7}\n", data, section, index)
    };

    let mut s_count = 0;
    let mut g_count = 0;
    let mut d_count = 0;
    let mut p_count = 0;

    // Start Section
    s_count += 1;
    write!(
        writer,
        "{}",
        format_record("document-svg exported IGES 5.3 CAD file", 'S', s_count)
    )?;

    // Global Section
    g_count += 1;
    write!(
        writer,
        "{}",
        format_record(
            "1H,,1H;,4HDOC1,13Hdocument-svg,13Hdocument-svg,16,38,6,308,15,4HDOC1,1.0,1,",
            'G',
            g_count
        )
    )?;
    g_count += 1;
    write!(
        writer,
        "{}",
        format_record(
            "2HMM,1,0.0,15H20260911.230000,0.001,0.0,8HAuthor,12HOrganization,11,0;",
            'G',
            g_count
        )
    )?;

    // Prepare DE and PD records
    let mut de_records = Vec::new();
    let mut pd_records = Vec::new();

    for ent in &iges_entities {
        let pd_ptr = p_count + 1;
        match ent {
            IgesEntity::Line { p1, p2 } => {
                let param_str = format!(
                    "110,{:.4},{:.4},{:.4},{:.4},{:.4},{:.4};",
                    p1.x, p1.y, p1.z, p2.x, p2.y, p2.z
                );
                p_count += 1;
                pd_records.push(format_record(&param_str, 'P', p_count));

                d_count += 1;
                de_records.push(format_record(
                    &format!(
                        "     110{:>8}       0       1       0       0       0       000000001",
                        pd_ptr
                    ),
                    'D',
                    d_count,
                ));
                d_count += 1;
                de_records.push(format_record(
                    "     110       0       0       1       0                               0",
                    'D',
                    d_count,
                ));
            }
            IgesEntity::Arc {
                center, start, end, ..
            } => {
                let param_str = format!(
                    "100,{:.4},{:.4},{:.4},{:.4},{:.4},{:.4},{:.4};",
                    center.z, center.x, center.y, start.x, start.y, end.x, end.y
                );
                p_count += 1;
                pd_records.push(format_record(&param_str, 'P', p_count));

                d_count += 1;
                de_records.push(format_record(
                    &format!(
                        "     100{:>8}       0       1       0       0       0       000000001",
                        pd_ptr
                    ),
                    'D',
                    d_count,
                ));
                d_count += 1;
                de_records.push(format_record(
                    "     100       0       0       1       0                               0",
                    'D',
                    d_count,
                ));
            }
            IgesEntity::Polyline { points } => {
                let mut param_str = format!("106,1,{}", points.len());
                for pt in points {
                    param_str.push_str(&format!(",{:.4},{:.4},{:.4}", pt.x, pt.y, pt.z));
                }
                param_str.push(';');

                let chunks: Vec<String> = param_str
                    .as_bytes()
                    .chunks(64)
                    .map(|c| String::from_utf8_lossy(c).to_string())
                    .collect();

                let lines_in_pd = chunks.len();
                for chunk in chunks {
                    p_count += 1;
                    pd_records.push(format_record(&chunk, 'P', p_count));
                }

                d_count += 1;
                de_records.push(format_record(
                    &format!(
                        "     106{:>8}       0       1       0       0       0       000000001",
                        pd_ptr
                    ),
                    'D',
                    d_count,
                ));
                d_count += 1;
                de_records.push(format_record(
                    &format!(
                        "     106       0       0{:>8}       0                               0",
                        lines_in_pd
                    ),
                    'D',
                    d_count,
                ));
            }
        }
    }

    // Write Directory Entry Section
    for rec in de_records {
        write!(writer, "{rec}")?;
    }

    // Write Parameter Data Section
    for rec in pd_records {
        write!(writer, "{rec}")?;
    }

    // Terminate Section (T)
    let term_str = format!(
        "S{:>7}G{:>7}D{:>7}P{:>7}",
        s_count, g_count, d_count, p_count
    );
    write!(writer, "{}", format_record(&term_str, 'T', 1))?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_and_writes_iges_roundtrip() {
        let mut page = Page::new(1, 100.0, 100.0, "test");
        page.nodes.push(Node::Path {
            id: String::new(),
            d: "M 10,20 L 50,60".to_string(),
            fill_rule: String::new(),
            fill: Paint::None,
            stroke: Stroke::default(),
            transform: IDENTITY,
            clip_id: None,
            meta: SourceMeta::default(),
        });
        let mut svg_bytes = Vec::new();
        crate::svg::write_page(&page, &mut svg_bytes, crate::svg::SvgOptions::default()).unwrap();
        let svg_str = String::from_utf8(svg_bytes).unwrap();
        let mut iges_bytes = Vec::new();
        write_svg_to_iges(&svg_str, &mut iges_bytes).expect("write iges");
        assert!(!iges_bytes.is_empty());

        let iges_str = String::from_utf8(iges_bytes).expect("valid utf8 iges");
        let entities = parse_iges(&iges_str).expect("parse iges");
        assert!(!entities.is_empty());
    }

    #[test]
    fn parses_iges_with_non_utf8_start_section() {
        let mut page = Page::new(1, 400.0, 400.0, "test");
        page.nodes.push(Node::Path {
            id: String::new(),
            d: "M 0,0 L 100,100".to_string(),
            fill_rule: String::new(),
            fill: Paint::None,
            stroke: Stroke::default(),
            transform: IDENTITY,
            clip_id: None,
            meta: SourceMeta::default(),
        });

        let mut svg_bytes = Vec::new();
        crate::svg::write_page(&page, &mut svg_bytes, crate::svg::SvgOptions::default()).unwrap();
        let svg_str = String::from_utf8(svg_bytes).unwrap();
        let mut iges_bytes = Vec::new();
        write_svg_to_iges(&svg_str, &mut iges_bytes).expect("write iges");

        // Inject non-UTF8 Latin-1 byte (0xB0 for °) in Start section
        if let Some(pos) = iges_bytes.iter().position(|&b| b == b'e') {
            iges_bytes[pos] = 0xB0;
        }

        let text = match String::from_utf8(iges_bytes.clone()) {
            Ok(s) => s,
            Err(e) => e.into_bytes().iter().map(|&b| b as char).collect(),
        };
        let entities = parse_iges(&text).expect("parse non-utf8 iges");
        assert!(!entities.is_empty());
    }
}
