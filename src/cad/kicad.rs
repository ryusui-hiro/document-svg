//! Bounded 2D previews for KiCad PCB board files.
//!
//! This parser reads the board S-expression directly. It renders tracks,
//! cached zone fills, vias, footprint pads, Edge.Cuts and common graphics.
//! It never resolves project files, 3D models, images, or external libraries.

use std::borrow::Cow;
use std::path::Path;

use crate::cad::dxf::geometry::{circle_path, fmt_coord};
use crate::convert::{ConvertOptions, PageConsumer, read_limited_file};
use crate::error::{Error, Result};
use crate::ir::{
    IDENTITY, LineCap, LineJoin, Node, Page, Paint, SourceMeta, Stroke, TextAnchor, TextRun,
};

const MAX_INPUT_BYTES: u64 = 64 * 1024 * 1024;
const MAX_SEXP_NODES: usize = 1_000_000;
const MAX_SEXP_DEPTH: usize = 128;
const MAX_QUOTED_BYTES: usize = MAX_INPUT_BYTES as usize;
const MAX_TEXT_BYTES: usize = 4 * 1024 * 1024;
const MAX_TEXT_ITEM_BYTES: usize = 16 * 1024;
const MAX_GEOMETRIES: usize = 250_000;
const MAX_RENDER_POINTS: usize = 1_000_000;
const MAX_POLYGON_POINTS: usize = 100_000;
const MAX_WARNINGS: usize = 128;
const MAX_COORDINATE_MM: f64 = 1_000_000.0;
const PAGE_WIDTH: f64 = 842.0;
const PAGE_HEIGHT: f64 = 595.0;
const PAGE_MARGIN: f64 = 28.0;

#[derive(Clone, Copy, Debug)]
struct SexpNode {
    start: usize,
    end: usize,
    first_child: Option<usize>,
    last_child: Option<usize>,
    next_sibling: Option<usize>,
    is_list: bool,
    quoted: bool,
}

pub(crate) struct SexpArena<'a> {
    source: &'a str,
    nodes: Vec<SexpNode>,
    quoted_bytes: usize,
}

pub(crate) struct Children<'a> {
    nodes: &'a [SexpNode],
    next: Option<usize>,
}

impl Iterator for Children<'_> {
    type Item = usize;

    fn next(&mut self) -> Option<Self::Item> {
        let index = self.next?;
        self.next = self.nodes[index].next_sibling;
        Some(index)
    }
}

#[derive(Clone, Copy, Debug)]
struct Point {
    x: f64,
    y: f64,
}

#[derive(Clone, Copy, Debug)]
struct Bounds {
    min_x: f64,
    min_y: f64,
    max_x: f64,
    max_y: f64,
    valid: bool,
}

impl Default for Bounds {
    fn default() -> Self {
        Self {
            min_x: f64::INFINITY,
            min_y: f64::INFINITY,
            max_x: f64::NEG_INFINITY,
            max_y: f64::NEG_INFINITY,
            valid: false,
        }
    }
}

impl Bounds {
    fn include(&mut self, point: Point, expand: f64) {
        self.min_x = self.min_x.min(point.x - expand);
        self.min_y = self.min_y.min(point.y - expand);
        self.max_x = self.max_x.max(point.x + expand);
        self.max_y = self.max_y.max(point.y + expand);
        self.valid = true;
    }

    fn width(self) -> f64 {
        self.max_x - self.min_x
    }

    fn height(self) -> f64 {
        self.max_y - self.min_y
    }
}

#[derive(Clone, Debug)]
struct Primitive {
    d: String,
    fill_rule: &'static str,
    fill: Paint,
    stroke: Stroke,
    layer: String,
    role: &'static str,
    order: u8,
}

#[derive(Clone, Copy)]
struct PrimitiveStyle<'a> {
    layer: &'a str,
    fill: bool,
    width: f64,
    fill_rule: &'static str,
    role: &'static str,
}

#[derive(Clone, Debug)]
struct TextItem {
    text: String,
    point: Point,
    size_mm: f64,
    layer: String,
}

#[derive(Clone, Copy, Debug)]
struct FootprintPose {
    origin: Point,
    angle_degrees: f64,
    bottom: bool,
}

struct PadShape<'a> {
    center: Point,
    width: f64,
    height: f64,
    shape: &'a str,
    local_angle: f64,
    pose: FootprintPose,
    node: usize,
}

#[derive(Default)]
struct BoardRender {
    bounds: Bounds,
    primitives: Vec<Primitive>,
    text: Vec<TextItem>,
    warnings: Vec<String>,
    text_bytes: usize,
    render_points: usize,
}

impl BoardRender {
    fn warn_once(&mut self, warning: impl Into<String>) {
        let warning = warning.into();
        if self.warnings.len() < MAX_WARNINGS && !self.warnings.contains(&warning) {
            self.warnings.push(warning);
        }
    }

    fn add_path(&mut self, d: String, points: &[Point], style: PrimitiveStyle<'_>) -> Result<()> {
        if self.primitives.len().saturating_add(self.text.len()) >= MAX_GEOMETRIES {
            return Err(Error::LimitExceeded(format!(
                "KiCad PCB geometry exceeds {MAX_GEOMETRIES} items"
            )));
        }
        if points.iter().any(|point| {
            !point.x.is_finite()
                || !point.y.is_finite()
                || point.x.abs() > MAX_COORDINATE_MM
                || point.y.abs() > MAX_COORDINATE_MM
        }) {
            return Err(Error::LimitExceeded(format!(
                "KiCad geometry coordinate exceeds ±{MAX_COORDINATE_MM} mm"
            )));
        }
        self.render_points = self.render_points.saturating_add(points.len());
        if self.render_points > MAX_RENDER_POINTS {
            return Err(Error::LimitExceeded(format!(
                "KiCad PCB geometry exceeds {MAX_RENDER_POINTS} rendered points"
            )));
        }
        let width = safe_width(style.width).unwrap_or_else(|| {
            self.warn_once(
                "KiCad geometry with an invalid stroke width uses a small preview width",
            );
            0.12
        });
        let expand = if style.fill { 0.0 } else { width / 2.0 };
        for point in points {
            self.bounds.include(*point, expand);
        }
        let color = layer_color(style.layer);
        self.primitives.push(Primitive {
            d,
            fill_rule: style.fill_rule,
            fill: if style.fill {
                Paint::solid(color)
            } else {
                Paint::None
            },
            stroke: if width > 0.0 {
                Stroke {
                    paint: Paint::solid(color),
                    width,
                    line_cap: LineCap::Round,
                    line_join: LineJoin::Round,
                    miter_limit: 4.0,
                    dash_array: Vec::new(),
                    dash_offset: 0.0,
                }
            } else {
                Stroke::default()
            },
            layer: style.layer.to_owned(),
            role: style.role,
            order: primitive_order(style.layer, style.role),
        });
        Ok(())
    }

