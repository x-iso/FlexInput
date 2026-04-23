/// Evaluate a piecewise curve at `x`.
/// Each segment is a straight line between its two endpoints, bent by a per-segment
/// bias: positive bends up (log-like), negative bends down (exp-like), 0 = straight.
/// The curve always passes through every control point and never overshoots them.
pub fn sample_curve(pts: &[[f32; 2]], x: f32, biases: &[f32]) -> f32 {
    match pts.len() {
        0 => x,
        1 => pts[0][1],
        _ => {
            if x <= pts[0][0] { return pts[0][1]; }
            let last = pts.len() - 1;
            if x >= pts[last][0] { return pts[last][1]; }

            let seg = pts.windows(2).position(|w| x <= w[1][0]).unwrap_or(last - 1);
            let p1 = pts[seg];
            let p2 = pts[seg + 1];
            let t = (x - p1[0]) / (p2[0] - p1[0]);
            let bias = biases.get(seg).copied().unwrap_or(0.0);
            lerp_biased(p1[1], p2[1], t, bias)
        }
    }
}

/// Linear interpolation from y1 to y2, with a bell-shaped bias offset.
/// bias=0 → straight line; bias>0 → bows up; bias<0 → bows down.
/// Endpoints are always exact regardless of bias.
fn lerp_biased(y1: f32, y2: f32, t: f32, bias: f32) -> f32 {
    let base = y1 + (y2 - y1) * t;
    // Bell peaks at t=0.5 with weight 1.0; zero at t=0 and t=1.
    base + bias * 4.0 * t * (1.0 - t)
}
