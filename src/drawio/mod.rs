//! draw.io (diagrams.net) mxGraphModel documents to Page IR.
//!
//! A `.drawio` file is an `mxfile` wrapper around one `diagram` element per
//! page. The diagram body is either plain `mxGraphModel` XML or the same XML
//! run through `encodeURIComponent`, raw DEFLATE and base64, which is what the
//! editor writes by default. Each diagram becomes one output page.
//!
//! Model coordinates are CSS pixels with the origin at the top-left. Everything
//! in this module stays in pixels; the page's root group carries the single
//! pixel-to-point scale and the crop offset, so shape geometry, stroke widths
//! and font sizes all scale together.

// The reader is split by the job each part does: the model and the page come
// together here, and each of these draws one kind of thing.
mod artwork;
mod bpmn;
mod edge;
mod geometry;
mod label;
mod shapes;
mod stencil;

use artwork::*;
use bpmn::*;
use edge::*;
use geometry::*;
use label::*;
use shapes::*;

use std::collections::HashMap;
use std::f64::consts::PI;
use std::io::Read;
use std::path::Path;

use base64::Engine;
use quick_xml::Reader;
use quick_xml::events::Event;

use crate::convert::{ConvertOptions, PageConsumer};
use crate::error::{Error, Result};
use crate::ir::{
    ClipPath, GradientStop, IDENTITY, LineCap, LineJoin, LinearGradient, Matrix, Node, Page, Paint,
    SourceMeta, Stroke, TextAnchor, TextRun, compose,
};
use crate::ooxml::{attribute, local_name, text_advance_factor};

/// draw.io lays out in CSS pixels at 96 dpi; the IR and the SVG writer use
/// points, so a 100-pixel box has to arrive as 75 points to keep its real size.
const POINTS_PER_PIXEL: f64 = 0.75;
/// Blank margin kept around the cropped drawing, in pixels.
const PAGE_MARGIN: f64 = 10.0;
/// `mxConstants.DEFAULT_FONTSIZE`.
const DEFAULT_FONT_SIZE: f64 = 12.0;
/// mxGraph draws label lines at 1.2 times the font size.
const LINE_HEIGHT: f64 = 1.2;
/// `mxConstants.DEFAULT_MARKERSIZE`.
const DEFAULT_MARKER_SIZE: f64 = 6.0;
/// `mxConstants.LINE_ARCSIZE / 2`, the corner radius of a rounded connector.
const CONNECTOR_ARC: f64 = 10.0;
/// `mxEdgeStyle.orthBuffer`: how far an orthogonal route leaves a shape before
/// it is allowed to turn.
const ORTH_BUFFER: f64 = 10.0;
/// `mxConstants.SHADOW_OFFSET_X`, `SHADOW_OFFSET_Y` and `SHADOWCOLOR`. draw.io
/// draws a shadow as a solid grey copy of the shape, not as a blur.
const SHADOW_OFFSET: (f64, f64) = (2.0, 3.0);
const SHADOW_COLOR: &str = "#808080";
/// Default label padding, `mxConstants.DEFAULT_SPACING`-equivalent in drawio.
const LABEL_SPACING: f64 = 2.0;
/// Guard against a malformed model whose parent links form a cycle.
const MAX_PARENT_DEPTH: usize = 64;
/// Upper bound on one embedded picture's encoded bytes.
const MAX_IMAGE_CHARS: usize = 16 * 1024 * 1024;

pub(crate) fn convert(
    path: &Path,
    options: &ConvertOptions,
    sink: &mut dyn PageConsumer,
) -> Result<Vec<String>> {
    let bytes = std::fs::read(path)?;
    let (diagrams, root) = split_diagrams(&bytes, options)?;
    if diagrams.is_empty() {
        // Naming what the file actually is saves the caller from guessing: a
        // shape library and a template index are both XML a draw.io user has
        // on hand, and neither one has pages.
        return Err(match root.as_str() {
            "mxlibrary" => Error::Unsupported(
                "input is a draw.io shape library, not a diagram; it holds shapes to place, not pages to convert".into(),
            ),
            "" => Error::InvalidInput(
                "input has no XML root element; expected an mxfile with at least one diagram, or a bare mxGraphModel".into(),
            ),
            other => Error::InvalidInput(format!(
                "input is XML with a <{other}> root; expected an mxfile with at least one diagram, or a bare mxGraphModel"
            )),
        });
    }
    if diagrams.len() > options.max_pages {
        return Err(Error::LimitExceeded(format!(
            "drawio file contains {} diagrams; maximum is {}",
            diagrams.len(),
            options.max_pages
        )));
    }
    let mut warnings = Vec::new();
    let mut library = stencil::Library::default();
    for (index, diagram) in diagrams.into_iter().enumerate() {
        let xml = diagram.decode(options)?;
        let model = parse_model(&xml, options.max_xml_events)?;
        load_stencils(&model, &mut library, options)?;
        let mut page = render(&model, index + 1, &diagram.name, &mut library);
        if options.embed_drawio_source {
            page.embedded_source = Some(diagram.as_mxfile(&xml));
        }
        for warning in &page.warnings {
            if !warnings.contains(warning) {
                warnings.push(warning.clone());
            }
        }
        sink.consume(page)?;
    }
    Ok(warnings)
}

// ---------------------------------------------------------------------------
// mxfile container
// ---------------------------------------------------------------------------

struct Diagram {
    id: String,
    name: String,
    /// Raw bytes between `<diagram>` and `</diagram>`: either the nested
    /// `mxGraphModel` element or the compressed, base64-encoded body.
    body: Vec<u8>,
}

impl Diagram {
    fn decode(&self, options: &ConvertOptions) -> Result<Vec<u8>> {
        let trimmed = self.body.trim_ascii();
        if trimmed.is_empty() {
            return Err(Error::InvalidInput("drawio diagram is empty".into()));
        }
        if trimmed.starts_with(b"<") {
            return Ok(trimmed.to_vec());
        }
        let packed = base64::engine::general_purpose::STANDARD
            .decode(strip_whitespace(trimmed))
            .map_err(|error| {
                Error::InvalidInput(format!("drawio diagram is not valid base64: {error}"))
            })?;
        let limit = options.max_zip_entry_bytes;
        let mut inflated = Vec::new();
        // The editor stores the body as a raw DEFLATE stream with no zlib
        // header, so this has to be the headerless decoder.
        flate2::read::DeflateDecoder::new(packed.as_slice())
            .take(limit.saturating_add(1))
            .read_to_end(&mut inflated)
            .map_err(|error| {
                Error::InvalidInput(format!("drawio diagram is not deflate data: {error}"))
            })?;
        if inflated.len() as u64 > limit {
            return Err(Error::LimitExceeded(format!(
                "drawio diagram expands past {limit} bytes"
            )));
        }
        let text = String::from_utf8(inflated).map_err(|error| {
            Error::InvalidInput(format!("drawio diagram is not UTF-8: {error}"))
        })?;
        // The editor URI-encodes the XML before compressing it. A body that
        // already starts with an element was written by a tool that skipped
        // that step, so decoding it again would corrupt literal percent signs.
        let decoded = if text.trim_start().starts_with('<') {
            text
        } else {
            percent_decode(&text)?
        };
        Ok(decoded.into_bytes())
    }
}

/// The diagrams in an `mxfile`, plus the name of the document's root element
/// so the caller can say what a file without diagrams actually is.
fn split_diagrams(bytes: &[u8], options: &ConvertOptions) -> Result<(Vec<Diagram>, String)> {
    let mut reader = Reader::from_reader(bytes);
    // Byte offsets are only contiguous while whitespace is still reported, and
    // the offsets are what delimit each diagram body.
    reader.config_mut().trim_text(false);
    let mut buffer = Vec::new();
    let mut diagrams = Vec::new();
    let mut open: Option<(String, String, usize)> = None;
    let mut depth = 0usize;
    let mut saw_model = false;
    let mut root = String::new();
    let mut events = 0usize;
    loop {
        let position = usize::try_from(reader.buffer_position()).unwrap_or(usize::MAX);
        events += 1;
        if events > options.max_xml_events {
            return Err(Error::LimitExceeded(format!(
                "drawio XML exceeds {} events",
                options.max_xml_events
            )));
        }
        match reader.read_event_into(&mut buffer)? {
            Event::Start(start) => {
                let name = local_name(start.name().as_ref()).to_vec();
                if root.is_empty() {
                    root = String::from_utf8_lossy(&name).into_owned();
                }
                if name == b"mxGraphModel" {
                    saw_model = true;
                }
                if name == b"diagram" && open.is_none() {
                    let identity = attribute(&start, b"id").unwrap_or_default();
                    let label = attribute(&start, b"name").unwrap_or_default();
                    open = Some((
                        identity,
                        label,
                        usize::try_from(reader.buffer_position()).unwrap_or(usize::MAX),
                    ));
                    depth = 0;
                } else if open.is_some() {
                    depth += 1;
                }
            }
            Event::Empty(start) => {
                if root.is_empty() {
                    root = String::from_utf8_lossy(local_name(start.name().as_ref())).into_owned();
                }
            }
            Event::End(end) => {
                if local_name(end.name().as_ref()) == b"diagram" && depth == 0 {
                    if let Some((id, name, start_offset)) = open.take() {
                        let body = bytes
                            .get(start_offset..position)
                            .unwrap_or_default()
                            .to_vec();
                        diagrams.push(Diagram { id, name, body });
                    }
                } else if open.is_some() {
                    depth = depth.saturating_sub(1);
                }
            }
            Event::Eof => break,
            _ => {}
        }
        buffer.clear();
    }
    if diagrams.is_empty() && saw_model {
        // A bare mxGraphModel document, which is what older exports and many
        // scripted producers write.
        diagrams.push(Diagram {
            id: String::new(),
            name: String::new(),
            body: bytes.to_vec(),
        });
    }
    Ok((diagrams, root))
}

fn strip_whitespace(bytes: &[u8]) -> Vec<u8> {
    bytes
        .iter()
        .copied()
        .filter(|byte| !byte.is_ascii_whitespace())
        .collect()
}

/// Undo `encodeURIComponent`. Shared with the reverse conversion, which finds
/// the same encoding in the `content` attribute of older draw.io SVG exports.
pub(crate) fn percent_decode(text: &str) -> Result<String> {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0usize;
    while index < bytes.len() {
        if bytes[index] == b'%' && index + 2 < bytes.len() {
            let high = (bytes[index + 1] as char).to_digit(16);
            let low = (bytes[index + 2] as char).to_digit(16);
            if let (Some(high), Some(low)) = (high, low) {
                out.push((high * 16 + low) as u8);
                index += 3;
                continue;
            }
        }
        out.push(bytes[index]);
        index += 1;
    }
    String::from_utf8(out).map_err(|error| {
        Error::InvalidInput(format!(
            "drawio diagram is not UTF-8 after decoding: {error}"
        ))
    })
}

// ---------------------------------------------------------------------------
// mxGraphModel
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, Default)]
struct Geometry {
    x: f64,
    y: f64,
    width: f64,
    height: f64,
    relative: bool,
    offset: Option<(f64, f64)>,
    source_point: Option<(f64, f64)>,
    target_point: Option<(f64, f64)>,
    points: Vec<(f64, f64)>,
}

#[derive(Clone, Debug, Default)]
struct Cell {
    id: String,
    parent: String,
    style: Style,
    label: String,
    /// True when the label came from an `object`/`UserObject` wrapper, whose
    /// text is always HTML regardless of the cell's `html` style.
    label_is_html: bool,
    vertex: bool,
    edge: bool,
    visible: bool,
    source: String,
    target: String,
    geometry: Geometry,
}

#[derive(Clone, Debug, Default)]
struct Model {
    cells: Vec<Cell>,
    background: Option<String>,
    /// The page size the model declares, which is the only size a diagram with
    /// nothing on it has to offer.
    page: Option<(f64, f64)>,
}

fn parse_model(xml: &[u8], max_events: usize) -> Result<Model> {
    let mut reader = Reader::from_reader(xml);
    reader.config_mut().trim_text(true);
    let mut buffer = Vec::new();
    let mut model = Model::default();
    let mut saw_model = false;
    let mut current = None::<Cell>;
    let mut wrapper = None::<(String, String)>;
    let mut in_geometry = false;
    let mut in_points = false;
    let mut events = 0usize;
    loop {
        events += 1;
        if events > max_events {
            return Err(Error::LimitExceeded(format!(
                "drawio model XML exceeds {max_events} events"
            )));
        }
        let event = reader.read_event_into(&mut buffer)?;
        let (start, is_empty) = match &event {
            Event::Start(start) => (Some(start), false),
            Event::Empty(start) => (Some(start), true),
            Event::End(_) | Event::Eof => (None, false),
            _ => {
                buffer.clear();
                continue;
            }
        };
        if let Some(start) = start {
            let name = local_name(start.name().as_ref()).to_vec();
            match name.as_slice() {
                b"mxGraphModel" => {
                    saw_model = true;
                    model.background = attribute(start, b"background")
                        .filter(|value| !value.eq_ignore_ascii_case("none") && !value.is_empty());
                    let side = |name: &[u8]| {
                        attribute(start, name)
                            .and_then(|value| value.trim().parse::<f64>().ok())
                            .filter(|value| value.is_finite() && *value > 0.0)
                            .map(bounded)
                    };
                    model.page = side(b"pageWidth").zip(side(b"pageHeight"));
                }
                b"object" | b"UserObject" => {
                    let label = attribute(start, b"label").unwrap_or_default();
                    let id = attribute(start, b"id").unwrap_or_default();
                    wrapper = Some((id, label));
                }
                b"mxCell" => {
                    let mut cell = Cell {
                        id: attribute(start, b"id").unwrap_or_default(),
                        parent: attribute(start, b"parent").unwrap_or_default(),
                        style: Style::parse(&attribute(start, b"style").unwrap_or_default()),
                        label: attribute(start, b"value").unwrap_or_default(),
                        label_is_html: false,
                        vertex: attribute(start, b"vertex").as_deref() == Some("1"),
                        edge: attribute(start, b"edge").as_deref() == Some("1"),
                        visible: attribute(start, b"visible").as_deref() != Some("0"),
                        source: attribute(start, b"source").unwrap_or_default(),
                        target: attribute(start, b"target").unwrap_or_default(),
                        geometry: Geometry::default(),
                    };
                    if let Some((id, label)) = wrapper.take() {
                        // The wrapper owns the identity and the text; the inner
                        // mxCell only carries geometry and style.
                        if !id.is_empty() {
                            cell.id = id;
                        }
                        cell.label = label;
                        cell.label_is_html = true;
                    }
                    if is_empty {
                        model.cells.push(cell);
                    } else {
                        current = Some(cell);
                    }
                }
                b"mxGeometry" => {
                    if let Some(cell) = current.as_mut() {
                        cell.geometry = Geometry {
                            x: number(start, b"x", 0.0),
                            y: number(start, b"y", 0.0),
                            width: number(start, b"width", 0.0),
                            height: number(start, b"height", 0.0),
                            relative: attribute(start, b"relative").as_deref() == Some("1"),
                            ..Geometry::default()
                        };
                        in_geometry = !is_empty;
                    }
                }
                b"Array" => {
                    in_points = attribute(start, b"as").as_deref() == Some("points");
                }
                b"mxPoint" => {
                    if in_geometry && let Some(cell) = current.as_mut() {
                        let point = (number(start, b"x", 0.0), number(start, b"y", 0.0));
                        match attribute(start, b"as").as_deref() {
                            Some("sourcePoint") => cell.geometry.source_point = Some(point),
                            Some("targetPoint") => cell.geometry.target_point = Some(point),
                            Some("offset") => cell.geometry.offset = Some(point),
                            _ if in_points => cell.geometry.points.push(point),
                            _ => {}
                        }
                    }
                }
                _ => {}
            }
        } else {
            match &event {
                Event::End(end) => match local_name(end.name().as_ref()) {
                    b"mxCell" => {
                        if let Some(cell) = current.take() {
                            model.cells.push(cell);
                        }
                    }
                    b"mxGeometry" => in_geometry = false,
                    b"Array" => in_points = false,
                    b"object" | b"UserObject" => wrapper = None,
                    _ => {}
                },
                Event::Eof => break,
                _ => {}
            }
        }
        buffer.clear();
    }
    if let Some(cell) = current.take() {
        model.cells.push(cell);
    }
    if !saw_model {
        return Err(Error::InvalidInput(
            "input is XML but not a drawio document; no mxGraphModel element was found".into(),
        ));
    }
    Ok(model)
}

fn number(start: &quick_xml::events::BytesStart<'_>, name: &[u8], default: f64) -> f64 {
    attribute(start, name)
        .and_then(|value| value.trim().parse::<f64>().ok())
        .filter(|value| value.is_finite())
        .map_or(default, bounded)
}

// ---------------------------------------------------------------------------
// mxStyle
// ---------------------------------------------------------------------------

/// A parsed mxGraph style string: `name;key=value;key=value`.
///
/// The leading bare token, when there is one, is the shape name. Later
/// assignments win, which is how drawio's own style editor overrides a base
/// style.
#[derive(Clone, Debug, Default)]
struct Style {
    values: HashMap<String, String>,
    bare: Vec<String>,
}

impl Style {
    fn parse(text: &str) -> Self {
        let mut style = Self::default();
        for token in text.split(';') {
            let token = token.trim();
            if token.is_empty() {
                continue;
            }
            if let Some((key, value)) = token.split_once('=') {
                style
                    .values
                    .insert(key.trim().to_ascii_lowercase(), value.trim().to_owned());
            } else {
                // A bare token names a style in the stylesheet. mxGraph merges
                // that style's own settings in at this point, which is how
                // `plain-yellow` gives a shape its fill, its border and its
                // gradient, and later settings still win over it.
                if let Some(named) = named_style(token) {
                    for (key, value) in named {
                        style.values.insert((*key).to_owned(), (*value).to_owned());
                    }
                }
                style.bare.push(token.to_owned());
            }
        }
        style
    }

    fn get(&self, key: &str) -> Option<&str> {
        // Keys are folded to lower case when the style is parsed, so a lookup
        // spelled the way draw.io writes it would silently never match.
        debug_assert!(
            key.chars().all(|character| !character.is_uppercase()),
            "style key {key} must be lower case"
        );
        self.values.get(key).map(String::as_str)
    }

