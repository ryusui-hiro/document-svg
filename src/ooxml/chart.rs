use quick_xml::Reader;
use quick_xml::events::Event;

use crate::error::{Error, Result};
use crate::ir::{IDENTITY, Node, Paint, SourceMeta, Stroke, TextAnchor, TextRun};
use crate::ooxml::{attribute, decode_xml_reference, local_name};

const MAX_CHART_CACHE_SLOTS: usize = 1_000_000;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum ChartKind {
    #[default]
    Bar,
    Line,
    Pie,
    Doughnut,
    Area,
    Scatter,
    Bubble,
    Unsupported,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct ChartSeries {
    pub name: String,
    pub categories: Vec<Option<String>>,
    pub x_values: Vec<Option<f64>>,
    pub values: Vec<Option<f64>>,
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
    pub scatter_style: ScatterStyle,
    pub legend_visible: bool,
    pub doughnut_hole_percent: Option<u8>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum ScatterStyle {
    Marker,
    Line,
    #[default]
    LineMarker,
    Smooth,
    SmoothMarker,
}

/// How a category chart combines its series at each category.
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
    let mut chart_groups = 0usize;
    let mut series = None::<ChartSeries>;
    let mut capture = None::<(Capture, Option<usize>)>;
    let mut point_index = None::<usize>;
    let mut next_cache_index = 0usize;
    let mut cache_slots = 0usize;
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
                    other if chart_kind(other).is_some() => register_chart_group(
                        &mut chart,
                        &mut chart_groups,
                        chart_kind(other).unwrap_or(ChartKind::Unsupported),
                    ),
                    other if other.ends_with("Chart") => {
                        register_chart_group(&mut chart, &mut chart_groups, ChartKind::Unsupported)
                    }
                    "numCache" | "strCache" | "multiLvlStrCache" => next_cache_index = 0,
                    "pt" => {
                        let index = attribute(&start, b"idx")
                            .and_then(|value| value.parse::<usize>().ok())
                            .unwrap_or(next_cache_index);
                        if index >= max_events {
                            return Err(Error::LimitExceeded(format!(
                                "chart point index {index} exceeds chart event limit {max_events}"
                            )));
                        }
                        point_index = Some(index);
                        next_cache_index = index.saturating_add(1);
                    }
                    "ptCount" => {
                        let count = attribute(&start, b"val")
                            .and_then(|value| value.parse::<usize>().ok())
                            .unwrap_or(0);
                        if count > max_events {
                            return Err(Error::LimitExceeded(format!(
                                "chart point count {count} exceeds chart event limit {max_events}"
                            )));
                        }
                        if let Some(target) = capture_target(&stack, series.is_some()) {
                            ensure_capture_length(
                                series.as_mut(),
                                target,
                                count,
                                &mut cache_slots,
                            )?;
                        }
                    }
                    "ser" => series = Some(ChartSeries::default()),
                    "legend" => chart.legend_visible = true,
                    "delete" if stack.iter().any(|item| item == "legend") => {
                        if attribute(&start, b"val").as_deref().is_some_and(xml_true) {
                            chart.legend_visible = false;
                        }
                    }
                    "v" | "t" => {
                        capture = capture_target(&stack, series.is_some())
                            .map(|target| (target, point_index));
                        text.clear();
                    }
                    _ => {}
                }
                stack.push(name);
            }
            Event::Empty(start) if local_name(start.name().as_ref()).ends_with(b"Chart") => {
                let name = String::from_utf8_lossy(local_name(start.name().as_ref())).into_owned();
                let kind = chart_kind(&name).unwrap_or(ChartKind::Unsupported);
                register_chart_group(&mut chart, &mut chart_groups, kind);
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
            Event::Empty(start) if local_name(start.name().as_ref()) == b"scatterStyle" => {
                chart.scatter_style = match attribute(&start, b"val").as_deref() {
                    Some("marker") => ScatterStyle::Marker,
                    Some("line") => ScatterStyle::Line,
                    Some("smooth") => ScatterStyle::Smooth,
                    Some("smoothMarker") => ScatterStyle::SmoothMarker,
                    _ => ScatterStyle::LineMarker,
                };
            }
            Event::Empty(start) if local_name(start.name().as_ref()) == b"holeSize" => {
                chart.doughnut_hole_percent = attribute(&start, b"val")
                    .and_then(|value| value.parse::<u8>().ok())
                    .filter(|value| (10..=90).contains(value));
            }
            Event::Empty(start) if local_name(start.name().as_ref()) == b"legend" => {
                chart.legend_visible = true;
            }
            Event::Empty(start)
                if local_name(start.name().as_ref()) == b"delete"
                    && stack.iter().any(|item| item == "legend") =>
            {
                if attribute(&start, b"val").as_deref().is_some_and(xml_true) {
                    chart.legend_visible = false;
                }
            }
            Event::Empty(start) if local_name(start.name().as_ref()) == b"pt" => {
                let index = attribute(&start, b"idx")
                    .and_then(|value| value.parse::<usize>().ok())
                    .unwrap_or(next_cache_index);
                if index >= max_events {
                    return Err(Error::LimitExceeded(format!(
                        "chart point index {index} exceeds chart event limit {max_events}"
                    )));
                }
                next_cache_index = index.saturating_add(1);
            }
            Event::Empty(start) if local_name(start.name().as_ref()) == b"ptCount" => {
                let count = attribute(&start, b"val")
                    .and_then(|value| value.parse::<usize>().ok())
                    .unwrap_or(0);
                if count > max_events {
                    return Err(Error::LimitExceeded(format!(
                        "chart point count {count} exceeds chart event limit {max_events}"
                    )));
                }
                if let Some(target) = capture_target(&stack, series.is_some()) {
                    ensure_capture_length(series.as_mut(), target, count, &mut cache_slots)?;
                }
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
                    if let Some((target, index)) = capture.take() {
                        apply_capture(
                            &mut chart,
                            series.as_mut(),
                            target,
                            index,
                            &text,
                            &mut cache_slots,
                        )?;
                    }
                } else if name == b"pt" {
                    point_index = None;
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

fn chart_kind(name: &str) -> Option<ChartKind> {
    match name {
        "barChart" | "bar3DChart" => Some(ChartKind::Bar),
        "lineChart" | "line3DChart" => Some(ChartKind::Line),
        "pieChart" | "pie3DChart" => Some(ChartKind::Pie),
        "doughnutChart" => Some(ChartKind::Doughnut),
        "areaChart" | "area3DChart" => Some(ChartKind::Area),
        "scatterChart" => Some(ChartKind::Scatter),
        "bubbleChart" => Some(ChartKind::Bubble),
        _ => None,
    }
}

fn xml_true(value: &str) -> bool {
    matches!(value, "1" | "true")
}

fn register_chart_group(chart: &mut ChartData, group_count: &mut usize, kind: ChartKind) {
    *group_count = group_count.saturating_add(1);
    chart.kind = if *group_count == 1 {
        kind
    } else {
        ChartKind::Unsupported
    };
}

#[derive(Clone, Copy)]
enum Capture {
    Title,
    SeriesName,
    Category,
    XValue,
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
    } else if stack.iter().any(|item| item == "xVal") {
        Some(Capture::XValue)
    } else if stack.iter().any(|item| item == "cat") {
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
    index: Option<usize>,
    text: &str,
    cache_slots: &mut usize,
) -> Result<()> {
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
                set_indexed(
                    &mut series.categories,
                    index,
                    Some(text.into()),
                    cache_slots,
                )?;
            }
        }
        Capture::XValue => {
            if let Some(series) = series {
                let value = text.parse::<f64>().ok().filter(|value| value.is_finite());
                set_indexed(&mut series.x_values, index, value, cache_slots)?;
            }
        }
        Capture::Value => {
            if let Some(series) = series {
                let value = text.parse::<f64>().ok().filter(|value| value.is_finite());
                set_indexed(&mut series.values, index, value, cache_slots)?;
            }
        }
    }
    Ok(())
}

