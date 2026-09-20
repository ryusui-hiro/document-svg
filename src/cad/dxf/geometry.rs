//! 2D CAD geometry calculations and conversion to SVG path strings.

use std::f64::consts::PI;

/// A 2D bounding box representing `[min_x, min_y, max_x, max_y]`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BBox {
    pub min_x: f64,
    pub min_y: f64,
    pub max_x: f64,
    pub max_y: f64,
}

impl Default for BBox {
    fn default() -> Self {
        Self {
            min_x: f64::INFINITY,
            min_y: f64::INFINITY,
            max_x: f64::NEG_INFINITY,
            max_y: f64::NEG_INFINITY,
        }
    }
}

impl BBox {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn update(&mut self, x: f64, y: f64) {
        if x.is_finite() {
            if x < self.min_x {
                self.min_x = x;
            }
            if x > self.max_x {
                self.max_x = x;
            }
        }
        if y.is_finite() {
            if y < self.min_y {
                self.min_y = y;
            }
            if y > self.max_y {
                self.max_y = y;
            }
        }
    }

    #[must_use]
    pub fn is_valid(&self) -> bool {
        self.min_x.is_finite()
            && self.min_y.is_finite()
            && self.max_x.is_finite()
            && self.max_y.is_finite()
            && self.min_x <= self.max_x
            && self.min_y <= self.max_y
    }

    #[must_use]
    pub fn width(&self) -> f64 {
        if self.is_valid() {
            (self.max_x - self.min_x).max(0.0)
        } else {
            0.0
        }
    }

    #[must_use]
    pub fn height(&self) -> f64 {
        if self.is_valid() {
            (self.max_y - self.min_y).max(0.0)
        } else {
            0.0
        }
    }
}

/// Formats a coordinate float compactly with precision up to 5 decimal places.
#[must_use]
pub fn fmt_coord(v: f64) -> String {
    if !v.is_finite() {
        return "0".into();
    }
    let rounded = (v * 100_000.0).round() / 100_000.0;
    if rounded == 0.0 {
        "0".into()
    } else {
        let mut s = format!("{rounded:.5}");
        while s.ends_with('0') {
            s.pop();
        }
        if s.ends_with('.') {
            s.pop();
        }
        s
    }
}

/// Robust float parser for CAD files (STEP, IGES, DXF, G-code, Excellon, STL).
/// Handles omitted leading zeros (e.g. `.5`, `-.5`, `+.5`).
#[must_use]
pub fn parse_cad_float(s: &str) -> Option<f64> {
    let trimmed = s.trim();
    if trimmed.is_empty() {
        return None;
    }
    if let Ok(v) = trimmed.parse::<f64>() {
        return Some(v);
    }
    if trimmed.starts_with('.') {
        format!("0{trimmed}").parse().ok()
    } else if let Some(rest) = trimmed.strip_prefix("-.") {
        format!("-0.{rest}").parse().ok()
    } else if let Some(rest) = trimmed.strip_prefix("+.") {
        format!("+0.{rest}").parse().ok()
    } else {
        None
    }
}

/// Converts an angle in degrees to radians.
#[must_use]
pub fn deg_to_rad(deg: f64) -> f64 {
    deg * PI / 180.0
}

/// Generates an SVG path for a full circle at `(cx, cy)` with radius `r`.
#[must_use]
pub fn circle_path(cx: f64, cy: f64, r: f64) -> String {
    if !r.is_finite() || r <= 0.0 {
        return String::new();
    }
    let x1 = cx - r;
    let x2 = cx + r;
    format!(
        "M {} {} A {} {} 0 1 0 {} {} A {} {} 0 1 0 {} {} Z",
        fmt_coord(x1),
        fmt_coord(cy),
        fmt_coord(r),
        fmt_coord(r),
        fmt_coord(x2),
        fmt_coord(cy),
        fmt_coord(r),
        fmt_coord(r),
        fmt_coord(x1),
        fmt_coord(cy)
    )
}

