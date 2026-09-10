//! Connectors: where a route leaves and enters a shape, how it turns between
//! them, the arrowheads at each end, and the label along the way.

use super::*;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Side {
    North,
    East,
    South,
    West,
}

impl Side {
    pub(super) fn vector(self) -> (f64, f64) {
        match self {
            Self::North => (0.0, -1.0),
            Self::East => (1.0, 0.0),
            Self::South => (0.0, 1.0),
            Self::West => (-1.0, 0.0),
        }
    }

    pub(super) fn horizontal(self) -> bool {
        matches!(self, Self::East | Self::West)
    }

    pub(super) fn opposite(self) -> Self {
        match self {
            Self::North => Self::South,
            Self::East => Self::West,
            Self::South => Self::North,
            Self::West => Self::East,
        }
    }

    pub(super) fn anchor(self, rect: Rect) -> (f64, f64) {
        let (cx, cy) = rect.center();
        match self {
            Self::North => (cx, rect.y),
            Self::East => (rect.right(), cy),
            Self::South => (cx, rect.bottom()),
            Self::West => (rect.x, cy),
        }
    }
}

/// Which sides an orthogonal route should leave and enter by.
///
/// mxGraph's `OrthConnector` scores the four sides against a table of route
/// patterns indexed by the quadrant the two shapes sit in. This picks the same
/// side that table does in the ordinary cases: the axis on which the two boxes
/// are actually separated, and the axis with the larger centre distance when
/// they overlap on both.
pub(super) fn choose_sides(source: Rect, target: Rect) -> (Side, Side) {
    let horizontal_gap = if target.x >= source.right() {
        target.x - source.right()
    } else if source.x >= target.right() {
        source.x - target.right()
    } else {
        -1.0
    };
    let vertical_gap = if target.y >= source.bottom() {
        target.y - source.bottom()
    } else if source.y >= target.bottom() {
        source.y - target.bottom()
    } else {
        -1.0
    };
    let (source_center, target_center) = (source.center(), target.center());
    let dx = target_center.0 - source_center.0;
    let dy = target_center.1 - source_center.1;
    let horizontal = if horizontal_gap >= 0.0 && vertical_gap >= 0.0 {
        horizontal_gap >= vertical_gap
    } else if horizontal_gap >= 0.0 {
        true
    } else if vertical_gap >= 0.0 {
        false
    } else {
        dx.abs() >= dy.abs()
    };
    let side = if horizontal {
        if dx >= 0.0 { Side::East } else { Side::West }
    } else if dy >= 0.0 {
        Side::South
    } else {
        Side::North
    };
    (side, side.opposite())
}

/// The side a fixed connection point sits on, or `None` when it is inside the
/// shape and draw.io would treat it as floating.
pub(super) fn fixed_side(x: f64, y: f64) -> Option<Side> {
    const EPSILON: f64 = 0.01;
    if x.abs() < EPSILON {
        Some(Side::West)
    } else if (x - 1.0).abs() < EPSILON {
        Some(Side::East)
    } else if y.abs() < EPSILON {
        Some(Side::North)
    } else if (y - 1.0).abs() < EPSILON {
        Some(Side::South)
    } else {
        None
    }
}

pub(super) struct Endpoint {
    pub(super) point: (f64, f64),
    pub(super) side: Option<Side>,
    pub(super) fixed: bool,
}

pub(super) fn endpoint(style: &Style, rect: Option<Rect>, prefix: &str) -> Option<Endpoint> {
    let rect = rect?;
    let x = style.get(&format!("{prefix}x"))?.parse::<f64>().ok()?;
    let y = style.get(&format!("{prefix}y"))?.parse::<f64>().ok()?;
    if !x.is_finite() || !y.is_finite() {
        return None;
    }
    let dx = style.number(&format!("{prefix}dx"), 0.0);
    let dy = style.number(&format!("{prefix}dy"), 0.0);
    Some(Endpoint {
        point: (rect.x + rect.width * x + dx, rect.y + rect.height * y + dy),
        side: fixed_side(x, y),
        fixed: true,
    })
}

