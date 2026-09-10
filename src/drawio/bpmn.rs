//! BPMN events, gateways and activities.
//!
//! draw.io builds these from three layers the style names separately: the
//! gateway diamond behind, the event ring itself, and the symbol that says
//! which event it is.

use super::*;

/// Draw a BPMN event, gateway or activity marker.
///
/// draw.io builds these from three layers rather than from one outline: an
/// optional gateway diamond behind, the event ring itself, and the symbol that
/// says which event it is. A throwing or end event inverts the symbol's colours,
/// which is what makes a terminate event a solid disc and a throwing message a
/// white envelope.
pub(super) fn draw_bpmn(cell: &Cell, rect: Rect, transform: Matrix, nodes: &mut Vec<Node>) -> bool {
    let style = &cell.style;
    let fill = match style.get("fillcolor") {
        Some(value) => parse_color(value),
        None => parse_color("#FFFFFF"),
    };
    let stroke = match style.get("strokecolor") {
        Some(value) => parse_color(value),
        None => parse_color("#000000"),
    };
    let width = style.number("strokewidth", 1.0).max(0.0);
    let opacity = (style.number("opacity", 100.0) / 100.0).clamp(0.0, 1.0);
    let dashed = style.flag("dashed");
    let identity = format!("drawio-{}", cell.id);
    let mut index = 0usize;
    let mut paint =
        |d: String, fill: Option<&str>, stroke: Option<&str>, width: f64, dashed: bool| {
            if d.is_empty() {
                return;
            }
            index += 1;
            nodes.push(Node::Path {
                id: format!("{identity}-bpmn-{index}"),
                d,
                fill_rule: "nonzero".into(),
                fill: fill.map_or(Paint::None, |color| Paint::Solid {
                    color: color.to_owned(),
                    opacity,
                }),
                stroke: stroke.map_or_else(Stroke::default, |color| Stroke {
                    paint: Paint::Solid {
                        color: color.to_owned(),
                        opacity,
                    },
                    width,
                    dash_array: if dashed {
                        let scale = width.max(1.0);
                        vec![3.0 * scale, 3.0 * scale]
                    } else {
                        Vec::new()
                    },
                    ..Stroke::default()
                }),
                transform,
                clip_id: None,
                meta: SourceMeta {
                    kind: "drawio-bpmn".into(),
                    source_id: cell.id.clone(),
                    ..SourceMeta::default()
                },
            });
        };
    // A gateway draws its diamond first, then holds the event in the middle
    // half of the box.
    let gateway = style.get("background") == Some("gateway");
    let inner = if gateway {
        let (cx, cy) = rect.center();
        paint(
            polygon_path(&[
                (cx, rect.y),
                (rect.right(), cy),
                (cx, rect.bottom()),
                (rect.x, cy),
            ]),
            fill.as_deref(),
            stroke.as_deref(),
            width,
            dashed,
        );
        Rect {
            x: rect.x + rect.width / 4.0,
            y: rect.y + rect.height / 4.0,
            width: rect.width / 2.0,
            height: rect.height / 2.0,
        }
    } else {
        rect
    };
    let outline = style.text("outline", "none");
    match outline.as_str() {
        "standard" | "eventInt" | "eventNonint" => {
            let dashed = dashed || outline == "eventNonint";
            paint(
                ellipse_path(inner),
                fill.as_deref(),
                stroke.as_deref(),
                width,
                dashed,
            );
        }
        "catching" | "boundInt" | "boundNonint" => {
            let dashed = dashed || outline == "boundNonint";
            paint(
                ellipse_path(inner),
                fill.as_deref(),
                stroke.as_deref(),
                width,
                dashed,
            );
            paint(
                ellipse_path(inner.grow(-2.0)),
                None,
                stroke.as_deref(),
                width,
                false,
            );
        }
        "throwing" => {
            paint(
                ellipse_path(inner),
                fill.as_deref(),
                stroke.as_deref(),
                width,
                dashed,
            );
            paint(
                ellipse_path(Rect {
                    x: inner.x + inner.width * 0.02 + 2.0,
                    y: inner.y + inner.height * 0.02 + 2.0,
                    width: inner.width * 0.96 - 4.0,
                    height: inner.height * 0.96 - 4.0,
                }),
                None,
                stroke.as_deref(),
                width,
                false,
            );
        }
        // An end event is the same ring at three times the stroke width.
        "end" => paint(
            ellipse_path(inner),
            fill.as_deref(),
            stroke.as_deref(),
            width * 3.0,
            dashed,
        ),
        _ => {
            if !gateway {
                paint(
                    ellipse_path(inner),
                    fill.as_deref(),
                    stroke.as_deref(),
                    width,
                    dashed,
                );
            }
        }
    }
    let symbol = style.text("symbol", "general");
    // A throwing or end event swaps the symbol's colours over.
    let inverse = matches!(outline.as_str(), "throwing" | "end");
    let (symbol_fill, symbol_stroke) = if inverse {
        (stroke.as_deref(), fill.as_deref())
    } else {
        (fill.as_deref(), stroke.as_deref())
    };
    draw_bpmn_symbol(
        &symbol,
        inner,
        symbol_fill,
        symbol_stroke,
        stroke.as_deref(),
        width,
        &mut paint,
    );
    true
}

