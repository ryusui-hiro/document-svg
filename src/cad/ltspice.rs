//! Bounded LTspice schematic (`.asc`) preview.
//!
//! LTspice's line-oriented sheet is rendered without loading `.asy` symbol
//! libraries. Wires, flags, symbols, text, and basic graphics are preserved as
//! a deterministic approximation; no simulator or external model is invoked.

use std::path::Path;

use crate::convert::{ConvertOptions, PageConsumer, read_limited_file};
use crate::error::{Error, Result};
use crate::ir::{LineCap, LineJoin, Node, Page, Paint, SourceMeta, Stroke, TextAnchor, TextRun};

const MAX_INPUT_BYTES: u64 = 128 * 1024 * 1024;
const MAX_LINES: usize = 2_000_000;
const MAX_LINE_BYTES: usize = 1 << 20;
const MAX_ITEMS: usize = 500_000;
const MAX_TEXT_BYTES: usize = 16 * 1024 * 1024;
const MAX_COORDINATE: f64 = 100_000_000.0;
const PAGE_WIDTH: f64 = 842.0;
const PAGE_HEIGHT: f64 = 595.0;
const PAGE_MARGIN: f64 = 28.0;

#[derive(Clone, Copy, Debug)]
struct Point {
    x: f64,
    y: f64,
}
#[derive(Clone, Copy, Debug)]
struct Segment {
    a: Point,
    b: Point,
}
#[derive(Clone, Debug)]
struct Symbol {
    kind: String,
    instance: String,
    value: String,
    point: Point,
}
#[derive(Clone, Debug)]
struct Label {
    text: String,
    point: Point,
    size: f64,
}
#[derive(Default)]
struct Sheet {
    width: Option<f64>,
    height: Option<f64>,
    wires: Vec<Segment>,
    symbols: Vec<Symbol>,
    labels: Vec<Label>,
    graphics: Vec<Segment>,
    warnings: Vec<String>,
    text_bytes: usize,
}

pub(crate) fn looks_like_prefix(prefix: &[u8]) -> bool {
    decode_text(prefix).ok().is_some_and(|text| {
        let lines = text
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .take(2)
            .collect::<Vec<_>>();
        lines.len() == 2
            && lines[0].to_ascii_uppercase().starts_with("VERSION ")
            && lines[1].to_ascii_uppercase().starts_with("SHEET ")
    })
}

pub(crate) fn convert(
    path: &Path,
    options: &ConvertOptions,
    sink: &mut dyn PageConsumer,
) -> Result<Vec<String>> {
    let bytes = read_limited_file(
        path,
        options.max_input_bytes.min(MAX_INPUT_BYTES),
        "LTspice schematic input",
    )?;
    let text = decode_text(&bytes)?;
    let sheet = parse_sheet(&text)?;
    let mut page = render_sheet(&sheet)?;
    page.source_format = "ltspice_asc".into();
    page.title = "LTspice schematic".into();
    page.description = "LTspice WIRE/FLAG/SYMBOL/TEXT data is rendered as a bounded approximation; .asy libraries and simulation are not executed".into();
    for warning in &sheet.warnings {
        page.warn(warning.clone());
    }
    sink.consume(page)?;
    Ok(sheet.warnings)
}

fn decode_text(bytes: &[u8]) -> Result<String> {
    if bytes.starts_with(&[0xff, 0xfe]) {
        if !(bytes.len() - 2).is_multiple_of(2) {
            return Err(Error::InvalidInput(
                "LTspice UTF-16LE input has an odd byte length".into(),
            ));
        }
        let values = bytes[2..]
            .chunks_exact(2)
            .map(|chunk| u16::from_le_bytes([chunk[0], chunk[1]]))
            .collect::<Vec<_>>();
        return String::from_utf16(&values).map_err(|error| {
            Error::InvalidInput(format!("LTspice UTF-16LE input is invalid: {error}"))
        });
    }
    if bytes.starts_with(&[0xfe, 0xff]) {
        if !(bytes.len() - 2).is_multiple_of(2) {
            return Err(Error::InvalidInput(
                "LTspice UTF-16BE input has an odd byte length".into(),
            ));
        }
        let values = bytes[2..]
            .chunks_exact(2)
            .map(|chunk| u16::from_be_bytes([chunk[0], chunk[1]]))
            .collect::<Vec<_>>();
        return String::from_utf16(&values).map_err(|error| {
            Error::InvalidInput(format!("LTspice UTF-16BE input is invalid: {error}"))
        });
    }
    String::from_utf8(bytes.to_vec()).map_err(|error| {
        Error::InvalidInput(format!(
            "LTspice input must be UTF-8/ASCII or UTF-16 with BOM: {error}"
        ))
    })
}

