//! Bounded ESRI ASCII Grid raster previews.

use std::collections::HashMap;
use std::io::Read;

use base64::Engine;

use crate::convert::{ConvertOptions, PageConsumer};
use crate::error::{Error, Result};
use crate::ir::{IDENTITY, Node, Page, Paint, SourceMeta, Stroke, TextAnchor, TextRun};

const MAX_ASCII_GRID_BYTES: u64 = 128 * 1024 * 1024;
const MAX_ASCII_GRID_LINE_BYTES: usize = 1024 * 1024;
const MAX_ASCII_GRID_CELLS: usize = 5_000_000;
const MAX_ASCII_GRID_DIMENSION: usize = 100_000;
const MAX_ASCII_GRID_HEADER_LINES: usize = 8;
const MAX_ASCII_GRID_PNG_BYTES: usize = 32 * 1024 * 1024;
const MAX_ASCII_GRID_COORDINATE: f64 = 1.0e15;
const MAX_ASCII_GRID_VALUE: f64 = 1.0e30;
const MAP_LONG_EDGE_POINTS: f64 = 1050.0;
const LEGEND_WIDTH_POINTS: f64 = 170.0;
const LEGEND_BANDS: usize = 48;

const COLOR_RAMP: [[u8; 3]; 5] = [
    [68, 1, 84],
    [59, 82, 139],
    [33, 145, 140],
    [94, 201, 98],
    [253, 231, 37],
];

#[derive(Clone, Copy, Debug, PartialEq)]
enum OriginKind {
    Corner,
    Center,
}

#[derive(Clone, Copy, Debug)]
struct GridHeader {
    columns: usize,
    rows: usize,
    x_origin: f64,
    y_origin: f64,
    origin_kind: OriginKind,
    cell_size: f64,
    nodata: f64,
    data_start: usize,
}

pub(crate) fn looks_like_prefix(bytes: &[u8]) -> bool {
    let Ok(text) = std::str::from_utf8(bytes) else {
        return false;
    };
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let mut keys = std::collections::HashSet::new();
    for line in text.lines().take(MAX_ASCII_GRID_HEADER_LINES) {
        let mut fields = line.split_whitespace();
        let Some(key) = fields.next() else {
            continue;
        };
        let key = key.to_ascii_lowercase();
        if matches!(
            key.as_str(),
            "ncols"
                | "nrows"
                | "xllcorner"
                | "xllcenter"
                | "yllcorner"
                | "yllcenter"
                | "cellsize"
                | "nodata_value"
        ) {
            keys.insert(key);
        } else {
            break;
        }
    }
    keys.contains("ncols")
        && keys.contains("nrows")
        && keys.contains("cellsize")
        && (keys.contains("xllcorner") || keys.contains("xllcenter"))
        && (keys.contains("yllcorner") || keys.contains("yllcenter"))
}