    fn add_line(
        &mut self,
        start: Point,
        end: Point,
        layer: &str,
        width: f64,
        role: &'static str,
    ) -> Result<()> {
        let d = format!(
            "M {} {} L {} {}",
            fmt_coord(start.x),
            fmt_coord(start.y),
            fmt_coord(end.x),
            fmt_coord(end.y)
        );
        self.add_path(
            d,
            &[start, end],
            PrimitiveStyle {
                layer,
                fill: false,
                width,
                fill_rule: "nonzero",
                role,
            },
        )
    }

    fn add_polygon(
        &mut self,
        points: &[Point],
        layer: &str,
        fill: bool,
        width: f64,
        fill_rule: &'static str,
        role: &'static str,
    ) -> Result<()> {
        if points.len() < 3 {
            return Ok(());
        }
        if points.len() > MAX_POLYGON_POINTS {
            return Err(Error::LimitExceeded(format!(
                "KiCad PCB polygon exceeds {MAX_POLYGON_POINTS} points"
            )));
        }
        let d = path_from_points(points, true);
        self.add_path(
            d,
            points,
            PrimitiveStyle {
                layer,
                fill,
                width,
                fill_rule,
                role,
            },
        )
    }

    fn add_circle(
        &mut self,
        center: Point,
        radius: f64,
        layer: &str,
        fill: bool,
        width: f64,
        role: &'static str,
    ) -> Result<()> {
        if !radius.is_finite() || radius <= 0.0 || radius > MAX_COORDINATE_MM {
            self.warn_once("KiCad circle with an invalid radius was omitted");
            return Ok(());
        }
        let d = circle_path(center.x, center.y, radius);
        let points = [
            Point {
                x: center.x - radius,
                y: center.y - radius,
            },
            Point {
                x: center.x + radius,
                y: center.y + radius,
            },
        ];
        self.add_path(
            d,
            &points,
            PrimitiveStyle {
                layer,
                fill,
                width,
                fill_rule: "nonzero",
                role,
            },
        )
    }

    fn add_ring(
        &mut self,
        center: Point,
        outer_radius: f64,
        inner_radius: f64,
        layer: &str,
        role: &'static str,
    ) -> Result<()> {
        if outer_radius <= 0.0 || outer_radius > MAX_COORDINATE_MM {
            self.warn_once("KiCad pad or via ring with an invalid size was omitted");
            return Ok(());
        }
        let mut d = circle_path(center.x, center.y, outer_radius);
        if inner_radius > 0.0 && inner_radius < outer_radius {
            d.push(' ');
            d.push_str(&circle_path(center.x, center.y, inner_radius));
        }
        let points = [
            Point {
                x: center.x - outer_radius,
                y: center.y - outer_radius,
            },
            Point {
                x: center.x + outer_radius,
                y: center.y + outer_radius,
            },
        ];
        self.add_path(
            d,
            &points,
            PrimitiveStyle {
                layer,
                fill: true,
                width: 0.0,
                fill_rule: "evenodd",
                role,
            },
        )
    }

    fn add_text(&mut self, text: String, point: Point, size_mm: f64, layer: &str) -> Result<()> {
        if text.trim().is_empty() {
            return Ok(());
        }
        if text.len() > MAX_TEXT_ITEM_BYTES {
            self.warn_once("KiCad PCB text item exceeded the per-item limit and was omitted");
            return Ok(());
        }
        if self.primitives.len().saturating_add(self.text.len()) >= MAX_GEOMETRIES {
            return Err(Error::LimitExceeded(format!(
                "KiCad PCB geometry exceeds {MAX_GEOMETRIES} rendered items"
            )));
        }
        if !point.x.is_finite()
            || !point.y.is_finite()
            || point.x.abs() > MAX_COORDINATE_MM
            || point.y.abs() > MAX_COORDINATE_MM
        {
            return Err(Error::LimitExceeded(format!(
                "KiCad text coordinate exceeds ±{MAX_COORDINATE_MM} mm"
            )));
        }
        self.text_bytes = self.text_bytes.saturating_add(text.len());
        if self.text_bytes > MAX_TEXT_BYTES {
            return Err(Error::LimitExceeded(format!(
                "KiCad PCB text exceeds {MAX_TEXT_BYTES} bytes"
            )));
        }
        let size_mm = safe_width(size_mm).unwrap_or(1.2).clamp(0.25, 100.0);
        let approx_width = size_mm * text.chars().count().clamp(1, 256) as f64 * 0.45;
        self.bounds.include(
            Point {
                x: point.x - approx_width / 2.0,
                y: point.y - size_mm,
            },
            0.0,
        );
        self.bounds.include(
            Point {
                x: point.x + approx_width / 2.0,
                y: point.y + size_mm,
            },
            0.0,
        );
        self.text.push(TextItem {
            text,
            point,
            size_mm,
            layer: layer.to_owned(),
        });
        Ok(())
    }
}

/// Detect the unique KiCad board root token from a short prefix.
pub(crate) fn looks_like_prefix(bytes: &[u8]) -> bool {
    let Ok(text) = std::str::from_utf8(bytes) else {
        return false;
    };
    let bytes = text.as_bytes();
    let cursor = skip_space_and_comments(bytes, 0);
    if bytes.get(cursor) != Some(&b'(') {
        return false;
    }
    let mut cursor = skip_space_and_comments(bytes, cursor + 1);
    let start = cursor;
    while bytes
        .get(cursor)
        .is_some_and(|byte| !byte.is_ascii_whitespace() && !matches!(*byte, b'(' | b')' | b';'))
    {
        cursor += 1;
    }
    &bytes[start..cursor] == b"kicad_pcb"
}