    fn text(&self, key: &str, default: &str) -> String {
        self.get(key).unwrap_or(default).to_owned()
    }

    fn number(&self, key: &str, default: f64) -> f64 {
        self.get(key)
            .and_then(|value| value.trim().parse::<f64>().ok())
            .filter(|value| value.is_finite())
            .map_or(default, bounded)
    }

    fn flag(&self, key: &str) -> bool {
        matches!(self.get(key), Some("1") | Some("true"))
    }

    fn has_bare(&self, name: &str) -> bool {
        self.bare.iter().any(|token| token == name)
    }

    /// The shape this style asks for, and the name the file spells it with.
    ///
    /// An explicit `shape=` wins. Otherwise mxGraph looks each bare token up in
    /// the stylesheet's named styles and ignores the ones it does not know, so
    /// a leading `ellipse` selects a shape while the stray `9E9E9E` left behind
    /// by a bad edit selects nothing.
    fn shape(&self) -> Shape {
        if let Some(declared) = self.get("shape") {
            return match canonical_shape(&declared.to_ascii_lowercase()) {
                Some(drawn) => Shape {
                    drawn,
                    unknown: None,
                },
                // A shape this module does not draw still gets the rectangle's
                // geometry, so it keeps the style's rounding and its fill.
                None => Shape {
                    drawn: "rectangle",
                    unknown: Some(declared.to_owned()),
                },
            };
        }
        Shape {
            drawn: self
                .bare
                .iter()
                .find_map(|token| canonical_shape(&token.to_ascii_lowercase()))
                .unwrap_or("rectangle"),
            unknown: None,
        }
    }

    /// A colour value, or `None` for `none`, an empty value and anything the
    /// SVG writer could not use verbatim.
    fn color(&self, key: &str) -> Option<String> {
        parse_color(self.get(key)?)
    }
}

/// The settings a named style carries, from draw.io's default stylesheet.
///
/// A style string may name one of these instead of spelling its settings out;
/// the editor ships them in `styles/default.xml` and applies them the same way.
/// Keys are already folded to lower case, the way [`Style::parse`] stores them.
fn named_style(name: &str) -> Option<&'static [(&'static str, &'static str)]> {
    Some(match name {
        "text" => &[
            ("fillcolor", "none"),
            ("gradientcolor", "none"),
            ("strokecolor", "none"),
            ("align", "left"),
            ("verticalalign", "top"),
        ],
        "label" => &[
            ("fontstyle", "1"),
            ("align", "left"),
            ("verticalalign", "middle"),
            ("spacing", "2"),
            ("spacingleft", "52"),
            ("imagewidth", "42"),
            ("imageheight", "42"),
            ("rounded", "1"),
        ],
        "swimlane" => &[("fontsize", "12"), ("fontstyle", "1"), ("startsize", "23")],
        "group" => &[
            ("verticalalign", "top"),
            ("fillcolor", "none"),
            ("strokecolor", "none"),
            ("gradientcolor", "none"),
        ],
        "ellipse" => &[("perimeter", "ellipsePerimeter")],
        "rhombus" => &[("perimeter", "rhombusPerimeter")],
        "triangle" => &[("perimeter", "trianglePerimeter")],
        "line" => &[
            ("strokewidth", "4"),
            ("verticalalign", "top"),
            ("spacingtop", "8"),
        ],
        "image" => &[
            ("verticalalign", "top"),
            ("verticallabelposition", "bottom"),
        ],
        "arrow" => &[("edgestyle", "none")],
        "fancy" => &[("shadow", "1"), ("glass", "1")],
        "plain-gray" => &[
            ("gradientcolor", "#B3B3B3"),
            ("fillcolor", "#F5F5F5"),
            ("strokecolor", "#666666"),
        ],
        "plain-blue" => &[
            ("gradientcolor", "#7EA6E0"),
            ("fillcolor", "#DAE8FC"),
            ("strokecolor", "#6C8EBF"),
        ],
        "plain-green" => &[
            ("gradientcolor", "#97D077"),
            ("fillcolor", "#D5E8D4"),
            ("strokecolor", "#82B366"),
        ],
        "plain-turquoise" => &[
            ("gradientcolor", "#67AB9F"),
            ("fillcolor", "#D5E8D4"),
            ("strokecolor", "#6A9153"),
        ],
        "plain-yellow" => &[
            ("gradientcolor", "#FFD966"),
            ("fillcolor", "#FFF2CC"),
            ("strokecolor", "#D6B656"),
        ],
        "plain-orange" => &[
            ("gradientcolor", "#FFA500"),
            ("fillcolor", "#FFCD28"),
            ("strokecolor", "#D79B00"),
        ],
        "plain-red" => &[
            ("gradientcolor", "#EA6B66"),
            ("fillcolor", "#F8CECC"),
            ("strokecolor", "#B85450"),
        ],
        "plain-pink" => &[
            ("gradientcolor", "#B5739D"),
            ("fillcolor", "#E6D0DE"),
            ("strokecolor", "#996185"),
        ],
        "plain-purple" => &[
            ("gradientcolor", "#8C6C9C"),
            ("fillcolor", "#E1D5E7"),
            ("strokecolor", "#9673A6"),
        ],
        _ => return None,
    })
}

/// What a style asks to be drawn as.
struct Shape {
    /// The shape this module draws. A style that names nothing it knows falls
    /// back to the rectangle every mxGraph vertex starts as.
    drawn: &'static str,
    /// The `shape=` value as written, when it named a shape this module does
    /// not draw. A bare token that named nothing is not reported: mxGraph looks
    /// those up in its stylesheet and ignores the ones it does not find.
    unknown: Option<String>,
}

fn parse_color(value: &str) -> Option<String> {
    let value = value.trim();
    if value.is_empty()
        || value.eq_ignore_ascii_case("none")
        || value.eq_ignore_ascii_case("default")
        || value.eq_ignore_ascii_case("inherit")
        || value.eq_ignore_ascii_case("transparent")
    {
        return None;
    }
    let hex = value.trim_start_matches('#');
    if (hex.len() == 3 || hex.len() == 6) && hex.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Some(format!("#{}", hex.to_ascii_uppercase()));
    }
    // Named CSS colours occur in hand-written styles; pass through the ones
    // that are safe identifiers and let anything else fall back to the default.
    if value.bytes().all(|byte| byte.is_ascii_alphabetic()) && value.len() <= 24 {
        return Some(value.to_ascii_lowercase());
    }
    None
}

// ---------------------------------------------------------------------------
// Page assembly
// ---------------------------------------------------------------------------

/// Absolute geometry for every cell, resolved through the parent chain.
struct Scene<'a> {
    cells: &'a [Cell],
    index: HashMap<&'a str, usize>,
    origins: HashMap<usize, (f64, f64)>,
}

impl<'a> Scene<'a> {
    fn new(cells: &'a [Cell]) -> Self {
        let index = cells
            .iter()
            .enumerate()
            .map(|(position, cell)| (cell.id.as_str(), position))
            .collect();
        let mut scene = Self {
            cells,
            index,
            origins: HashMap::new(),
        };
        for position in 0..cells.len() {
            let origin = scene.resolve_origin(position, 0);
            scene.origins.insert(position, origin);
        }
        scene
    }

    fn parent_of(&self, position: usize) -> Option<usize> {
        let parent = self
            .index
            .get(self.cells[position].parent.as_str())
            .copied()?;
        (parent != position && self.cells[parent].vertex).then_some(parent)
    }

    /// Where a cell sits relative to the origin its parent gives it.
    ///
    /// A vertex normally carries its own coordinates. One marked `relative`
    /// carries fractions of its parent's size instead, which is how draw.io
    /// pins a caption to a share of the box it belongs to, and either form can
    /// add an absolute offset on top.
    fn placement(&self, position: usize) -> (f64, f64) {
        let geometry = &self.cells[position].geometry;
        let (mut x, mut y) = match self.parent_of(position) {
            Some(parent) if geometry.relative => {
                let parent = &self.cells[parent].geometry;
                (geometry.x * parent.width, geometry.y * parent.height)
            }
            _ => (geometry.x, geometry.y),
        };
        if let Some((dx, dy)) = geometry.offset {
            x += dx;
            y += dy;
        }
        (x, y)
    }

    /// The absolute origin a cell's own geometry is measured from.
    ///
    /// Children of a group vertex are placed relative to that group. Layers,
    /// which are the root's own children and carry no geometry, contribute
    /// nothing.
    fn resolve_origin(&self, position: usize, depth: usize) -> (f64, f64) {
        if depth > MAX_PARENT_DEPTH {
            return (0.0, 0.0);
        }
        let Some(parent) = self.parent_of(position) else {
            return (0.0, 0.0);
        };
        let (x, y) = self.resolve_origin(parent, depth + 1);
        let (dx, dy) = self.placement(parent);
        (x + dx, y + dy)
    }

    fn origin(&self, position: usize) -> (f64, f64) {
        self.origins.get(&position).copied().unwrap_or((0.0, 0.0))
    }

    fn rect(&self, position: usize) -> Rect {
        let (x, y) = self.origin(position);
        let (dx, dy) = self.placement(position);
        let geometry = &self.cells[position].geometry;
        Rect {
            x: x + dx,
            y: y + dy,
            width: geometry.width,
            height: geometry.height,
        }
    }

    fn vertex_rect(&self, id: &str) -> Option<Rect> {
        let position = *self.index.get(id)?;
        self.cells[position].vertex.then(|| self.rect(position))
    }

    fn is_drawn(&self, position: usize) -> bool {
        let cell = &self.cells[position];
        if !cell.visible {
            return false;
        }
        let mut current = cell.parent.as_str();
        for _ in 0..MAX_PARENT_DEPTH {
            let Some(&parent) = self.index.get(current) else {
                return true;
            };
            if !self.cells[parent].visible {
                return false;
            }
            current = self.cells[parent].parent.as_str();
        }
        true
    }
}

/// Read the shapes this page asks for out of the caller's libraries, and unpack
/// the ones the diagram carries itself.
fn load_stencils(
    model: &Model,
    library: &mut stencil::Library,
    options: &ConvertOptions,
) -> Result<()> {
    let mut wanted = std::collections::HashSet::new();
    for cell in &model.cells {
        let Some(declared) = cell.style.get("shape") else {
            continue;
        };
        if let Some(payload) = inline_stencil_payload(declared) {
            let key = declared.to_ascii_lowercase();
            if library.get(&key).is_none() {
                let xml = decode_inline_stencil(payload, options)?;
                library.insert_inline(&key, &xml, options.max_xml_events)?;
            }
        } else if !declared.is_empty() {
            wanted.insert(declared.to_ascii_lowercase());
        }
    }
    // A tile names the glyph it carries in the style rather than in the shape.
    for cell in &model.cells {
        for key in ["resicon", "gricon", "network2icon"] {
            if let Some(name) = cell.style.get(key).filter(|value| !value.is_empty()) {
                wanted.insert(name.to_ascii_lowercase());
            }
        }
    }
    if wanted.is_empty() || options.stencil_paths.is_empty() {
        return Ok(());
    }
    library.load(&options.stencil_paths, &wanted, options.max_xml_events)
}

/// The payload of a `shape=stencil(<data>)` style, which is how a diagram
/// carries a shape of its own rather than naming one from a library.
fn inline_stencil_payload(declared: &str) -> Option<&str> {
    declared
        .strip_prefix("stencil(")
        .or_else(|| declared.strip_prefix("stencil%28"))
        .and_then(|rest| rest.strip_suffix(')').or_else(|| rest.strip_suffix("%29")))
}

fn decode_inline_stencil(payload: &str, options: &ConvertOptions) -> Result<Vec<u8>> {
    let packed = base64::engine::general_purpose::STANDARD
        .decode(strip_whitespace(payload.as_bytes()))
        .map_err(|error| {
            Error::InvalidInput(format!(
                "inline drawio stencil is not valid base64: {error}"
            ))
        })?;
    let limit = options.max_zip_entry_bytes;
    let mut inflated = Vec::new();
    flate2::read::DeflateDecoder::new(packed.as_slice())
        .take(limit.saturating_add(1))
        .read_to_end(&mut inflated)
        .map_err(|error| {
            Error::InvalidInput(format!(
                "inline drawio stencil is not deflate data: {error}"
            ))
        })?;
    if inflated.len() as u64 > limit {
        return Err(Error::LimitExceeded(format!(
            "inline drawio stencil expands past {limit} bytes"
        )));
    }
    let text = String::from_utf8(inflated).map_err(|error| {
        Error::InvalidInput(format!("inline drawio stencil is not UTF-8: {error}"))
    })?;
    let decoded = if text.trim_start().starts_with('<') {
        text
    } else {
        percent_decode(&text)?
    };
    Ok(decoded.into_bytes())
}

fn render(model: &Model, number: usize, name: &str, library: &mut stencil::Library) -> Page {
    let scene = Scene::new(&model.cells);
    let mut nodes = Vec::new();
    let mut warnings = Vec::<String>::new();
    let mut bounds = None::<Rect>;
    let mut clips = Vec::<ClipPath>::new();
    for position in 0..model.cells.len() {
        let cell = &model.cells[position];
        if !scene.is_drawn(position) {
            continue;
        }
        if cell.vertex {
            let rect = scene.rect(position);
            if rect.width <= 0.0 || rect.height <= 0.0 {
                continue;
            }
            extend(&mut bounds, rect);
            draw_vertex(
                cell,
                rect,
                &mut nodes,
                &mut warnings,
                &mut bounds,
                &mut clips,
                library,
            );
        } else if cell.edge {
            let route = route_edge(&scene, position);
            if route.len() < 2 {
                continue;
            }
            for &(x, y) in &route {
                extend(
                    &mut bounds,
                    Rect {
                        x,
                        y,
                        width: 0.0,
                        height: 0.0,
                    },
                );
            }
            draw_edge(
                cell,
                &route,
                &mut nodes,
                &mut warnings,
                &mut bounds,
                &mut clips,
            );
        }
    }
    // A page with nothing on it has no content to size itself against, so it
    // takes the size the model declares. draw.io stands its own "click here to
    // edit" placeholder on such a page; that placeholder is not in the model,
    // and inventing it here would put a shape in the output that the diagram
    // does not contain.
    let mut frame = match bounds {
        Some(content) => content.grow(PAGE_MARGIN),
        None => {
            warnings.push(
                "drawio page has no cells to draw; a blank page of the size the model declares is written"
                    .to_owned(),
            );
            let (width, height) = model.page.unwrap_or((850.0, 1100.0));
            Rect {
                x: 0.0,
                y: 0.0,
                width,
                height,
            }
        }
    };
    // Geometry is already held inside the coordinate range, but a drawing that
    // spans it from end to end still has to produce a page a renderer can open.
    let clamped = Rect {
        x: bounded(frame.x),
        y: bounded(frame.y),
        width: frame.width.clamp(1.0, MAX_COORDINATE),
        height: frame.height.clamp(1.0, MAX_COORDINATE),
    };
    if clamped != frame {
        warnings.push(format!(
            "drawio page is {} x {} pixels; it is cropped to {} x {} because the model places content outside the drawable range",
            n(frame.width),
            n(frame.height),
            n(clamped.width),
            n(clamped.height)
        ));
    }
    frame = clamped;
    let mut page = Page::new(
        number,
        (frame.width.max(1.0) * POINTS_PER_PIXEL).max(1.0),
        (frame.height.max(1.0) * POINTS_PER_PIXEL).max(1.0),
        "drawio",
    );
    page.title = if name.is_empty() {
        format!("Page {number}")
    } else {
        name.to_owned()
    };
    if let Some(background) = model.background.as_deref().and_then(parse_color) {
        nodes.insert(
            0,
            Node::Path {
                id: "drawio-background".into(),
                d: rectangle_path(frame),
                fill_rule: "nonzero".into(),
                fill: Paint::solid(background),
                stroke: Stroke::default(),
                transform: IDENTITY,
                clip_id: None,
                meta: SourceMeta {
                    kind: "drawio-background".into(),
                    ..SourceMeta::default()
                },
            },
        );
    }
    // One group carries the pixel-to-point scale and the crop offset so that
    // geometry, stroke widths and font sizes stay in the model's own units
    // everywhere above this point.
    page.nodes.push(Node::Group {
        id: "drawio-page".into(),
        nodes,
        transform: [
            POINTS_PER_PIXEL,
            0.0,
            0.0,
            POINTS_PER_PIXEL,
            -frame.x * POINTS_PER_PIXEL,
            -frame.y * POINTS_PER_PIXEL,
        ],
        opacity: 1.0,
        clip_id: None,
        meta: SourceMeta {
            kind: "drawio-page".into(),
            ..SourceMeta::default()
        },
    });
    page.clips = clips;
    for warning in warnings {
        page.warn(warning);
    }
    page
}

fn extend(bounds: &mut Option<Rect>, rect: Rect) {
    let merged = match *bounds {
        None => rect,
        Some(current) => {
            let x = current.x.min(rect.x);
            let y = current.y.min(rect.y);
            Rect {
                x,
                y,
                width: current.right().max(rect.right()) - x,
                height: current.bottom().max(rect.bottom()) - y,
            }
        }
    };
    *bounds = Some(merged);
}

// ---------------------------------------------------------------------------
// Paint
// ---------------------------------------------------------------------------

struct Appearance {
    fill: Paint,
    stroke: Stroke,
    shadow: bool,
}