pub(crate) fn convert(
    path: &std::path::Path,
    options: &ConvertOptions,
    sink: &mut dyn PageConsumer,
) -> Result<Vec<String>> {
    let input_limit = options.max_input_bytes.min(MAX_ASCII_GRID_BYTES);
    let mut bytes = Vec::new();
    Read::take(std::fs::File::open(path)?, input_limit.saturating_add(1))
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > input_limit {
        return Err(Error::LimitExceeded(format!(
            "ESRI ASCII Grid input exceeds maximum limit of {input_limit} bytes"
        )));
    }
    let text = String::from_utf8(bytes).map_err(|error| {
        Error::Unsupported(format!(
            "ESRI ASCII Grid input must be ASCII/UTF-8: {error}"
        ))
    })?;
    let text = text.strip_prefix('\u{feff}').unwrap_or(&text);
    for (line_number, line) in text.lines().enumerate() {
        if line_number >= MAX_ASCII_GRID_CELLS.saturating_add(MAX_ASCII_GRID_HEADER_LINES) {
            return Err(Error::LimitExceeded(format!(
                "ESRI ASCII Grid line count exceeds {}",
                MAX_ASCII_GRID_CELLS + MAX_ASCII_GRID_HEADER_LINES
            )));
        }
        if line.len() > MAX_ASCII_GRID_LINE_BYTES {
            return Err(Error::LimitExceeded(format!(
                "ESRI ASCII Grid line exceeds {MAX_ASCII_GRID_LINE_BYTES} bytes"
            )));
        }
    }
    if options.max_pages == 0 {
        return Err(Error::LimitExceeded(
            "ESRI ASCII Grid conversion requires one output page, but max_pages is zero".into(),
        ));
    }
    let header = parse_header(text)?;
    let cell_count = header
        .columns
        .checked_mul(header.rows)
        .ok_or_else(|| Error::LimitExceeded("ESRI ASCII Grid cell count overflowed".into()))?;
    if cell_count > MAX_ASCII_GRID_CELLS {
        return Err(Error::LimitExceeded(format!(
            "ESRI ASCII Grid declares {cell_count} cells; maximum is {MAX_ASCII_GRID_CELLS}"
        )));
    }

    let (scanned_min, scanned_max, nodata_cells) = scan_cell_values(text, header, cell_count)?;
    let has_data = scanned_min.is_finite() && scanned_max.is_finite();
    let (value_min, value_max) = if has_data {
        (scanned_min, scanned_max)
    } else {
        (0.0, 0.0)
    };
    let image_href = {
        let png_bytes = encode_grid_png(text, header, cell_count, value_min, value_max)?;
        if png_bytes.len() > MAX_ASCII_GRID_PNG_BYTES {
            return Err(Error::LimitExceeded(format!(
                "ESRI ASCII Grid PNG exceeds {MAX_ASCII_GRID_PNG_BYTES} bytes"
            )));
        }
        format!(
            "data:image/png;base64,{}",
            base64::engine::general_purpose::STANDARD.encode(png_bytes)
        )
    };

    let scale = MAP_LONG_EDGE_POINTS / header.columns.max(header.rows) as f64;
    let image_width = header.columns as f64 * scale;
    let image_height = header.rows as f64 * scale;
    let page_height = image_height.max(140.0);
    let page_width = image_width + LEGEND_WIDTH_POINTS;
    let image_y = (page_height - image_height) / 2.0;
    let legend_x = image_width + 28.0;
    let legend_top = 24.0;
    let legend_height = (page_height - 48.0).max(40.0);

    let mut page = Page::new(1, page_width, page_height, "esri_ascii_grid");
    page.title = "ESRI ASCII Grid".into();
    let origin_label = match header.origin_kind {
        OriginKind::Corner => "lower-left corner",
        OriginKind::Center => "lower-left cell center",
    };
    page.description = if has_data {
        format!(
            "ESRI ASCII Grid {}×{} cells, {origin_label} ({}, {}), cell size {}, values {} to {}",
            header.columns,
            header.rows,
            format_number(header.x_origin),
            format_number(header.y_origin),
            format_number(header.cell_size),
            format_number(value_min),
            format_number(value_max)
        )
    } else {
        format!(
            "ESRI ASCII Grid {}×{} cells, {origin_label} ({}, {}), cell size {}, all cells are NODATA",
            header.columns,
            header.rows,
            format_number(header.x_origin),
            format_number(header.y_origin),
            format_number(header.cell_size)
        )
    };
    page.nodes.push(Node::Image {
        id: "esri-ascii-grid-raster".into(),
        href: image_href,
        x: 0.0,
        y: image_y,
        width: image_width,
        height: image_height,
        transform: IDENTITY,
        opacity: 1.0,
        clip_id: None,
        meta: SourceMeta {
            semantic_role: "esri:ascii-grid-raster".into(),
            alt_text: format!("{} by {} cell raster", header.columns, header.rows),
            ..Default::default()
        },
    });
    if has_data {
        add_legend(
            &mut page,
            legend_x,
            legend_top,
            legend_height,
            value_min,
            value_max,
        );
    } else {
        add_no_data_legend(&mut page, legend_x, legend_top);
    }

    let mut warnings = vec![
        if has_data {
            "ESRI ASCII Grid values use a deterministic continuous color ramp; data classification breaks and color tables are not read".into()
        } else {
            "ESRI ASCII Grid has no valid data cells outside NODATA_VALUE; no value scale is available".into()
        },
        "The raster is displayed in grid coordinates; CRS reprojection and map overlays are not performed".into(),
    ];
    if nodata_cells > 0 {
        warnings.push(format!(
            "{nodata_cells} NODATA_VALUE cells were rendered transparent"
        ));
    }
    for warning in &warnings {
        page.warn(warning.clone());
    }
    sink.consume(page)?;
    Ok(warnings)
}