pub(crate) fn convert(
    path: &Path,
    options: &ConvertOptions,
    sink: &mut dyn PageConsumer,
) -> Result<Vec<String>> {
    if options.max_pages == 0 {
        return Err(Error::InvalidInput("max_pages must be at least 1".into()));
    }
    let bytes = read_limited_file(
        path,
        options.max_input_bytes.min(MAX_INPUT_BYTES),
        "KiCad PCB",
    )?;
    let source = std::str::from_utf8(&bytes)
        .map_err(|error| Error::InvalidInput(format!("KiCad PCB is not UTF-8: {error}")))?;
    let arena = SexpArena::parse(source)?;
    let mut root_children = arena.children(0);
    let root = root_children
        .next()
        .ok_or_else(|| Error::InvalidInput("KiCad PCB has no board expression".into()))?;
    if arena.head(root).as_deref() != Some("kicad_pcb") || root_children.next().is_some() {
        return Err(Error::InvalidInput(
            "KiCad PCB must contain one kicad_pcb root expression".into(),
        ));
    }

    let mut board = BoardRender::default();
    let mut title = "KiCad PCB preview".to_owned();
    let mut saw_layers = false;
    for child in arena.children(root).skip(1) {
        match arena.head(child).as_deref() {
            Some("layers") => saw_layers = true,
            Some("title_block") => {
                if let Some(title_node) = arena.find_child(child, "title")
                    && let Some(value) = arena.value_text(title_node, 0)
                    && !value.trim().is_empty()
                {
                    title = value.chars().take(512).collect();
                }
            }
            Some("segment") => parse_track_segment(&arena, child, &mut board)?,
            Some("arc") => parse_track_arc(&arena, child, &mut board)?,
            Some("via") => parse_via(&arena, child, &mut board)?,
            Some("footprint" | "module") => parse_footprint(&arena, child, &mut board)?,
            Some("zone") => parse_zone(&arena, child, &mut board)?,
            Some("gr_line") => parse_graphic_line(&arena, child, None, &mut board)?,
            Some("gr_rect") => parse_graphic_rect(&arena, child, None, &mut board)?,
            Some("gr_circle") => parse_graphic_circle(&arena, child, None, &mut board)?,
            Some("gr_poly") => parse_graphic_poly(&arena, child, None, &mut board)?,
            Some("gr_arc") => parse_graphic_arc(&arena, child, None, &mut board)?,
            Some("gr_text") => parse_text(&arena, child, &mut board, None)?,
            Some("gr_text_box" | "gr_curve" | "dimension" | "image") => board.warn_once(format!(
                "KiCad {} objects are omitted from the 2D preview",
                arena.head(child).unwrap_or_default()
            )),
            Some(
                "kicad_pcb" | "version" | "generator" | "general" | "paper" | "setup" | "property"
                | "net" | "embedded_fonts" | "groups",
            ) => {}
            Some(_) | None => {}
        }
    }
    if !saw_layers {
        return Err(Error::InvalidInput(
            "KiCad PCB has no required layers section".into(),
        ));
    }
    if !board.bounds.valid || (board.primitives.is_empty() && board.text.is_empty()) {
        return Err(Error::Unsupported(
            "KiCad PCB has no supported visible board geometry".into(),
        ));
    }
    board.warn_once(
        "KiCad PCB is a bounded 2D artwork preview; it does not run DRC, resolve project rules, or reconstruct 3D models",
    );
    board.warn_once(
        "Images, custom pads, text boxes, complex curves, and text variables are omitted or approximated",
    );

    let mut page = render_board_page(&board, &title)?;
    for warning in &board.warnings {
        page.warn(warning.clone());
    }
    sink.consume(page)?;
    Ok(board.warnings)
}

impl<'a> SexpArena<'a> {
    pub(crate) fn parse(source: &'a str) -> Result<Self> {
        let mut arena = Self {
            source,
            nodes: vec![SexpNode {
                start: 0,
                end: source.len(),
                first_child: None,
                last_child: None,
                next_sibling: None,
                is_list: true,
                quoted: false,
            }],
            quoted_bytes: 0,
        };
        let bytes = source.as_bytes();
        let mut cursor = 0usize;
        let mut stack = vec![0usize];
        while cursor < bytes.len() {
            cursor = skip_space_and_comments(bytes, cursor);
            if cursor >= bytes.len() {
                break;
            }
            match bytes[cursor] {
                b'(' => {
                    if stack.len() >= MAX_SEXP_DEPTH {
                        return Err(Error::LimitExceeded(format!(
                            "KiCad S-expression nesting exceeds {MAX_SEXP_DEPTH}"
                        )));
                    }
                    let node = arena.push_node(SexpNode {
                        start: cursor,
                        end: 0,
                        first_child: None,
                        last_child: None,
                        next_sibling: None,
                        is_list: true,
                        quoted: false,
                    })?;
                    arena.append_child(*stack.last().unwrap(), node);
                    stack.push(node);
                    cursor += 1;
                }
                b')' => {
                    if stack.len() == 1 {
                        return Err(Error::InvalidInput(
                            "KiCad S-expression has an unmatched closing parenthesis".into(),
                        ));
                    }
                    let node = stack.pop().unwrap();
                    cursor += 1;
                    arena.nodes[node].end = cursor;
                }
                b'"' => {
                    let start = cursor + 1;
                    cursor += 1;
                    let mut closed = false;
                    while cursor < bytes.len() {
                        match bytes[cursor] {
                            b'\\' => {
                                if cursor + 1 >= bytes.len() {
                                    return Err(Error::InvalidInput(
                                        "KiCad string ends with an escape".into(),
                                    ));
                                }
                                cursor += 2;
                            }
                            b'"' => {
                                let end = cursor;
                                cursor += 1;
                                arena.quoted_bytes = arena.quoted_bytes.saturating_add(end - start);
                                if arena.quoted_bytes > MAX_QUOTED_BYTES {
                                    return Err(Error::LimitExceeded(format!(
                                        "KiCad quoted strings exceed {MAX_QUOTED_BYTES} bytes"
                                    )));
                                }
                                let node = arena.push_node(SexpNode {
                                    start,
                                    end,
                                    first_child: None,
                                    last_child: None,
                                    next_sibling: None,
                                    is_list: false,
                                    quoted: true,
                                })?;
                                arena.append_child(*stack.last().unwrap(), node);
                                closed = true;
                                break;
                            }
                            _ => cursor += 1,
                        }
                    }
                    if !closed {
                        return Err(Error::InvalidInput(
                            "KiCad quoted string is not terminated".into(),
                        ));
                    }
                }
                _ => {
                    let start = cursor;
                    while cursor < bytes.len()
                        && !bytes[cursor].is_ascii_whitespace()
                        && !matches!(bytes[cursor], b'(' | b')' | b';')
                    {
                        cursor += 1;
                    }
                    if start == cursor {
                        return Err(Error::InvalidInput(
                            "KiCad S-expression has an invalid token boundary".into(),
                        ));
                    }
                    let node = arena.push_node(SexpNode {
                        start,
                        end: cursor,
                        first_child: None,
                        last_child: None,
                        next_sibling: None,
                        is_list: false,
                        quoted: false,
                    })?;
                    arena.append_child(*stack.last().unwrap(), node);
                }
            }
        }
        if stack.len() != 1 {
            return Err(Error::InvalidInput(
                "KiCad S-expression contains an unterminated list".into(),
            ));
        }
        Ok(arena)
    }

    fn push_node(&mut self, node: SexpNode) -> Result<usize> {
        if self.nodes.len() >= MAX_SEXP_NODES {
            return Err(Error::LimitExceeded(format!(
                "KiCad S-expression exceeds {MAX_SEXP_NODES} tokens"
            )));
        }
        let index = self.nodes.len();
        self.nodes.push(node);
        Ok(index)
    }

