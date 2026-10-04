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
/// - `full`     — block whenever flick or stick-aim is enabled.
/// - `deadzone` — block only while the stick is past the flick deadzone, so small
///   movements inside the deadzone still reach the default mapping (flick only).
///
/// A calibration sweep blocks the stick whatever the mode: the sweep drives the
/// game from this module alone (the 360° also counts the stick circled round its
/// edge), so the stick must not turn the camera a second time through its
/// default mapping. Outside a sweep only `suppress_source` decides — stick-aim
/// never mutes the stick for other modules on its own.
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
    let (flick_on, stick_aim) = rws_stick_uses(inputs, &snap.params);
    let sup_mode = snap.params.get("suppress_source").and_then(|v| v.as_str()).unwrap_or("off").to_string();
    let deadzone = snap.params.get("flick_deadzone").and_then(|v| v.as_f64()).unwrap_or(0.85) as f32;
    let measuring = matches!(snap.params.get("cal_measure").and_then(|v| v.as_str()), Some("pitch" | "yaw"));

    let can_suppress = (measuring || ((flick_on || stick_aim) && sup_mode != "off"))
        && !dev_id.is_empty()
        && !stick.is_empty();

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
    let out = rws_fit_outputs(compute_rws(&corrected, node_state, &snap.params, dt), snap.n_outputs);

    // Publish the source-block for the flick stick (drained into `source_block`
    // at tick end, applied next tick — one tick stale, imperceptible at kHz).
    if can_suppress {
        let mag = live_flick.map(|v| v.length()).unwrap_or(0.0);
        let past = mag >= deadzone.max(0.05);
        let do_block = measuring
            || sup_mode == "full"
            || (sup_mode == "deadzone" && flick_on && past);
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

/// Where the flick goes (`flick_output`): "mouse" — only the Flick pin, in mouse
/// counts (the default); "stick" — only the Flick pin, as stick deflection;
/// "both" — into the Mouse AND Stick outputs alongside the aim. "main" is the
/// old name for merging into the outputs.
pub fn rws_flick_mode(params: &HashMap<String, Value>) -> &'static str {
    match params.get("flick_output").and_then(|v| v.as_str()) {
        Some("stick") => "stick",
        Some("both") | Some("main") => "both",
        _ => "mouse",
    }
}

