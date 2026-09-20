//! Excalidraw JSON diagram parser and SVG vector renderer.
//!
//! Parses `.excalidraw` and `.excalidraw.json` documents containing vector shapes
//! (rectangles, ellipses, diamonds, lines, arrows, freedraw paths, and text),
//! normalizes coordinates into a positive bounding box, and renders styled SVG elements.

use std::path::Path;

use serde::Deserialize;

use crate::convert::{ConvertOptions, PageConsumer, read_limited_file};
use crate::error::{Error, Result};
use crate::ir::{
    IDENTITY, LineCap, LineJoin, Node, Page, Paint, SourceMeta, Stroke, TextAnchor, TextRun,
};

fn default_stroke() -> String {
    "#1e1e1e".into()
}
fn default_transparent() -> String {
    "transparent".into()
}
fn default_stroke_width() -> f64 {
    2.0
}
fn default_opacity() -> f64 {
    100.0
}

#[derive(Debug, Deserialize)]
struct ExcalidrawDoc {
    #[serde(default)]
    elements: Vec<ExcalidrawElement>,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct ExcalidrawElement {
    #[serde(default)]
    id: String,
    #[serde(rename = "type")]
    element_type: String,
    #[serde(default)]
    x: f64,
    #[serde(default)]
    y: f64,
    #[serde(default)]
    width: f64,
    #[serde(default)]
    height: f64,
    #[serde(rename = "strokeColor", default = "default_stroke")]
    stroke_color: String,
    #[serde(rename = "backgroundColor", default = "default_transparent")]
    background_color: String,
    #[serde(rename = "strokeWidth", default = "default_stroke_width")]
    stroke_width: f64,
    #[serde(rename = "strokeStyle", default)]
    stroke_style: String,
    #[serde(default = "default_opacity")]
    opacity: f64,
    #[serde(default)]
    text: Option<String>,
    #[serde(rename = "fontSize")]
    font_size: Option<f64>,
    #[serde(default)]
    points: Option<Vec<[f64; 2]>>,
    #[serde(rename = "isDeleted", default)]
    is_deleted: bool,
}

pub(crate) fn convert(
    path: &Path,
    options: &ConvertOptions,
    sink: &mut dyn PageConsumer,
) -> Result<Vec<String>> {
    let bytes = read_limited_file(path, options.max_input_bytes, "excalidraw file")?;
    let text = String::from_utf8(bytes)
        .map_err(|e| Error::InvalidInput(format!("excalidraw file is not valid UTF-8: {e}")))?;

    let doc: ExcalidrawDoc = serde_json::from_str(&text)
        .map_err(|e| Error::InvalidInput(format!("failed to parse excalidraw JSON: {e}")))?;

    let active_elements: Vec<&ExcalidrawElement> = doc
        .elements
        .iter()
        .filter(|e| !e.is_deleted && e.element_type != "selection")
        .collect();

    if active_elements.is_empty() {
        return Err(Error::InvalidInput(
            "excalidraw contains no active elements".into(),
        ));
    }

    // Compute bounding box
    let mut min_x = f64::INFINITY;
    let mut max_x = f64::NEG_INFINITY;
    let mut min_y = f64::INFINITY;
    let mut max_y = f64::NEG_INFINITY;

    for el in &active_elements {
        let mut el_min_x = el.x;
        let mut el_max_x = el.x + el.width.abs();
        let mut el_min_y = el.y;
        let mut el_max_y = el.y + el.height.abs();

        if let Some(pts) = &el.points {
            for &[px, py] in pts {
                el_min_x = el_min_x.min(el.x + px);
                el_max_x = el_max_x.max(el.x + px);
                el_min_y = el_min_y.min(el.y + py);
                el_max_y = el_max_y.max(el.y + py);
            }
        }

        min_x = min_x.min(el_min_x);
        max_x = max_x.max(el_max_x);
        min_y = min_y.min(el_min_y);
        max_y = max_y.max(el_max_y);
    }

    let margin = 40.0;
    let width = ((max_x - min_x) + margin * 2.0).max(200.0);
    let height = ((max_y - min_y) + margin * 2.0).max(150.0);

    let mut page = Page::new(1, width, height, "excalidraw");

    for (idx, el) in active_elements.iter().enumerate() {
        let node_id = if el.id.is_empty() {
            format!("excalidraw_{idx}")
        } else {
            el.id.clone()
        };
        let ox = (el.x - min_x) + margin;
        let oy = (el.y - min_y) + margin;
        let w = el.width.abs();
        let h = el.height.abs();
        let _opacity = (el.opacity / 100.0).clamp(0.0, 1.0);

        let fill = if el.background_color.is_empty() || el.background_color == "transparent" {
            Paint::None
        } else {
            Paint::solid(&el.background_color)
        };

        let dash_array = match el.stroke_style.as_str() {
            "dashed" => vec![8.0, 6.0],
            "dotted" => vec![3.0, 3.0],
            _ => Vec::new(),
        };

        let stroke = if el.stroke_color.is_empty() || el.stroke_color == "transparent" {
            Stroke::default()
        } else {
            Stroke {
                paint: Paint::solid(&el.stroke_color),
                width: el.stroke_width.max(0.5),
                line_cap: LineCap::Round,
                line_join: LineJoin::Round,
                dash_array,
                ..Default::default()
            }
        };

        match el.element_type.as_str() {
            "rectangle" => {
                let d = format!("M {ox:.2},{oy:.2} h {w:.2} v {h:.2} h -{w:.2} Z");
                page.nodes.push(Node::Path {
                    id: node_id,
                    d,
                    fill_rule: "evenodd".into(),
                    fill,
                    stroke,
                    transform: IDENTITY,
                    clip_id: None,
                    meta: SourceMeta {
                        kind: "excalidraw:rectangle".into(),
                        ..Default::default()
                    },
                });
            }
            "ellipse" => {
                let cx = ox + w / 2.0;
                let cy = oy + h / 2.0;
                let rx = w / 2.0;
                let ry = h / 2.0;
                let kx = rx * 0.55228475;
                let ky = ry * 0.55228475;
                let d = format!(
                    "M {cx:.2},{cy_minus:.2} C {cx_plus_kx:.2},{cy_minus:.2} {cx_plus_rx:.2},{cy_minus_ky:.2} {cx_plus_rx:.2},{cy:.2} C {cx_plus_rx:.2},{cy_plus_ky:.2} {cx_plus_kx:.2},{cy_plus_ry:.2} {cx:.2},{cy_plus_ry:.2} C {cx_minus_kx:.2},{cy_plus_ry:.2} {cx_minus_rx:.2},{cy_plus_ky:.2} {cx_minus_rx:.2},{cy:.2} C {cx_minus_rx:.2},{cy_minus_ky:.2} {cx_minus_kx:.2},{cy_minus:.2} {cx:.2},{cy_minus:.2} Z",
                    cy_minus = cy - ry,
                    cx_plus_kx = cx + kx,
                    cx_plus_rx = cx + rx,
                    cy_minus_ky = cy - ky,
                    cy_plus_ky = cy + ky,
                    cy_plus_ry = cy + ry,
                    cx_minus_kx = cx - kx,
                    cx_minus_rx = cx - rx,
                );
                page.nodes.push(Node::Path {
                    id: format!("excalidraw_{idx}"),
                    d,
                    fill_rule: "evenodd".into(),
                    fill,
                    stroke,
                    transform: IDENTITY,
                    clip_id: None,
                    meta: SourceMeta {
                        kind: "excalidraw:ellipse".into(),
                        ..Default::default()
                    },
                });
            }
            "diamond" => {
                let cx = ox + w / 2.0;
                let cy = oy + h / 2.0;
                let top = oy;
                let right = ox + w;
                let bottom = oy + h;
                let left = ox;
                let d = format!(
                    "M {cx:.2},{top:.2} L {right:.2},{cy:.2} L {cx:.2},{bottom:.2} L {left:.2},{cy:.2} Z"
                );
                page.nodes.push(Node::Path {
                    id: node_id.clone(),
                    d,
                    fill_rule: "evenodd".into(),
                    fill,
                    stroke,
                    transform: IDENTITY,
                    clip_id: None,
                    meta: SourceMeta {
                        kind: "excalidraw:diamond".into(),
                        ..Default::default()
                    },
                });
            }
            "line" | "arrow" | "freedraw" => {
                if let Some(pts) = el.points.as_deref().filter(|pts| !pts.is_empty()) {
                    let mut d = String::new();
                    for (step, &[px, py]) in pts.iter().enumerate() {
                        let x = ox + px;
                        let y = oy + py;
                        if step == 0 {
                            d.push_str(&format!("M {x:.2},{y:.2}"));
                        } else {
                            d.push_str(&format!(" L {x:.2},{y:.2}"));
                        }
                    }

                    // Arrowhead
                    if el.element_type == "arrow" && pts.len() >= 2 {
                        let last = pts[pts.len() - 1];
                        let prev = pts[pts.len() - 2];
                        let dx = last[0] - prev[0];
                        let dy = last[1] - prev[1];
                        let len = (dx * dx + dy * dy).sqrt().max(1e-4);
                        let ux = dx / len;
                        let uy = dy / len;
                        let arrow_len = 12.0;
                        let arrow_w = 6.0;

                        let end_x = ox + last[0];
                        let end_y = oy + last[1];
                        let left_x = end_x - arrow_len * ux + arrow_w * -uy;
                        let left_y = end_y - arrow_len * uy + arrow_w * ux;
                        let right_x = end_x - arrow_len * ux - arrow_w * -uy;
                        let right_y = end_y - arrow_len * uy - arrow_w * ux;

                        d.push_str(&format!(" M {left_x:.2},{left_y:.2} L {end_x:.2},{end_y:.2} L {right_x:.2},{right_y:.2}"));
                    }

                    page.nodes.push(Node::Path {
                        id: node_id.clone(),
                        d,
                        fill_rule: "evenodd".into(),
                        fill: Paint::None,
                        stroke,
                        transform: IDENTITY,
                        clip_id: None,
                        meta: SourceMeta {
                            kind: format!("excalidraw:{}", el.element_type),
                            ..Default::default()
                        },
                    });
                }
            }
            "text" => {
                if let Some(txt) = &el.text {
                    let font_size = el.font_size.unwrap_or(20.0);
                    let mut runs = Vec::new();
                    for line in txt.lines() {
                        runs.push(TextRun {
                            text: line.to_string(),
                            font_family: "sans-serif".into(),
                            font_size,
                            bold: false,
                            italic: false,
                            fill: Paint::solid(&el.stroke_color),
                            baseline_shift: 0.0,
                            glyph_x_offsets: Vec::new(),
                            target_advance: None,
                        });
                    }
                    page.nodes.push(Node::Text {
                        id: node_id,
                        x: ox,
                        y: oy + font_size * 0.85,
                        runs,
                        anchor: TextAnchor::Start,
                        transform: IDENTITY,
                        opacity: 1.0,
                        stroke: Stroke::default(),
                        clip_id: None,
                        meta: SourceMeta {
                            semantic_role: "excalidraw:text".into(),
                            ..Default::default()
                        },
                    });
                }
            }
            _ => {}
        }
    }

    sink.consume(page)?;
    Ok(Vec::new())
}