    fn append_child(&mut self, parent: usize, child: usize) {
        if let Some(previous) = self.nodes[parent].last_child {
            self.nodes[previous].next_sibling = Some(child);
        } else {
            self.nodes[parent].first_child = Some(child);
        }
        self.nodes[parent].last_child = Some(child);
    }

    pub(crate) fn children(&self, parent: usize) -> Children<'_> {
        Children {
            nodes: &self.nodes,
            next: self.nodes[parent].first_child,
        }
    }

    fn atom_text(&self, node: usize) -> Option<Cow<'a, str>> {
        let item = self.nodes.get(node)?;
        if item.is_list {
            return None;
        }
        let raw = self.source.get(item.start..item.end)?;
        if item.quoted && raw.len() > MAX_TEXT_ITEM_BYTES {
            return None;
        }
        if !item.quoted || !raw.contains('\\') {
            return Some(Cow::Borrowed(raw));
        }
        let mut result = String::with_capacity(raw.len());
        let mut chars = raw.chars();
        while let Some(character) = chars.next() {
            if character != '\\' {
                result.push(character);
                continue;
            }
            let escaped = chars.next()?;
            match escaped {
                'n' => result.push('\n'),
                'r' => result.push('\r'),
                't' => result.push('\t'),
                '"' | '\\' => result.push(escaped),
                other => {
                    result.push('\\');
                    result.push(other);
                }
            }
        }
        Some(Cow::Owned(result))
    }

    pub(crate) fn head(&self, node: usize) -> Option<Cow<'a, str>> {
        self.nodes
            .get(node)?
            .first_child
            .and_then(|child| self.atom_text(child))
    }

    fn value_node(&self, list: usize, index: usize) -> Option<usize> {
        let mut children = self.children(list);
        children.next()?;
        children.nth(index)
    }

    pub(crate) fn value_text(&self, list: usize, index: usize) -> Option<Cow<'a, str>> {
        self.atom_text(self.value_node(list, index)?)
    }

    fn value_number(&self, list: usize, index: usize) -> Option<f64> {
        let value = self.value_text(list, index)?.parse::<f64>().ok()?;
        (value.is_finite() && value.abs() <= MAX_COORDINATE_MM).then_some(value)
    }

    pub(crate) fn find_child(&self, list: usize, name: &str) -> Option<usize> {
        self.children(list)
            .filter(|child| self.nodes[*child].is_list)
            .find(|child| self.head(*child).as_deref() == Some(name))
    }

    fn has_atom(&self, list: usize, name: &str) -> bool {
        self.children(list)
            .filter(|child| !self.nodes[*child].is_list)
            .any(|child| self.atom_text(child).as_deref() == Some(name))
    }

    fn pair(&self, list: usize, name: &str) -> Option<(f64, f64)> {
        let pair = self.find_child(list, name)?;
        Some((self.value_number(pair, 0)?, self.value_number(pair, 1)?))
    }

    fn triple(&self, list: usize, name: &str) -> Option<(f64, f64, f64)> {
        let tuple = self.find_child(list, name)?;
        Some((
            self.value_number(tuple, 0)?,
            self.value_number(tuple, 1)?,
            self.value_number(tuple, 2).unwrap_or(0.0),
        ))
    }

    fn string_value(&self, list: usize, name: &str) -> Option<Cow<'a, str>> {
        self.value_text(self.find_child(list, name)?, 0)
    }

    fn number_value(&self, list: usize, name: &str) -> Option<f64> {
        self.value_number(self.find_child(list, name)?, 0)
    }

    fn list_values_text(&self, list: Option<usize>) -> Vec<String> {
        let Some(list) = list else {
            return Vec::new();
        };
        self.children(list)
            .skip(1)
            .filter_map(|child| self.atom_text(child).map(Cow::into_owned))
            .collect()
    }

    fn points(&self, list: usize) -> Result<Vec<Point>> {
        let points_node = self
            .find_child(list, "pts")
            .ok_or_else(|| Error::InvalidInput("KiCad polygon has no pts list".into()))?;
        let mut points = Vec::new();
        for child in self.children(points_node).skip(1) {
            if self.head(child).as_deref() == Some("xy") {
                let x = self.value_number(child, 0).ok_or_else(|| {
                    Error::InvalidInput("KiCad xy point has an invalid x coordinate".into())
                })?;
                let y = self.value_number(child, 1).ok_or_else(|| {
                    Error::InvalidInput("KiCad xy point has an invalid y coordinate".into())
                })?;
                points.push(Point { x, y });
                if points.len() > MAX_POLYGON_POINTS {
                    return Err(Error::LimitExceeded(format!(
                        "KiCad polygon exceeds {MAX_POLYGON_POINTS} points"
                    )));
                }
            }
        }
        Ok(points)
    }
}

fn skip_space_and_comments(bytes: &[u8], mut cursor: usize) -> usize {
    loop {
        while cursor < bytes.len()
            && (bytes[cursor].is_ascii_whitespace()
                || (cursor == 0 && bytes[cursor..].starts_with(&[0xEF, 0xBB, 0xBF])))
        {
            if cursor == 0 && bytes[cursor..].starts_with(&[0xEF, 0xBB, 0xBF]) {
                cursor += 3;
            } else {
                cursor += 1;
            }
        }
        if bytes.get(cursor) == Some(&b';') {
            while cursor < bytes.len() && bytes[cursor] != b'\n' {
                cursor += 1;
            }
            continue;
        }
        return cursor;
    }
}

fn parse_track_segment(arena: &SexpArena<'_>, node: usize, board: &mut BoardRender) -> Result<()> {
    let start = required_point(arena, node, "start")?;
    let end = required_point(arena, node, "end")?;
    let layer = required_layer(arena, node)?;
    board.add_line(
        start,
        end,
        &layer,
        arena.number_value(node, "width").unwrap_or(0.2),
        "pcb:track",
    )
}

fn parse_track_arc(arena: &SexpArena<'_>, node: usize, board: &mut BoardRender) -> Result<()> {
    let start = required_point(arena, node, "start")?;
    let mid = required_point(arena, node, "mid")?;
    let end = required_point(arena, node, "end")?;
    let points = sample_arc(start, mid, end);
    board.add_path(
        path_from_points(&points, false),
        &points,
        PrimitiveStyle {
            layer: &required_layer(arena, node)?,
            fill: false,
            width: arena.number_value(node, "width").unwrap_or(0.2),
            fill_rule: "nonzero",
            role: "pcb:track_arc",
        },
    )
}