/// Generates an SVG path arc from `(x1, y1)` to `(x2, y2)` with a given DXF bulge factor.
///
/// In DXF, `bulge = tan(theta / 4)`, where `theta` is the included angle.
/// A positive bulge means counter-clockwise (CCW), negative means clockwise (CW).
/// When mapped to SVG coordinates (where Y grows downwards), a CCW CAD arc becomes
/// a CW SVG arc (sweep-flag = 1) if no prior Y-flip was done on points, or sweep-flag = 0
/// if points `(x1, y1)` and `(x2, y2)` have already been flipped into SVG coordinates.
/// Here we assume `y_is_flipped` indicates whether the points are already in SVG display space.
#[must_use]
pub fn bulge_to_svg_arc(
    x1: f64,
    y1: f64,
    x2: f64,
    y2: f64,
    bulge: f64,
    y_is_flipped: bool,
) -> String {
    if !bulge.is_finite() || bulge.abs() < 1e-9 {
        return format!("L {} {}", fmt_coord(x2), fmt_coord(y2));
    }
    let dx = x2 - x1;
    let dy = y2 - y1;
    let chord_len = (dx * dx + dy * dy).sqrt();
    if chord_len < 1e-9 {
        return String::new();
    }
    // Radius formula: R = (chord / 2) * (1 + bulge^2) / (2 * |bulge|)
    let r = (chord_len * (1.0 + bulge * bulge)) / (4.0 * bulge.abs());
    let large_arc = if bulge.abs() > 1.0 { 1 } else { 0 };
    // In DXF (Y up): positive bulge = CCW.
    // In SVG (Y down): CCW corresponds to sweep_flag = 0.
    // If Y is already flipped to SVG space: CAD CCW (bulge > 0) becomes SVG CW (sweep_flag = 1).
    let sweep = if y_is_flipped {
        if bulge > 0.0 { 1 } else { 0 }
    } else if bulge > 0.0 {
        0
    } else {
        1
    };

    format!(
        "A {} {} 0 {} {} {} {}",
        fmt_coord(r),
        fmt_coord(r),
        large_arc,
        sweep,
        fmt_coord(x2),
        fmt_coord(y2)
    )
}

/// Generates an SVG path segment for an ARC entity given in CAD coordinates
/// with center `(cx, cy)`, radius `r`, start angle (deg), and end angle (deg).
///
/// If `y_is_flipped` is true, the center coordinates and angles are assumed
/// mapped to SVG coordinate space.
#[must_use]
pub fn arc_to_svg_path(
    cx: f64,
    cy: f64,
    r: f64,
    start_deg: f64,
    end_deg: f64,
    y_is_flipped: bool,
) -> String {
    if r <= 0.0 {
        return String::new();
    }
    let mut sweep_angle = end_deg - start_deg;
    while sweep_angle <= 0.0 {
        sweep_angle += 360.0;
    }
    while sweep_angle > 360.0 {
        sweep_angle -= 360.0;
    }

    let start_rad = deg_to_rad(start_deg);
    let end_rad = deg_to_rad(end_deg);

    let (start_x, start_y, end_x, end_y, sweep_flag) = if y_is_flipped {
        // Y is down: CAD angle theta has y = cy - r * sin(theta)
        let sx = cx + r * start_rad.cos();
        let sy = cy - r * start_rad.sin();
        let ex = cx + r * end_rad.cos();
        let ey = cy - r * end_rad.sin();
        // In SVG coordinates, a CCW CAD arc moves in the direction of increasing screen angle (CW)
        (sx, sy, ex, ey, 0)
    } else {
        let sx = cx + r * start_rad.cos();
        let sy = cy + r * start_rad.sin();
        let ex = cx + r * end_rad.cos();
        let ey = cy + r * end_rad.sin();
        (sx, sy, ex, ey, 1)
    };

    let large_arc = if sweep_angle > 180.0 { 1 } else { 0 };

    format!(
        "M {} {} A {} {} 0 {} {} {} {}",
        fmt_coord(start_x),
        fmt_coord(start_y),
        fmt_coord(r),
        fmt_coord(r),
        large_arc,
        sweep_flag,
        fmt_coord(end_x),
        fmt_coord(end_y)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_bbox_calculation() {
        let mut bbox = BBox::new();
        assert!(!bbox.is_valid());
        bbox.update(10.0, 20.0);
        bbox.update(50.0, -10.0);
        assert!(bbox.is_valid());
        assert_eq!(bbox.min_x, 10.0);
        assert_eq!(bbox.max_x, 50.0);
        assert_eq!(bbox.min_y, -10.0);
        assert_eq!(bbox.max_y, 20.0);
        assert_eq!(bbox.width(), 40.0);
        assert_eq!(bbox.height(), 30.0);
    }

    #[test]
    fn test_circle_path() {
        let path = circle_path(100.0, 100.0, 50.0);
        assert!(path.starts_with("M 50 100 A 50 50 0 1 0 150 100"));
        assert!(path.ends_with("Z"));
    }

    #[test]
    fn test_bulge_to_arc() {
        // Bulge = 1.0 is a semicircle (180 deg)
        let arc_cmd = bulge_to_svg_arc(0.0, 0.0, 100.0, 0.0, 1.0, true);
        assert!(arc_cmd.starts_with("A 50 50 0 0 1 100 0"));

        // Bulge = 0.0 is a straight line
        let line_cmd = bulge_to_svg_arc(0.0, 0.0, 100.0, 0.0, 0.0, true);
        assert_eq!(line_cmd, "L 100 0");
    }
}