fn appearance(style: &Style, rect: Rect, default_fill: &str, default_stroke: &str) -> Appearance {
    let opacity = (style.number("opacity", 100.0) / 100.0).clamp(0.0, 1.0);
    let fill_opacity = opacity * (style.number("fillopacity", 100.0) / 100.0).clamp(0.0, 1.0);
    let stroke_opacity = opacity * (style.number("strokeopacity", 100.0) / 100.0).clamp(0.0, 1.0);
    let fill_color = match style.get("fillcolor") {
        Some(value) => parse_color(value),
        None => parse_color(default_fill),
    };
    let fill = match fill_color {
        None => Paint::None,
        Some(color) => match style.color("gradientcolor") {
            None => Paint::Solid {
                color,
                opacity: fill_opacity,
            },
            Some(second) => Paint::LinearGradient(Box::new(gradient(
                rect,
                &color,
                &second,
                style.get("gradientdirection").unwrap_or("south"),
                fill_opacity,
            ))),
        },
    };
    let stroke_color = match style.get("strokecolor") {
        Some(value) => parse_color(value),
        None => parse_color(default_stroke),
    };
    let width = style.number("strokewidth", 1.0).max(0.0);
    let stroke = match stroke_color {
        None => Stroke::default(),
        Some(color) if width <= 0.0 => {
            let _ = color;
            Stroke::default()
        }
        Some(color) => Stroke {
            paint: Paint::Solid {
                color,
                opacity: stroke_opacity,
            },
            width,
            line_cap: LineCap::Butt,
            line_join: LineJoin::Miter,
            miter_limit: 4.0,
            dash_array: dash_array(style, width),
            dash_offset: 0.0,
        },
    };
    Appearance {
        fill,
        stroke,
        shadow: style.flag("shadow"),
    }
}

/// mxGraph's dashed stroke is `3 3` scaled by the stroke width, unless the
/// style spells the pattern out in stroke-width multiples.
fn dash_array(style: &Style, width: f64) -> Vec<f64> {
    if !style.flag("dashed") {
        return Vec::new();
    }
    let scale = width.max(1.0);
    match style.get("dashpattern") {
        Some(pattern) => {
            let values = pattern
                .split_whitespace()
                .filter_map(|value| value.parse::<f64>().ok())
                .filter(|value| value.is_finite() && *value >= 0.0)
                .map(|value| value * scale)
                .collect::<Vec<_>>();
            if values.is_empty() {
                vec![3.0 * scale, 3.0 * scale]
            } else {
                values
            }
        }
        None => vec![3.0 * scale, 3.0 * scale],
    }
}

fn gradient(rect: Rect, from: &str, to: &str, direction: &str, opacity: f64) -> LinearGradient {
    // mxGraph names the direction the gradient runs towards; south, the
    // default, shades from the fill colour at the top to the gradient colour
    // at the bottom.
    let (x1, y1, x2, y2) = match direction {
        "north" => (rect.x, rect.bottom(), rect.x, rect.y),
        "east" => (rect.x, rect.y, rect.right(), rect.y),
        "west" => (rect.right(), rect.y, rect.x, rect.y),
        _ => (rect.x, rect.y, rect.x, rect.bottom()),
    };
    LinearGradient {
        x1,
        y1,
        x2,
        y2,
        stops: vec![
            GradientStop {
                offset: 0.0,
                color: from.to_owned(),
                opacity,
            },
            GradientStop {
                offset: 1.0,
                color: to.to_owned(),
                opacity,
            },
        ],
    }
}

// ---------------------------------------------------------------------------
// Geometry helpers
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// Vertices
// ---------------------------------------------------------------------------

fn draw_vertex(
    cell: &Cell,
    rect: Rect,
    nodes: &mut Vec<Node>,
    warnings: &mut Vec<String>,
    bounds: &mut Option<Rect>,
    clips: &mut Vec<ClipPath>,
    library: &mut stencil::Library,
) {
    let style = &cell.style;
    let shape = style.shape();
    let name = shape.drawn;
    let (drawing, direction_turn) = directed_rect(rect, &style.text("direction", "east"));
    let rotation = style.number("rotation", 0.0);
    let transform = shape_transform(
        rect,
        rotation + direction_turn,
        style.flag("fliph"),
        style.flag("flipv"),
    );
    if matches!(
        style.get("shape"),
        Some(
            "mxgraph.infographic.shadedCube"
                | "mxgraph.infographic.shadedTriangle"
                | "mxgraph.infographic.shadedPyramid"
                | "mxgraph.infographic.pyramidStep"
                | "mxgraph.infographic.banner"
                | "mxgraph.infographic.bannerSingleFold"
                | "mxgraph.bootstrap.radioButton"
                | "mxgraph.sysml.actFinal"
                | "mxgraph.ios7ui.appBar"
                | "mxgraph.ios7ui.pageControl"
                | "mxgraph.ios7ui.downloadBar"
                | "mxgraph.ios7ui.slider"
                | "mxgraph.ios7ui.onOffButton"
                | "mxgraph.lean_mapping.truck_shipment"
                | "mxgraph.archimate3.application"
                | "mxgraph.archimate3.tech"
                | "mxgraph.archimate3.service"
                | "mxgraph.archimate3.actor"
                | "mxgraph.rackGeneral.plate"
                | "mxgraph.rackGeneral.neatPatch"
                | "mxgraph.rackGeneral.rackCabinet3"
                | "mxgraph.c4.person"
                | "mxgraph.mockup.containers.browserWindow"
                | "mxgraph.mockup.graphics.columnChart"
                | "mxgraph.mockup.containers.videoPlayer"
                | "mxgraph.eip.messageChannel"
                | "mxgraph.eip.deadLetterChannel"
                | "mxgraph.ios7ui.icon"
                | "mxgraph.bootstrap.image"
                | "mxgraph.mockup.containers.window"
                | "mxgraph.mockup.navigation.scrollBar"
                | "mxgraph.mockup.forms.spinner"
                | "mxgraph.mockup.forms.checkboxGroup"
                | "mxgraph.mockup.misc.pin"
                | "mxgraph.mockup.misc.rating"
                | "mxgraph.bootstrap.rating"
                | "mxgraph.bootstrap.leftButtonStriped"
                | "mxgraph.networks.bus"
                | "smileyFace"
                | "mxgraph.mockup.containers.userMale"
                | "mxgraph.ios.iBgMap"
                | "mxgraph.ios.iBgStriped"
                | "mxgraph.ios.iPin"
                | "mxgraph.gmdl.player"
                | "mxgraph.electrical.logic_gates.logic_gate"
                | "mxgraph.ios.iLocBar"
                | "mxgraph.android.statusBar"
                | "mxgraph.infographic.circularCallout2"
                | "mxgraph.ios7ui.actionDialog"
                | "mxgraph.mockup.graphics.pieChart"
        )
    ) && draw_shaded_shape(cell, drawing, transform, nodes)
    {
        draw_label(
            cell,
            label_bounds(rect, style),
            rotation,
            nodes,
            bounds,
            clips,
            false,
        );
        return;
    }
    // This connector paints itself in the set's own amber, whatever the style
    // says, so it is drawn here rather than through the shared colour rules.
    if style.get("shape") == Some("mxgraph.aws3d.flatDoubleEdge") {
        nodes.push(Node::Path {
            id: format!("drawio-{}", cell.id),
            d: isometric_double_edge_path(drawing),
            fill_rule: "nonzero".into(),
            fill: Paint::solid("#F4B934"),
            stroke: Stroke::default(),
            transform,
            clip_id: None,
            meta: SourceMeta {
                kind: "drawio-isometric-edge".into(),
                source_id: cell.id.clone(),
                ..SourceMeta::default()
            },
        });
        draw_label(
            cell,
            label_bounds(rect, style),
            rotation,
            nodes,
            bounds,
            clips,
            false,
        );
        return;
    }
    // A network icon stands on a rounded tile and carries its picture as a
    // separate stencil, which the style names rather than spelling out.
    if style.get("shape") == Some("mxgraph.networks2.icon") {
        let drawn_tile = draw_shaded_shape(cell, drawing, transform, nodes);
        let icon = style.text("network2icon", "").to_ascii_lowercase();
        // A tile shrinks the picture to five sevenths and centres it, the way
        // the editor does; without one the picture fills the box.
        let inside = if drawn_tile {
            Rect {
                x: drawing.x + drawing.width / 7.0,
                y: drawing.y + drawing.height / 7.0,
                width: drawing.width / 1.4,
                height: drawing.height / 1.4,
            }
        } else {
            drawing
        };
        // draw.io also draws a long shadow behind the picture, from a stencil
        // of its own filled solid black. A stencil is painted here in the
        // cell's own colours, so that silhouette would come out over the
        // picture rather than under it, and it is left out instead.
        if !icon.is_empty() && !draw_stencil(cell, &icon, inside, transform, nodes, library) {
            let warning = format!(
                "drawio shape 'mxgraph.networks2.icon' carries the icon '{icon}'; pass that library's stencil file to draw it"
            );
            if !warnings.contains(&warning) {
                warnings.push(warning);
            }
        }
        draw_label(
            cell,
            label_bounds(rect, style),
            rotation,
            nodes,
            bounds,
            clips,
            false,
        );
        return;
    }
    // The AWS 3D set draws every service as the same isometric box with a
    // white glyph on top. The box is faithful; the glyph lives in the editor's
    // own code, so it is named as missing rather than quietly left out.
    if let Some(declared) = style
        .get("shape")
        .filter(|value| value.starts_with("mxgraph.aws3d."))
        .filter(|value| !is_isometric_connector(value))
        && draw_isometric_box(cell, drawing, transform, nodes)
    {
        let warning = format!(
            "drawio shape '{declared}' is drawn as the isometric box it stands on; the service glyph it carries is not drawn"
        );
        if !warnings.contains(&warning) {
            warnings.push(warning);
        }
        draw_label(
            cell,
            label_bounds(rect, style),
            rotation,
            nodes,
            bounds,
            clips,
            false,
        );
        return;
    }
    // BPMN events, gateways and activities are built from layers the style
    // names separately, so they are drawn here rather than as one outline.
    if matches!(
        style.get("shape"),
        Some("mxgraph.bpmn.shape" | "mxgraph.bpmn.event" | "mxgraph.bpmn.gateway2")
    ) && draw_bpmn(cell, drawing, transform, nodes)
    {
        draw_label(
            cell,
            label_bounds(rect, style),
            rotation,
            nodes,
            bounds,
            clips,
            false,
        );
        return;
    }
    // A shape library holds the outline the editor itself draws, so when the
    // caller has supplied one it wins over this module's own approximation of
    // the same name.
    if let Some(declared) = style.get("shape")
        && draw_stencil(
            cell,
            &declared.to_ascii_lowercase(),
            drawing,
            transform,
            nodes,
            library,
        )
    {
        draw_inner_icon(cell, drawing, transform, nodes, library);
        draw_image(cell, rect, name, transform, nodes, warnings);
        draw_label(
            cell,
            label_bounds(rect, style),
            rotation,
            nodes,
            bounds,
            clips,
            false,
        );
        return;
    }
    if let Some(declared) = shape.unknown.as_deref() {
        let warning = if declared.starts_with("mxgraph.") {
            format!(
                "drawio shape '{declared}' comes from a shape library; pass that library's stencil file to draw it, or a placeholder rectangle with its label is used instead"
            )
        } else {
            format!(
                "drawio shape '{declared}' is not drawn; a placeholder rectangle with its label is used instead"
            )
        };
        if !warnings.contains(&warning) {
            warnings.push(warning);
        }
    }
    let paths =
        shape_paths(name, drawing, style).unwrap_or_else(|| Paths::new(rectangle_path(drawing)));
    let (default_fill, default_stroke) = match name {
        "text" | "image" => ("none", "none"),
        "line" => ("none", "#000000"),
        _ => ("#FFFFFF", "#000000"),
    };
    let look = appearance(style, drawing, default_fill, default_stroke);
    let meta = SourceMeta {
        kind: "drawio-shape".into(),
        source_id: cell.id.clone(),
        semantic_role: shape.unknown.clone().unwrap_or_else(|| name.to_owned()),
        ..SourceMeta::default()
    };
    // `glass=1` lays a white sheen over the top of the shape, fading out.
    let glass = style.flag("glass") && !paths.outline.is_empty();
    if look.shadow && !paths.outline.is_empty() {
        nodes.push(shadow_node(
            &format!("drawio-{}-shadow", cell.id),
            paths.outline.clone(),
            &look,
            transform,
        ));
    }
    if !paths.outline.is_empty() {
        nodes.push(Node::Path {
            id: format!("drawio-{}", cell.id),
            d: paths.outline,
            fill_rule: "nonzero".into(),
            fill: look.fill.clone(),
            stroke: if paths.stroke_outline {
                look.stroke.clone()
            } else {
                Stroke::default()
            },
            transform,
            clip_id: None,
            meta: meta.clone(),
        });
    }
    if glass {
        nodes.push(glass_node(
            &format!("drawio-{}-glass", cell.id),
            drawing,
            style,
            transform,
        ));
    }
    if name == "swimlane" {
        draw_swimlane_divider(cell, drawing, &look, transform, nodes);
    }
    for (index, decoration) in paths.decorations.into_iter().enumerate() {
        nodes.push(Node::Path {
            id: format!("drawio-{}-detail-{index}", cell.id),
            d: decoration,
            fill_rule: "nonzero".into(),
            fill: Paint::None,
            stroke: look.stroke.clone(),
            transform,
            clip_id: None,
            meta: SourceMeta {
                kind: "drawio-shape-detail".into(),
                ..meta.clone()
            },
        });
    }
    draw_inner_icon(cell, drawing, transform, nodes, library);
    draw_image(cell, rect, name, transform, nodes, warnings);
    let label_box = if name == "umlFrame" {
        umlframe_tab(rect, style)
    } else if name == "folder" && style.flag("labelinheader") {
        // A package's name goes in its tab.
        folder_tab(rect, style)
    } else if name == "folder" && style.flag("boundedlbl") {
        let tab = folder_tab(rect, style);
        Rect {
            y: tab.bottom(),
            height: (rect.bottom() - tab.bottom()).max(0.0),
            ..rect
        }
    } else if name == "swimlane" {
        let start = style.number("startsize", 23.0).max(0.0);
        if style.get("horizontal") == Some("0") {
            Rect {
                width: start.min(rect.width),
                ..rect
            }
        } else {
            Rect {
                height: start.min(rect.height),
                ..rect
            }
        }
    } else {
        label_bounds(rect, style)
    };
    draw_label(cell, label_box, rotation, nodes, bounds, clips, false);
}

/// The rule that puts an icon's caption underneath it: draw.io keeps the label
/// box the same size as the shape and moves it one full box in the requested
/// direction.
fn label_bounds(rect: Rect, style: &Style) -> Rect {
    let mut box_rect = rect;
    match style.get("labelposition") {
        Some("left") => box_rect.x -= rect.width,
        Some("right") => box_rect.x += rect.width,
        _ => {}
    }
    match style.get("verticallabelposition") {
        Some("top") => box_rect.y -= rect.height,
        Some("bottom") => box_rect.y += rect.height,
        _ => {}
    }
    box_rect
}

fn draw_swimlane_divider(
    cell: &Cell,
    rect: Rect,
    look: &Appearance,
    transform: Matrix,
    nodes: &mut Vec<Node>,
) {
    let start = cell.style.number("startsize", 23.0).max(0.0);
    if start <= 0.0 {
        return;
    }
    let vertical = cell.style.get("horizontal") == Some("0");
    let body = if vertical {
        Rect {
            x: rect.x + start,
            width: (rect.width - start).max(0.0),
            ..rect
        }
    } else {
        Rect {
            y: rect.y + start,
            height: (rect.height - start).max(0.0),
            ..rect
        }
    };
    if body.width <= 0.0 || body.height <= 0.0 {
        return;
    }
    // The lane body takes its own fill; the title bar keeps the shape's.
    if let Some(color) = cell.style.color("swimlanefillcolor") {
        nodes.push(Node::Path {
            id: format!("drawio-{}-lane", cell.id),
            d: rectangle_path(body),
            fill_rule: "nonzero".into(),
            fill: Paint::solid(color),
            stroke: Stroke::default(),
            transform,
            clip_id: None,
            meta: SourceMeta {
                kind: "drawio-swimlane-body".into(),
                source_id: cell.id.clone(),
                ..SourceMeta::default()
            },
        });
    }
    let divider = if vertical {
        line_path((body.x, rect.y), (body.x, rect.bottom()))
    } else {
        line_path((rect.x, body.y), (rect.right(), body.y))
    };
    nodes.push(Node::Path {
        id: format!("drawio-{}-divider", cell.id),
        d: divider,
        fill_rule: "nonzero".into(),
        fill: Paint::None,
        stroke: look.stroke.clone(),
        transform,
        clip_id: None,
        meta: SourceMeta {
            kind: "drawio-swimlane-divider".into(),
            source_id: cell.id.clone(),
            ..SourceMeta::default()
        },
    });
}

// ---------------------------------------------------------------------------
// Labels
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// Edges
// ---------------------------------------------------------------------------

/// draw.io's drop shadow: the same outline again, one solid grey copy offset
/// down and to the right, drawn under the shape itself.
fn shadow_node(id: &str, d: String, look: &Appearance, transform: Matrix) -> Node {
    Node::Path {
        id: id.to_owned(),
        d,
        fill_rule: "nonzero".into(),
        fill: match look.fill {
            Paint::None => Paint::None,
            _ => Paint::solid(SHADOW_COLOR),
        },
        stroke: match look.stroke.paint {
            Paint::None => Stroke::default(),
            _ => Stroke {
                paint: Paint::solid(SHADOW_COLOR),
                ..look.stroke.clone()
            },
        },
        transform: compose(
            [1.0, 0.0, 0.0, 1.0, SHADOW_OFFSET.0, SHADOW_OFFSET.1],
            transform,
        ),
        clip_id: None,
        meta: SourceMeta {
            kind: "drawio-shadow".into(),
            ..SourceMeta::default()
        },
    }
}

