//! Imported spline parameterizations can defeat recursive tolerance subdivision.
use truck_meshalgo::prelude::*;
use truck_stepio::r#in::alias::{Curve3D, ElementarySurface, Surface};
use truck_topology::compress::{CompressedEdge, CompressedFace, CompressedShell};
#[derive(Clone)]
pub(crate) struct PreviewSurface(Surface);
impl ParametricSurface for PreviewSurface {
    type Point = Point3;
    type Vector = Vector3;
    fn subs(&self, u: f64, v: f64) -> Point3 {
        self.0.subs(u, v)
    }
    fn uder(&self, u: f64, v: f64) -> Vector3 {
        self.0.uder(u, v)
    }
    fn vder(&self, u: f64, v: f64) -> Vector3 {
        self.0.vder(u, v)
    }
    fn uuder(&self, u: f64, v: f64) -> Vector3 {
        self.0.uuder(u, v)
    }
    fn uvder(&self, u: f64, v: f64) -> Vector3 {
        self.0.uvder(u, v)
    }
    fn vvder(&self, u: f64, v: f64) -> Vector3 {
        self.0.vvder(u, v)
    }
    fn parameter_range(&self) -> (ParameterRange, ParameterRange) {
        self.0.parameter_range()
    }
    fn u_period(&self) -> Option<f64> {
        self.0.u_period()
    }
    fn v_period(&self) -> Option<f64> {
        self.0.v_period()
    }
}
impl ParametricSurface3D for PreviewSurface {}
impl ParameterDivision2D for PreviewSurface {
    fn parameter_division(
        &self,
        range: ((f64, f64), (f64, f64)),
        tol: f64,
    ) -> (Vec<f64>, Vec<f64>) {
        match &self.0 {
            Surface::BSplineSurface(_) | Surface::NurbsSurface(_) => {
                let grid =
                    |(a, b): (f64, f64)| (0..=24).map(|i| a + (b - a) * i as f64 / 24.).collect();
                (grid(range.0), grid(range.1))
            }
            Surface::ElementarySurface(surface)
                if matches!(&**surface, ElementarySurface::Sphere(_)) =>
            {
                // Tiny pin/lead fillets can be smaller than the preview tolerance.
                // The kernel asserts tol < radius; bounded angular sampling is
                // independent of radius and retains these faces without panicking.
                let grid = |(a, b): (f64, f64)| {
                    let count = ((b - a).abs() / (std::f64::consts::PI / 12.))
                        .ceil()
                        .clamp(1., 48.) as usize;
                    (0..=count)
                        .map(|i| a + (b - a) * i as f64 / count as f64)
                        .collect()
                };
                (grid(range.0), grid(range.1))
            }
            _ => self.0.parameter_division(range, tol),
        }
    }
}
impl SearchParameter<D2> for PreviewSurface {
    type Point = Point3;
    fn search_parameter<H: Into<SPHint2D>>(
        &self,
        point: Point3,
        hint: H,
        trials: usize,
    ) -> Option<(f64, f64)> {
        self.0.search_parameter(point, hint, trials.min(16))
    }
}
impl SearchNearestParameter<D2> for PreviewSurface {
    type Point = Point3;
    fn search_nearest_parameter<H: Into<SPHint2D>>(
        &self,
        point: Point3,
        hint: H,
        trials: usize,
    ) -> Option<(f64, f64)> {
        self.0.search_nearest_parameter(point, hint, trials.min(16))
    }
}
// Tessellate edges before entering the kernel: its generic PCurve subdivision
// otherwise recurses up to 100 levels, including when samples are NaN. Cap work
// before allocating the resulting polylines, rather than trimming them afterwards.
const MAX_EDGE_SEGMENTS: usize = 512;
fn curve_points(curve: &Curve3D, ends: [Point3; 2], tol: f64) -> Vec<Point3> {
    if matches!(curve, Curve3D::Line(_)) {
        return ends.to_vec();
    }
    sampled_points(|t| curve.subs(t), curve.range_tuple(), ends, tol)
}
fn sampled_points(
    evaluate: impl Fn(f64) -> Point3,
    (start, end): (f64, f64),
    ends: [Point3; 2],
    tol: f64,
) -> Vec<Point3> {
    if !start.is_finite() || !end.is_finite() || start == end {
        return ends.to_vec();
    }
    let finite = |p: Point3| p.x.is_finite() && p.y.is_finite() && p.z.is_finite();
    let sample = |t: f64| {
        if t == start {
            return ends[0];
        }
        if t == end {
            return ends[1];
        }
        evaluate(t)
    };
    let mut points = vec![ends[0]];
    let mut stack = Vec::new();
    // Seed intervals catch closed loops and inflections whose midpoint is on
    // the chord. Quarter-point tests then adapt to geometric curvature.
    for i in (0..8).rev() {
        let a = start + (end - start) * i as f64 / 8.;
        let b = start + (end - start) * (i + 1) as f64 / 8.;
        stack.push((a, b, sample(a), sample(b), 0));
    }
    while let Some((a, b, p, q, depth)) = stack.pop() {
        let mid = (a + b) * 0.5;
        let m = sample(mid);
        let probes = [
            (0.25, sample(a + (b - a) * 0.25)),
            (0.5, m),
            (0.75, sample(a + (b - a) * 0.75)),
        ];
        let valid = finite(p) && finite(q) && probes.iter().all(|(_, v)| finite(*v));
        let split = valid
            && probes
                .iter()
                .any(|(t, v)| v.distance2(p + (q - p) * *t) > tol * tol);
        if split
            && depth < 8
            && points.len() + stack.len() + 1 < MAX_EDGE_SEGMENTS
            && mid != a
            && mid != b
        {
            stack.push((mid, b, m, q, depth + 1));
            stack.push((a, mid, p, m, depth + 1));
        } else {
            // Invalid parameterized segments use the topological edge chord.
            // Recursing on a non-finite error would create an exponential tree.
            let t = (b - start) / (end - start);
            points.push(if finite(q) {
                q
            } else {
                ends[0] + (ends[1] - ends[0]) * t
            });
        }
    }
    points
}
pub(crate) fn shell(
    shell: CompressedShell<Point3, Curve3D, Surface>,
    tolerance: f64,
) -> CompressedShell<Point3, PolylineCurve<Point3>, PreviewSurface> {
    let edges = shell
        .edges
        .into_iter()
        .map(|edge| {
            let ends = [
                shell.vertices[edge.vertices.0],
                shell.vertices[edge.vertices.1],
            ];
            CompressedEdge {
                vertices: edge.vertices,
                curve: PolylineCurve::from(curve_points(&edge.curve, ends, tolerance)),
            }
        })
        .collect();
    CompressedShell {
        vertices: shell.vertices,
        edges,
        faces: shell
            .faces
            .into_iter()
            .map(|f| CompressedFace {
                boundaries: f.boundaries,
                orientation: f.orientation,
                surface: PreviewSurface(f.surface),
            })
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use truck_stepio::r#in::alias::{BSplineSurface, KnotVec};
    #[test]
    fn pathological_curve_subdivision_has_a_fixed_sample_budget() {
        let calls = std::cell::Cell::new(0);
        let sample = |t: f64| {
            calls.set(calls.get() + 1);
            Point3::new(t, (t * 1000.).sin(), 0.)
        };
        let ends = [sample(0.), sample(1.)];
        let points = sampled_points(sample, (0., 1.), ends, 1e-12);
        assert!(points.len() <= MAX_EDGE_SEGMENTS + 1);
        assert!(calls.get() < MAX_EDGE_SEGMENTS * 8);
        assert_eq!(points.first(), Some(&ends[0]));
        assert_eq!(points.last(), Some(&ends[1]));
    }
    #[test]
    fn invalid_curve_samples_do_not_subdivide_and_keep_finite_topological_endpoints() {
        let calls = std::cell::Cell::new(0);
        let points = sampled_points(
            |_| {
                calls.set(calls.get() + 1);
                Point3::new(f64::NAN, f64::INFINITY, 0.)
            },
            (0., 1.),
            [Point3::new(0., 0., 0.), Point3::new(1., 1., 1.)],
            0.08,
        );
        assert!(points.len() <= 9);
        assert!(calls.get() < 50);
        assert!(
            points
                .iter()
                .all(|p| p.x.is_finite() && p.y.is_finite() && p.z.is_finite())
        );
        assert_eq!(points.last(), Some(&Point3::new(1., 1., 1.)));
    }
    #[test]
    fn closed_curve_sampling_retains_shape_and_preview_accuracy() {
        let sample = |t: f64| Point3::new(t.cos(), t.sin(), 0.);
        let points = sampled_points(
            sample,
            (0., std::f64::consts::TAU),
            [sample(0.), sample(std::f64::consts::TAU)],
            0.01,
        );
        assert!(points.len() > 8 && points.len() < MAX_EDGE_SEGMENTS);
        assert!(
            points
                .iter()
                .all(|p| (p.x * p.x + p.y * p.y - 1.).abs() < 1e-9)
        );
        assert!(
            points
                .windows(2)
                .all(|p| (p[0] + (p[1] - p[0]) * 0.5).to_vec().magnitude() > 0.99)
        );
    }
    #[test]
    fn spherical_fillets_smaller_than_tolerance_still_have_bounded_sampling() {
        use truck_stepio::r#in::alias::{Processor, Sphere};
        let surface = PreviewSurface(Surface::ElementarySurface(Box::new(
            ElementarySurface::Sphere(Processor::new(Sphere(
                truck_stepio::r#in::alias::re_exports::Sphere::new(Point3::new(0., 0., 0.), 0.01),
            ))),
        )));
        let (u, v) = surface.parameter_division(
            ((0., std::f64::consts::PI), (0., std::f64::consts::TAU)),
            0.08,
        );
        assert!((2..=49).contains(&u.len()) && (2..=49).contains(&v.len()));
        assert!(u.iter().chain(&v).all(|p| p.is_finite()));
        assert!(u.windows(2).all(|p| p[1] > p[0]));
    }
    #[test]
    fn nearest_boundary_fit_accepts_small_step_surface_roundoff() {
        let knots = KnotVec::from(vec![0., 0., 1., 1.]);
        let surface = PreviewSurface(Surface::BSplineSurface(Box::new(BSplineSurface::new(
            (knots.clone(), knots),
            vec![
                vec![Point3::new(0., 0., 0.), Point3::new(0., 1., 0.)],
                vec![Point3::new(1., 0., 0.), Point3::new(1., 1., 0.)],
            ],
        ))));
        let point = Point3::new(0.5, 0.5, 0.001);
        assert!(
            surface
                .search_parameter(point, Some((0.5, 0.5)), 100)
                .is_none()
        );
        let (u, v) = surface
            .search_nearest_parameter(point, Some((0.5, 0.5)), 100)
            .unwrap();
        assert!((u - 0.5).abs() < 1e-6 && (v - 0.5).abs() < 1e-6);
    }
    #[test]
    fn spline_preview_subdivision_is_bounded_even_at_extreme_tolerance() {
        let knots = KnotVec::from(vec![0., 0., 1., 1.]);
        let surface = PreviewSurface(Surface::BSplineSurface(Box::new(BSplineSurface::new(
            (knots.clone(), knots),
            vec![
                vec![Point3::new(0., 0., 0.), Point3::new(0., 1., 0.)],
                vec![Point3::new(1., 0., 0.), Point3::new(1., 1., 1.)],
            ],
        ))));
        let (u, v) = surface.parameter_division(((0., 1.), (0., 1.)), 1e-12);
        assert_eq!((u.len(), v.len()), (25, 25));
        assert_eq!((u[0], u[24], v[0], v[24]), (0., 1., 0., 1.));
        assert_eq!(surface.subs(1., 1.), Point3::new(1., 1., 1.));
        assert!(
            surface
                .search_parameter(Point3::new(1., 1., 1.), Some((0.9, 0.9)), 100)
                .is_some()
        );
    }
}