fn parse_via(arena: &SexpArena<'_>, node: usize, board: &mut BoardRender) -> Result<()> {
    let (x, y, _) = arena
        .triple(node, "at")
        .ok_or_else(|| Error::InvalidInput("KiCad via is missing a valid at position".into()))?;
    let size = arena
        .number_value(node, "size")
        .ok_or_else(|| Error::InvalidInput("KiCad via is missing a valid size".into()))?;
    let drill = arena.number_value(node, "drill").unwrap_or(0.0);
    let layer = first_copper_layer(arena, node).unwrap_or_else(|| "*.Cu".to_owned());
    board.add_ring(Point { x, y }, size / 2.0, drill / 2.0, &layer, "pcb:via")
}

fn parse_zone(arena: &SexpArena<'_>, node: usize, board: &mut BoardRender) -> Result<()> {
    let mut filled_count = 0usize;
    for child in arena.children(node).skip(1) {
        if arena.head(child).as_deref() == Some("filled_polygon") {
            let points = arena.points(child)?;
            board.add_polygon(
                &points,
                &required_layer(arena, child)?,
                true,
                0.0,
                "evenodd",
                "pcb:zone_fill",
            )?;
            filled_count += 1;
        }
    }
    if filled_count == 0 {
        board.warn_once(
            "KiCad PCB contains zones without cached filled polygons; zone copper was omitted",
        );
    } else {
        board.warn_once(
            "KiCad copper zones use cached filled polygons; zone refill and thermal relief generation are not performed",
        );
    }
    if arena.find_child(node, "keepout").is_some() {
        board.warn_once("KiCad keepout zones are not evaluated by the PCB preview");
    }
    Ok(())
}

fn parse_graphic_line(
    arena: &SexpArena<'_>,
    node: usize,
    pose: Option<FootprintPose>,
    board: &mut BoardRender,
) -> Result<()> {
    let mut start = required_point(arena, node, "start")?;
    let mut end = required_point(arena, node, "end")?;
    if let Some(pose) = pose {
        start = footprint_local_point(pose, start.x, start.y);
        end = footprint_local_point(pose, end.x, end.y);
    }
    board.add_line(
        start,
        end,
        &required_layer(arena, node)?,
        graphic_width(arena, node).unwrap_or(0.12),
        if pose.is_some() {
            "pcb:footprint_graphic"
        } else {
            "pcb:graphic"
        },
    )
}

fn parse_graphic_rect(
    arena: &SexpArena<'_>,
    node: usize,
    pose: Option<FootprintPose>,
    board: &mut BoardRender,
) -> Result<()> {
    let start = required_point(arena, node, "start")?;
    let end = required_point(arena, node, "end")?;
    let local = [
        start,
        Point {
            x: end.x,
            y: start.y,
        },
        end,
        Point {
            x: start.x,
            y: end.y,
        },
    ];
    let points = local
        .into_iter()
        .map(|point| pose.map_or(point, |pose| footprint_local_point(pose, point.x, point.y)))
        .collect::<Vec<_>>();
    board.add_polygon(
        &points,
        &required_layer(arena, node)?,
        is_filled(arena, node),
        graphic_width(arena, node).unwrap_or(0.12),
        "nonzero",
        if pose.is_some() {
            "pcb:footprint_graphic"
        } else {
            "pcb:graphic"
        },
    )
}

fn parse_graphic_circle(
    arena: &SexpArena<'_>,
    node: usize,
    pose: Option<FootprintPose>,
    board: &mut BoardRender,
) -> Result<()> {
    let center = required_point(arena, node, "center")?;
    let end = required_point(arena, node, "end")?;
    let radius = ((end.x - center.x).powi(2) + (end.y - center.y).powi(2)).sqrt();
    let center = pose.map_or(center, |pose| {
        footprint_local_point(pose, center.x, center.y)
    });
    board.add_circle(
        center,
        radius,
        &required_layer(arena, node)?,
        is_filled(arena, node),
        graphic_width(arena, node).unwrap_or(0.12),
        if pose.is_some() {
            "pcb:footprint_graphic"
        } else {
            "pcb:graphic"
        },
    )
}

fn parse_graphic_poly(
    arena: &SexpArena<'_>,
    node: usize,
    pose: Option<FootprintPose>,
    board: &mut BoardRender,
) -> Result<()> {
    let points = arena
        .points(node)?
        .into_iter()
        .map(|point| pose.map_or(point, |pose| footprint_local_point(pose, point.x, point.y)))
        .collect::<Vec<_>>();
    board.add_polygon(
        &points,
        &required_layer(arena, node)?,
        is_filled(arena, node),
        graphic_width(arena, node).unwrap_or(0.12),
        "nonzero",
        if pose.is_some() {
            "pcb:footprint_graphic"
        } else {
            "pcb:graphic"
        },
    )
}

fn parse_graphic_arc(
    arena: &SexpArena<'_>,
    node: usize,
    pose: Option<FootprintPose>,
    board: &mut BoardRender,
) -> Result<()> {
    let start = required_point(arena, node, "start")?;
    let mid = required_point(arena, node, "mid")?;
    let end = required_point(arena, node, "end")?;
    let points = sample_arc(start, mid, end)
        .into_iter()
        .map(|point| pose.map_or(point, |pose| footprint_local_point(pose, point.x, point.y)))
        .collect::<Vec<_>>();
    board.add_path(
        path_from_points(&points, false),
        &points,
        PrimitiveStyle {
            layer: &required_layer(arena, node)?,
            fill: false,
            width: graphic_width(arena, node).unwrap_or(0.12),
            fill_rule: "nonzero",
            role: if pose.is_some() {
                "pcb:footprint_graphic"
            } else {
                "pcb:graphic_arc"
            },
        },
    )
}

