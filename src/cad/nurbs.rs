//! Bounded, dependency-free NURBS/B-spline curve evaluator shared by the IGES
//! (entity 126, Rational B-Spline Curve) and STEP (`B_SPLINE_CURVE_WITH_KNOTS`)
//! readers.
//!
//! Implements the standard Cox-de Boor basis-function algorithm for clamped or
//! unclamped, uniform or non-uniform rational B-spline curves in 3D. Malformed
//! or degenerate inputs (mismatched knot/control-point counts, non-monotonic
//! knots, non-positive weights) make [`NurbsCurve::is_valid`] return `false`
//! rather than panicking; callers must check it before evaluating.

/// Upper bound on control points accepted from an untrusted CAD file, so a
/// single crafted curve entity cannot force an unbounded allocation.
const MAX_NURBS_CONTROL_POINTS: usize = 20_000;
/// Upper bound on how many points a single curve is tessellated into.
const MAX_NURBS_SAMPLES: usize = 4_096;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Point3 {
    pub x: f64,
    pub y: f64,
    pub z: f64,
}

/// A rational (or, with all-1 weights, polynomial) B-spline curve in 3D.
///
/// `knots.len()` must equal `control_points.len() + degree + 1` for the
/// curve to be valid; see [`NurbsCurve::is_valid`].
#[derive(Clone, Debug)]
pub struct NurbsCurve {
    pub degree: usize,
    pub control_points: Vec<Point3>,
    pub weights: Vec<f64>,
    pub knots: Vec<f64>,
}

impl NurbsCurve {
    pub fn is_valid(&self) -> bool {
        let n = self.control_points.len();
        (2..=MAX_NURBS_CONTROL_POINTS).contains(&n)
            && self.degree >= 1
            && self.degree < n
            && self.weights.len() == n
            && self.knots.len() == n + self.degree + 1
            && self.weights.iter().all(|w| w.is_finite() && *w > 0.0)
            && self.knots.iter().all(|k| k.is_finite())
            && self.knots.windows(2).all(|w| w[1] >= w[0])
            && self
                .control_points
                .iter()
                .all(|p| p.x.is_finite() && p.y.is_finite() && p.z.is_finite())
    }

    /// Evaluates the curve at parameter `t`, clamped to the valid knot domain.
    /// Returns `None` when the curve fails [`Self::is_valid`] or the domain
    /// is degenerate.
    pub fn evaluate(&self, t: f64) -> Option<Point3> {
        if !self.is_valid() {
            return None;
        }
        let n = self.control_points.len() - 1;
        let p = self.degree;
        let lo = self.knots[p];
        let hi = self.knots[n + 1];
        if hi <= lo {
            return None;
        }
        let t = t.clamp(lo, hi);
        let span = find_span(n, p, t, &self.knots);
        let basis = basis_funs(span, t, p, &self.knots);

        let mut num = Point3 {
            x: 0.0,
            y: 0.0,
            z: 0.0,
        };
        let mut den = 0.0;
        for (i, b) in basis.iter().enumerate() {
            let idx = span - p + i;
            let w = self.weights[idx] * b;
            let cp = self.control_points[idx];
            num.x += w * cp.x;
            num.y += w * cp.y;
            num.z += w * cp.z;
            den += w;
        }
        if den.abs() < 1e-12 {
            return None;
        }
        Some(Point3 {
            x: num.x / den,
            y: num.y / den,
            z: num.z / den,
        })
    }

    /// Samples the full knot domain into a bounded polyline (`steps + 1`
    /// points, `steps` clamped to `[1, MAX_NURBS_SAMPLES]`).
    pub fn tessellate(&self, steps: usize) -> Vec<Point3> {
        if !self.is_valid() {
            return Vec::new();
        }
        let n = self.control_points.len() - 1;
        let p = self.degree;
        self.tessellate_domain(self.knots[p], self.knots[n + 1], steps)
    }

    /// Samples an explicit `[t0, t1]` sub-domain (clamped to the curve's own
    /// knot domain by [`Self::evaluate`]) into a bounded polyline.
    pub fn tessellate_domain(&self, t0: f64, t1: f64, steps: usize) -> Vec<Point3> {
        if !self.is_valid() || !t0.is_finite() || !t1.is_finite() {
            return Vec::new();
        }
        let steps = steps.clamp(1, MAX_NURBS_SAMPLES);
        let (lo, hi) = if t0 <= t1 { (t0, t1) } else { (t1, t0) };
        (0..=steps)
            .filter_map(|i| self.evaluate(lo + (hi - lo) * (i as f64 / steps as f64)))
            .collect()
    }
}

/// Expands a STEP-style (multiplicity, value) knot pair list into a flat,
/// non-decreasing knot vector. Returns `None` for mismatched lengths,
/// non-finite values, a zero/absurd multiplicity, or a total knot count that
/// would exceed a bounded limit.
pub fn expand_knot_multiplicities(multiplicities: &[u64], values: &[f64]) -> Option<Vec<f64>> {
    const MAX_MULTIPLICITY: u64 = 10_000;
    if multiplicities.is_empty() || multiplicities.len() != values.len() {
        return None;
    }
    let mut knots = Vec::new();
    for (&mult, &val) in multiplicities.iter().zip(values.iter()) {
        if !val.is_finite() || mult == 0 || mult > MAX_MULTIPLICITY {
            return None;
        }
        if knots.len() + mult as usize > MAX_NURBS_CONTROL_POINTS * 2 {
            return None;
        }
        for _ in 0..mult {
            knots.push(val);
        }
    }
    Some(knots)
}

