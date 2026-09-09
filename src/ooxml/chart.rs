use quick_xml::Reader;
use quick_xml::events::Event;

use crate::error::{Error, Result};
use crate::ir::{IDENTITY, Node, Paint, SourceMeta, Stroke, TextAnchor, TextRun};
use crate::ooxml::{attribute, decode_xml_reference, local_name};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum ChartKind {
    #[default]
    Bar,
    Line,
    Pie,
    Area,
    Scatter,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct ChartSeries {
    pub name: String,
    pub categories: Vec<String>,
    pub values: Vec<f64>,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct ChartData {
    pub kind: ChartKind,
    pub title: String,
    pub series: Vec<ChartSeries>,
    /// `<c:barDir val="bar"/>`: categories run down the left and the bars grow
    /// to the right. The default, `col`, is the familiar column chart.
    pub horizontal_bars: bool,
    pub grouping: BarGrouping,
}

/// How a bar chart combines the series within one category.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum BarGrouping {
    /// Series sit side by side; each is measured from the axis.
    #[default]
    Clustered,
    /// Series sit on top of one another and the category total is what shows.
    Stacked,
    /// Stacked, with every category normalised to the same full length.
    PercentStacked,
}

pub(crate) fn parse_chart(xml: &[u8], max_events: usize) -> Result<ChartData> {
    let mut reader = Reader::from_reader(xml);
    reader.config_mut().trim_text(true);
    let mut buffer = Vec::new();
    let mut stack = Vec::<String>::new();
    let mut chart = ChartData::default();
    let mut series = None::<ChartSeries>;
    let mut capture = None::<Capture>;
    let mut text = String::new();
    let mut events = 0usize;
    loop {
        events += 1;
        if events > max_events {
            return Err(Error::LimitExceeded(format!(
                "chart XML exceeds {max_events} events"
            )));
        }
        match reader.read_event_into(&mut buffer)? {
            Event::Start(start) => {
                let name = String::from_utf8_lossy(local_name(start.name().as_ref())).into_owned();
                match name.as_str() {
                    "barChart" | "bar3DChart" => chart.kind = ChartKind::Bar,
                    "lineChart" | "line3DChart" => chart.kind = ChartKind::Line,
                    "pieChart" | "pie3DChart" | "doughnutChart" => chart.kind = ChartKind::Pie,
                    "areaChart" | "area3DChart" => chart.kind = ChartKind::Area,
                    "scatterChart" | "bubbleChart" => chart.kind = ChartKind::Scatter,
                    "ser" => series = Some(ChartSeries::default()),
                    "v" | "t" => {
                        capture = capture_target(&stack, series.is_some());
                        text.clear();
                    }
                    _ => {}
                }
                stack.push(name);
            }
            // `<c:barDir val="bar"/>` is self-closing, so it never arrives as a
            // start event.
            Event::Empty(start) if local_name(start.name().as_ref()) == b"barDir" => {
                chart.horizontal_bars = attribute(&start, b"val").as_deref() == Some("bar");
            }
            Event::Empty(start) if local_name(start.name().as_ref()) == b"grouping" => {
                chart.grouping = match attribute(&start, b"val").as_deref() {
                    Some("stacked") => BarGrouping::Stacked,
                    Some("percentStacked") => BarGrouping::PercentStacked,
                    _ => BarGrouping::Clustered,
                };
            }
            Event::Text(value) if capture.is_some() => {
                text.push_str(&value.decode().map_err(|error| {
                    Error::InvalidInput(format!("invalid chart text: {error}"))
                })?);
            }
            Event::GeneralRef(reference) if capture.is_some() => {
                text.push_str(&decode_xml_reference(&reference, "chart text")?);
            }
            Event::End(end) => {
                let qualified_name = end.name();
                let name = local_name(qualified_name.as_ref());
                if matches!(name, b"v" | b"t") {
                    if let Some(target) = capture.take() {
                        apply_capture(&mut chart, series.as_mut(), target, &text);
                    }
                } else if name == b"ser"
                    && let Some(series) = series.take()
                {
                    chart.series.push(series);
                }
                stack.pop();
            }
            Event::Eof => break,
            _ => {}
        }
        buffer.clear();
    }
    Ok(chart)
}

#[derive(Clone, Copy)]
enum Capture {
    Title,
    SeriesName,
    Category,
    Value,
}

fn capture_target(stack: &[String], in_series: bool) -> Option<Capture> {
    if !in_series && stack.iter().any(|item| item == "title") {
        return Some(Capture::Title);
    }
    if !in_series {
        return None;
    }
    if stack.iter().any(|item| item == "tx") {
        Some(Capture::SeriesName)
    } else if stack
        .iter()
        .any(|item| matches!(item.as_str(), "cat" | "xVal"))
    {
        Some(Capture::Category)
    } else if stack
        .iter()
        .any(|item| matches!(item.as_str(), "val" | "yVal" | "bubbleSize"))
    {
        Some(Capture::Value)
    } else {
        None
    }
}

fn apply_capture(
    chart: &mut ChartData,
    series: Option<&mut ChartSeries>,
    target: Capture,
    text: &str,
) {
    match target {
        Capture::Title => {
            if !text.is_empty() {
                if !chart.title.is_empty() {
                    chart.title.push(' ');
                }
                chart.title.push_str(text);
            }
        }
        Capture::SeriesName => {
            if let Some(series) = series
                && series.name.is_empty()
            {
                series.name = text.into();
            }
        }
        Capture::Category => {
            if let Some(series) = series {
                series.categories.push(text.into());
            }
        }
        Capture::Value => {
            if let Some(series) = series
                && let Ok(value) = text.parse()
            {
                series.values.push(value);
            }
        }
    }
}

pub(crate) fn render_chart(
    chart: &ChartData,
    x: f64,
    y: f64,
    width: f64,
    height: f64,
    id_prefix: &str,
) -> Vec<Node> {
    let mut nodes = Vec::new();
    let title_height = if chart.title.is_empty() { 8.0 } else { 24.0 };
    if !chart.title.is_empty() {
        nodes.push(text_node(
            format!("{id_prefix}-title"),
            x + width / 2.0,
            y + 16.0,
            chart.title.clone(),
            12.0,
            TextAnchor::Middle,
            "chart-title",
        ));
    }
    let plot_x = x + 36.0;
    let plot_y = y + title_height;
    let plot_width = (width - 48.0).max(12.0);
    let plot_height = (height - title_height - 28.0).max(12.0);
    if chart.kind == ChartKind::Pie {
        render_pie(
            chart,
            plot_x,
            plot_y,
            plot_width,
            plot_height,
            id_prefix,
            &mut nodes,
        );
        return nodes;
    }
    nodes.push(path_node(
        format!("{id_prefix}-axes"),
        format!(
            "M {} {} V {} H {}",
            number(plot_x),
            number(plot_y),
            number(plot_y + plot_height),
            number(plot_x + plot_width)
        ),
        Paint::None,
        Stroke {
            paint: Paint::solid("#666666"),
            width: 0.75,
            miter_limit: 10.0,
            ..Stroke::default()
        },
        "chart-axis",
    ));
    match chart.kind {
        ChartKind::Line | ChartKind::Scatter | ChartKind::Area => render_lines(
            chart,
            plot_x,
            plot_y,
            plot_width,
            plot_height,
            id_prefix,
            &mut nodes,
        ),
        _ => render_bars(
            chart,
            plot_x,
            plot_y,
            plot_width,
            plot_height,
            id_prefix,
            &mut nodes,
        ),
    }
    nodes
}

const CHART_COLORS: [&str; 8] = [
    "#4472C4", "#ED7D31", "#A5A5A5", "#FFC000", "#5B9BD5", "#70AD47", "#264478", "#9E480E",
];

#[allow(clippy::too_many_arguments)]
fn render_bars(
    chart: &ChartData,
    x: f64,
    y: f64,
    width: f64,
    height: f64,
    id_prefix: &str,
    nodes: &mut Vec<Node>,
) {
    let categories = chart
        .series
        .iter()
        .map(|series| series.values.len())
        .max()
        .unwrap_or(0);
    if categories == 0 || chart.series.is_empty() {
        return;
    }
    let stacked = matches!(
        chart.grouping,
        BarGrouping::Stacked | BarGrouping::PercentStacked
    );
    // A stacked chart is measured against the category total, not the largest
    // single value: rendering 64 and 82 side by side instead of stacked to 146
    // states something the source did not.
    let category_total = |index: usize| {
        chart
            .series
            .iter()
            .filter_map(|series| series.values.get(index))
            .map(|value| value.max(0.0))
            .sum::<f64>()
    };
    let maximum = if stacked {
        (0..categories).map(category_total).fold(0.0_f64, f64::max)
    } else {
        chart
            .series
            .iter()
            .flat_map(|series| series.values.iter())
            .copied()
            .fold(0.0_f64, f64::max)
    }
    .max(1e-9);
    // `barDir="bar"` lays categories down the left edge and grows the bars to
    // the right; the default `col` is the usual column chart.
    let span = if chart.horizontal_bars { height } else { width };
    let plot_length = if chart.horizontal_bars { width } else { height };
    let category_span = span / categories as f64;
    let bar_span = if stacked {
        category_span * 0.75
    } else {
        category_span * 0.75 / chart.series.len() as f64
    };
    let mut stacked_base = vec![0.0_f64; categories];
    for (series_index, series) in chart.series.iter().enumerate() {
        for (value_index, value) in series.values.iter().enumerate() {
            let scale = if chart.grouping == BarGrouping::PercentStacked {
                let total = category_total(value_index);
                if total <= 0.0 { 0.0 } else { plot_length / total }
            } else {
                plot_length / maximum
            };
            let length = value.max(0.0) * scale;
            let base = stacked_base.get(value_index).copied().unwrap_or(0.0);
            let offset = if stacked {
                value_index as f64 * category_span + category_span * 0.125
            } else {
                value_index as f64 * category_span
                    + category_span * 0.125
                    + series_index as f64 * bar_span
            };
            let bounds = if chart.horizontal_bars {
                rectangle(x + base, y + offset, length, bar_span * 0.9)
            } else {
                rectangle(
                    x + offset,
                    y + height - base - length,
                    bar_span * 0.9,
                    length,
                )
            };
            nodes.push(path_node(
                format!("{id_prefix}-bar-{series_index}-{value_index}"),
                bounds,
                Paint::solid(CHART_COLORS[series_index % CHART_COLORS.len()]),
                Stroke::default(),
                "chart-bar",
            ));
            if stacked && let Some(slot) = stacked_base.get_mut(value_index) {
                *slot += length;
            }
        }
    }
    render_category_labels(chart, x, y, width, height, categories, id_prefix, nodes);
}

/// Name each category next to its slot on the category axis.
///
/// The names are read out of the chart part already; without them on the page a
/// reader sees bars with nothing to identify them.
#[allow(clippy::too_many_arguments)]
fn render_category_labels(
    chart: &ChartData,
    x: f64,
    y: f64,
    width: f64,
    height: f64,
    categories: usize,
    id_prefix: &str,
    nodes: &mut Vec<Node>,
) {
    let Some(names) = chart
        .series
        .iter()
        .map(|series| &series.categories)
        .find(|names| !names.is_empty())
    else {
        return;
    };
    let span = if chart.horizontal_bars { height } else { width };
    let category_span = span / categories as f64;
    for (index, name) in names.iter().take(categories).enumerate() {
        if name.trim().is_empty() {
            continue;
        }
        let middle = index as f64 * category_span + category_span / 2.0;
        let (label_x, label_y, anchor) = if chart.horizontal_bars {
            (x - 4.0, y + middle + 3.0, TextAnchor::End)
        } else {
            (x + middle, y + height + 11.0, TextAnchor::Middle)
        };
        nodes.push(text_node(
            format!("{id_prefix}-category-{index}"),
            label_x,
            label_y,
            name.clone(),
            8.0,
            anchor,
            "chart-category-label",
        ));
    }
}

#[allow(clippy::too_many_arguments)]
fn render_lines(
    chart: &ChartData,
    x: f64,
    y: f64,
    width: f64,
    height: f64,
    id_prefix: &str,
    nodes: &mut Vec<Node>,
) {
    let maximum = chart
        .series
        .iter()
        .flat_map(|series| series.values.iter())
        .copied()
        .fold(0.0_f64, f64::max)
        .max(1e-9);
    for (series_index, series) in chart.series.iter().enumerate() {
        if series.values.is_empty() {
            continue;
        }
        let denominator = series.values.len().saturating_sub(1).max(1) as f64;
        let mut path = String::new();
        for (index, value) in series.values.iter().enumerate() {
            let point_x = x + index as f64 / denominator * width;
            let point_y = y + height - value.max(0.0) / maximum * height;
            path.push_str(&format!(
                "{} {} {} ",
                if index == 0 { "M" } else { "L" },
                number(point_x),
                number(point_y)
            ));
        }
        nodes.push(path_node(
            format!("{id_prefix}-line-{series_index}"),
            path,
            Paint::None,
            Stroke {
                paint: Paint::solid(CHART_COLORS[series_index % CHART_COLORS.len()]),
                width: 1.5,
                miter_limit: 10.0,
                ..Stroke::default()
            },
            "chart-line",
        ));
    }
}

#[allow(clippy::too_many_arguments)]
fn render_pie(
    chart: &ChartData,
    x: f64,
    y: f64,
    width: f64,
    height: f64,
    id_prefix: &str,
    nodes: &mut Vec<Node>,
) {
    let Some(series) = chart.series.first() else {
        return;
    };
    let total = series
        .values
        .iter()
        .copied()
        .map(|value| value.max(0.0))
        .sum::<f64>();
    if total <= 0.0 {
        return;
    }
    let center_x = x + width / 2.0;
    let center_y = y + height / 2.0;
    let radius = width.min(height) * 0.45;
    let mut start_angle = -std::f64::consts::FRAC_PI_2;
    for (index, value) in series.values.iter().enumerate() {
        let sweep = value.max(0.0) / total * std::f64::consts::TAU;
        let end_angle = start_angle + sweep;
        let start_x = center_x + radius * start_angle.cos();
        let start_y = center_y + radius * start_angle.sin();
        let end_x = center_x + radius * end_angle.cos();
        let end_y = center_y + radius * end_angle.sin();
        let large_arc = usize::from(sweep > std::f64::consts::PI);
        let path = format!(
            "M {} {} L {} {} A {} {} 0 {} 1 {} {} Z",
            number(center_x),
            number(center_y),
            number(start_x),
            number(start_y),
            number(radius),
            number(radius),
            large_arc,
            number(end_x),
            number(end_y)
        );
        nodes.push(path_node(
            format!("{id_prefix}-slice-{index}"),
            path,
            Paint::solid(CHART_COLORS[index % CHART_COLORS.len()]),
            Stroke {
                paint: Paint::solid("#FFFFFF"),
                width: 0.5,
                miter_limit: 10.0,
                ..Stroke::default()
            },
            "chart-slice",
        ));
        start_angle = end_angle;
    }
}

fn path_node(id: String, d: String, fill: Paint, stroke: Stroke, kind: &str) -> Node {
    Node::Path {
        id,
        d,
        fill_rule: "nonzero".into(),
        fill,
        stroke,
        transform: IDENTITY,
        clip_id: None,
        meta: SourceMeta {
            kind: kind.into(),
            ..SourceMeta::default()
        },
    }
}

fn text_node(
    id: String,
    x: f64,
    y: f64,
    text: String,
    size: f64,
    anchor: TextAnchor,
    kind: &str,
) -> Node {
    Node::Text {
        id,
        x,
        y,
        runs: vec![TextRun {
            text,
            font_size: size,
            ..TextRun::default()
        }],
        anchor,
        transform: IDENTITY,
        opacity: 1.0,
        stroke: Stroke::default(),
        clip_id: None,
        meta: SourceMeta {
            kind: kind.into(),
            ..SourceMeta::default()
        },
    }
}

fn rectangle(x: f64, y: f64, width: f64, height: f64) -> String {
    format!(
        "M {} {} H {} V {} H {} Z",
        number(x),
        number(y),
        number(x + width),
        number(y + height),
        number(x)
    )
}

fn number(value: f64) -> String {
    format!("{value:.4}")
        .trim_end_matches('0')
        .trim_end_matches('.')
        .to_owned()
}
