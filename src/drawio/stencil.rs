//! draw.io shape libraries, as mxStencil defines them.
//!
//! A library is an XML file of `<shape>` elements, each one a small drawing
//! program in its own coordinate space: move/line/curve/arc segments, plus
//! rectangles and ellipses, painted by `fill`, `stroke` and `fillstroke` with a
//! saveable graphics state. Interpreting that program is what draws an AWS,
//! Azure, GCP or BPMN icon exactly as the editor draws it, instead of standing
//! a labelled rectangle in its place.
//!
//! The libraries themselves are not shipped here. They come from the caller, as
//! files or directories of draw.io's own `stencils`, or from the diagram when it
//! carries a shape inline as `shape=stencil(<data>)`.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use quick_xml::Reader;
use quick_xml::events::Event;

use crate::error::{Error, Result};
use crate::ooxml::{attribute, local_name};

use super::{Rect, ellipse_path, n, parse_color, rectangle_path, rounded_rect_path};

/// Upper bound on one library file, so a stray large file cannot be read into
/// memory whole.
const MAX_LIBRARY_BYTES: u64 = 64 * 1024 * 1024;
/// Upper bound on the drawing operations one shape may hold.
const MAX_OPERATIONS: usize = 200_000;

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum StrokeWidth {
    /// `strokewidth="inherit"`: the cell's own stroke width, scaled with the
    /// shape.
    Inherit,
    /// A width in the stencil's coordinate space.
    Fixed(f64),
}

#[derive(Clone, Debug)]
pub(crate) struct Stencil {
    /// The coordinate space the operations are written in.
    pub(crate) width: f64,
    pub(crate) height: f64,
    /// `aspect="fixed"` keeps the shape's proportions and centres it in the
    /// cell; the default stretches it to fill.
    pub(crate) fixed_aspect: bool,
    pub(crate) stroke_width: StrokeWidth,
    pub(crate) operations: Vec<Operation>,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Operation {
    Save,
    Restore,
    Begin,
    Move(f64, f64),
    Line(f64, f64),
    Quad(f64, f64, f64, f64),
    Curve(f64, f64, f64, f64, f64, f64),
    Arc {
        rx: f64,
        ry: f64,
        rotation: f64,
        large_arc: bool,
        sweep: bool,
        x: f64,
        y: f64,
    },
    Close,
    Rect {
        x: f64,
        y: f64,
        width: f64,
        height: f64,
    },
    RoundRect {
        x: f64,
        y: f64,
        width: f64,
        height: f64,
        arc_size: f64,
    },
    Ellipse {
        x: f64,
        y: f64,
        width: f64,
        height: f64,
    },
    Fill,
    Stroke,
    FillStroke,
    FillColor(Option<String>),
    StrokeColor(Option<String>),
    /// `fixed` keeps the width out of the shape's scale.
    SetStrokeWidth {
        width: f64,
        fixed: bool,
    },
    Dashed(bool),
    DashPattern(Vec<f64>),
    Alpha(f64),
    FillAlpha(f64),
    StrokeAlpha(f64),
    LineCap(String),
    LineJoin(String),
    MiterLimit(f64),
}

/// The shapes a conversion has loaded, keyed the way a style names them.
#[derive(Debug, Default)]
pub(crate) struct Library {
    shapes: HashMap<String, Stencil>,
}

impl Library {
    pub(crate) fn get(&self, name: &str) -> Option<&Stencil> {
        self.shapes.get(name)
    }