/// Binary search (Piegl & Tiller, "The NURBS Book", algorithm A2.1) for the
/// knot span index containing parameter `t`.
fn find_span(n: usize, p: usize, t: f64, knots: &[f64]) -> usize {
    if t >= knots[n + 1] {
        return n;
    }
    let mut low = p;
    let mut high = n + 1;
    let mut mid = low.midpoint(high);
    while t < knots[mid] || t >= knots[mid + 1] {
        if t < knots[mid] {
            high = mid;
        } else {
            low = mid;
        }
        mid = low.midpoint(high);
    }
    mid
}

/// Cox-de Boor basis functions (algorithm A2.2) nonzero over `span`.
fn basis_funs(span: usize, t: f64, p: usize, knots: &[f64]) -> Vec<f64> {
    let mut left = vec![0.0f64; p + 1];
    let mut right = vec![0.0f64; p + 1];
    let mut n = vec![1.0f64; p + 1];
    for j in 1..=p {
        left[j] = t - knots[span + 1 - j];
        right[j] = knots[span + j] - t;
        let mut saved = 0.0;
        for r in 0..j {
            let denom = right[r + 1] + left[j - r];
            let temp = if denom.abs() > 1e-12 {
                n[r] / denom
            } else {
                0.0
            };
            n[r] = saved + right[r + 1] * temp;
            saved = left[j - r] * temp;
        }
        n[j] = saved;
    }
    n
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line_curve() -> NurbsCurve {
        // Degree-1 (piecewise linear) B-spline through (0,0,0) -> (10,0,0) -> (10,10,0).
        NurbsCurve {
            degree: 1,
            control_points: vec![
                Point3 {
                    x: 0.0,
                    y: 0.0,
                    z: 0.0,
                },
                Point3 {
                    x: 10.0,
                    y: 0.0,
                    z: 0.0,
                },
                Point3 {
                    x: 10.0,
                    y: 10.0,
                    z: 0.0,
                },
            ],
            weights: vec![1.0, 1.0, 1.0],
            knots: vec![0.0, 0.0, 1.0, 2.0, 2.0],
        }
    }

    #[test]
    fn evaluates_endpoints_and_midpoint_of_linear_bspline() {
        let curve = line_curve();
        assert!(curve.is_valid());
        let start = curve.evaluate(0.0).unwrap();
        assert!((start.x).abs() < 1e-9 && (start.y).abs() < 1e-9);
        let corner = curve.evaluate(1.0).unwrap();
        assert!((corner.x - 10.0).abs() < 1e-9 && (corner.y).abs() < 1e-9);
        let end = curve.evaluate(2.0).unwrap();
        assert!((end.x - 10.0).abs() < 1e-9 && (end.y - 10.0).abs() < 1e-9);
    }

    #[test]
    fn tessellates_bounded_point_count() {
        let curve = line_curve();
        let pts = curve.tessellate(10);
        assert_eq!(pts.len(), 11);
        let huge = curve.tessellate(usize::MAX);
        assert_eq!(huge.len(), MAX_NURBS_SAMPLES + 1);
    }

    #[test]
    fn rational_weights_pull_curve_toward_heavy_control_point() {
        // Degree-2 curve; boosting the middle control point's weight should
        // pull the midpoint evaluation toward it (rational NURBS behavior).
        let base = NurbsCurve {
            degree: 2,
            control_points: vec![
                Point3 {
                    x: 0.0,
                    y: 0.0,
                    z: 0.0,
                },
                Point3 {
                    x: 5.0,
                    y: 10.0,
                    z: 0.0,
                },
                Point3 {
                    x: 10.0,
                    y: 0.0,
                    z: 0.0,
                },
            ],
            weights: vec![1.0, 1.0, 1.0],
            knots: vec![0.0, 0.0, 0.0, 1.0, 1.0, 1.0],
        };
        let mut weighted = base.clone();
        weighted.weights[1] = 50.0;
        let mid_base = base.evaluate(0.5).unwrap();
        let mid_weighted = weighted.evaluate(0.5).unwrap();
        assert!(mid_weighted.y > mid_base.y);
    }

    #[test]
    fn invalid_curves_are_rejected() {
        let mut curve = line_curve();
        curve.knots.pop();
        assert!(!curve.is_valid());
        assert!(curve.evaluate(0.5).is_none());
        assert!(curve.tessellate(4).is_empty());

        let mut zero_weight = line_curve();
        zero_weight.weights[0] = 0.0;
        assert!(!zero_weight.is_valid());

        let mut non_monotonic = line_curve();
        non_monotonic.knots[2] = -1.0;
        assert!(!non_monotonic.is_valid());
    }

    #[test]
    fn expand_knot_multiplicities_flattens_and_bounds_input() {
        let knots = expand_knot_multiplicities(&[2, 1, 2], &[0.0, 1.0, 2.0]).unwrap();
        assert_eq!(knots, vec![0.0, 0.0, 1.0, 2.0, 2.0]);

        assert!(expand_knot_multiplicities(&[1, 2], &[0.0]).is_none());
        assert!(expand_knot_multiplicities(&[0], &[0.0]).is_none());
        assert!(expand_knot_multiplicities(&[u64::MAX], &[0.0]).is_none());
    }
}
