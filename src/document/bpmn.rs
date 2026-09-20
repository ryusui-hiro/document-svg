//! Bounded BPMN 2.0 diagram previews using the model and Diagram Interchange data.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use crate::convert::{ConvertOptions, PageConsumer, read_limited_file};
use crate::error::{Error, Result};
use crate::geospatial::xml_tree::{XmlElement, XmlLimits, parse_xml_tree};
use crate::ir::{
    IDENTITY, LineCap, LineJoin, Node, Page, Paint, SourceMeta, Stroke, TextAnchor, TextRun,
};

const BPMN_MODEL_NS: &str = "http://www.omg.org/spec/BPMN/20100524/MODEL";
const BPMN_DI_NS: &str = "http://www.omg.org/spec/BPMN/20100524/DI";
const DI_NS: &str = "http://www.omg.org/spec/DD/20100524/DI";
const DC_NS: &str = "http://www.omg.org/spec/DD/20100524/DC";

const MAX_BPMN_BYTES: u64 = 32 * 1024 * 1024;
const MAX_BPMN_EVENTS: usize = 500_000;
const MAX_BPMN_ELEMENTS: usize = 200_000;
const MAX_BPMN_DEPTH: usize = 80;
const MAX_BPMN_TEXT_BYTES: usize = 16 * 1024 * 1024;
const MAX_BPMN_DIAGRAMS: usize = 100;
const MAX_RENDERED_NODES: usize = 200_000;
const MAX_COORDINATE: f64 = 10_000_000.0;

const PAGE_WIDTH: f64 = 595.0;
const PAGE_HEIGHT: f64 = 842.0;
const PAGE_MARGIN: f64 = 38.0;

#[derive(Clone)]
struct ModelElement {
    id: String,
    name: String,
    kind: String,
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
}

#[derive(Clone)]
struct Waypoint {
    x: f64,
    y: f64,
}

#[derive(Clone)]
struct Edge {
    model_id: String,
    points: Vec<Waypoint>,
}

type DiagramGeometry = (String, Vec<Shape>, Vec<Edge>);

pub(crate) fn convert(
    path: &Path,
    options: &ConvertOptions,
    sink: &mut dyn PageConsumer,
) -> Result<Vec<String>> {
    let bytes = read_limited_file(
        path,
        options.max_input_bytes.min(MAX_BPMN_BYTES),
        "BPMN input",
    )?;
    let root = parse_xml_tree(
        &bytes,
        &XmlLimits {
            max_events: MAX_BPMN_EVENTS,
            max_nodes: MAX_BPMN_ELEMENTS,
            max_depth: MAX_BPMN_DEPTH,
            max_text_bytes: MAX_BPMN_TEXT_BYTES,
        },
        "BPMN",
    )?;
    if root.name != "definitions" || root.namespace.as_deref() != Some(BPMN_MODEL_NS) {
        return Err(Error::Unsupported(
            "BPMN input must have a BPMN 2.0 definitions root".into(),
        ));
    }
    let models = collect_model(&root)?;
    let diagrams = collect_diagrams(&root)?;
    if diagrams.is_empty() {
        return Err(Error::Unsupported(
            "BPMN document has no BPMN Diagram Interchange geometry to preview".into(),
        ));
    }
    if diagrams.len() > MAX_BPMN_DIAGRAMS {
        return Err(Error::LimitExceeded(format!(
            "BPMN input exceeds {MAX_BPMN_DIAGRAMS} diagrams"
        )));
    }
    if diagrams.len() > options.max_pages {
        return Err(Error::LimitExceeded(format!(
            "BPMN input contains {} diagrams; maximum is {}",
            diagrams.len(),
            options.max_pages
        )));
    }
    if options.max_pages == 0 {
        return Err(Error::LimitExceeded(
            "BPMN conversion requires at least one page; max_pages is zero".into(),
        ));
    }

    let mut warnings = Vec::new();
    for (index, (name, shapes, edges)) in diagrams.iter().enumerate() {
        let (mut page, page_warnings) = render_diagram(index + 1, name, shapes, edges, &models)?;
        for warning in &page_warnings {
            if !warnings.contains(warning) {
                warnings.push(warning.clone());
            }
        }
        for warning in page_warnings {
            page.warn(warning);
        }
        sink.consume(page)?;
    }
    Ok(warnings)
}