/// Place a cell's embedded picture.
///
/// A picture shape covers the whole cell; on any other shape draw.io draws the
/// picture as an icon inside it, sized by `imageWidth` and `imageHeight`.
fn draw_image(
    cell: &Cell,
    rect: Rect,
    name: &str,
    transform: Matrix,
    nodes: &mut Vec<Node>,
    warnings: &mut Vec<String>,
) {
    let Some(source) = cell.style.get("image") else {
        return;
    };
    let href = match image_href(source) {
        Ok(href) => href,
        Err(reason) => {
            if !warnings.contains(&reason) {
                warnings.push(reason);
            }
            return;
        }
    };
    let style = &cell.style;
    let box_rect = if name == "image" {
        rect
    } else {
        let width = style.number("imagewidth", 24.0).clamp(0.0, rect.width);
        let height = style.number("imageheight", 24.0).clamp(0.0, rect.height);
        let x = match style.get("imagealign") {
            Some("center") => rect.center().0 - width / 2.0,
            Some("right") => rect.right() - width,
            _ => rect.x,
        };
        let y = match style.get("imageverticalalign") {
            Some("top") => rect.y,
            Some("bottom") => rect.bottom() - height,
            _ => rect.center().1 - height / 2.0,
        };
        Rect {
            x,
            y,
            width,
            height,
        }
    };
    if box_rect.width <= 0.0 || box_rect.height <= 0.0 {
        return;
    }
    nodes.push(Node::Image {
        id: format!("drawio-{}-image", cell.id),
        href,
        x: box_rect.x,
        y: box_rect.y,
        width: box_rect.width,
        height: box_rect.height,
        transform,
        opacity: (style.number("opacity", 100.0) / 100.0).clamp(0.0, 1.0),
        clip_id: None,
        meta: SourceMeta {
            kind: "drawio-image".into(),
            source_id: cell.id.clone(),
            ..SourceMeta::default()
        },
    });
}

/// Normalise draw.io's `image=data:image/png,<base64>` into a data URI a
/// renderer accepts.
///
/// Only embedded data is used. A picture kept as a URL would have to be
/// fetched, which a local converter must not do. An embedded SVG is a second
/// document rather than pixels, so it has to pass the same checks that keep a
/// packaged SVG inert before it is copied into the output.
///
/// The reason comes back as the error, so the caller can say what was dropped.
fn image_href(source: &str) -> std::result::Result<String, String> {
    let source = source.trim();
    let Some(rest) = source.strip_prefix("data:") else {
        return Err(
            "drawio image is a URL rather than embedded data; it is not fetched, and the shape is drawn without it"
                .to_owned(),
        );
    };
    let Some((header, payload)) = rest.split_once(',') else {
        return Err(
            "drawio image data URI has no payload; the shape is drawn without it".to_owned(),
        );
    };
    let mime = match header.strip_suffix(";base64").unwrap_or(header) {
        "image/png" => "image/png",
        "image/jpeg" | "image/jpg" => "image/jpeg",
        "image/gif" => "image/gif",
        "image/webp" => "image/webp",
        "image/svg+xml" => "image/svg+xml",
        other => {
            return Err(format!(
                "drawio image type {other} is not embedded; the shape is drawn without it"
            ));
        }
    };
    let payload = payload.trim();
    if payload.is_empty() {
        return Err("drawio image data URI is empty; the shape is drawn without it".to_owned());
    }
    if payload.len() > MAX_IMAGE_CHARS {
        return Err(format!(
            "drawio image is larger than {MAX_IMAGE_CHARS} encoded bytes; the shape is drawn without it"
        ));
    }
    // draw.io leaves the `;base64` marker off, so confirm the payload really is
    // base64 before promising a renderer that it is.
    let decoded = base64::engine::general_purpose::STANDARD
        .decode(payload)
        .map_err(|error| {
            format!("drawio image payload is not base64 ({error}); the shape is drawn without it")
        })?;
    if mime == "image/svg+xml" {
        crate::reverse::validate_svg_document(&decoded, 1).map_err(|error| {
            format!(
                "drawio embedded SVG image was refused ({error}); the shape is drawn without it"
            )
        })?;
    }
    Ok(format!("data:{mime};base64,{payload}"))
}

impl Diagram {
    /// This one diagram as a standalone `mxfile`, which is the form draw.io
    /// stores in an SVG export and reads back when the SVG is opened.
    fn as_mxfile(&self, model: &[u8]) -> String {
        format!(
            "<mxfile host=\"document-svg\" agent=\"document-svg {}\" type=\"device\" pages=\"1\">\
             <diagram id=\"{}\" name=\"{}\">{}</diagram></mxfile>",
            env!("CARGO_PKG_VERSION"),
            escape_xml(&self.id),
            escape_xml(&self.name),
            String::from_utf8_lossy(model.trim_ascii())
        )
    }
}

fn escape_xml(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for character in value.chars() {
        match character {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '"' => escaped.push_str("&quot;"),
            '\'' => escaped.push_str("&apos;"),
            _ => escaped.push(character),
        }
    }
    escaped
}

/// Draw a shape from a library, if one has been loaded for this name.
///
/// Returns whether the shape was drawn, so the caller can fall back to its own
/// geometry when the library is not there.
fn draw_stencil(
    cell: &Cell,
    name: &str,
    rect: Rect,
    transform: Matrix,
    nodes: &mut Vec<Node>,
    library: &mut stencil::Library,
) -> bool {
    let Some(shape) = library.get(name) else {
        return false;
    };
    let style = &cell.style;
    let opacity = (style.number("opacity", 100.0) / 100.0).clamp(0.0, 1.0);
    let stroke_width = style.number("strokewidth", 1.0).max(0.0);
    let inherited = stencil::Inherited {
        fill: match style.get("fillcolor") {
            Some(value) => parse_color(value),
            None => parse_color("#FFFFFF"),
        },
        stroke: match style.get("strokecolor") {
            Some(value) => parse_color(value),
            None => parse_color("#000000"),
        },
        stroke_width,
        opacity,
        fill_opacity: (style.number("fillopacity", 100.0) / 100.0).clamp(0.0, 1.0),
        stroke_opacity: (style.number("strokeopacity", 100.0) / 100.0).clamp(0.0, 1.0),
        dash: dash_array(style, stroke_width),
    };
    let identity = format!("drawio-{}", cell.id);
    let drawn = shape.render(&identity, rect, &inherited);
    if drawn.is_empty() {
        // The library has this shape and it paints nothing, which is what the
        // editor draws for it too. Standing a placeholder in its place, and
        // asking for a library that is already loaded, would both be wrong.
        return true;
    }
    if style.flag("shadow") {
        // draw.io shadows a library shape the same way it shadows any other:
        // one solid grey copy, offset down and to the right.
        let shadow = shape.render(
            &format!("{identity}-shadow"),
            rect,
            &stencil::Inherited {
                fill: Some(SHADOW_COLOR.to_owned()),
                stroke: Some(SHADOW_COLOR.to_owned()),
                ..inherited
            },
        );
        nodes.push(Node::Group {
            id: format!("{identity}-shadow"),
            nodes: shadow,
            transform: compose(
                [1.0, 0.0, 0.0, 1.0, SHADOW_OFFSET.0, SHADOW_OFFSET.1],
                transform,
            ),
            opacity: 1.0,
            clip_id: None,
            meta: SourceMeta {
                kind: "drawio-shadow".into(),
                ..SourceMeta::default()
            },
        });
    }
    nodes.push(Node::Group {
        id: identity,
        nodes: drawn,
        transform,
        opacity: 1.0,
        clip_id: None,
        meta: SourceMeta {
            kind: "drawio-stencil".into(),
            source_id: cell.id.clone(),
            semantic_role: name.to_owned(),
            ..SourceMeta::default()
        },
    });
    true
}

/// Draw the icon a shape carries inside itself.
///
/// A cloud service tile names its own glyph in the style rather than in the
/// shape: `resIcon` puts one in the middle of the tile in white, and `grIcon`
/// puts one in the corner of a group's frame in the frame's own colour. Without
/// this the tile keeps its colour and loses the picture that says what service
/// it stands for.
fn draw_inner_icon(
    cell: &Cell,
    rect: Rect,
    transform: Matrix,
    nodes: &mut Vec<Node>,
    library: &mut stencil::Library,
) {
    let style = &cell.style;
    let (key, inner, fill) = if let Some(name) = style.get("resicon") {
        // draw.io draws a resource icon at seven tenths of the tile, centred.
        (
            name.to_ascii_lowercase(),
            Rect {
                x: rect.x + rect.width * 0.15,
                y: rect.y + rect.height * 0.15,
                width: rect.width * 0.7,
                height: rect.height * 0.7,
            },
            style
                .color("fontcolor")
                .filter(|_| style.get("fillcolor").is_some_and(|value| value == "none"))
                .unwrap_or_else(|| "#FFFFFF".to_owned()),
        )
    } else if let Some(name) = style.get("gricon") {
        let size = (rect.width * 0.2).min(rect.height * 0.2).min(26.0);
        (
            name.to_ascii_lowercase(),
            Rect {
                x: rect.x,
                y: rect.y,
                width: size,
                height: size,
            },
            style
                .color("strokecolor")
                .unwrap_or_else(|| "#000000".to_owned()),
        )
    } else {
        return;
    };
    let Some(shape) = library.get(&key) else {
        return;
    };
    let drawn = shape.render(
        &format!("drawio-{}-icon", cell.id),
        inner,
        &stencil::Inherited {
            fill: Some(fill),
            stroke: None,
            stroke_width: 0.0,
            opacity: (style.number("opacity", 100.0) / 100.0).clamp(0.0, 1.0),
            fill_opacity: 1.0,
            stroke_opacity: 1.0,
            dash: Vec::new(),
        },
    );
    if drawn.is_empty() {
        return;
    }
    nodes.push(Node::Group {
        id: format!("drawio-{}-icon", cell.id),
        nodes: drawn,
        transform,
        opacity: 1.0,
        clip_id: None,
        meta: SourceMeta {
            kind: "drawio-stencil-icon".into(),
            source_id: cell.id.clone(),
            semantic_role: key,
            ..SourceMeta::default()
        },
    });
}

// ---------------------------------------------------------------------------
// BPMN
// ---------------------------------------------------------------------------

/// The isometric box the AWS 3D set stands every service on: a six-sided
/// silhouette with the two visible faces shaded.
fn draw_isometric_box(cell: &Cell, rect: Rect, transform: Matrix, nodes: &mut Vec<Node>) -> bool {
    if rect.width <= 0.0 || rect.height <= 0.0 {
        return false;
    }
    let style = &cell.style;
    let Rect { x, y, .. } = rect;
    let (w, h) = (rect.width, rect.height);
    let point = |fx: f64, fy: f64| (x + w * fx, y + h * fy);
    let fill = match style.get("fillcolor") {
        Some(value) => parse_color(value),
        None => parse_color("#FFFFFF"),
    };
    let outline = style
        .color("strokecolor2")
        .or_else(|| style.color("strokecolor"))
        .unwrap_or_else(|| "#292929".to_owned());
    let width = style.number("strokewidth", 1.0).max(0.0);
    let opacity = (style.number("opacity", 100.0) / 100.0).clamp(0.0, 1.0);
    // `shadingCols` gives the two face alphas, the left face first.
    let shading = style.text("shadingcols", "0.1,0.3");
    let mut alphas = shading.split(',').map(|value| {
        value
            .trim()
            .parse::<f64>()
            .ok()
            .filter(|value| value.is_finite())
            .map_or(0.1, |value| value.clamp(0.0, 1.0))
    });
    let (left_alpha, right_alpha) = (alphas.next().unwrap_or(0.1), alphas.next().unwrap_or(0.3));
    let identity = format!("drawio-{}", cell.id);
    nodes.push(Node::Path {
        id: format!("{identity}-body"),
        d: polygon_path(&[
            point(0.0, 0.7236),
            point(0.0, 0.2863),
            point(0.5, 0.0),
            point(1.0, 0.2863),
            point(1.0, 0.7236),
            point(0.5, 1.0),
        ]),
        fill_rule: "nonzero".into(),
        fill: fill.map_or(Paint::None, |color| Paint::Solid { color, opacity }),
        stroke: Stroke {
            paint: Paint::Solid {
                color: outline,
                opacity,
            },
            width: width * 2.0,
            line_join: LineJoin::Round,
            ..Stroke::default()
        },
        transform,
        clip_id: None,
        meta: SourceMeta {
            kind: "drawio-isometric".into(),
            source_id: cell.id.clone(),
            ..SourceMeta::default()
        },
    });
    for (suffix, alpha, points) in [
        (
            "left",
            left_alpha,
            [
                point(0.0, 0.2863),
                point(0.5, 0.5726),
                point(0.5, 1.0),
                point(0.0, 0.7177),
            ],
        ),
        (
            "right",
            right_alpha,
            [
                point(1.0, 0.2863),
                point(0.5, 0.5726),
                point(0.5, 1.0),
                point(1.0, 0.7177),
            ],
        ),
    ] {
        nodes.push(Node::Path {
            id: format!("{identity}-{suffix}"),
            d: polygon_path(&points),
            fill_rule: "nonzero".into(),
            fill: Paint::Solid {
                color: "#000000".into(),
                opacity: alpha * opacity,
            },
            stroke: Stroke::default(),
            transform,
            clip_id: None,
            meta: SourceMeta {
                kind: "drawio-isometric-face".into(),
                source_id: cell.id.clone(),
                ..SourceMeta::default()
            },
        });
    }
    true
}

/// Whether an AWS 3D name is one of the flat connectors rather than a service
/// that stands on an isometric box.
fn is_isometric_connector(name: &str) -> bool {
    let tail = name.rsplit('.').next().unwrap_or(name).to_ascii_lowercase();
    tail.contains("edge") || tail.contains("arrow") || tail.contains("plane")
}

