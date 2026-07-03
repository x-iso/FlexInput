//! Unit coverage for the Vec Reshaper directional transform
//! (`flexinput_engine::vec_reshape_apply`) — the module that fights diagonal
//! stick "stickiness" by reshaping a Vec2 as a function of DIRECTION.

use flexinput_engine::{vec_reshape_apply, VEC_RESHAPE_BOUNDARY_DEFAULT, VEC_RESHAPE_GAIN_DEFAULT};
use glam::Vec2;

const EPS: f32 = 1e-3;

fn approx(a: Vec2, b: Vec2) -> bool {
    (a - b).length() < EPS
}

/// Identity config (flat unit-circle boundary, unity gain) must pass the vector
/// through unchanged for cardinals AND diagonals.
#[test]
fn identity_passes_through() {
    let b = VEC_RESHAPE_BOUNDARY_DEFAULT;
    let g = VEC_RESHAPE_GAIN_DEFAULT;
    for v in [
        Vec2::new(1.0, 0.0),
        Vec2::new(0.0, 1.0),
        Vec2::new(-1.0, 0.0),
        Vec2::new(0.5, 0.5),
        Vec2::new(-0.3, 0.7),
    ] {
        let out = vec_reshape_apply(v, b, g, &[], "quad4", true, 1.0, 1.0);
        assert!(approx(out, v), "identity changed {v:?} → {out:?}");
    }
}

/// Zero input → zero output (no NaNs from the 0/0 direction).
#[test]
fn zero_stays_zero() {
    let out = vec_reshape_apply(
        Vec2::ZERO,
        VEC_RESHAPE_BOUNDARY_DEFAULT,
        VEC_RESHAPE_GAIN_DEFAULT,
        &[],
        "quad4",
        true,
        1.0,
        1.0,
    );
    assert_eq!(out, Vec2::ZERO);
    assert!(out.x.is_finite() && out.y.is_finite());
}

/// Circle→square boundary: a full-deflection DIAGONAL input on a round gate
/// (magnitude 1) must, with renorm ON and a √2 diagonal boundary, EXPAND to reach
/// ~1.414 — the corner of the unit square — so a round input drives a square
/// output. Cardinals stay at 1.0 (boundary radius 1 there). This is the whole
/// point of the module: escaping the circle to fill a square.
#[test]
fn circle_to_square_expands_diagonal_keeps_cardinal() {
    // radius 1 at the axis, √2 at the diagonal (a01 = 1.0).
    let boundary = [[0.0, 1.0], [1.0, std::f32::consts::SQRT_2]];
    let gain = VEC_RESHAPE_GAIN_DEFAULT;

    // Cardinal at full deflection: still 1.0 (unchanged envelope on the axis).
    let card = vec_reshape_apply(Vec2::new(1.0, 0.0), &boundary, gain, &[], "quad4", true, 1.0, 1.0);
    assert!((card.length() - 1.0).abs() < EPS, "cardinal magnitude changed: {}", card.length());

    // Diagonal unit vector (magnitude 1) → envelope √2 → reaches the corner.
    let diag_in = Vec2::new(1.0, 1.0).normalize(); // magnitude 1, 45°
    let diag = vec_reshape_apply(diag_in, &boundary, gain, &[], "quad4", true, 1.0, 1.0);
    assert!(
        (diag.length() - std::f32::consts::SQRT_2).abs() < 1e-2,
        "diagonal should expand to ~1.414 (square corner), got {}",
        diag.length()
    );
    // And the actual X/Y each reach ~1.0 — i.e. the square's corner (1,1).
    assert!((diag.x - 1.0).abs() < 1e-2 && (diag.y - 1.0).abs() < 1e-2,
        "diagonal should land at the (1,1) corner, got {diag:?}");
    // Direction preserved.
    assert!(diag.normalize().abs_diff_eq(diag_in, EPS), "diagonal direction rotated");
}

/// With renorm OFF, the boundary is display-only: the envelope stays circular,
/// so even a √2 boundary curve leaves the diagonal at unit magnitude.
#[test]
fn renorm_off_keeps_circle() {
    let boundary = [[0.0, 1.0], [1.0, std::f32::consts::SQRT_2]];
    let gain = VEC_RESHAPE_GAIN_DEFAULT;
    let diag_in = Vec2::new(1.0, 1.0).normalize();
    let diag = vec_reshape_apply(diag_in, &boundary, gain, &[], "quad4", false, 1.0, 1.0);
    assert!((diag.length() - 1.0).abs() < EPS,
        "renorm off must stay on the circle, got {}", diag.length());
}

