//! Response curves and the small signal helpers built on them: curve
//! sampling and bias, the log-ish scale mapping shared with the UI's
//! curve editors, vector reshaping, and multiband collapse.

use super::*;

pub fn sample_curve(pts: &[[f32; 2]], x: f32, biases: &[f32]) -> f32 {
    match pts.len() {
        0 => x,
        1 => pts[0][1],
        _ => {
            if x <= pts[0][0] { return pts[0][1]; }
            let last = pts.len() - 1;
            if x >= pts[last][0] { return pts[last][1]; }
            let seg = pts.windows(2).position(|w| x <= w[1][0]).unwrap_or(last - 1);
            let p1 = pts[seg]; let p2 = pts[seg + 1];
            let t    = (x - p1[0]) / (p2[0] - p1[0]);
            let bias = biases.get(seg).copied().unwrap_or(0.0);
            let base = p1[1] + (p2[1] - p1[1]) * t;
            base + bias * 4.0 * t * (1.0 - t)
        }
    }
}

pub fn apply_curve(
    x: f32, pts: &[[f32; 2]], biases: &[f32],
    absolute: bool, in_min: f32, in_max: f32, out_min: f32, out_max: f32, scale_t: f32,
) -> f32 {
    if absolute {
        let sign     = if x < 0.0 { -1.0f32 } else { 1.0 };
        let abs_max  = in_max.abs().max(in_min.abs()).max(f32::EPSILON);
        let abs_norm = (x.abs() / abs_max).clamp(0.0, 1.0);
        let scaled   = curve_scale(abs_norm, scale_t);
        let curve_y  = sample_curve(pts, scaled, biases).clamp(0.0, 1.0);
        let out_y    = curve_scale_inv(curve_y, scale_t);
        sign * out_y * out_max.abs().max(out_min.abs())
    } else {
        let in_range  = (in_max - in_min).abs().max(f32::EPSILON);
        let out_range = out_max - out_min;
        let norm      = ((x - in_min) / in_range * 2.0 - 1.0).clamp(-1.0, 1.0);
        let sign      = if norm < 0.0 { -1.0f32 } else { 1.0 };
        let scaled    = sign * curve_scale(norm.abs(), scale_t);
        let curve_y   = sample_curve(pts, scaled, biases);
        let sign_out  = if curve_y < 0.0 { -1.0f32 } else { 1.0 };
        let out_y     = sign_out * curve_scale_inv(curve_y.abs(), scale_t);
        out_min + (out_y.clamp(-1.0, 1.0) + 1.0) * 0.5 * out_range
    }
}

pub fn curve_scale(x: f32, t: f32) -> f32 {
    if t.abs() < 1e-4 { return x; }
    x.clamp(0.0, 1.0).powf(2.0f32.powf(t * 3.0))
}

pub fn curve_scale_inv(y: f32, t: f32) -> f32 {
    if t.abs() < 1e-4 { return y; }
    y.clamp(0.0, 1.0).powf(1.0 / 2.0f32.powf(t * 3.0))
}

pub fn curve_points_from_params(params: &HashMap<String, Value>) -> Vec<[f32; 2]> {
    let absolute = params.get("absolute").and_then(|v| v.as_bool()).unwrap_or(true);
    params.get("points").and_then(|v| v.as_array()).map(|arr| {
        arr.iter().filter_map(|pt| {
            let a = pt.as_array()?;
            Some([a.get(0)?.as_f64()? as f32, a.get(1)?.as_f64()? as f32])
        }).collect()
    }).unwrap_or_else(|| {
        if absolute { vec![[0.0, 0.0], [1.0, 1.0]] } else { vec![[-1.0, -1.0], [1.0, 1.0]] }
    })
}

pub fn biases_from_params(params: &HashMap<String, Value>) -> Vec<f32> {
    params.get("biases").and_then(|v| v.as_array()).map(|arr| {
        arr.iter().map(|v| v.as_f64().unwrap_or(0.0) as f32).collect()
    }).unwrap_or_default()
}

/// Read curve points from a CUSTOM params key (the standard helper is fixed to
/// `"points"`). Used by the Audio Stream Haptics band EQ, which lives under
/// `"asth_eq_points"` so it doesn't collide with any `"points"` key. Returns
/// `None` when the key is absent (→ EQ disabled, single-carrier path), or the
/// `[[x,y],…]` control points (x = band position 0..1, y = gain 0..1) otherwise.
pub fn curve_points_from_params_keyed(params: &HashMap<String, Value>, key: &str) -> Option<Vec<[f32; 2]>> {
    let arr = params.get(key)?.as_array()?;
    let pts: Vec<[f32; 2]> = arr.iter().filter_map(|pt| {
        let a = pt.as_array()?;
        Some([a.get(0)?.as_f64()? as f32, a.get(1)?.as_f64()? as f32])
    }).collect();
    if pts.len() >= 2 { Some(pts) } else { None }
}

// ── Vec Reshaper (directional Vec2 reshaping) ─────────────────────────────────

