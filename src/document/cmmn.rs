//! Bounded CMMN 1.1 case-plan previews using explicit CMMN Diagram Interchange geometry.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use crate::convert::{ConvertOptions, PageConsumer, read_limited_file};
use crate::error::{Error, Result};
use crate::geospatial::xml_tree::{XmlElement, XmlLimits, parse_xml_tree};
use crate::ir::{
    IDENTITY, LineCap, LineJoin, Node, Page, Paint, SourceMeta, Stroke, TextAnchor, TextRun,
};

const CMMN_MODEL_NS: &str = "http://www.omg.org/spec/CMMN/20151109/MODEL";
const CMMNDI_NS: &str = "http://www.omg.org/spec/CMMN/20151109/CMMNDI";
const CMMN_DI_NS: &str = "http://www.omg.org/spec/CMMN/20151109/DI";
const CMMN_DC_NS: &str = "http://www.omg.org/spec/CMMN/20151109/DC";

const MAX_CMMN_BYTES: u64 = 32 * 1024 * 1024;
const MAX_CMMN_EVENTS: usize = 500_000;
const MAX_CMMN_ELEMENTS: usize = 200_000;
const MAX_CMMN_DEPTH: usize = 80;
const MAX_CMMN_TEXT_BYTES: usize = 16 * 1024 * 1024;
const MAX_CMMN_DIAGRAMS: usize = 100;
const MAX_CMMN_RENDERED_NODES: usize = 200_000;
const MAX_CMMN_COORDINATE: f64 = 10_000_000.0;

const PAGE_WIDTH: f64 = 595.0;
const PAGE_HEIGHT: f64 = 842.0;
const PAGE_MARGIN: f64 = 40.0;

#[derive(Clone)]
struct ModelElement {
    id: String,
    name: String,
    kind: String,
    definition_ref: Option<String>,
}

#[derive(Clone, Copy)]
struct Rect {
    x: f64,
    y: f64,
    width: f64,
    height: f64,
}

#[derive(Clone)]
struct Shape {
    model_id: String,
    rect: Rect,
    collapsed: bool,
}

#[derive(Clone, Copy)]
struct Point {
    x: f64,
    y: f64,
}

#[derive(Clone)]
struct Edge {
    model_id: String,
    points: Vec<Point>,
}

type DiagramGeometry = (String, Vec<Shape>, Vec<Edge>);

struct CmmnPageSink<'a> {
    inner: &'a mut dyn PageConsumer,
    warnings: &'a [String],
}

impl PageConsumer for CmmnPageSink<'_> {
    fn consume(&mut self, mut page: Page) -> Result<()> {
        page.source_format = "cmmn".into();
        if page.title.is_empty() {
            page.title = "CMMN case plan".into();
        }
        for warning in self.warnings {
            page.warn(warning.clone());
        }
        self.inner.consume(page)
    }
}

pub(crate) fn convert(
    path: &Path,
    options: &ConvertOptions,
    sink: &mut dyn PageConsumer,
) -> Result<Vec<String>> {
    let bytes = read_limited_file(
        path,
        options.max_input_bytes.min(MAX_CMMN_BYTES),
        "CMMN input",
    )?;
    let root = parse_cmmn(&bytes)?;
    let model = collect_model(&root)?;
    let diagrams = collect_diagrams(&root)?;
    if diagrams.is_empty() {
        return Err(Error::Unsupported(
            "CMMN document has no CMMNDiagram geometry to preview".into(),
        ));
    }
    if diagrams.len() > MAX_CMMN_DIAGRAMS {
        return Err(Error::LimitExceeded(format!(
            "CMMN input exceeds {MAX_CMMN_DIAGRAMS} diagrams"
        )));
    }
    if diagrams.len() > options.max_pages {
        return Err(Error::LimitExceeded(format!(
            "CMMN input contains {} diagrams; maximum is {}",
            diagrams.len(),
            options.max_pages
        )));
    }
    if options.max_pages == 0 {
        return Err(Error::LimitExceeded(
            "CMMN conversion requires at least one page; max_pages is zero".into(),
        ));
    }

    let lifecycle_semantics_present = contains_lifecycle_semantics(&root);
    let mut warnings = Vec::new();
    if lifecycle_semantics_present {
        warnings.push(
            "CMMN sentries, criteria, case-file updates and lifecycle rules are shown only as diagram structure; they are not evaluated".into(),
        );
    }
    for (index, (name, shapes, edges)) in diagrams.iter().enumerate() {
        let (mut page, page_warnings) = render_diagram(index + 1, name, shapes, edges, &model)?;
        for warning in &page_warnings {
            if !warnings.contains(warning) {
                warnings.push(warning.clone());
            }
        }
        for warning in page_warnings {
            page.warn(warning);
        }
        if lifecycle_semantics_present {
            page.warn(warnings[0].clone());
        }
        let mut page_sink = CmmnPageSink {
            inner: sink,
            warnings: &[],
        };
        page_sink.consume(page)?;
    }
    Ok(warnings)
}