/// Where a straight edge meets a shape.
///
/// draw.io names the rule in the style — `perimeter=ellipsePerimeter` and its
/// kin — and falls back to the shape's own outline when it does not. Attaching
/// a connector to a triangle or a hexagon at its bounding box, rather than at
/// the outline, leaves a visible gap between the line and the shape.
pub(super) fn perimeter_point(
    cell: &Cell,
    rect: Rect,
    toward: (f64, f64),
    shape: &str,
) -> (f64, f64) {
    let (cx, cy) = rect.center();
    let (dx, dy) = (toward.0 - cx, toward.1 - cy);
    if dx.abs() < f64::EPSILON && dy.abs() < f64::EPSILON {
        return (cx, cy);
    }
    let (rx, ry) = (rect.width / 2.0, rect.height / 2.0);
    if rx <= 0.0 || ry <= 0.0 {
        return (cx, cy);
    }
    let ellipse = || {
        let scale = ((dx / rx).powi(2) + (dy / ry).powi(2)).sqrt();
        if scale <= f64::EPSILON {
            (cx, cy)
        } else {
            (cx + dx / scale, cy + dy / scale)
        }
    };
    let rhombus = || {
        let scale = (dx / rx).abs() + (dy / ry).abs();
        if scale <= f64::EPSILON {
            (cx, cy)
        } else {
            (cx + dx / scale, cy + dy / scale)
        }
    };
    let rectangle = || {
        let scale = (dx / rx).abs().max((dy / ry).abs());
        if scale <= f64::EPSILON {
            (cx, cy)
        } else {
            (cx + dx / scale, cy + dy / scale)
        }
    };
    let named = cell.style.text("perimeter", String::new().as_str());
    match named.as_str() {
        "ellipsePerimeter" => return ellipse(),
        "rhombusPerimeter" => return rhombus(),
        "centerPerimeter" => return (cx, cy),
        // A lifeline hangs from the middle of its head, so a connector meets it
        // on the vertical line rather than on the box around it.
        "lifelinePerimeter" | "backbonePerimeter" => {
            return (cx, toward.1.clamp(rect.y, rect.bottom()));
        }
        _ => {}
    }
    let polygon = match named.as_str() {
        "trianglePerimeter" => perimeter_polygon("triangle", cell, rect),
        "hexagonPerimeter" | "hexagonPerimeter2" => perimeter_polygon("hexagon", cell, rect),
        "stepPerimeter" => perimeter_polygon("step", cell, rect),
        "parallelogramPerimeter" => perimeter_polygon("parallelogram", cell, rect),
        "trapezoidPerimeter" => perimeter_polygon("trapezoid", cell, rect),
        "rectanglePerimeter" | "orthogonalPerimeter" | "calloutPerimeter" => None,
        // No rule named: follow the shape that is actually drawn.
        "" => match shape {
            "ellipse" | "doubleEllipse" | "actor" | "cloud" | "startState" | "endState" => {
                return ellipse();
            }
            "rhombus" => return rhombus(),
            other => perimeter_polygon(other, cell, rect),
        },
        _ => None,
    };
    match polygon.and_then(|points| ray_hits_polygon(&points, (cx, cy), (dx, dy))) {
        Some(point) => point,
        None => rectangle(),
    }
}

/// The outline of a shape whose perimeter is a polygon, in page coordinates
/// with the shape's own direction and rotation already applied.
pub(super) fn perimeter_polygon(shape: &str, cell: &Cell, rect: Rect) -> Option<Vec<(f64, f64)>> {
    let style = &cell.style;
    let (drawing, turn) = directed_rect(rect, &style.text("direction", "east"));
    let Rect { x, y, .. } = drawing;
    let width = drawing.width;
    let (right, bottom) = (drawing.right(), drawing.bottom());
    let (cx, cy) = drawing.center();
    let points = match shape {
        "triangle" => vec![(x, y), (right, cy), (x, bottom)],
        "hexagon" => vec![
            (x + width * 0.25, y),
            (x + width * 0.75, y),
            (right, cy),
            (x + width * 0.75, bottom),
            (x + width * 0.25, bottom),
            (x, cy),
        ],
        "parallelogram" => {
            let dx = width * style.number("size", 0.2).clamp(0.0, 1.0);
            vec![(x, bottom), (x + dx, y), (right, y), (right - dx, bottom)]
        }
        "trapezoid" => {
            let dx = width * style.number("size", 0.2).clamp(0.0, 0.5);
            vec![(x, bottom), (x + dx, y), (right - dx, y), (right, bottom)]
        }
        "step" => {
            let dx = width * style.number("size", 0.2).clamp(0.0, 1.0);
            vec![
                (x, y),
                (right - dx, y),
                (right, cy),
                (right - dx, bottom),
                (x, bottom),
                (x + dx, cy),
            ]
        }
        "extract" | "merge" if shape == "extract" => vec![(cx, y), (right, bottom), (x, bottom)],
        "merge" => vec![(x, y), (right, y), (cx, bottom)],
        _ => return None,
    };
    let matrix = shape_transform(
        rect,
        style.number("rotation", 0.0) + turn,
        style.flag("fliph"),
        style.flag("flipv"),
    );
    Some(
        points
            .into_iter()
            .map(|(px, py)| crate::ir::transform_point(matrix, px, py))
            .collect(),
    )
}

