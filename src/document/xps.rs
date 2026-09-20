//! Bounded XPS/OpenXPS fixed-page reader.
//!
//! The importer follows the OPC fixed-representation relationship and renders
//! basic Path and Glyphs page markings. Unsupported brushes and resource
//! references are reported rather than fetched or silently interpreted.

use std::collections::HashMap;
use std::collections::HashSet;
use std::fs::File;
use std::io::Cursor;
use std::path::Path;

use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use quick_xml::Reader;
use quick_xml::events::{BytesStart, Event};

use crate::convert::{ConvertOptions, PageConsumer};
use crate::error::{Error, Result};
use crate::ir::{
    ClipPath, IDENTITY, LineCap, LineJoin, Node, Page, Paint, SourceMeta, Stroke, TextAnchor,
    TextRun,
};
use crate::ooxml::{ZipPackage, attribute, local_name, resolve_part_target, sniff_image_mime};
use crate::svg::reader::{SvgPathTokenizer, decompose_svg_path};

const MAX_XPS_ARCHIVE_ENTRIES: usize = 100_000;
const MAX_XPS_DOCUMENTS: usize = 10_000;
const MAX_XPS_PAGES: usize = 20_000;
const MAX_XPS_SHAPES_PER_PAGE: usize = 200_000;
const MAX_XPS_CANVAS_DEPTH: usize = 256;
const MAX_XPS_PATH_TOKENS: usize = 100_000;
const MAX_XPS_PATH_POINTS: usize = 500_000;
const MAX_XPS_TOTAL_PATH_POINTS: usize = 2_000_000;
const MAX_XPS_TEXT_BYTES: usize = 32 * 1024 * 1024;
const MAX_XPS_TOTAL_IMAGE_BYTES: usize = 128 * 1024 * 1024;
const MAX_XPS_IMAGE_PIXELS: u64 = 40_000_000;
const MAX_XPS_DECODED_XML_BYTES: usize = 64 * 1024 * 1024;
const MAX_XPS_PAGE_DIMENSION: f64 = 1_000_000.0;
const XPS_TO_POINTS: f64 = 0.75;

struct PageReference {
    part: String,
    width: Option<f64>,
    height: Option<f64>,
}

struct XpsCanvas {
    name: String,
    transform: [f64; 6],
    opacity: f64,
    nodes: Vec<Node>,
}

struct XpsPath {
    data: Option<String>,
    image_source: Option<String>,
    image_opacity: f64,
    fill_rule: String,
    geometry_transform: [f64; 6],
    fill: Paint,
    stroke: Stroke,
    opacity: f64,
    transform: [f64; 6],
    unsupported_geometry: bool,
}

#[derive(Clone)]
struct XpsImage {
    data_uri: String,
    width: u32,
    height: u32,
}

struct XpsGlyphs {
    text: String,
    font_uri: Option<String>,
    origin_x: f64,
    origin_y: f64,
    font_size: f64,
    fill: Paint,
    opacity: f64,
    transform: [f64; 6],
    bold: bool,
    italic: bool,
    has_indices: bool,
}

struct FixedPageState {
    page: Page,
    containers: Vec<XpsCanvas>,
    xml_stack: Vec<String>,
    ignored_resource_depth: usize,
    path: Option<XpsPath>,
    glyphs: Option<XpsGlyphs>,
    shape_count: usize,
    canvas_count: usize,
    total_path_points: usize,
    total_text_bytes: usize,
    total_embedded_image_bytes: usize,
    images: HashMap<String, XpsImage>,
    warnings: Vec<String>,
}

impl FixedPageState {
    fn new(page_number: usize, width: f64, height: f64, images: HashMap<String, XpsImage>) -> Self {
        Self {
            page: Page::new(
                page_number,
                width * XPS_TO_POINTS,
                height * XPS_TO_POINTS,
                "xps",
            ),
            containers: vec![XpsCanvas {
                name: "xps-page-scale".into(),
                transform: [XPS_TO_POINTS, 0.0, 0.0, XPS_TO_POINTS, 0.0, 0.0],
                opacity: 1.0,
                nodes: Vec::new(),
            }],
            xml_stack: Vec::new(),
            ignored_resource_depth: 0,
            path: None,
            glyphs: None,
            shape_count: 0,
            canvas_count: 0,
            total_path_points: 0,
            total_text_bytes: 0,
            total_embedded_image_bytes: 0,
            images,
            warnings: Vec::new(),
        }
    }

    fn warn_once(&mut self, warning: &str) {
        if !self.warnings.iter().any(|existing| existing == warning) {
            self.warnings.push(warning.to_owned());
        }
    }

    fn append_node(&mut self, node: Node) -> Result<()> {
        self.shape_count = self.shape_count.saturating_add(1);
        if self.shape_count > MAX_XPS_SHAPES_PER_PAGE {
            return Err(Error::LimitExceeded(format!(
                "XPS page exceeds {MAX_XPS_SHAPES_PER_PAGE} rendered shapes"
            )));
        }
        self.containers
            .last_mut()
            .ok_or_else(|| Error::InvalidInput("XPS canvas stack is empty".into()))?
            .nodes
            .push(node);
        Ok(())
    }