fn parse_header(text: &str) -> Result<GridHeader> {
    let mut values = HashMap::<String, String>::new();
    let mut cursor = 0usize;
    let mut header_lines = 0usize;
    for (line_index, line) in text.lines().enumerate() {
        cursor = line_index;
        let line = line.trim();
        if line.is_empty() {
            cursor = line_index + 1;
            continue;
        }
        let mut fields = line.split_whitespace();
        let Some(key) = fields.next() else { continue };
        let normalized_key = key.to_ascii_lowercase();
        if !matches!(
            normalized_key.as_str(),
            "ncols"
                | "nrows"
                | "xllcorner"
                | "xllcenter"
                | "yllcorner"
                | "yllcenter"
                | "cellsize"
                | "nodata_value"
        ) {
            break;
        }
        let value = fields.next().ok_or_else(|| {
            Error::InvalidInput(format!(
                "ESRI ASCII Grid header key '{normalized_key}' has no value"
            ))
        })?;
        if fields.next().is_some() {
            return Err(Error::InvalidInput(format!(
                "ESRI ASCII Grid header key '{normalized_key}' has extra fields"
            )));
        }
        if values
            .insert(normalized_key.clone(), value.to_owned())
            .is_some()
        {
            return Err(Error::InvalidInput(format!(
                "ESRI ASCII Grid header repeats '{normalized_key}'"
            )));
        }
        cursor = line_index + 1;
        header_lines += 1;
        if header_lines == MAX_ASCII_GRID_HEADER_LINES {
            return Err(Error::LimitExceeded(format!(
                "ESRI ASCII Grid header exceeds {MAX_ASCII_GRID_HEADER_LINES} lines"
            )));
        }
    }

    let columns = parse_required_usize(&values, "ncols")?;
    let rows = parse_required_usize(&values, "nrows")?;
    if columns == 0 || rows == 0 {
        return Err(Error::InvalidInput(
            "ESRI ASCII Grid dimensions must be positive".into(),
        ));
    }
    if columns > MAX_ASCII_GRID_DIMENSION || rows > MAX_ASCII_GRID_DIMENSION {
        return Err(Error::LimitExceeded(format!(
            "ESRI ASCII Grid dimensions exceed {MAX_ASCII_GRID_DIMENSION} cells per side"
        )));
    }
    let (origin_kind, x_key, y_key) = match (
        values.contains_key("xllcorner"),
        values.contains_key("xllcenter"),
        values.contains_key("yllcorner"),
        values.contains_key("yllcenter"),
    ) {
        (true, false, true, false) => (OriginKind::Corner, "xllcorner", "yllcorner"),
        (false, true, false, true) => (OriginKind::Center, "xllcenter", "yllcenter"),
        (false, false, false, false) => {
            return Err(Error::InvalidInput(
                "ESRI ASCII Grid header is missing its lower-left origin".into(),
            ));
        }
        _ => {
            return Err(Error::InvalidInput(
                "ESRI ASCII Grid X/Y origin keys must use a matching corner or center pair".into(),
            ));
        }
    };
    let x_origin = parse_required_f64(&values, x_key)?;
    let y_origin = parse_required_f64(&values, y_key)?;
    let cell_size = parse_required_f64(&values, "cellsize")?;
    let nodata = values
        .get("nodata_value")
        .map(|value| parse_f64_value(value, "nodata_value"))
        .transpose()?
        .unwrap_or(-9999.0);
    if ![x_origin, y_origin]
        .iter()
        .all(|value| value.is_finite() && value.abs() <= MAX_ASCII_GRID_COORDINATE)
    {
        return Err(Error::InvalidInput(
            "ESRI ASCII Grid origin is non-finite or outside ±1e15".into(),
        ));
    }
    if !cell_size.is_finite() || cell_size <= 0.0 || cell_size > MAX_ASCII_GRID_COORDINATE {
        return Err(Error::InvalidInput(
            "ESRI ASCII Grid cellsize must be finite and positive".into(),
        ));
    }
    if !nodata.is_finite() {
        return Err(Error::InvalidInput(
            "ESRI ASCII Grid NODATA_VALUE must be finite".into(),
        ));
    }
    Ok(GridHeader {
        columns,
        rows,
        x_origin,
        y_origin,
        origin_kind,
        cell_size,
        nodata,
        data_start: cursor,
    })
}