fn parse_sheet(text: &str) -> Result<Sheet> {
    let lines = text.lines().collect::<Vec<_>>();
    if lines.len() > MAX_LINES {
        return Err(Error::LimitExceeded(format!(
            "LTspice schematic exceeds {MAX_LINES} lines"
        )));
    }
    let mut sheet = Sheet::default();
    let mut last_symbol = None::<usize>;
    for (line_number, original) in lines.iter().enumerate() {
        if original.len() > MAX_LINE_BYTES {
            return Err(Error::LimitExceeded(format!(
                "LTspice line {} exceeds {MAX_LINE_BYTES} bytes",
                line_number + 1
            )));
        }
        let line = original.trim_end_matches('\r').trim();
        if line.is_empty() {
            continue;
        }
        let fields = line.split_ascii_whitespace().collect::<Vec<_>>();
        let keyword = fields
            .first()
            .copied()
            .unwrap_or_default()
            .to_ascii_uppercase();
        match keyword.as_str() {
            "VERSION" => {}
            "SHEET" if fields.len() >= 4 => {
                sheet.width = parse_coord(fields[2], line_number + 1).ok();
                sheet.height = parse_coord(fields[3], line_number + 1).ok();
            }
            "WIRE" if fields.len() == 5 => {
                sheet.wires.push(Segment {
                    a: point(fields[1], fields[2], line_number + 1)?,
                    b: point(fields[3], fields[4], line_number + 1)?,
                });
                last_symbol = None;
            }
            "FLAG" if fields.len() >= 4 => {
                let text = fields[3..].join(" ");
                add_label(
                    &mut sheet,
                    text,
                    point(fields[1], fields[2], line_number + 1)?,
                    2.0,
                )?;
                last_symbol = None;
            }
            "SYMBOL" if fields.len() >= 4 => {
                if sheet.symbols.len() >= MAX_ITEMS {
                    return Err(Error::LimitExceeded(format!(
                        "LTspice exceeds {MAX_ITEMS} symbols"
                    )));
                }
                let item = sheet.symbols.len();
                sheet.symbols.push(Symbol {
                    kind: fields[1].to_owned(),
                    instance: String::new(),
                    value: String::new(),
                    point: point(fields[2], fields[3], line_number + 1)?,
                });
                last_symbol = Some(item);
            }
            "SYMATTR" if fields.len() >= 3 => {
                if let Some(item) = last_symbol {
                    let value = fields[2..].join(" ");
                    match fields[1].to_ascii_lowercase().as_str() {
                        "instname" => sheet.symbols[item].instance = value,
                        "value" => sheet.symbols[item].value = value,
                        _ => {}
                    }
                }
            }
            "TEXT" if fields.len() >= 6 => {
                let text = line
                    .splitn(6, char::is_whitespace)
                    .nth(5)
                    .unwrap_or_default()
                    .trim()
                    .trim_end_matches(';')
                    .to_owned();
                add_label(
                    &mut sheet,
                    text,
                    point(fields[1], fields[2], line_number + 1)?,
                    2.0,
                )?;
                last_symbol = None;
            }
            "LINE" if fields.len() >= 6 => {
                sheet.graphics.push(Segment {
                    a: point(fields[2], fields[3], line_number + 1)?,
                    b: point(fields[4], fields[5], line_number + 1)?,
                });
                last_symbol = None;
            }
            "RECTANGLE" if fields.len() >= 6 => {
                let a = point(fields[2], fields[3], line_number + 1)?;
                let b = point(fields[4], fields[5], line_number + 1)?;
                sheet.graphics.extend([
                    Segment {
                        a: Point { x: a.x, y: a.y },
                        b: Point { x: b.x, y: a.y },
                    },
                    Segment {
                        a: Point { x: b.x, y: a.y },
                        b: Point { x: b.x, y: b.y },
                    },
                    Segment {
                        a: Point { x: b.x, y: b.y },
                        b: Point { x: a.x, y: b.y },
                    },
                    Segment {
                        a: Point { x: a.x, y: b.y },
                        b: a,
                    },
                ]);
                last_symbol = None;
            }
            "CIRCLE" if fields.len() >= 6 => {
                let center = point(fields[2], fields[3], line_number + 1)?;
                let radius = parse_coord(fields[4], line_number + 1)?.abs();
                sheet.graphics.push(Segment {
                    a: Point {
                        x: center.x - radius,
                        y: center.y,
                    },
                    b: Point {
                        x: center.x + radius,
                        y: center.y,
                    },
                });
                last_symbol = None;
            }
            "WINDOW" | "IOPIN" | "ARC" => {
                last_symbol = if keyword == "WINDOW" {
                    last_symbol
                } else {
                    None
                };
            }
            _ => {
                if line.starts_with('.') || line.starts_with("CONTROL") {
                    sheet.warnings.push(format!(
                        "LTspice directive on line {} was kept inert",
                        line_number + 1
                    ));
                } else {
                    sheet.warnings.push(format!(
                        "unsupported LTspice line {} was omitted",
                        line_number + 1
                    ));
                }
                last_symbol = None;
            }
        }
    }
    if sheet.wires.is_empty() && sheet.symbols.is_empty() && sheet.labels.is_empty() {
        return Err(Error::InvalidInput(
            "LTspice schematic contains no drawable items".into(),
        ));
    }
    sheet.warnings.push(".asy symbol libraries, external model paths, hierarchical sheets and simulation commands were not opened or executed".into());
    Ok(sheet)
}