pub(crate) fn looks_like_prefix(bytes: &[u8]) -> bool {
    crate::geospatial::xml_tree::looks_like_root(
        bytes,
        b"definitions",
        Some(CMMN_MODEL_NS.as_bytes()),
    )
}

fn parse_cmmn(bytes: &[u8]) -> Result<XmlElement> {
    let root = parse_xml_tree(
        bytes,
        &XmlLimits {
            max_events: MAX_CMMN_EVENTS,
            max_nodes: MAX_CMMN_ELEMENTS,
            max_depth: MAX_CMMN_DEPTH,
            max_text_bytes: MAX_CMMN_TEXT_BYTES,
        },
        "CMMN",
    )?;
    if root.name != "definitions" || root.namespace.as_deref() != Some(CMMN_MODEL_NS) {
        return Err(Error::Unsupported(
            "CMMN input must have a CMMN 1.1 definitions root".into(),
        ));
    }
    Ok(root)
}

fn collect_model(root: &XmlElement) -> Result<HashMap<String, ModelElement>> {
    let mut elements = Vec::new();
    collect_elements(root, &mut elements);
    let mut model = HashMap::new();
    for element in elements {
        if element.namespace.as_deref() != Some(CMMN_MODEL_NS) {
            continue;
        }
        let Some(id) = element.attribute("id") else {
            continue;
        };
        let item = ModelElement {
            id: id.to_owned(),
            name: element.attribute("name").unwrap_or_default().to_owned(),
            kind: element.name.clone(),
            definition_ref: element.attribute("definitionRef").map(str::to_owned),
        };
        if model.insert(id.to_owned(), item).is_some() {
            return Err(Error::InvalidInput(format!(
                "CMMN document contains duplicate id {id:?}"
            )));
        }
    }
    Ok(model)
}

fn collect_elements<'a>(element: &'a XmlElement, output: &mut Vec<&'a XmlElement>) {
    output.push(element);
    for child in &element.children {
        collect_elements(child, output);
    }
}

fn contains_lifecycle_semantics(root: &XmlElement) -> bool {
    let mut elements = Vec::new();
    collect_elements(root, &mut elements);
    elements.iter().any(|element| {
        element.namespace.as_deref() == Some(CMMN_MODEL_NS)
            && matches!(
                element.name.as_str(),
                "sentry"
                    | "entryCriterion"
                    | "exitCriterion"
                    | "caseFileItemOnPart"
                    | "planItemOnPart"
                    | "requiredRule"
                    | "repetitionRule"
                    | "manualActivationRule"
            )
    })
}

fn collect_diagrams(root: &XmlElement) -> Result<Vec<DiagramGeometry>> {
    let mut diagrams = Vec::new();
    let mut elements = Vec::new();
    collect_elements(root, &mut elements);
    for diagram in elements.into_iter().filter(|element| {
        element.name == "CMMNDiagram" && element.namespace.as_deref() == Some(CMMNDI_NS)
    }) {
        let name = diagram.attribute("name").unwrap_or_default().to_owned();
        let mut shapes = Vec::new();
        let mut edges = Vec::new();
        for child in &diagram.children {
            match (child.namespace.as_deref(), child.name.as_str()) {
                (Some(CMMNDI_NS), "CMMNShape") => shapes.push(parse_shape(child)?),
                (Some(CMMNDI_NS), "CMMNEdge") => edges.push(parse_edge(child)?),
                _ => {}
            }
        }
        if shapes.is_empty() {
            return Err(Error::InvalidInput(
                "CMMNDiagram has no CMMNShape geometry".into(),
            ));
        }
        diagrams.push((name, shapes, edges));
    }
    Ok(diagrams)
}

