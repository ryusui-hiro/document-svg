//! Bounded KiCad 6+ S-expression schematic (`.kicad_sch`) preview.
//!
//! The generic S-expression arena validates nesting and quoted strings before
//! this module extracts common symbols, wires, buses, labels, text, and
//! junctions. Embedded library definitions, pin connectivity, images, models,
//! and ERC/DRC semantics are intentionally not evaluated.

use std::borrow::Cow;
use std::path::Path;

use crate::cad::kicad::SexpArena;
use crate::convert::{ConvertOptions, PageConsumer, read_limited_file};
use crate::error::{Error, Result};
use crate::ir::{LineCap, LineJoin, Node, Page, Paint, SourceMeta, Stroke, TextAnchor, TextRun};

const MAX_INPUT_BYTES: u64 = 128 * 1024 * 1024;
const MAX_ITEMS: usize = 500_000;
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
#[derive(Clone, Debug)]
struct Symbol {
    library: String,
    reference: String,
    value: String,
    point: Point,
}
#[derive(Clone, Debug)]
struct Label {
    text: String,
    point: Point,
}
#[derive(Default)]
struct Schematic {
    symbols: Vec<Symbol>,
    segments: Vec<(Point, Point, bool)>,
    labels: Vec<Label>,
    junctions: Vec<Point>,
    warnings: Vec<String>,
    text_bytes: usize,
}

pub(crate) fn looks_like_prefix(prefix: &[u8]) -> bool {
    let Ok(text) = std::str::from_utf8(prefix) else {
        return false;
    };
    text.lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .is_some_and(|line| line.starts_with("(kicad_sch"))
}

pub(crate) fn convert(
    path: &Path,
    options: &ConvertOptions,
    sink: &mut dyn PageConsumer,
) -> Result<Vec<String>> {
    let bytes = read_limited_file(
        path,
        options.max_input_bytes.min(MAX_INPUT_BYTES),
        "KiCad schematic input",
    )?;
    let text = String::from_utf8(bytes).map_err(|error| {
        Error::InvalidInput(format!("KiCad schematic must be UTF-8/ASCII: {error}"))
    })?;
    let schematic = parse_schematic(&text)?;
    let mut page = render_schematic(&schematic)?;
    page.source_format = "kicad_sch".into();
    page.title = "KiCad schematic".into();
    page.description = "KiCad 6+ symbols and wires are rendered as a bounded approximation; embedded libraries and external resources are not evaluated".into();
    for warning in &schematic.warnings {
        page.warn(warning.clone());
    }
    sink.consume(page)?;
    Ok(schematic.warnings)
}

fn parse_schematic(source: &str) -> Result<Schematic> {
    let arena = SexpArena::parse(source)?;
    let root = arena
        .children(0)
        .find(|node| arena.head(*node).as_deref() == Some("kicad_sch"))
        .ok_or_else(|| Error::InvalidInput("KiCad schematic root is missing".into()))?;
    let mut schematic = Schematic::default();
    walk(&arena, root, &mut schematic, 0)?;
    if schematic.symbols.is_empty() && schematic.segments.is_empty() && schematic.labels.is_empty()
    {
        return Err(Error::InvalidInput(
            "KiCad schematic contains no drawable items".into(),
        ));
    }
    schematic.warnings.push("embedded library definitions, pin connectivity, images, 3D models and ERC/DRC metadata were omitted".into());
    Ok(schematic)
}

fn walk(arena: &SexpArena<'_>, node: usize, schematic: &mut Schematic, depth: usize) -> Result<()> {
    if depth > 128 {
        return Err(Error::LimitExceeded(
            "KiCad schematic nesting exceeds 128".into(),
        ));
    }
    match arena.head(node).as_deref() {
        Some("symbol") if arena.find_child(node, "lib_id").is_some() => {
            parse_symbol(arena, node, schematic)?
        }
        Some("wire") | Some("bus") => parse_points(
            arena,
            node,
            schematic,
            arena.head(node).as_deref() == Some("bus"),
        )?,
        Some("polyline") => parse_points(arena, node, schematic, false)?,
        Some("junction") => {
            if let Some(point) = find_at(arena, node)? {
                push_junction(schematic, point)?;
            }
        }
        Some("label") | Some("global_label") | Some("hierarchical_label") | Some("text") => {
            parse_label(arena, node, schematic)?
        }
        _ => {}
    }
    for child in arena.children(node) {
        if arena.head(child).is_some() {
            walk(arena, child, schematic, depth + 1)?;
        }
    }
    Ok(())
}

