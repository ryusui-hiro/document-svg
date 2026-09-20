//! 2D point, affine transformation matrix, and parametric curve samplers for SVG geometry.

/// 2D point representation for vector geometry extraction and transformation.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Point2D {
    pub x: f64,
    pub y: f64,
}

impl Point2D {
    #[inline]
    pub const fn new(x: f64, y: f64) -> Self {
        Self { x, y }
    }

    #[inline]
    pub fn distance_to(&self, other: &Point2D) -> f64 {
        ((self.x - other.x).powi(2) + (self.y - other.y).powi(2)).sqrt()
    }
}

/// 2D affine transformation matrix:
/// ```text
/// [ x' ]   [ a  c  e ] [ x ]
/// [ y' ] = [ b  d  f ] [ y ]
/// [ 1  ]   [ 0  0  1 ] [ 1 ]
/// ```
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Transform2D {
    pub a: f64,
    pub b: f64,
    pub c: f64,
    pub d: f64,
    pub e: f64,
    pub f: f64,
}

impl Default for Transform2D {
    fn default() -> Self {
        Self::identity()
    }
}

impl Transform2D {
    pub const fn identity() -> Self {
        Self {
            a: 1.0,
            b: 0.0,
            c: 0.0,
            d: 1.0,
            e: 0.0,
            f: 0.0,
        }
    }

    #[inline]
    pub fn is_identity(&self) -> bool {
        (self.a - 1.0).abs() < 1e-9
            && self.b.abs() < 1e-9
            && self.c.abs() < 1e-9
            && (self.d - 1.0).abs() < 1e-9
            && self.e.abs() < 1e-9
            && self.f.abs() < 1e-9
    }

    #[inline]
    pub fn apply(&self, p: Point2D) -> Point2D {
        Point2D::new(
            self.a * p.x + self.c * p.y + self.e,
            self.b * p.x + self.d * p.y + self.f,
        )
    }

    pub fn multiply(&self, rhs: &Self) -> Self {
        Self {
            a: self.a * rhs.a + self.c * rhs.b,
            b: self.b * rhs.a + self.d * rhs.b,
            c: self.a * rhs.c + self.c * rhs.d,
            d: self.b * rhs.c + self.d * rhs.d,
            e: self.a * rhs.e + self.c * rhs.f + self.e,
            f: self.b * rhs.e + self.d * rhs.f + self.f,
        }
    }

    pub fn translate(tx: f64, ty: f64) -> Self {
        Self {
            a: 1.0,
            b: 0.0,
            c: 0.0,
            d: 1.0,
            e: tx,
            f: ty,
        }
    }

    pub fn scale(sx: f64, sy: f64) -> Self {
        Self {
            a: sx,
            b: 0.0,
            c: 0.0,
            d: sy,
            e: 0.0,
            f: 0.0,
        }
    }

    pub fn rotate(angle_rad: f64) -> Self {
        let cos = angle_rad.cos();
        let sin = angle_rad.sin();
        Self {
            a: cos,
            b: sin,
            c: -sin,
            d: cos,
            e: 0.0,
            f: 0.0,
        }
    }
}