fn parse_shape(element: &XmlElement) -> Result<Shape> {
    let model_id = element
        .attribute("cmmnElementRef")
        .ok_or_else(|| Error::InvalidInput("CMMNShape is missing cmmnElementRef".into()))?;
    let bounds = element
        .children
        .iter()
        .find(|child| child.name == "Bounds" && child.namespace.as_deref() == Some(CMMN_DC_NS))
        .ok_or_else(|| Error::InvalidInput("CMMNShape is missing DC Bounds".into()))?;
    Ok(Shape {
        model_id: reference_local(model_id).to_owned(),
        rect: Rect {
            x: coordinate(bounds.attribute("x"), "shape x")?,
            y: coordinate(bounds.attribute("y"), "shape y")?,
            width: positive_dimension(bounds.attribute("width"), "shape width")?,
            height: positive_dimension(bounds.attribute("height"), "shape height")?,
        },
        collapsed: element.attribute("isCollapsed") == Some("true"),
    })
}

fn parse_edge(element: &XmlElement) -> Result<Edge> {
    let model_id = element
        .attribute("cmmnElementRef")
        .ok_or_else(|| Error::InvalidInput("CMMNEdge is missing cmmnElementRef".into()))?;
    let points = element
        .children
        .iter()
        .filter(|child| child.name == "waypoint" && child.namespace.as_deref() == Some(CMMN_DI_NS))
        .map(|point| {
            Ok(Point {
                x: coordinate(point.attribute("x"), "waypoint x")?,
                y: coordinate(point.attribute("y"), "waypoint y")?,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    if points.len() < 2 {
        return Err(Error::InvalidInput(
            "CMMNEdge requires at least two DI waypoints".into(),
        ));
    }
    Ok(Edge {
        model_id: reference_local(model_id).to_owned(),
        points,
    })
}

fn reference_local(value: &str) -> &str {
    value
        .strip_prefix('#')
        .unwrap_or(value)
        .rsplit(':')
        .next()
        .unwrap_or(value)
}

fn coordinate(value: Option<&str>, label: &str) -> Result<f64> {
    let value = value
        .ok_or_else(|| Error::InvalidInput(format!("CMMN DI is missing {label}")))?
        .parse::<f64>()
        .map_err(|_| Error::InvalidInput(format!("CMMN DI has invalid {label}")))?;
    if !value.is_finite() || value.abs() > MAX_CMMN_COORDINATE {
        return Err(Error::LimitExceeded(format!(
            "CMMN DI {label} exceeds coordinate bounds"
        )));
    }
    Ok(value)
}

fn positive_dimension(value: Option<&str>, label: &str) -> Result<f64> {
    let value = coordinate(value, label)?;
    if value <= 0.0 {
        return Err(Error::InvalidInput(format!(
            "CMMN DI {label} must be positive"
        )));
    }
    Ok(value)
}

fn render_diagram(
    number: usize,
    name: &str,
    shapes: &[Shape],
    edges: &[Edge],
    model: &HashMap<String, ModelElement>,
) -> Result<(Page, Vec<String>)> {
    let mut warnings = Vec::new();
    let (min_x, min_y, max_x, max_y) = bounds(shapes, edges)?;
    let content_width = PAGE_WIDTH - PAGE_MARGIN * 2.0;
    let content_height = PAGE_HEIGHT - PAGE_MARGIN * 2.0;
    let width = (max_x - min_x).max(1.0);
    let height = (max_y - min_y).max(1.0);
    let scale = (content_width / width)
        .min(content_height / height)
        .min(2.0);
    let map_point = |x: f64, y: f64| {
        (
            PAGE_MARGIN + (x - min_x) * scale,
            PAGE_MARGIN + (y - min_y) * scale,
        )
    };
    let mut page = Page::new(number, PAGE_WIDTH, PAGE_HEIGHT, "cmmn");
    page.title = if name.trim().is_empty() {
        format!("CMMN case diagram {number}")
    } else {
        name.to_owned()
    };

    let unresolved_shapes = shapes
        .iter()
        .filter(|shape| !model.contains_key(&shape.model_id))
        .count();
    if unresolved_shapes > 0 {
        warnings.push(format!(
            "{unresolved_shapes} CMMNShape reference(s) have no model element and were omitted"
        ));
    }

    for shape in shapes.iter().filter(|shape| {
        model
            .get(&shape.model_id)
            .is_some_and(|element| matches!(element.kind.as_str(), "casePlanModel" | "stage"))
    }) {
        if let Some((element, _)) = resolve_model(shape, model) {
            draw_shape(
                &mut page,
                shape,
                element,
                model,
                scale,
                &map_point,
                &mut warnings,
            )?;
        }
    }
    for edge in edges {
        let label = model
            .get(&edge.model_id)
            .map(|element| element.name.as_str())
            .unwrap_or("");
        draw_edge(&mut page, edge, label, scale, &map_point)?;
    }
    for shape in shapes.iter().filter(|shape| {
        model
            .get(&shape.model_id)
            .is_some_and(|element| !matches!(element.kind.as_str(), "casePlanModel" | "stage"))
    }) {
        if let Some((element, definition)) = resolve_model(shape, model) {
            draw_shape(
                &mut page,
                shape,
                element,
                model,
                scale,
                &map_point,
                &mut warnings,
            )?;
            if let Some(definition) = definition
                && matches!(
                    definition.kind.as_str(),
                    "humanTask" | "processTask" | "caseTask" | "decisionTask"
                )
            {
                // The plan item is drawn with the definition type, but lifecycle controls are not run.
            }
        }
    }
    let rendered_ids = shapes
        .iter()
        .map(|shape| shape.model_id.as_str())
        .collect::<HashSet<_>>();
    let missing = model
        .values()
        .filter(|element| {
            matches!(
                element.kind.as_str(),
                "planItem" | "casePlanModel" | "stage"
            ) && !rendered_ids.contains(element.id.as_str())
        })
        .count();
    if missing > 0 {
        warnings.push(format!(
            "{missing} CMMN plan element(s) have no CMMNShape and were omitted"
        ));
    }
    Ok((page, warnings))
}

fn resolve_model<'a>(
    shape: &Shape,
    model: &'a HashMap<String, ModelElement>,
) -> Option<(&'a ModelElement, Option<&'a ModelElement>)> {
    let element = model.get(&shape.model_id)?;
    if element.kind == "planItem"
        && let Some(definition_ref) = element.definition_ref.as_deref()
        && let Some(definition) = model.get(reference_local(definition_ref))
    {
        return Some((element, Some(definition)));
    }
    Some((element, None))
}

fn bounds(shapes: &[Shape], edges: &[Edge]) -> Result<(f64, f64, f64, f64)> {
    let mut min_x = f64::INFINITY;
    let mut min_y = f64::INFINITY;
    let mut max_x = f64::NEG_INFINITY;
    let mut max_y = f64::NEG_INFINITY;
    for shape in shapes {
        min_x = min_x.min(shape.rect.x);
        min_y = min_y.min(shape.rect.y);
        max_x = max_x.max(shape.rect.x + shape.rect.width);
        max_y = max_y.max(shape.rect.y + shape.rect.height);
    }
    for edge in edges {
        for point in &edge.points {
            min_x = min_x.min(point.x);
            min_y = min_y.min(point.y);
            max_x = max_x.max(point.x);
            max_y = max_y.max(point.y);
        }
    }
    if !min_x.is_finite() || !min_y.is_finite() || !max_x.is_finite() || !max_y.is_finite() {
        return Err(Error::InvalidInput(
            "CMMN diagram has no finite geometry".into(),
        ));
    }
    Ok((min_x, min_y, max_x, max_y))
}

fn draw_shape(
    page: &mut Page,
    shape: &Shape,
    element: &ModelElement,
    model: &HashMap<String, ModelElement>,
    scale: f64,
    map_point: &impl Fn(f64, f64) -> (f64, f64),
    warnings: &mut Vec<String>,
) -> Result<()> {
    let definition = if element.kind == "planItem" {
        element
            .definition_ref
            .as_deref()
            .and_then(|reference| model.get(reference_local(reference)))
    } else {
        None
    };
    let kind = definition
        .map(|definition| definition.kind.as_str())
        .unwrap_or(&element.kind);
    let label = if element.name.is_empty() {
        definition
            .map(|definition| definition.name.as_str())
            .unwrap_or("")
    } else {
        element.name.as_str()
    };
    let rect = shape.rect;
    let (x, y) = map_point(rect.x, rect.y);
    let width = rect.width * scale;
    let height = rect.height * scale;
    let container = matches!(kind, "casePlanModel" | "stage");
    let milestone = kind == "milestone";
    let event = matches!(
        kind,
        "eventListener" | "userEventListener" | "timerEventListener"
    );
    let file_item = kind == "caseFileItem";
    let task = matches!(
        kind,
        "task" | "humanTask" | "processTask" | "caseTask" | "decisionTask"
    );
    let (fill, stroke) = if container {
        ("#FFF7DB", "#C9A227")
    } else if milestone {
        ("#D5E8D4", "#82B366")
    } else if event {
        ("#FFFFFF", "#6C8EBF")
    } else if file_item {
        ("#E1D5E7", "#9673A6")
    } else if task {
        ("#DAE8FC", "#6C8EBF")
    } else {
        warnings.push(format!("CMMN {} shape uses a generic outline", kind));
        ("#F1F5F9", "#64748B")
    };
    let path = if milestone {
        format!(
            "M {:.2} {:.2} L {:.2} {:.2} L {:.2} {:.2} L {:.2} {:.2} Z",
            x + width / 2.0,
            y,
            x + width,
            y + height / 2.0,
            x + width / 2.0,
            y + height,
            x,
            y + height / 2.0
        )
    } else if event {
        ellipse_path(x, y, width, height)
    } else if container {
        case_container_path(x, y, width, height, 30.0_f64.min(width / 3.0))
    } else if file_item {
        folded_document_path(x, y, width, height)
    } else {
        rounded_rect_path(x, y, width, height, if container { 4.0 } else { 8.0 })
    };
    push_node(
        page,
        Node::Path {
            id: format!("cmmn-shape-{}-{}", safe_id(&element.id), page.nodes.len()),
            d: path,
            fill_rule: "nonzero".into(),
            fill: Paint::solid(fill),
            stroke: make_stroke(stroke, if shape.collapsed { 2.0 } else { 1.3 }),
            transform: IDENTITY,
            clip_id: None,
            meta: source_meta(&element.id, &format!("cmmn:{kind}")),
        },
    )?;
    if shape.collapsed {
        add_text(
            page,
            &element.id,
            "+",
            x + width - 10.0,
            y + 13.0,
            10.0,
            "cmmn:collapsed",
        )?;
    }
    if !label.is_empty() {
        if container {
            add_text(
                page,
                &element.id,
                label,
                x + width / 2.0,
                y + 21.0,
                9.0,
                "cmmn:label",
            )?;
        } else if milestone || event {
            add_text(
                page,
                &element.id,
                label,
                x + width / 2.0,
                y + height + 13.0,
                8.5,
                "cmmn:label",
            )?;
        } else {
            let lines = wrap_label(label, ((width / 5.5).floor() as usize).clamp(6, 28), 3);
            let line_height = 11.0;
            let start_y =
                y + height / 2.0 - (lines.len().saturating_sub(1) as f64 * line_height / 2.0) + 3.5;
            for (index, line) in lines.iter().enumerate() {
                add_text(
                    page,
                    &element.id,
                    line,
                    x + width / 2.0,
                    start_y + index as f64 * line_height,
                    9.0,
                    "cmmn:label",
                )?;
            }
        }
    }
    if kind == "planItem"
        || definition.is_some_and(|definition| definition.kind == "planItemDefinition")
    {
        warnings.push(format!(
            "CMMN shape {:?} has an unresolved plan-item definition",
            element.id
        ));
    }
    Ok(())
}

fn draw_edge(
    page: &mut Page,
    edge: &Edge,
    label: &str,
    scale: f64,
    map_point: &impl Fn(f64, f64) -> (f64, f64),
) -> Result<()> {
    let points = edge
        .points
        .iter()
        .map(|point| map_point(point.x, point.y))
        .collect::<Vec<_>>();
    let mut d = format!("M {:.2} {:.2}", points[0].0, points[0].1);
    for point in points.iter().skip(1) {
        d.push_str(&format!(" L {:.2} {:.2}", point.0, point.1));
    }
    push_node(
        page,
        Node::Path {
            id: format!("cmmn-edge-{}-{}", safe_id(&edge.model_id), page.nodes.len()),
            d,
            fill_rule: "nonzero".into(),
            fill: Paint::None,
            stroke: Stroke {
                dash_array: vec![3.5, 2.5],
                ..make_stroke("#64748B", 1.2)
            },
            transform: IDENTITY,
            clip_id: None,
            meta: source_meta(&edge.model_id, "cmmn:connector"),
        },
    )?;
    if !label.is_empty() {
        let midpoint = points[points.len() / 2];
        add_text(
            page,
            &edge.model_id,
            label,
            midpoint.0,
            midpoint.1 - 5.0,
            8.0,
            "cmmn:connector-label",
        )?;
    }
    let _ = scale;
    Ok(())
}

fn case_container_path(x: f64, y: f64, width: f64, height: f64, tab_width: f64) -> String {
    let tab_height = 14.0_f64.min(height / 4.0);
    let top = y + tab_height;
    let right = x + width;
    let bottom = y + height;
    format!(
        "M {x:.2} {top:.2} H {tab_right:.2} L {tab_tip:.2} {y:.2} H {right:.2} Q {right:.2} {top:.2} {right:.2} {right_curve:.2} V {bottom_curve:.2} Q {right:.2} {bottom:.2} {right_bottom:.2} {bottom:.2} H {left_bottom:.2} Q {x:.2} {bottom:.2} {x:.2} {left_curve:.2} V {left_curve_top:.2} Q {x:.2} {top:.2} {left_top:.2} {top:.2} Z",
        tab_right = x + tab_width,
        tab_tip = x + tab_width + 12.0,
        right_curve = top + 5.0,
        bottom_curve = bottom - 5.0,
        right_bottom = right - 5.0,
        left_bottom = x + 5.0,
        left_curve = bottom - 5.0,
        left_curve_top = top + 5.0,
        left_top = x + 5.0,
    )
}

fn folded_document_path(x: f64, y: f64, width: f64, height: f64) -> String {
    let fold = 10.0_f64.min(width / 4.0).min(height / 4.0);
    format!(
        "M {:.2} {:.2} H {:.2} L {:.2} {:.2} V {:.2} H {:.2} Z M {:.2} {:.2} L {:.2} {:.2} L {:.2} {:.2}",
        x,
        y,
        x + width - fold,
        x + width,
        y + fold,
        x + width,
        y + height,
        x,
        y + fold,
        x + width - fold,
        y + fold,
        x + width - fold,
        y
    )
}

fn ellipse_path(x: f64, y: f64, width: f64, height: f64) -> String {
    let rx = width / 2.0;
    let ry = height / 2.0;
    let cx = x + rx;
    let cy = y + ry;
    let k = 0.552_284_749_8;
    format!(
        "M {:.2} {:.2} C {:.2} {:.2} {:.2} {:.2} {:.2} {:.2} C {:.2} {:.2} {:.2} {:.2} {:.2} {:.2} C {:.2} {:.2} {:.2} {:.2} {:.2} {:.2} C {:.2} {:.2} {:.2} {:.2} {:.2} {:.2} Z",
        cx + rx,
        cy,
        cx + rx,
        cy + k * ry,
        cx + k * rx,
        cy + ry,
        cx,
        cy + ry,
        cx - k * rx,
        cy + ry,
        cx - rx,
        cy + k * ry,
        cx - rx,
        cy,
        cx - rx,
        cy - k * ry,
        cx - k * rx,
        cy - ry,
        cx,
        cy - ry,
        cx + k * rx,
        cy - ry,
        cx + rx,
        cy - k * ry,
        cx + rx,
        cy
    )
}

fn rounded_rect_path(x: f64, y: f64, width: f64, height: f64, radius: f64) -> String {
    let r = radius.min(width / 2.0).min(height / 2.0);
    format!(
        "M {:.2} {:.2} H {:.2} Q {:.2} {:.2} {:.2} {:.2} V {:.2} Q {:.2} {:.2} {:.2} {:.2} H {:.2} Q {:.2} {:.2} {:.2} {:.2} V {:.2} Q {:.2} {:.2} {:.2} {:.2} Z",
        x + r,
        y,
        x + width - r,
        x + width,
        y,
        x + width,
        y + r,
        y + height - r,
        x + width,
        y + height,
        x + width - r,
        y + height,
        x + r,
        x,
        y + height,
        x,
        y + height - r,
        y + r,
        x,
        y,
        x + r,
        y
    )
}

fn make_stroke(color: &str, width: f64) -> Stroke {
    Stroke {
        paint: Paint::solid(color),
        width,
        line_cap: LineCap::Round,
        line_join: LineJoin::Round,
        miter_limit: 4.0,
        ..Stroke::default()
    }
}

fn source_meta(id: &str, role: &str) -> SourceMeta {
    SourceMeta {
        kind: "cmmn".into(),
        source_id: id.into(),
        semantic_role: role.into(),
        ..SourceMeta::default()
    }
}

fn add_text(
    page: &mut Page,
    source_id: &str,
    text: &str,
    x: f64,
    y: f64,
    size: f64,
    role: &str,
) -> Result<()> {
    if text.is_empty() {
        return Ok(());
    }
    push_node(
        page,
        Node::Text {
            id: format!("cmmn-text-{}-{}", safe_id(source_id), page.nodes.len()),
            x,
            y,
            runs: vec![TextRun {
                text: text.to_owned(),
                font_size: size,
                fill: Paint::solid("#1E293B"),
                ..TextRun::default()
            }],
            anchor: TextAnchor::Middle,
            transform: IDENTITY,
            opacity: 1.0,
            stroke: Stroke::default(),
            clip_id: None,
            meta: source_meta(source_id, role),
        },
    )
}

fn push_node(page: &mut Page, node: Node) -> Result<()> {
    if page.nodes.len() >= MAX_CMMN_RENDERED_NODES {
        return Err(Error::LimitExceeded(format!(
            "CMMN page exceeds {MAX_CMMN_RENDERED_NODES} rendered nodes"
        )));
    }
    page.nodes.push(node);
    Ok(())
}

fn wrap_label(text: &str, max_chars: usize, max_lines: usize) -> Vec<String> {
    let mut lines = Vec::new();
    let mut current = String::new();
    for word in text.split_whitespace() {
        if current.is_empty() {
            current.push_str(word);
        } else if current.chars().count() + word.chars().count() < max_chars {
            current.push(' ');
            current.push_str(word);
        } else {
            lines.push(std::mem::take(&mut current));
            current.push_str(word);
            if lines.len() + 1 >= max_lines {
                break;
            }
        }
    }
    if !current.is_empty() && lines.len() < max_lines {
        lines.push(current);
    }
    lines
}

fn safe_id(value: &str) -> String {
    value
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.') {
                character
            } else {
                '_'
            }
        })
        .collect::<String>()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognizes_cmmn_11_definitions_namespace() {
        let source = format!("<definitions xmlns=\"{CMMN_MODEL_NS}\"/>");
        assert!(looks_like_prefix(source.as_bytes()));
        assert!(!looks_like_prefix(b"<definitions/>"));
    }

    #[test]
    fn rejects_doctypes_and_wrong_model_namespaces() {
        let source = format!(
            "<!DOCTYPE definitions SYSTEM \"https://example.invalid/cmmn.dtd\"><definitions xmlns=\"{CMMN_MODEL_NS}\"/>"
        );
        assert!(parse_cmmn(source.as_bytes()).is_err());
        assert!(matches!(
            parse_cmmn(b"<definitions/>"),
            Err(Error::Unsupported(_))
        ));
    }
}