fn parse_symbol(arena: &SexpArena<'_>, node: usize, schematic: &mut Schematic) -> Result<()> {
    let library = value(arena, arena.find_child(node, "lib_id").unwrap(), 0).unwrap_or_default();
    let point = find_at(arena, node)?
        .ok_or_else(|| Error::InvalidInput("KiCad symbol has no at position".into()))?;
    let mut reference = String::new();
    let mut value_text = String::new();
    for child in arena.children(node) {
        if arena.head(child).as_deref() != Some("property") {
            continue;
        }
        let key = value(arena, child, 0).unwrap_or_default();
        let text = value(arena, child, 1).unwrap_or_default();
        match key.as_ref() {
            "Reference" => reference = text.into_owned(),
            "Value" => value_text = text.into_owned(),
            _ => {}
        }
    }
    push_text_bytes(
        schematic,
        library.len() + reference.len() + value_text.len(),
    )?;
    if schematic.symbols.len() >= MAX_ITEMS {
        return Err(Error::LimitExceeded(format!(
            "KiCad schematic exceeds {MAX_ITEMS} symbols"
        )));
    }
    schematic.symbols.push(Symbol {
        library: library.into_owned(),
        reference,
        value: value_text,
        point,
    });
    Ok(())
}

fn parse_points(
    arena: &SexpArena<'_>,
    node: usize,
    schematic: &mut Schematic,
    bus: bool,
) -> Result<()> {
    let pts = arena
        .find_child(node, "pts")
        .ok_or_else(|| Error::InvalidInput("KiCad wire is missing pts".into()))?;
    let mut points = Vec::new();
    for child in arena.children(pts) {
        if arena.head(child).as_deref() == Some("xy") {
            points.push(parse_xy(arena, child)?);
        }
    }
    if points.len() < 2 {
        return Err(Error::InvalidInput(
            "KiCad wire must contain at least two xy points".into(),
        ));
    }
    for pair in points.windows(2) {
        if schematic.segments.len() >= MAX_ITEMS {
            return Err(Error::LimitExceeded(format!(
                "KiCad schematic exceeds {MAX_ITEMS} wire segments"
            )));
        }
        schematic.segments.push((pair[0], pair[1], bus));
    }
    Ok(())
}

fn parse_label(arena: &SexpArena<'_>, node: usize, schematic: &mut Schematic) -> Result<()> {
    let text = value(arena, node, 0).unwrap_or_default();
    let Some(point) = find_at(arena, node)? else {
        return Ok(());
    };
    push_text_bytes(schematic, text.len())?;
    if schematic.labels.len() >= MAX_ITEMS {
        return Err(Error::LimitExceeded(format!(
            "KiCad schematic exceeds {MAX_ITEMS} labels"
        )));
    }
    schematic.labels.push(Label {
        text: text.into_owned(),
        point,
    });
    Ok(())
}

fn find_at(arena: &SexpArena<'_>, node: usize) -> Result<Option<Point>> {
    arena
        .find_child(node, "at")
        .map(|at| parse_xy(arena, at))
        .transpose()
}
fn parse_xy(arena: &SexpArena<'_>, node: usize) -> Result<Point> {
    let x = parse_number(value(arena, node, 0).as_deref().unwrap_or_default(), "x")?;
    let y = parse_number(value(arena, node, 1).as_deref().unwrap_or_default(), "y")?;
    Ok(Point { x, y })
}
fn parse_number(value: &str, axis: &str) -> Result<f64> {
    let number = value.parse::<f64>().map_err(|_| {
        Error::InvalidInput(format!("KiCad schematic {axis} coordinate is invalid"))
    })?;
    if !number.is_finite() || number.abs() > MAX_COORDINATE_MM {
        return Err(Error::LimitExceeded(
            "KiCad schematic coordinate exceeds configured range".into(),
        ));
    }
    Ok(number)
}
fn value<'a>(arena: &'a SexpArena<'a>, node: usize, index: usize) -> Option<Cow<'a, str>> {
    arena.value_text(node, index)
}
fn push_junction(schematic: &mut Schematic, point: Point) -> Result<()> {
    if schematic.junctions.len() >= MAX_ITEMS {
        return Err(Error::LimitExceeded(format!(
            "KiCad schematic exceeds {MAX_ITEMS} junctions"
        )));
    }
    schematic.junctions.push(point);
    Ok(())
}
fn push_text_bytes(schematic: &mut Schematic, bytes: usize) -> Result<()> {
    schematic.text_bytes = schematic
        .text_bytes
        .checked_add(bytes)
        .ok_or_else(|| Error::LimitExceeded("KiCad schematic text byte count overflowed".into()))?;
    if schematic.text_bytes > MAX_TEXT_BYTES {
        return Err(Error::LimitExceeded(format!(
            "KiCad schematic text exceeds {MAX_TEXT_BYTES} bytes"
        )));
    }
    Ok(())
}