/// Default boundary control points for `module.vec_reshape`: a flat unit circle
/// (radius 1 at every angle) → identity gate. `angle01`: 0 = nearest cardinal
/// axis, 1 = diagonal. `radius`: gate distance in that direction (1.0 = circle).
pub const VEC_RESHAPE_BOUNDARY_DEFAULT: &[[f32; 2]] = &[[0.0, 1.0], [1.0, 1.0]];
/// Default gain curve: unity gain at every angle → no directional acceleration.
pub const VEC_RESHAPE_GAIN_DEFAULT: &[[f32; 2]] = &[[0.0, 1.0], [1.0, 1.0]];

/// Parse an `[[x,y],…]` control-point array from a params key, falling back to
/// `default` when absent/short (need ≥2 points to interpolate).
pub(crate) fn reshape_pts(params: &HashMap<String, Value>, key: &str, default: &[[f32; 2]]) -> Vec<[f32; 2]> {
    params.get(key).and_then(|v| v.as_array()).map(|arr| {
        arr.iter().filter_map(|p| {
            let a = p.as_array()?;
            Some([a.get(0)?.as_f64()? as f32, a.get(1)?.as_f64()? as f32])
        }).collect::<Vec<_>>()
    }).filter(|v| v.len() >= 2).unwrap_or_else(|| default.to_vec())
}

/// Fold a raw direction angle (radians, atan2 convention) into the single edited
/// quadrant and return `angle01` where 0 = nearest cardinal axis and 1 = the
/// diagonal, honouring the symmetry mode.
///
/// `quad4` — full 4-way symmetry: every 90° octant mirrors, so we fold into
///   0..45° measured from the nearest axis.
/// `xmirror` — left/right mirror only (top and bottom halves may differ): fold
///   about the vertical axis, then measure 0..90° from the +X axis so the whole
///   upper/lower semicircle is editable as one quadrant-parameterised curve.
pub(crate) fn reshape_angle01(theta: f32, symmetry: &str) -> f32 {
    use std::f32::consts::{FRAC_PI_2, FRAC_PI_4};
    match symmetry {
        "xmirror" => {
            // Left/right mirror only: measure absolute elevation from the
            // horizontal plane, 0 at ±X, 1 at ±Y. Top and bottom halves are NOT
            // folded together, so an asymmetric up-vs-down feel is expressible
            // (the caller edits the full 0..1 elevation as one curve).
            theta.sin().abs().clamp(0.0, 1.0).asin() / FRAC_PI_2
        }
        _ => {
            // quad4: angle within the current 90° sector, folded about its 45°
            // bisector so both halves of the octant share one curve.
            let s = theta.rem_euclid(FRAC_PI_2);   // 0..90°
            let d = (s - FRAC_PI_4).abs();          // 0 at diagonal, 45° at axis
            1.0 - (d / FRAC_PI_4)                   // 1 at diagonal, 0 at axis
        }
    }
}

/// The pure Vec Reshaper transform, shared by the engine (`eval_pure`) and the
/// UI preview so the on-node dots exactly match the routed signal.
///
/// The two controls are ORTHOGONAL:
///
///   • **Boundary** `boundary(a01)` sets the reachable OUTPUT ENVELOPE radius per
///     direction, in units where 1.0 = the unit circle and √2 ≈ 1.414 = the
///     corner of the full square. It is what lets the output ESCAPE the circle:
///     a boundary that rises to √2 on the diagonal turns a round input gate into
///     a full square (`renorm` on). This is the circle→square use case.
///   • **Gain** `gain(a01)` redistributes the deflection *within* 0..envelope
///     (accelerate/decelerate along a direction) WITHOUT changing how far the
///     envelope reaches. Unity (1.0) = linear, >1 = reach the edge sooner
///     (stretch), <1 = later (squeeze).
///
/// Pipeline: `frac = clamp(norm · gain, 0..1)` is the fraction of the envelope
/// reached; output magnitude = `frac · envelope · out_max`, where
/// `envelope = boundary` when `renorm` else 1.0 (boundary becomes display-only,
/// output stays circular). Direction is preserved.
#[allow(clippy::too_many_arguments)]
pub fn vec_reshape_apply(
    v: glam::Vec2,
    boundary_pts: &[[f32; 2]],
    gain_pts: &[[f32; 2]],
    gain_biases: &[f32],
    symmetry: &str,
    renorm: bool,
    in_max: f32,
    out_max: f32,
) -> glam::Vec2 {
    let mag = v.length();
    if mag < f32::EPSILON { return glam::Vec2::ZERO; }
    let dir = v / mag;
    let in_max = in_max.max(f32::EPSILON);

    let a01 = reshape_angle01(v.y.atan2(v.x), symmetry);

    // Deflection as a 0..1 fraction of the ROUND input gate in this direction.
    let norm = (mag / in_max).clamp(0.0, 1.0);

    // Gain redistributes WITHIN the envelope (does not change its reach).
    let gain = sample_curve(gain_pts, a01, gain_biases).max(0.0);
    let frac = (norm * gain).clamp(0.0, 1.0);

    // Envelope radius: >1 on the diagonal lets the vector reach the square's
    // corner. When renorm is off the envelope stays circular (boundary is a
    // display-only reference) so only gain shapes the feel.
    let envelope = if renorm {
        sample_curve(boundary_pts, a01, &[]).clamp(0.05, std::f32::consts::SQRT_2)
    } else {
        1.0
    };

    dir * (frac * envelope * out_max)
}