    fn open_element(&mut self, element: &BytesStart<'_>, name: &str, empty: bool) -> Result<()> {
        if self.ignored_resource_depth > 0 {
            if !empty {
                self.ignored_resource_depth += 1;
                if self.ignored_resource_depth > MAX_XPS_CANVAS_DEPTH {
                    return Err(Error::LimitExceeded(format!(
                        "XPS XML nesting exceeds {MAX_XPS_CANVAS_DEPTH} levels"
                    )));
                }
            }
            return Ok(());
        }
        if name.ends_with(".Resources") || name == "ResourceDictionary" {
            self.warn_once(
                "XPS resource dictionaries and indirect brush references are not resolved; affected markings may be omitted",
            );
            if !empty {
                self.ignored_resource_depth = 1;
            }
            return Ok(());
        }
        if self.xml_stack.len() >= MAX_XPS_CANVAS_DEPTH {
            return Err(Error::LimitExceeded(format!(
                "XPS XML nesting exceeds {MAX_XPS_CANVAS_DEPTH} levels"
            )));
        }

        match name {
            "Canvas" => {
                if !empty {
                    if attribute(element, b"Clip").is_some()
                        || attribute(element, b"OpacityMask").is_some()
                    {
                        self.warn_once(
                            "XPS Canvas Clip and OpacityMask properties are not applied",
                        );
                    }
                    self.canvas_count = self.canvas_count.saturating_add(1);
                    self.containers.push(XpsCanvas {
                        name: format!("xps-canvas-{}", self.canvas_count),
                        transform: parse_xps_matrix(attribute(element, b"RenderTransform"))?,
                        opacity: parse_xps_opacity(attribute(element, b"Opacity"))?,
                        nodes: Vec::new(),
                    });
                }
            }
            "Path" => {
                if self.path.is_some() || self.glyphs.is_some() {
                    return Err(Error::InvalidInput(
                        "nested XPS Path/Glyphs markings are unsupported".into(),
                    ));
                }
                if attribute(element, b"Clip").is_some()
                    || attribute(element, b"OpacityMask").is_some()
                {
                    self.warn_once("XPS Path Clip and OpacityMask properties are not applied");
                }
                self.path = Some(XpsPath {
                    data: attribute(element, b"Data"),
                    image_source: None,
                    image_opacity: 1.0,
                    fill_rule: "evenodd".into(),
                    geometry_transform: IDENTITY,
                    fill: parse_xps_paint_attribute(attribute(element, b"Fill"), self)?,
                    stroke: parse_xps_stroke(element, self)?,
                    opacity: parse_xps_opacity(attribute(element, b"Opacity"))?,
                    transform: parse_xps_matrix(attribute(element, b"RenderTransform"))?,
                    unsupported_geometry: false,
                });
                if empty {
                    self.finish_path()?;
                    return Ok(());
                }
            }
            "Glyphs" => {
                if self.path.is_some() || self.glyphs.is_some() {
                    return Err(Error::InvalidInput(
                        "nested XPS Path/Glyphs markings are unsupported".into(),
                    ));
                }
                if attribute(element, b"Clip").is_some()
                    || attribute(element, b"OpacityMask").is_some()
                {
                    self.warn_once("XPS Glyphs Clip and OpacityMask properties are not applied");
                }
                let font_size = parse_xps_optional_number(
                    attribute(element, b"FontRenderingEmSize"),
                    "FontRenderingEmSize",
                )?
                .unwrap_or(0.0);
                let simulations = attribute(element, b"StyleSimulations").unwrap_or_default();
                if attribute(element, b"IsSideways").is_some_and(|value| value == "true")
                    || attribute(element, b"BidiLevel").is_some_and(|value| value != "0")
                {
                    self.warn_once(
                        "XPS sideways or bidirectional Glyphs positioning is approximated",
                    );
                }
                self.glyphs = Some(XpsGlyphs {
                    text: attribute(element, b"UnicodeString").unwrap_or_default(),
                    font_uri: attribute(element, b"FontUri"),
                    origin_x: parse_xps_optional_number(attribute(element, b"OriginX"), "OriginX")?
                        .unwrap_or(0.0),
                    origin_y: parse_xps_optional_number(attribute(element, b"OriginY"), "OriginY")?
                        .unwrap_or(0.0),
                    font_size,
                    fill: parse_xps_paint_attribute(attribute(element, b"Fill"), self)?,
                    opacity: parse_xps_opacity(attribute(element, b"Opacity"))?,
                    transform: parse_xps_matrix(attribute(element, b"RenderTransform"))?,
                    bold: matches!(
                        simulations.as_str(),
                        "BoldSimulation" | "BoldItalicSimulation"
                    ),
                    italic: matches!(
                        simulations.as_str(),
                        "ItalicSimulation" | "BoldItalicSimulation"
                    ),
                    has_indices: attribute(element, b"Indices").is_some(),
                });
                if empty {
                    self.finish_glyphs()?;
                    return Ok(());
                }
            }
            "PathGeometry" => {
                if let Some(path) = self.path.as_mut() {
                    if let Some(figures) = attribute(element, b"Figures") {
                        path.data = Some(figures);
                    }
                    if let Some(fill_rule) = attribute(element, b"FillRule") {
                        path.fill_rule = parse_xps_fill_rule(&fill_rule)?;
                    }
                    if let Some(transform) = attribute(element, b"Transform") {
                        path.geometry_transform = parse_xps_matrix(Some(transform))?;
                    }
                }
            }
            "Canvas.Clip" | "Canvas.OpacityMask" | "Path.Clip" | "Path.OpacityMask"
            | "Glyphs.Clip" | "Glyphs.OpacityMask" => {
                self.warn_once("XPS Clip and OpacityMask property elements are not applied");
            }
            "PathFigure"
            | "ArcSegment"
            | "BezierSegment"
            | "PolyBezierSegment"
            | "PolyLineSegment"
            | "PolyQuadraticBezierSegment"
            | "LineSegment" => {
                if let Some(path) = self.path.as_mut() {
                    path.unsupported_geometry = true;
                }
            }
            "SolidColorBrush" => {
                if let Some(color) = attribute(element, b"Color")
                    && let Some(paint) = parse_xps_solid_paint(&color)
                {
                    match self.xml_stack.last().map(String::as_str) {
                        Some("Path.Fill") => {
                            if let Some(path) = self.path.as_mut() {
                                path.fill = paint;
                            }
                        }
                        Some("Path.Stroke") => {
                            if let Some(path) = self.path.as_mut() {
                                path.stroke.paint = paint;
                            }
                        }
                        Some("Glyphs.Fill") => {
                            if let Some(glyphs) = self.glyphs.as_mut() {
                                glyphs.fill = paint;
                            }
                        }
                        _ => {}
                    }
                } else if attribute(element, b"Color").is_some() {
                    self.warn_once("XPS solid brush color syntax is unsupported and was omitted");
                }
            }
            "ImageBrush" => {
                if self.xml_stack.last().map(String::as_str) == Some("Path.Fill") {
                    let image_source = attribute(element, b"ImageSource");
                    if let Some(path) = self.path.as_mut() {
                        path.image_source = image_source;
                        path.image_opacity = parse_xps_opacity(attribute(element, b"Opacity"))?;
                        path.fill = Paint::None;
                    }
                    if [
                        b"Viewbox".as_slice(),
                        b"Viewport".as_slice(),
                        b"Transform".as_slice(),
                        b"RelativeTransform".as_slice(),
                    ]
                    .iter()
                    .any(|attribute_name| attribute(element, attribute_name).is_some())
                    {
                        self.warn_once(
                            "XPS ImageBrush viewbox/viewport/transform mapping is approximated to the Path bounds",
                        );
                    }
                    if attribute(element, b"TileMode")
                        .is_some_and(|mode| !mode.eq_ignore_ascii_case("None"))
                    {
                        self.warn_once("XPS ImageBrush tiling is not rendered");
                    }
                    if attribute(element, b"Stretch")
                        .is_some_and(|stretch| !stretch.eq_ignore_ascii_case("Fill"))
                    {
                        self.warn_once(
                            "XPS ImageBrush non-Fill stretch is approximated to the Path bounds",
                        );
                    }
                } else {
                    self.warn_once("XPS ImageBrush outside Path.Fill is omitted");
                }
            }
            "LinearGradientBrush" | "RadialGradientBrush" | "VisualBrush" | "Visual" => {
                self.warn_once(
                    "XPS gradient and visual brushes are not rendered; their filled markings may be omitted",
                );
            }
            _ => {}
        }
        if !empty {
            self.xml_stack.push(name.to_owned());
        }
        Ok(())
    }