pub(crate) fn looks_like_prefix(bytes: &[u8]) -> bool {
    crate::geospatial::xml_tree::looks_like_root(
        bytes,
        b"definitions",
        Some(BPMN_MODEL_NS.as_bytes()),
    )
}

fn collect_model(root: &XmlElement) -> Result<HashMap<String, ModelElement>> {
    let mut output = HashMap::new();
    let mut nodes = Vec::new();
    collect_elements(root, &mut nodes);
    for element in nodes {
        if element.namespace.as_deref() != Some(BPMN_MODEL_NS) {
            continue;
        }
        let Some(id) = element.attribute("id") else {
            continue;
        };
        let item = ModelElement {
            id: id.to_owned(),
            name: element.attribute("name").unwrap_or_default().to_owned(),
            kind: element.name.clone(),
        };
        if output.insert(id.to_owned(), item).is_some() {
            return Err(Error::InvalidInput(format!(
                "BPMN document contains duplicate model id {id:?}"
            )));
        }
    }
    Ok(output)
}

fn collect_elements<'a>(element: &'a XmlElement, output: &mut Vec<&'a XmlElement>) {
    output.push(element);
    for child in &element.children {
        collect_elements(child, output);
    }
}

fn is_supported_node(name: &str) -> bool {
    matches!(
        name,
        "startEvent"
            | "endEvent"
            | "intermediateCatchEvent"
            | "intermediateThrowEvent"
            | "boundaryEvent"
            | "task"
            | "userTask"
            | "serviceTask"
            | "manualTask"
            | "scriptTask"
            | "sendTask"
            | "receiveTask"
            | "businessRuleTask"
            | "callActivity"
            | "subProcess"
            | "transaction"
            | "exclusiveGateway"
            | "inclusiveGateway"
            | "parallelGateway"
            | "eventBasedGateway"
            | "complexGateway"
            | "participant"
            | "lane"
    )
}

fn collect_diagrams(root: &XmlElement) -> Result<Vec<DiagramGeometry>> {
    let mut diagrams = Vec::new();
    let mut elements = Vec::new();
    collect_elements(root, &mut elements);
    for diagram in elements.into_iter().filter(|element| {
        element.name == "BPMNDiagram" && element.namespace.as_deref() == Some(BPMN_DI_NS)
    }) {
        let name = diagram.attribute("name").unwrap_or_default().to_owned();
        let plane = diagram
            .children
            .iter()
            .find(|child| {
                child.name == "BPMNPlane" && child.namespace.as_deref() == Some(BPMN_DI_NS)
            })
            .ok_or_else(|| Error::InvalidInput("BPMNDiagram has no BPMNPlane".into()))?;
        let mut shapes = Vec::new();
        let mut edges = Vec::new();
        for child in &plane.children {
            match (child.namespace.as_deref(), child.name.as_str()) {
                (Some(BPMN_DI_NS), "BPMNShape") => {
                    shapes.push(parse_shape(child)?);
                }
                (Some(BPMN_DI_NS), "BPMNEdge") => {
                    edges.push(parse_edge(child)?);
                }
                _ => {}
            }
        }
        if shapes.is_empty() {
            return Err(Error::InvalidInput(
                "BPMN diagram has no BPMNShape geometry".into(),
            ));
        }
        diagrams.push((name, shapes, edges));
    }
    Ok(diagrams)
}

fn parse_shape(element: &XmlElement) -> Result<Shape> {
    let model_id = element
        .attribute("bpmnElement")
        .ok_or_else(|| Error::InvalidInput("BPMNShape is missing bpmnElement".into()))?;
    let bounds = element
        .children
        .iter()
        .find(|child| child.name == "Bounds" && child.namespace.as_deref() == Some(DC_NS))
        .ok_or_else(|| Error::InvalidInput("BPMNShape is missing DC Bounds".into()))?;
    let rect = Rect {
        x: coordinate(bounds.attribute("x"), "x")?,
        y: coordinate(bounds.attribute("y"), "y")?,
        width: positive_dimension(bounds.attribute("width"), "width")?,
        height: positive_dimension(bounds.attribute("height"), "height")?,
    };
    Ok(Shape {
        model_id: qname_local(model_id).to_owned(),
        rect,
    })
}