/// The point where a ray from `origin` first leaves a closed polygon.
pub(super) fn ray_hits_polygon(
    points: &[(f64, f64)],
    origin: (f64, f64),
    direction: (f64, f64),
) -> Option<(f64, f64)> {
    let mut nearest = None::<f64>;
    for pair in 0..points.len() {
        let (ax, ay) = points[pair];
        let (bx, by) = points[(pair + 1) % points.len()];
        let (ex, ey) = (bx - ax, by - ay);
        let denominator = direction.0 * ey - direction.1 * ex;
        if denominator.abs() < 1e-9 {
            continue;
        }
        let (ox, oy) = (ax - origin.0, ay - origin.1);
        let along = (ox * ey - oy * ex) / denominator;
        let across = (ox * direction.1 - oy * direction.0) / -denominator;
        if along >= 0.0 && (0.0..=1.0).contains(&across) {
            nearest = Some(nearest.map_or(along, |best: f64| best.max(along)));
        }
    }
    nearest.map(|along| {
        (
            origin.0 + direction.0 * along,
            origin.1 + direction.1 * along,
        )
    })
}

pub(super) fn shape_of(cell: &Cell) -> &'static str {
    cell.style.shape().drawn
}

pub(super) fn route_edge(scene: &Scene<'_>, position: usize) -> Vec<(f64, f64)> {
    let cell = &scene.cells[position];
    let style = &cell.style;
    let origin = scene.origin(position);
    let geometry = &cell.geometry;
    let source_rect = scene.vertex_rect(&cell.source);
    let target_rect = scene.vertex_rect(&cell.target);
    let waypoints = geometry
        .points
        .iter()
        .map(|(x, y)| (x + origin.0, y + origin.1))
        .collect::<Vec<_>>();
    let source_fixed = endpoint(style, source_rect, "exit");
    let target_fixed = endpoint(style, target_rect, "entry");
    let floating_source = geometry
        .source_point
        .map(|(x, y)| (x + origin.0, y + origin.1));
    let floating_target = geometry
        .target_point
        .map(|(x, y)| (x + origin.0, y + origin.1));
    // `segmentEdgeStyle` routes through its waypoints with square corners, the
    // same shape of route as the orthogonal styles.
    let orthogonal = matches!(
        style.get("edgestyle"),
        Some(
            "orthogonalEdgeStyle"
                | "elbowEdgeStyle"
                | "entityRelationEdgeStyle"
                | "segmentEdgeStyle"
        )
    ) || style.has_bare("orthogonalEdgeStyle")
        || style.has_bare("elbowEdgeStyle")
        || style.has_bare("segmentEdgeStyle");
    let source_cell = scene
        .index
        .get(cell.source.as_str())
        .map(|position| &scene.cells[*position]);
    let target_cell = scene
        .index
        .get(cell.target.as_str())
        .map(|position| &scene.cells[*position]);
    let source_shape = source_cell.map_or("rectangle", |cell| shape_of(cell));
    let target_shape = target_cell.map_or("rectangle", |cell| shape_of(cell));
    if orthogonal
        && (source_rect.is_some() || source_fixed.is_some())
        && let Some(route) = orthogonal_route(
            source_rect,
            target_rect,
            source_fixed.as_ref(),
            target_fixed.as_ref(),
            floating_source,
            floating_target,
            &waypoints,
        )
    {
        return route;
    }
    // Straight or segmented: every anchor is the point where the line leaves
    // the shape's outline.
    let mut points = Vec::new();
    let first_target = waypoints
        .first()
        .copied()
        .or(floating_target)
        .or_else(|| target_fixed.as_ref().map(|end| end.point))
        .or_else(|| target_rect.map(Rect::center));
    let start = match (&source_fixed, source_rect, floating_source) {
        (Some(fixed), _, _) => Some(fixed.point),
        (None, Some(rect), _) => first_target
            .zip(source_cell)
            .map(|(toward, from)| perimeter_point(from, rect, toward, source_shape)),
        (None, None, point) => point,
    };
    if let Some(start) = start {
        points.push(start);
    }
    points.extend(waypoints.iter().copied());
    let last_source = points.last().copied();
    let end = match (&target_fixed, target_rect, floating_target) {
        (Some(fixed), _, _) => Some(fixed.point),
        (None, Some(rect), _) => last_source
            .zip(target_cell)
            .map(|(toward, to)| perimeter_point(to, rect, toward, target_shape)),
        (None, None, point) => point,
    };
    if let Some(end) = end {
        points.push(end);
    }
    simplify(points)
}