fn parse_required_usize(values: &HashMap<String, String>, key: &str) -> Result<usize> {
    values
        .get(key)
        .ok_or_else(|| Error::InvalidInput(format!("ESRI ASCII Grid header is missing {key}")))?
        .parse::<usize>()
        .map_err(|_| Error::InvalidInput(format!("ESRI ASCII Grid {key} is not an integer")))
}

fn parse_required_f64(values: &HashMap<String, String>, key: &str) -> Result<f64> {
    parse_f64_value(
        values.get(key).ok_or_else(|| {
            Error::InvalidInput(format!("ESRI ASCII Grid header is missing {key}"))
        })?,
        key,
    )
}

fn parse_f64_value(value: &str, label: &str) -> Result<f64> {
    value
        .parse::<f64>()
        .map_err(|_| Error::InvalidInput(format!("ESRI ASCII Grid {label} is not numeric")))
}

fn scan_cell_values(text: &str, header: GridHeader, count: usize) -> Result<(f64, f64, usize)> {
    let mut values = grid_values(text, header.data_start);
    let mut minimum = f64::INFINITY;
    let mut maximum = f64::NEG_INFINITY;
    let mut nodata_cells = 0usize;
    for cell in 0..count {
        let token = values.next().ok_or_else(|| {
            Error::InvalidInput(format!(
                "ESRI ASCII Grid contains fewer than {count} cell values (stopped at {cell})"
            ))
        })?;
        let value = parse_cell_value(token, cell)?;
        if value == header.nodata {
            nodata_cells += 1;
        } else {
            minimum = minimum.min(value);
            maximum = maximum.max(value);
        }
    }
    if values.next().is_some() {
        return Err(Error::InvalidInput(format!(
            "ESRI ASCII Grid contains more than {count} cell values"
        )));
    }
    Ok((minimum, maximum, nodata_cells))
}

fn encode_grid_png(
    text: &str,
    header: GridHeader,
    count: usize,
    minimum: f64,
    maximum: f64,
) -> Result<Vec<u8>> {
    let mut rgba = Vec::with_capacity(count.saturating_mul(4));
    let mut values = grid_values(text, header.data_start);
    for cell in 0..count {
        let token = values.next().ok_or_else(|| {
            Error::InvalidInput(format!(
                "ESRI ASCII Grid data ended while encoding cell {cell}"
            ))
        })?;
        let value = parse_cell_value(token, cell)?;
        if value == header.nodata {
            rgba.extend_from_slice(&[0, 0, 0, 0]);
        } else {
            rgba.extend_from_slice(&color_ramp(normalize_value(value, minimum, maximum)));
            rgba.push(255);
        }
    }
    let mut bytes = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut bytes, header.columns as u32, header.rows as u32);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header().map_err(|error| {
            Error::InvalidInput(format!("cannot create ESRI ASCII Grid PNG header: {error}"))
        })?;
        writer.write_image_data(&rgba).map_err(|error| {
            Error::InvalidInput(format!("cannot encode ESRI ASCII Grid PNG: {error}"))
        })?;
    }
    Ok(bytes)
}

fn grid_values(text: &str, start: usize) -> impl Iterator<Item = &str> {
    text.lines().skip(start).flat_map(str::split_whitespace)
}

fn parse_cell_value(token: &str, cell: usize) -> Result<f64> {
    let value = token.parse::<f64>().map_err(|_| {
        Error::InvalidInput(format!(
            "ESRI ASCII Grid cell {cell} is not a numeric value"
        ))
    })?;
    if !value.is_finite() || value.abs() > MAX_ASCII_GRID_VALUE {
        return Err(Error::InvalidInput(format!(
            "ESRI ASCII Grid cell {cell} is non-finite or outside ±1e30"
        )));
    }
    Ok(value)
}

fn normalize_value(value: f64, minimum: f64, maximum: f64) -> f64 {
    if maximum > minimum {
        (value - minimum) / (maximum - minimum)
    } else {
        0.5
    }
}

fn color_ramp(value: f64) -> [u8; 3] {
    let position = value.clamp(0.0, 1.0) * (COLOR_RAMP.len() - 1) as f64;
    let lower = (position.floor() as usize).min(COLOR_RAMP.len() - 2);
    let fraction = position - lower as f64;
    std::array::from_fn(|channel| {
        let start = f64::from(COLOR_RAMP[lower][channel]);
        let end = f64::from(COLOR_RAMP[lower + 1][channel]);
        (start + (end - start) * fraction).round() as u8
    })
}