/// Shapes that need more than one fill colour: an isometric cube with two
/// shaded faces, and a pie chart whose slices each take their own colour.
fn draw_shaded_shape(cell: &Cell, rect: Rect, transform: Matrix, nodes: &mut Vec<Node>) -> bool {
    if rect.width <= 0.0 || rect.height <= 0.0 {
        return false;
    }
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
    let mut shade = Shading {
        identity: format!("drawio-{}", cell.id),
        index: 0,
        stroke: stroke.clone(),
        width,
        opacity,
        transform,
        source: cell.id.clone(),
    };
    let Rect { x, y, .. } = rect;
    let (w, h) = (rect.width, rect.height);
    let (right, bottom) = (rect.right(), rect.bottom());
    let (cx, cy) = rect.center();
    if style.get("shape") == Some("mxgraph.infographic.shadedCube") {
        // The angle is given in gradians of the isometric projection.
        let angle = style.number("isoangle", 15.0).clamp(0.01, 94.0) * PI / 200.0;
        let iso = (w * angle.tan()).min(h * 0.5);
        shade.paint(
            nodes,
            polygon_path(&[
                (x + w * 0.5, y),
                (right, y + iso),
                (right, bottom - iso),
                (x + w * 0.5, bottom),
                (x, bottom - iso),
                (x, y + iso),
            ]),
            fill.as_deref(),
            1.0,
            true,
            None,
        );
        for points in [
            [
                (x + w * 0.5, y + 2.0 * iso),
                (right, y + iso),
                (right, bottom - iso),
                (x + w * 0.5, bottom),
            ],
            [
                (x + w * 0.5, y + 2.0 * iso),
                (x, y + iso),
                (x, bottom - iso),
                (x + w * 0.5, bottom),
            ],
        ] {
            shade.paint(
                nodes,
                polygon_path(&points),
                Some("#000000"),
                0.2,
                false,
                None,
            );
        }
        return true;
    }
    // An action sheet: the panel, with a button for each choice on it.
    if style.get("shape") == Some("mxgraph.ios7ui.actionDialog") {
        shade.paint(
            nodes,
            rectangle_path(rect),
            fill.as_deref(),
            1.0,
            false,
            None,
        );
        let button = style.text("buttoncolor", "#E0E0E0");
        for top in [0.1, 0.55] {
            shade.paint(
                nodes,
                oval_rect_path(
                    Rect {
                        x: x + w * 0.05,
                        y: y + h * top,
                        width: w * 0.9,
                        height: h * 0.35,
                    },
                    w * 0.025,
                    h * 0.05,
                ),
                Some(&button),
                1.0,
                false,
                None,
            );
        }
        return true;
    }
    // A network icon: the rounded tile it stands on, shaded when the style
    // asks for it. The picture itself is a stencil the caller has to supply,
    // and is named in a warning when it is not drawn.
    if style.get("shape") == Some("mxgraph.networks2.icon") {
        let background = style.text("network2bgfillcolor", "none");
        if background == "none" {
            return false;
        }
        let tile = oval_rect_path(rect, w * 0.03571, h * 0.03571);
        match style.get("gradientcolor").filter(|value| *value != "none") {
            Some(second) => shade.shade(nodes, tile, rect, &background, second, false),
            None => shade.paint(nodes, tile, Some(&background), 1.0, false, None),
        }
        return true;
    }
    // A location bar: a shaded plaque with a pointer on one edge, and the
    // controls drawn along it.
    if style.get("shape") == Some("mxgraph.ios.iLocBar") {
        let radius = 2.5;
        let dead = radius + 7.5;
        let at = x
            + dead
            + (w - 2.0 * dead).max(0.0) * style.number("barpos", 80.0).clamp(0.0, 100.0) / 100.0;
        let top = style.text("pointerpos", "bottom") == "top";
        let body = if top {
            path_of(&[
                Step::Move(x, y + radius + 7.5),
                Step::Arc(radius, radius, 0, 1, x + radius, y + 7.5),
                Step::Line(at - 7.5, y + 7.5),
                Step::Line(at, y),
                Step::Line(at + 7.5, y + 7.5),
                Step::Line(right - radius, y + 7.5),
                Step::Arc(radius, radius, 0, 1, right, y + radius + 7.5),
                Step::Line(right, bottom - radius),
                Step::Arc(radius, radius, 0, 1, right - radius, bottom),
                Step::Line(x + radius, bottom),
                Step::Arc(radius, radius, 0, 1, x, bottom - radius),
                Step::Close,
            ])
        } else {
            path_of(&[
                Step::Move(x, y + radius),
                Step::Arc(radius, radius, 0, 1, x + radius, y),
                Step::Line(right - radius, y),
                Step::Arc(radius, radius, 0, 1, right, y + radius),
                Step::Line(right, bottom - radius - 7.5),
                Step::Arc(radius, radius, 0, 1, right - radius, bottom - 7.5),
                Step::Line(at + 7.5, bottom - 7.5),
                Step::Line(at, bottom),
                Step::Line(at - 7.5, bottom - 7.5),
                Step::Line(x + radius, bottom - 7.5),
                Step::Arc(radius, radius, 0, 1, x, bottom - radius - 7.5),
                Step::Close,
            ])
        };
        shade.shade(
            nodes,
            body,
            rect,
            &style.text("fillcolor3", "#888888"),
            &style.text("fillcolor2", "#000000"),
            false,
        );
        shade.line(
            nodes,
            ios_location_bar(rect),
            Some(&style.text("strokecolor2", "#000000")),
            0.5,
        );
        return true;
    }
    // An Android status bar: the band, then its marks in the second colour.
    if style.get("shape") == Some("mxgraph.android.statusBar") {
        shade.paint(
            nodes,
            rectangle_path(rect),
            fill.as_deref(),
            1.0,
            false,
            None,
        );
        let ink = style.text("strokecolor2", "#FFFFFF");
        shade.paint(
            nodes,
            android_status_bar(rect),
            Some(&ink),
            1.0,
            false,
            Some(&ink),
        );
        return true;
    }
    // A numbered pin: a ring on a stalk, with the ring and the dot at its foot
    // punched out so the fill shows through them.
    if style.get("shape") == Some("mxgraph.infographic.circularCallout2") {
        let rx = (w * 0.5).min(h * 0.4).min(h * 0.5 - 7.0).max(0.0);
        let small = (rx * 0.1).max(6.0);
        let mut pin = vec![
            Step::Move(cx - 2.0, y + 2.15 * rx),
            Step::Arc(rx * 0.23, rx * 0.23, 0, 0, cx - rx * 0.2, y + rx * 1.97),
            Step::Arc(rx, rx, 0, 1, cx - rx, y + rx),
            Step::Arc(rx, rx, 0, 1, cx, y),
            Step::Arc(rx, rx, 0, 1, cx + rx, y + rx),
            Step::Arc(rx, rx, 0, 1, cx + rx * 0.2, y + rx * 1.97),
            Step::Arc(rx * 0.23, rx * 0.23, 0, 0, cx + 2.0, y + 2.15 * rx),
        ];
        // A big enough pin tapers into its foot; a small one runs straight in.
        let waisted = rx * 0.04 > 4.0;
        if waisted {
            pin.push(Step::Line(cx + 2.0, bottom - rx * 0.22));
            pin.push(Step::Arc(
                rx * 0.05,
                rx * 0.05,
                0,
                0,
                cx + rx * 0.04,
                bottom - rx * 0.19,
            ));
        } else {
            pin.push(Step::Line(cx + 2.0, bottom - 2.0 * small));
        }
        pin.push(Step::Arc(small, small, 0, 1, cx + small, bottom - small));
        pin.push(Step::Arc(small, small, 0, 1, cx, bottom));
        pin.push(Step::Arc(small, small, 0, 1, cx - small, bottom - small));
        if waisted {
            pin.push(Step::Arc(
                small,
                small,
                0,
                1,
                cx - rx * 0.04,
                bottom - rx * 0.19,
            ));
            pin.push(Step::Arc(
                small * 0.5,
                small * 0.5,
                0,
                0,
                cx - 2.0,
                bottom - rx * 0.22,
            ));
        } else {
            pin.push(Step::Arc(
                small,
                small,
                0,
                1,
                cx - 2.0,
                bottom - 2.0 * small,
            ));
        }
        pin.push(Step::Close);
        // The two holes, wound the other way so the fill shows through them.
        let hole = |centre: f64, radius: f64| {
            vec![
                Step::Move(cx, centre - radius),
                Step::Arc(radius, radius, 0, 0, cx - radius, centre),
                Step::Arc(radius, radius, 0, 0, cx, centre + radius),
                Step::Arc(radius, radius, 0, 0, cx + radius, centre),
                Step::Arc(radius, radius, 0, 0, cx, centre - radius),
                Step::Close,
            ]
        };
        pin.extend(hole(y + rx, rx * 0.8));
        pin.extend(hole(bottom - small, small * 0.75));
        shade.paint(nodes, path_of(&pin), stroke.as_deref(), 1.0, false, None);
        return true;
    }
    // A portrait: the frame, then the face and shoulders drawn over it in the
    // style's second stroke colour.
    if style.get("shape") == Some("mxgraph.mockup.containers.userMale") {
        shade.paint(
            nodes,
            rectangle_path(rect),
            fill.as_deref(),
            1.0,
            true,
            None,
        );
        shade.paint(
            nodes,
            user_male(rect),
            None,
            1.0,
            false,
            Some(&style.text("strokecolor2", "#008CFF")),
        );
        return true;
    }
    // A map view: the streets and blocks, then the frame over the top of them.
    if style.get("shape") == Some("mxgraph.ios.iBgMap") {
        shade.paint(
            nodes,
            rectangle_path(rect),
            fill.as_deref(),
            1.0,
            true,
            None,
        );
        shade.line(
            nodes,
            ios_map(rect),
            Some(&style.text("strokecolor2", "#CCCCCC")),
            0.5,
        );
        shade.paint(nodes, rectangle_path(rect), None, 1.0, true, None);
        return true;
    }
    // A striped background: a rule every five pixels, framed.
    if style.get("shape") == Some("mxgraph.ios.iBgStriped") {
        shade.paint(
            nodes,
            rectangle_path(rect),
            fill.as_deref(),
            1.0,
            true,
            None,
        );
        let mut rules = Vec::new();
        let mut at = 5.0;
        while at < w && rules.len() < 4096 {
            rules.push(Step::Move(x + at, y));
            rules.push(Step::Line(x + at, bottom));
            at += 5.0;
        }
        shade.paint(
            nodes,
            path_of(&rules),
            None,
            1.0,
            false,
            Some(&style.text("strokecolor2", "#657E8F")),
        );
        shade.paint(nodes, rectangle_path(rect), None, 1.0, true, None);
        return true;
    }
    // A map pin, the iOS one: the same head and stem the mock-up pin has.
    if style.get("shape") == Some("mxgraph.ios.iPin") {
        let head = Rect {
            x,
            y,
            width: w,
            height: h * 0.4,
        };
        shade.line(
            nodes,
            line_path((cx, y + h * 0.4), (cx, bottom)),
            Some(&style.text("pinstemcolor", "#666666")),
            1.5,
        );
        shade.shade(
            nodes,
            ellipse_path(head),
            head,
            &style.text("fillcolor2", "#000000"),
            &style.text("fillcolor3", "#000000"),
            true,
        );
        shade.paint(
            nodes,
            ellipse_path(Rect {
                x: x + w * 0.2,
                y: y + h * 0.08,
                width: w * 0.3,
                height: h * 0.12,
            }),
            Some("#FFFFFF"),
            0.5,
            false,
            None,
        );
        return true;
    }
    // A media player bar: the track played so far, and the pause button.
    if style.get("shape") == Some("mxgraph.gmdl.player") {
        shade.paint(
            nodes,
            rectangle_path(rect),
            fill.as_deref(),
            1.0,
            true,
            None,
        );
        if h >= 4.0 {
            shade.paint(
                nodes,
                rectangle_path(Rect {
                    x,
                    y,
                    width: w * 0.8,
                    height: 4.0,
                }),
                Some(&style.text("progresscolor", "#FFED00")),
                1.0,
                false,
                None,
            );
        }
        if h >= 14.0 && w >= 33.0 {
            let icon = style.text("iconcolor", "#717171");
            for at in [33.0, 25.0] {
                shade.paint(
                    nodes,
                    rectangle_path(Rect {
                        x: right - at,
                        y: cy - 7.0,
                        width: 4.0,
                        height: 14.0,
                    }),
                    Some(&icon),
                    1.0,
                    false,
                    None,
                );
            }
        }
        return true;
    }
    // A logic gate: the body its operation asks for, the input and output
    // leads, and the bubble that negates it.
    if style.get("shape") == Some("mxgraph.electrical.logic_gates.logic_gate") {
        let operation = style.text("operation", "and");
        let inputs = style.number("numinputs", 2.0).clamp(1.0, 64.0);
        let spacing = h / inputs;
        let mut leads = vec![Step::Move(x + w * 0.8, cy), Step::Line(right, cy)];
        let reach = if operation == "and" { 0.2 } else { 0.23 };
        for index in 0..inputs as u32 {
            let at = y + spacing * (0.5 + f64::from(index));
            leads.push(Step::Move(x, at));
            leads.push(Step::Line(x + w * reach, at));
        }
        shade.paint(nodes, path_of(&leads), None, 1.0, true, None);
        // An exclusive gate carries a second arc in front of the body.
        if operation == "xor" {
            shade.paint(
                nodes,
                path_of(&[
                    Step::Move(x + w * 0.1, y),
                    Step::Arc(w * 0.6, h, 0, 1, x + w * 0.1, bottom),
                ]),
                None,
                1.0,
                true,
                None,
            );
        }
        let body = if operation == "or" || operation == "xor" {
            path_of(&[
                Step::Move(x + w * 0.4, y),
                Step::Arc(w * 0.45, h * 0.83, 0, 1, x + w * 0.8, cy),
                Step::Arc(w * 0.45, h * 0.83, 0, 1, x + w * 0.4, bottom),
                Step::Line(x + w * 0.15, bottom),
                Step::Arc(w * 0.6, h, 0, 0, x + w * 0.15, y),
                Step::Close,
            ])
        } else {
            path_of(&[
                Step::Move(x + w * 0.2, y),
                Step::Line(cx, y),
                Step::Arc(w * 0.3, h * 0.5, 0, 1, cx, bottom),
                Step::Line(x + w * 0.2, bottom),
                Step::Close,
            ])
        };
        shade.paint(nodes, body, fill.as_deref(), 1.0, true, None);
        if style.flag("negating") {
            shade.paint(
                nodes,
                ellipse_path(Rect {
                    x: x + w * 0.8,
                    y: cy - w * 0.05,
                    width: w * 0.1,
                    height: w * 0.1,
                }),
                fill.as_deref(),
                1.0,
                true,
                None,
            );
        }
        return true;
    }
    // A network bus: the same pipe an integration channel is drawn as, with a
    // white end cap rather than a shaded one.
    if style.get("shape") == Some("mxgraph.networks.bus") {
        let (top, foot) = (cy - 10.0, cy + 10.0);
        shade.paint(
            nodes,
            format!(
                "M {} {} A 12 12 0 0 1 {} {} L {} {} A 12 12 0 0 1 {} {} Z",
                n(x + 8.0),
                n(foot),
                n(x + 8.0),
                n(top),
                n(right - 8.0),
                n(top),
                n(right - 8.0),
                n(foot)
            ),
            fill.as_deref(),
            1.0,
            true,
            None,
        );
        shade.paint(
            nodes,
            format!(
                "M {} {} A 12 12 0 0 1 {} {} A 12 12 0 0 1 {} {} Z",
                n(right - 8.0),
                n(top),
                n(right - 8.0),
                n(foot),
                n(right - 8.0),
                n(top)
            ),
            Some("#FFFFFF"),
            1.0,
            true,
            None,
        );
        return true;
    }
    // A face: two eyes and a mouth that turns up, flat or down. Everything but
    // the face itself is drawn in the feature colour.
    if style.get("shape") == Some("smileyFace") {
        let radius = w.min(h) / 2.0;
        let ink = style.text("smileyfeaturecolor", "#666666");
        let unit = w.min(h) / 30.0;
        shade.paint(
            nodes,
            ellipse_path(Rect {
                x: cx - radius,
                y: cy - radius,
                width: 2.0 * radius,
                height: 2.0 * radius,
            }),
            fill.as_deref(),
            1.0,
            true,
            None,
        );
        for side in [-1.0, 1.0] {
            let eye = 1.5 * unit;
            shade.paint(
                nodes,
                ellipse_path(Rect {
                    x: cx + side * 5.0 * unit - eye,
                    y: cy - 5.0 * unit - eye,
                    width: 2.0 * eye,
                    height: 2.0 * eye,
                }),
                Some(&ink),
                1.0,
                false,
                Some(&ink),
            );
        }
        // A happy or sad mouth is a crescent; a neutral one is a line.
        let mouth = style.text("smileytype", "happy");
        if mouth == "neutral" {
            shade.line(
                nodes,
                line_path(
                    (cx - 5.0 * unit, cy + 7.0 * unit),
                    (cx + 5.0 * unit, cy + 7.0 * unit),
                ),
                Some(&ink),
                unit,
            );
        } else {
            let happy = mouth != "sad";
            let level = cy + if happy { 2.0 * unit } else { 7.0 * unit };
            let (from, back) = if happy { (1.0, -1.0) } else { (-1.0, 1.0) };
            shade.paint(
                nodes,
                format!(
                    "M {} {} A {r} {r} 0 1 1 {} {} L {} {} A {i} {i} 0 1 0 {} {} Z",
                    n(cx + from * 7.5 * unit),
                    n(level),
                    n(cx + back * 7.5 * unit),
                    n(level),
                    n(cx + back * 6.818 * unit),
                    n(level),
                    n(cx + from * 6.818 * unit),
                    n(level),
                    r = n(7.5 * unit),
                    i = n(6.818 * unit),
                ),
                Some("#000000"),
                1.0,
                false,
                Some(&ink),
            );
        }
        return true;
    }
    // A plain window: the frame, three buttons and the line under the title.
    if style.get("shape") == Some("mxgraph.mockup.containers.window") {
        let (w, h) = (w.max(90.0), h.max(30.0));
        let right = x + w;
        let close = style.text("strokecolor2", "#008CFF");
        let inside = style.text("strokecolor3", "#C4C4C4");
        shade.paint(
            nodes,
            rectangle_path(Rect {
                x,
                y,
                width: w,
                height: h,
            }),
            fill.as_deref(),
            1.0,
            true,
            None,
        );
        for (at, colour) in [
            (75.0, stroke.as_deref()),
            (50.0, stroke.as_deref()),
            (25.0, Some(close.as_str())),
        ] {
            shade.paint(
                nodes,
                ellipse_path(Rect {
                    x: right - at,
                    y: y + 5.0,
                    width: 20.0,
                    height: 20.0,
                }),
                None,
                1.0,
                false,
                colour,
            );
        }
        shade.paint(
            nodes,
            line_path((x, y + 30.0), (right, y + 30.0)),
            None,
            1.0,
            false,
            Some(&inside),
        );
        return true;
    }
    // A scroll bar: a track twenty pixels tall, an arrow at each end and the
    // handle where the bar has been dragged to.
    if style.get("shape") == Some("mxgraph.mockup.navigation.scrollBar") {
        let button = 20.0;
        let (w, h) = (w.max(2.0 * button), 20.0);
        let (right, bottom) = (x + w, y + h);
        shade.paint(
            nodes,
            rectangle_path(Rect {
                x,
                y,
                width: w,
                height: h,
            }),
            fill.as_deref(),
            1.0,
            true,
            None,
        );
        for at in [x + button, right - button] {
            shade.paint(
                nodes,
                line_path((at, y), (at, bottom)),
                None,
                1.0,
                true,
                None,
            );
        }
        let arrow_fill = style.text("fillcolor2", "#99DDFF");
        let arrow_edge = style.text("strokecolor2", "none");
        let arrow_edge = (arrow_edge != "none").then_some(arrow_edge);
        for pointing_left in [true, false] {
            let (tip, back) = if pointing_left {
                (x + button * 0.2, x + button * 0.8)
            } else {
                (right - button * 0.2, right - button * 0.8)
            };
            shade.paint(
                nodes,
                polygon_path(&[(tip, y + h * 0.5), (back, y + h * 0.2), (back, y + h * 0.8)]),
                Some(&arrow_fill),
                1.0,
                false,
                arrow_edge.as_deref(),
            );
        }
        // The handle keeps its own width and slides between the two arrows.
        let span = (w - 2.0 * button).max(0.0);
        let handle = 60.0_f64.min(span);
        let at =
            x + button + (span - handle) * style.number("barpos", 20.0).clamp(0.0, 100.0) / 100.0;
        shade.paint(
            nodes,
            rounded_rect_path(
                Rect {
                    x: at,
                    y: y + h * 0.15,
                    width: handle,
                    height: h * 0.7,
                },
                5.0,
            ),
            Some(&arrow_fill),
            1.0,
            false,
            arrow_edge.as_deref(),
        );
        return true;
    }
    // A number field with its own stepper down one side.
    if style.get("shape") == Some("mxgraph.mockup.forms.spinner") {
        let layout = style.text("spinnerlayout", "right");
        shade.paint(
            nodes,
            rounded_rect_path(rect, 10.0),
            Some(&style.text("fillcolor2", "#FFFFFF")),
            1.0,
            true,
            None,
        );
        // The stepper is twenty pixels along whichever side it sits on.
        let (divider, split) = match layout.as_str() {
            "left" => (
                line_path((x + 20.0, y), (x + 20.0, bottom)),
                line_path((x + 20.0, cy), (x, cy)),
            ),
            "top" => (
                line_path((x, y + 20.0), (right, y + 20.0)),
                line_path((cx, y + 20.0), (cx, y)),
            ),
            "bottom" => (
                line_path((x, bottom - 20.0), (right, bottom - 20.0)),
                line_path((cx, bottom - 20.0), (cx, bottom)),
            ),
            _ => (
                line_path((right - 20.0, y), (right - 20.0, bottom)),
                line_path((right - 20.0, cy), (right, cy)),
            ),
        };
        shade.paint(nodes, divider, None, 1.0, true, None);
        shade.paint(nodes, split, None, 1.0, true, None);
        return true;
    }
    // A group of checkboxes, one per option the style lists.
    if style.get("shape") == Some("mxgraph.mockup.forms.checkboxGroup") {
        let options = style.text("maintext", "Option 1");
        let count = options.split(',').count().max(1);
        let size = 15.0;
        let font = style.number("textsize", 17.0).max(1.0);
        let line = (font * 1.5).max(size);
        let full = line * count as f64;
        let h = h.max(full);
        shade.paint(
            nodes,
            rectangle_path(Rect {
                x,
                y,
                width: w,
                height: h,
            }),
            fill.as_deref(),
            1.0,
            true,
            None,
        );
        for (index, option) in options.split(',').enumerate().take(64) {
            let middle = y + (index as f64 * line + line * 0.5) * h / full;
            let box_top = middle - size * 0.5;
            shade.paint(
                nodes,
                rectangle_path(Rect {
                    x: x + size * 0.5,
                    y: box_top,
                    width: size,
                    height: size,
                }),
                fill.as_deref(),
                1.0,
                true,
                None,
            );
            // A leading `+` in the option marks the one that is ticked.
            if option.starts_with('+') {
                shade.paint(
                    nodes,
                    polyline_path(&[
                        (x + size * 0.5 + size * 0.8, box_top + size * 0.2),
                        (x + size * 0.5 + size * 0.4, box_top + size * 0.8),
                        (x + size * 0.5 + size * 0.25, box_top + size * 0.6),
                    ]),
                    None,
                    1.0,
                    true,
                    None,
                );
            }
        }
        return true;
    }
    // A map pin: a shaded head on a stem, with a highlight over the top.
    if style.get("shape") == Some("mxgraph.mockup.misc.pin") {
        let head = Rect {
            x,
            y,
            width: w,
            height: h * 0.4,
        };
        shade.line(
            nodes,
            line_path((cx, y + h * 0.4), (cx, bottom)),
            Some(&style.text("strokecolor2", "#666666")),
            3.0,
        );
        shade.shade(
            nodes,
            ellipse_path(head),
            head,
            &style.text("fillcolor2", "#000000"),
            &style.text("fillcolor3", "#000000"),
            true,
        );
        shade.paint(
            nodes,
            ellipse_path(Rect {
                x: x + w * 0.2,
                y: y + h * 0.08,
                width: w * 0.3,
                height: h * 0.12,
            }),
            Some(&style.text("fillcolor4", "#FFFFFF")),
            0.5,
            false,
            None,
        );
        return true;
    }
    // A rating: as many filled stars or hearts as the grade, then empty ones
    // up to the scale. Each is drawn in a box as wide as the shape is tall.
    if matches!(
        style.get("shape"),
        Some("mxgraph.bootstrap.rating" | "mxgraph.mockup.misc.rating")
    ) {
        let hearts = style.text("ratingstyle", "star") == "heart";
        let grade = style.number("grade", 5.0).clamp(0.0, 64.0);
        let scale = style.number("ratingscale", 10.0).clamp(0.0, 64.0);
        let empty = style.text("emptyfillcolor", "#FFFFFF");
        for index in 0..scale.max(grade) as u32 {
            let at = x + f64::from(index) * h * 1.2;
            let colour = if f64::from(index) < grade {
                fill.as_deref()
            } else {
                Some(empty.as_str())
            };
            shade.paint(
                nodes,
                rating_mark(at, y, h, hearts),
                colour,
                1.0,
                true,
                None,
            );
        }
        return true;
    }
    // A button rounded on its left edge, with a barber pole across it.
    if style.get("shape") == Some("mxgraph.bootstrap.leftButtonStriped") {
        let radius = 5.0_f64.min(w / 2.0).min(h / 2.0);
        let body = format!(
            "M {} {} L {} {} L {} {} A {r} {r} 0 0 1 {} {} L {} {} A {r} {r} 0 0 1 {} {} Z",
            n(right),
            n(y),
            n(right),
            n(bottom),
            n(x + radius),
            n(bottom),
            n(x),
            n(bottom - radius),
            n(x),
            n(y + radius),
            n(x + radius),
            n(y),
            r = n(radius),
        );
        shade.paint(nodes, body, fill.as_deref(), 1.0, false, None);
        // The stripes run at forty five degrees and are clipped to the button
        // by taking whichever of its edges each one reaches first.
        let stripe = h * 0.5;
        shade.paint(
            nodes,
            polygon_path(&[
                (x, y + h * 0.75),
                (x, y + h * 0.25),
                (x + h * 0.75, bottom),
                (x + h * 0.25, bottom),
            ]),
            Some("#FFFFFF"),
            0.2,
            false,
            None,
        );
        let mut at = stripe * 0.5;
        while at <= w && stripe > 0.0 {
            let mut points = vec![(x + at, y)];
            if at + stripe >= w {
                points.push((right, y));
                points.push((right, y + w - at));
            } else {
                points.push((x + at + stripe, y));
                if at + stripe + h > w {
                    points.push((right, y + w - at - stripe));
                    if w - at > h {
                        points.push((right, bottom));
                        points.push((x + at + h, bottom));
                    } else {
                        points.push((right, y + w - at));
                    }
                } else {
                    points.push((x + at + stripe + h, bottom));
                    points.push((x + at + h, bottom));
                }
            }
            shade.paint(
                nodes,
                polygon_path(&points),
                Some("#FFFFFF"),
                0.2,
                false,
                None,
            );
            at += 2.0 * stripe;
        }
        return true;
    }
    // A column chart: three pairs of bars in two colours, over axes drawn
    // twice as thick as the rest.
    if style.get("shape") == Some("mxgraph.mockup.graphics.columnChart") {
        shade.paint(
            nodes,
            rectangle_path(rect),
            fill.as_deref(),
            1.0,
            true,
            None,
        );
        let bar = style.text("strokecolor2", "none");
        let bar = (bar != "none").then_some(bar);
        let column = |at: f64, top: f64| {
            rectangle_path(Rect {
                x: x + w * at,
                y: y + h * top,
                width: w * 0.05,
                height: h * (1.0 - top),
            })
        };
        for (colour, bars) in [
            (
                style.text("fillcolor2", "#008CFF"),
                [(0.2, 0.25), (0.45, 0.4), (0.7, 0.05)],
            ),
            (
                style.text("fillcolor3", "#DDDDDD"),
                [(0.25, 0.15), (0.5, 0.35), (0.75, 0.2)],
            ),
        ] {
            for (at, top) in bars {
                shade.paint(
                    nodes,
                    column(at, top),
                    Some(&colour),
                    1.0,
                    false,
                    bar.as_deref(),
                );
            }
        }
        let axis = style.text("strokecolor3", "#666666");
        shade.line(
            nodes,
            polyline_path(&[(x, y), (x, bottom), (right, bottom)]),
            Some(&axis),
            shade.width * 2.0,
        );
        return true;
    }
    // A video player: the picture, a progress bar in two colours with a handle
    // where it has got to, and the transport controls under it.
    if style.get("shape") == Some("mxgraph.mockup.containers.videoPlayer") {
        let bar_height = style.number("barheight", 30.0).clamp(0.0, h);
        let button = style.text("fillcolor2", "#C4C4C4");
        let done = style.text("strokecolor2", "#008CFF");
        let togo = style.text("strokecolor3", "#C4C4C4");
        let reach = 8.0;
        let track = bottom - bar_height;
        let at = x
            + reach
            + (w - 2.0 * reach).max(0.0) * style.number("barpos", 20.0).clamp(0.0, 100.0) / 100.0;
        shade.paint(
            nodes,
            rectangle_path(rect),
            fill.as_deref(),
            1.0,
            true,
            None,
        );
        shade.line(
            nodes,
            line_path((x, track), (at, track)),
            Some(&done),
            shade.width,
        );
        shade.line(
            nodes,
            line_path((at, track), (right, track)),
            Some(&togo),
            shade.width,
        );
        // The handle, then the smaller ring inside it drawn half as thick.
        shade.paint(
            nodes,
            ellipse_path(Rect {
                x: at - reach,
                y: track - reach,
                width: 2.0 * reach,
                height: 2.0 * reach,
            }),
            fill.as_deref(),
            1.0,
            true,
            None,
        );
        shade.line(
            nodes,
            ellipse_path(Rect {
                x: at - reach * 0.5,
                y: track - reach * 0.5,
                width: reach,
                height: reach,
            }),
            None,
            shade.width / 2.0,
        );
        let icon = bar_height * 0.3;
        let top = bottom - (bar_height + icon) * 0.5;
        // Play, then the speaker with the two waves coming off it.
        shade.paint(
            nodes,
            polygon_path(&[
                (x + bar_height * 0.3, top),
                (x + bar_height * 0.3 + icon, top + icon * 0.5),
                (x + bar_height * 0.3, top + icon),
            ]),
            Some(&button),
            1.0,
            false,
            Some(&button),
        );
        let (speaker, level) = (x + bar_height, track);
        shade.paint(
            nodes,
            polygon_path(&[
                (speaker + bar_height * 0.05, level + bar_height * 0.4),
                (speaker + bar_height * 0.15, level + bar_height * 0.4),
                (speaker + bar_height * 0.3, level + bar_height * 0.25),
                (speaker + bar_height * 0.3, level + bar_height * 0.75),
                (speaker + bar_height * 0.15, level + bar_height * 0.6),
                (speaker + bar_height * 0.05, level + bar_height * 0.6),
            ]),
            Some(&button),
            1.0,
            false,
            Some(&button),
        );
        for (from, span, reach) in [(0.35, 0.65, (0.2, 0.3)), (0.25, 0.75, (0.225, 0.35))] {
            let start = if from == 0.35 { 0.4 } else { 0.425 };
            shade.paint(
                nodes,
                format!(
                    "M {} {} A {} {} 0 0 1 {} {}",
                    n(speaker + bar_height * start),
                    n(level + bar_height * from),
                    n(bar_height * reach.0),
                    n(bar_height * reach.1),
                    n(speaker + bar_height * start),
                    n(level + bar_height * span)
                ),
                None,
                1.0,
                false,
                Some(&button),
            );
        }
        // The four corners of the full-screen button.
        let screen = right - bar_height * 1.3;
        for (side, updown) in [(0.1, 0.3), (0.1, 0.7), (0.9, 0.3), (0.9, 0.7)] {
            let inward = if side < 0.5 { 0.25 } else { 0.75 };
            let back = if updown < 0.5 { 0.4 } else { 0.6 };
            shade.paint(
                nodes,
                polyline_path(&[
                    (screen + bar_height * side, level + bar_height * back),
                    (screen + bar_height * side, level + bar_height * updown),
                    (screen + bar_height * inward, level + bar_height * updown),
                ]),
                None,
                1.0,
                false,
                Some(&button),
            );
        }
        return true;
    }
    // An integration channel: a pipe shaded along its length, with the open
    // end drawn over it, and a marker for the letters that never arrive.
    if let Some(kind) = style
        .get("shape")
        .filter(|name| name.starts_with("mxgraph.eip.") && name.ends_with("Channel"))
    {
        let near = style.text("channelcolor1", "#E6E6E6");
        let far = style.text("channelcolor2", "#808080");
        let (top, foot) = (cy - 10.0, cy + 10.0);
        shade.shade(
            nodes,
            format!(
                "M {} {} A 12 12 0 0 1 {} {} L {} {} A 12 12 0 0 1 {} {} Z",
                n(x + 8.0),
                n(foot),
                n(x + 8.0),
                n(top),
                n(right - 8.0),
                n(top),
                n(right - 8.0),
                n(foot)
            ),
            rect,
            &near,
            &far,
            true,
        );
        shade.paint(
            nodes,
            format!(
                "M {} {} A 12 12 0 0 1 {} {} A 12 12 0 0 1 {} {} Z",
                n(right - 8.0),
                n(top),
                n(right - 8.0),
                n(foot),
                n(right - 8.0),
                n(top)
            ),
            Some(&near),
            1.0,
            true,
            None,
        );
        if kind.ends_with("deadLetterChannel") {
            let marker = style.text("markercolor", "#FF0000");
            shade.paint(
                nodes,
                polygon_path(&[
                    (cx - 6.0, cy - 3.0),
                    (cx - 3.0, cy - 6.0),
                    (cx + 3.0, cy - 6.0),
                    (cx + 6.0, cy - 3.0),
                    (cx + 6.0, cy + 3.0),
                    (cx + 3.0, cy + 6.0),
                    (cx - 3.0, cy + 6.0),
                    (cx - 6.0, cy + 3.0),
                ]),
                Some(&marker),
                1.0,
                true,
                None,
            );
            let bar = style.text("markericoncolor", "#FFFFFF");
            shade.line(
                nodes,
                line_path((cx - 4.0, cy), (cx + 4.0, cy)),
                Some(&bar),
                2.0,
            );
        }
        return true;
    }
    // An app tile: a rounded square shaded from one colour to another, with no
    // border of its own.
    if style.get("shape") == Some("mxgraph.ios7ui.icon") {
        shade.shade(
            nodes,
            oval_rect_path(rect, w * 0.1, h * 0.1),
            rect,
            &style.text("fillcolor2", "#00D0F0"),
            &style.text("fillcolor3", "#0080F0"),
            false,
        );
        return true;
    }
    // A placeholder image: a rounded frame with a filled panel inside it.
    if style.get("shape") == Some("mxgraph.bootstrap.image") {
        let radius = style.number("rsize", 10.0).max(0.0);
        shade.paint(
            nodes,
            rounded_rect_path(rect, radius),
            None,
            1.0,
            true,
            None,
        );
        let half = radius * 0.5;
        shade.paint(
            nodes,
            rounded_rect_path(
                Rect {
                    x: x + half,
                    y: y + half,
                    width: (w - radius).max(0.0),
                    height: (h - radius).max(0.0),
                },
                half,
            ),
            fill.as_deref(),
            1.0,
            false,
            None,
        );
        return true;
    }
    // A browser window: the frame, the tab and address bar in a third colour,
    // and the navigation icons filled in that same colour.
    if style.get("shape") == Some("mxgraph.mockup.containers.browserWindow") {
        // draw.io lays the chrome out at fixed pixel offsets, so the window has
        // a size below which the parts would overlap.
        let (w, h) = (w.max(260.0), h.max(110.0));
        let right = x + w;
        let close = style.text("strokecolor2", "#008CFF");
        let inside = style.text("strokecolor3", "#C4C4C4");
        shade.paint(
            nodes,
            rectangle_path(Rect {
                x,
                y,
                width: w,
                height: h,
            }),
            fill.as_deref(),
            1.0,
            true,
            None,
        );
        // Two window buttons, then the close button in its own colour.
        for (at, colour) in [
            (75.0, stroke.as_deref()),
            (50.0, stroke.as_deref()),
            (25.0, Some(close.as_str())),
        ] {
            shade.paint(
                nodes,
                ellipse_path(Rect {
                    x: right - at,
                    y: y + 5.0,
                    width: 20.0,
                    height: 20.0,
                }),
                None,
                1.0,
                false,
                colour,
            );
        }
        let ink = Some(inside.as_str());
        // The tab, the line under the toolbar, and the address field.
        shade.paint(nodes,
            format!(
                "M {} {} L {} {} L {} {} A 5 5 0 0 1 {} {} L {} {} A 5 5 0 0 1 {} {} L {} {} L {} {}",
                n(x),
                n(y + 40.0),
                n(x + 30.0),
                n(y + 40.0),
                n(x + 30.0),
                n(y + 15.0),
                n(x + 35.0),
                n(y + 10.0),
                n(x + 170.0),
                n(y + 10.0),
                n(x + 175.0),
                n(y + 15.0),
                n(x + 175.0),
                n(y + 40.0),
                n(right),
                n(y + 40.0),
            ),
            None,
            1.0,
            false,
            ink,
        );
        shade.paint(
            nodes,
            line_path((x, y + 110.0), (right, y + 110.0)),
            None,
            1.0,
            false,
            ink,
        );
        shade.paint(nodes,
            format!(
                "M {} {} A 5 5 0 0 1 {} {} L {} {} A 5 5 0 0 1 {} {} L {} {} A 5 5 0 0 1 {} {} L {} {} A 5 5 0 0 1 {} {} Z",
                n(x + 100.0),
                n(y + 60.0),
                n(x + 105.0),
                n(y + 55.0),
                n(right - 15.0),
                n(y + 55.0),
                n(right - 10.0),
                n(y + 60.0),
                n(right - 10.0),
                n(y + 85.0),
                n(right - 15.0),
                n(y + 90.0),
                n(x + 105.0),
                n(y + 90.0),
                n(x + 100.0),
                n(y + 85.0),
            ),
            None,
            1.0,
            false,
            ink,
        );
        // The page icon, once on the tab and once in the address bar.
        for (at, down) in [(37.0, 17.0), (107.0, 64.0)] {
            let (at, down) = (x + at, y + down);
            shade.paint(
                nodes,
                polygon_path(&[
                    (at, down),
                    (at + 11.0, down),
                    (at + 15.0, down + 4.0),
                    (at + 15.0, down + 18.0),
                    (at, down + 18.0),
                ]),
                None,
                1.0,
                false,
                ink,
            );
            shade.paint(
                nodes,
                polyline_path(&[
                    (at + 11.0, down),
                    (at + 11.0, down + 4.0),
                    (at + 15.0, down + 5.0),
                ]),
                None,
                1.0,
                false,
                ink,
            );
        }
        // Back, forward and reload, each twenty pixels across.
        let size = 20.0;
        let (back, down) = (x + 12.0, y + 64.0);
        let arrow = |at: f64, forward: bool| {
            let point = if forward { at + size } else { at };
            let spine = if forward { at } else { at + size };
            polygon_path(&[
                (point, down + size * 0.5),
                (at + size * 0.5, down),
                (at + size * 0.5, down + size * 0.3),
                (spine, down + size * 0.3),
                (spine, down + size * 0.7),
                (at + size * 0.5, down + size * 0.7),
                (at + size * 0.5, down + size),
            ])
        };
        shade.paint(nodes, arrow(back, false), ink, 1.0, false, ink);
        shade.paint(nodes, arrow(back + 30.0, true), ink, 1.0, false, ink);
        let reload = back + 60.0;
        shade.paint(
            nodes,
            format!(
                "M {} {} A {} {} 0 1 1 {} {} L {} {} L {} {} L {} {} L {} {} A {} {} 0 1 0 {} {} Z",
                n(reload + size * 0.78),
                n(down + size * 0.665),
                n(size * 0.3),
                n(size * 0.3),
                n(reload + size * 0.675),
                n(down + size * 0.252),
                n(reload + size * 0.595),
                n(down + size * 0.325),
                n(reload + size * 0.99),
                n(down + size * 0.415),
                n(reload + size * 0.9),
                n(down + size * 0.04),
                n(reload + size * 0.815),
                n(down + size * 0.12),
                n(size * 0.49),
                n(size * 0.49),
                n(reload + size * 0.92),
                n(down + size * 0.8),
            ),
            ink,
            1.0,
            false,
            ink,
        );
        return true;
    }
    // A C4 person: a head over a rounded body, with the head drawn again on top
    // so that the body's shoulder line stops at it.
    if style.get("shape") == Some("mxgraph.c4.person") {
        let head = (w / 2.0).min(h / 3.0);
        let radius = head / 2.0;
        let shoulder = y + head * 0.8;
        let face = Rect {
            x: cx - head * 0.5,
            y,
            width: head,
            height: head,
        };
        shade.paint(nodes, ellipse_path(face), fill.as_deref(), 1.0, true, None);
        shade.paint(
            nodes,
            format!(
                "M {} {} A {r} {r} 0 0 1 {} {} L {} {} A {r} {r} 0 0 1 {} {} L {} {} \
                 A {r} {r} 0 0 1 {} {} L {} {} A {r} {r} 0 0 1 {} {} Z",
                n(x),
                n(shoulder + radius),
                n(x + radius),
                n(shoulder),
                n(right - radius),
                n(shoulder),
                n(right),
                n(shoulder + radius),
                n(right),
                n(bottom - radius),
                n(right - radius),
                n(bottom),
                n(x + radius),
                n(bottom),
                n(x),
                n(bottom - radius),
                r = n(radius),
            ),
            fill.as_deref(),
            1.0,
            true,
            None,
        );
        shade.paint(nodes, ellipse_path(face), fill.as_deref(), 1.0, true, None);
        return true;
    }
    // The rack units that are more than one colour: a blanking plate shaded at
    // each end, a patch panel in its own body colour, and a cabinet frame.
    match style.get("shape") {
        Some("mxgraph.rackGeneral.plate") => {
            shade.paint(
                nodes,
                rectangle_path(rect),
                fill.as_deref(),
                1.0,
                true,
                None,
            );
            // Nine pixels at each end are the mounting ears, shaded darker.
            if w > 18.0 {
                for at in [x, right - 9.0] {
                    shade.paint(
                        nodes,
                        rectangle_path(Rect {
                            x: at,
                            y,
                            width: 9.0,
                            height: h,
                        }),
                        Some("#000000"),
                        0.23,
                        false,
                        None,
                    );
                }
                shade.paint(nodes, rectangle_path(rect), None, 1.0, true, None);
                shade.paint(
                    nodes,
                    rectangle_path(Rect {
                        x: x + 9.0,
                        y,
                        width: w - 18.0,
                        height: h,
                    }),
                    None,
                    1.0,
                    true,
                    None,
                );
            }
            return true;
        }
        Some("mxgraph.rackGeneral.neatPatch") => {
            let body = style.text("bodycolor", "#666666");
            shade.paint(nodes, rectangle_path(rect), Some(&body), 1.0, true, None);
            return true;
        }
        Some("mxgraph.rackGeneral.rackCabinet3") => {
            let unit = style.number("rackunitsize", 14.8).max(1.0);
            let numbered = style.text("numdisp", "descend") != "off";
            let font = style.number("textsize", 12.0).max(0.0);
            // The numbering column is outside the cabinet's own frame.
            let column = if numbered { font * 2.0 } else { 0.0 };
            let left = if numbered && style.text("rackunitdirleft", "1") != "0" {
                x + column
            } else {
                x
            };
            let width = w - column;
            // The frame is a whole number of rack units tall, plus its rails.
            let height = ((h - 42.0) / unit).round().max(0.0) * unit + 42.0;
            if width <= 0.0 {
                return false;
            }
            let panel = style.text("fillcolor2", "#ffffff");
            let rail = fill.clone().unwrap_or_else(|| "#F4F4F4".to_owned());
            shade.paint(
                nodes,
                rectangle_path(Rect {
                    x: left,
                    y,
                    width,
                    height,
                }),
                Some(&panel),
                1.0,
                true,
                None,
            );
            for bar in [
                Rect {
                    x: left,
                    y,
                    width,
                    height: 21.0,
                },
                Rect {
                    x: left,
                    y: y + height - 21.0,
                    width,
                    height: 21.0,
                },
                Rect {
                    x: left,
                    y: y + 21.0,
                    width: 9.0,
                    height: height - 42.0,
                },
                Rect {
                    x: left + width - 9.0,
                    y: y + 21.0,
                    width: 9.0,
                    height: height - 42.0,
                },
            ] {
                shade.paint(nodes, rectangle_path(bar), Some(&rail), 1.0, true, None);
            }
            // The four screws that hold the frame into the rack.
            for (at, down) in [
                (left + 2.5, y + 7.5),
                (left + width - 8.5, y + 7.5),
                (left + 2.5, y + height - 13.5),
                (left + width - 8.5, y + height - 13.5),
            ] {
                shade.paint(
                    nodes,
                    ellipse_path(Rect {
                        x: at,
                        y: down,
                        width: 6.0,
                        height: 6.0,
                    }),
                    None,
                    1.0,
                    true,
                    None,
                );
            }
            return true;
        }
        _ => {}
    }
    // ArchiMate 3 draws an element as a frame with a small badge in its top
    // right corner saying which kind of element it is. The frame's shape comes
    // from `archiType` and the badge from `appType` or `techType`.
    if let Some(kind) = style.get("shape").filter(|name| {
        matches!(
            *name,
            "mxgraph.archimate3.application" | "mxgraph.archimate3.tech"
        )
    }) {
        let technology = kind.ends_with(".tech");
        let frame = if technology {
            // A technology element is always drawn as a node box.
            archimate_badge("node", rect)
        } else {
            match style.text("architype", "square").as_str() {
                "rounded" => vec![(rounded_rect_path(rect, 10.0), true)],
                "oct" if w >= 20.0 && h >= 20.0 => vec![(
                    polygon_path(&[
                        (x, y + 10.0),
                        (x + 10.0, y),
                        (right - 10.0, y),
                        (right, y + 10.0),
                        (right, bottom - 10.0),
                        (right - 10.0, bottom),
                        (x + 10.0, bottom),
                        (x, bottom - 10.0),
                    ]),
                    true,
                )],
                _ => vec![(rectangle_path(rect), true)],
            }
        };
        for (part, filled) in frame {
            shade.paint(
                nodes,
                part,
                if filled { fill.as_deref() } else { None },
                1.0,
                true,
                None,
            );
        }
        // The badge sits fifteen pixels in from the corner, and is fifteen
        // across, whatever size the element itself is.
        let badge = if technology {
            Rect {
                x: right - 30.0,
                y: y + 15.0,
                width: 15.0,
                height: 15.0,
            }
        } else {
            Rect {
                x: right - 20.0,
                y: y + 5.0,
                width: 15.0,
                height: 15.0,
            }
        };
        let name = if technology {
            style.text("techtype", "")
        } else {
            style.text("apptype", "")
        };
        for (part, filled) in archimate_badge(&name, badge) {
            shade.paint(
                nodes,
                part,
                if filled { fill.as_deref() } else { None },
                1.0,
                true,
                None,
            );
        }
        // A goal's centre is filled solid in the stroke colour.
        if name == "goal" {
            shade.paint(
                nodes,
                ellipse_path(Rect {
                    x: badge.x + badge.width * 0.3,
                    y: badge.y + badge.height * 0.3,
                    width: badge.width * 0.4,
                    height: badge.height * 0.4,
                }),
                stroke.as_deref(),
                1.0,
                true,
                None,
            );
        }
        return true;
    }
    // The two ArchiMate elements that are a badge and nothing else.
    if let Some(name) = style
        .get("shape")
        .and_then(|name| name.strip_prefix("mxgraph.archimate3."))
        .filter(|name| matches!(*name, "service" | "actor"))
    {
        for (part, filled) in archimate_badge(name, rect) {
            shade.paint(
                nodes,
                part,
                if filled { fill.as_deref() } else { None },
                1.0,
                true,
                None,
            );
        }
        return true;
    }
    // A lorry: a body and a cab, on wheels that take the stroke colour.
    if style.get("shape") == Some("mxgraph.lean_mapping.truck_shipment") {
        shade.paint(
            nodes,
            rectangle_path(Rect {
                x,
                y,
                width: w * 0.6,
                height: h * 0.8,
            }),
            fill.as_deref(),
            1.0,
            true,
            None,
        );
        shade.paint(
            nodes,
            rectangle_path(Rect {
                x: x + w * 0.6,
                y: y + h * 0.35,
                width: w * 0.4,
                height: h * 0.45,
            }),
            fill.as_deref(),
            1.0,
            true,
            None,
        );
        for at in [0.15, 0.65] {
            shade.paint(
                nodes,
                ellipse_path(Rect {
                    x: x + w * at,
                    y: y + h * 0.8,
                    width: w * 0.2,
                    height: h * 0.2,
                }),
                stroke.as_deref(),
                1.0,
                true,
                None,
            );
        }
        return true;
    }
    // The iOS controls that carry a second colour of their own. Each keeps its
    // parts in the order draw.io paints them.
    match style.get("shape") {
        // A row of page dots: the one for the page you are on is the odd one
        // out, so it takes the fill and the rest take the stroke colour.
        Some("mxgraph.ios7ui.pageControl") => {
            let radius = (h * 0.5).min(w * 0.05);
            let dot = |at: f64, colour: Option<&str>| {
                (
                    ellipse_path(Rect {
                        x: at,
                        y: cy - radius,
                        width: 2.0 * radius,
                        height: 2.0 * radius,
                    }),
                    colour.map(str::to_owned),
                )
            };
            let dots = [
                dot(x, stroke.as_deref()),
                dot(x + w * 0.25 - radius * 0.5, stroke.as_deref()),
                dot(cx - radius, stroke.as_deref()),
                dot(x + w * 0.75 - radius * 1.5, stroke.as_deref()),
                dot(right - 2.0 * radius, fill.as_deref()),
            ];
            for (d, colour) in dots {
                shade.paint(nodes, d, colour.as_deref(), 1.0, false, None);
            }
            return true;
        }
        // A progress bar: the part already downloaded is drawn over the whole
        // track in the other colour. Both are two pixel lines.
        Some("mxgraph.ios7ui.downloadBar") => {
            let done = w * style.number("barpos", 80.0).clamp(0.0, 100.0) / 100.0;
            let bar = |span: f64| {
                rectangle_path(Rect {
                    x,
                    y: cy - 1.0,
                    width: span,
                    height: 2.0,
                })
            };
            shade.paint(nodes, bar(w), fill.as_deref(), 1.0, false, None);
            shade.paint(nodes, bar(done), stroke.as_deref(), 1.0, false, None);
            return true;
        }
        // A slider: the same track, plus a handle sitting where it is set to.
        Some("mxgraph.ios7ui.slider") => {
            let track = style.text("barcolor", "#bbbbbb");
            let at = x + w * (style.number("barpos", 40.0) / 100.0).clamp(0.0, 1.0);
            let size = style.number("handlesize", 10.0).max(0.0);
            let bar = |to: f64| line_path((x, cy), (to, cy));
            shade.paint(nodes, bar(right), None, 1.0, false, Some(&track));
            shade.paint(nodes, bar(at), None, 1.0, true, None);
            shade.paint(
                nodes,
                ellipse_path(Rect {
                    x: at - size * 0.5,
                    y: cy - size * 0.5,
                    width: size,
                    height: size,
                }),
                fill.as_deref(),
                1.0,
                false,
                Some(&track),
            );
            return true;
        }
        // A switch: a pill with a handle at one end. Off, it is drawn in the
        // second pair of colours the style carries.
        Some("mxgraph.ios7ui.onOffButton") => {
            let on = style.text("buttonstate", "on") != "off";
            let w = w.max(2.0 * h);
            let pill = Rect {
                x,
                y,
                width: w,
                height: h,
            };
            let off_fill = style.text("fillcolor2", "#ffffff");
            let off_stroke = style.text("strokecolor2", "#aaaaaa");
            let handle = style.text("handlecolor", "#ffffff");
            if on {
                shade.paint(
                    nodes,
                    rounded_rect_path(pill, h * 0.5),
                    fill.as_deref(),
                    1.0,
                    true,
                    None,
                );
                shade.paint(
                    nodes,
                    ellipse_path(Rect {
                        x: x + w - h + 1.0,
                        y: y + 1.0,
                        width: (h - 2.0).max(0.0),
                        height: (h - 2.0).max(0.0),
                    }),
                    Some(&handle),
                    1.0,
                    false,
                    None,
                );
            } else {
                shade.paint(
                    nodes,
                    rounded_rect_path(pill, h * 0.5),
                    Some(&off_fill),
                    1.0,
                    false,
                    Some(&off_stroke),
                );
                shade.paint(
                    nodes,
                    ellipse_path(Rect {
                        x,
                        y,
                        width: h,
                        height: h,
                    }),
                    None,
                    1.0,
                    false,
                    Some(&off_stroke),
                );
            }
            return true;
        }
        // The bar across the top of a phone screen: signal, wifi, battery and
        // a back arrow, all drawn in the second fill colour.
        Some("mxgraph.ios7ui.appBar") => {
            let ink = style.text("fillcolor2", "#222222");
            let ink = Some(ink.as_str());
            shade.paint(
                nodes,
                rectangle_path(rect),
                fill.as_deref(),
                1.0,
                false,
                None,
            );
            let dot = |at: f64, top: f64, size: f64| {
                ellipse_path(Rect {
                    x: x + at,
                    y: cy + top,
                    width: size,
                    height: size,
                })
            };
            for at in [5.0, 9.0, 13.0, 17.0, 21.0] {
                shade.paint(nodes, dot(at, -1.5, 3.0), ink, 1.0, false, None);
            }
            shade.paint(nodes, dot(54.0, 2.0, 2.0), ink, 1.0, false, None);
            // Two arcs over the dot make the wifi fan.
            for (span, from, lift) in [(3.5, 52.0, 1.0), (6.0, 50.0, -1.0)] {
                shade.paint(
                    nodes,
                    format!(
                        "M {} {} A {} {} 0 0 1 {} {}",
                        n(x + from),
                        n(cy + lift),
                        n(span),
                        n(span),
                        n(x + from + 2.0 * span),
                        n(cy + lift)
                    ),
                    None,
                    1.0,
                    false,
                    ink,
                );
            }
            // The charge inside the battery, then the play glyph beside it.
            shade.paint(
                nodes,
                polygon_path(&[
                    (right - 19.0, cy - 2.0),
                    (right - 6.0, cy - 2.0),
                    (right - 6.0, cy + 2.0),
                    (right - 19.0, cy + 2.0),
                ]),
                ink,
                1.0,
                false,
                None,
            );
            shade.paint(
                nodes,
                polyline_path(&[
                    (right - 44.0, cy - 2.5),
                    (right - 36.0, cy + 2.5),
                    (right - 40.0, cy + 5.0),
                    (right - 40.0, cy - 5.0),
                    (right - 36.0, cy - 2.5),
                    (right - 44.0, cy + 2.5),
                ]),
                None,
                1.0,
                false,
                ink,
            );
            // The battery case, with its terminal on the right.
            shade.paint(
                nodes,
                polygon_path(&[
                    (right - 20.0, cy - 3.0),
                    (right - 5.0, cy - 3.0),
                    (right - 5.0, cy - 1.0),
                    (right - 3.5, cy - 1.0),
                    (right - 3.5, cy + 1.0),
                    (right - 5.0, cy + 1.0),
                    (right - 5.0, cy + 3.0),
                    (right - 20.0, cy + 3.0),
                ]),
                None,
                1.0,
                false,
                ink,
            );
            return true;
        }
        _ => {}
    }
    // An activity's end: a ring with a solid disc inside it, and the disc takes
    // the stroke colour, the way a radio button's dot does.
    if style.get("shape") == Some("mxgraph.sysml.actFinal") {
        shade.paint(nodes, ellipse_path(rect), fill.as_deref(), 1.0, true, None);
        shade.paint(
            nodes,
            ellipse_path(Rect {
                x: x + 5.0,
                y: y + 5.0,
                width: (w - 10.0).max(0.0),
                height: (h - 10.0).max(0.0),
            }),
            stroke.as_deref(),
            1.0,
            true,
            None,
        );
        return true;
    }
    // A radio button fills its dot with the stroke colour rather than the fill.
    if style.get("shape") == Some("mxgraph.bootstrap.radioButton") {
        shade.paint(nodes, ellipse_path(rect), fill.as_deref(), 1.0, true, None);
        shade.paint(
            nodes,
            ellipse_path(Rect {
                x: x + w * 0.25,
                y: y + h * 0.25,
                width: w * 0.5,
                height: h * 0.5,
            }),
            stroke.as_deref(),
            1.0,
            false,
            None,
        );
        return true;
    }
    // Three shapes built the same way: an outline, a light face on the left and
    // a dark one on the right, then the outline stroked over the top of both.
    if let Some((outline, light, dark)) = match style.get("shape") {
        Some("mxgraph.infographic.shadedTriangle") => Some((
            vec![(x, bottom), (x + w * 0.5, y), (right, bottom)],
            vec![(x, bottom), (x + w * 0.5, y), (x + w * 0.5, y + h * 0.67)],
            vec![
                (right, bottom),
                (x + w * 0.5, y + h * 0.67),
                (x + w * 0.5, y),
            ],
        )),
        Some("mxgraph.infographic.shadedPyramid") => {
            let ridge = bottom - (w * 0.3).min(h);
            Some((
                vec![
                    (x, ridge),
                    (x + w * 0.5, y),
                    (right, ridge),
                    (x + w * 0.5, bottom),
                ],
                vec![(x, ridge), (x + w * 0.5, y), (x + w * 0.5, bottom)],
                vec![(right, ridge), (x + w * 0.5, bottom), (x + w * 0.5, y)],
            ))
        }
        Some("mxgraph.infographic.pyramidStep") => {
            let ridge = y + (w * 0.1).min(h);
            Some((
                vec![
                    (x, ridge),
                    (x + w * 0.5, y),
                    (right, ridge),
                    (right, bottom),
                    (x, bottom),
                ],
                vec![
                    (x, ridge),
                    (x + w * 0.5, y),
                    (x + w * 0.5, bottom),
                    (x, bottom),
                ],
                vec![
                    (right, ridge),
                    (right, bottom),
                    (x + w * 0.5, bottom),
                    (x + w * 0.5, y),
                ],
            ))
        }
        _ => None,
    } {
        shade.paint(
            nodes,
            polygon_path(&outline),
            fill.as_deref(),
            1.0,
            false,
            None,
        );
        shade.paint(
            nodes,
            polygon_path(&light),
            Some("#FFFFFF"),
            0.2,
            false,
            None,
        );
        shade.paint(
            nodes,
            polygon_path(&dark),
            Some("#000000"),
            0.2,
            false,
            None,
        );
        shade.paint(nodes, polygon_path(&outline), None, 1.0, true, None);
        return true;
    }
    // A ribbon banner, with its folded ends shaded darker than its face.
    if let Some(shape) = style.get("shape").filter(|name| {
        matches!(
            *name,
            "mxgraph.infographic.banner" | "mxgraph.infographic.bannerSingleFold"
        )
    }) {
        let single = shape.ends_with("SingleFold");
        let dy = style.number("dy", 0.5).clamp(0.0, h * 0.5);
        let dx = style
            .number("dx", 0.5)
            .clamp(0.0, if single { w } else { w / 2.0 })
            .min(if single { w } else { w / 2.0 } - 2.0 * dy);
        let dx = dx.max(0.0);
        let notch = style.number("notch", 0.5).clamp(0.0, w).min(dx);
        // The point of the notch cut into each end of the ribbon.
        let waist = y + (h - dy) * 0.5 + dy;
        let (fold, tail) = (right - dx, right - dx - 2.0 * dy);
        let right_end = vec![
            (right, y + dy),
            (fold, y + dy),
            (fold, bottom - dy),
            (tail, bottom),
            (right, bottom),
            (right - notch, waist),
        ];
        let right_fold = vec![(fold, bottom - dy), (tail, bottom - dy), (tail, bottom)];
        let outline = if single {
            let dx2 = style
                .number("dx2", 0.5)
                .clamp(0.0, (w - dx - 2.0 * dy).max(0.0));
            vec![
                (x + dx2, y),
                (fold, y),
                (fold, y + dy),
                (right, y + dy),
                (right - notch, waist),
                (right, bottom),
                (tail, bottom),
                (tail, bottom - dy),
                (x + dx2, bottom - dy),
                (x, y + (h - dy) * 0.5),
            ]
        } else {
            vec![
                (x, y + dy),
                (x + dx, y + dy),
                (x + dx, y),
                (fold, y),
                (fold, y + dy),
                (right, y + dy),
                (right - notch, waist),
                (right, bottom),
                (tail, bottom),
                (tail, bottom - dy),
                (x + dx + 2.0 * dy, bottom - dy),
                (x + dx + 2.0 * dy, bottom),
                (x, bottom),
                (x + notch, waist),
            ]
        };
        shade.paint(
            nodes,
            polygon_path(&outline),
            fill.as_deref(),
            1.0,
            true,
            None,
        );
        // A single fold has only the one shaded end, and shades it faintly.
        shade.paint(
            nodes,
            polygon_path(&right_end),
            Some("#000000"),
            if single { 0.05 } else { 0.2 },
            false,
            None,
        );
        if !single {
            shade.paint(
                nodes,
                polygon_path(&[
                    (x, y + dy),
                    (x + dx, y + dy),
                    (x + dx, bottom - dy),
                    (x + dx + 2.0 * dy, bottom),
                    (x, bottom),
                    (x + notch, waist),
                ]),
                Some("#000000"),
                0.2,
                false,
                None,
            );
        }
        shade.paint(
            nodes,
            polygon_path(&right_fold),
            Some("#000000"),
            0.4,
            false,
            None,
        );
        if !single {
            shade.paint(
                nodes,
                polygon_path(&[
                    (x + dx, bottom - dy),
                    (x + dx + 2.0 * dy, bottom - dy),
                    (x + dx + 2.0 * dy, bottom),
                ]),
                Some("#000000"),
                0.4,
                false,
                None,
            );
        }
        if single {
            shade.paint(nodes, polygon_path(&outline), None, 1.0, true, None);
        }
        return true;
    }
    // A pie chart: each slice takes the next colour the style lists.
    let parts = style.text("parts", "10,20,30");
    let values = parts
        .split(',')
        .filter_map(|value| value.trim().parse::<f64>().ok())
        .filter(|value| value.is_finite() && *value > 0.0)
        .take(64)
        .collect::<Vec<_>>();
    let total = values.iter().sum::<f64>();
    if values.is_empty() || total <= 0.0 {
        return false;
    }
    let colours = style.text("partcolors", "#333333,#666666,#999999");
    let colours = colours
        .split(',')
        .filter_map(parse_color)
        .collect::<Vec<_>>();
    shade.paint(nodes, ellipse_path(rect), fill.as_deref(), 1.0, true, None);
    let (cx, cy) = rect.center();
    let (rx, ry) = (w / 2.0, h / 2.0);
    let mut travelled = 0.0;
    for (slice, value) in values.iter().enumerate() {
        let start = travelled / total * 2.0 * PI;
        travelled += value;
        let end = travelled / total * 2.0 * PI;
        let large = u8::from(end - start >= PI);
        let at = |angle: f64| (cx + angle.sin() * rx, cy - angle.cos() * ry);
        shade.paint(
            nodes,
            format!(
                "M {} {} L {} {} A {} {} 0 {large} 1 {} {} Z",
                n(cx),
                n(cy),
                n(at(start).0),
                n(at(start).1),
                n(rx),
                n(ry),
                n(at(end).0),
                n(at(end).1)
            ),
            colours
                .get(slice % colours.len().max(1))
                .map(String::as_str)
                .or(Some("#FF0000")),
            1.0,
            true,
            None,
        );
    }
    true
}