#[allow(clippy::too_many_arguments)]
pub(super) fn orthogonal_route(
    source_rect: Option<Rect>,
    target_rect: Option<Rect>,
    source_fixed: Option<&Endpoint>,
    target_fixed: Option<&Endpoint>,
    floating_source: Option<(f64, f64)>,
    floating_target: Option<(f64, f64)>,
    waypoints: &[(f64, f64)],
) -> Option<Vec<(f64, f64)>> {
    let source_box = source_rect.or_else(|| {
        floating_source.map(|(x, y)| Rect {
            x,
            y,
            width: 0.0,
            height: 0.0,
        })
    })?;
    let target_box = target_rect.or_else(|| {
        floating_target.map(|(x, y)| Rect {
            x,
            y,
            width: 0.0,
            height: 0.0,
        })
    })?;
    let (default_source_side, default_target_side) = choose_sides(source_box, target_box);
    let source_side = source_fixed
        .and_then(|end| end.side)
        .unwrap_or(default_source_side);
    let target_side = target_fixed
        .and_then(|end| end.side)
        .unwrap_or(default_target_side);
    let start = source_fixed
        .filter(|end| end.fixed)
        .map_or_else(|| source_side.anchor(source_box), |end| end.point);
    let end = target_fixed
        .filter(|end| end.fixed)
        .map_or_else(|| target_side.anchor(target_box), |end| end.point);
    let (sx, sy) = source_side.vector();
    let (tx, ty) = target_side.vector();
    // mxEdgeStyle.orthBuffer: the route always clears the shape before it is
    // allowed to turn.
    let source_jetty = (start.0 + sx * ORTH_BUFFER, start.1 + sy * ORTH_BUFFER);
    let target_jetty = (end.0 + tx * ORTH_BUFFER, end.1 + ty * ORTH_BUFFER);
    // The chain has to arrive at the target's jetty travelling along the side
    // the edge enters by; the jetty-to-shape segment is already on that axis,
    // so it is appended afterwards rather than routed. Without this the turn
    // lands on the jetty instead of halfway between the two shapes.
    let mut through = vec![start, source_jetty];
    through.extend(waypoints.iter().copied());
    through.push(target_jetty);
    let mut route = orthogonal_chain(&through, source_side.horizontal(), target_side.horizontal());
    route.push(end);
    Some(simplify(route))
}

/// Insert the corners that make every segment axis-aligned.
///
/// The first segment keeps the direction the route left the shape by and the
/// last one keeps the direction it has to arrive by, which is what makes an
/// orthogonal edge meet a shape square on.
pub(super) fn orthogonal_chain(
    points: &[(f64, f64)],
    start_horizontal: bool,
    end_horizontal: bool,
) -> Vec<(f64, f64)> {
    const EPSILON: f64 = 0.01;
    let mut route: Vec<(f64, f64)> = points.first().copied().into_iter().collect();
    let mut horizontal = start_horizontal;
    for (index, &next) in points.iter().enumerate().skip(1) {
        let current = *route.last().expect("route has a first point");
        let aligned_x = (current.0 - next.0).abs() < EPSILON;
        let aligned_y = (current.1 - next.1).abs() < EPSILON;
        if aligned_x && aligned_y {
            continue;
        }
        let last = index + 1 == points.len();
        if aligned_x || aligned_y {
            // Already a single axis-aligned segment.
            route.push(next);
            horizontal = aligned_y;
            continue;
        }
        if last {
            match (horizontal, end_horizontal) {
                (true, true) => {
                    let middle = f64::midpoint(current.0, next.0);
                    route.push((middle, current.1));
                    route.push((middle, next.1));
                }
                (false, false) => {
                    let middle = f64::midpoint(current.1, next.1);
                    route.push((current.0, middle));
                    route.push((next.0, middle));
                }
                (true, false) => route.push((next.0, current.1)),
                (false, true) => route.push((current.0, next.1)),
            }
            route.push(next);
            horizontal = end_horizontal;
        } else {
            if horizontal {
                route.push((next.0, current.1));
                horizontal = false;
            } else {
                route.push((current.0, next.1));
                horizontal = true;
            }
            route.push(next);
        }
    }
    dedupe(route)
}

/// Drop the points a straight run passes through, so a jetty that happens to
/// line up with the next segment does not become a corner the rounded-corner
/// pass would try to draw an arc around.
pub(super) fn simplify(points: Vec<(f64, f64)>) -> Vec<(f64, f64)> {
    const EPSILON: f64 = 0.01;
    let points = dedupe(points);
    if points.len() < 3 {
        return points;
    }
    let mut result = vec![points[0]];
    for index in 1..points.len() - 1 {
        let previous = *result.last().expect("route has a first point");
        let current = points[index];
        let next = points[index + 1];
        let (ax, ay) = (current.0 - previous.0, current.1 - previous.1);
        let (bx, by) = (next.0 - current.0, next.1 - current.1);
        let collinear = (ax * by - ay * bx).abs() < EPSILON;
        // A point where the route doubles back is collinear too, and it has to
        // stay: dropping it would cut the corner off entirely.
        if !collinear || ax * bx + ay * by < 0.0 {
            result.push(current);
        }
    }
    result.push(*points.last().expect("route has a last point"));
    result
}

pub(super) fn dedupe(points: Vec<(f64, f64)>) -> Vec<(f64, f64)> {
    const EPSILON: f64 = 0.01;
    let mut result = Vec::<(f64, f64)>::with_capacity(points.len());
    for point in points {
        if result.last().is_some_and(|last| {
            (last.0 - point.0).abs() < EPSILON && (last.1 - point.1).abs() < EPSILON
        }) {
            continue;
        }
        result.push(point);
    }
    result
}

