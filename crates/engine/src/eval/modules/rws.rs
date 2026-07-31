//! RWS (Real-World Sensitivity) aim evaluator.
//!
//! Scales a rotation-rate Vec2 into per-tick MOUSE DISPLACEMENT so that a
//! physical rotation maps 1:1 to the in-game camera rotation once `scale`
//! (mouse counts per degree) is calibrated; `rws` multiplies that ground truth.
//! The output (out 0, Vec2) is wired to the KB/M `mouse_move` sink pin — applied
//! once per tick and NOT scaled by the device card's mouse_sensitivity — so the
//! calibrated `scale` is the sole knob and presets stay portable.
//!
//! - Rate → displacement (gyro / stick_rate modes) + the calibration constant.
//! - Flick-stick (input 1): pushing the stick past a deadzone snaps the camera to
//!   face the stick direction, and holding it out + rotating traces the camera.

use super::*;

/// Signal-graph gyro normalization: ±1.0 corresponds to ±this many deg/s.
/// Mirrors `flexinput_devices::gyro::GYRO_REF_DPS` (kept local to avoid leaning
/// on a cross-crate pub path for one constant; keep the two in sync).
pub(crate) const GYRO_REF_DPS: f32 = 2000.0;

/// Intercepted evaluation for the RWS module (mirrors `eval_menu_node`): the same
/// math as `compute_rws`, plus flick-stick SOURCE-SUPPRESSION modelled on the
/// Virtual Menu. The flick stick (auto-detected at build time → `_rws_flick_*`)
/// is blocked at the source so it can't leak to its default mapping (e.g. the
/// virtual Right Stick), while THIS module keeps steering from it via the
/// pre-block snapshot (`unblocked_src`). `suppress_source`:
/// - `off`      — no block (plain passthrough).
/// - `full`     — block whenever flick is enabled.
/// - `deadzone` — block only while the stick is past the flick deadzone, so small
///   movements inside the deadzone still reach the default mapping.
pub(crate) fn eval_rws_node(
    snap: &NodeSnap,
    uid: usize,
    inputs: &[Option<Signal>],
    collector_sigs: &mut HashMap<(String, String), Signal>,
    state: &mut HashMap<usize, NodeState>,
    dt: f32,
) -> Vec<Option<Signal>> {
    let dev_id = snap.params.get("_rws_flick_device").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let stick = snap.params.get("_rws_flick_stick").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let flick_enabled = snap.params.get("flick_enabled").and_then(|v| v.as_bool()).unwrap_or(false);
    let sup_mode = snap.params.get("suppress_source").and_then(|v| v.as_str()).unwrap_or("off").to_string();
    let deadzone = snap.params.get("flick_deadzone").and_then(|v| v.as_f64()).unwrap_or(0.85) as f32;

    let can_suppress =
        flick_enabled && sup_mode != "off" && !dev_id.is_empty() && !stick.is_empty();

    // Recover the flick stick from the pre-block snapshot (we're the reason it's
    // blocked, so we must still read it); falls back to the resolved input when
    // it isn't (yet) being blocked.
    let unblocked: HashMap<(String, String), Signal> = state
        .get(&MACRO_CARRY_UID)
        .map(|s| s.unblocked_src.clone())
        .unwrap_or_default();
    let sig_f32 = |s: &Signal| if let Signal::Float(f) = s { Some(*f) } else { None };
    let flick_snapshot = if can_suppress {
        let x = unblocked.get(&(dev_id.clone(), format!("{stick}_x"))).and_then(sig_f32);
        let y = unblocked.get(&(dev_id.clone(), format!("{stick}_y"))).and_then(sig_f32);
        match (x, y) {
            (Some(x), Some(y)) => Some(glam::Vec2::new(x, y)),
            _ => match unblocked.get(&(dev_id.clone(), stick.clone())) {
                Some(Signal::Vec2(v)) => Some(*v),
                _ => None,
            },
        }
    } else {
        None
    };

    // Corrected inputs: override the Flick input (1) with the unblocked stick.
    let mut corrected: Vec<Option<Signal>> = inputs.to_vec();
    if let Some(v) = flick_snapshot {
        if corrected.len() < 2 {
            corrected.resize(2, None);
        }
        corrected[1] = Some(Signal::Vec2(v));
    }
    // Live flick vector for the deadzone-gated block decision (snapshot first,
    // else the resolved input).
    let live_flick = flick_snapshot.or_else(|| match corrected.get(1).and_then(|s| *s) {
        Some(Signal::Vec2(v)) => Some(v),
        _ => None,
    });

    let node_state = state.entry(uid).or_default();
    let out = compute_rws(&corrected, node_state, &snap.params, dt);

    // Publish the source-block for the flick stick (drained into `source_block`
    // at tick end, applied next tick — one tick stale, imperceptible at kHz).
    if can_suppress {
        let past = live_flick.map(|v| v.length() >= deadzone.max(0.05)).unwrap_or(false);
        let do_block = sup_mode == "full" || (sup_mode == "deadzone" && past);
        if do_block {
            let bk = format!("{SRC_BLOCK_PREFIX}{dev_id}");
            for p in [
                stick.clone(),
                format!("{stick}_x"),
                format!("{stick}_y"),
                format!("{stick}_up"),
                format!("{stick}_down"),
                format!("{stick}_left"),
                format!("{stick}_right"),
            ] {
                collector_sigs.insert((bk.clone(), p), Signal::Bool(true));
            }
        }
    }
    out
}