/// The box a symbol occupies inside its event ring, as `mxBpmnShape` scales it.
pub(super) fn bpmn_symbol_box(symbol: &str, rect: Rect) -> Rect {
    let inset = |left: f64, top: f64, width: f64, height: f64| Rect {
        x: rect.x + rect.width * left,
        y: rect.y + rect.height * top,
        width: rect.width * width,
        height: rect.height * height,
    };
    match symbol {
        "message" => inset(0.15, 0.3, 0.7, 0.4),
        "timer" => inset(0.11, 0.11, 0.78, 0.78),
        "escalation" | "signal" => inset(0.19, 0.15, 0.62, 0.57),
        "conditional" => inset(0.3, 0.16, 0.4, 0.68),
        "link" => inset(0.27, 0.33, 0.46, 0.34),
        "error" => inset(0.212, 0.243, 0.58, 0.507),
        "cancel" => inset(0.22, 0.22, 0.56, 0.56),
        "compensation" => inset(0.28, 0.35, 0.44, 0.3),
        "multiple" => inset(0.2, 0.19, 0.6, 0.565),
        "parallelMultiple" => inset(0.2, 0.2, 0.6, 0.6),
        "terminate" => inset(0.05, 0.05, 0.9, 0.9),
        "exclusiveGw" => inset(0.12, 0.0, 0.76, 1.0),
        _ => rect,
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) fn draw_bpmn_symbol(
    symbol: &str,
    rect: Rect,
    fill: Option<&str>,
    stroke: Option<&str>,
    line_color: Option<&str>,
    width: f64,
    paint: &mut impl FnMut(String, Option<&str>, Option<&str>, f64, bool),
) {
    if symbol == "general" || symbol.is_empty() {
        return;
    }
    let box_rect = bpmn_symbol_box(symbol, rect);
    let Rect { x, y, .. } = box_rect;
    let (w, h) = (box_rect.width, box_rect.height);
    let point = |fx: f64, fy: f64| (x + w * fx, y + h * fy);
    let polygon = |points: &[(f64, f64)]| polygon_path(points);
    match symbol {
        "message" => {
            paint(rectangle_path(box_rect), fill, stroke, width, false);
            paint(
                format!(
                    "M {} {} L {} {} L {} {}",
                    n(point(0.0, 0.0).0),
                    n(point(0.0, 0.0).1),
                    n(point(0.5, 0.5).0),
                    n(point(0.5, 0.5).1),
                    n(point(1.0, 0.0).0),
                    n(point(1.0, 0.0).1)
                ),
                None,
                stroke,
                width,
                false,
            );
        }
        "timer" => {
            paint(ellipse_path(box_rect), fill, stroke, width, false);
            // Twelve marks around the dial, then the hands.
            let mut dial = String::new();
            for step in 0..12 {
                let angle = f64::from(step) * PI / 6.0;
                let (sin, cos) = angle.sin_cos();
                let outer = (x + w / 2.0 + sin * w * 0.5, y + h / 2.0 - cos * h * 0.5);
                let inner = (x + w / 2.0 + sin * w * 0.43, y + h / 2.0 - cos * h * 0.43);
                if !dial.is_empty() {
                    dial.push(' ');
                }
                dial.push_str(&format!(
                    "M {} {} L {} {}",
                    n(outer.0),
                    n(outer.1),
                    n(inner.0),
                    n(inner.1)
                ));
            }
            paint(dial, None, stroke, width, false);
            paint(
                format!(
                    "M {} {} L {} {} M {} {} L {} {}",
                    n(point(0.5, 0.5).0),
                    n(point(0.5, 0.5).1),
                    n(point(0.5, 0.13).0),
                    n(point(0.5, 0.13).1),
                    n(point(0.5, 0.5).0),
                    n(point(0.5, 0.5).1),
                    n(point(0.78, 0.62).0),
                    n(point(0.78, 0.62).1)
                ),
                None,
                stroke,
                width,
                false,
            );
        }
        "terminate" => paint(ellipse_path(box_rect), fill, stroke, width, false),
        "error" => paint(
            polygon(&[
                point(0.0, 1.0),
                point(0.3287, 0.123),
                point(0.6194, 0.6342),
                point(1.0, 0.0),
                point(0.6625, 0.939),
                point(0.3717, 0.5064),
            ]),
            fill,
            stroke,
            width,
            false,
        ),
        "link" => paint(
            polygon(&[
                point(0.0, 0.76),
                point(0.0, 0.24),
                point(0.63, 0.24),
                point(0.63, 0.0),
                point(1.0, 0.5),
                point(0.63, 1.0),
                point(0.63, 0.76),
            ]),
            fill,
            stroke,
            width,
            false,
        ),
        "signal" => paint(
            polygon(&[point(0.0, 1.0), point(0.5, 0.0), point(1.0, 1.0)]),
            fill,
            stroke,
            width,
            false,
        ),
        "escalation" => paint(
            polygon(&[
                point(0.0, 1.0),
                point(0.5, 0.0),
                point(1.0, 1.0),
                point(0.5, 0.5),
            ]),
            fill,
            stroke,
            width,
            false,
        ),
        "multiple" => paint(
            polygon(&[
                point(0.0, 0.39),
                point(0.5, 0.0),
                point(1.0, 0.39),
                point(0.815, 1.0),
                point(0.185, 1.0),
            ]),
            fill,
            stroke,
            width,
            false,
        ),
        "cancel" => paint(
            polygon(&[
                point(0.1051, 0.0),
                point(0.5, 0.3738),
                point(0.8909, 0.0),
                point(1.0, 0.1054),
                point(0.623, 0.5),
                point(1.0, 0.8926),
                point(0.8909, 1.0),
                point(0.5, 0.6242),
                point(0.1051, 1.0),
                point(0.0, 0.8926),
                point(0.373, 0.5),
                point(0.0, 0.1054),
            ]),
            fill,
            stroke,
            width,
            false,
        ),
        "compensation" => paint(
            format!(
                "{} {}",
                polygon(&[point(0.0, 0.5), point(0.5, 0.0), point(0.5, 1.0)]),
                polygon(&[point(0.5, 0.5), point(1.0, 0.0), point(1.0, 1.0)])
            ),
            fill,
            stroke,
            width,
            false,
        ),
        "conditional" => {
            paint(rectangle_path(box_rect), fill, stroke, width, false);
            let mut lines = String::new();
            for offset in [0.1027, 0.3669, 0.6311, 0.8953] {
                if !lines.is_empty() {
                    lines.push(' ');
                }
                lines.push_str(&format!(
                    "M {} {} L {} {}",
                    n(point(0.0, offset).0),
                    n(point(0.0, offset).1),
                    n(point(0.798, offset).0),
                    n(point(0.798, offset).1)
                ));
            }
            paint(lines, None, stroke, width, false);
        }
        // A gateway's own marks are drawn the other way round: the line colour
        // fills them.
        "exclusiveGw" => paint(
            polygon(&[
                point(0.105, 0.0),
                point(0.5, 0.38),
                point(0.895, 0.0),
                point(1.0, 0.11),
                point(0.6172, 0.5),
                point(1.0, 0.89),
                point(0.895, 1.0),
                point(0.5, 0.62),
                point(0.105, 1.0),
                point(0.0, 0.89),
                point(0.3808, 0.5),
                point(0.0, 0.11),
            ]),
            line_color,
            fill,
            width,
            false,
        ),
        "parallelGw" | "parallelMultiple" => {
            let (fill, stroke) = if symbol == "parallelGw" {
                (line_color, fill)
            } else {
                (fill, stroke)
            };
            paint(
                polygon(&[
                    point(0.38, 0.0),
                    point(0.62, 0.0),
                    point(0.62, 0.38),
                    point(1.0, 0.38),
                    point(1.0, 0.62),
                    point(0.62, 0.62),
                    point(0.62, 1.0),
                    point(0.38, 1.0),
                    point(0.38, 0.62),
                    point(0.0, 0.62),
                    point(0.0, 0.38),
                    point(0.38, 0.38),
                ]),
                fill,
                stroke,
                width,
                false,
            );
        }
        "complexGw" => paint(
            polygon(&[
                point(0.0, 0.44),
                point(0.36, 0.44),
                point(0.1, 0.18),
                point(0.18, 0.1),
                point(0.44, 0.36),
                point(0.44, 0.0),
                point(0.56, 0.0),
                point(0.56, 0.36),
                point(0.82, 0.1),
                point(0.9, 0.18),
                point(0.64, 0.44),
                point(1.0, 0.44),
                point(1.0, 0.56),
                point(0.64, 0.56),
                point(0.9, 0.82),
                point(0.82, 0.9),
                point(0.56, 0.64),
                point(0.56, 1.0),
                point(0.44, 1.0),
                point(0.44, 0.64),
                point(0.18, 0.9),
                point(0.1, 0.82),
                point(0.36, 0.56),
                point(0.0, 0.56),
            ]),
            line_color,
            fill,
            width,
            false,
        ),
        _ => {}
    }
}