pub(super) fn distance(from: (f64, f64), to: (f64, f64)) -> f64 {
    ((to.0 - from.0).powi(2) + (to.1 - from.1).powi(2)).sqrt()
}

pub(super) fn unit(from: (f64, f64), to: (f64, f64)) -> (f64, f64) {
    let length = distance(from, to);
    if length <= f64::EPSILON {
        (0.0, 0.0)
    } else {
        ((to.0 - from.0) / length, (to.1 - from.1) / length)
    }
}

pub(super) fn edge_path(points: &[(f64, f64)], rounded: bool, curved: bool) -> String {
    if points.len() < 2 {
        return String::new();
    }
    let mut path = format!("M {} {}", n(points[0].0), n(points[0].1));
    if points.len() == 2 {
        path.push_str(&format!(" L {} {}", n(points[1].0), n(points[1].1)));
        return path;
    }
    if curved {
        // mxGraph draws a curved connector as quadratic segments whose control
        // points are the waypoints and whose ends are the segment midpoints.
        for index in 1..points.len() - 1 {
            let control = points[index];
            let next = points[index + 1];
            let end = (
                f64::midpoint(control.0, next.0),
                f64::midpoint(control.1, next.1),
            );
            path.push_str(&format!(
                " Q {} {} {} {}",
                n(control.0),
                n(control.1),
                n(end.0),
                n(end.1)
            ));
        }
        let last = points[points.len() - 1];
        path.push_str(&format!(" L {} {}", n(last.0), n(last.1)));
        return path;
    }
    for index in 1..points.len() - 1 {
        let previous = points[index - 1];
        let corner = points[index];
        let next = points[index + 1];
        if !rounded {
            path.push_str(&format!(" L {} {}", n(corner.0), n(corner.1)));
            continue;
        }
        let radius = CONNECTOR_ARC
            .min(distance(previous, corner) / 2.0)
            .min(distance(corner, next) / 2.0);
        if radius <= 0.01 {
            path.push_str(&format!(" L {} {}", n(corner.0), n(corner.1)));
            continue;
        }
        let incoming = unit(corner, previous);
        let outgoing = unit(corner, next);
        path.push_str(&format!(
            " L {} {} Q {} {} {} {}",
            n(corner.0 + incoming.0 * radius),
            n(corner.1 + incoming.1 * radius),
            n(corner.0),
            n(corner.1),
            n(corner.0 + outgoing.0 * radius),
            n(corner.1 + outgoing.1 * radius)
        ));
    }
    let last = points[points.len() - 1];
    path.push_str(&format!(" L {} {}", n(last.0), n(last.1)));
    path
}

/// One arrowhead: its outline, whether it is filled, and how far back along the
/// line it starts so the stroke does not show through the tip.
pub(super) struct Marker {
    pub(super) parts: Vec<(String, MarkerPaint)>,
    pub(super) trim: f64,
}

/// How one piece of an arrowhead is painted.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum MarkerPaint {
    /// Filled and outlined with the line's own colour.
    Solid,
    /// Filled white, which is how an entity-relationship end draws the "zero"
    /// ring over the line it sits on.
    Hollow,
    /// Drawn as lines only.
    Line,
}

impl Marker {
    pub(super) fn solid(d: String, trim: f64) -> Self {
        Self {
            parts: vec![(d, MarkerPaint::Solid)],
            trim,
        }
    }

    pub(super) fn line(d: String, trim: f64) -> Self {
        Self {
            parts: vec![(d, MarkerPaint::Line)],
            trim,
        }
    }

    /// A head that the style may ask to be left unfilled.
    pub(super) fn head(d: String, filled: bool, trim: f64) -> Self {
        if filled {
            Self::solid(d, trim)
        } else {
            Self::line(d, trim)
        }
    }
}

