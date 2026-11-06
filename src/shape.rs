use std::f64::consts::{FRAC_PI_2, FRAC_PI_4, SQRT_2};
use std::ptr;

use objc2_core_foundation::{CFRetained, CGPoint};
use objc2_core_graphics::CGMutablePath;

/// A rounded rectangle with the dock's corners: Figma's corner smoothing, a cubic easing into a
/// circular arc and another easing out of it, so the curvature builds up instead of jumping where
/// the edges end.
pub fn smooth_rect(
    left: f64,
    bottom: f64,
    width: f64,
    height: f64,
    radius: f64,
    smoothing: f64,
) -> CFRetained<CGMutablePath> {
    let path = CGMutablePath::new();
    if width <= 0.01 || height <= 0.01 {
        return path;
    }
    let k = 1.0 + smoothing;
    let r = radius.min(width / 2.0 / k).min(height / 2.0 / k);
    let (right, top) = (left + width, bottom + height);
    let (up, down, leftward, rightward) = ((0.0, 1.0), (0.0, -1.0), (-1.0, 0.0), (1.0, 0.0));
    let corners = [
        (CGPoint::new(right, bottom), rightward, up),
        (CGPoint::new(right, top), up, leftward),
        (CGPoint::new(left, top), leftward, down),
        (CGPoint::new(left, bottom), down, rightward),
    ];
    let target = Some(&*path);
    for (i, (vertex, u, v)) in corners.into_iter().enumerate() {
        let (start, segments) = corner(vertex, u, v, r, smoothing);
        // SAFETY: a null transform is allowed, and means none.
        unsafe {
            if i == 0 {
                CGMutablePath::move_to_point(target, ptr::null(), start.x, start.y);
            } else {
                CGMutablePath::add_line_to_point(target, ptr::null(), start.x, start.y);
            }
            for [a, b, end] in segments {
                CGMutablePath::add_curve_to_point(target, ptr::null(), a.x, a.y, b.x, b.y, end.x, end.y);
            }
        }
    }
    CGMutablePath::close_subpath(target);
    path
}

/// The corner at `vertex` between an edge heading along `u` into it and one heading along `v` out
/// of it: where it starts, (1 + s)·r before the vertex, and three cubics as control points and end.
fn corner(vertex: CGPoint, u: (f64, f64), v: (f64, f64), r: f64, s: f64) -> (CGPoint, [[CGPoint; 3]; 3]) {
    let offset = |point: CGPoint, along: f64, across: f64| {
        CGPoint::new(point.x + along * u.0 + across * v.0, point.y + along * u.1 + across * v.1)
    };
    let p = (1.0 + s) * r;
    let start = offset(vertex, -p, 0.0);
    let arc_angle = FRAC_PI_2 * (1.0 - s);
    let arc_side = (arc_angle / 2.0).sin() * r * SQRT_2;
    let tangent = r * ((FRAC_PI_2 - arc_angle) / 4.0).tan();
    let beta = FRAC_PI_4 * s;
    let c = tangent * beta.cos();
    let d = c * beta.tan();
    let b = (p - arc_side - c - d) / 3.0;
    let a = 2.0 * b;
    let arc_start = offset(start, a + b + c, d);
    let arc_end = offset(arc_start, arc_side, arc_side);
    // The arc as one cubic: its unit tangents at either end, in (u, v), times 4/3·tan(θ/4) of the
    // radius.
    let handle = 4.0 / 3.0 * (arc_angle / 4.0).tan();
    (
        start,
        [
            [offset(start, a, 0.0), offset(start, a + b, 0.0), arc_start],
            [
                offset(arc_start, (r - d) * handle, (r - arc_side - d) * handle),
                offset(arc_end, -(r - arc_side - d) * handle, -(r - d) * handle),
                arc_end,
            ],
            [offset(arc_end, d, c), offset(arc_end, d, b + c), offset(arc_end, d, a + b + c)],
        ],
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_corner_runs_from_p_before_the_vertex_to_p_after_it() {
        for s in [0.0, 0.3, 0.6, 1.0] {
            let (start, segments) = corner(CGPoint::new(10.0, 10.0), (1.0, 0.0), (0.0, -1.0), 4.0, s);
            let p = (1.0 + s) * 4.0;
            let end = segments[2][2];
            assert!((start.x - (10.0 - p)).abs() < 1e-9 && (start.y - 10.0).abs() < 1e-9, "smoothing {s}");
            assert!((end.x - 10.0).abs() < 1e-9 && (end.y - (10.0 - p)).abs() < 1e-9, "smoothing {s}");
        }
    }

    #[test]
    fn the_arc_lies_on_the_corner_circle() {
        let (r, s) = (10.0, 0.6);
        let (_, segments) = corner(CGPoint::new(0.0, 0.0), (1.0, 0.0), (0.0, 1.0), r, s);
        // The circle touching both edges is centred r back along u and r along v.
        for point in [segments[0][2], segments[1][2]] {
            let distance = ((point.x + r).powi(2) + (point.y - r).powi(2)).sqrt();
            assert!((distance - r).abs() < 1e-9, "distance {distance}");
        }
    }
}