    fn close_element(&mut self, name: &str) -> Result<()> {
        if self.ignored_resource_depth > 0 {
            self.ignored_resource_depth -= 1;
            return Ok(());
        }
        if self.xml_stack.pop().as_deref() != Some(name) {
            return Err(Error::InvalidInput(format!(
                "mismatched XPS XML end tag '{name}'"
            )));
        }
        match name {
            "Path" => self.finish_path()?,
            "Glyphs" => self.finish_glyphs()?,
            "Canvas" => self.finish_canvas()?,
            _ => {}
        }
        Ok(())
    }

    fn finish_canvas(&mut self) -> Result<()> {
        if self.containers.len() <= 1 {
            return Err(Error::InvalidInput(
                "XPS Canvas close has no matching open element".into(),
            ));
        }
        let canvas = self
            .containers
            .pop()
            .ok_or_else(|| Error::InvalidInput("XPS Canvas stack is empty".into()))?;
        if canvas.nodes.is_empty() {
            return Ok(());
        }
        self.append_node(Node::Group {
            id: canvas.name,
            nodes: canvas.nodes,
            transform: canvas.transform,
            opacity: canvas.opacity,
            clip_id: None,
            meta: SourceMeta {
                semantic_role: "xps:canvas".into(),
                ..Default::default()
            },
        })
    }

    fn finish_path(&mut self) -> Result<()> {
        let Some(path) = self.path.take() else {
            return Err(Error::InvalidInput(
                "XPS Path close has no open Path".into(),
            ));
        };
        if path.unsupported_geometry {
            self.warn_once(
                "XPS PathGeometry property-element segments are not supported; that Path was omitted",
            );
            return Ok(());
        }
        let Some(data) = path.data.as_deref() else {
            self.warn_once("XPS Path without abbreviated Data/Figures geometry was omitted");
            return Ok(());
        };
        let (geometry, fill_rule) = split_xps_fill_rule(data, &path.fill_rule);
        let token_count = SvgPathTokenizer::new(geometry).count();
        if token_count > MAX_XPS_PATH_TOKENS {
            return Err(Error::LimitExceeded(format!(
                "XPS path exceeds {MAX_XPS_PATH_TOKENS} geometry tokens"
            )));
        }
        let polylines = decompose_svg_path(geometry);
        let mut point_total = 0usize;
        let mut min_x = f64::INFINITY;
        let mut min_y = f64::INFINITY;
        let mut max_x = f64::NEG_INFINITY;
        let mut max_y = f64::NEG_INFINITY;
        let mut d = String::new();
        for (points, closed) in polylines {
            point_total = point_total.saturating_add(points.len());
            if point_total > MAX_XPS_PATH_POINTS {
                return Err(Error::LimitExceeded(format!(
                    "XPS path exceeds {MAX_XPS_PATH_POINTS} tessellated points"
                )));
            }
            if points.len() < 2 {
                continue;
            }
            for (index, point) in points.iter().enumerate() {
                if !point.x.is_finite()
                    || !point.y.is_finite()
                    || point.x.abs() > MAX_XPS_PAGE_DIMENSION
                    || point.y.abs() > MAX_XPS_PAGE_DIMENSION
                {
                    return Err(Error::InvalidInput(
                        "XPS path coordinate is outside the supported finite range".into(),
                    ));
                }
                min_x = min_x.min(point.x);
                min_y = min_y.min(point.y);
                max_x = max_x.max(point.x);
                max_y = max_y.max(point.y);
                if index == 0 {
                    d.push_str(&format!("M {} {}", point.x, point.y));
                } else {
                    d.push_str(&format!(" L {} {}", point.x, point.y));
                }
            }
            if closed {
                d.push_str(" Z");
            }
            d.push(' ');
        }
        self.total_path_points = self
            .total_path_points
            .checked_add(point_total)
            .ok_or_else(|| Error::LimitExceeded("XPS page path point count overflowed".into()))?;
        if self.total_path_points > MAX_XPS_TOTAL_PATH_POINTS {
            return Err(Error::LimitExceeded(format!(
                "XPS page exceeds {MAX_XPS_TOTAL_PATH_POINTS} total path points"
            )));
        }
        if d.is_empty() {
            self.warn_once("XPS Path geometry contained no renderable points and was omitted");
            return Ok(());
        }
        let transform = compose_xps_matrix(path.transform, path.geometry_transform);
        let shape_id = self.shape_count + 1;
        let mut markings = Vec::new();
        if let Some(source) = path.image_source.as_deref() {
            if let Some(image) = self.images.get(source).cloned() {
                if self
                    .total_embedded_image_bytes
                    .checked_add(image.data_uri.len())
                    .is_none_or(|total| total > MAX_XPS_TOTAL_IMAGE_BYTES)
                {
                    return Err(Error::LimitExceeded(format!(
                        "XPS page embedded images exceed {MAX_XPS_TOTAL_IMAGE_BYTES} data-URI bytes"
                    )));
                }
                self.total_embedded_image_bytes += image.data_uri.len();
                let width = max_x - min_x;
                let height = max_y - min_y;
                if width <= 0.0 || height <= 0.0 {
                    self.warn_once("XPS ImageBrush on a degenerate Path has no visible area");
                } else {
                    let image_aspect = f64::from(image.width) / f64::from(image.height);
                    let path_aspect = width / height;
                    if (image_aspect - path_aspect).abs() / image_aspect.max(path_aspect) > 0.02 {
                        self.warn_once(
                            "XPS ImageBrush aspect/crop mapping differs from the Path bounds and is approximated",
                        );
                    }
                    let clip_id = format!("xps-image-clip-{shape_id}");
                    self.page.clips.push(ClipPath {
                        id: clip_id.clone(),
                        d: d.clone(),
                        transform,
                        fill_rule: fill_rule.clone(),
                        parent_id: None,
                        additional_paths: Vec::new(),
                    });
                    markings.push(Node::Image {
                        id: format!("xps-image-{shape_id}"),
                        href: image.data_uri,
                        x: min_x,
                        y: min_y,
                        width,
                        height,
                        transform,
                        opacity: path.image_opacity,
                        clip_id: Some(clip_id),
                        meta: SourceMeta {
                            semantic_role: "xps:image".into(),
                            ..Default::default()
                        },
                    });
                }
            } else {
                self.warn_once(&format!(
                    "XPS ImageBrush resource '{source}' was unavailable and was omitted"
                ));
            }
        }
        if !matches!(&path.fill, Paint::None) || !matches!(&path.stroke.paint, Paint::None) {
            markings.push(Node::Path {
                id: format!("xps-path-{shape_id}"),
                d,
                fill_rule,
                fill: path.fill,
                stroke: path.stroke,
                transform,
                clip_id: None,
                meta: SourceMeta {
                    semantic_role: "xps:path".into(),
                    ..Default::default()
                },
            });
        }
        if markings.is_empty() {
            return Ok(());
        }
        if path.opacity < 1.0 {
            self.append_node(Node::Group {
                id: format!("xps-path-opacity-{shape_id}"),
                nodes: markings,
                transform: IDENTITY,
                opacity: path.opacity,
                clip_id: None,
                meta: SourceMeta::default(),
            })?;
        } else {
            for marking in markings {
                self.append_node(marking)?;
            }
        }
        Ok(())
    }