fn render_schematic(schematic: &Schematic) -> Result<Page> {
    let mut min_x = f64::INFINITY;
    let mut min_y = f64::INFINITY;
    let mut max_x = f64::NEG_INFINITY;
    let mut max_y = f64::NEG_INFINITY;
    let mut include = |point: Point| {
        min_x = min_x.min(point.x);
        min_y = min_y.min(point.y);
        max_x = max_x.max(point.x);
        max_y = max_y.max(point.y);
    };
    for (a, b, _) in &schematic.segments {
        include(*a);
        include(*b);
    }
    for symbol in &schematic.symbols {
        include(Point {
            x: symbol.point.x - 3.0,
            y: symbol.point.y - 2.0,
        });
        include(Point {
            x: symbol.point.x + 3.0,
            y: symbol.point.y + 2.0,
        });
    }
    for label in &schematic.labels {
        include(label.point);
    }
    if !min_x.is_finite() {
        return Err(Error::InvalidInput(
            "KiCad schematic has no finite geometry".into(),
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
    let mut page = Page::new(1, PAGE_WIDTH, PAGE_HEIGHT, "kicad_sch");
    let wire_stroke = Stroke {
        paint: Paint::solid("#2563eb"),
        width: 0.35,
        line_cap: LineCap::Round,
        line_join: LineJoin::Round,
        ..Stroke::default()
    };
    for (i, (a, b, bus)) in schematic.segments.iter().enumerate() {
        let color = if *bus { "#b45309" } else { "#2563eb" };
        let stroke = Stroke {
            paint: Paint::solid(color),
            ..wire_stroke.clone()
        };
        page.nodes.push(Node::Path {
            id: format!("kicad-sch-modern-wire-{i}"),
            d: format!("M {} {} L {} {}", a.x, a.y, b.x, b.y),
            fill_rule: "nonzero".into(),
            fill: Paint::None,
            stroke,
            transform,
            clip_id: None,
            meta: SourceMeta {
                kind: "kicad_schematic_wire".into(),
                ..SourceMeta::default()
            },
        });
    }
    let symbol_stroke = Stroke {
        paint: Paint::solid("#111827"),
        width: 0.3,
        line_join: LineJoin::Round,
        ..Stroke::default()
    };
    for (i, symbol) in schematic.symbols.iter().enumerate() {
        let x = symbol.point.x - 3.0;
        let y = symbol.point.y - 2.0;
        page.nodes.push(Node::Path {
            id: format!("kicad-sch-modern-symbol-{i}"),
            d: format!("M {x} {y} h 6 v 4 h -6 Z"),
            fill_rule: "nonzero".into(),
            fill: Paint::solid("#f8fafc"),
            stroke: symbol_stroke.clone(),
            transform,
            clip_id: None,
            meta: SourceMeta {
                kind: "kicad_schematic_symbol".into(),
                source_id: symbol.library.clone(),
                ..SourceMeta::default()
            },
        });
        push_text(
            &mut page,
            &format!("kicad-sch-modern-ref-{i}"),
            &symbol.reference,
            symbol.point,
            1.3,
            transform,
        );
        push_text(
            &mut page,
            &format!("kicad-sch-modern-value-{i}"),
            &symbol.value,
            Point {
                x: symbol.point.x,
                y: symbol.point.y + 1.4,
            },
            1.0,
            transform,
        );
    }
    for (i, label) in schematic.labels.iter().enumerate() {
        push_text(
            &mut page,
            &format!("kicad-sch-modern-label-{i}"),
            &label.text,
            label.point,
            1.2,
            transform,
        );
    }
    for (i, point) in schematic.junctions.iter().enumerate() {
        page.nodes.push(Node::Path {
            id: format!("kicad-sch-modern-junction-{i}"),
            d: format!(
                "M {} {} m -0.7 0 a 0.7 0.7 0 1 0 1.4 0 a 0.7 0.7 0 1 0 -1.4 0",
                point.x, point.y
            ),
            fill_rule: "nonzero".into(),
            fill: Paint::solid("#dc2626"),
            stroke: Stroke::default(),
            transform,
            clip_id: None,
            meta: SourceMeta {
                kind: "kicad_schematic_junction".into(),
                ..SourceMeta::default()
            },
        });
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
            kind: "kicad_schematic_text".into(),
            ..SourceMeta::default()
        },
    });
}