/// The sheen `glass=1` lays over the top of a shape.
///
/// mxGraph fills a lens over the top four tenths, bowed down in the middle,
/// with white fading from nine tenths opaque to one tenth.
fn glass_node(id: &str, rect: Rect, style: &Style, transform: Matrix) -> Node {
    let stroke = (style.number("strokewidth", 1.0).max(0.0) / 2.0).ceil();
    let arc = if style.flag("rounded") {
        corner_radius(rect, style) + 2.0 * stroke
    } else {
        0.0
    };
    let Rect { x, y, .. } = rect;
    let (w, h) = (rect.width, rect.height);
    let lens = h * 0.4;
    let d = if arc > 0.0 {
        format!(
            "M {} {} Q {} {} {} {} L {} {} Q {} {} {} {} L {} {} Q {} {} {} {} Z",
            n(x - stroke + arc),
            n(y - stroke),
            n(x - stroke),
            n(y - stroke),
            n(x - stroke),
            n(y - stroke + arc),
            n(x - stroke),
            n(y + lens),
            n(x + w * 0.5),
            n(y + h * 0.7),
            n(x + w + stroke),
            n(y + lens),
            n(x + w + stroke),
            n(y - stroke + arc),
            n(x + w + stroke),
            n(y - stroke),
            n(x + w + stroke - arc),
            n(y - stroke)
        )
    } else {
        format!(
            "M {} {} L {} {} Q {} {} {} {} L {} {} Z",
            n(x - stroke),
            n(y - stroke),
            n(x - stroke),
            n(y + lens),
            n(x + w * 0.5),
            n(y + h * 0.7),
            n(x + w + stroke),
            n(y + lens),
            n(x + w + stroke),
            n(y - stroke)
        )
    };
    Node::Path {
        id: id.to_owned(),
        d,
        fill_rule: "nonzero".into(),
        fill: Paint::LinearGradient(Box::new(LinearGradient {
            x1: x,
            y1: y,
            x2: x,
            y2: y + h * 0.6,
            stops: vec![
                GradientStop {
                    offset: 0.0,
                    color: "#FFFFFF".into(),
                    opacity: 0.9,
                },
                GradientStop {
                    offset: 1.0,
                    color: "#FFFFFF".into(),
                    opacity: 0.1,
                },
            ],
        })),
        stroke: Stroke::default(),
        transform,
        clip_id: None,
        meta: SourceMeta {
            kind: "drawio-glass".into(),
            ..SourceMeta::default()
        },
    }
}