    fn finish_glyphs(&mut self) -> Result<()> {
        let Some(glyphs) = self.glyphs.take() else {
            return Err(Error::InvalidInput(
                "XPS Glyphs close has no open Glyphs".into(),
            ));
        };
        if glyphs.text.is_empty() || glyphs.font_size <= 0.0 {
            self.warn_once(
                "XPS Glyphs without UnicodeString or positive FontRenderingEmSize were omitted",
            );
            return Ok(());
        }
        if glyphs.font_size > MAX_XPS_PAGE_DIMENSION
            || glyphs.origin_x.abs() > MAX_XPS_PAGE_DIMENSION
            || glyphs.origin_y.abs() > MAX_XPS_PAGE_DIMENSION
        {
            return Err(Error::InvalidInput(
                "XPS Glyphs geometry is outside the supported finite range".into(),
            ));
        }
        self.total_text_bytes = self.total_text_bytes.saturating_add(glyphs.text.len());
        if self.total_text_bytes > MAX_XPS_TEXT_BYTES {
            return Err(Error::LimitExceeded(format!(
                "XPS page text exceeds {MAX_XPS_TEXT_BYTES} bytes"
            )));
        }
        if glyphs.font_uri.is_some() {
            self.warn_once("XPS embedded fonts are not loaded; text uses a system fallback font");
        }
        if glyphs.has_indices {
            self.warn_once(
                "XPS Glyphs indices and exact glyph advances are approximated by text metrics",
            );
        }
        let node = Node::Text {
            id: format!("xps-glyphs-{}", self.shape_count + 1),
            x: glyphs.origin_x,
            y: glyphs.origin_y,
            runs: vec![TextRun {
                text: glyphs.text,
                font_family: "sans-serif".into(),
                font_size: glyphs.font_size,
                bold: glyphs.bold,
                italic: glyphs.italic,
                fill: glyphs.fill,
                baseline_shift: 0.0,
                glyph_x_offsets: Vec::new(),
                target_advance: None,
            }],
            anchor: TextAnchor::Start,
            transform: glyphs.transform,
            opacity: glyphs.opacity,
            stroke: Stroke::default(),
            clip_id: None,
            meta: SourceMeta {
                semantic_role: "xps:glyphs".into(),
                ..Default::default()
            },
        };
        self.append_node(node)
    }
}

pub(crate) fn convert(
    path: &Path,
    options: &ConvertOptions,
    sink: &mut dyn PageConsumer,
) -> Result<Vec<String>> {
    let metadata = std::fs::metadata(path)?;
    if metadata.len() > options.max_input_bytes {
        return Err(Error::LimitExceeded(format!(
            "XPS package is {} bytes; maximum is {} bytes",
            metadata.len(),
            options.max_input_bytes
        )));
    }
    let mut archive = ZipPackage::open(path, options.max_zip_entry_bytes)
        .map_err(|error| Error::InvalidInput(format!("failed to open XPS package: {error}")))?;
    if archive.entry_count() > MAX_XPS_ARCHIVE_ENTRIES {
        return Err(Error::LimitExceeded(format!(
            "XPS package exceeds {MAX_XPS_ARCHIVE_ENTRIES} ZIP entries"
        )));
    }

    let (root_relationships, mut total_bytes) = archive
        .package_relationships(options.max_xml_events)
        .map_err(|error| {
            Error::InvalidInput(format!("XPS package relationships are invalid: {error}"))
        })?;
    if total_bytes as u64 > options.max_input_bytes {
        return Err(Error::LimitExceeded(
            "XPS relationship data exceeds the input limit".into(),
        ));
    }
    let mut fixed_sequences = root_relationships
        .ids_of_type("fixedrepresentation")
        .filter_map(|(id, _)| root_relationships.target(id, ""))
        .collect::<Vec<_>>();
    fixed_sequences.sort();
    fixed_sequences.dedup();
    if fixed_sequences.len() != 1 {
        return Err(Error::InvalidInput(
            "XPS package must have exactly one internal fixed-representation root".into(),
        ));
    }
    let sequence_part = fixed_sequences.remove(0);
    let sequence_xml = read_xps_part(&mut archive, &sequence_part, options, &mut total_bytes)?;
    let documents = parse_document_sequence(&sequence_xml, &sequence_part, options.max_xml_events)?;
    if documents.len() > MAX_XPS_DOCUMENTS {
        return Err(Error::LimitExceeded(format!(
            "XPS document sequence exceeds {MAX_XPS_DOCUMENTS} documents"
        )));
    }

    let mut warnings = Vec::new();
    let mut page_number = 0usize;
    let mut page_count_total = 0usize;
    let mut total_image_bytes = 0usize;
    for (document_index, document_part) in documents.iter().enumerate() {
        let document_xml = read_xps_part(&mut archive, document_part, options, &mut total_bytes)?;
        let page_references =
            parse_fixed_document(&document_xml, document_part, options.max_xml_events)?;
        page_count_total = page_count_total
            .checked_add(page_references.len())
            .ok_or_else(|| Error::LimitExceeded("XPS page count overflowed".into()))?;
        if page_count_total > MAX_XPS_PAGES {
            return Err(Error::LimitExceeded(format!(
                "XPS package exceeds {MAX_XPS_PAGES} pages"
            )));
        }
        for page_reference in page_references {
            page_number += 1;
            let page_xml_bytes = read_xps_part(
                &mut archive,
                &page_reference.part,
                options,
                &mut total_bytes,
            )?;
            let page_xml = decode_xps_xml(&page_xml_bytes, "FixedPage")?;
            let image_sources = collect_xps_image_sources(&page_xml, options.max_xml_events)?;
            let (images, image_warnings) = load_xps_images(
                &mut archive,
                &page_reference.part,
                image_sources,
                options,
                &mut total_bytes,
                &mut total_image_bytes,
            )?;
            let (mut page, mut page_warnings) = parse_fixed_page(
                &page_xml,
                page_number,
                page_reference.width,
                page_reference.height,
                images,
                options.max_xml_events,
            )?;
            page_warnings.extend(image_warnings);
            page.title = format!("XPS document {}, page {page_number}", document_index + 1);
            page.description = format!("Fixed page {page_number} from XPS/OXPS");
            page.warnings = page_warnings.clone();
            warnings.extend(page_warnings);
            sink.consume(page)?;
        }
    }
    if page_number == 0 {
        return Err(Error::InvalidInput(
            "XPS fixed representation contains no pages".into(),
        ));
    }
    deduplicate_warnings(&mut warnings);
    Ok(warnings)
}