/// What RWS Aim does with the stick on its Flick input right now: `(flick,
/// stick_aim)`. Both need the "Flick On" control (input 2) — unwired is on; a
/// mode shift turns it off so the stick behaves normally again (a weapon wheel),
/// which also lifts the module's suppression of it. The flick is on only where
/// it reaches anything: merged into the outputs, or through a wired Flick pin
/// (`_rws_flick_out_wired`, stamped at build time). A flick going nowhere would
/// still take the outer stick range away from Stick aim.
pub fn rws_stick_uses(inputs: &[Option<Signal>], params: &HashMap<String, Value>) -> (bool, bool) {
    let on = match inputs.get(2).copied().flatten() {
        Some(Signal::Bool(b)) => b,
        Some(Signal::Float(f)) => f > 0.5,
        Some(Signal::Int(i)) => i != 0,
        _ => true,
    };
    let pin_wired = params.get("_rws_flick_out_wired").and_then(|v| v.as_bool()).unwrap_or(false);
    let flick = on && (rws_flick_mode(params) == "both" || pin_wired);
    let aim = on && params.get("stick_aim_enabled").and_then(|v| v.as_bool()).unwrap_or(false);
    (flick, aim)
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

// Measure-calibration state, after the flick slots in `NodeState::aux_f32`:
//   [6] measured rotation on the active axis (deg, signed — backtracking subtracts)
//   [7] active axis code (0 off, 1 pitch, 2 yaw) — a change restarts the count
//   [8] peak unclamped Stick deflection seen this sweep (>1 = stick saturated)
const MEAS_DEG: usize = 6;
const MEAS_AXIS: usize = 7;
const MEAS_PEAK_DEFL: usize = 8;
//   [9] flick rotation (deg) still owed to the Flick pin in stick mode — what a
//       full-deflection tick couldn't carry, paid out on the ticks after
const FLK_STICK_CARRY: usize = 9;
//   [10] tracked heading while engaged, unwrapped (rad, 0 at engage) — what the
//        stabiliser filters
//   [11] the stabiliser's previous output (rad), so only its change is turned
const FLK_STAB_RAW: usize = 10;
const FLK_STAB_OUT: usize = 11;

// Speed-gated flick (`flick_speed_ms`): every push of the Flick stick out of the
// centre is one GESTURE, classed by how fast it reaches the flick zone. Fast is
// a flick, and the Stick aim the push made on its way out is thrown away, so the
// flick lands clean. Slow is steering: the aim held back while deciding catches
// up over the smoothing time, and with `flick_slow_lock` the flick zone stays
// off until the stick is back at the centre — past the flick deadzone it just
// aims at full rate. Once a gesture has flicked, Stick aim stays off until the
// centre, so the stick springing back through the aim zone turns nothing.
//   [12] gesture: GST_NEUTRAL / GST_DECIDING / GST_STEERING / GST_FLICKED
//   [13] time since the stick left the centre (s) — counted only while deciding
//   [14..15] Stick aim held back while deciding (yaw, pitch; deg)
//   [16..17] held-back aim still catching up (yaw, pitch; deg)
//   [18] catch-up time left (s)
const GST: usize = 12;
const GST_T: usize = 13;
const GST_HELD: usize = 14;
const GST_CATCH: usize = 16;
const GST_CATCH_S: usize = 18;
const GST_NEUTRAL: f32 = 0.0;
const GST_DECIDING: f32 = 1.0;
const GST_STEERING: f32 = 2.0;
const GST_FLICKED: f32 = 3.0;
/// Out past this a gesture starts; back inside it, the gesture is over.
const GESTURE_OUT: f32 = 0.1;

/// Where RWS Aim's display-only measured angle lands in `last_out`: right after
/// the node's pins. Nodes saved before the Flick pin have two pins, newer ones
/// three, and the engine trims its output to match (see [`rws_fit_outputs`]).
pub fn rws_cal_deg_out(n_outputs: usize) -> usize {
    n_outputs.clamp(2, 3)
}
/// Where the sweep's peak unclamped stick deflection lands: after the angle.
pub fn rws_cal_peak_out(n_outputs: usize) -> usize {
    rws_cal_deg_out(n_outputs) + 1
}

/// Fit [`compute_rws`]'s full output to the node's pins: a node from before the
/// Flick pin has no slot for it, so it is dropped and the display-only entries
/// move up to sit right after the pins it does have.
pub(crate) fn rws_fit_outputs(mut out: Vec<Option<Signal>>, n_outputs: usize) -> Vec<Option<Signal>> {
    if n_outputs < 3 && out.len() > 2 {
        out.remove(2);
    }
    out
}

/// Compute one tick of the RWS module.
/// Outputs: [Mouse Vec2 (per-tick displacement), Stick Vec2 (right-stick
/// deflection, unit range), Flick Vec2 (the flick alone, when `flick_output`
/// sends it there — mouse counts or stick deflection), then two DISPLAY-ONLY
/// trailing entries no wire reads (wiring indexes by pin): measured calibration
/// degrees (Float), and the sweep's peak unclamped stick deflection (Float). The
/// UI reads them from `last_out` (same pattern as the Virtual Menu's open/hover
/// entries) at [`rws_cal_deg_out`] / [`rws_cal_peak_out`].
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

    let scale = pf("scale", 100.0);
    let mut rws = pf("rws", 1.0);
    // Full-deflection turn rate for the stick-aim (input 1). The Rotation input
    // (input 0) is always a gyro rate now.
    let max_rate = pf("max_rate_dps", 360.0);
    let (flick_on, stick_aim) = rws_stick_uses(inputs, params);
    let flick_deadzone = pf("flick_deadzone", 0.85);
    let flick_smooth_ms = pf("flick_smooth_ms", 100.0);
    let flick_fwd_dz = pf("flick_fwd_dz_deg", 0.0).clamp(0.0, 180.0);
    let flick_stab_s = pf("flick_stabilise_ms", 0.0).clamp(0.0, 1000.0) / 1000.0;
    let flick_output = rws_flick_mode(params);
    // Speed gate: how fast a push must reach the flick zone to flick (0 = off),
    // and whether a slower one keeps the flick zone off until the centre.
    let speed_s = pf("flick_speed_ms", 0.0).clamp(0.0, 2000.0) / 1000.0;
    let slow_lock = params.get("flick_slow_lock").and_then(|v| v.as_bool()).unwrap_or(false);
    // Vertical/horizontal sensitivity bias: horizontal (yaw) is the reference the
    // `scale` is calibrated on, so the ratio scales VERTICAL (pitch) only —
    // separately for the gyro source and stick sources. 1.0 = equal.
    let gyro_vh_ratio = pf("gyro_vh_ratio", 1.0).max(0.0);
    let stick_vh_ratio = pf("stick_vh_ratio", 1.0).max(0.0);
    // User-driven MEASURE calibration ("pitch"|"yaw"|"off"): the user turns the
    // camera a known amount (180° down→up, or 360°) while we drive the game at the
    // BASE scale (rws = 1, no V/H bias, off-axis blocked, no flick/stick-aim). The
    // physical rotation is integrated HERE at tick rate (exact dt, the one copy of
    // the node the engine evaluates) and published for the UI to back-solve.
    let cal_measure = ps("cal_measure", "off");
    let measuring = cal_measure == "pitch" || cal_measure == "yaw";

    // Rotation rate in deg/s (yaw = x, pitch = y). The Rotation input is a gyro
    // rate: ±1 == ±GYRO_REF_DPS deg/s.
    let (yaw_dps, pitch_dps) = {
        let rot = match inputs.first().and_then(|s| *s) {
            Some(Signal::Vec2(v)) => v,
            Some(Signal::Float(f)) => glam::Vec2::new(f, 0.0),
            _ => glam::Vec2::ZERO,
        };
        let k = GYRO_REF_DPS;
        if measuring {
            // Drive only the axis being measured at the BASE scale (no V/H bias),
            // so the UI's integral back-solves the ground-truth constant.
            rws = 1.0;
            match cal_measure.as_str() {
                "pitch" => {
                    state.rws_cal_flick = None;
                    (0.0, rot.y * k)
                }
                _ => {
                    // "yaw": the 360° can also be turned by circling the stick on
                    // the Flick input round its edge (as JSM's sweep does) — the
                    // stick for the bulk, the pad to line up. Each degree swept
                    // turns the game one degree at the base scale, just as a degree
                    // of pad rotation does, so the two add. eval_rws_node blocks
                    // the stick from its default mapping for the sweep.
                    let stick = match inputs.get(1).and_then(|s| *s) {
                        Some(Signal::Vec2(v)) => v,
                        _ => glam::Vec2::ZERO,
                    };
                    let flick = state.rws_cal_flick.get_or_insert_with(Default::default);
                    (rot.x * k + flick.rate([glam::Vec2::ZERO, stick], dt), 0.0)
                }
            }
        } else {
            state.rws_cal_flick = None;
            // Apply the gyro V/H bias to the vertical (pitch) axis.
            (rot.x * k, rot.y * k * gyro_vh_ratio)
        }
    };

    // Integrate the measured axis. Signed, so turning back (repositioning, or
    // correcting an overshoot) subtracts. Restart whenever the sweep (re)starts or
    // switches axis; hold the value while off so a Finish read lands intact.
    while state.aux_f32.len() <= GST_CATCH_S {
        state.aux_f32.push(0.0);
    }
    let axis_code = match cal_measure.as_str() {
        "pitch" => 1.0,
        "yaw" => 2.0,
        _ => 0.0,
    };
    if measuring {
        if state.aux_f32[MEAS_AXIS] != axis_code {
            state.aux_f32[MEAS_DEG] = 0.0;
            state.aux_f32[MEAS_PEAK_DEFL] = 0.0;
            state.aux_f32[MEAS_AXIS] = axis_code;
        }
        let axis_dps = if axis_code == 1.0 { pitch_dps } else { yaw_dps };
        state.aux_f32[MEAS_DEG] += axis_dps * dt;
    } else {
        state.aux_f32[MEAS_AXIS] = 0.0;
    }

    // Flick-stick yaw contribution (degrees). It is a REAL rotation, so it maps
    // 1:1 through `scale` only — `rws` (a feel multiplier) deliberately does NOT
    // apply, so a flick lands on the direction you point regardless of RWS.
    let flick_stick = match inputs.get(1).and_then(|s| *s) {
        Some(Signal::Vec2(v)) => v,
        _ => glam::Vec2::ZERO,
    };
    let gated = flick_on && speed_s > 0.0 && !measuring;
    let catch_s = (flick_smooth_ms / 1000.0).max(0.0);
    gesture_before_flick(state, gated, flick_stick.length(), speed_s, catch_s, dt);
    let allow_flick = !(gated && slow_lock && state.aux_f32[GST] == GST_STEERING);

    let flick_deg = if flick_on && !measuring {
        compute_flick(inputs, state, flick_deadzone, flick_smooth_ms, flick_fwd_dz, flick_stab_s, allow_flick, dt)
    } else {
        reset_flick(state);
        0.0
    };
    if gated {
        gesture_after_flick(state, flick_stick.length());
    }

    let stick_max = pf("stick_out_dps", 360.0).max(1.0);

    // The flick as stick deflection (the "stick" and "both" modes). A stick sets
    // a turn RATE capped at full tilt, so whatever a tick can't carry is owed to
    // the next ones: the total turned is conserved and the flick still lands
    // where the stick points, just over a longer time.
    let flick_defl = if flick_output == "mouse" {
        state.aux_f32[FLK_STICK_CARRY] = 0.0;
        0.0
    } else {
        let owed = state.aux_f32[FLK_STICK_CARRY] + flick_deg;
        let cap = stick_max * dt;
        if cap > 0.0 {
            let emit = owed.clamp(-cap, cap);
            state.aux_f32[FLK_STICK_CARRY] = owed - emit;
            emit / cap
        } else {
            state.aux_f32[FLK_STICK_CARRY] = owed;
            0.0
        }
    };
    // The Flick pin carries it alone; "both" merges it into the outputs instead.
    let flick_pin = match flick_output {
        "mouse" => glam::Vec2::new(flick_deg * scale, 0.0),
        "stick" => glam::Vec2::new(flick_defl, 0.0),
        _ => glam::Vec2::ZERO,
    };
    let (flick_to_mouse, flick_to_stick) = if flick_output == "both" { (flick_deg, flick_defl) } else { (0.0, 0.0) };

    // Mouse output: per-tick displacement in mouse counts (scale = counts/degree).
    // Right-stick output: the desired turn RATE (rws applied) normalized by the
    // game's full-deflection turn rate, clamped below to the unit range. Both
    // accumulate the primary source first, then the optional stick-aim.
    let mut dx = yaw_dps * dt * scale * rws + flick_to_mouse * scale;
    let mut dy = pitch_dps * dt * scale * rws;
    let mut sx = yaw_dps * rws / stick_max + flick_to_stick;
    let mut sy = pitch_dps * rws / stick_max;

    // Stick-aim: the stick wired to the Flick input drives BOTH outputs as a rate
    // aim with its own RWS. When flick is ALSO enabled it is active only INSIDE the
    // flick deadzone (past → the flick takes over); with flick off it uses the full
    // stick range. Its vertical axis honours the stick V/H bias, and eval_rws_node
    // suppresses the stick from its default mapping so it doesn't double-drive.
    //
    // With the speed gate on, the gesture decides what the aim does: held back
    // while deciding, nothing once the gesture has flicked, and — when a slow
    // push locks the flick zone — the full stick range while steering.
    let gesture = if gated { state.aux_f32[GST] } else { GST_NEUTRAL };
    if stick_aim && !measuring && gesture != GST_FLICKED {
        let v = flick_stick;
        let m = v.length();
        let dz = flick_deadzone.clamp(0.05, 0.99);
        let locked = gated && slow_lock && gesture == GST_STEERING;
        let (active, t) = if flick_on && !locked {
            // Ramp 0→full across the deadzone; past it belongs to the flick.
            (m > 1e-4 && m < dz, (m / dz).clamp(0.0, 1.0))
        } else if flick_on {
            // Steering with the flick zone locked: the same ramp, then full rate
            // out past the deadzone.
            (m > 1e-4, (m / dz).clamp(0.0, 1.0))
        } else {
            // No flick: the full stick deflection is the rate.
            (m > 1e-4, m.clamp(0.0, 1.0))
        };
        if active {
            let aim_rws = pf("stick_aim_rws", 1.0);
            let dir = v / m;
            let a_yaw = dir.x * t * max_rate * aim_rws;
            let a_pitch = dir.y * t * max_rate * stick_vh_ratio * aim_rws;
            if gesture == GST_DECIDING {
                state.aux_f32[GST_HELD] += a_yaw * dt;
                state.aux_f32[GST_HELD + 1] += a_pitch * dt;
            } else {
                dx += a_yaw * dt * scale;
                dy += a_pitch * dt * scale;
                sx += a_yaw / stick_max;
                sy += a_pitch / stick_max;
            }
        }
    }

    // Held-back aim released by a steering gesture, paid out over the smoothing
    // time — a rotation, so into both outputs like the aim it stands in for.
    let catch = gesture_catch_up(state, dt);
    if catch != glam::Vec2::ZERO {
        dx += catch.x * scale;
        dy += catch.y * scale;
        if dt > 0.0 {
            sx += catch.x / dt / stick_max;
            sy += catch.y / dt / stick_max;
        }
    }

    // Peak UNclamped stick deflection during a sweep: past 1.0 the stick is pinned
    // at full tilt, so the game turns slower than the gyro and a Stick back-solve
    // would come out wrong — the UI refuses it and asks for a slower turn.
    if measuring {
        let defl = sx.abs().max(sy.abs());
        if defl > state.aux_f32[MEAS_PEAK_DEFL] {
            state.aux_f32[MEAS_PEAK_DEFL] = defl;
        }
    }

    let mut sx = sx.clamp(-1.0, 1.0);
    let mut sy = sy.clamp(-1.0, 1.0);

    // While MEASURING, drive ONLY the output being calibrated (`cal_output`) so a
    // patch that wires BOTH outputs (with selectors deciding which is live) can't
    // move the game via the wrong one and skew the back-solve.
    if measuring {
        if ps("cal_output", "mouse") == "stick" {
            dx = 0.0;
            dy = 0.0;
        } else {
            sx = 0.0;
            sy = 0.0;
        }
    }

    vec![
        Some(Signal::Vec2(glam::Vec2::new(dx, dy))),
        Some(Signal::Vec2(glam::Vec2::new(sx, sy))),
        Some(Signal::Vec2(flick_pin)),
        // Display-only (not a pin): see the doc comment above.
        Some(Signal::Float(state.aux_f32[MEAS_DEG])),
        Some(Signal::Float(state.aux_f32[MEAS_PEAK_DEFL])),
    ]
}

