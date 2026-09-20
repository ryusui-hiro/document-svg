//! Chart and data visualization (Bar, Line, Pie), Page IR rendering, and CSV/data reverse extraction.

use std::path::Path;

use quick_xml::Reader;
use quick_xml::events::Event;
use serde::{Deserialize, Serialize};

use crate::convert::{ConvertOptions, PageConsumer, read_limited_file};
use crate::error::{Error, Result};
use crate::ir::{
    IDENTITY, LineCap, LineJoin, Node, Page, Paint, SourceMeta, Stroke, TextAnchor, TextRun,
};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChartType {
    Bar,
    Line,
    Pie,
    #[serde(alias = "donut")]
    Doughnut,
    Area,
    Scatter,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ChartSeries {
    pub name: String,
    pub data: Vec<f64>,
    pub color: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ChartSpec {
    pub r#type: ChartType,
    pub title: Option<String>,
    pub labels: Vec<String>,
    pub series: Vec<ChartSeries>,
}

pub(crate) fn convert(
    path: &Path,
    options: &ConvertOptions,
    sink: &mut dyn PageConsumer,
) -> Result<Vec<String>> {
    let bytes = read_limited_file(path, options.max_input_bytes, "chart input")?;
    let spec: ChartSpec = serde_json::from_slice(&bytes)
        .map_err(|e| Error::InvalidInput(format!("chart JSON is invalid: {e}")))?;

    let page = layout_and_render_chart(&spec, options)?;
    sink.consume(page)?;
    Ok(Vec::new())
}

pub fn layout_and_render_chart(spec: &ChartSpec, _options: &ConvertOptions) -> Result<Page> {
    let page_width = 640.0;
    let page_height = 400.0;
    let mut page = Page::new(1, page_width, page_height, "chart");
    if let Ok(json_str) = serde_json::to_string(spec) {
        page.embedded_source = Some(json_str);
    }

    let default_palette = [
        "#3b82f6", // Blue
        "#10b981", // Emerald
        "#f59e0b", // Amber
        "#ef4444", // Red
        "#8b5cf6", // Purple
        "#06b6d4", // Cyan
    ];

    let plot_left = 60.0;
    let plot_right = page_width - 40.0;
    let plot_top = if spec.title.is_some() { 60.0 } else { 40.0 };
    let plot_bottom = page_height - 60.0;
    let plot_width = plot_right - plot_left;
    let plot_height = plot_bottom - plot_top;

    // 1. Title
    if let Some(ref title) = spec.title {
        page.nodes.push(Node::Text {
            id: String::new(),
            x: page_width / 2.0,
            y: 28.0,
            runs: vec![TextRun {
                text: title.clone(),
                font_size: 16.0,
                font_family: "Helvetica, Arial, sans-serif".to_string(),
                bold: true,
                fill: Paint::solid("#0f172a"),
                ..Default::default()
            }],
            anchor: TextAnchor::Middle,
            transform: IDENTITY,
            opacity: 1.0,
            stroke: Stroke::default(),
            clip_id: None,
            meta: SourceMeta::default(),
        });
    }

    // 2. Legend (for multiple series or named series)
    if matches!(
        spec.r#type,
        ChartType::Bar | ChartType::Line | ChartType::Area | ChartType::Scatter
    ) && spec.series.len() > 1
    {
        let mut legend_x = plot_left;
        let legend_y = if spec.title.is_some() { 48.0 } else { 24.0 };

        for (s_idx, s) in spec.series.iter().enumerate() {
            let color = s
                .color
                .as_deref()
                .unwrap_or(default_palette[s_idx % default_palette.len()]);
            // Color marker box
            let marker_d = format!("M {:.1},{:.1} h 10 v 10 h -10 Z", legend_x, legend_y - 8.0);
            page.nodes.push(Node::Path {
                id: String::new(),
                d: marker_d,
                fill_rule: String::new(),
                fill: Paint::solid(color),
                stroke: Stroke::default(),
                transform: IDENTITY,
                clip_id: None,
                meta: SourceMeta::default(),
            });

            // Series name
            page.nodes.push(Node::Text {
                id: String::new(),
                x: legend_x + 14.0,
                y: legend_y,
                runs: vec![TextRun {
                    text: s.name.clone(),
                    font_size: 11.0,
                    font_family: "Helvetica, Arial, sans-serif".to_string(),
                    fill: Paint::solid("#475569"),
                    ..Default::default()
                }],
                anchor: TextAnchor::Start,
                transform: IDENTITY,
                opacity: 1.0,
                stroke: Stroke::default(),
                clip_id: None,
                meta: SourceMeta::default(),
            });

            legend_x += (s.name.len() as f64 * 7.0) + 32.0;
        }
    }

    match spec.r#type {
        ChartType::Bar | ChartType::Line | ChartType::Area | ChartType::Scatter => {
            let mut min_val = 0.0f64;
            let mut max_val = 0.0f64;
            let mut has_data = false;
            for s in &spec.series {
                for &v in &s.data {
                    if v.is_finite() {
                        if !has_data {
                            min_val = v;
                            max_val = v;
                            has_data = true;
                        } else {
                            if v < min_val {
                                min_val = v;
                            }
                            if v > max_val {
                                max_val = v;
                            }
                        }
                    }
                }
            }

            let (min_y, max_y) = if !has_data {
                (0.0, 10.0)
            } else if min_val >= 0.0 {
                (0.0, (max_val * 1.15).ceil().max(5.0))
            } else if max_val <= 0.0 {
                ((min_val * 1.15).floor().min(-5.0), 0.0)
            } else {
                let pad = ((max_val - min_val) * 0.1).max(1.0);
                ((min_val - pad).floor(), (max_val + pad).ceil())
            };

            let range_y = (max_y - min_y).max(1e-6);
            let map_y = |val: f64| -> f64 {
                let frac = (val - min_y) / range_y;
                plot_bottom - frac * plot_height
            };
            let zero_y = map_y(0.0).clamp(plot_top, plot_bottom);

            let steps = 4;
            for i in 0..=steps {
                let val = min_y + (i as f64 / steps as f64) * (max_y - min_y);
                let y = map_y(val);

                // Grid line
                let d = format!("M {:.2},{:.2} L {:.2},{:.2}", plot_left, y, plot_right, y);
                let is_zero_line = val.abs() < 1e-6 || (i == 0 && min_y == 0.0);
                page.nodes.push(Node::Path {
                    id: String::new(),
                    d,
                    fill_rule: String::new(),
                    fill: Paint::None,
                    stroke: Stroke {
                        paint: Paint::solid(if is_zero_line { "#94a3b8" } else { "#f1f5f9" }),
                        width: if is_zero_line { 1.2 } else { 1.0 },
                        line_cap: LineCap::Butt,
                        line_join: LineJoin::Miter,
                        ..Default::default()
                    },
                    transform: IDENTITY,
                    clip_id: None,
                    meta: SourceMeta::default(),
                });

                // Y-axis label
                let text = format!("{val:.0}");
                page.nodes.push(Node::Text {
                    id: String::new(),
                    x: plot_left - 8.0,
                    y: y + 4.0,
                    runs: vec![TextRun {
                        text,
                        font_size: 11.0,
                        font_family: "Helvetica, Arial, sans-serif".to_string(),
                        fill: Paint::solid("#64748b"),
                        ..Default::default()
                    }],
                    anchor: TextAnchor::End,
                    transform: IDENTITY,
                    opacity: 1.0,
                    stroke: Stroke::default(),
                    clip_id: None,
                    meta: SourceMeta::default(),
                });
            }

            let max_data_len = spec.series.iter().map(|s| s.data.len()).max().unwrap_or(0);
            let cat_count = spec.labels.len().max(max_data_len).max(1);
            let cat_width = plot_width / cat_count as f64;

            // X-axis category labels
            for (idx, label) in spec.labels.iter().enumerate() {
                let x = plot_left + (idx as f64 + 0.5) * cat_width;
                page.nodes.push(Node::Text {
                    id: String::new(),
                    x,
                    y: plot_bottom + 20.0,
                    runs: vec![TextRun {
                        text: label.clone(),
                        font_size: 11.0,
                        font_family: "Helvetica, Arial, sans-serif".to_string(),
                        fill: Paint::solid("#475569"),
                        ..Default::default()
                    }],
                    anchor: TextAnchor::Middle,
                    transform: IDENTITY,
                    opacity: 1.0,
                    stroke: Stroke::default(),
                    clip_id: None,
                    meta: SourceMeta::default(),
                });
            }

            if matches!(spec.r#type, ChartType::Bar) {
                let series_count = spec.series.len().max(1);
                let bar_group_width = cat_width * 0.7;
                let single_bar_width = (bar_group_width / series_count as f64).min(40.0);

                for (s_idx, s) in spec.series.iter().enumerate() {
                    let color = s
                        .color
                        .as_deref()
                        .unwrap_or(default_palette[s_idx % default_palette.len()]);
                    for (c_idx, &val) in s.data.iter().enumerate() {
                        if c_idx < cat_count {
                            let val = if val.is_finite() { val } else { 0.0 };
                            let val_y = map_y(val);
                            let (top_y, bottom_y) = if val >= 0.0 {
                                (val_y, zero_y)
                            } else {
                                (zero_y, val_y)
                            };
                            let group_start_x = plot_left
                                + (c_idx as f64 * cat_width)
                                + (cat_width - bar_group_width) / 2.0;
                            let bar_x = group_start_x + s_idx as f64 * single_bar_width;

                            let d = format!(
                                "M {:.2},{:.2} L {:.2},{:.2} L {:.2},{:.2} L {:.2},{:.2} Z",
                                bar_x,
                                bottom_y,
                                bar_x,
                                top_y,
                                bar_x + single_bar_width * 0.9,
                                top_y,
                                bar_x + single_bar_width * 0.9,
                                bottom_y
                            );

                            page.nodes.push(Node::Path {
                                id: String::new(),
                                d,
                                fill_rule: String::new(),
                                fill: Paint::solid(color),
                                stroke: Stroke::default(),
                                transform: IDENTITY,
                                clip_id: None,
                                meta: SourceMeta::default(),
                            });
                        }
                    }
                }
            } else {
                for (s_idx, s) in spec.series.iter().enumerate() {
                    let color = s
                        .color
                        .as_deref()
                        .unwrap_or(default_palette[s_idx % default_palette.len()]);
                    let mut d = String::new();
                    let mut first_x = None;
                    let mut last_x = None;

                    for (c_idx, &val) in s.data.iter().enumerate() {
                        if c_idx < cat_count {
                            let val = if val.is_finite() { val } else { 0.0 };
                            let pt_x = plot_left + (c_idx as f64 + 0.5) * cat_width;
                            let pt_y = map_y(val);
                            if c_idx == 0 {
                                d.push_str(&format!("M {:.2},{:.2} ", pt_x, pt_y));
                                first_x = Some(pt_x);
                            } else {
                                d.push_str(&format!("L {:.2},{:.2} ", pt_x, pt_y));
                            }
                            last_x = Some(pt_x);
                        }
                    }

                    if matches!(spec.r#type, ChartType::Area)
                        && let (Some(fx), Some(lx)) = (first_x, last_x)
                    {
                        let mut area_d = d.clone();
                        area_d.push_str(&format!(
                            "L {:.2},{:.2} L {:.2},{:.2} Z",
                            lx, zero_y, fx, zero_y
                        ));
                        page.nodes.push(Node::Path {
                            id: String::new(),
                            d: area_d,
                            fill_rule: String::new(),
                            fill: Paint::Solid {
                                color: color.to_string(),
                                opacity: 0.25,
                            },
                            stroke: Stroke::default(),
                            transform: IDENTITY,
                            clip_id: None,
                            meta: SourceMeta::default(),
                        });
                    }

                    if !matches!(spec.r#type, ChartType::Scatter) {
                        page.nodes.push(Node::Path {
                            id: String::new(),
                            d,
                            fill_rule: String::new(),
                            fill: Paint::None,
                            stroke: Stroke {
                                paint: Paint::solid(color),
                                width: 2.5,
                                line_cap: LineCap::Round,
                                line_join: LineJoin::Round,
                                ..Default::default()
                            },
                            transform: IDENTITY,
                            clip_id: None,
                            meta: SourceMeta::default(),
                        });
                    }

                    // Point markers
                    for (c_idx, &val) in s.data.iter().enumerate() {
                        if c_idx < cat_count {
                            let val = if val.is_finite() { val } else { 0.0 };
                            let pt_x = plot_left + (c_idx as f64 + 0.5) * cat_width;
                            let pt_y = map_y(val);
                            let r = 4.0;
                            let c = 0.5522847498 * r;
                            let circle_d = format!(
                                "M {:.2},{:.2} C {:.2},{:.2} {:.2},{:.2} {:.2},{:.2} C {:.2},{:.2} {:.2},{:.2} {:.2},{:.2} C {:.2},{:.2} {:.2},{:.2} {:.2},{:.2} C {:.2},{:.2} {:.2},{:.2} {:.2},{:.2} Z",
                                pt_x,
                                pt_y - r,
                                pt_x + c,
                                pt_y - r,
                                pt_x + r,
                                pt_y - c,
                                pt_x + r,
                                pt_y,
                                pt_x + r,
                                pt_y + c,
                                pt_x + c,
                                pt_y + r,
                                pt_x,
                                pt_y + r,
                                pt_x - c,
                                pt_y + r,
                                pt_x - r,
                                pt_y + c,
                                pt_x - r,
                                pt_y,
                                pt_x - r,
                                pt_y - c,
                                pt_x - c,
                                pt_y - r,
                                pt_x,
                                pt_y - r
                            );
                            page.nodes.push(Node::Path {
                                id: String::new(),
                                d: circle_d,
                                fill_rule: String::new(),
                                fill: Paint::solid("#ffffff"),
                                stroke: Stroke {
                                    paint: Paint::solid(color),
                                    width: 2.0,
                                    ..Default::default()
                                },
                                transform: IDENTITY,
                                clip_id: None,
                                meta: SourceMeta::default(),
                            });
                        }
                    }
                }
            }
        }
        ChartType::Pie | ChartType::Doughnut => {
            let cx = page_width * 0.36;
            let cy = plot_top + plot_height / 2.0;
            let radius = (plot_height / 2.0).min(plot_width * 0.28) - 10.0;

            let mut total_val = 0.0;
            if let Some(first_series) = spec.series.first() {
                for &v in &first_series.data {
                    if v.is_finite() && v > 0.0 {
                        total_val += v;
                    }
                }

                let mut current_angle = -std::f64::consts::FRAC_PI_2;
                let legend_start_x = page_width * 0.62;
                let mut legend_y = cy - (first_series.data.len() as f64 * 14.0);

                for (idx, &v) in first_series.data.iter().enumerate() {
                    let v_clean = if v.is_finite() { v.max(0.0) } else { 0.0 };
                    let slice_angle = if total_val > 0.0 {
                        (v_clean / total_val) * std::f64::consts::TAU
                    } else {
                        0.0
                    };
                    let percent = if total_val > 0.0 {
                        (v_clean / total_val) * 100.0
                    } else {
                        0.0
                    };
                    let end_angle = current_angle + slice_angle;
                    let color = default_palette[idx % default_palette.len()];

                    let is_donut = matches!(spec.r#type, ChartType::Doughnut);
                    let inner_radius = if is_donut { radius * 0.55 } else { 0.0 };

                    let mut d = String::new();
                    let steps = 16;
                    for step in 0..=steps {
                        let a = current_angle + (step as f64 / steps as f64) * slice_angle;
                        let px = cx + radius * a.cos();
                        let py = cy + radius * a.sin();
                        if step == 0 {
                            d.push_str(&format!("M {:.2},{:.2} ", px, py));
                        } else {
                            d.push_str(&format!("L {:.2},{:.2} ", px, py));
                        }
                    }
                    if is_donut {
                        for step in (0..=steps).rev() {
                            let a = current_angle + (step as f64 / steps as f64) * slice_angle;
                            let px = cx + inner_radius * a.cos();
                            let py = cy + inner_radius * a.sin();
                            d.push_str(&format!("L {:.2},{:.2} ", px, py));
                        }
                    } else {
                        d.push_str(&format!("L {:.2},{:.2} ", cx, cy));
                    }
                    d.push('Z');

                    page.nodes.push(Node::Path {
                        id: String::new(),
                        d,
                        fill_rule: String::new(),
                        fill: Paint::solid(color),
                        stroke: Stroke {
                            paint: Paint::solid("#ffffff"),
                            width: 2.0,
                            ..Default::default()
                        },
                        transform: IDENTITY,
                        clip_id: None,
                        meta: SourceMeta::default(),
                    });

                    // Legend item for slice
                    let label = spec.labels.get(idx).map(|s| s.as_str()).unwrap_or("Item");
                    let legend_text = format!("{label}: {v:.0} ({percent:.1}%)");

                    let marker_d = format!(
                        "M {:.1},{:.1} h 10 v 10 h -10 Z",
                        legend_start_x,
                        legend_y - 8.0
                    );
                    page.nodes.push(Node::Path {
                        id: String::new(),
                        d: marker_d,
                        fill_rule: String::new(),
                        fill: Paint::solid(color),
                        stroke: Stroke::default(),
                        transform: IDENTITY,
                        clip_id: None,
                        meta: SourceMeta::default(),
                    });

                    page.nodes.push(Node::Text {
                        id: String::new(),
                        x: legend_start_x + 16.0,
                        y: legend_y,
                        runs: vec![TextRun {
                            text: legend_text,
                            font_size: 12.0,
                            font_family: "Helvetica, Arial, sans-serif".to_string(),
                            fill: Paint::solid("#334155"),
                            ..Default::default()
                        }],
                        anchor: TextAnchor::Start,
                        transform: IDENTITY,
                        opacity: 1.0,
                        stroke: Stroke::default(),
                        clip_id: None,
                        meta: SourceMeta::default(),
                    });

                    legend_y += 24.0;
                    current_angle = end_angle;
                }
            }
        }
    }

    Ok(page)
}

/// Reverse extraction: parses an SVG chart and recovers tabular data as CSV.
pub fn extract_csv_from_chart_svg(svg_bytes: &[u8]) -> Result<String> {
    let svg_text = std::str::from_utf8(svg_bytes)
        .map_err(|e| Error::InvalidInput(format!("SVG is not valid UTF-8: {e}")))?;

    // 1. Check embedded source (content attribute)
    if let Some(decoded) = crate::cad::svg_reader::extract_embedded_source(svg_bytes)
        && let Ok(spec) = serde_json::from_str::<ChartSpec>(&decoded)
    {
        let mut csv = String::new();
        // Headers: Label, Series1, Series2, ...
        csv.push_str("Label");
        for s in &spec.series {
            csv.push(',');
            let series_name = if s.name.is_empty() { "Value" } else { &s.name };
            csv.push_str(&format!("\"{}\"", series_name.replace('"', "\"\"")));
        }
        csv.push('\n');

        let max_data_len = spec.series.iter().map(|s| s.data.len()).max().unwrap_or(0);
        let row_count = spec.labels.len().max(max_data_len);

        // Rows for each category label / data index
        for idx in 0..row_count {
            let default_label = format!("Item {}", idx + 1);
            let label = spec
                .labels
                .get(idx)
                .map(|s| s.as_str())
                .unwrap_or(&default_label);
            csv.push_str(&format!("\"{}\"", label.replace('"', "\"\"")));
            for s in &spec.series {
                csv.push(',');
                if let Some(&val) = s.data.get(idx) {
                    csv.push_str(&format!("{val}"));
                }
            }
            csv.push('\n');
        }
        return Ok(csv);
    }

    // 2. Geometric fallback: extract text nodes
    let mut reader = Reader::from_str(svg_text);
    reader.config_mut().trim_text(true);

    let mut texts = Vec::new();
    let mut in_text = false;
    let mut current_text = String::new();

    while let Ok(event) = reader.read_event() {
        match event {
            Event::Start(e) if e.name().as_ref() == b"text" => {
                in_text = true;
                current_text.clear();
            }
            Event::Text(e) if in_text => {
                let bytes = e.as_ref();
                if let Ok(s) = std::str::from_utf8(bytes) {
                    current_text.push_str(s);
                }
            }
            Event::End(e) if e.name().as_ref() == b"text" => {
                in_text = false;
                let trimmed = current_text.trim();
                if !trimmed.is_empty() {
                    texts.push(trimmed.to_string());
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }

    let mut csv = String::from("Label,Value\n");
    for t in texts {
        // If text is in the format "Label: Value (Percent%)" (e.g. from pie chart legend), split into Label and Value
        if let Some((label_part, rest)) = t.split_once(':') {
            let label = label_part.trim().replace('"', "\"\"");
            let val = rest
                .split('(')
                .next()
                .unwrap_or(rest)
                .trim()
                .replace('"', "\"\"");
            csv.push_str(&format!("\"{label}\",\"{val}\"\n"));
        } else {
            csv.push_str(&format!("\"{}\",\n", t.replace('"', "\"\"")));
        }
    }
    Ok(csv)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_bar_chart_with_positive_and_negative_values() {
        let spec = ChartSpec {
            r#type: ChartType::Bar,
            title: Some("Revenue Growth".to_string()),
            labels: vec!["Q1".into(), "Q2".into(), "Q3".into(), "Q4".into()],
            series: vec![ChartSeries {
                name: "Net Profit".into(),
                data: vec![100.0, -50.0, 150.0, -20.0],
                color: None,
            }],
        };

        let page =
            layout_and_render_chart(&spec, &ConvertOptions::default()).expect("render chart");
        assert_eq!(page.source_format, "chart");
        assert!(page.nodes.len() >= 4); // title + axes + bars
    }

    #[test]
    fn renders_area_and_scatter_with_multi_series_legend() {
        let spec_area = ChartSpec {
            r#type: ChartType::Area,
            title: Some("Multi Area".to_string()),
            labels: vec!["Jan".into(), "Feb".into(), "Mar".into()],
            series: vec![
                ChartSeries {
                    name: "Series A".into(),
                    data: vec![10.0, 20.0, 30.0],
                    color: Some("#ff0000".into()),
                },
                ChartSeries {
                    name: "Series B".into(),
                    data: vec![5.0, 15.0, 25.0],
                    color: Some("#00ff00".into()),
                },
            ],
        };

        let page_area =
            layout_and_render_chart(&spec_area, &ConvertOptions::default()).expect("render area");
        // Verify legend text is rendered
        let has_legend_a = page_area.nodes.iter().any(|n| match n {
            Node::Text { runs, .. } => runs.iter().any(|r| r.text == "Series A"),
            _ => false,
        });
        assert!(
            has_legend_a,
            "Area chart should render series legend for multiple series"
        );

        let spec_scatter = ChartSpec {
            r#type: ChartType::Scatter,
            title: Some("Scatter Plot".to_string()),
            labels: vec!["P1".into(), "P2".into()],
            series: vec![
                ChartSeries {
                    name: "Points A".into(),
                    data: vec![1.0, 2.0],
                    color: None,
                },
                ChartSeries {
                    name: "Points B".into(),
                    data: vec![3.0, 4.0],
                    color: None,
                },
            ],
        };

        let page_scatter = layout_and_render_chart(&spec_scatter, &ConvertOptions::default())
            .expect("render scatter");
        let has_legend_scatter = page_scatter.nodes.iter().any(|n| match n {
            Node::Text { runs, .. } => runs.iter().any(|r| r.text == "Points B"),
            _ => false,
        });
        assert!(
            has_legend_scatter,
            "Scatter chart should render series legend for multiple series"
        );
    }

    #[test]
    fn renders_doughnut_and_extracts_csv() {
        let spec = ChartSpec {
            r#type: ChartType::Doughnut,
            title: Some("Market Share".to_string()),
            labels: vec!["Product A".into(), "Product B".into()],
            series: vec![ChartSeries {
                name: "Share".into(),
                data: vec![60.0, 40.0],
                color: None,
            }],
        };

        let page =
            layout_and_render_chart(&spec, &ConvertOptions::default()).expect("render doughnut");
        let embedded = page.embedded_source.as_ref().expect("embedded source");
        let svg = format!(
            r#"<svg xmlns="http://www.w3.org/2000/svg" content="{}"><text>Product A</text></svg>"#,
            embedded.replace('"', "&quot;")
        );
        let csv = extract_csv_from_chart_svg(svg.as_bytes()).expect("extract csv");
        assert!(csv.contains("Product A"));
        assert!(csv.contains("Product B"));
        assert!(csv.contains("60"));
        assert!(csv.contains("40"));
    }
}