fn read_xps_part(
    archive: &mut ZipPackage<File>,
    part: &str,
    options: &ConvertOptions,
    total_bytes: &mut usize,
) -> Result<Vec<u8>> {
    let limit = options.max_input_bytes.min(options.max_zip_entry_bytes);
    let bytes = archive.read_limited(part, limit)?;
    *total_bytes = total_bytes
        .checked_add(bytes.len())
        .ok_or_else(|| Error::LimitExceeded("XPS package read byte count overflowed".into()))?;
    if *total_bytes as u64 > options.max_input_bytes {
        return Err(Error::LimitExceeded(format!(
            "XPS parts read exceed the {}-byte input limit",
            options.max_input_bytes
        )));
    }
    Ok(bytes)
}

fn collect_xps_image_sources(xml: &str, max_events: usize) -> Result<Vec<String>> {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(true);
    let mut buffer = Vec::new();
    let mut sources = Vec::new();
    let mut seen = HashSet::new();
    let mut ignored_resource_depth = 0usize;
    let mut events = 0usize;
    loop {
        events = events.saturating_add(1);
        if events > max_events {
            return Err(Error::LimitExceeded(format!(
                "XPS FixedPage image scan exceeds {max_events} parser events"
            )));
        }
        match reader.read_event_into(&mut buffer)? {
            Event::Start(element) => {
                let qualified_name = element.name();
                let name = local_name(qualified_name.as_ref());
                if ignored_resource_depth > 0 {
                    ignored_resource_depth += 1;
                } else if name.ends_with(b".Resources") || name == b"ResourceDictionary" {
                    ignored_resource_depth = 1;
                } else if name == b"ImageBrush"
                    && let Some(source) = attribute(&element, b"ImageSource")
                    && seen.insert(source.clone())
                {
                    sources.push(source);
                }
            }
            Event::Empty(element) if ignored_resource_depth == 0 => {
                if local_name(element.name().as_ref()) == b"ImageBrush"
                    && let Some(source) = attribute(&element, b"ImageSource")
                    && seen.insert(source.clone())
                {
                    sources.push(source);
                }
            }
            Event::End(_) if ignored_resource_depth > 0 => ignored_resource_depth -= 1,
            Event::DocType(_) => {
                return Err(Error::InvalidInput(
                    "XPS document type declarations are not supported".into(),
                ));
            }
            Event::Eof => break,
            _ => {}
        }
        buffer.clear();
    }
    Ok(sources)
}

fn load_xps_images(
    archive: &mut ZipPackage<File>,
    page_part: &str,
    sources: Vec<String>,
    options: &ConvertOptions,
    total_bytes: &mut usize,
    total_image_bytes: &mut usize,
) -> Result<(HashMap<String, XpsImage>, Vec<String>)> {
    let mut images = HashMap::new();
    let mut warnings = Vec::new();
    for source in sources {
        if source.starts_with("//")
            || source.starts_with("data:")
            || source
                .split_once(':')
                .is_some_and(|(scheme, _)| !scheme.contains('/') && !scheme.is_empty())
        {
            warnings.push(format!(
                "XPS external ImageBrush source '{source}' was not fetched"
            ));
            continue;
        }
        let image_part = resolve_part_target(page_part, &source)?;
        let bytes = match read_xps_part(archive, &image_part, options, total_bytes) {
            Ok(bytes) => bytes,
            Err(Error::Zip(zip::result::ZipError::FileNotFound)) => {
                warnings.push(format!(
                    "XPS ImageBrush part '{image_part}' is missing and was omitted"
                ));
                continue;
            }
            Err(Error::LimitExceeded(message)) => return Err(Error::LimitExceeded(message)),
            Err(error) => {
                warnings.push(format!(
                    "XPS ImageBrush part '{image_part}' could not be read: {error}"
                ));
                continue;
            }
        };
        *total_image_bytes = total_image_bytes
            .checked_add(bytes.len())
            .ok_or_else(|| Error::LimitExceeded("XPS image byte count overflowed".into()))?;
        if *total_image_bytes > MAX_XPS_TOTAL_IMAGE_BYTES {
            return Err(Error::LimitExceeded(format!(
                "XPS embedded image data exceeds {MAX_XPS_TOTAL_IMAGE_BYTES} bytes"
            )));
        }
        let mime = match sniff_image_mime(&bytes) {
            Some("image/png") => "image/png",
            Some("image/jpeg") => "image/jpeg",
            Some(other) => {
                warnings.push(format!(
                    "XPS ImageBrush '{image_part}' uses unsupported raster format '{other}'"
                ));
                continue;
            }
            None => {
                warnings.push(format!(
                    "XPS ImageBrush '{image_part}' has an unknown image format"
                ));
                continue;
            }
        };
        let (width, height) = match xps_raster_dimensions(&bytes, mime) {
            Ok(dimensions) => dimensions,
            Err(error) => {
                warnings.push(format!(
                    "XPS ImageBrush '{image_part}' could not be decoded: {error}"
                ));
                continue;
            }
        };
        let pixels = u64::from(width)
            .checked_mul(u64::from(height))
            .ok_or_else(|| Error::LimitExceeded("XPS image dimensions overflowed".into()))?;
        if width == 0 || height == 0 || pixels > MAX_XPS_IMAGE_PIXELS {
            return Err(Error::LimitExceeded(format!(
                "XPS ImageBrush '{image_part}' dimensions {width}x{height} exceed the {MAX_XPS_IMAGE_PIXELS}-pixel limit"
            )));
        }
        images.insert(
            source,
            XpsImage {
                data_uri: format!("data:{mime};base64,{}", BASE64_STANDARD.encode(bytes)),
                width,
                height,
            },
        );
    }
    Ok((images, warnings))
}