/// Reproduces `mxMarker`: the head is `size + strokeWidth` long, half as wide
/// as it is long for the standard heads and a third as wide for the thin ones,
/// and the classic head notches back to three quarters of its length.
pub(super) fn marker(
    kind: &str,
    tip: (f64, f64),
    direction: (f64, f64),
    size: f64,
    width: f64,
) -> Option<Marker> {
    let length = size + width;
    if length <= 0.0 || (direction.0 == 0.0 && direction.1 == 0.0) {
        return None;
    }
    let perpendicular = (-direction.1, direction.0);
    let along = |distance: f64| {
        (
            tip.0 - direction.0 * distance,
            tip.1 - direction.1 * distance,
        )
    };
    let offset = |point: (f64, f64), amount: f64| {
        (
            point.0 + perpendicular.0 * amount,
            point.1 + perpendicular.1 * amount,
        )
    };
    let filled_default = !matches!(kind, "open" | "openThin" | "openAsync" | "halfCircle");
    let thin = kind.ends_with("Thin");
    let segment = |from: (f64, f64), to: (f64, f64)| {
        format!("M {} {} L {} {}", n(from.0), n(from.1), n(to.0), n(to.1))
    };
    match kind {
        "classic" | "classicThin" => {
            let back = along(length);
            let half = length / if thin { 3.0 } else { 2.0 };
            Some(Marker::head(
                polygon_path(&[
                    tip,
                    offset(back, half),
                    along(length * 0.75),
                    offset(back, -half),
                ]),
                filled_default,
                length * 0.75,
            ))
        }
        "block" | "blockThin" | "async" => {
            let back = along(length);
            let half = length / if thin { 3.0 } else { 2.0 };
            Some(Marker::head(
                polygon_path(&[tip, offset(back, half), offset(back, -half)]),
                filled_default,
                length,
            ))
        }
        "open" | "openThin" | "openAsync" => {
            let back = along(length);
            let half = length / if thin { 3.0 } else { 2.0 };
            Some(Marker::line(
                format!(
                    "M {} {} L {} {} L {} {}",
                    n(offset(back, half).0),
                    n(offset(back, half).1),
                    n(tip.0),
                    n(tip.1),
                    n(offset(back, -half).0),
                    n(offset(back, -half).1)
                ),
                0.0,
            ))
        }
        "oval" | "circle" | "circlePlus" => {
            let radius = length / 2.0;
            let center = along(radius);
            let circle = ellipse_path(Rect {
                x: center.0 - radius,
                y: center.1 - radius,
                width: radius * 2.0,
                height: radius * 2.0,
            });
            let mut head = if kind == "oval" {
                Marker::head(circle, filled_default, radius * 2.0)
            } else {
                Marker {
                    parts: vec![(circle, MarkerPaint::Hollow)],
                    trim: radius * 2.0,
                }
            };
            if kind == "circlePlus" {
                head.parts.push((
                    format!(
                        "{} {}",
                        segment(
                            (
                                center.0 - direction.0 * radius,
                                center.1 - direction.1 * radius
                            ),
                            (
                                center.0 + direction.0 * radius,
                                center.1 + direction.1 * radius
                            ),
                        ),
                        segment(offset(center, radius), offset(center, -radius)),
                    ),
                    MarkerPaint::Line,
                ));
            }
            Some(head)
        }
        "diamond" | "diamondThin" => {
            let long = length * 1.118;
            let half = long / if thin { 4.0 } else { 2.0 };
            let middle = along(long / 2.0);
            Some(Marker::head(
                polygon_path(&[
                    tip,
                    offset(middle, half),
                    along(long),
                    offset(middle, -half),
                ]),
                filled_default,
                long,
            ))
        }
        "box" => {
            let back = along(length);
            let half = length / 2.0;
            Some(Marker::head(
                polygon_path(&[
                    offset(tip, half),
                    offset(tip, -half),
                    offset(back, -half),
                    offset(back, half),
                ]),
                filled_default,
                length,
            ))
        }
        "dash" => Some(Marker::line(
            segment(
                offset(tip, length / 2.0),
                offset(along(length), -length / 2.0),
            ),
            0.0,
        )),
        "cross" => Some(Marker::line(
            format!(
                "{} {}",
                segment(
                    offset(tip, length / 2.0),
                    offset(along(length), -length / 2.0)
                ),
                segment(
                    offset(tip, -length / 2.0),
                    offset(along(length), length / 2.0)
                ),
            ),
            0.0,
        )),
        "halfCircle" => {
            let radius = length / 2.0;
            let back = along(length);
            Some(Marker::line(
                format!(
                    "M {} {} A {} {} 0 0 1 {} {}",
                    n(offset(back, radius).0),
                    n(offset(back, radius).1),
                    n(radius),
                    n(radius),
                    n(offset(back, -radius).0),
                    n(offset(back, -radius).1)
                ),
                0.0,
            ))
        }
        // Entity-relationship ends: a tick for one, a crow's foot for many and
        // a ring for zero, at the distances draw.io draws them.
        "ERone" | "ERmandOne" | "ERmany" | "ERoneToMany" | "ERzeroToOne" | "ERzeroToMany" => Some(
            entity_relationship_marker(kind, tip, direction, size, width),
        ),
        _ => None,
    }
}

