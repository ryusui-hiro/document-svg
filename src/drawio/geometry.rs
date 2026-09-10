//! The page's own units: rectangles, the path strings a shape is drawn from,
//! and the transforms that turn, flip and place it.

use super::*;

/// Every coordinate and length read from a model is held inside this many
/// pixels of the origin.
///
/// A drawing is measured in screen pixels, so a million of them is already
/// hundreds of metres of paper: past that the file is broken rather than large,
/// and letting the value through would put a 300-digit number in the SVG's own
/// width and leave nothing a renderer could use.
pub(super) const MAX_COORDINATE: f64 = 1_000_000.0;

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(super) struct Rect {
    pub(super) x: f64,
    pub(super) y: f64,
    pub(super) width: f64,
    pub(super) height: f64,
}

impl Rect {
    pub(super) fn center(self) -> (f64, f64) {
        (self.x + self.width / 2.0, self.y + self.height / 2.0)
    }

    pub(super) fn right(self) -> f64 {
        self.x + self.width
    }

    pub(super) fn bottom(self) -> f64 {
        self.y + self.height
    }

    pub(super) fn grow(self, amount: f64) -> Self {
        Self {
            x: self.x - amount,
            y: self.y - amount,
            width: self.width + amount * 2.0,
            height: self.height + amount * 2.0,
        }
    }
}

/// Hold a value read from the model inside the coordinate range, so later
/// arithmetic cannot reach infinity and the SVG cannot inherit a number no
/// renderer can use.
pub(super) fn bounded(value: f64) -> f64 {
    if value.is_finite() {
        value.clamp(-MAX_COORDINATE, MAX_COORDINATE)
    } else {
        0.0
    }
}

pub(super) fn n(value: f64) -> String {
    if !value.is_finite() {
        return "0".into();
    }
    let rounded = (value * 1000.0).round() / 1000.0;
    let text = format!("{rounded}");
    if text == "-0" { "0".into() } else { text }
}

pub(super) fn rectangle_path(rect: Rect) -> String {
    format!(
        "M {} {} H {} V {} H {} Z",
        n(rect.x),
        n(rect.y),
        n(rect.right()),
        n(rect.bottom()),
        n(rect.x)
    )
}

pub(super) fn rounded_rect_path(rect: Rect, radius: f64) -> String {
    oval_rect_path(rect, radius, radius)
}

/// A rectangle whose corners are quarters of an ellipse rather than of a
/// circle, which is the form `mxAbstractCanvas2D.roundrect` takes.
pub(super) fn oval_rect_path(rect: Rect, rx: f64, ry: f64) -> String {
    let rx = rx.min(rect.width / 2.0).max(0.0);
    let ry = ry.min(rect.height / 2.0).max(0.0);
    if rx <= 0.0 || ry <= 0.0 {
        return rectangle_path(rect);
    }
    format!(
        "M {} {} H {} A {rx} {ry} 0 0 1 {} {} V {} A {rx} {ry} 0 0 1 {} {} H {} A {rx} {ry} 0 0 1 {} {} V {} A {rx} {ry} 0 0 1 {} {} Z",
        n(rect.x + rx),
        n(rect.y),
        n(rect.right() - rx),
        n(rect.right()),
        n(rect.y + ry),
        n(rect.bottom() - ry),
        n(rect.right() - rx),
        n(rect.bottom()),
        n(rect.x + rx),
        n(rect.x),
        n(rect.bottom() - ry),
        n(rect.y + ry),
        n(rect.x + rx),
        n(rect.y),
        rx = n(rx),
        ry = n(ry),
    )
}

pub(super) fn ellipse_path(rect: Rect) -> String {
    let (cx, cy) = rect.center();
    let (rx, ry) = (rect.width / 2.0, rect.height / 2.0);
    format!(
        "M {} {} A {} {} 0 1 0 {} {} A {} {} 0 1 0 {} {} Z",
        n(cx - rx),
        n(cy),
        n(rx),
        n(ry),
        n(cx + rx),
        n(cy),
        n(rx),
        n(ry),
        n(cx - rx),
        n(cy)
    )
}

pub(super) fn polygon_path(points: &[(f64, f64)]) -> String {
    let mut path = String::new();
    for (index, (x, y)) in points.iter().enumerate() {
        path.push_str(if index == 0 { "M " } else { " L " });
        path.push_str(&n(*x));
        path.push(' ');
        path.push_str(&n(*y));
    }
    path.push_str(" Z");
    path
}

pub(super) fn polyline_path(points: &[(f64, f64)]) -> String {
    let mut path = String::new();
    for (index, (x, y)) in points.iter().enumerate() {
        path.push_str(if index == 0 { "M " } else { " L " });
        path.push_str(&n(*x));
        path.push(' ');
        path.push_str(&n(*y));
    }
    path
}

pub(super) fn line_path(from: (f64, f64), to: (f64, f64)) -> String {
    format!("M {} {} L {} {}", n(from.0), n(from.1), n(to.0), n(to.1))
}