fn set_indexed<T>(
    values: &mut Vec<Option<T>>,
    index: Option<usize>,
    value: Option<T>,
    cache_slots: &mut usize,
) -> Result<()> {
    let new_len = index.map_or(values.len().saturating_add(1), |index| {
        values.len().max(index.saturating_add(1))
    });
    let added = new_len.saturating_sub(values.len());
    let total = cache_slots.saturating_add(added);
    if total > MAX_CHART_CACHE_SLOTS {
        return Err(Error::LimitExceeded(format!(
            "chart cache arrays exceed {MAX_CHART_CACHE_SLOTS} slots"
        )));
    }
    if new_len > values.len() {
        values.resize_with(new_len, || None);
        *cache_slots = total;
    }
    if let Some(index) = index {
        values[index] = value;
    } else if let Some(last) = values.last_mut() {
        *last = value;
    }
    Ok(())
}

fn ensure_capture_length(
    series: Option<&mut ChartSeries>,
    target: Capture,
    count: usize,
    cache_slots: &mut usize,
) -> Result<()> {
    let Some(series) = series else {
        return Ok(());
    };
    match target {
        Capture::Category => ensure_slots(&mut series.categories, count, cache_slots),
        Capture::XValue => ensure_slots(&mut series.x_values, count, cache_slots),
        Capture::Value => ensure_slots(&mut series.values, count, cache_slots),
        Capture::Title | Capture::SeriesName => Ok(()),
    }
}