    /// Read the shapes named in `wanted` out of the given files and
    /// directories, ignoring everything else they hold.
    ///
    /// A library file runs to megabytes and holds thousands of shapes, so only
    /// the ones a diagram actually asks for are kept.
    pub(crate) fn load(
        &mut self,
        sources: &[PathBuf],
        wanted: &HashSet<String>,
        max_events: usize,
    ) -> Result<()> {
        let mut outstanding = wanted
            .iter()
            .filter(|name| !self.shapes.contains_key(*name))
            .cloned()
            .collect::<HashSet<_>>();
        if outstanding.is_empty() {
            return Ok(());
        }
        for source in sources {
            for file in library_files(source)? {
                let metadata = std::fs::metadata(&file)?;
                if metadata.len() > MAX_LIBRARY_BYTES {
                    return Err(Error::LimitExceeded(format!(
                        "stencil library {} is {} bytes; maximum is {MAX_LIBRARY_BYTES} bytes",
                        file.display(),
                        metadata.len()
                    )));
                }
                let bytes = std::fs::read(&file)?;
                self.read_set(&bytes, &outstanding, max_events)?;
                // A whole shape set runs to megabytes, and a diagram usually
                // draws from one of them; there is no reason to read the rest.
                outstanding.retain(|name| !self.shapes.contains_key(name));
                if outstanding.is_empty() {
                    return Ok(());
                }
            }
        }
        Ok(())
    }

    /// Parse one `shape=stencil(...)` payload, which carries a single shape.
    pub(crate) fn insert_inline(
        &mut self,
        name: &str,
        xml: &[u8],
        max_events: usize,
    ) -> Result<()> {
        let mut wanted = HashSet::new();
        wanted.insert(String::new());
        let shapes = parse_library(xml, &wanted, max_events, true)?;
        if let Some((_, stencil)) = shapes.into_iter().next() {
            self.shapes.insert(name.to_owned(), stencil);
        }
        Ok(())
    }