/// Collapse an EQ-gained log-band spectrum to a single carrier (used by the
/// single-carrier path / the UI's carrier marker). Applies the per-band gain curve
/// (`eq_pts`, x = band position 0..1, y = gain 0..1) and returns the amplitude-
/// weighted **centroid** band position as the carrier. `None` when silent.
pub fn multiband_collapse_carrier(spectrum: &[f32], eq_pts: &[[f32; 2]]) -> Option<f32> {
    multiband_collapse_band(spectrum, eq_pts, 0.0, 1.0).map(|(carrier, _)| carrier)
}

/// Collapse one sub-band `[lo, hi]` (band positions 0..1) of an EQ-gained spectrum
/// to a single carrier. Returns `(carrier_pos, energy)` where `carrier_pos` is the
/// gain-weighted centroid WITHIN the sub-band remapped back to 0..1 over the full
/// range (so it's a normal carrier value), and `energy` is the summed gained
/// magnitude in the sub-band (used to weight the band's amplitude). `None` when the
/// sub-band is essentially silent.
pub fn multiband_collapse_band(spectrum: &[f32], eq_pts: &[[f32; 2]], lo: f32, hi: f32) -> Option<(f32, f32)> {
    let n = spectrum.len();
    if n == 0 || hi <= lo { return None; }
    let mut num = 0.0f32; // Σ gained * position
    let mut den = 0.0f32; // Σ gained
    for (i, &m) in spectrum.iter().enumerate() {
        let pos = (i as f32 + 0.5) / n as f32;
        if pos < lo || pos >= hi { continue; }
        let gain = sample_curve(eq_pts, pos, &[]).clamp(0.0, 4.0);
        let gained = m.max(0.0).sqrt() * gain; // perceptual weight, matches the view
        num += gained * pos;
        den += gained;
    }
    if den <= 1.0e-4 { return None; }
    Some(((num / den).clamp(0.0, 1.0), den))
}

/// Convert a crossover frequency (Hz) to a band position 0..1 on the log-spaced
/// spectrum range (40 Hz–1253 Hz), matching `flexinput_devices::spectrum`'s bands.
pub fn crossover_hz_to_pos(hz: f32) -> f32 {
    const MIN: f32 = 40.0;
    const MAX: f32 = 1253.0;
    let hz = hz.clamp(MIN, MAX);
    ((hz / MIN).ln() / (MAX / MIN).ln()).clamp(0.0, 1.0)
}

pub fn read_scale_t(params: &HashMap<String, Value>) -> f32 {
    params.get("scale_t").and_then(|v| v.as_f64()).map(|f| f as f32)
        .unwrap_or_else(|| match params.get("in_scale").and_then(|v| v.as_i64()).unwrap_or(0) {
            1 => -0.5,
            2 =>  0.5,
            _ =>  0.0,
        })
}

// ── Helpers ───────────────────────────────────────────────────────────────────

pub fn sig_to_f32(s: Option<Signal>) -> Option<f32> {
    match s {
        Some(Signal::Float(f)) => Some(f),
        Some(Signal::Bool(b))  => Some(if b { 1.0 } else { 0.0 }),
        Some(Signal::Vec2(v))  => Some(v.length()),
        Some(Signal::Vec4(v))  => Some(v.length()),
        Some(Signal::Int(i))   => Some(i as f32),
        None => None,
    }
}

pub fn get_f(inputs: &[Option<Signal>], i: usize, default: f32) -> f32 {
    inputs.get(i).and_then(|s| *s).map(|s| s.as_float()).unwrap_or(default)
}

pub fn get_b(inputs: &[Option<Signal>], i: usize, default: bool) -> bool {
    inputs.get(i).and_then(|s| *s).map(|s| s.as_bool()).unwrap_or(default)
}

/// Lift input slot to Vec2: Vec2 passes through, scalars are splatted, None → splat(default).
pub(crate) fn get_v2(inputs: &[Option<Signal>], i: usize, default: f32) -> Vec2 {
    match inputs.get(i).and_then(|s| *s) {
        Some(Signal::Vec2(v)) => v,
        Some(other) => Vec2::splat(other.as_float()),
        None => Vec2::splat(default),
    }
}

pub(crate) fn sig_scalar(s: Signal) -> f32 {
    match s {
        Signal::Float(f) => f,
        Signal::Int(i)   => i as f32,
        Signal::Bool(b)  => if b { 1.0 } else { 0.0 },
        Signal::Vec2(v)  => v.length(),
        Signal::Vec4(v)  => v.length(),
    }
}