fn parse_edge(element: &XmlElement) -> Result<Edge> {
    let model_id = element
        .attribute("bpmnElement")
        .ok_or_else(|| Error::InvalidInput("BPMNEdge is missing bpmnElement".into()))?;
    let points = element
        .children
        .iter()
        .filter(|child| child.name == "waypoint" && child.namespace.as_deref() == Some(DI_NS))
        .map(|point| {
            Ok(Waypoint {
                x: coordinate(point.attribute("x"), "waypoint x")?,
                y: coordinate(point.attribute("y"), "waypoint y")?,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    if points.len() < 2 {
        return Err(Error::InvalidInput(
            "BPMNEdge requires at least two DI waypoints".into(),
        ));
    }
    Ok(Edge {
        model_id: qname_local(model_id).to_owned(),
        points,
    })
}

fn coordinate(value: Option<&str>, label: &str) -> Result<f64> {
    let value = value
        .ok_or_else(|| Error::InvalidInput(format!("BPMN DI is missing {label}")))?
        .parse::<f64>()
        .map_err(|_| Error::InvalidInput(format!("BPMN DI has invalid {label}")))?;
    if !value.is_finite() || value.abs() > MAX_COORDINATE {
        return Err(Error::LimitExceeded(format!(
            "BPMN DI {label} exceeds coordinate bounds"
        )));
    }
    Ok(value)
}

fn positive_dimension(value: Option<&str>, label: &str) -> Result<f64> {
    let value = coordinate(value, label)?;
    if value <= 0.0 {
        return Err(Error::InvalidInput(format!(
            "BPMN DI {label} must be positive"
        )));
    }
    Ok(value)
}

fn qname_local(value: &str) -> &str {
    value.rsplit(':').next().unwrap_or(value)
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
    let raw_width = (max_x - min_x).max(1.0);
    let raw_height = (max_y - min_y).max(1.0);
    let scale = (content_width / raw_width)
        .min(content_height / raw_height)
        .min(2.0);
    let map_point = |x: f64, y: f64| {
        (
            PAGE_MARGIN + (x - min_x) * scale,
            PAGE_MARGIN + (y - min_y) * scale,
        )
    };
    let mut page = Page::new(number, PAGE_WIDTH, PAGE_HEIGHT, "bpmn");
    page.title = if name.trim().is_empty() {
        format!("BPMN diagram {number}")
    } else {
        name.to_owned()
    };
    let unresolved_shapes = shapes
        .iter()
        .filter(|shape| !model.contains_key(&shape.model_id))
        .count();
    if unresolved_shapes > 0 {
        warn_once(
            &mut warnings,
            format!(
                "{unresolved_shapes} BPMNShape reference(s) have no model element and were omitted"
            ),
        );
    }

    // Participants and lanes form the background; draw them before flows and activities.
    for shape in shapes.iter().filter(|shape| {
        model
            .get(&shape.model_id)
            .is_some_and(|element| matches!(element.kind.as_str(), "participant" | "lane"))
    }) {
        let Some(element) = model.get(&shape.model_id) else {
            continue;
        };
        if !is_supported_node(&element.kind) {
            warn_once(
                &mut warnings,
                format!(
                    "BPMN {} element(s) are displayed as generic activity shapes",
                    element.kind
                ),
            );
        }
        draw_shape(&mut page, shape, element, scale, &map_point)?;
    }
    for edge in edges {
        let Some(element) = model.get(&edge.model_id) else {
            warn_once(
                &mut warnings,
                format!(
                    "BPMN DI edge references missing model element {:?}",
                    edge.model_id
                ),
            );
            continue;
        };
        if element.kind != "sequenceFlow" {
            warn_once(
                &mut warnings,
                format!("BPMN {} edges are omitted", element.kind),
            );
            continue;
        }
        draw_edge(&mut page, edge, element, scale, &map_point)?;
    }
    for shape in shapes.iter().filter(|shape| {
        model
            .get(&shape.model_id)
            .is_some_and(|element| !matches!(element.kind.as_str(), "participant" | "lane"))
    }) {
        let Some(element) = model.get(&shape.model_id) else {
            warn_once(
                &mut warnings,
                format!(
                    "BPMN DI shape references missing model element {:?}",
                    shape.model_id
                ),
            );
            continue;
        };
        draw_shape(&mut page, shape, element, scale, &map_point)?;
    }
    let rendered_ids = shapes
        .iter()
        .map(|shape| shape.model_id.as_str())
        .collect::<HashSet<_>>();
    let missing = model
        .values()
        .filter(|element| {
            is_supported_node(&element.kind) && !rendered_ids.contains(element.id.as_str())
        })
        .count();
    if missing > 0 {
        warn_once(
            &mut warnings,
            format!("{missing} BPMN model element(s) have no BPMNShape and were omitted"),
        );
    }
    Ok((page, warnings))
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
            "BPMN diagram has no finite geometry".into(),
        ));
    }
    Ok((min_x, min_y, max_x, max_y))
}

fn draw_shape(
    page: &mut Page,
    shape: &Shape,
    element: &ModelElement,
    scale: f64,
    map_point: &impl Fn(f64, f64) -> (f64, f64),
) -> Result<()> {
    let rect = shape.rect;
    let (x, y) = map_point(rect.x, rect.y);
    let width = rect.width * scale;
    let height = rect.height * scale;
    let event = matches!(
        element.kind.as_str(),
        "startEvent"
            | "endEvent"
            | "intermediateCatchEvent"
            | "intermediateThrowEvent"
            | "boundaryEvent"
    );
    let gateway = element.kind.ends_with("Gateway");
    let container = matches!(element.kind.as_str(), "participant" | "lane");
    let (fill, stroke, stroke_width) = match element.kind.as_str() {
        "startEvent" => ("#D5E8D4", "#82B366", 2.0),
        "endEvent" => ("#F8CECC", "#B85450", 3.0),
        kind if kind.contains("Event") => ("#FFFFFF", "#6C8EBF", 2.0),
        kind if kind.ends_with("Gateway") => ("#FFF2CC", "#D6B656", 1.8),
        "participant" => ("#F8FAFC", "#475569", 1.4),
        "lane" => ("#FFFFFF", "#64748B", 1.0),
        _ => ("#DAE8FC", "#6C8EBF", 1.4),
    };
    let d = if event {
        ellipse_path(x, y, width, height)
    } else if gateway {
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
    } else {
        rounded_rect_path(x, y, width, height, if container { 2.0 } else { 7.0 })
    };
    push_node(
        page,
        Node::Path {
            id: format!("bpmn-shape-{}-{}", safe_id(&element.id), page.nodes.len()),
            d,
            fill_rule: "nonzero".into(),
            fill: Paint::solid(fill),
            stroke: make_stroke(stroke, stroke_width),
            transform: IDENTITY,
            clip_id: None,
            meta: source_meta(&element.id, &element.kind),
        },
    )?;
    if event && element.kind == "endEvent" {
        let inset = 4.0 * scale.max(0.6);
        push_node(
            page,
            Node::Path {
                id: format!(
                    "bpmn-event-inner-{}-{}",
                    safe_id(&element.id),
                    page.nodes.len()
                ),
                d: ellipse_path(
                    x + inset,
                    y + inset,
                    (width - inset * 2.0).max(1.0),
                    (height - inset * 2.0).max(1.0),
                ),
                fill_rule: "nonzero".into(),
                fill: Paint::None,
                stroke: make_stroke(stroke, 1.0),
                transform: IDENTITY,
                clip_id: None,
                meta: source_meta(&element.id, "bpmn:end-event-marker"),
            },
        )?;
    }
    if gateway {
        let marker = match element.kind.as_str() {
            "parallelGateway" => "+",
            "inclusiveGateway" => "○",
            "eventBasedGateway" => "◇",
            "complexGateway" => "*",
            _ => "×",
        };
        add_text(
            page,
            &element.id,
            marker,
            x + width / 2.0,
            y + height / 2.0 + 4.0,
            16.0 * scale.clamp(0.65, 1.0),
            "bpmn:gateway-marker",
        )?;
    }
    if !element.name.trim().is_empty() {
        if gateway {
            add_text(
                page,
                &element.id,
                &element.name,
                x + width / 2.0,
                y - 9.0,
                9.0,
                "bpmn:label",
            )?;
        } else if event {
            add_text(
                page,
                &element.id,
                &element.name,
                x + width / 2.0,
                y + height + 13.0,
                9.0,
                "bpmn:label",
            )?;
        } else {
            let max_chars = ((width / 5.5).floor() as usize).clamp(6, 28);
            let lines = wrap_label(&element.name, max_chars, 3);
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
                    "bpmn:label",
                )?;
            }
        }
    }
    Ok(())
}