/// A shape's drawn geometry: one outline that takes the fill, plus strokes that
/// sit on top of it, such as the inner bars of a flowchart process.
pub(super) struct Paths {
    pub(super) outline: String,
    pub(super) decorations: Vec<String>,
    /// Whether the outline takes the stroke as well as the fill. A shape that
    /// draws only some of its sides fills the whole outline and strokes those
    /// sides as decorations instead.
    pub(super) stroke_outline: bool,
}

impl Paths {
    pub(super) fn new(outline: String) -> Self {
        Self {
            outline,
            decorations: Vec::new(),
            stroke_outline: true,
        }
    }

    pub(super) fn with(outline: String, decorations: Vec<String>) -> Self {
        Self {
            outline,
            decorations,
            stroke_outline: true,
        }
    }

    pub(super) fn fill_only(outline: String, decorations: Vec<String>) -> Self {
        Self {
            outline,
            decorations,
            stroke_outline: false,
        }
    }
}

/// The corner radius mxGraph would use for a rounded rectangle.
pub(super) fn corner_radius(rect: Rect, style: &Style) -> f64 {
    if style.flag("absolutearcsize") {
        style.number("arcsize", 30.0) / 2.0
    } else {
        // `mxConstants.RECTANGLE_ROUNDING_FACTOR` is 0.15 and `arcSize` is the
        // same factor written as a percentage.
        rect.width.min(rect.height) * (style.number("arcsize", 15.0) / 100.0)
    }
}

/// mxGraph draws a `direction` other than east by painting the shape in an
/// upright box and rotating the result about the centre; north and south also
/// swap the box's width and height.
pub(super) fn directed_rect(rect: Rect, direction: &str) -> (Rect, f64) {
    let (cx, cy) = rect.center();
    let swapped = Rect {
        x: cx - rect.height / 2.0,
        y: cy - rect.width / 2.0,
        width: rect.height,
        height: rect.width,
    };
    match direction {
        "north" => (swapped, -90.0),
        "south" => (swapped, 90.0),
        "west" => (rect, 180.0),
        _ => (rect, 0.0),
    }
}

pub(super) fn shape_transform(rect: Rect, degrees: f64, flip_h: bool, flip_v: bool) -> Matrix {
    if degrees == 0.0 && !flip_h && !flip_v {
        return IDENTITY;
    }
    let (cx, cy) = rect.center();
    let mut matrix: Matrix = [1.0, 0.0, 0.0, 1.0, -cx, -cy];
    if flip_h || flip_v {
        let scale: Matrix = [
            if flip_h { -1.0 } else { 1.0 },
            0.0,
            0.0,
            if flip_v { -1.0 } else { 1.0 },
            0.0,
            0.0,
        ];
        matrix = compose(scale, matrix);
    }
    if degrees != 0.0 {
        let radians = degrees * PI / 180.0;
        let (sin, cos) = radians.sin_cos();
        matrix = compose([cos, sin, -sin, cos, 0.0, 0.0], matrix);
    }
    compose([1.0, 0.0, 0.0, 1.0, cx, cy], matrix)
}

/// One step of a path draw.io's own code draws segment by segment.
///
/// The shapes the editor implements in JavaScript are long runs of `moveTo`,
/// `lineTo` and curves over coordinates derived from the shape's own box.
/// Keeping them as steps rather than as formatted strings means the geometry
/// reads the way the editor writes it.
pub(super) enum Step {
    Move(f64, f64),
    Line(f64, f64),
    Curve(f64, f64, f64, f64, f64, f64),
    Quad(f64, f64, f64, f64),
    /// Radii, the large-arc and sweep flags, then the point to end at.
    Arc(f64, f64, u8, u8, f64, f64),
    Close,
}

pub(super) fn path_of(steps: &[Step]) -> String {
    let mut path = String::new();
    for step in steps {
        if !path.is_empty() {
            path.push(' ');
        }
        match step {
            Step::Move(x, y) => path.push_str(&format!("M {} {}", n(*x), n(*y))),
            Step::Line(x, y) => path.push_str(&format!("L {} {}", n(*x), n(*y))),
            Step::Curve(x1, y1, x2, y2, x, y) => path.push_str(&format!(
                "C {} {} {} {} {} {}",
                n(*x1),
                n(*y1),
                n(*x2),
                n(*y2),
                n(*x),
                n(*y)
            )),
            Step::Quad(x1, y1, x, y) => {
                path.push_str(&format!("Q {} {} {} {}", n(*x1), n(*y1), n(*x), n(*y)));
            }
            Step::Arc(rx, ry, large, sweep, x, y) => path.push_str(&format!(
                "A {} {} 0 {large} {sweep} {} {}",
                n(*rx),
                n(*ry),
                n(*x),
                n(*y)
            )),
            Step::Close => path.push('Z'),
        }
    }
    path
}