/// The crow's-foot family. Every distance is a multiple of
/// `size + strokeWidth + 1` along the line, as `mxMarker` measures them.
pub(super) fn entity_relationship_marker(
    kind: &str,
    tip: (f64, f64),
    direction: (f64, f64),
    size: f64,
    width: f64,
) -> Marker {
    let length = size + width + 1.0;
    let perpendicular = (-direction.1, direction.0);
    let at = |back: f64, side: f64| {
        (
            tip.0 - direction.0 * back + perpendicular.0 * side,
            tip.1 - direction.1 * back + perpendicular.1 * side,
        )
    };
    let half = length / 2.0;
    let tick = |back: f64| {
        let (from, to) = (at(back, half), at(back, -half));
        format!("M {} {} L {} {}", n(from.0), n(from.1), n(to.0), n(to.1))
    };
    let foot = {
        let (start, apex, end) = (at(0.0, -half), at(length, 0.0), at(0.0, half));
        format!(
            "M {} {} L {} {} L {} {}",
            n(start.0),
            n(start.1),
            n(apex.0),
            n(apex.1),
            n(end.0),
            n(end.1)
        )
    };
    let mut parts = Vec::new();
    let mut trim = 0.0;
    if kind.starts_with("ERzeroTo") {
        // The ring sits a length and a half back, and the line stops at it.
        let radius = size / 2.0;
        let center = at(1.5 * length, 0.0);
        parts.push((
            ellipse_path(Rect {
                x: center.0 - radius,
                y: center.1 - radius,
                width: radius * 2.0,
                height: radius * 2.0,
            }),
            MarkerPaint::Hollow,
        ));
        trim = 1.5 * length + radius;
    }
    let glyph = match kind {
        "ERone" | "ERzeroToOne" => tick(half),
        "ERmandOne" => format!("{} {}", tick(half), tick(length)),
        "ERoneToMany" => format!("{} {}", tick(length), foot),
        _ => foot,
    };
    parts.push((glyph, MarkerPaint::Line));
    Marker { parts, trim }
}

pub(super) fn draw_edge(
    cell: &Cell,
    route: &[(f64, f64)],
    nodes: &mut Vec<Node>,
    warnings: &mut Vec<String>,
    bounds: &mut Option<Rect>,
    clips: &mut Vec<ClipPath>,
) {
    let style = &cell.style;
    let look = appearance(style, route_bounds(route), "none", "#000000");
    let stroke_width = style.number("strokewidth", 1.0).max(0.0);
    let color = style
        .color("strokecolor")
        .unwrap_or_else(|| "#000000".to_owned());
    let end_kind = style.text("endarrow", "classic");
    let start_kind = style.text("startarrow", "none");
    let end_size = style.number("endsize", DEFAULT_MARKER_SIZE).max(0.0);
    let start_size = style.number("startsize", DEFAULT_MARKER_SIZE).max(0.0);
    for kind in [&end_kind, &start_kind] {
        if !matches!(
            kind.as_str(),
            "none"
                | ""
                | "classic"
                | "classicThin"
                | "block"
                | "blockThin"
                | "async"
                | "open"
                | "openThin"
                | "openAsync"
                | "oval"
                | "circle"
                | "circlePlus"
                | "diamond"
                | "diamondThin"
                | "box"
                | "dash"
                | "cross"
                | "halfCircle"
                | "ERone"
                | "ERmandOne"
                | "ERmany"
                | "ERoneToMany"
                | "ERzeroToOne"
                | "ERzeroToMany"
        ) {
            let warning =
                format!("drawio arrowhead '{kind}' is not drawn; the connector keeps its line");
            if !warnings.contains(&warning) {
                warnings.push(warning);
            }
        }
    }
    let last = route.len() - 1;
    let end_marker = marker(
        &end_kind,
        route[last],
        unit(route[last - 1], route[last]),
        end_size,
        stroke_width,
    );
    let start_marker = marker(
        &start_kind,
        route[0],
        unit(route[1], route[0]),
        start_size,
        stroke_width,
    );
    let mut line = route.to_vec();
    if let Some(head) = &end_marker {
        trim_end(&mut line, head.trim, true);
    }
    if let Some(head) = &start_marker {
        trim_end(&mut line, head.trim, false);
    }
    let meta = SourceMeta {
        kind: "drawio-edge".into(),
        source_id: cell.id.clone(),
        ..SourceMeta::default()
    };
    let path = edge_path(&line, style.flag("rounded"), style.flag("curved"));
    if look.shadow && !path.is_empty() {
        nodes.push(shadow_node(
            &format!("drawio-{}-shadow", cell.id),
            path.clone(),
            &look,
            IDENTITY,
        ));
    }
    if !path.is_empty() {
        nodes.push(Node::Path {
            id: format!("drawio-{}", cell.id),
            d: path,
            fill_rule: "nonzero".into(),
            fill: Paint::None,
            stroke: Stroke {
                line_join: LineJoin::Round,
                line_cap: if look.stroke.dash_array.is_empty() {
                    LineCap::Round
                } else {
                    LineCap::Butt
                },
                ..look.stroke.clone()
            },
            transform: IDENTITY,
            clip_id: None,
            meta: meta.clone(),
        });
    }
    for (suffix, head) in [("end", end_marker), ("start", start_marker)] {
        let Some(head) = head else { continue };
        // `startFill=0` / `endFill=0` hollow out a head that is filled by
        // default; a head drawn as lines stays lines either way.
        let hollowed = matches!(
            style.get(if suffix == "end" {
                "endfill"
            } else {
                "startfill"
            }),
            Some("0")
        );
        for (index, (d, paint)) in head.parts.into_iter().enumerate() {
            let paint = match paint {
                MarkerPaint::Solid if hollowed => MarkerPaint::Line,
                other => other,
            };
            nodes.push(Node::Path {
                id: format!("drawio-{}-{suffix}-{index}", cell.id),
                d,
                fill_rule: "nonzero".into(),
                fill: match paint {
                    MarkerPaint::Solid => Paint::solid(color.clone()),
                    MarkerPaint::Hollow => Paint::solid("#FFFFFF"),
                    MarkerPaint::Line => Paint::None,
                },
                stroke: Stroke {
                    dash_array: Vec::new(),
                    line_join: LineJoin::Miter,
                    ..look.stroke.clone()
                },
                transform: IDENTITY,
                clip_id: None,
                meta: SourceMeta {
                    kind: "drawio-edge-marker".into(),
                    ..meta.clone()
                },
            });
        }
    }
    draw_edge_label(cell, route, nodes, bounds, clips);
}