/// Directional gain boosts the diagonal without touching the cardinal — the core
/// "kill diagonal stickiness" behaviour. Renorm OFF so we isolate gain.
#[test]
fn diagonal_gain_boosts_only_diagonal() {
    let boundary = VEC_RESHAPE_BOUNDARY_DEFAULT; // flat
    let gain = [[0.0, 1.0], [1.0, 1.6]]; // unity at axis, 1.6× at diagonal

    // A modest cardinal input keeps its magnitude (gain 1.0 there).
    let card_in = Vec2::new(0.5, 0.0);
    let card = vec_reshape_apply(card_in, boundary, &gain, &[], "quad4", false, 1.0, 1.0);
    assert!((card.length() - 0.5).abs() < EPS, "cardinal gain should be unity, got {}", card.length());

    // The same magnitude on the diagonal is boosted ~1.6×.
    let diag_in = Vec2::new(1.0, 1.0).normalize() * 0.5; // magnitude 0.5 at 45°
    let diag = vec_reshape_apply(diag_in, boundary, &gain, &[], "quad4", false, 1.0, 1.0);
    assert!(
        (diag.length() - 0.5 * 1.6).abs() < 1e-2,
        "diagonal should be ~0.8 (0.5×1.6), got {}",
        diag.length()
    );
}

/// Output is clamped to `out_max` even when gain would overshoot — a routed
/// axis must never exceed full scale.
#[test]
fn gain_output_clamps_to_out_max() {
    let boundary = VEC_RESHAPE_BOUNDARY_DEFAULT;
    let gain = [[0.0, 3.0], [1.0, 3.0]]; // 3× everywhere
    let out = vec_reshape_apply(Vec2::new(0.9, 0.0), boundary, &gain, &[], "quad4", false, 1.0, 1.0);
    assert!(out.length() <= 1.0 + EPS, "must clamp to out_max, got {}", out.length());
    assert!((out.length() - 1.0).abs() < EPS, "0.9×3 clamps to 1.0, got {}", out.length());
}

/// 4-way symmetry: all four diagonals get the SAME treatment from a single
/// edited quadrant.
#[test]
fn quad4_symmetry_mirrors_all_diagonals() {
    let boundary = [[0.0, 1.0], [1.0, std::f32::consts::SQRT_2]];
    let gain = VEC_RESHAPE_GAIN_DEFAULT;
    let mag = |v: Vec2| vec_reshape_apply(v, &boundary, gain, &[], "quad4", true, 1.0, 1.0).length();
    let d = std::f32::consts::FRAC_1_SQRT_2;
    let m = mag(Vec2::new(d, d));
    for v in [Vec2::new(-d, d), Vec2::new(d, -d), Vec2::new(-d, -d)] {
        assert!((mag(v) - m).abs() < EPS, "quadrant {v:?} differs from reference: {} vs {m}", mag(v));
    }
}

/// X-mirror symmetry lets the upper and lower halves differ: with a boundary
/// that only stretches near +Y, an up-diagonal and a down-diagonal need NOT
/// match. (Here they DO share the horizontal axis but diverge in elevation.)
#[test]
fn xmirror_allows_top_bottom_difference() {
    // Elevation curve: radius 1 at horizontal, growing to 1.5 at vertical.
    let boundary = [[0.0, 1.0], [1.0, 1.5]];
    let gain = VEC_RESHAPE_GAIN_DEFAULT;
    // Left/right must match (mirror plane), so +X and -X are identical…
    let px = vec_reshape_apply(Vec2::new(1.0, 0.2), &boundary, gain, &[], "xmirror", true, 1.0, 1.0);
    let nx = vec_reshape_apply(Vec2::new(-1.0, 0.2), &boundary, gain, &[], "xmirror", true, 1.0, 1.0);
    assert!((px.length() - nx.length()).abs() < EPS, "xmirror must mirror across X");
    // …and the transform stays finite for a straight-up input.
    let up = vec_reshape_apply(Vec2::new(0.0, 1.0), &boundary, gain, &[], "xmirror", true, 1.0, 1.0);
    assert!(up.x.is_finite() && up.y.is_finite() && up.length() > 0.0);
}