    fn read_set(
        &mut self,
        bytes: &[u8],
        wanted: &HashSet<String>,
        max_events: usize,
    ) -> Result<()> {
        for (key, stencil) in parse_library(bytes, wanted, max_events, false)? {
            self.shapes.entry(key).or_insert(stencil);
        }
        Ok(())
    }
}

/// The XML files a source names: the file itself, or the files directly inside
/// a directory.
fn library_files(source: &Path) -> Result<Vec<PathBuf>> {
    let metadata = std::fs::metadata(source)?;
    if metadata.is_file() {
        return Ok(vec![source.to_path_buf()]);
    }
    if !metadata.is_dir() {
        return Err(Error::InvalidInput(format!(
            "{} is neither a stencil file nor a directory of them",
            source.display()
        )));
    }
    let mut files = Vec::new();
    for entry in std::fs::read_dir(source)? {
        let path = entry?.path();
        if path.is_file()
            && path
                .extension()
                .and_then(|value| value.to_str())
                .is_some_and(|value| value.eq_ignore_ascii_case("xml"))
        {
            files.push(path);
        }
    }
    // A directory listing has no order of its own; the output must not depend
    // on which file the file system happened to hand back first.
    files.sort();
    Ok(files)
}

/// The name a style uses for a shape: the library's name, then the shape's own,
/// folded to lower case with spaces turned into underscores.
fn registry_key(library: &str, shape: &str) -> String {
    let shape = shape.trim().to_lowercase().replace([' ', '\u{a0}'], "_");
    if library.is_empty() {
        shape
    } else {
        format!("{}.{shape}", library.trim().to_lowercase())
    }
}

fn parse_library(
    bytes: &[u8],
    wanted: &HashSet<String>,
    max_events: usize,
    take_first: bool,
) -> Result<Vec<(String, Stencil)>> {
    let mut reader = Reader::from_reader(bytes);
    reader.config_mut().trim_text(true);
    reader.config_mut().check_end_names = false;
    let mut buffer = Vec::new();
    let mut library = String::new();
    let mut found = Vec::new();
    let mut current = None::<(String, Stencil)>;
    let mut collecting = false;
    let mut events = 0usize;
    loop {
        events += 1;
        if events > max_events {
            return Err(Error::LimitExceeded(format!(
                "stencil XML exceeds {max_events} events"
            )));
        }
        let event = reader.read_event_into(&mut buffer)?;
        let (start, empty) = match &event {
            Event::Start(start) => (Some(start), false),
            Event::Empty(start) => (Some(start), true),
            Event::Eof => break,
            Event::End(end) => {
                if local_name(end.name().as_ref()) == b"shape"
                    && let Some(entry) = current.take()
                {
                    found.push(entry);
                    collecting = false;
                    if take_first {
                        break;
                    }
                }
                buffer.clear();
                continue;
            }
            _ => {
                buffer.clear();
                continue;
            }
        };
        let Some(start) = start else { continue };
        let name = local_name(start.name().as_ref()).to_vec();
        match name.as_slice() {
            b"shapes" => library = attribute(start, b"name").unwrap_or_default(),
            b"shape" => {
                let shape_name = attribute(start, b"name").unwrap_or_default();
                let key = registry_key(&library, &shape_name);
                collecting = take_first || wanted.contains(&key);
                if collecting {
                    let stencil = Stencil {
                        width: number(start, b"w", 100.0).max(1.0),
                        height: number(start, b"h", 100.0).max(1.0),
                        fixed_aspect: attribute(start, b"aspect").as_deref() == Some("fixed"),
                        stroke_width: match attribute(start, b"strokewidth").as_deref() {
                            Some("inherit") | None => StrokeWidth::Inherit,
                            Some(value) => value
                                .parse::<f64>()
                                .ok()
                                .filter(|value| value.is_finite())
                                .map_or(StrokeWidth::Inherit, StrokeWidth::Fixed),
                        },
                        operations: Vec::new(),
                    };
                    current = Some((key, stencil));
                }
                if empty && let Some(entry) = current.take() {
                    found.push(entry);
                    collecting = false;
                }
            }
            _ if collecting => {
                if let Some((_, stencil)) = current.as_mut()
                    && let Some(operation) = parse_operation(&name, start)
                {
                    if stencil.operations.len() >= MAX_OPERATIONS {
                        return Err(Error::LimitExceeded(format!(
                            "stencil shape exceeds {MAX_OPERATIONS} drawing operations"
                        )));
                    }
                    stencil.operations.push(operation);
                }
            }
            _ => {}
        }
        buffer.clear();
    }
    Ok(found)
}

fn parse_operation(name: &[u8], start: &quick_xml::events::BytesStart<'_>) -> Option<Operation> {
    Some(match name {
        b"save" => Operation::Save,
        b"restore" => Operation::Restore,
        b"path" => Operation::Begin,
        b"move" => Operation::Move(number(start, b"x", 0.0), number(start, b"y", 0.0)),
        b"line" => Operation::Line(number(start, b"x", 0.0), number(start, b"y", 0.0)),
        b"quad" => Operation::Quad(
            number(start, b"x1", 0.0),
            number(start, b"y1", 0.0),
            number(start, b"x2", 0.0),
            number(start, b"y2", 0.0),
        ),
        b"curve" => Operation::Curve(
            number(start, b"x1", 0.0),
            number(start, b"y1", 0.0),
            number(start, b"x2", 0.0),
            number(start, b"y2", 0.0),
            number(start, b"x3", 0.0),
            number(start, b"y3", 0.0),
        ),
        b"arc" => Operation::Arc {
            rx: number(start, b"rx", 0.0),
            ry: number(start, b"ry", 0.0),
            rotation: number(start, b"x-axis-rotation", 0.0),
            large_arc: attribute(start, b"large-arc-flag").as_deref() == Some("1"),
            sweep: attribute(start, b"sweep-flag").as_deref() == Some("1"),
            x: number(start, b"x", 0.0),
            y: number(start, b"y", 0.0),
        },
        b"close" => Operation::Close,
        b"rect" => Operation::Rect {
            x: number(start, b"x", 0.0),
            y: number(start, b"y", 0.0),
            width: number(start, b"w", 0.0),
            height: number(start, b"h", 0.0),
        },
        b"roundrect" => Operation::RoundRect {
            x: number(start, b"x", 0.0),
            y: number(start, b"y", 0.0),
            width: number(start, b"w", 0.0),
            height: number(start, b"h", 0.0),
            arc_size: number(start, b"arcsize", 0.0),
        },
        b"ellipse" => Operation::Ellipse {
            x: number(start, b"x", 0.0),
            y: number(start, b"y", 0.0),
            width: number(start, b"w", 0.0),
            height: number(start, b"h", 0.0),
        },
        b"fill" => Operation::Fill,
        b"stroke" => Operation::Stroke,
        b"fillstroke" => Operation::FillStroke,
        b"fillcolor" => Operation::FillColor(attribute(start, b"color")),
        b"strokecolor" => Operation::StrokeColor(attribute(start, b"color")),
        b"strokewidth" => Operation::SetStrokeWidth {
            width: number(start, b"width", 1.0),
            fixed: attribute(start, b"fixed").as_deref() == Some("1"),
        },
        b"dashed" => Operation::Dashed(attribute(start, b"dashed").as_deref() == Some("1")),
        b"dashpattern" => Operation::DashPattern(
            attribute(start, b"pattern")
                .unwrap_or_default()
                .split_whitespace()
                .filter_map(|value| value.parse::<f64>().ok())
                .filter(|value| value.is_finite() && *value >= 0.0)
                .collect(),
        ),
        b"alpha" => Operation::Alpha(number(start, b"alpha", 1.0)),
        b"fillalpha" => Operation::FillAlpha(number(start, b"alpha", 1.0)),
        b"strokealpha" => Operation::StrokeAlpha(number(start, b"alpha", 1.0)),
        b"linecap" => Operation::LineCap(attribute(start, b"cap").unwrap_or_default()),
        b"linejoin" => Operation::LineJoin(attribute(start, b"join").unwrap_or_default()),
        b"miterlimit" => Operation::MiterLimit(number(start, b"limit", 4.0)),
        // `connections`, `background`, `foreground`, `text`, `image` and
        // `include-shape` carry no geometry this renderer draws.
        _ => return None,
    })
}

fn number(start: &quick_xml::events::BytesStart<'_>, name: &[u8], default: f64) -> f64 {
    attribute(start, name)
        .and_then(|value| value.trim().parse::<f64>().ok())
        .filter(|value| value.is_finite())
        .map_or(default, |value| value.clamp(-1e7, 1e7))
}

// ---------------------------------------------------------------------------
// Drawing
// ---------------------------------------------------------------------------

use crate::ir::{IDENTITY, LineCap, LineJoin, Node, Paint, SourceMeta, Stroke};

/// What the cell's own style contributes: a stencil inherits the fill, the
/// stroke and the opacity from the shape it is drawn for.
pub(crate) struct Inherited {
    pub(crate) fill: Option<String>,
    pub(crate) stroke: Option<String>,
    pub(crate) stroke_width: f64,
    pub(crate) opacity: f64,
    pub(crate) fill_opacity: f64,
    pub(crate) stroke_opacity: f64,
    pub(crate) dash: Vec<f64>,
}

#[derive(Clone)]
struct State {
    fill: Option<String>,
    stroke: Option<String>,
    stroke_width: f64,
    dash: Vec<f64>,
    alpha: f64,
    fill_alpha: f64,
    stroke_alpha: f64,
    cap: LineCap,
    join: LineJoin,
    miter: f64,
}

impl Stencil {
    /// Draw the shape into `rect`.
    ///
    /// Coordinates are mapped from the stencil's own space the way
    /// `mxStencil.computeAspect` maps them: stretched to the cell, or scaled
    /// evenly and centred when the shape declares a fixed aspect.
    pub(crate) fn render(&self, id: &str, rect: Rect, inherited: &Inherited) -> Vec<Node> {
        let mut scale_x = rect.width / self.width;
        let mut scale_y = rect.height / self.height;
        let (mut origin_x, mut origin_y) = (rect.x, rect.y);
        if self.fixed_aspect {
            let scale = scale_x.min(scale_y);
            scale_x = scale;
            scale_y = scale;
            origin_x += (rect.width - self.width * scale) / 2.0;
            origin_y += (rect.height - self.height * scale) / 2.0;
        }
        let smallest = scale_x.min(scale_y);
        let point = |x: f64, y: f64| (origin_x + x * scale_x, origin_y + y * scale_y);
        let base = State {
            fill: inherited.fill.clone(),
            stroke: inherited.stroke.clone(),
            stroke_width: match self.stroke_width {
                StrokeWidth::Inherit => inherited.stroke_width,
                StrokeWidth::Fixed(width) => width * smallest,
            },
            dash: inherited.dash.clone(),
            alpha: inherited.opacity,
            fill_alpha: inherited.fill_opacity,
            stroke_alpha: inherited.stroke_opacity,
            cap: LineCap::Butt,
            join: LineJoin::Miter,
            miter: 4.0,
        };
        let mut state = base.clone();
        let mut stack: Vec<State> = Vec::new();
        let mut path = String::new();
        let mut nodes = Vec::new();
        for operation in &self.operations {
            match operation {
                Operation::Save => stack.push(state.clone()),
                Operation::Restore => {
                    if let Some(previous) = stack.pop() {
                        state = previous;
                    }
                }
                Operation::Begin => path.clear(),
                Operation::Move(x, y) => {
                    let (x, y) = point(*x, *y);
                    push_segment(&mut path, &format!("M {} {}", n(x), n(y)));
                }
                Operation::Line(x, y) => {
                    let (x, y) = point(*x, *y);
                    push_segment(&mut path, &format!("L {} {}", n(x), n(y)));
                }
                Operation::Quad(x1, y1, x2, y2) => {
                    let (cx, cy) = point(*x1, *y1);
                    let (x, y) = point(*x2, *y2);
                    push_segment(
                        &mut path,
                        &format!("Q {} {} {} {}", n(cx), n(cy), n(x), n(y)),
                    );
                }
                Operation::Curve(x1, y1, x2, y2, x3, y3) => {
                    let (ax, ay) = point(*x1, *y1);
                    let (bx, by) = point(*x2, *y2);
                    let (x, y) = point(*x3, *y3);
                    push_segment(
                        &mut path,
                        &format!(
                            "C {} {} {} {} {} {}",
                            n(ax),
                            n(ay),
                            n(bx),
                            n(by),
                            n(x),
                            n(y)
                        ),
                    );
                }
                Operation::Arc {
                    rx,
                    ry,
                    rotation,
                    large_arc,
                    sweep,
                    x,
                    y,
                } => {
                    let (x, y) = point(*x, *y);
                    push_segment(
                        &mut path,
                        &format!(
                            "A {} {} {} {} {} {} {}",
                            n(rx * scale_x),
                            n(ry * scale_y),
                            n(*rotation),
                            u8::from(*large_arc),
                            u8::from(*sweep),
                            n(x),
                            n(y)
                        ),
                    );
                }
                Operation::Close => push_segment(&mut path, "Z"),
                Operation::Rect {
                    x,
                    y,
                    width,
                    height,
                } => {
                    let (x, y) = point(*x, *y);
                    path = rectangle_path(Rect {
                        x,
                        y,
                        width: width * scale_x,
                        height: height * scale_y,
                    });
                }
                Operation::RoundRect {
                    x,
                    y,
                    width,
                    height,
                    arc_size,
                } => {
                    let (x, y) = point(*x, *y);
                    let (width, height) = (width * scale_x, height * scale_y);
                    // mxStencil reads a zero arc size as the default rounding.
                    let factor = if *arc_size <= 0.0 {
                        0.15
                    } else {
                        arc_size / 100.0
                    };
                    path = rounded_rect_path(
                        Rect {
                            x,
                            y,
                            width,
                            height,
                        },
                        (width * factor).min(height * factor),
                    );
                }
                Operation::Ellipse {
                    x,
                    y,
                    width,
                    height,
                } => {
                    let (x, y) = point(*x, *y);
                    path = ellipse_path(Rect {
                        x,
                        y,
                        width: width * scale_x,
                        height: height * scale_y,
                    });
                }
                Operation::Fill | Operation::Stroke | Operation::FillStroke => {
                    if !path.is_empty() {
                        let filled = matches!(operation, Operation::Fill | Operation::FillStroke);
                        let stroked =
                            matches!(operation, Operation::Stroke | Operation::FillStroke);
                        nodes.push(paint_node(
                            &format!("{id}-{}", nodes.len()),
                            std::mem::take(&mut path),
                            &state,
                            filled,
                            stroked,
                        ));
                    }
                }
                Operation::FillColor(color) => {
                    state.fill = color.as_deref().and_then(parse_color);
                }
                Operation::StrokeColor(color) => {
                    state.stroke = color.as_deref().and_then(parse_color);
                }
                Operation::SetStrokeWidth { width, fixed } => {
                    state.stroke_width = width * if *fixed { 1.0 } else { smallest };
                }
                Operation::Dashed(dashed) => {
                    state.dash = if *dashed {
                        let scale = state.stroke_width.max(1.0);
                        vec![3.0 * scale, 3.0 * scale]
                    } else {
                        Vec::new()
                    };
                }
                Operation::DashPattern(pattern) => {
                    if !pattern.is_empty() {
                        let scale = state.stroke_width.max(1.0);
                        state.dash = pattern.iter().map(|value| value * scale).collect();
                    }
                }
                Operation::Alpha(alpha) => state.alpha = alpha.clamp(0.0, 1.0),
                Operation::FillAlpha(alpha) => state.fill_alpha = alpha.clamp(0.0, 1.0),
                Operation::StrokeAlpha(alpha) => state.stroke_alpha = alpha.clamp(0.0, 1.0),
                Operation::LineCap(cap) => {
                    state.cap = match cap.as_str() {
                        "round" => LineCap::Round,
                        "square" => LineCap::Square,
                        _ => LineCap::Butt,
                    };
                }
                Operation::LineJoin(join) => {
                    state.join = match join.as_str() {
                        "round" => LineJoin::Round,
                        "bevel" => LineJoin::Bevel,
                        _ => LineJoin::Miter,
                    };
                }
                Operation::MiterLimit(limit) => state.miter = limit.max(1.0),
            }
        }
        nodes
    }
}

fn push_segment(path: &mut String, segment: &str) {
    if !path.is_empty() {
        path.push(' ');
    }
    path.push_str(segment);
}

fn paint_node(id: &str, d: String, state: &State, filled: bool, stroked: bool) -> Node {
    let fill = match (filled, state.fill.as_deref()) {
        (true, Some(color)) => Paint::Solid {
            color: color.to_owned(),
            opacity: state.alpha * state.fill_alpha,
        },
        _ => Paint::None,
    };
    let stroke = match (stroked, state.stroke.as_deref()) {
        (true, Some(color)) if state.stroke_width > 0.0 => Stroke {
            paint: Paint::Solid {
                color: color.to_owned(),
                opacity: state.alpha * state.stroke_alpha,
            },
            width: state.stroke_width,
            line_cap: state.cap,
            line_join: state.join,
            miter_limit: state.miter,
            dash_array: state.dash.clone(),
            dash_offset: 0.0,
        },
        _ => Stroke::default(),
    };
    Node::Path {
        id: id.to_owned(),
        d,
        fill_rule: "nonzero".into(),
        fill,
        stroke,
        transform: IDENTITY,
        clip_id: None,
        meta: SourceMeta {
            kind: "drawio-stencil".into(),
            ..SourceMeta::default()
        },
    }
}