fn add_label(sheet: &mut Sheet, text: String, point: Point, size: f64) -> Result<()> {
    if sheet.labels.len() >= MAX_ITEMS {
        return Err(Error::LimitExceeded(format!(
            "LTspice exceeds {MAX_ITEMS} labels"
        )));
    }
    sheet.text_bytes = sheet
        .text_bytes
        .checked_add(text.len())
        .ok_or_else(|| Error::LimitExceeded("LTspice text byte count overflowed".into()))?;
    if sheet.text_bytes > MAX_TEXT_BYTES {
        return Err(Error::LimitExceeded(format!(
            "LTspice text exceeds {MAX_TEXT_BYTES} bytes"
        )));
    }
    sheet.labels.push(Label { text, point, size });
    Ok(())
}
fn parse_coord(value: &str, line: usize) -> Result<f64> {
    let n = value.parse::<f64>().map_err(|_| {
        Error::InvalidInput(format!("LTspice coordinate on line {line} is invalid"))
    })?;
    if !n.is_finite() || n.abs() > MAX_COORDINATE {
        return Err(Error::LimitExceeded(
            "LTspice coordinate exceeds configured range".into(),
        ));
    }
    Ok(n)
}
fn point(x: &str, y: &str, line: usize) -> Result<Point> {
    Ok(Point {
        x: parse_coord(x, line)?,
        y: parse_coord(y, line)?,
    })
}