fn ensure_slots<T>(
    values: &mut Vec<Option<T>>,
    count: usize,
    cache_slots: &mut usize,
) -> Result<()> {
    let added = count.saturating_sub(values.len());
    let total = cache_slots.saturating_add(added);
    if total > MAX_CHART_CACHE_SLOTS {
        return Err(Error::LimitExceeded(format!(
            "chart cache arrays exceed {MAX_CHART_CACHE_SLOTS} slots"
        )));
    }
    if count > values.len() {
        values.resize_with(count, || None);
        *cache_slots = total;
    }
    Ok(())
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
    let legend_width = if chart.legend_visible
        && !matches!(chart.kind, ChartKind::Bubble | ChartKind::Unsupported)
    {
        (width * 0.28).clamp(48.0, 100.0)
    } else {
        0.0
    };
    let plot_x = x + 36.0;
    let plot_y = y + title_height;
    let plot_width = (width - 48.0 - legend_width).max(12.0);
    let plot_height = (height - title_height - 28.0).max(12.0);
    if matches!(chart.kind, ChartKind::Pie | ChartKind::Doughnut) {
        render_pie(
            chart,
            plot_x,
            plot_y,
            plot_width,
            plot_height,
            id_prefix,
            &mut nodes,
        );
        if chart.legend_visible {
            render_pie_legend(
                chart,
                plot_x + plot_width + 8.0,
                plot_y + 8.0,
                id_prefix,
                legend_item_limit(plot_height),
                legend_label_limit(legend_width),
                &mut nodes,
            );
        }
        return nodes;
    }
    if matches!(chart.kind, ChartKind::Bubble | ChartKind::Unsupported) {
        let text = if chart.kind == ChartKind::Bubble {
            "Bubble chart preview is not supported"
        } else {
            "This chart type or combination is not supported"
        };
        nodes.push(text_node(
            format!("{id_prefix}-unsupported"),
            x + width / 2.0,
            y + height / 2.0,
            text.into(),
            10.0,
            TextAnchor::Middle,
            "chart-unsupported",
        ));
        return nodes;
    }
    let axes_path = if matches!(chart.kind, ChartKind::Bar | ChartKind::Area) {
        category_axes_path(chart, plot_x, plot_y, plot_width, plot_height)
    } else {
        format!(
            "M {} {} V {} H {}",
            number(plot_x),
            number(plot_y),
            number(plot_y + plot_height),
            number(plot_x + plot_width)
        )
    };
    nodes.push(path_node(
        format!("{id_prefix}-axes"),
        axes_path,
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
        ChartKind::Scatter => render_scatter(
            chart,
            plot_x,
            plot_y,
            plot_width,
            plot_height,
            id_prefix,
            &mut nodes,
        ),
        ChartKind::Area => render_area(
            chart,
            plot_x,
            plot_y,
            plot_width,
            plot_height,
            id_prefix,
            &mut nodes,
        ),
        ChartKind::Line => render_lines(
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
    if chart.legend_visible {
        render_series_legend(
            chart,
            plot_x + plot_width + 8.0,
            plot_y + 8.0,
            id_prefix,
            legend_item_limit(plot_height),
            legend_label_limit(legend_width),
            &mut nodes,
        );
    }
    nodes
}

const CHART_COLORS: [&str; 8] = [
    "#4472C4", "#ED7D31", "#A5A5A5", "#FFC000", "#5B9BD5", "#70AD47", "#264478", "#9E480E",
];
const MAX_LEGEND_ITEMS: usize = 16;
const MAX_LEGEND_LABEL_CHARS: usize = 40;

fn category_value_domain(chart: &ChartData, categories: usize) -> (f64, f64) {
    let positive_total = |index: usize| {
        chart
            .series
            .iter()
            .filter_map(|series| series.values.get(index).copied().flatten())
            .filter(|value| *value > 0.0)
            .sum::<f64>()
    };
    let negative_total = |index: usize| {
        chart
            .series
            .iter()
            .filter_map(|series| series.values.get(index).copied().flatten())
            .filter(|value| *value < 0.0)
            .map(f64::abs)
            .sum::<f64>()
    };
    let has_negative = chart
        .series
        .iter()
        .flat_map(|series| series.values.iter().flatten())
        .any(|value| *value < 0.0);
    if chart.grouping == BarGrouping::PercentStacked {
        (if has_negative { -1.0 } else { 0.0 }, 1.0)
    } else if matches!(chart.grouping, BarGrouping::Stacked) {
        (
            -(0..categories).map(negative_total).fold(0.0_f64, f64::max),
            (0..categories).map(positive_total).fold(0.0_f64, f64::max),
        )
    } else {
        let values = chart
            .series
            .iter()
            .flat_map(|series| series.values.iter().flatten())
            .copied();
        values.fold((0.0_f64, 0.0_f64), |(min, max), value| {
            (min.min(value), max.max(value))
        })
    }
}

fn category_axes_path(chart: &ChartData, x: f64, y: f64, width: f64, height: f64) -> String {
    let categories = chart
        .series
        .iter()
        .map(|series| series.values.len())
        .max()
        .unwrap_or(0);
    let (minimum, maximum) = category_value_domain(chart, categories);
    let range = (maximum - minimum).max(1e-9);
    if chart.horizontal_bars {
        let zero_x = x + (0.0 - minimum) / range * width;
        if minimum >= 0.0 {
            return format!(
                "M {} {} V {} H {}",
                number(x),
                number(y),
                number(y + height),
                number(x + width)
            );
        }
        format!(
            "M {} {} H {} M {} {} V {}",
            number(x),
            number(y + height),
            number(x + width),
            number(zero_x),
            number(y),
            number(y + height)
        )
    } else {
        let zero_y = y + height - (0.0 - minimum) / range * height;
        if minimum >= 0.0 {
            return format!(
                "M {} {} V {} H {}",
                number(x),
                number(y),
                number(y + height),
                number(x + width)
            );
        }
        format!(
            "M {} {} V {} M {} {} H {}",
            number(x),
            number(y),
            number(y + height),
            number(x),
            number(zero_y),
            number(x + width)
        )
    }
}

fn render_series_legend(
    chart: &ChartData,
    x: f64,
    y: f64,
    id_prefix: &str,
    max_items: usize,
    max_label_chars: usize,
    nodes: &mut Vec<Node>,
) {
    let labels = chart
        .series
        .iter()
        .enumerate()
        .map(|(index, series)| {
            if series.name.trim().is_empty() {
                (index, format!("Series {}", index + 1))
            } else {
                (index, series.name.clone())
            }
        })
        .collect::<Vec<_>>();
    render_legend_items(&labels, x, y, id_prefix, max_items, max_label_chars, nodes);
}

fn render_pie_legend(
    chart: &ChartData,
    x: f64,
    y: f64,
    id_prefix: &str,
    max_items: usize,
    max_label_chars: usize,
    nodes: &mut Vec<Node>,
) {
    let Some(series) = chart.series.first() else {
        return;
    };
    let labels = series
        .categories
        .iter()
        .enumerate()
        .filter_map(|(index, label)| label.as_ref().map(|label| (index, label.clone())))
        .collect::<Vec<_>>();
    render_legend_items(&labels, x, y, id_prefix, max_items, max_label_chars, nodes);
}

fn render_legend_items(
    labels: &[(usize, String)],
    x: f64,
    y: f64,
    id_prefix: &str,
    max_items: usize,
    max_label_chars: usize,
    nodes: &mut Vec<Node>,
) {
    for (row_index, (color_index, label)) in labels.iter().take(max_items).enumerate() {
        let row_y = y + row_index as f64 * 11.0;
        nodes.push(path_node(
            format!("{id_prefix}-legend-swatch-{row_index}"),
            rectangle(x, row_y, 7.0, 7.0),
            Paint::solid(CHART_COLORS[color_index % CHART_COLORS.len()]),
            Stroke::default(),
            "chart-legend-swatch",
        ));
        let mut text = label.chars().take(max_label_chars).collect::<String>();
        if label.chars().nth(max_label_chars).is_some() {
            text.push('…');
        }
        nodes.push(text_node(
            format!("{id_prefix}-legend-label-{row_index}"),
            x + 11.0,
            row_y + 6.5,
            text,
            7.0,
            TextAnchor::Start,
            "chart-legend-label",
        ));
    }
    if labels.len() > max_items {
        nodes.push(text_node(
            format!("{id_prefix}-legend-overflow"),
            x + 11.0,
            y + max_items as f64 * 11.0 + 6.5,
            format!("+ {} more", labels.len() - max_items),
            7.0,
            TextAnchor::Start,
            "chart-legend-overflow",
        ));
    }
}

fn legend_item_limit(plot_height: f64) -> usize {
    (plot_height / 11.0)
        .floor()
        .max(1.0)
        .min(MAX_LEGEND_ITEMS as f64) as usize
}

fn legend_label_limit(legend_width: f64) -> usize {
    ((legend_width - 18.0) / 4.0)
        .floor()
        .max(4.0)
        .min(MAX_LEGEND_LABEL_CHARS as f64) as usize
}

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
    let (negative_min, positive_max) = category_value_domain(chart, categories);
    let positive_total = |index: usize| {
        chart
            .series
            .iter()
            .filter_map(|series| series.values.get(index).copied().flatten())
            .filter(|value| *value > 0.0)
            .sum::<f64>()
    };
    let negative_total = |index: usize| {
        chart
            .series
            .iter()
            .filter_map(|series| series.values.get(index).copied().flatten())
            .filter(|value| *value < 0.0)
            .map(f64::abs)
            .sum::<f64>()
    };
    let positive_max = positive_max.max(if negative_min == 0.0 { 1e-9 } else { 0.0 });
    let range = (positive_max - negative_min).max(1e-9);
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
    let mut positive_base = vec![0.0_f64; categories];
    let mut negative_base = vec![0.0_f64; categories];
    for (series_index, series) in chart.series.iter().enumerate() {
        for (value_index, value) in series.values.iter().enumerate() {
            let Some(value) = value else { continue };
            let plotted_value = if chart.grouping == BarGrouping::PercentStacked {
                let total = if *value >= 0.0 {
                    positive_total(value_index)
                } else {
                    negative_total(value_index)
                };
                if total <= 0.0 { 0.0 } else { *value / total }
            } else {
                *value
            };
            let base = if stacked {
                if plotted_value >= 0.0 {
                    positive_base.get(value_index).copied().unwrap_or(0.0)
                } else {
                    negative_base.get(value_index).copied().unwrap_or(0.0)
                }
            } else {
                0.0
            };
            let end = base + plotted_value;
            let low = base.min(end);
            let high = base.max(end);
            let length = plotted_value.abs() * plot_length / range;
            let low_offset = (low - negative_min) * plot_length / range;
            let high_offset = (high - negative_min) * plot_length / range;
            let offset = if stacked {
                value_index as f64 * category_span + category_span * 0.125
            } else {
                value_index as f64 * category_span
                    + category_span * 0.125
                    + series_index as f64 * bar_span
            };
            let bounds = if chart.horizontal_bars {
                rectangle(x + low_offset, y + offset, length, bar_span * 0.9)
            } else {
                rectangle(x + offset, y + height - high_offset, bar_span * 0.9, length)
            };
            nodes.push(path_node(
                format!("{id_prefix}-bar-{series_index}-{value_index}"),
                bounds,
                Paint::solid(CHART_COLORS[series_index % CHART_COLORS.len()]),
                Stroke::default(),
                "chart-bar",
            ));
            if stacked {
                let base = if plotted_value >= 0.0 {
                    &mut positive_base
                } else {
                    &mut negative_base
                };
                if let Some(slot) = base.get_mut(value_index) {
                    *slot += plotted_value;
                }
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
        .find(|names| names.iter().any(Option::is_some))
    else {
        return;
    };
    // Past this many slots the names overlap into an unreadable band, and a
    // chart with that many points is read from its shape, not its labels.
    const MAX_LABELLED_CATEGORIES: usize = 24;
    if categories > MAX_LABELLED_CATEGORIES {
        return;
    }
    let span = if chart.horizontal_bars { height } else { width };
    // Bars occupy a slot and are labelled at its centre; a line's points sit on
    // the plot edges, so its labels belong under the points themselves.
    let plots_points = matches!(
        chart.kind,
        ChartKind::Line | ChartKind::Area | ChartKind::Scatter
    );
    let category_span = span / categories as f64;
    let last = categories.saturating_sub(1).max(1) as f64;
    for (index, name) in names.iter().take(categories).enumerate() {
        let Some(name) = name else { continue };
        if name.trim().is_empty() {
            continue;
        }
        let middle = if plots_points {
            index as f64 / last * span
        } else {
            index as f64 * category_span + category_span / 2.0
        };
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
    let categories = chart
        .series
        .iter()
        .map(|series| series.values.len())
        .max()
        .unwrap_or(0);
    if categories > 0 {
        render_category_labels(chart, x, y, width, height, categories, id_prefix, nodes);
    }
    let (minimum, maximum) = chart
        .series
        .iter()
        .flat_map(|series| series.values.iter().flatten())
        .copied()
        .fold((0.0_f64, 0.0_f64), |(minimum, maximum), value| {
            (minimum.min(value), maximum.max(value))
        });
    let range = (maximum - minimum).max(1e-9);
    for (series_index, series) in chart.series.iter().enumerate() {
        if series.values.is_empty() {
            continue;
        }
        let denominator = series.values.len().saturating_sub(1).max(1) as f64;
        let mut path = String::new();
        let mut move_to = true;
        for (index, value) in series.values.iter().enumerate() {
            let Some(value) = value else {
                move_to = true;
                continue;
            };
            let point_x = x + index as f64 / denominator * width;
            let point_y = y + height - (value - minimum) / range * height;
            path.push_str(&format!(
                "{} {} {} ",
                if move_to { "M" } else { "L" },
                number(point_x),
                number(point_y)
            ));
            move_to = false;
        }
        if path.is_empty() {
            continue;
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
fn render_scatter(
    chart: &ChartData,
    x: f64,
    y: f64,
    width: f64,
    height: f64,
    id_prefix: &str,
    nodes: &mut Vec<Node>,
) {
    let mut points = Vec::<Vec<(f64, f64)>>::new();
    let mut bounds = (
        f64::INFINITY,
        f64::NEG_INFINITY,
        f64::INFINITY,
        f64::NEG_INFINITY,
    );
    for series in &chart.series {
        let series_points = series
            .x_values
            .iter()
            .zip(&series.values)
            .filter_map(|(x_value, y_value)| {
                x_value.as_ref().copied().zip(y_value.as_ref().copied())
            })
            .collect::<Vec<_>>();
        for (x_value, y_value) in &series_points {
            bounds.0 = bounds.0.min(*x_value);
            bounds.1 = bounds.1.max(*x_value);
            bounds.2 = bounds.2.min(*y_value);
            bounds.3 = bounds.3.max(*y_value);
        }
        points.push(series_points);
    }
    if !bounds.0.is_finite() {
        return;
    }
    let map_x = |value: f64| {
        if (bounds.1 - bounds.0).abs() < f64::EPSILON {
            x + width / 2.0
        } else {
            x + (value - bounds.0) / (bounds.1 - bounds.0) * width
        }
    };
    let map_y = |value: f64| {
        if (bounds.3 - bounds.2).abs() < f64::EPSILON {
            y + height / 2.0
        } else {
            y + height - (value - bounds.2) / (bounds.3 - bounds.2) * height
        }
    };
    let show_line = matches!(
        chart.scatter_style,
        ScatterStyle::Line
            | ScatterStyle::LineMarker
            | ScatterStyle::Smooth
            | ScatterStyle::SmoothMarker
    );
    let show_markers = matches!(
        chart.scatter_style,
        ScatterStyle::Marker | ScatterStyle::LineMarker | ScatterStyle::SmoothMarker
    );
    for (series_index, series_points) in points.iter().enumerate() {
        if series_points.is_empty() {
            continue;
        }
        let color = CHART_COLORS[series_index % CHART_COLORS.len()];
        if show_line && series_points.len() > 1 {
            let path = series_points
                .iter()
                .enumerate()
                .map(|(index, (x_value, y_value))| {
                    format!(
                        "{} {} {}",
                        if index == 0 { "M" } else { "L" },
                        number(map_x(*x_value)),
                        number(map_y(*y_value))
                    )
                })
                .collect::<Vec<_>>()
                .join(" ");
            nodes.push(path_node(
                format!("{id_prefix}-scatter-line-{series_index}"),
                path,
                Paint::None,
                Stroke {
                    paint: Paint::solid(color),
                    width: 1.25,
                    miter_limit: 10.0,
                    ..Stroke::default()
                },
                "chart-scatter-line",
            ));
        }
        if show_markers {
            for (point_index, (x_value, y_value)) in series_points.iter().enumerate() {
                let cx = map_x(*x_value);
                let cy = map_y(*y_value);
                let radius = 2.0;
                let path = format!(
                    "M {} {} A {} {} 0 1 0 {} {} A {} {} 0 1 0 {} {} Z",
                    number(cx - radius),
                    number(cy),
                    number(radius),
                    number(radius),
                    number(cx + radius),
                    number(cy),
                    number(radius),
                    number(radius),
                    number(cx - radius),
                    number(cy)
                );
                nodes.push(path_node(
                    format!("{id_prefix}-scatter-point-{series_index}-{point_index}"),
                    path,
                    Paint::solid(color),
                    Stroke::default(),
                    "chart-scatter-point",
                ));
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn render_area(
    chart: &ChartData,
    x: f64,
    y: f64,
    width: f64,
    height: f64,
    id_prefix: &str,
    nodes: &mut Vec<Node>,
) {
    let count = chart
        .series
        .iter()
        .map(|series| series.values.len())
        .max()
        .unwrap_or(0);
    if count == 0 {
        return;
    }
    let stacked = matches!(
        chart.grouping,
        BarGrouping::Stacked | BarGrouping::PercentStacked
    );
    let percent = chart.grouping == BarGrouping::PercentStacked;
    let positive_total = |index: usize| {
        chart
            .series
            .iter()
            .filter_map(|series| series.values.get(index).copied().flatten())
            .filter(|value| *value > 0.0)
            .sum::<f64>()
    };
    let negative_total = |index: usize| {
        chart
            .series
            .iter()
            .filter_map(|series| series.values.get(index).copied().flatten())
            .filter(|value| *value < 0.0)
            .map(f64::abs)
            .sum::<f64>()
    };
    let (minimum, maximum) = category_value_domain(chart, count);
    let range = (maximum - minimum).max(1e-9);
    let map_y = |value: f64| y + height - (value - minimum) / range * height;
    let category_count = count.saturating_sub(1).max(1) as f64;
    for (series_index, series) in chart.series.iter().enumerate() {
        let mut path = String::new();
        let mut start = 0usize;
        while start < series.values.len() {
            while start < series.values.len() && series.values[start].is_none() {
                start += 1;
            }
            if start == series.values.len() {
                break;
            }
            let mut end = start;
            while end + 1 < series.values.len() && series.values[end + 1].is_some() {
                end += 1;
            }
            for index in start..=end {
                let x_value = x + index as f64 / category_count * width;
                let raw_value = series.values[index].unwrap_or_default();
                let side_total = if raw_value >= 0.0 {
                    positive_total(index)
                } else {
                    negative_total(index)
                };
                let value = if percent && side_total > 0.0 {
                    raw_value / side_total
                } else {
                    raw_value
                };
                let base = if stacked && raw_value >= 0.0 {
                    chart
                        .series
                        .iter()
                        .take(series_index)
                        .filter_map(|prior| prior.values.get(index).copied().flatten())
                        .filter(|prior| *prior > 0.0)
                        .map(|prior| {
                            if percent && side_total > 0.0 {
                                prior / side_total
                            } else {
                                prior
                            }
                        })
                        .sum::<f64>()
                } else if stacked {
                    chart
                        .series
                        .iter()
                        .take(series_index)
                        .filter_map(|prior| prior.values.get(index).copied().flatten())
                        .filter(|prior| *prior < 0.0)
                        .map(|prior| {
                            if percent && side_total > 0.0 {
                                prior / side_total
                            } else {
                                prior
                            }
                        })
                        .sum::<f64>()
                } else {
                    0.0
                };
                let end_value = base + value;
                let upper_value = base.max(end_value);
                path.push_str(&format!(
                    "{} {} {} ",
                    if index == start { "M" } else { "L" },
                    number(x_value),
                    number(map_y(upper_value))
                ));
            }
            for index in (start..=end).rev() {
                let x_value = x + index as f64 / category_count * width;
                let raw_value = series.values[index].unwrap_or_default();
                let side_total = if raw_value >= 0.0 {
                    positive_total(index)
                } else {
                    negative_total(index)
                };
                let value = if percent && side_total > 0.0 {
                    raw_value / side_total
                } else {
                    raw_value
                };
                let base = if stacked && raw_value >= 0.0 {
                    chart
                        .series
                        .iter()
                        .take(series_index)
                        .filter_map(|prior| prior.values.get(index).copied().flatten())
                        .filter(|prior| *prior > 0.0)
                        .map(|prior| {
                            if percent && side_total > 0.0 {
                                prior / side_total
                            } else {
                                prior
                            }
                        })
                        .sum::<f64>()
                } else if stacked {
                    chart
                        .series
                        .iter()
                        .take(series_index)
                        .filter_map(|prior| prior.values.get(index).copied().flatten())
                        .filter(|prior| *prior < 0.0)
                        .map(|prior| {
                            if percent && side_total > 0.0 {
                                prior / side_total
                            } else {
                                prior
                            }
                        })
                        .sum::<f64>()
                } else {
                    0.0
                };
                let end_value = base + value;
                let lower_value = base.min(end_value);
                path.push_str(&format!(
                    "L {} {} ",
                    number(x_value),
                    number(map_y(lower_value))
                ));
            }
            path.push_str("Z ");
            start = end + 1;
        }
        if path.is_empty() {
            continue;
        }
        nodes.push(path_node(
            format!("{id_prefix}-area-{series_index}"),
            path,
            Paint::Solid {
                color: CHART_COLORS[series_index % CHART_COLORS.len()].into(),
                opacity: 0.28,
            },
            Stroke {
                paint: Paint::solid(CHART_COLORS[series_index % CHART_COLORS.len()]),
                width: 1.25,
                miter_limit: 10.0,
                ..Stroke::default()
            },
            "chart-area",
        ));
    }
    render_category_labels(chart, x, y, width, height, count, id_prefix, nodes);
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
        .flatten()
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
        let Some(value) = value else { continue };
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
    if chart.kind == ChartKind::Doughnut {
        let radius = radius * f64::from(chart.doughnut_hole_percent.unwrap_or(75)) / 100.0;
        let hole = format!(
            "M {} {} A {} {} 0 1 0 {} {} A {} {} 0 1 0 {} {} Z",
            number(center_x - radius),
            number(center_y),
            number(radius),
            number(radius),
            number(center_x + radius),
            number(center_y),
            number(radius),
            number(radius),
            number(center_x - radius),
            number(center_y)
        );
        nodes.push(path_node(
            format!("{id_prefix}-doughnut-hole"),
            hole,
            Paint::solid("#FFFFFF"),
            Stroke::default(),
            "chart-doughnut-hole",
        ));
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scatter_uses_numeric_x_y_and_keeps_sparse_point_indices() {
        let xml = br#"<c:chartSpace xmlns:c="c"><c:chart><c:plotArea><c:scatterChart><c:scatterStyle val="marker"/><c:ser><c:xVal><c:numRef><c:numCache><c:ptCount val="4"/><c:pt idx="0"><c:v>2</c:v></c:pt><c:pt idx="2"><c:v>10</c:v></c:pt></c:numCache></c:numRef></c:xVal><c:yVal><c:numRef><c:numCache><c:ptCount val="4"/><c:pt idx="0"><c:v>4</c:v></c:pt><c:pt idx="2"><c:v>8</c:v></c:pt></c:numCache></c:numRef></c:yVal></c:ser></c:scatterChart></c:plotArea></c:chart></c:chartSpace>"#;
        let chart = parse_chart(xml, 10_000).unwrap();
        assert_eq!(chart.kind, ChartKind::Scatter);
        assert_eq!(
            chart.series[0].x_values,
            [Some(2.0), None, Some(10.0), None]
        );
        assert_eq!(chart.series[0].values, [Some(4.0), None, Some(8.0), None]);

        let nodes = render_chart(&chart, 0.0, 0.0, 200.0, 120.0, "scatter");
        assert_eq!(nodes.iter().filter(|node| matches!(node, Node::Path { meta, .. } if meta.kind == "chart-scatter-point")).count(), 2);
        let point_paths = nodes
            .iter()
            .filter_map(|node| match node {
                Node::Path { d, meta, .. } if meta.kind == "chart-scatter-point" => Some(d),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert!(point_paths[0].contains("M 34 "));
        assert!(point_paths[1].contains("M 186 "));
    }

    #[test]
    fn area_chart_is_filled_and_bubble_chart_is_explicitly_unsupported() {
        let area = parse_chart(
            br#"<c:chartSpace xmlns:c="c"><c:chart><c:plotArea><c:areaChart><c:ser><c:cat><c:strRef><c:strCache><c:pt idx="0"><c:v>A</c:v></c:pt><c:pt idx="1"><c:v>B</c:v></c:pt></c:strCache></c:strRef></c:cat><c:val><c:numRef><c:numCache><c:pt idx="0"><c:v>2</c:v></c:pt><c:pt idx="1"><c:v>4</c:v></c:pt></c:numCache></c:numRef></c:val></c:ser></c:areaChart></c:plotArea></c:chart></c:chartSpace>"#,
            10_000,
        )
        .unwrap();
        let nodes = render_chart(&area, 0.0, 0.0, 200.0, 120.0, "area");
        assert!(nodes.iter().any(|node| matches!(node, Node::Path { fill: Paint::Solid { opacity, .. }, d, meta, .. } if meta.kind == "chart-area" && *opacity == 0.28 && d.contains("Z"))));

        let bubble = parse_chart(
            br#"<c:chartSpace xmlns:c="c"><c:chart><c:plotArea><c:bubbleChart><c:ser><c:xVal><c:numRef><c:numCache><c:pt idx="0"><c:v>1</c:v></c:pt></c:numCache></c:numRef></c:xVal><c:yVal><c:numRef><c:numCache><c:pt idx="0"><c:v>2</c:v></c:pt></c:numCache></c:numRef></c:yVal><c:bubbleSize><c:numRef><c:numCache><c:pt idx="0"><c:v>100</c:v></c:pt></c:numCache></c:numRef></c:bubbleSize></c:ser></c:bubbleChart></c:plotArea></c:chart></c:chartSpace>"#,
            10_000,
        )
        .unwrap();
        let nodes = render_chart(&bubble, 0.0, 0.0, 200.0, 120.0, "bubble");
        assert_eq!(bubble.kind, ChartKind::Bubble);
        assert!(nodes.iter().any(
            |node| matches!(node, Node::Text { meta, .. } if meta.kind == "chart-unsupported")
        ));
        assert!(!nodes.iter().any(
            |node| matches!(node, Node::Path { meta, .. } if meta.kind.starts_with("chart-scatter"))
        ));

        let doughnut = parse_chart(
            br#"<c:chartSpace xmlns:c="c"><c:chart><c:plotArea><c:doughnutChart><c:holeSize val="60"/><c:ser><c:val><c:numRef><c:numCache><c:pt idx="0"><c:v>3</c:v></c:pt><c:pt idx="1"><c:v>2</c:v></c:pt></c:numCache></c:numRef></c:val></c:ser></c:doughnutChart></c:plotArea></c:chart></c:chartSpace>"#,
            10_000,
        )
        .unwrap();
        let nodes = render_chart(&doughnut, 0.0, 0.0, 200.0, 120.0, "doughnut");
        assert_eq!(doughnut.kind, ChartKind::Doughnut);
        assert_eq!(doughnut.doughnut_hole_percent, Some(60));
        assert!(nodes.iter().any(|node| matches!(node, Node::Path { meta, fill: Paint::Solid { color, .. }, .. } if meta.kind == "chart-doughnut-hole" && color == "#FFFFFF")));

        let combo = parse_chart(
            br#"<c:chartSpace xmlns:c="c"><c:chart><c:plotArea><c:barChart/><c:lineChart/></c:plotArea></c:chart></c:chartSpace>"#,
            100,
        )
        .unwrap();
        assert_eq!(combo.kind, ChartKind::Unsupported);

        let hidden_legend = parse_chart(
            br#"<c:chartSpace xmlns:c="c"><c:chart><c:plotArea><c:barChart/></c:plotArea><c:legend><c:delete val="true"/></c:legend></c:chart></c:chartSpace>"#,
            100,
        )
        .unwrap();
        assert!(!hidden_legend.legend_visible);
    }

    #[test]
    fn indexed_cache_expansion_is_bounded() {
        let xml = br#"<c:chartSpace xmlns:c="c"><c:chart><c:plotArea><c:lineChart><c:ser><c:val><c:numRef><c:numCache><c:pt idx="1000000"><c:v>1</c:v></c:pt></c:numCache></c:numRef></c:val></c:ser></c:lineChart></c:plotArea></c:chart></c:chartSpace>"#;
        let error = parse_chart(xml, 2_000_000).unwrap_err();
        assert!(error.to_string().contains("chart cache arrays exceed"));
    }

    #[test]
    fn negative_bar_line_and_area_values_cross_the_zero_axis() {
        let xml = br#"<c:chartSpace xmlns:c="c"><c:chart><c:plotArea><c:barChart><c:ser><c:cat><c:strRef><c:strCache><c:pt idx="0"><c:v>Loss</c:v></c:pt><c:pt idx="1"><c:v>Gain</c:v></c:pt></c:strCache></c:strRef></c:cat><c:val><c:numRef><c:numCache><c:pt idx="0"><c:v>-5</c:v></c:pt><c:pt idx="1"><c:v>10</c:v></c:pt></c:numCache></c:numRef></c:val></c:ser></c:barChart></c:plotArea></c:chart></c:chartSpace>"#;
        let bars = parse_chart(xml, 10_000).unwrap();
        let bar_nodes = render_chart(&bars, 0.0, 0.0, 200.0, 120.0, "negative-bars");
        let bar_paths = bar_nodes
            .iter()
            .filter_map(|node| match node {
                Node::Path { d, meta, .. } if meta.kind == "chart-bar" => Some(d),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(bar_paths.len(), 2);
        assert!(bar_paths[0].contains("M 45.5 64"));
        assert!(bar_paths[0].contains("V 92"));
        assert!(bar_paths[1].contains("M 121.5 8"));
        assert!(bar_paths[1].contains("V 64"));
        assert!(bar_nodes.iter().any(|node| matches!(node, Node::Path { d, meta, .. } if meta.kind == "chart-axis" && d.contains("M 36 64 H 188"))));

        let horizontal = parse_chart(
            br#"<c:chartSpace xmlns:c="c"><c:chart><c:plotArea><c:barChart><c:barDir val="bar"/><c:ser><c:cat><c:strRef><c:strCache><c:pt idx="0"><c:v>Loss</c:v></c:pt><c:pt idx="1"><c:v>Gain</c:v></c:pt></c:strCache></c:strRef></c:cat><c:val><c:numRef><c:numCache><c:pt idx="0"><c:v>-5</c:v></c:pt><c:pt idx="1"><c:v>10</c:v></c:pt></c:numCache></c:numRef></c:val></c:ser></c:barChart></c:plotArea></c:chart></c:chartSpace>"#,
            10_000,
        )
        .unwrap();
        let horizontal_nodes =
            render_chart(&horizontal, 0.0, 0.0, 200.0, 120.0, "negative-horizontal");
        assert!(horizontal_nodes.iter().any(|node| matches!(node, Node::Path { d, meta, .. } if meta.kind == "chart-axis" && d.contains("M 86.6667 8 V 92"))));
        assert!(horizontal_nodes.iter().any(|node| matches!(node, Node::Path { d, meta, .. } if meta.kind == "chart-bar" && d.contains("M 36 13.25 H 86.6667"))));

        let line = parse_chart(
            br#"<c:chartSpace xmlns:c="c"><c:chart><c:plotArea><c:lineChart><c:ser><c:val><c:numRef><c:numCache><c:pt idx="0"><c:v>-10</c:v></c:pt><c:pt idx="1"><c:v>10</c:v></c:pt></c:numCache></c:numRef></c:val></c:ser></c:lineChart></c:plotArea></c:chart></c:chartSpace>"#,
            10_000,
        )
        .unwrap();
        let line_nodes = render_chart(&line, 0.0, 0.0, 200.0, 120.0, "negative-line");
        assert!(line_nodes.iter().any(|node| matches!(node, Node::Path { d, meta, .. } if meta.kind == "chart-line" && d.contains("M 36 92 L 188 8"))));

        let area = parse_chart(
            br#"<c:chartSpace xmlns:c="c"><c:chart><c:plotArea><c:areaChart><c:ser><c:val><c:numRef><c:numCache><c:pt idx="0"><c:v>-5</c:v></c:pt><c:pt idx="1"><c:v>10</c:v></c:pt></c:numCache></c:numRef></c:val></c:ser></c:areaChart></c:plotArea></c:chart></c:chartSpace>"#,
            10_000,
        )
        .unwrap();
        let area_nodes = render_chart(&area, 0.0, 0.0, 200.0, 120.0, "negative-area");
        assert!(area_nodes.iter().any(|node| matches!(node, Node::Path { d, meta, .. } if meta.kind == "chart-area" && d.contains("M 36 64 L 188 8") && d.contains("L 36 92 Z"))));
    }
}