fn parse_footprint(arena: &SexpArena<'_>, node: usize, board: &mut BoardRender) -> Result<()> {
    let (x, y, angle_degrees) = arena
        .triple(node, "at")
        .ok_or_else(|| Error::InvalidInput("KiCad footprint has no valid at position".into()))?;
    let layer = arena
        .string_value(node, "layer")
        .unwrap_or(Cow::Borrowed("F.Cu"));
    let non_unit_scale = arena.find_child(node, "scale").is_some_and(|scale| {
        arena.pair(node, "scale").is_none_or(|(scale_x, scale_y)| {
            (scale_x - 1.0).abs() > 1e-9 || (scale_y - 1.0).abs() > 1e-9
        }) || arena.head(scale).is_none()
    });
    let unsupported_flip = arena
        .find_child(node, "flip")
        .is_some_and(|flip| !matches!(arena.value_text(flip, 0).as_deref(), Some("no" | "false")));
    if non_unit_scale || unsupported_flip {
        board.warn_once(
            "KiCad affine-scaled or explicitly flipped footprints are omitted from the preview",
        );
        return Ok(());
    }
    let pose = FootprintPose {
        origin: Point { x, y },
        angle_degrees,
        bottom: is_bottom_layer(&layer),
    };
    for child in arena.children(node).skip(1) {
        match arena.head(child).as_deref() {
            Some("pad") => parse_pad(arena, child, pose, board)?,
            Some("fp_line") => parse_graphic_line(arena, child, Some(pose), board)?,
            Some("fp_rect") => parse_graphic_rect(arena, child, Some(pose), board)?,
            Some("fp_circle") => parse_graphic_circle(arena, child, Some(pose), board)?,
            Some("fp_poly") => parse_graphic_poly(arena, child, Some(pose), board)?,
            Some("fp_arc") => parse_graphic_arc(arena, child, Some(pose), board)?,
            Some("fp_text" | "property") => parse_text(arena, child, board, Some(pose))?,
            Some("fp_text_box" | "fp_curve") => board.warn_once(format!(
                "KiCad {} footprint items are omitted from the preview",
                arena.head(child).unwrap_or_default()
            )),
            Some("model") => {
                board.warn_once("KiCad 3D model references are never opened or rendered")
            }
            _ => {}
        }
    }
    Ok(())
}

fn parse_pad(
    arena: &SexpArena<'_>,
    node: usize,
    pose: FootprintPose,
    board: &mut BoardRender,
) -> Result<()> {
    let args = arena.children(node).skip(1).collect::<Vec<_>>();
    let Some(shape_node) = args.get(2).copied() else {
        board.warn_once("Malformed KiCad pad was omitted");
        return Ok(());
    };
    let Some(shape) = arena.atom_text(shape_node).map(Cow::into_owned) else {
        board.warn_once("KiCad custom pad shape was omitted");
        return Ok(());
    };
    let Some((local_x, local_y, local_angle)) = arena.triple(node, "at") else {
        board.warn_once("KiCad pad without a valid at position was omitted");
        return Ok(());
    };
    let Some((width, height)) = arena.pair(node, "size") else {
        board.warn_once("KiCad pad without a valid size was omitted");
        return Ok(());
    };
    if width <= 0.0 || height <= 0.0 {
        board.warn_once("KiCad pad with non-positive dimensions was omitted");
        return Ok(());
    }
    let layers = arena.list_values_text(arena.find_child(node, "layers"));
    let copper_layers = pad_copper_layers(&layers);
    if copper_layers.is_empty() {
        return Ok(());
    }
    let points = pad_outline(
        arena,
        PadShape {
            center: Point {
                x: local_x,
                y: local_y,
            },
            width,
            height,
            shape: &shape,
            local_angle,
            pose,
            node,
        },
        board,
    );
    let Some(points) = points else {
        return Ok(());
    };
    let drill = pad_drill(arena, node, board);
    for layer in copper_layers {
        if drill > 0.0 {
            let center = footprint_local_point(pose, local_x, local_y);
            let mut d = path_from_points(&points, true);
            d.push(' ');
            d.push_str(&circle_path(center.x, center.y, drill / 2.0));
            board.add_path(
                d,
                &points,
                PrimitiveStyle {
                    layer: &layer,
                    fill: true,
                    width: 0.0,
                    fill_rule: "evenodd",
                    role: "pcb:pad",
                },
            )?;
        } else {
            board.add_polygon(&points, &layer, true, 0.0, "nonzero", "pcb:pad")?;
        }
    }
    Ok(())
}

fn pad_outline(
    arena: &SexpArena<'_>,
    pad: PadShape<'_>,
    board: &mut BoardRender,
) -> Option<Vec<Point>> {
    let local = match pad.shape {
        "circle" => circle_points(
            Point { x: 0.0, y: 0.0 },
            pad.width.min(pad.height) / 2.0,
            32,
        ),
        "oval" => rounded_rectangle_points(pad.width, pad.height, 0.5),
        "roundrect" => rounded_rectangle_points(
            pad.width,
            pad.height,
            arena
                .number_value(pad.node, "roundrect_rratio")
                .unwrap_or(0.25)
                .clamp(0.0, 0.5),
        ),
        "rect" | "trapezoid" => {
            if pad.shape == "trapezoid" || arena.find_child(pad.node, "rect_delta").is_some() {
                board.warn_once("KiCad trapezoid or delta pads are shown as rectangular pads");
            }
            vec![
                Point {
                    x: -pad.width / 2.0,
                    y: -pad.height / 2.0,
                },
                Point {
                    x: pad.width / 2.0,
                    y: -pad.height / 2.0,
                },
                Point {
                    x: pad.width / 2.0,
                    y: pad.height / 2.0,
                },
                Point {
                    x: -pad.width / 2.0,
                    y: pad.height / 2.0,
                },
            ]
        }
        "custom" => {
            board.warn_once("KiCad custom pad primitives are omitted from the PCB preview");
            return None;
        }
        _ => {
            board.warn_once(format!(
                "KiCad pad shape {} is unsupported and was omitted",
                pad.shape
            ));
            return None;
        }
    };
    let pad_angle = if pad.pose.bottom {
        pad.local_angle
    } else {
        -pad.local_angle
    };
    let (pad_sin, pad_cos) = pad_angle.to_radians().sin_cos();
    let center = Point {
        x: pad.center.x,
        y: if pad.pose.bottom {
            -pad.center.y
        } else {
            pad.center.y
        },
    };
    let footprint_angle = -pad.pose.angle_degrees.to_radians();
    let (foot_sin, foot_cos) = footprint_angle.sin_cos();
    Some(
        local
            .into_iter()
            .map(|point| {
                let x = point.x * pad_cos - point.y * pad_sin;
                let y = point.x * pad_sin + point.y * pad_cos;
                let x = center.x + x;
                let y = center.y + y;
                Point {
                    x: pad.pose.origin.x + x * foot_cos - y * foot_sin,
                    y: pad.pose.origin.y + x * foot_sin + y * foot_cos,
                }
            })
            .collect(),
    )
}

fn rounded_rectangle_points(width: f64, height: f64, ratio: f64) -> Vec<Point> {
    let radius = width.min(height) * ratio;
    if radius <= 0.0 {
        return vec![
            Point {
                x: -width / 2.0,
                y: -height / 2.0,
            },
            Point {
                x: width / 2.0,
                y: -height / 2.0,
            },
            Point {
                x: width / 2.0,
                y: height / 2.0,
            },
            Point {
                x: -width / 2.0,
                y: height / 2.0,
            },
        ];
    }
    let corners = [
        (width / 2.0 - radius, -height / 2.0 + radius, -90.0),
        (width / 2.0 - radius, height / 2.0 - radius, 0.0),
        (-width / 2.0 + radius, height / 2.0 - radius, 90.0),
        (-width / 2.0 + radius, -height / 2.0 + radius, 180.0),
    ];
    let mut points = Vec::with_capacity(16);
    for (cx, cy, start) in corners {
        for step in 0..4 {
            let angle = (start + step as f64 * 30.0).to_radians();
            points.push(Point {
                x: cx + radius * angle.cos(),
                y: cy + radius * angle.sin(),
            });
        }
    }
    points
}