fn xps_raster_dimensions(bytes: &[u8], mime: &str) -> Result<(u32, u32)> {
    match mime {
        "image/png" => {
            let decoder = png::Decoder::new(Cursor::new(bytes));
            let reader = decoder
                .read_info()
                .map_err(|error| Error::InvalidInput(format!("invalid PNG header: {error}")))?;
            Ok((reader.info().width, reader.info().height))
        }
        "image/jpeg" => {
            let mut decoder = jpeg_decoder::Decoder::new(Cursor::new(bytes));
            decoder
                .read_info()
                .map_err(|error| Error::InvalidInput(format!("invalid JPEG header: {error}")))?;
            let info = decoder
                .info()
                .ok_or_else(|| Error::InvalidInput("JPEG image metadata is missing".into()))?;
            Ok((u32::from(info.width), u32::from(info.height)))
        }
        other => Err(Error::Unsupported(format!(
            "unsupported XPS raster type '{other}'"
        ))),
    }
}

fn parse_document_sequence(xml: &[u8], part: &str, max_events: usize) -> Result<Vec<String>> {
    let xml = decode_xps_xml(xml, "fixed document sequence")?;
    let mut reader = Reader::from_str(&xml);
    reader.config_mut().trim_text(true);
    let mut buffer = Vec::new();
    let mut documents = Vec::new();
    let mut events = 0usize;
    loop {
        events = events.saturating_add(1);
        if events > max_events {
            return Err(Error::LimitExceeded(format!(
                "XPS fixed document sequence exceeds {max_events} parser events"
            )));
        }
        match reader.read_event_into(&mut buffer)? {
            Event::Start(element) | Event::Empty(element)
                if local_name(element.name().as_ref()) == b"DocumentReference" =>
            {
                let source = attribute(&element, b"Source").ok_or_else(|| {
                    Error::InvalidInput("XPS DocumentReference is missing Source".into())
                })?;
                documents.push(resolve_part_target(part, &source)?);
                if documents.len() > MAX_XPS_DOCUMENTS {
                    return Err(Error::LimitExceeded(format!(
                        "XPS document sequence exceeds {MAX_XPS_DOCUMENTS} documents"
                    )));
                }
            }
            Event::DocType(_) => {
                return Err(Error::InvalidInput(
                    "XPS document type declarations are not supported".into(),
                ));
            }
            Event::Eof => break,
            _ => {}
        }
        buffer.clear();
    }
    if documents.is_empty() {
        return Err(Error::InvalidInput(
            "XPS fixed document sequence contains no DocumentReference".into(),
        ));
    }
    Ok(documents)
}

fn parse_fixed_document(xml: &[u8], part: &str, max_events: usize) -> Result<Vec<PageReference>> {
    let xml = decode_xps_xml(xml, "FixedDocument")?;
    let mut reader = Reader::from_str(&xml);
    reader.config_mut().trim_text(true);
    let mut buffer = Vec::new();
    let mut pages = Vec::new();
    let mut events = 0usize;
    loop {
        events = events.saturating_add(1);
        if events > max_events {
            return Err(Error::LimitExceeded(format!(
                "XPS FixedDocument '{part}' exceeds {max_events} parser events"
            )));
        }
        match reader.read_event_into(&mut buffer)? {
            Event::Start(element) | Event::Empty(element)
                if local_name(element.name().as_ref()) == b"PageContent" =>
            {
                let source = attribute(&element, b"Source").ok_or_else(|| {
                    Error::InvalidInput(format!("XPS PageContent in '{part}' is missing Source"))
                })?;
                let page_part = resolve_part_target(part, &source)?;
                pages.push(PageReference {
                    part: page_part,
                    width: parse_xps_optional_number(
                        attribute(&element, b"Width"),
                        "PageContent Width",
                    )?,
                    height: parse_xps_optional_number(
                        attribute(&element, b"Height"),
                        "PageContent Height",
                    )?,
                });
                if pages.len() > MAX_XPS_PAGES {
                    return Err(Error::LimitExceeded(format!(
                        "XPS FixedDocument exceeds {MAX_XPS_PAGES} pages"
                    )));
                }
            }
            Event::DocType(_) => {
                return Err(Error::InvalidInput(
                    "XPS document type declarations are not supported".into(),
                ));
            }
            Event::Eof => break,
            _ => {}
        }
        buffer.clear();
    }
    if pages.is_empty() {
        return Err(Error::InvalidInput(format!(
            "XPS FixedDocument '{part}' contains no PageContent"
        )));
    }
    Ok(pages)
}