/// The glyph ArchiMate 3 draws for one kind of element, inside `rect`.
///
/// Each part is paired with whether it takes the element's fill; a line drawn
/// across a badge, such as an assessment's handle or a figure's arms, is
/// stroked only. An element type this does not know returns nothing, which
/// leaves the frame drawn and the corner empty.
fn archimate_badge(kind: &str, rect: Rect) -> Vec<(String, bool)> {
    let Rect { x, y, .. } = rect;
    let (w, h) = (rect.width, rect.height);
    let (right, bottom) = (rect.right(), rect.bottom());
    // Several badges are drawn inset from the box they are given.
    let inset = |top: f64| Rect {
        x,
        y: y + top,
        width: w,
        height: (h - 2.0 * top).max(0.0),
    };
    match kind {
        // A ring with a solid centre: something the design is aiming at.
        "goal" => {
            let ring = |share: f64| {
                ellipse_path(Rect {
                    x: x + w * (1.0 - share) * 0.5,
                    y: y + h * (1.0 - share) * 0.5,
                    width: w * share,
                    height: h * share,
                })
            };
            vec![(ring(1.0), true), (ring(0.7), false)]
        }
        // A leaning box: something the design has to satisfy.
        "requirement" => vec![(
            polygon_path(&[
                (x + w * 0.25, y),
                (right, y),
                (right - w * 0.25, bottom),
                (x, bottom),
            ]),
            true,
        )],
        // A magnifying glass: something the design has looked into.
        "assess" => vec![
            (
                ellipse_path(Rect {
                    x: x + w * 0.2,
                    y,
                    width: w * 0.8,
                    height: h * 0.8,
                }),
                true,
            ),
            (line_path((x, bottom), (x + w * 0.32, y + h * 0.68)), false),
        ],
        // A box drawn in three dimensions: a node.
        "node" => vec![(
            format!(
                "{} {}",
                polygon_path(&[
                    (x, y + h * 0.25),
                    (x + w * 0.25, y),
                    (right, y),
                    (right, bottom - h * 0.25),
                    (right - w * 0.25, bottom),
                    (x, bottom),
                ]),
                polyline_path(&[
                    (x, y + h * 0.25),
                    (right - w * 0.25, y + h * 0.25),
                    (right - w * 0.25, bottom),
                ])
            ),
            true,
        )],
        "func" => vec![(
            polygon_path(&[
                (x + w * 0.5, y),
                (right, y + h * 0.2),
                (right, bottom),
                (x + w * 0.5, bottom - h * 0.2),
                (x, bottom),
                (x, y + h * 0.2),
            ]),
            true,
        )],
        // Two overlapping circles: two parties working together.
        "collab" => {
            let inner = inset(3.0);
            vec![
                (
                    ellipse_path(Rect {
                        width: inner.width * 0.6,
                        ..inner
                    }),
                    true,
                ),
                (
                    ellipse_path(Rect {
                        x: inner.x + inner.width * 0.4,
                        width: inner.width * 0.6,
                        ..inner
                    }),
                    true,
                ),
            ]
        }
        // An arrow notched at the back: something that happens.
        "event" => {
            let inner = inset(3.0);
            let (top, foot) = (inner.y, inner.bottom());
            let reach = inner.height * 0.5;
            vec![(
                format!(
                    "M {} {} A {} {} 0 0 1 {} {} L {} {} L {} {} L {} {} Z",
                    n(inner.right() - reach),
                    n(top),
                    n(reach),
                    n(reach),
                    n(inner.right() - reach),
                    n(foot),
                    n(inner.x),
                    n(foot),
                    n(inner.x + reach),
                    n(top + reach),
                    n(inner.x),
                    n(top)
                ),
                true,
            )]
        }
        // A thick arrow: something that runs from one state to another.
        "proc" => vec![(
            polygon_path(&[
                (x, y + h * 0.3),
                (x + w * 0.6, y + h * 0.3),
                (x + w * 0.6, y),
                (right, y + h * 0.5),
                (x + w * 0.6, bottom),
                (x + w * 0.6, bottom - h * 0.3),
                (x, bottom - h * 0.3),
            ]),
            true,
        )],
        // A stick figure: somebody who acts.
        "actor" => vec![
            (
                ellipse_path(Rect {
                    x: x + w * 0.2,
                    y,
                    width: w * 0.6,
                    height: h * 0.3,
                }),
                true,
            ),
            (
                line_path((x + w * 0.5, y + h * 0.3), (x + w * 0.5, y + h * 0.75)),
                false,
            ),
            (line_path((x, y + h * 0.45), (right, y + h * 0.45)), false),
            (
                polyline_path(&[(x, bottom), (x + w * 0.5, y + h * 0.75), (right, bottom)]),
                false,
            ),
        ],
        // A rounded bar: something offered to somebody else.
        "serv" | "service" => {
            let inner = if kind == "serv" { inset(3.0) } else { rect };
            let (near, far) = (
                (inner.width - inner.height * 0.5).max(inner.width * 0.5),
                (inner.height * 0.5).min(inner.width * 0.5),
            );
            let reach = inner.height * 0.5;
            vec![(
                format!(
                    "M {} {} A {} {} 0 0 1 {} {} L {} {} A {} {} 0 0 1 {} {} Z",
                    n(inner.x + near),
                    n(inner.y),
                    n(reach),
                    n(reach),
                    n(inner.x + near),
                    n(inner.bottom()),
                    n(inner.x + far),
                    n(inner.bottom()),
                    n(reach),
                    n(reach),
                    n(inner.x + far),
                    n(inner.y)
                ),
                true,
            )]
        }
        // Two overlapping discs: the system software a node runs.
        "sysSw" => vec![
            (
                ellipse_path(Rect {
                    x: x + w * 0.3,
                    y,
                    width: w * 0.7,
                    height: h * 0.7,
                }),
                true,
            ),
            (
                ellipse_path(Rect {
                    x,
                    y: y + h * 0.02,
                    width: w * 0.98,
                    height: h * 0.98,
                }),
                true,
            ),
        ],
        _ => Vec::new(),
    }
}