fn add_legend(page: &mut Page, x: f64, y: f64, height: f64, minimum: f64, maximum: f64) {
    let bar_width = 18.0;
    let segment_height = height / LEGEND_BANDS as f64;
    for index in 0..LEGEND_BANDS {
        let normalized = 1.0 - index as f64 / (LEGEND_BANDS - 1) as f64;
        let color = color_ramp(normalized);
        let color = format!("#{:02X}{:02X}{:02X}", color[0], color[1], color[2]);
        let top = y + index as f64 * segment_height;
        page.nodes.push(Node::Path {
            id: format!("grid-legend-band-{index}"),
            d: format!("M {x} {top} h {bar_width} v {segment_height} h -{bar_width} Z"),
            fill_rule: "nonzero".into(),
            fill: Paint::solid(color),
            stroke: Stroke::default(),
            transform: IDENTITY,
            clip_id: None,
            meta: SourceMeta {
                semantic_role: "esri:ascii-grid-legend".into(),
                ..Default::default()
            },
        });
    }
    push_legend_label(
        page,
        "grid-legend-max",
        x + bar_width + 6.0,
        y + 10.0,
        maximum,
    );
    push_legend_label(
        page,
        "grid-legend-min",
        x + bar_width + 6.0,
        y + height,
        minimum,
    );
}

fn add_no_data_legend(page: &mut Page, x: f64, y: f64) {
    page.nodes.push(Node::Text {
        id: "grid-legend-no-data".into(),
        x,
        y: y + 12.0,
        runs: vec![TextRun {
            text: "No valid data cells".into(),
            font_family: "Arial, sans-serif".into(),
            font_size: 9.0,
            fill: Paint::solid("#374151"),
            ..Default::default()
        }],
        anchor: TextAnchor::Start,
        transform: IDENTITY,
        opacity: 1.0,
        stroke: Stroke::default(),
        clip_id: None,
        meta: SourceMeta {
            semantic_role: "esri:ascii-grid-legend-label".into(),
            ..Default::default()
        },
    });
}

fn push_legend_label(page: &mut Page, id: &str, x: f64, y: f64, value: f64) {
    page.nodes.push(Node::Text {
        id: id.into(),
        x,
        y,
        runs: vec![TextRun {
            text: format_number(value),
            font_family: "Arial, sans-serif".into(),
            font_size: 9.0,
            fill: Paint::solid("#111827"),
            ..Default::default()
        }],
        anchor: TextAnchor::Start,
        transform: IDENTITY,
        opacity: 1.0,
        stroke: Stroke::default(),
        clip_id: None,
        meta: SourceMeta {
            semantic_role: "esri:ascii-grid-legend-label".into(),
            ..Default::default()
        },
    });
}

fn format_number(value: f64) -> String {
    format!("{value:.6}")
        .trim_end_matches('0')
        .trim_end_matches('.')
        .to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_required_esri_ascii_grid_header_keys() {
        assert!(looks_like_prefix(
            b"NCOLS 2\nNROWS 2\nXLLCORNER 0\nYLLCORNER 0\nCELLSIZE 1\n1 2\n3 4\n"
        ));
        assert!(looks_like_prefix(
            b"nrows 2\nncols 2\nxllcenter 0.5\nyllcenter 0.5\ncellsize 1\nnodata_value -9999\n"
        ));
        assert!(!looks_like_prefix(b"ncols 2\nnrows 2\n0 1\n"));
        assert!(looks_like_prefix(
            b"ncols 1\nnrows 1\nxllcenter 0.5\nyllcorner 0\ncellsize 1\n1\n"
        ));
    }

    #[test]
    fn maps_constant_and_ranged_values_to_the_color_ramp() {
        assert_eq!(color_ramp(normalize_value(5.0, 5.0, 5.0)), COLOR_RAMP[2]);
        assert_eq!(color_ramp(normalize_value(0.0, 0.0, 10.0)), COLOR_RAMP[0]);
        assert_eq!(color_ramp(normalize_value(10.0, 0.0, 10.0)), COLOR_RAMP[4]);
    }
}