/// Parses an SVG `transform` attribute value into a `Transform2D` with zero heap allocation.
pub fn parse_transform(s: &str) -> Transform2D {
    let mut tf = Transform2D::identity();
    let s = s.trim();
    if s.is_empty() {
        return tf;
    }

    let mut remaining = s;
    while let Some(open_paren) = remaining.find('(') {
        let op_name = remaining[..open_paren].trim();
        let close_paren = match remaining[open_paren + 1..].find(')') {
            Some(idx) => open_paren + 1 + idx,
            None => break,
        };
        let args_str = &remaining[open_paren + 1..close_paren];

        // Stack-allocated parsing buffer to avoid heap allocations
        let mut args = [0.0f64; 6];
        let mut argc = 0;
        for part in args_str.split([' ', ',']) {
            let part = part.trim();
            if !part.is_empty()
                && let Ok(v) = part.parse::<f64>()
                && argc < 6
            {
                args[argc] = v;
                argc += 1;
            }
        }

        let op = op_name.split_whitespace().last().unwrap_or(op_name);

        let cur = match op {
            "matrix" if argc >= 6 => Transform2D {
                a: args[0],
                b: args[1],
                c: args[2],
                d: args[3],
                e: args[4],
                f: args[5],
            },
            "translate" if argc > 0 => {
                let tx = args[0];
                let ty = if argc > 1 { args[1] } else { 0.0 };
                Transform2D::translate(tx, ty)
            }
            "scale" if argc > 0 => {
                let sx = args[0];
                let sy = if argc > 1 { args[1] } else { sx };
                Transform2D::scale(sx, sy)
            }
            "rotate" if argc > 0 => {
                let rad = args[0].to_radians();
                if argc >= 3 {
                    let cx = args[1];
                    let cy = args[2];
                    Transform2D::translate(cx, cy)
                        .multiply(&Transform2D::rotate(rad))
                        .multiply(&Transform2D::translate(-cx, -cy))
                } else {
                    Transform2D::rotate(rad)
                }
            }
            "skewx" | "skewX" if argc > 0 => {
                let tan = args[0].to_radians().tan();
                Transform2D {
                    a: 1.0,
                    b: 0.0,
                    c: tan,
                    d: 1.0,
                    e: 0.0,
                    f: 0.0,
                }
            }
            "skewy" | "skewY" if argc > 0 => {
                let tan = args[0].to_radians().tan();
                Transform2D {
                    a: 1.0,
                    b: tan,
                    c: 0.0,
                    d: 1.0,
                    e: 0.0,
                    f: 0.0,
                }
            }
            _ => Transform2D::identity(),
        };

        tf = tf.multiply(&cur);
        remaining = &remaining[close_paren + 1..];
    }

    tf
}

/// Samples a cubic bezier curve into line segments with `steps`.
#[inline]
pub fn sample_cubic_bezier<F>(
    p0: Point2D,
    p1: Point2D,
    p2: Point2D,
    p3: Point2D,
    steps: usize,
    mut on_point: F,
) where
    F: FnMut(Point2D),
{
    let n = steps.max(1);
    for step in 1..=n {
        let t = (step as f64) / (n as f64);
        let u = 1.0 - t;
        let x =
            u * u * u * p0.x + 3.0 * u * u * t * p1.x + 3.0 * u * t * t * p2.x + t * t * t * p3.x;
        let y =
            u * u * u * p0.y + 3.0 * u * u * t * p1.y + 3.0 * u * t * t * p2.y + t * t * t * p3.y;
        on_point(Point2D::new(x, y));
    }
}

/// Samples a quadratic bezier curve into line segments with `steps`.
#[inline]
pub fn sample_quad_bezier<F>(p0: Point2D, p1: Point2D, p2: Point2D, steps: usize, mut on_point: F)
where
    F: FnMut(Point2D),
{
    let n = steps.max(1);
    for step in 1..=n {
        let t = (step as f64) / (n as f64);
        let u = 1.0 - t;
        let x = u * u * p0.x + 2.0 * u * t * p1.x + t * t * p2.x;
        let y = u * u * p0.y + 2.0 * u * t * p1.y + t * t * p2.y;
        on_point(Point2D::new(x, y));
    }
}

