//! Bounded Autodesk EAGLE XML schematic (`.sch`) preview.
//!
//! The reader extracts schematic parts, instances, wires, labels, and plain
//! text from the XML container. Library package geometry, external files,
//! scripts, and electrical analysis are never opened or evaluated.

use std::collections::HashMap;
use std::io::Cursor;
use std::path::Path;

use quick_xml::Reader;
use quick_xml::events::Event;

use crate::convert::{ConvertOptions, PageConsumer, read_limited_file};
use crate::error::{Error, Result};
use crate::ir::{LineCap, LineJoin, Node, Page, Paint, SourceMeta, Stroke, TextAnchor, TextRun};
use crate::ooxml::{attribute, local_name};

const MAX_INPUT_BYTES: u64 = 128 * 1024 * 1024;
const MAX_XML_EVENTS: usize = 2_000_000;
const MAX_XML_DEPTH: usize = 256;
const MAX_COMPONENTS: usize = 250_000;
const MAX_WIRES: usize = 1_000_000;
const MAX_LABELS: usize = 250_000;
const MAX_TEXT_BYTES: usize = 16 * 1024 * 1024;
const MAX_COORDINATE_MM: f64 = 100_000_000.0;
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
struct Component {
    name: String,
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
struct Schematic {
    components: Vec<Component>,
    wires: Vec<Segment>,
    labels: Vec<Label>,
    warnings: Vec<String>,
    text_bytes: usize,
}

pub(crate) fn looks_like_prefix(prefix: &[u8]) -> bool {
    let text = String::from_utf8_lossy(prefix).to_ascii_lowercase();
    text.lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .is_some_and(|line| line.starts_with("<?xml") || line.starts_with("<eagle"))
        && text.contains("<eagle")
}

pub(crate) fn convert(
    path: &Path,
    options: &ConvertOptions,
    sink: &mut dyn PageConsumer,
) -> Result<Vec<String>> {
    let bytes = read_limited_file(
        path,
        options.max_input_bytes.min(MAX_INPUT_BYTES),
        "EAGLE schematic input",
    )?;
    let text = String::from_utf8(bytes).map_err(|error| {
        Error::InvalidInput(format!("EAGLE schematic must be UTF-8/ASCII: {error}"))
    })?;
    let schematic = parse_schematic(text.as_bytes(), options.max_xml_events)?;
    let mut page = render_schematic(&schematic)?;
    page.source_format = "eagle_sch".into();
    page.title = "EAGLE schematic".into();
    page.description = "EAGLE XML components and wires are rendered as a bounded approximation; libraries and external resources are not opened".into();
    for warning in &schematic.warnings {
        page.warn(warning.clone());
    }
    sink.consume(page)?;
    Ok(schematic.warnings)
}

fn parse_schematic(bytes: &[u8], max_events: usize) -> Result<Schematic> {
    let mut reader = Reader::from_reader(Cursor::new(bytes));
    reader.config_mut().trim_text(true);
    let mut buffer = Vec::new();
    let mut stack = Vec::<String>::new();
    let mut parts = HashMap::<String, String>::new();
    let mut schematic = Schematic::default();
    let mut active_text = None::<Label>;
    let mut events = 0usize;
    loop {
        events = events.saturating_add(1);
        if events > max_events.min(MAX_XML_EVENTS) {
            return Err(Error::LimitExceeded(format!(
                "EAGLE XML exceeds {} parser events",
                max_events.min(MAX_XML_EVENTS)
            )));
        }
        match reader.read_event_into(&mut buffer)? {
            Event::Start(element) => {
                if stack.len() >= MAX_XML_DEPTH {
                    return Err(Error::LimitExceeded(format!(
                        "EAGLE XML nesting exceeds {MAX_XML_DEPTH}"
                    )));
                }
                let name = String::from_utf8_lossy(local_name(element.name().as_ref()))
                    .to_ascii_lowercase();
                if name == "part" {
                    if let Some(id) = attribute(&element, b"name") {
                        let value = attribute(&element, b"value")
                            .or_else(|| attribute(&element, b"deviceset"))
                            .unwrap_or_default();
                        if parts.len() >= MAX_COMPONENTS {
                            return Err(Error::LimitExceeded(format!(
                                "EAGLE schematic exceeds {MAX_COMPONENTS} parts"
                            )));
                        }
                        parts.insert(id, value);
                    }
                } else if name == "instance" {
                    if let (Some(part), Some(x), Some(y)) = (
                        attribute(&element, b"part"),
                        attribute(&element, b"x"),
                        attribute(&element, b"y"),
                    ) {
                        let point = parse_point(&x, &y)?;
                        if schematic.components.len() >= MAX_COMPONENTS {
                            return Err(Error::LimitExceeded(format!(
                                "EAGLE schematic exceeds {MAX_COMPONENTS} instances"
                            )));
                        }
                        schematic.components.push(Component {
                            name: part.clone(),
                            value: parts.get(&part).cloned().unwrap_or_default(),
                            point,
                        });
                    }
                } else if name == "wire" && in_draw_context(&stack) {
                    if let (Some(x1), Some(y1), Some(x2), Some(y2)) = (
                        attribute(&element, b"x1"),
                        attribute(&element, b"y1"),
                        attribute(&element, b"x2"),
                        attribute(&element, b"y2"),
                    ) {
                        if schematic.wires.len() >= MAX_WIRES {
                            return Err(Error::LimitExceeded(format!(
                                "EAGLE schematic exceeds {MAX_WIRES} wires"
                            )));
                        }
                        schematic.wires.push(Segment {
                            a: parse_point(&x1, &y1)?,
                            b: parse_point(&x2, &y2)?,
                        });
                    }
                } else if matches!(name.as_str(), "text" | "label") && in_draw_context(&stack) {
                    let x = attribute(&element, b"x").unwrap_or_else(|| "0".into());
                    let y = attribute(&element, b"y").unwrap_or_else(|| "0".into());
                    let size = attribute(&element, b"size")
                        .and_then(|v| v.parse::<f64>().ok())
                        .filter(|v| v.is_finite() && *v > 0.0)
                        .unwrap_or(1.5);
                    active_text = Some(Label {
                        text: String::new(),
                        point: parse_point(&x, &y)?,
                        size,
                    });
                }
                stack.push(name);
            }
            Event::Empty(element) => {
                let name = String::from_utf8_lossy(local_name(element.name().as_ref()))
                    .to_ascii_lowercase();
                if name == "wire" && in_draw_context(&stack) {
                    if let (Some(x1), Some(y1), Some(x2), Some(y2)) = (
                        attribute(&element, b"x1"),
                        attribute(&element, b"y1"),
                        attribute(&element, b"x2"),
                        attribute(&element, b"y2"),
                    ) {
                        if schematic.wires.len() >= MAX_WIRES {
                            return Err(Error::LimitExceeded(format!(
                                "EAGLE schematic exceeds {MAX_WIRES} wires"
                            )));
                        }
                        schematic.wires.push(Segment {
                            a: parse_point(&x1, &y1)?,
                            b: parse_point(&x2, &y2)?,
                        });
                    }
                } else if name == "instance" {
                    if let (Some(part), Some(x), Some(y)) = (
                        attribute(&element, b"part"),
                        attribute(&element, b"x"),
                        attribute(&element, b"y"),
                    ) {
                        if schematic.components.len() >= MAX_COMPONENTS {
                            return Err(Error::LimitExceeded(format!(
                                "EAGLE schematic exceeds {MAX_COMPONENTS} instances"
                            )));
                        }
                        schematic.components.push(Component {
                            name: part.clone(),
                            value: parts.get(&part).cloned().unwrap_or_default(),
                            point: parse_point(&x, &y)?,
                        });
                    }
                } else if name == "part"
                    && let Some(id) = attribute(&element, b"name")
                {
                    let value = attribute(&element, b"value")
                        .or_else(|| attribute(&element, b"deviceset"))
                        .unwrap_or_default();
                    parts.insert(id, value);
                }
            }
            Event::Text(event) => {
                if let Some(label) = active_text.as_mut() {
                    label.text.push_str(&event.decode().map_err(|e| {
                        Error::InvalidInput(format!("EAGLE text decode failed: {e}"))
                    })?);
                }
            }
            Event::End(event) => {
                let name =
                    String::from_utf8_lossy(local_name(event.name().as_ref())).to_ascii_lowercase();
                if matches!(name.as_str(), "text" | "label")
                    && let Some(label) = active_text.take()
                    && !label.text.is_empty()
                {
                    add_label(&mut schematic, label)?;
                }
                stack.pop();
            }
            Event::DocType(_) => schematic
                .warnings
                .push("EAGLE XML DTD was ignored; external entities were not loaded".into()),
            Event::Eof => break,
            _ => {}
        }
        buffer.clear();
    }
    if schematic.components.is_empty() && schematic.wires.is_empty() && schematic.labels.is_empty()
    {
        return Err(Error::InvalidInput(
            "EAGLE schematic contains no drawable items".into(),
        ));
    }
    schematic.warnings.push("EAGLE library package geometry, external files, scripts, pin connectivity and DRC/ERC were omitted".into());
    Ok(schematic)
}

fn in_draw_context(stack: &[String]) -> bool {
    stack
        .iter()
        .any(|name| matches!(name.as_str(), "plain" | "segment" | "net"))
}
fn add_label(schematic: &mut Schematic, label: Label) -> Result<()> {
    if schematic.labels.len() >= MAX_LABELS {
        return Err(Error::LimitExceeded(format!(
            "EAGLE schematic exceeds {MAX_LABELS} labels"
        )));
    }
    schematic.text_bytes = schematic
        .text_bytes
        .checked_add(label.text.len())
        .ok_or_else(|| Error::LimitExceeded("EAGLE text byte count overflowed".into()))?;
    if schematic.text_bytes > MAX_TEXT_BYTES {
        return Err(Error::LimitExceeded(format!(
            "EAGLE text exceeds {MAX_TEXT_BYTES} bytes"
        )));
    }
    schematic.labels.push(label);
    Ok(())
}
fn parse_point(x: &str, y: &str) -> Result<Point> {
    let x = x
        .parse::<f64>()
        .map_err(|_| Error::InvalidInput("EAGLE coordinate is invalid".into()))?;
    let y = y
        .parse::<f64>()
        .map_err(|_| Error::InvalidInput("EAGLE coordinate is invalid".into()))?;
    if !x.is_finite()
        || !y.is_finite()
        || x.abs() > MAX_COORDINATE_MM
        || y.abs() > MAX_COORDINATE_MM
    {
        return Err(Error::LimitExceeded(
            "EAGLE coordinate exceeds configured range".into(),
        ));
    }
    Ok(Point { x, y })
}

fn render_schematic(schematic: &Schematic) -> Result<Page> {
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
    for s in &schematic.wires {
        inc(s.a);
        inc(s.b);
    }
    for c in &schematic.components {
        inc(Point {
            x: c.point.x - 3.0,
            y: c.point.y - 2.0,
        });
        inc(Point {
            x: c.point.x + 3.0,
            y: c.point.y + 2.0,
        });
    }
    for l in &schematic.labels {
        inc(l.point);
    }
    if !min_x.is_finite() {
        return Err(Error::InvalidInput(
            "EAGLE schematic has no finite geometry".into(),
        ));
    }
    let width = (max_x - min_x).max(1.0);
    let height = (max_y - min_y).max(1.0);
    let scale = ((PAGE_WIDTH - 2.0 * PAGE_MARGIN) / width)
        .min((PAGE_HEIGHT - 2.0 * PAGE_MARGIN) / height)
        .min(10.0);
    let transform = [
        scale,
        0.0,
        0.0,
        scale,
        PAGE_MARGIN - min_x * scale,
        PAGE_MARGIN - min_y * scale,
    ];
    let mut page = Page::new(1, PAGE_WIDTH, PAGE_HEIGHT, "eagle_sch");
    let wire_stroke = Stroke {
        paint: Paint::solid("#2563eb"),
        width: 0.35,
        line_cap: LineCap::Round,
        line_join: LineJoin::Round,
        ..Stroke::default()
    };
    for (i, s) in schematic.wires.iter().enumerate() {
        page.nodes.push(Node::Path {
            id: format!("eagle-wire-{i}"),
            d: format!("M {} {} L {} {}", s.a.x, s.a.y, s.b.x, s.b.y),
            fill_rule: "nonzero".into(),
            fill: Paint::None,
            stroke: wire_stroke.clone(),
            transform,
            clip_id: None,
            meta: SourceMeta {
                kind: "eagle_schematic_wire".into(),
                ..SourceMeta::default()
            },
        });
    }
    for (i, c) in schematic.components.iter().enumerate() {
        let x = c.point.x - 3.0;
        let y = c.point.y - 2.0;
        page.nodes.push(Node::Path {
            id: format!("eagle-component-{i}"),
            d: format!("M {x} {y} h 6 v 4 h -6 Z"),
            fill_rule: "nonzero".into(),
            fill: Paint::solid("#f8fafc"),
            stroke: Stroke {
                paint: Paint::solid("#111827"),
                width: 0.3,
                ..Stroke::default()
            },
            transform,
            clip_id: None,
            meta: SourceMeta {
                kind: "eagle_schematic_component".into(),
                source_id: c.name.clone(),
                ..SourceMeta::default()
            },
        });
        push_text(
            &mut page,
            &format!("eagle-component-name-{i}"),
            &c.name,
            Point {
                x: c.point.x,
                y: c.point.y - 0.6,
            },
            1.0,
            transform,
        );
        push_text(
            &mut page,
            &format!("eagle-component-value-{i}"),
            &c.value,
            Point {
                x: c.point.x,
                y: c.point.y + 0.8,
            },
            0.8,
            transform,
        );
    }
    for (i, l) in schematic.labels.iter().enumerate() {
        push_text(
            &mut page,
            &format!("eagle-label-{i}"),
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
            kind: "eagle_schematic_text".into(),
            ..SourceMeta::default()
        },
    });
}