fn draw_edge(
    page: &mut Page,
    edge: &Edge,
    element: &ModelElement,
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
    let color = "#64748B";
    push_node(
        page,
        Node::Path {
            id: format!("bpmn-flow-{}-{}", safe_id(&element.id), page.nodes.len()),
            d,
            fill_rule: "nonzero".into(),
            fill: Paint::None,
            stroke: make_stroke(color, 1.4),
            transform: IDENTITY,
            clip_id: None,
            meta: source_meta(&element.id, "bpmn:sequence-flow"),
        },
    )?;
    if let [.., previous, tip] = points.as_slice() {
        let dx = tip.0 - previous.0;
        let dy = tip.1 - previous.1;
        let length = dx.hypot(dy).max(0.001);
        let ux = dx / length;
        let uy = dy / length;
        let arrow_length = 8.0 * scale.clamp(0.55, 1.0);
        let half_width = 3.6 * scale.clamp(0.55, 1.0);
        let base_x = tip.0 - ux * arrow_length;
        let base_y = tip.1 - uy * arrow_length;
        let left_x = base_x - uy * half_width;
        let left_y = base_y + ux * half_width;
        let right_x = base_x + uy * half_width;
        let right_y = base_y - ux * half_width;
        push_node(
            page,
            Node::Path {
                id: format!(
                    "bpmn-flow-arrow-{}-{}",
                    safe_id(&element.id),
                    page.nodes.len()
                ),
                d: format!(
                    "M {:.2} {:.2} L {:.2} {:.2} L {:.2} {:.2} Z",
                    tip.0, tip.1, left_x, left_y, right_x, right_y
                ),
                fill_rule: "nonzero".into(),
                fill: Paint::solid(color),
                stroke: Stroke::default(),
                transform: IDENTITY,
                clip_id: None,
                meta: source_meta(&element.id, "bpmn:sequence-flow-arrow"),
            },
        )?;
    }
    if !element.name.trim().is_empty() {
        let midpoint = points[points.len() / 2];
        add_text(
            page,
            &element.id,
            &element.name,
            midpoint.0,
            midpoint.1 - 5.0,
            8.0,
            "bpmn:flow-label",
        )?;
    }
    Ok(())
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
        kind: "bpmn".into(),
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
    font_size: f64,
    role: &str,
) -> Result<()> {
    if text.is_empty() {
        return Ok(());
    }
    push_node(
        page,
        Node::Text {
            id: format!("bpmn-text-{}-{}", safe_id(source_id), page.nodes.len()),
            x,
            y,
            runs: vec![TextRun {
                text: text.to_owned(),
                font_size,
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
    if page.nodes.len() >= MAX_RENDERED_NODES {
        return Err(Error::LimitExceeded(format!(
            "BPMN page exceeds {MAX_RENDERED_NODES} rendered nodes"
        )));
    }
    page.nodes.push(node);
    Ok(())
}

fn wrap_label(label: &str, max_chars: usize, max_lines: usize) -> Vec<String> {
    let mut lines = Vec::new();
    let mut current = String::new();
    for word in label.split_whitespace() {
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
    if lines.is_empty() {
        vec![label.chars().take(max_chars).collect()]
    } else {
        lines
    }
}

fn safe_id(value: &str) -> String {
    let mut output = value
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.') {
                character
            } else {
                '_'
            }
        })
        .collect::<String>();
    if output.is_empty() {
        output.push('x');
    }
    output
}

fn warn_once(warnings: &mut Vec<String>, warning: String) {
    if !warnings.contains(&warning) {
        warnings.push(warning);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognizes_only_bpmn_20_definitions_roots() {
        let bpmn = format!("<definitions xmlns=\"{BPMN_MODEL_NS}\"/>");
        assert!(looks_like_prefix(bpmn.as_bytes()));
        assert!(!looks_like_prefix(b"<definitions/>"));
    }

    #[test]
    fn wraps_labels_without_losing_words() {
        let lines = wrap_label("Review and approve request", 12, 3);
        assert_eq!(lines, ["Review and", "approve", "request"]);
    }
}