fn pad_drill(arena: &SexpArena<'_>, node: usize, board: &mut BoardRender) -> f64 {
    let Some(drill) = arena.find_child(node, "drill") else {
        return 0.0;
    };
    if arena
        .pair(drill, "offset")
        .is_some_and(|(x, y)| x.abs() > 1e-8 || y.abs() > 1e-8)
    {
        board.warn_once("KiCad pad drill offsets are centered in the preview");
    }
    if let Some(size) = arena.value_number(drill, 0) {
        return size.max(0.0);
    }
    if let Some((width, height)) = arena.pair(drill, "size") {
        if (width - height).abs() > 1e-6 {
            board.warn_once("Oval KiCad pad drills are approximated as circular holes");
        }
        return width.min(height).max(0.0);
    }
    0.0
}

fn parse_text(
    arena: &SexpArena<'_>,
    node: usize,
    board: &mut BoardRender,
    pose: Option<FootprintPose>,
) -> Result<()> {
    let head = arena.head(node).unwrap_or_default();
    let (text_index, layer) = if head == "gr_text" {
        (0, required_layer(arena, node)?)
    } else if head == "fp_text" {
        if arena.has_atom(node, "hide") {
            return Ok(());
        }
        (1, required_layer(arena, node)?)
    } else if head == "property" {
        if arena.has_atom(node, "hide") {
            return Ok(());
        }
        let Some(kind) = arena.value_text(node, 0) else {
            return Ok(());
        };
        if !matches!(kind.as_ref(), "Reference" | "Value") {
            return Ok(());
        }
        (1, required_layer(arena, node)?)
    } else {
        return Ok(());
    };
    let Some(text) = arena.value_text(node, text_index) else {
        return Ok(());
    };
    let Some((x, y, angle)) = arena.triple(node, "at") else {
        board.warn_once("KiCad text without a valid position was omitted");
        return Ok(());
    };
    if angle.abs() > 1e-8 || pose.is_some_and(|pose| pose.angle_degrees.abs() > 1e-8) {
        board.warn_once("KiCad rotated text is shown horizontally in the preview");
    }
    let point = pose.map_or(Point { x, y }, |pose| footprint_local_point(pose, x, y));
    let size = arena
        .find_child(node, "effects")
        .and_then(|effects| arena.find_child(effects, "font"))
        .and_then(|font| arena.pair(font, "size"))
        .map_or(1.2, |(_, height)| height);
    board.add_text(text.into_owned(), point, size, &layer)
}

fn footprint_local_point(pose: FootprintPose, x: f64, y: f64) -> Point {
    // The PCB coordinate convention is Y-down. A positive stored angle is
    // counter-clockwise on the board; bottom footprints are flipped about X.
    let y = if pose.bottom { -y } else { y };
    let angle = -pose.angle_degrees.to_radians();
    let (sin, cos) = angle.sin_cos();
    Point {
        x: pose.origin.x + x * cos - y * sin,
        y: pose.origin.y + x * sin + y * cos,
    }
}

fn circle_points(center: Point, radius: f64, segments: usize) -> Vec<Point> {
    let segments = segments.max(8);
    (0..segments)
        .map(|index| {
            let angle = index as f64 * std::f64::consts::TAU / segments as f64;
            Point {
                x: center.x + radius * angle.cos(),
                y: center.y + radius * angle.sin(),
            }
        })
        .collect()
}

fn sample_arc(start: Point, mid: Point, end: Point) -> Vec<Point> {
    let determinant =
        2.0 * (start.x * (mid.y - end.y) + mid.x * (end.y - start.y) + end.x * (start.y - mid.y));
    if determinant.abs() < 1e-10 {
        return vec![start, mid, end];
    }
    let start_sq = start.x * start.x + start.y * start.y;
    let mid_sq = mid.x * mid.x + mid.y * mid.y;
    let end_sq = end.x * end.x + end.y * end.y;
    let center = Point {
        x: (start_sq * (mid.y - end.y) + mid_sq * (end.y - start.y) + end_sq * (start.y - mid.y))
            / determinant,
        y: (start_sq * (end.x - mid.x) + mid_sq * (start.x - end.x) + end_sq * (mid.x - start.x))
            / determinant,
    };
    let radius = ((start.x - center.x).powi(2) + (start.y - center.y).powi(2)).sqrt();
    if !radius.is_finite() || radius > MAX_COORDINATE_MM {
        return vec![start, mid, end];
    }
    let angle = |point: Point| (point.y - center.y).atan2(point.x - center.x);
    let start_angle = angle(start);
    let mid_angle = angle(mid);
    let end_angle = angle(end);
    let normalize = |value: f64| value.rem_euclid(std::f64::consts::TAU);
    let ccw_end = normalize(end_angle - start_angle);
    let ccw_mid = normalize(mid_angle - start_angle);
    let sweep = if ccw_mid <= ccw_end {
        ccw_end
    } else {
        ccw_end - std::f64::consts::TAU
    };
    let count = ((sweep.abs() / (std::f64::consts::PI / 12.0)).ceil() as usize).clamp(2, 144);
    (0..=count)
        .map(|index| {
            let theta = start_angle + sweep * (index as f64 / count as f64);
            Point {
                x: center.x + radius * theta.cos(),
                y: center.y + radius * theta.sin(),
            }
        })
        .collect()
}

fn path_from_points(points: &[Point], close: bool) -> String {
    let mut d = String::new();
    for (index, point) in points.iter().enumerate() {
        d.push_str(if index == 0 { "M " } else { " L " });
        d.push_str(&fmt_coord(point.x));
        d.push(' ');
        d.push_str(&fmt_coord(point.y));
    }
    if close {
        d.push_str(" Z");
    }
    d
}

fn required_point(arena: &SexpArena<'_>, node: usize, key: &str) -> Result<Point> {
    let (x, y) = arena
        .pair(node, key)
        .ok_or_else(|| Error::InvalidInput(format!("KiCad {key} point is missing or invalid")))?;
    Ok(Point { x, y })
}

fn required_layer(arena: &SexpArena<'_>, node: usize) -> Result<String> {
    arena
        .string_value(node, "layer")
        .map(Cow::into_owned)
        .filter(|layer| !layer.is_empty())
        .ok_or_else(|| Error::InvalidInput("KiCad geometry has no valid layer".into()))
}