/// One tick of flick-stick, returning the yaw rotation to apply in DEGREES.
/// Input 1 is the stick position (Vec2, up = +y). Crossing `deadzone` outward
/// engages: the camera flicks by the stick's angle from forward (smoothed over
/// `smooth_ms`). While engaged, further rotation of the stick tracks the camera
/// 1:1 (shortest arc). Dropping back inside the deadzone disengages.
///
/// `fwd_dz_deg` is the forward deadzone: a flick that engages within that many
/// degrees of straight up doesn't snap at all — it engages and tracks from
/// there, so a shallow push leaves the camera still and primes the stick for a
/// rotation (turning a full 360° with the stick, say).
///
/// `allow_engage` false keeps a new flick from engaging (a steering gesture
/// with the flick zone locked — see the speed gate).
///
/// `stab_s` stabilises the tracking: the tracked heading runs through a
/// one-euro filter (the Gyro to Stick Rotation's), which holds back a thumb's
/// tremor while a deliberate sweep opens it up. Whatever it is still holding
/// back when the flick lets go is turned then, so the camera still ends where
/// the stick pointed. 0 = none.
fn compute_flick(
    inputs: &[Option<Signal>],
    state: &mut NodeState,
    deadzone: f32,
    smooth_ms: f32,
    fwd_dz_deg: f32,
    stab_s: f32,
    allow_engage: bool,
    dt: f32,
) -> f32 {
    while state.aux_f32.len() <= FLK_STAB_OUT {
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
        // Coming back from a dip below the release floor (still engaged — a
        // dropout is held, see below).
        let dipped = state.aux_f32[FLK_BELOW_S] > 0.0;
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
            // Back from the dip on the far side of the centre: a released stick
            // springing past it, not a turn — tracking it turned a 180° flick
            // straight back. The heading is taken from here, unturned. (A dropout
            // comes back where it was, and a real sweep never dips.)
            if dipped && d.abs() > std::f32::consts::FRAC_PI_2 {
                d = 0.0;
            }
            if stab_s > 0.0 {
                state.aux_f32[FLK_STAB_RAW] += d;
            } else {
                state.aux_f32[FLK_TRACK_PENDING] += d.to_degrees();
            }
            state.aux_f32[FLK_PREV_ANGLE] = angle;
        } else if mag >= engage_dz && allow_engage {
            // Engage: flick to the stick's heading from forward (up), clockwise
            // positive so pushing RIGHT flicks the camera right (positive yaw).
            state.aux_f32[FLK_ENGAGED] = 1.0;
            state.aux_f32[FLK_PREV_ANGLE] = angle;
            state.aux_f32[FLK_STAB_RAW] = 0.0;
            state.aux_f32[FLK_STAB_OUT] = 0.0;
            state.rws_flick_stab = None;
            let flick_deg = angle.to_degrees();
            // Inside the forward deadzone: engaged and tracking, but no snap.
            let flick_deg = if flick_deg.abs() <= fwd_dz_deg { 0.0 } else { flick_deg };
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
            // Let go: turn whatever the stabiliser was still holding back.
            let held = state.aux_f32[FLK_STAB_RAW] - state.aux_f32[FLK_STAB_OUT];
            state.aux_f32[FLK_TRACK_PENDING] += held.to_degrees();
            state.aux_f32[FLK_STAB_RAW] = 0.0;
            state.aux_f32[FLK_STAB_OUT] = 0.0;
            state.rws_flick_stab = None;
        }
    }

    // Stabilised tracking: filter the tracked heading every tick while engaged
    // (also between stick polls, so the filter settles onto a held heading) and
    // turn by its change.
    if stab_s > 0.0 && state.aux_f32[FLK_ENGAGED] > 0.5 {
        let raw = state.aux_f32[FLK_STAB_RAW];
        let filt = state.rws_flick_stab.get_or_insert_with(Default::default);
        let out = filt.filter(raw, dt, 1.0 / (std::f32::consts::TAU * stab_s));
        state.aux_f32[FLK_TRACK_PENDING] += (out - state.aux_f32[FLK_STAB_OUT]).to_degrees();
        state.aux_f32[FLK_STAB_OUT] = out;
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

/// The speed gate's gesture, before the flick runs: start a gesture as the
/// stick leaves the centre, and decide a slow one (out of time, or back at the
/// centre without reaching the flick zone) as steering — releasing the aim held
/// back meanwhile to catch up. With the gate off, any held aim is released and
/// the gesture forgotten.
fn gesture_before_flick(state: &mut NodeState, gated: bool, m: f32, speed_s: f32, catch_s: f32, dt: f32) {
    let release = |state: &mut NodeState| {
        for i in 0..2 {
            state.aux_f32[GST_CATCH + i] += state.aux_f32[GST_HELD + i];
            state.aux_f32[GST_HELD + i] = 0.0;
        }
        state.aux_f32[GST_CATCH_S] = catch_s;
    };
    if !gated {
        if state.aux_f32[GST_HELD] != 0.0 || state.aux_f32[GST_HELD + 1] != 0.0 {
            release(state);
        }
        state.aux_f32[GST] = GST_NEUTRAL;
        return;
    }
    let g = state.aux_f32[GST];
    if g == GST_NEUTRAL {
        if m >= GESTURE_OUT {
            state.aux_f32[GST] = GST_DECIDING;
            state.aux_f32[GST_T] = 0.0;
        }
    } else if g == GST_DECIDING {
        state.aux_f32[GST_T] += dt;
        if m < GESTURE_OUT {
            // A quick nudge that never got to the flick zone: aim, not a flick.
            release(state);
            state.aux_f32[GST] = GST_NEUTRAL;
        } else if state.aux_f32[GST_T] >= speed_s {
            release(state);
            state.aux_f32[GST] = GST_STEERING;
        }
    } else if g == GST_STEERING && m < GESTURE_OUT {
        state.aux_f32[GST] = GST_NEUTRAL;
    }
}

/// The speed gate's gesture, after the flick runs: a flick that engaged makes
/// the gesture a flick and throws away the aim held back on the way out; a
/// flicked gesture is over once the flick has let go and the stick is back at
/// the centre (not before — the stick springing back through the aim zone is
/// still part of it).
fn gesture_after_flick(state: &mut NodeState, m: f32) {
    let engaged = state.aux_f32[FLK_ENGAGED] > 0.5;
    let g = state.aux_f32[GST];
    if engaged && g != GST_FLICKED {
        state.aux_f32[GST] = GST_FLICKED;
        state.aux_f32[GST_HELD] = 0.0;
        state.aux_f32[GST_HELD + 1] = 0.0;
    } else if g == GST_FLICKED && !engaged && m < GESTURE_OUT {
        state.aux_f32[GST] = GST_NEUTRAL;
    }
}

/// One tick of the released aim's catch-up (yaw, pitch; deg), spread over the
/// time it was given and conserving the total.
fn gesture_catch_up(state: &mut NodeState, dt: f32) -> glam::Vec2 {
    let owed = glam::Vec2::new(state.aux_f32[GST_CATCH], state.aux_f32[GST_CATCH + 1]);
    if owed == glam::Vec2::ZERO {
        return owed;
    }
    let t = state.aux_f32[GST_CATCH_S];
    let emit = if t <= dt { owed } else { owed * (dt / t) };
    state.aux_f32[GST_CATCH] -= emit.x;
    state.aux_f32[GST_CATCH + 1] -= emit.y;
    state.aux_f32[GST_CATCH_S] = (t - dt).max(0.0);
    emit
}

/// Clear flick state so the next engage starts clean (used when flick is
/// disabled or while calibrating).
fn reset_flick(state: &mut NodeState) {
    for &i in &[
        FLK_ENGAGED, FLK_PENDING_DEG, FLK_PAYOUT_S, FLK_BELOW_S, FLK_TRACK_PENDING, FLK_STICK_CARRY,
        FLK_STAB_RAW, FLK_STAB_OUT,
    ] {
        if let Some(v) = state.aux_f32.get_mut(i) {
            *v = 0.0;
        }
    }
    state.rws_flick_stab = None;
}