fn parse_fixed_page(
    xml: &str,
    page_number: usize,
    fallback_width: Option<f64>,
    fallback_height: Option<f64>,
    images: HashMap<String, XpsImage>,
    max_events: usize,
) -> Result<(Page, Vec<String>)> {
    let mut images = Some(images);
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(true);
    let mut buffer = Vec::new();
    let mut page_dimensions = None;
    let mut state: Option<FixedPageState> = None;
    let mut events = 0usize;

    loop {
        events = events.saturating_add(1);
        if events > max_events {
            return Err(Error::LimitExceeded(format!(
                "XPS FixedPage exceeds {max_events} parser events"
            )));
        }
        match reader.read_event_into(&mut buffer)? {
            Event::Start(element) => {
                let name =
                    String::from_utf8_lossy(local_name(element.name().as_ref())).into_owned();
                if name == "FixedPage" {
                    if state.is_some() {
                        return Err(Error::InvalidInput(
                            "XPS part contains multiple FixedPage roots".into(),
                        ));
                    }
                    let width = parse_xps_optional_number(
                        attribute(&element, b"Width"),
                        "FixedPage Width",
                    )?
                    .or(fallback_width)
                    .ok_or_else(|| Error::InvalidInput("XPS FixedPage is missing Width".into()))?;
                    let height = parse_xps_optional_number(
                        attribute(&element, b"Height"),
                        "FixedPage Height",
                    )?
                    .or(fallback_height)
                    .ok_or_else(|| Error::InvalidInput("XPS FixedPage is missing Height".into()))?;
                    validate_xps_page_dimension(width, "Width")?;
                    validate_xps_page_dimension(height, "Height")?;
                    page_dimensions = Some((width, height));
                    state = Some(FixedPageState::new(
                        page_number,
                        width,
                        height,
                        images.take().unwrap_or_default(),
                    ));
                }
                if let Some(state) = state.as_mut() {
                    state.open_element(&element, &name, false)?;
                }
            }
            Event::Empty(element) => {
                let name =
                    String::from_utf8_lossy(local_name(element.name().as_ref())).into_owned();
                if name == "FixedPage" {
                    if state.is_some() {
                        return Err(Error::InvalidInput(
                            "XPS part contains multiple FixedPage roots".into(),
                        ));
                    }
                    let width = parse_xps_optional_number(
                        attribute(&element, b"Width"),
                        "FixedPage Width",
                    )?
                    .or(fallback_width)
                    .ok_or_else(|| Error::InvalidInput("XPS FixedPage is missing Width".into()))?;
                    let height = parse_xps_optional_number(
                        attribute(&element, b"Height"),
                        "FixedPage Height",
                    )?
                    .or(fallback_height)
                    .ok_or_else(|| Error::InvalidInput("XPS FixedPage is missing Height".into()))?;
                    validate_xps_page_dimension(width, "Width")?;
                    validate_xps_page_dimension(height, "Height")?;
                    page_dimensions = Some((width, height));
                    state = Some(FixedPageState::new(
                        page_number,
                        width,
                        height,
                        images.take().unwrap_or_default(),
                    ));
                } else if let Some(state) = state.as_mut() {
                    state.open_element(&element, &name, true)?;
                }
            }
            Event::End(element) => {
                let name =
                    String::from_utf8_lossy(local_name(element.name().as_ref())).into_owned();
                if name == "FixedPage" {
                    let page_state = state.as_mut().ok_or_else(|| {
                        Error::InvalidInput("XPS FixedPage end has no start".into())
                    })?;
                    if page_state.xml_stack.pop().as_deref() != Some("FixedPage")
                        || !page_state.xml_stack.is_empty()
                    {
                        return Err(Error::InvalidInput(
                            "XPS FixedPage ended with unclosed Canvas or marking elements".into(),
                        ));
                    }
                } else if let Some(state) = state.as_mut() {
                    state.close_element(&name)?;
                }
            }
            Event::DocType(_) => {
                return Err(Error::InvalidInput(
                    "XPS document type declarations are not supported".into(),
                ));
            }
            Event::Eof => break,
            _ => {}
        }
        buffer.clear();
    }
    let mut state =
        state.ok_or_else(|| Error::InvalidInput("XPS part is not a FixedPage".into()))?;
    if page_dimensions.is_none() || state.containers.len() != 1 {
        return Err(Error::InvalidInput("incomplete XPS FixedPage".into()));
    }
    let root = state
        .containers
        .pop()
        .ok_or_else(|| Error::InvalidInput("XPS FixedPage canvas stack is empty".into()))?;
    if !root.nodes.is_empty() {
        state.page.nodes.push(Node::Group {
            id: root.name,
            nodes: root.nodes,
            transform: root.transform,
            opacity: root.opacity,
            clip_id: None,
            meta: SourceMeta {
                semantic_role: "xps:page".into(),
                ..Default::default()
            },
        });
    }
    state.page.warnings = state.warnings.clone();
    Ok((state.page, state.warnings))
}

fn decode_xps_xml(bytes: &[u8], context: &str) -> Result<String> {
    if bytes.len() > MAX_XPS_DECODED_XML_BYTES.saturating_mul(2) {
        return Err(Error::LimitExceeded(format!(
            "XPS {context} exceeds the decoded XML input limit"
        )));
    }
    let (body, encoding) = if let Some(body) = bytes.strip_prefix(&[0xef, 0xbb, 0xbf]) {
        (body, None)
    } else if let Some(body) = bytes.strip_prefix(&[0xff, 0xfe]) {
        (body, Some(false))
    } else if let Some(body) = bytes.strip_prefix(&[0xfe, 0xff]) {
        (body, Some(true))
    } else if bytes.starts_with(&[b'<', 0]) {
        (bytes, Some(false))
    } else if bytes.starts_with(&[0, b'<']) {
        (bytes, Some(true))
    } else {
        (bytes, None)
    };
    let decoded = if let Some(big_endian) = encoding {
        if body.len() % 2 != 0 {
            return Err(Error::InvalidInput(format!(
                "XPS {context} has an odd-length UTF-16 byte stream"
            )));
        }
        let units = body
            .chunks_exact(2)
            .map(|pair| {
                if big_endian {
                    u16::from_be_bytes([pair[0], pair[1]])
                } else {
                    u16::from_le_bytes([pair[0], pair[1]])
                }
            })
            .collect::<Vec<_>>();
        String::from_utf16(&units).map_err(|error| {
            Error::InvalidInput(format!("XPS {context} contains invalid UTF-16: {error}"))
        })?
    } else {
        String::from_utf8(body.to_vec()).map_err(|error| {
            Error::InvalidInput(format!("XPS {context} is not valid UTF-8: {error}"))
        })?
    };
    if decoded.len() > MAX_XPS_DECODED_XML_BYTES {
        return Err(Error::LimitExceeded(format!(
            "XPS {context} exceeds {MAX_XPS_DECODED_XML_BYTES} decoded XML bytes"
        )));
    }
    Ok(decoded)
}

fn parse_xps_optional_number(value: Option<String>, context: &str) -> Result<Option<f64>> {
    value
        .map(|value| {
            let parsed = value.parse::<f64>().map_err(|_| {
                Error::InvalidInput(format!("invalid XPS {context} value '{value}'"))
            })?;
            if !parsed.is_finite() {
                return Err(Error::InvalidInput(format!("XPS {context} is not finite")));
            }
            Ok(parsed)
        })
        .transpose()
}

fn validate_xps_page_dimension(value: f64, context: &str) -> Result<()> {
    if value <= 0.0 || value > MAX_XPS_PAGE_DIMENSION {
        return Err(Error::InvalidInput(format!(
            "XPS FixedPage {context} {value} is outside the supported positive range"
        )));
    }
    Ok(())
}

fn parse_xps_opacity(value: Option<String>) -> Result<f64> {
    let opacity = parse_xps_optional_number(value, "Opacity")?.unwrap_or(1.0);
    if !(0.0..=1.0).contains(&opacity) {
        return Err(Error::InvalidInput(format!(
            "XPS Opacity {opacity} is outside the range 0..1"
        )));
    }
    Ok(opacity)
}

fn parse_xps_matrix(value: Option<String>) -> Result<[f64; 6]> {
    let Some(value) = value else {
        return Ok(IDENTITY);
    };
    let values = value
        .split(|character: char| character == ',' || character.is_ascii_whitespace())
        .filter(|part| !part.is_empty())
        .map(|part| {
            let value = part.parse::<f64>().map_err(|_| {
                Error::InvalidInput(format!("invalid XPS RenderTransform value '{part}'"))
            })?;
            if !value.is_finite() {
                return Err(Error::InvalidInput(
                    "XPS RenderTransform is not finite".into(),
                ));
            }
            Ok(value)
        })
        .collect::<Result<Vec<_>>>()?;
    let matrix: [f64; 6] = values
        .try_into()
        .map_err(|_| Error::InvalidInput("XPS RenderTransform must contain six values".into()))?;
    if matrix
        .iter()
        .any(|value| value.abs() > MAX_XPS_PAGE_DIMENSION)
    {
        return Err(Error::InvalidInput(
            "XPS RenderTransform is outside the supported finite range".into(),
        ));
    }
    Ok(matrix)
}