// Flick-stick state lives in `NodeState::aux_f32`:
//   [0] engaged (0/1)  [1] prev stick angle (rad)
//   [2] pending flick to pay out (deg)  [3] pay-out time left (s)
const FLK_ENGAGED: usize = 0;
const FLK_PREV_ANGLE: usize = 1;
const FLK_PENDING_DEG: usize = 2;
const FLK_PAYOUT_S: usize = 3;
const FLK_BELOW_S: usize = 4; // time (s) the stick has sat below the release floor
const FLK_TRACK_PENDING: usize = 5; // un-delivered tracking rotation (deg), smoothed out
/// Sustained release needed to disengage. Brief dropouts shorter than this (e.g.
/// a Bluetooth input gap that momentarily reads ~0) are ignored, so the flick
/// doesn't drop and re-fire.
const FLK_DISENGAGE_HOLD_S: f32 = 0.06;
/// Time constant for paying out tracked rotation. The stick usually polls slower
/// than the eval loop, so the raw per-tick heading delta arrives as 0,0,spike,…;
/// spreading it over this window turns it into a smooth stream the mouse driver
/// can consume (and steady rolling reaches a steady output rate after it primes).
const FLK_TRACK_SMOOTH_S: f32 = 0.025;

/// Compute one tick of the RWS module.
/// Outputs: [Mouse Vec2 (per-tick displacement), Stick Vec2 (right-stick
/// deflection, unit range)].
pub(crate) fn compute_rws(
    inputs: &[Option<Signal>],
    state: &mut NodeState,
    params: &HashMap<String, Value>,
    dt: f32,
) -> Vec<Option<Signal>> {
    let pf = |k: &str, d: f32| {
        params.get(k).and_then(|v| v.as_f64()).map(|v| v as f32).unwrap_or(d)
    };
    let ps = |k: &str, d: &'static str| {
        params.get(k).and_then(|v| v.as_str()).unwrap_or(d).to_string()
    };
    let pb = |k: &str, d: bool| params.get(k).and_then(|v| v.as_bool()).unwrap_or(d);

    let scale = pf("scale", 100.0);
    let rws = pf("rws", 1.0);
    let calibrating = pb("calibrating", false);
    let cal_speed = pf("cal_speed", 0.5);
    let input_mode = ps("input_mode", "gyro");
    let max_rate = pf("max_rate_dps", 360.0);
    let flick_enabled = pb("flick_enabled", false);
    let flick_deadzone = pf("flick_deadzone", 0.85);
    let flick_smooth_ms = pf("flick_smooth_ms", 100.0);

    // Rotation rate in deg/s (yaw = x, pitch = y). While calibrating we ignore
    // the inputs and spin at a KNOWN rate (cal_speed revolutions/second) so the
    // user can match the game to the on-screen reference; cal_speed 1.0 = 1 rev/s.
    let (yaw_dps, pitch_dps) = if calibrating {
        (cal_speed * 360.0, 0.0)
    } else {
        let rot = match inputs.first().and_then(|s| *s) {
            Some(Signal::Vec2(v)) => v,
            Some(Signal::Float(f)) => glam::Vec2::new(f, 0.0),
            _ => glam::Vec2::ZERO,
        };
        // Gyro axes are already ±1 == ±GYRO_REF_DPS deg/s; a stick is bounded
        // deflection, so treat it as a rate up to `max_rate_dps` at full tilt.
        let k = if input_mode == "stick_rate" { max_rate } else { GYRO_REF_DPS };
        (rot.x * k, rot.y * k)
    };

    // Flick-stick yaw contribution (degrees). It is a REAL rotation, so it maps
    // 1:1 through `scale` only — `rws` (a feel multiplier) deliberately does NOT
    // apply, so a flick lands on the direction you point regardless of RWS.
    let flick_deg = if flick_enabled && !calibrating {
        compute_flick(inputs, state, flick_deadzone, flick_smooth_ms, dt)
    } else {
        reset_flick(state);
        0.0
    };

    // Mouse output: per-tick displacement in mouse counts (scale = counts/degree).
    let dx = yaw_dps * dt * scale * rws + flick_deg * scale;
    let dy = pitch_dps * dt * scale * rws;

    // Right-stick output: the desired turn RATE (rws applied) normalized by the
    // game's full-deflection turn rate, clamped to the stick's unit range. Wire
    // this to a virtual Right Stick for stick-aim games. (The flick and a future
    // speed-cap "landing" that keeps deflecting until the intended angle is
    // reached are mouse-only for now.)
    let stick_max = pf("stick_out_dps", 360.0).max(1.0);
    let sx = (yaw_dps * rws / stick_max).clamp(-1.0, 1.0);
    let sy = (pitch_dps * rws / stick_max).clamp(-1.0, 1.0);

    vec![
        Some(Signal::Vec2(glam::Vec2::new(dx, dy))),
        Some(Signal::Vec2(glam::Vec2::new(sx, sy))),
    ]
}