/// Paints one part of a shape draw.io draws in more than one colour.
///
/// The parts of such a shape are painted in the order the editor paints them,
/// each with its own fill and stroke, so the painter carries the shape's own
/// colours and hands out a distinct id per part.
struct Shading {
    identity: String,
    index: usize,
    stroke: Option<String>,
    width: f64,
    opacity: f64,
    transform: Matrix,
    source: String,
}

impl Shading {
    /// `edge` names a colour to stroke with instead of the shape's own, for
    /// the parts draw.io paints in a second colour the style carries
    /// separately; `stroked` asks for the shape's own stroke.
    fn paint(
        &mut self,
        nodes: &mut Vec<Node>,
        d: String,
        fill: Option<&str>,
        alpha: f64,
        stroked: bool,
        edge: Option<&str>,
    ) {
        let paint = fill.map_or(Paint::None, |color| Paint::Solid {
            color: color.to_owned(),
            opacity: alpha * self.opacity,
        });
        self.push(nodes, d, paint, stroked, edge, None);
    }

    /// The same, for a part filled with a gradient rather than a flat colour.
    fn shade(
        &mut self,
        nodes: &mut Vec<Node>,
        d: String,
        rect: Rect,
        from: &str,
        to: &str,
        stroked: bool,
    ) {
        let paint = match (parse_color(from), parse_color(to)) {
            (Some(from), Some(to)) => {
                Paint::LinearGradient(Box::new(gradient(rect, &from, &to, "south", self.opacity)))
            }
            _ => Paint::None,
        };
        self.push(nodes, d, paint, stroked, None, None);
    }

    /// The same, for a part draw.io strokes thicker or thinner than the rest.
    fn line(&mut self, nodes: &mut Vec<Node>, d: String, edge: Option<&str>, thickness: f64) {
        self.push(nodes, d, Paint::None, true, edge, Some(thickness));
    }

    fn push(
        &mut self,
        nodes: &mut Vec<Node>,
        d: String,
        fill: Paint,
        stroked: bool,
        edge: Option<&str>,
        thickness: Option<f64>,
    ) {
        self.index += 1;
        nodes.push(Node::Path {
            id: format!("{}-part-{}", self.identity, self.index),
            d,
            fill_rule: "nonzero".into(),
            fill,
            stroke: match (stroked || edge.is_some(), edge.or(self.stroke.as_deref())) {
                (true, Some(color)) => Stroke {
                    paint: Paint::Solid {
                        color: color.to_owned(),
                        opacity: self.opacity,
                    },
                    width: thickness.unwrap_or(self.width),
                    ..Stroke::default()
                },
                _ => Stroke::default(),
            },
            transform: self.transform,
            clip_id: None,
            meta: SourceMeta {
                kind: "drawio-shaded".into(),
                source_id: self.source.clone(),
                ..SourceMeta::default()
            },
        });
    }
}

/// One mark of a rating, in a box as wide as the rating is tall.
///
/// draw.io spaces the marks one and a fifth of that box apart, and draws a
/// star from straight edges and a heart from four curves.
fn rating_mark(at: f64, y: f64, size: f64, heart: bool) -> String {
    if !heart {
        return polygon_path(&[
            (at, y + 0.33 * size),
            (at + 0.364 * size, y + 0.33 * size),
            (at + 0.475 * size, y),
            (at + 0.586 * size, y + 0.33 * size),
            (at + 0.95 * size, y + 0.33 * size),
            (at + 0.66 * size, y + 0.551 * size),
            (at + 0.775 * size, y + 0.9 * size),
            (at + 0.475 * size, y + 0.684 * size),
            (at + 0.175 * size, y + 0.9 * size),
            (at + 0.29 * size, y + 0.551 * size),
        ]);
    }
    let across = |share: f64| n(at + share * size);
    let down = |share: f64| n(y + share * size);
    format!(
        "M {} {} C {} {} {} {} {} {} C {} {} {} {} {} {} C {} {} {} {} {} {} \
         C {} {} {} {} {} {} C {} {} {} {} {} {} Z",
        across(0.519),
        down(0.947),
        across(0.558),
        down(0.908),
        across(0.778),
        down(0.682),
        across(0.916),
        down(0.54),
        across(1.039),
        down(0.414),
        across(1.036),
        down(0.229),
        across(0.924),
        down(0.115),
        across(0.812),
        down(0.0),
        across(0.631),
        down(0.0),
        across(0.519),
        down(0.115),
        across(0.408),
        down(0.0),
        across(0.227),
        down(0.0),
        across(0.115),
        down(0.115),
        across(0.03),
        down(0.229),
        across(0.0),
        down(0.414),
        across(0.123),
        down(0.54),
    )
}