fn compose_xps_matrix(left: [f64; 6], right: [f64; 6]) -> [f64; 6] {
    [
        left[0] * right[0] + left[2] * right[1],
        left[1] * right[0] + left[3] * right[1],
        left[0] * right[2] + left[2] * right[3],
        left[1] * right[2] + left[3] * right[3],
        left[0] * right[4] + left[2] * right[5] + left[4],
        left[1] * right[4] + left[3] * right[5] + left[5],
    ]
}

fn parse_xps_fill_rule(value: &str) -> Result<String> {
    match value {
        "EvenOdd" | "evenodd" => Ok("evenodd".into()),
        "NonZero" | "nonzero" => Ok("nonzero".into()),
        _ => Err(Error::InvalidInput(format!(
            "unsupported XPS FillRule '{value}'"
        ))),
    }
}

fn split_xps_fill_rule<'a>(data: &'a str, default: &str) -> (&'a str, String) {
    let data = data.trim_start();
    let bytes = data.as_bytes();
    if bytes.len() >= 2 && bytes[0] == b'F' && (bytes[1] == b'0' || bytes[1] == b'1') {
        let fill_rule = if bytes[1] == b'0' {
            "evenodd"
        } else {
            "nonzero"
        };
        return (data[2..].trim_start(), fill_rule.into());
    }
    (data, default.to_owned())
}

fn parse_xps_solid_paint(value: &str) -> Option<Paint> {
    let hex = value.strip_prefix('#')?;
    let (opacity, rgb) = match hex.len() {
        6 => (1.0, hex),
        8 => {
            let alpha = u8::from_str_radix(hex.get(..2)?, 16).ok()?;
            (f64::from(alpha) / 255.0, hex.get(2..8)?)
        }
        _ => return None,
    };
    u32::from_str_radix(rgb, 16).ok()?;
    Some(Paint::Solid {
        color: format!("#{rgb}"),
        opacity,
    })
}

fn parse_xps_paint_attribute(value: Option<String>, state: &mut FixedPageState) -> Result<Paint> {
    let Some(value) = value else {
        return Ok(Paint::None);
    };
    if let Some(paint) = parse_xps_solid_paint(&value) {
        Ok(paint)
    } else {
        state.warn_once("XPS non-solid or resource-based brushes are not resolved");
        Ok(Paint::None)
    }
}

fn parse_xps_stroke(element: &BytesStart<'_>, state: &mut FixedPageState) -> Result<Stroke> {
    let paint = parse_xps_paint_attribute(attribute(element, b"Stroke"), state)?;
    let width =
        parse_xps_optional_number(attribute(element, b"StrokeThickness"), "StrokeThickness")?
            .unwrap_or(1.0);
    if !(0.0..=MAX_XPS_PAGE_DIMENSION).contains(&width) {
        return Err(Error::InvalidInput(format!(
            "XPS StrokeThickness {width} is outside the supported range"
        )));
    }
    let line_join = match attribute(element, b"StrokeLineJoin").as_deref() {
        None | Some("Miter") => LineJoin::Miter,
        Some("Round") => LineJoin::Round,
        Some("Bevel") => LineJoin::Bevel,
        Some(other) => {
            state.warn_once(&format!(
                "XPS StrokeLineJoin '{other}' is approximated as Miter"
            ));
            LineJoin::Miter
        }
    };
    let cap = match attribute(element, b"StrokeStartLineCap").as_deref() {
        None | Some("Flat") => LineCap::Butt,
        Some("Round") => LineCap::Round,
        Some("Square") => LineCap::Square,
        Some(other) => {
            state.warn_once(&format!("XPS stroke line cap '{other}' is approximated"));
            LineCap::Butt
        }
    };
    let end_cap = match attribute(element, b"StrokeEndLineCap").as_deref() {
        None | Some("Flat") => LineCap::Butt,
        Some("Round") => LineCap::Round,
        Some("Square") => LineCap::Square,
        Some(other) => {
            state.warn_once(&format!("XPS stroke line cap '{other}' is approximated"));
            LineCap::Butt
        }
    };
    if end_cap != cap {
        state.warn_once("XPS different start/end stroke caps are approximated with one cap style");
    }
    if attribute(element, b"StrokeDashCap").is_some_and(|value| !matches!(value.as_str(), "Flat")) {
        state.warn_once("XPS StrokeDashCap is not applied to individual dash ends");
    }
    let mut dash_array = Vec::new();
    if let Some(value) = attribute(element, b"StrokeDashArray") {
        for part in
            value.split(|character: char| character == ',' || character.is_ascii_whitespace())
        {
            if part.is_empty() {
                continue;
            }
            let dash = part.parse::<f64>().map_err(|_| {
                Error::InvalidInput(format!("invalid XPS StrokeDashArray value '{part}'"))
            })?;
            if !dash.is_finite() || !(0.0..=MAX_XPS_PAGE_DIMENSION).contains(&dash) {
                return Err(Error::InvalidInput(
                    "XPS StrokeDashArray must be finite and nonnegative".into(),
                ));
            }
            if dash_array.len() >= 256 {
                return Err(Error::LimitExceeded(
                    "XPS StrokeDashArray exceeds 256 values".into(),
                ));
            }
            dash_array.push(dash * width);
        }
        if dash_array.len() % 2 != 0 {
            return Err(Error::InvalidInput(
                "XPS StrokeDashArray requires an even number of values".into(),
            ));
        }
    }
    let dash_offset =
        parse_xps_optional_number(attribute(element, b"StrokeDashOffset"), "StrokeDashOffset")?
            .unwrap_or(0.0)
            * width;
    if !dash_offset.is_finite() {
        return Err(Error::InvalidInput(
            "XPS StrokeDashOffset is outside the supported finite range".into(),
        ));
    }
    Ok(Stroke {
        paint,
        width,
        line_cap: cap,
        line_join,
        miter_limit: parse_xps_optional_number(
            attribute(element, b"StrokeMiterLimit"),
            "StrokeMiterLimit",
        )?
        .unwrap_or(10.0),
        dash_array,
        dash_offset,
    })
}

fn deduplicate_warnings(warnings: &mut Vec<String>) {
    let mut seen = HashSet::new();
    warnings.retain(|warning| seen.insert(warning.clone()));
}