fn graphic_width(arena: &SexpArena<'_>, node: usize) -> Option<f64> {
    arena.number_value(node, "width").or_else(|| {
        arena
            .find_child(node, "stroke")
            .and_then(|stroke| arena.number_value(stroke, "width"))
    })
}

fn is_filled(arena: &SexpArena<'_>, node: usize) -> bool {
    let Some(fill) = arena.find_child(node, "fill") else {
        return false;
    };
    let value = arena.value_text(fill, 0).unwrap_or(Cow::Borrowed(""));
    value == "yes"
        || value == "solid"
        || arena.string_value(fill, "type").as_deref() == Some("solid")
}

fn first_copper_layer(arena: &SexpArena<'_>, node: usize) -> Option<String> {
    arena
        .list_values_text(arena.find_child(node, "layers"))
        .into_iter()
        .find(|layer| layer == "*.Cu" || layer.ends_with(".Cu"))
}

fn pad_copper_layers(layers: &[String]) -> Vec<String> {
    if layers.iter().any(|layer| layer == "*.Cu") {
        return vec!["*.Cu".into()];
    }
    layers
        .iter()
        .filter_map(|layer| match layer.as_str() {
            "F.Cu" | "top_side.Cu" => Some("F.Cu".to_owned()),
            "B.Cu" | "bottom_side.Cu" => Some("B.Cu".to_owned()),
            value if value.starts_with("In") && value.ends_with(".Cu") => Some(value.to_owned()),
            _ => None,
        })
        .collect()
}

fn safe_width(width: f64) -> Option<f64> {
    (width.is_finite() && (0.0..=MAX_COORDINATE_MM).contains(&width)).then_some(width)
}

fn layer_color(layer: &str) -> &'static str {
    if layer == "F.Cu" || layer == "top_side.Cu" {
        "#c5413a"
    } else if layer == "B.Cu" || layer == "bottom_side.Cu" {
        "#357ab8"
    } else if layer.ends_with(".Cu") || layer == "*.Cu" {
        "#d08a2e"
    } else if layer.ends_with(".SilkS") {
        "#354151"
    } else if layer == "Edge.Cuts" || layer == "edge.Cuts" {
        "#111827"
    } else if layer.ends_with(".Mask") {
        "#20906a"
    } else {
        "#64748b"
    }
}

fn is_bottom_layer(layer: &str) -> bool {
    layer.starts_with("B.") || layer.starts_with("bottom_side.")
}

fn primitive_order(layer: &str, role: &str) -> u8 {
    if role == "pcb:zone_fill" {
        0
    } else if role == "pcb:track" || role == "pcb:track_arc" {
        1
    } else if role == "pcb:via" || role == "pcb:pad" {
        2
    } else if layer == "Edge.Cuts" || layer == "edge.Cuts" {
        5
    } else if role == "pcb:footprint_graphic" || layer.ends_with(".SilkS") {
        4
    } else {
        3
    }
}

fn render_board_page(board: &BoardRender, title: &str) -> Result<Page> {
    if !board.bounds.valid {
        return Err(Error::InvalidInput(
            "KiCad PCB has no valid geometric bounds".into(),
        ));
    }
    let width = board.bounds.width().max(1e-6);
    let height = board.bounds.height().max(1e-6);
    let scale = ((PAGE_WIDTH - 2.0 * PAGE_MARGIN) / width)
        .min((PAGE_HEIGHT - 2.0 * PAGE_MARGIN) / height)
        .clamp(1e-9, 10_000.0);
    let transform = [
        scale,
        0.0,
        0.0,
        scale,
        PAGE_MARGIN - board.bounds.min_x * scale,
        PAGE_MARGIN - board.bounds.min_y * scale,
    ];
    let mut page = Page::new(1, PAGE_WIDTH, PAGE_HEIGHT, "kicadpcb");
    page.title = title.to_owned();
    page.description = "2D KiCad PCB artwork and routing preview".into();
    let mut primitive_indices = (0..board.primitives.len()).collect::<Vec<_>>();
    primitive_indices.sort_by_key(|index| (board.primitives[*index].order, *index));
    for index in primitive_indices {
        let primitive = &board.primitives[index];
        page.nodes.push(Node::Path {
            id: format!("kicad-geometry-{index}"),
            d: primitive.d.clone(),
            fill_rule: primitive.fill_rule.into(),
            fill: primitive.fill.clone(),
            stroke: primitive.stroke.clone(),
            transform,
            clip_id: None,
            meta: SourceMeta {
                kind: "pcb_geometry".into(),
                source_id: primitive.layer.clone(),
                semantic_role: primitive.role.into(),
                ..SourceMeta::default()
            },
        });
    }
    for (index, text) in board.text.iter().enumerate() {
        let color = layer_color(&text.layer);
        let run = TextRun {
            text: text.text.clone(),
            font_family: "sans-serif".into(),
            font_size: text.size_mm * scale,
            fill: Paint::solid(color),
            ..TextRun::default()
        };
        page.nodes.push(Node::Text {
            id: format!("kicad-text-{index}"),
            x: text.point.x * scale + transform[4],
            y: text.point.y * scale + transform[5],
            runs: vec![run],
            anchor: TextAnchor::Middle,
            transform: IDENTITY,
            opacity: 1.0,
            stroke: Stroke::default(),
            clip_id: None,
            meta: SourceMeta {
                kind: "pcb_text".into(),
                source_id: text.layer.clone(),
                ..SourceMeta::default()
            },
        });
    }
    Ok(page)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_comments_nested_lists_and_escaped_utf8_strings() {
        let source = r#"; board preamble
(kicad_pcb (version 20250114) (property "Reference" "R\"1\nµC"))"#;
        assert!(looks_like_prefix(source.as_bytes()));
        let arena = SexpArena::parse(source).unwrap();
        let root = arena.children(0).next().unwrap();
        assert_eq!(arena.head(root).as_deref(), Some("kicad_pcb"));
        let property = arena.find_child(root, "property").unwrap();
        assert_eq!(arena.value_text(property, 0).as_deref(), Some("Reference"));
        assert_eq!(arena.value_text(property, 1).as_deref(), Some("R\"1\nµC"));
    }

    #[test]
    fn rejects_malformed_and_overscoped_sexpressions() {
        assert!(SexpArena::parse("(kicad_pcb (version 20250114)").is_err());
        assert!(SexpArena::parse("(kicad_pcb) )").is_err());
        let deeply_nested = format!(
            "{}x{}",
            "(".repeat(MAX_SEXP_DEPTH + 1),
            ")".repeat(MAX_SEXP_DEPTH + 1)
        );
        assert!(matches!(
            SexpArena::parse(&deeply_nested),
            Err(Error::LimitExceeded(_))
        ));
    }
}