/// One tick of flick-stick, returning the yaw rotation to apply in DEGREES.
/// Input 1 is the stick position (Vec2, up = +y). Crossing `deadzone` outward
/// engages: the camera flicks by the stick's angle from forward (smoothed over
/// `smooth_ms`). While engaged, further rotation of the stick tracks the camera
/// 1:1 (shortest arc). Dropping back inside the deadzone disengages.
fn compute_flick(
    inputs: &[Option<Signal>],
    state: &mut NodeState,
    deadzone: f32,
    smooth_ms: f32,
    dt: f32,
) -> f32 {
    while state.aux_f32.len() < 6 {
        state.aux_f32.push(0.0);
    }
    let stick = match inputs.get(1).and_then(|s| *s) {
        Some(Signal::Vec2(v)) => v,
        _ => glam::Vec2::ZERO,
    };
    let mut out = 0.0_f32;
    let engaged = state.aux_f32[FLK_ENGAGED] > 0.5;
    let mag = stick.length();
    // Engage past the deadzone; once engaged, TRACK as long as the stick is
    // actively deflected (a much lower "release floor"). Two guards keep a sweep
    // smooth on real hardware:
    //  • The heading is only read when the stick is reliably deflected — below the
    //    floor `atan2` is meaningless (a dropout reads ~0), so we hold the last
    //    heading instead of yanking toward 0°.
    //  • Disengage requires a SUSTAINED release (`FLK_DISENGAGE_HOLD_S`), so a
    //    brief Bluetooth input gap doesn't drop the flick and re-fire it.
    let engage_dz = deadzone.clamp(0.05, 0.99);
    let release_dz = (engage_dz * 0.5).clamp(0.15, 0.7);
    let reliable = mag >= release_dz;

    if reliable {
        state.aux_f32[FLK_BELOW_S] = 0.0;
        let angle = stick.x.atan2(stick.y);
        if engaged {
            // Track: shortest-arc change in heading since last reliable sample,
            // accumulated for smoothed pay-out below (not emitted as a raw spike).
            let prev = state.aux_f32[FLK_PREV_ANGLE];
            let mut d = angle - prev;
            if d > std::f32::consts::PI {
                d -= std::f32::consts::TAU;
            } else if d < -std::f32::consts::PI {
                d += std::f32::consts::TAU;
            }
            state.aux_f32[FLK_TRACK_PENDING] += d.to_degrees();
            state.aux_f32[FLK_PREV_ANGLE] = angle;
        } else if mag >= engage_dz {
            // Engage: flick to the stick's heading from forward (up), clockwise
            // positive so pushing RIGHT flicks the camera right (positive yaw).
            state.aux_f32[FLK_ENGAGED] = 1.0;
            state.aux_f32[FLK_PREV_ANGLE] = angle;
            let flick_deg = angle.to_degrees();
            if smooth_ms > 0.5 {
                state.aux_f32[FLK_PENDING_DEG] = flick_deg;
                state.aux_f32[FLK_PAYOUT_S] = smooth_ms / 1000.0;
            } else {
                out += flick_deg;
            }
        }
    } else if engaged {
        // Below the release floor: hold the heading (no spurious rotation) and
        // disengage only after a sustained release, so brief dropouts are ignored.
        state.aux_f32[FLK_BELOW_S] += dt;
        if state.aux_f32[FLK_BELOW_S] >= FLK_DISENGAGE_HOLD_S {
            state.aux_f32[FLK_ENGAGED] = 0.0;
        }
    }

    // Pay out the smoothed INITIAL flick over its own window (continues even
    // while tracking).
    if state.aux_f32[FLK_PAYOUT_S] > 0.0 {
        let t = state.aux_f32[FLK_PAYOUT_S];
        let remaining = state.aux_f32[FLK_PENDING_DEG];
        let emit = if t <= dt { remaining } else { remaining * (dt / t) };
        out += emit;
        state.aux_f32[FLK_PENDING_DEG] = remaining - emit;
        state.aux_f32[FLK_PAYOUT_S] = (t - dt).max(0.0);
    }

    // Pay out accumulated TRACKING rotation over a short window so poll-rate
    // quantization (0,0,spike,…) becomes a smooth stream. Conserves total.
    let tp = state.aux_f32[FLK_TRACK_PENDING];
    if tp != 0.0 {
        let emit = if FLK_TRACK_SMOOTH_S <= dt { tp } else { tp * (dt / FLK_TRACK_SMOOTH_S) };
        out += emit;
        state.aux_f32[FLK_TRACK_PENDING] = tp - emit;
    }

    out
}

/// Clear flick state so the next engage starts clean (used when flick is
/// disabled or while calibrating).
fn reset_flick(state: &mut NodeState) {
    for &i in &[FLK_ENGAGED, FLK_PENDING_DEG, FLK_PAYOUT_S, FLK_BELOW_S, FLK_TRACK_PENDING] {
        if let Some(v) = state.aux_f32.get_mut(i) {
            *v = 0.0;
        }
    }
}