/// Samples an elliptical arc curve into line segments per W3C SVG 1.1 Appendix F.6.
#[allow(clippy::too_many_arguments)]
pub fn sample_elliptical_arc<F>(
    p0: Point2D,
    p1: Point2D,
    mut rx: f64,
    mut ry: f64,
    x_axis_rotation_deg: f64,
    large_arc_flag: bool,
    sweep_flag: bool,
    steps: usize,
    mut on_point: F,
) where
    F: FnMut(Point2D),
{
    if (p0.x - p1.x).abs() < 1e-9 && (p0.y - p1.y).abs() < 1e-9 {
        return;
    }
    if rx.abs() < 1e-9 || ry.abs() < 1e-9 {
        on_point(p1);
        return;
    }

    rx = rx.abs();
    ry = ry.abs();
    let phi = x_axis_rotation_deg.to_radians();
    let cos_phi = phi.cos();
    let sin_phi = phi.sin();

    // Step 1: Compute (x1', y1')
    let dx = (p0.x - p1.x) / 2.0;
    let dy = (p0.y - p1.y) / 2.0;
    let x1_prime = cos_phi * dx + sin_phi * dy;
    let y1_prime = -sin_phi * dx + cos_phi * dy;

    // Check radii scaling
    let lambda = (x1_prime * x1_prime) / (rx * rx) + (y1_prime * y1_prime) / (ry * ry);
    if lambda > 1.0 {
        let scale = lambda.sqrt();
        rx *= scale;
        ry *= scale;
    }

    // Step 2: Compute (cx', cy')
    let sign = if large_arc_flag != sweep_flag {
        1.0
    } else {
        -1.0
    };
    let rx_sq = rx * rx;
    let ry_sq = ry * ry;
    let x1_sq = x1_prime * x1_prime;
    let y1_sq = y1_prime * y1_prime;

    let numerator = (rx_sq * ry_sq - rx_sq * y1_sq - ry_sq * x1_sq).max(0.0);
    let denominator = rx_sq * y1_sq + ry_sq * x1_sq;
    let sq = if denominator > 0.0 {
        (numerator / denominator).sqrt()
    } else {
        0.0
    };
    let cx_prime = sign * sq * (rx * y1_prime / ry);
    let cy_prime = sign * sq * -(ry * x1_prime / rx);

    // Step 3: Compute (cx, cy) from (cx', cy')
    let cx = cos_phi * cx_prime - sin_phi * cy_prime + (p0.x + p1.x) / 2.0;
    let cy = sin_phi * cx_prime + cos_phi * cy_prime + (p0.y + p1.y) / 2.0;

    // Step 4: Compute theta1 and delta_theta
    let ux = (x1_prime - cx_prime) / rx;
    let uy = (y1_prime - cy_prime) / ry;
    let vx = (-x1_prime - cx_prime) / rx;
    let vy = (-y1_prime - cy_prime) / ry;

    let vector_angle = |u_x: f64, u_y: f64, v_x: f64, v_y: f64| -> f64 {
        let dot = u_x * v_x + u_y * v_y;
        let len = (u_x * u_x + u_y * u_y).sqrt() * (v_x * v_x + v_y * v_y).sqrt();
        let val = if len > 0.0 {
            (dot / len).clamp(-1.0, 1.0)
        } else {
            0.0
        };
        let mut ang = val.acos();
        if u_x * v_y - u_y * v_x < 0.0 {
            ang = -ang;
        }
        ang
    };

    let theta1 = vector_angle(1.0, 0.0, ux, uy);
    let mut d_theta = vector_angle(ux, uy, vx, vy);

    let two_pi = std::f64::consts::PI * 2.0;
    if !sweep_flag && d_theta > 0.0 {
        d_theta -= two_pi;
    } else if sweep_flag && d_theta < 0.0 {
        d_theta += two_pi;
    }

    let n = steps.max(4);
    for i in 1..=n {
        let t = i as f64 / n as f64;
        let angle = theta1 + t * d_theta;
        let cos_a = angle.cos();
        let sin_a = angle.sin();
        let ex = rx * cos_a;
        let ey = ry * sin_a;
        let x = cos_phi * ex - sin_phi * ey + cx;
        let y = sin_phi * ex + cos_phi * ey + cy;
        on_point(Point2D::new(x, y));
    }
}