fn render_sheet(sheet: &Sheet) -> Result<Page> {
    let mut min_x = f64::INFINITY;
    let mut min_y = f64::INFINITY;
    let mut max_x = f64::NEG_INFINITY;
    let mut max_y = f64::NEG_INFINITY;
    let mut inc = |p: Point| {
        min_x = min_x.min(p.x);
        min_y = min_y.min(p.y);
        max_x = max_x.max(p.x);
        max_y = max_y.max(p.y);
    };
    for s in sheet.wires.iter().chain(sheet.graphics.iter()) {
        inc(s.a);
        inc(s.b);
    }
    for s in &sheet.symbols {
        inc(Point {
            x: s.point.x - 20.0,
            y: s.point.y - 14.0,
        });
        inc(Point {
            x: s.point.x + 20.0,
            y: s.point.y + 14.0,
        });
    }
    for l in &sheet.labels {
        inc(l.point);
    }
    if !min_x.is_finite() {
        return Err(Error::InvalidInput(
            "LTspice schematic has no finite geometry".into(),
        ));
    }
    let width = sheet
        .width
        .unwrap_or(max_x - min_x)
        .max(max_x - min_x)
        .max(1.0);
    let height = sheet
        .height
        .unwrap_or(max_y - min_y)
        .max(max_y - min_y)
        .max(1.0);
    let scale = ((PAGE_WIDTH - 2.0 * PAGE_MARGIN) / width)
        .min((PAGE_HEIGHT - 2.0 * PAGE_MARGIN) / height)
        .min(4.0);
    let transform = [
        scale,
        0.0,
        0.0,
        scale,
        PAGE_MARGIN - min_x * scale,
        PAGE_MARGIN - min_y * scale,
    ];
    let mut page = Page::new(1, PAGE_WIDTH, PAGE_HEIGHT, "ltspice_asc");
    let wire_stroke = Stroke {
        paint: Paint::solid("#2563eb"),
        width: 1.0,
        line_cap: LineCap::Round,
        line_join: LineJoin::Round,
        ..Stroke::default()
    };
    for (i, s) in sheet.wires.iter().enumerate() {
        page.nodes.push(Node::Path {
            id: format!("ltspice-wire-{i}"),
            d: format!("M {} {} L {} {}", s.a.x, s.a.y, s.b.x, s.b.y),
            fill_rule: "nonzero".into(),
            fill: Paint::None,
            stroke: wire_stroke.clone(),
            transform,
            clip_id: None,
            meta: SourceMeta {
                kind: "ltspice_wire".into(),
                ..SourceMeta::default()
            },
        });
    }
    for (i, s) in sheet.graphics.iter().enumerate() {
        page.nodes.push(Node::Path {
            id: format!("ltspice-graphic-{i}"),
            d: format!("M {} {} L {} {}", s.a.x, s.a.y, s.b.x, s.b.y),
            fill_rule: "nonzero".into(),
            fill: Paint::None,
            stroke: Stroke {
                paint: Paint::solid("#64748b"),
                width: 0.8,
                ..Stroke::default()
            },
            transform,
            clip_id: None,
            meta: SourceMeta {
                kind: "ltspice_graphic".into(),
                ..SourceMeta::default()
            },
        });
    }
    for (i, s) in sheet.symbols.iter().enumerate() {
        let x = s.point.x - 20.0;
        let y = s.point.y - 14.0;
        page.nodes.push(Node::Path {
            id: format!("ltspice-symbol-{i}"),
            d: format!("M {x} {y} h 40 v 28 h -40 Z"),
            fill_rule: "nonzero".into(),
            fill: Paint::solid("#f8fafc"),
            stroke: Stroke {
                paint: Paint::solid("#111827"),
                width: 1.0,
                ..Stroke::default()
            },
            transform,
            clip_id: None,
            meta: SourceMeta {
                kind: "ltspice_symbol".into(),
                source_id: s.kind.clone(),
                ..SourceMeta::default()
            },
        });
        push_text(
            &mut page,
            &format!("ltspice-instance-{i}"),
            &s.instance,
            Point {
                x: s.point.x,
                y: s.point.y - 2.0,
            },
            10.0,
            transform,
        );
        push_text(
            &mut page,
            &format!("ltspice-value-{i}"),
            &s.value,
            Point {
                x: s.point.x,
                y: s.point.y + 10.0,
            },
            8.0,
            transform,
        );
    }
    for (i, l) in sheet.labels.iter().enumerate() {
        push_text(
            &mut page,
            &format!("ltspice-label-{i}"),
            &l.text,
            l.point,
            l.size,
            transform,
        );
    }
    Ok(page)
}
fn push_text(page: &mut Page, id: &str, text: &str, point: Point, size: f64, transform: [f64; 6]) {
    if text.is_empty() {
        return;
    }
    page.nodes.push(Node::Text {
        id: id.into(),
        x: point.x,
        y: point.y,
        runs: vec![TextRun {
            text: text.into(),
            font_family: "sans-serif".into(),
            font_size: (size * transform[0]).max(2.0),
            fill: Paint::solid("#111827"),
            ..TextRun::default()
        }],
        anchor: TextAnchor::Middle,
        transform,
        opacity: 1.0,
        stroke: Stroke::default(),
        clip_id: None,
        meta: SourceMeta {
            kind: "ltspice_text".into(),
            ..SourceMeta::default()
        },
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_bom_marked_utf16_little_endian() {
        let mut bytes = vec![0xff, 0xfe];
        for unit in "Version 4\nSHEET 1 880 680\n".encode_utf16() {
            bytes.extend_from_slice(&unit.to_le_bytes());
        }
        assert_eq!(decode_text(&bytes).unwrap(), "Version 4\nSHEET 1 880 680\n");
        assert!(looks_like_prefix(&bytes));
    }
}