pub(super) fn route_bounds(route: &[(f64, f64)]) -> Rect {
    let mut bounds = None;
    for &(x, y) in route {
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
    bounds.unwrap_or_default()
}

/// Pull one end of the line back by `amount` so a filled arrowhead covers the
/// stroke instead of sitting on top of it.
pub(super) fn trim_end(points: &mut Vec<(f64, f64)>, amount: f64, from_end: bool) {
    if amount <= 0.0 || points.len() < 2 {
        return;
    }
    let mut remaining = amount;
    loop {
        let count = points.len();
        if count < 2 {
            return;
        }
        let (tip_index, neighbour_index) = if from_end {
            (count - 1, count - 2)
        } else {
            (0, 1)
        };
        let tip = points[tip_index];
        let neighbour = points[neighbour_index];
        let length = distance(tip, neighbour);
        if length > remaining {
            let direction = unit(tip, neighbour);
            points[tip_index] = (
                tip.0 + direction.0 * remaining,
                tip.1 + direction.1 * remaining,
            );
            return;
        }
        // The whole segment is inside the arrowhead; drop it and keep going.
        if count == 2 {
            let direction = unit(tip, neighbour);
            points[tip_index] = (tip.0 + direction.0 * length, tip.1 + direction.1 * length);
            return;
        }
        remaining -= length;
        points.remove(tip_index);
    }
}

pub(super) fn draw_edge_label(
    cell: &Cell,
    route: &[(f64, f64)],
    nodes: &mut Vec<Node>,
    bounds: &mut Option<Rect>,
    clips: &mut Vec<ClipPath>,
) {
    if cell.label.trim().is_empty() {
        return;
    }
    // mxGraph places an edge label at a position in [-1, 1] along the route,
    // with 0 at the middle. The geometry's `y` is a perpendicular offset in
    // pixels from that point, and `offset` is an absolute one on top of it.
    let position = if cell.geometry.relative {
        ((cell.geometry.x + 1.0) / 2.0).clamp(0.0, 1.0)
    } else {
        0.5
    };
    let ((x, y), (ux, uy)) = point_along(route, position);
    let perpendicular = if cell.geometry.relative {
        cell.geometry.y
    } else {
        0.0
    };
    let offset = cell.geometry.offset.unwrap_or((0.0, 0.0));
    let anchor = Rect {
        x: x + uy * perpendicular + offset.0,
        y: y - ux * perpendicular + offset.1,
        width: 0.0,
        height: 0.0,
    };
    draw_label(cell, anchor, 0.0, nodes, bounds, clips, true);
}

/// The point a fraction of the way along the route, and the unit direction of
/// the segment it lands on.
pub(super) fn point_along(route: &[(f64, f64)], position: f64) -> ((f64, f64), (f64, f64)) {
    let fallback = route.first().copied().unwrap_or((0.0, 0.0));
    let total = route
        .windows(2)
        .map(|pair| distance(pair[0], pair[1]))
        .sum::<f64>();
    if total <= f64::EPSILON {
        return (fallback, (1.0, 0.0));
    }
    let mut travelled = position * total;
    for pair in route.windows(2) {
        let length = distance(pair[0], pair[1]);
        if travelled <= length {
            let ratio = if length <= f64::EPSILON {
                0.0
            } else {
                travelled / length
            };
            return (
                (
                    pair[0].0 + (pair[1].0 - pair[0].0) * ratio,
                    pair[0].1 + (pair[1].1 - pair[0].1) * ratio,
                ),
                unit(pair[0], pair[1]),
            );
        }
        travelled -= length;
    }
    let last = route.len() - 1;
    (route[last], unit(route[last - 1], route[last]))
}
