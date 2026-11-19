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
///   movements inside the deadzone still reach the default mapping (flick only);
///   not while a slow push has the flick zone shut (`flick_slow_lock`), as the
///   stick isn't flicking then and may be meant for something else.
///
/// The Flick input can be a stick, the D-pad or a trackpad's `touchN_xy`; a
/// trackpad is blocked as its whole touch field, and read from its Vec2 alone,
/// since its x and y hold where the last touch was.
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
    dev_sigs: &BusMap,
    collector_sigs: &mut BusMap,
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
    let unblocked: flexinput_core::SigMap = state
        .get(&MACRO_CARRY_UID)
        .map(|s| s.unblocked_src.clone())
        .unwrap_or_default();
    let sig_f32 = |s: &Signal| if let Signal::Float(f) = s { Some(*f) } else { None };
    let touch = flexinput_core::automap::TOUCH_VECTOR_PINS.iter().find(|p| p[0] == stick);
    let vec_of = |pin: &str| match unblocked.get_pin(&dev_id, &pin) {
        Some(Signal::Vec2(v)) => Some(*v),
        _ => None,
    };
    let flick_snapshot = if !can_suppress {
        None
    } else if touch.is_some() {
        vec_of(&stick)
    } else {
        let x = unblocked.get_pin(&dev_id, &format!("{stick}_x")).and_then(sig_f32);
        let y = unblocked.get_pin(&dev_id, &format!("{stick}_y")).and_then(sig_f32);
        match (x, y) {
            (Some(x), Some(y)) => Some(glam::Vec2::new(x, y)),
            _ => vec_of(&stick),
        }
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

    start_bound_turns(snap, uid, state);
    let node_state = state.entry(uid).or_default();
    let out = rws_fit_outputs(compute_rws(&corrected, node_state, &snap.params, dt), snap.n_outputs);
    publish_native_gyro(snap, uid, dev_sigs, collector_sigs);

    // Publish the source-block for the flick stick (drained into `source_block`
    // at tick end, applied next tick — one tick stale, imperceptible at kHz).
    if can_suppress {
        let mag = live_flick.map(|v| v.length()).unwrap_or(0.0);
        let past = mag >= deadzone.max(0.05);
        let do_block = measuring
            || sup_mode == "full"
            || (sup_mode == "deadzone" && flick_on && past && !rws_flick_zone_shut(state.get(&uid), &snap.params));
        if do_block {
            let bk = format!("{SRC_BLOCK_PREFIX}{dev_id}");
            if let Some(field) = touch {
                for p in field {
                    collector_sigs.set_pin(&bk, p, Signal::Bool(true));
                }
            } else {
                for p in [
                    stick.clone(),
                    format!("{stick}_x"),
                    format!("{stick}_y"),
                    format!("{stick}_up"),
                    format!("{stick}_down"),
                    format!("{stick}_left"),
                    format!("{stick}_right"),
                ] {
                    collector_sigs.set_pin(&bk, &p, Signal::Bool(true));
                }
            }
        }
    }
    out
}

/// Whether a slow push has the flick zone shut this tick: the speed gate is on with
/// `flick_slow_lock`, and the push in hand was too slow to flick.
fn rws_flick_zone_shut(state: Option<&NodeState>, params: &HashMap<String, Value>) -> bool {
    let lock = params.get("flick_slow_lock").and_then(|v| v.as_bool()).unwrap_or(false);
    let gated = params.get("flick_speed_ms").and_then(|v| v.as_f64()).unwrap_or(0.0) > 0.0;
    lock && gated && state.and_then(|s| s.aux_f32.get(GST)).copied() == Some(GST_STEERING)
}

/// One turn a card asked for (see `flexinput_core::flick`), still paying out.
#[derive(Clone, Copy, Debug)]
pub struct RwsTurn {
    /// Degrees still to turn; positive is right.
    deg: f32,
    /// Seconds left to turn them in.
    left_s: f32,
    via: flexinput_core::flick::FlickVia,
}

/// With `flick_target` ticked, start a turn for each flick pin a card has just
/// pressed. Read from last tick's macro snapshot rather than this tick's bus, so
/// it doesn't matter which side of this node the card evaluates on: a turn starts
/// one tick after the press, imperceptible at kHz. Two cards holding the same turn
/// at once are one press of it.
fn start_bound_turns(snap: &NodeSnap, uid: usize, state: &mut HashMap<usize, NodeState>) {
    use flexinput_core::flick::{is_flick_pin, FlickPin, TARGET_PARAM};
    use flexinput_core::macros::SIGS_NS;
    if !snap.params.get(TARGET_PARAM).and_then(|v| v.as_bool()).unwrap_or(false) {
        if let Some(ns) = state.get_mut(&uid) {
            ns.rws_turn_held.clear();
        }
        return;
    }
    // Nothing is allocated for a tick with no flick held: this runs on every one.
    let held: HashSet<String> = state.get(&MACRO_CARRY_UID)
        .map(|c| c.macro_prev.iter()
            .filter(|((ns, pin), sig)| *ns == SIGS_NS && is_flick_pin(pin) && sig_magnitude(**sig) >= 0.5)
            .map(|((_, pin), _)| pin.to_string())
            .collect())
        .unwrap_or_default();
    let ns = state.entry(uid).or_default();
    if held.is_empty() && ns.rws_turn_held.is_empty() {
        return;
    }
    let smooth_ms = snap.params.get("flick_smooth_ms").and_then(|v| v.as_f64()).unwrap_or(100.0) as f32;
    for pin in held.iter().filter(|p| !ns.rws_turn_held.contains(*p)) {
        let Some(p) = FlickPin::parse(pin) else { continue };
        ns.rws_turns.push(RwsTurn {
            deg: p.deg as f32,
            left_s: p.ms.map_or(smooth_ms, |ms| ms as f32).max(0.0) / 1000.0,
            via: p.via,
        });
    }
    ns.rws_turn_held = held;
}

/// One tick of the turns in flight: the degrees to turn now through the mouse,
/// and the degrees owed to the stick, added to what it couldn't carry before.
/// Linear, as a flick's own pay-out is, and each turn conserved exactly.
fn tick_bound_turns(state: &mut NodeState, dt: f32) -> (f32, f32) {
    use flexinput_core::flick::FlickVia;
    let (mut mouse, mut stick) = (0.0, 0.0);
    state.rws_turns.retain_mut(|t| {
        let emit = if t.left_s <= dt { t.deg } else { t.deg * (dt / t.left_s) };
        t.deg -= emit;
        t.left_s = (t.left_s - dt).max(0.0);
        match t.via {
            FlickVia::Mouse => mouse += emit,
            FlickVia::Stick => stick += emit,
        }
        t.left_s > 0.0
    });
    (mouse, stick)
}

/// How much of `owed` degrees of stick turn fits on a stick already pushed to
/// `at` this tick, as deflection to add, and what's left owing. A stick sets a turn
/// rate capped at full tilt (`cap` degrees a tick), so a turn it can't carry yet
/// takes longer; it still comes out to the degree.
fn stick_turn_fit(owed: f32, at: f32, cap: f32) -> (f32, f32) {
    if owed == 0.0 || cap <= 0.0 {
        return (0.0, owed);
    }
    let room = (1.0 - at * owed.signum()).max(0.0) * cap;
    let emit = owed.abs().min(room) * owed.signum();
    (emit / cap, owed - emit)
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
const GESTURE_OUT: f32 = RWS_GESTURE_OUT;
/// Where a push of the flick stick starts, for the speed gate: the flick circle draws it.
pub const RWS_GESTURE_OUT: f32 = 0.1;
//   [19] bound turn (deg) still owed to a stick output — what a full-deflection
//        tick couldn't carry, paid out on the ticks after
const TURN_STICK_CARRY: usize = 19;
// For the body's flick circle only (see `RwsFlickView`):
//   [20] how far this flick has turned the aim since it engaged (deg): the snap
//        plus the tracking, kept after it lets go until the next one
//   [21] 1 when this flick engaged inside the forward deadzone, so didn't snap
const FLK_TURNED: usize = 20;
const FLK_NO_SNAP: usize = 21;

/// Where RWS Aim's display-only measured angle lands in `last_out`: right after
/// the node's pins. Nodes saved before the Flick pin have two pins, before the
/// Gyro pin three, newer ones four, and the engine trims its output to match (see
/// [`rws_fit_outputs`]).
pub fn rws_cal_deg_out(n_outputs: usize) -> usize {
    n_outputs.clamp(2, 4)
}
/// Where the sweep's peak unclamped stick deflection lands: after the angle.
pub fn rws_cal_peak_out(n_outputs: usize) -> usize {
    rws_cal_deg_out(n_outputs) + 1
}
/// The release floor of a flick engaged at `deadzone`: once out, it keeps tracking
/// the stick down to here, and lets go only after the stick has stayed under it
/// for a moment (a Bluetooth gap reads as a dip, not a release).
pub fn rws_flick_release_floor(deadzone: f32) -> f32 {
    (deadzone.clamp(0.05, 0.99) * 0.5).clamp(0.15, 0.7)
}
/// Where the flick circle's live state starts in `last_out`: two display-only
/// `Vec4`s after the peak deflection. Read them with [`rws_flick_view`].
pub fn rws_flick_view_out(n_outputs: usize) -> usize {
    rws_cal_peak_out(n_outputs) + 1
}

/// Where a push of the flick stick is in the speed gate (`flick_speed_ms`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum RwsGesture {
    /// At the centre, or the gate is off.
    #[default]
    Neutral,
    /// Out of the centre, the clock running: a flick if it reaches the flick zone in time.
    Deciding,
    /// Too slow to flick: steering (with `flick_slow_lock`, the flick zone is shut).
    Steering,
    /// It flicked: Stick aim stays off until the stick is back at the centre.
    Flicked,
}

/// What the flick stick is doing this tick, for the body's flick circle.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct RwsFlickView {
    /// The stick on the Flick input, as the module reads it (up = +y).
    pub stick: glam::Vec2,
    /// A flick is engaged: the stick is out and the aim follows it round.
    pub engaged: bool,
    /// The stick's heading while engaged, degrees clockwise from straight up. Held
    /// through a dip below the release floor.
    pub heading_deg: f32,
    /// How far this flick has turned the aim (deg): the snap plus the tracking.
    pub turned_deg: f32,
    /// Of `turned_deg`, what the outputs haven't turned yet: the snap still being
    /// smoothed in, tracking held back by the stabiliser.
    pub owed_deg: f32,
    /// This flick engaged inside the forward deadzone, so it didn't snap.
    pub no_snap: bool,
    /// The stick has dipped below the release floor; the flick lets go if it stays.
    pub dipping: bool,
    /// The flick reaches something and the Flick On input has it on.
    pub flick_on: bool,
    /// Stick aim is on (and the Flick On input has it on).
    pub aim_on: bool,
    /// A calibration sweep has the stick.
    pub measuring: bool,
    pub gesture: RwsGesture,
    /// Seconds since the stick left the centre, while deciding.
    pub gate_s: f32,
}

/// The flick circle's live state from a node's `last_out` (see [`rws_flick_view_out`]).
pub fn rws_flick_view(out: &[Option<Signal>], n_outputs: usize) -> Option<RwsFlickView> {
    let at = rws_flick_view_out(n_outputs);
    let (Some(Signal::Vec4(a)), Some(Signal::Vec4(b))) = (out.get(at).copied().flatten(), out.get(at + 1).copied().flatten()) else {
        return None;
    };
    let flags = b.x as u32;
    Some(RwsFlickView {
        stick: glam::Vec2::new(a.x, a.y),
        heading_deg: a.z,
        turned_deg: a.w,
        engaged: flags & 1 != 0,
        flick_on: flags & 2 != 0,
        aim_on: flags & 4 != 0,
        measuring: flags & 8 != 0,
        no_snap: flags & 16 != 0,
        dipping: flags & 32 != 0,
        gesture: match b.y as u32 {
            1 => RwsGesture::Deciding,
            2 => RwsGesture::Steering,
            3 => RwsGesture::Flicked,
            _ => RwsGesture::Neutral,
        },
        gate_s: b.z,
        owed_deg: b.w,
    })
}

impl RwsFlickView {
    /// The two `Vec4`s [`rws_flick_view`] reads back.
    pub fn encode(&self) -> [Signal; 2] {
        let flags = u32::from(self.engaged)
            | u32::from(self.flick_on) << 1
            | u32::from(self.aim_on) << 2
            | u32::from(self.measuring) << 3
            | u32::from(self.no_snap) << 4
            | u32::from(self.dipping) << 5;
        let gesture = match self.gesture {
            RwsGesture::Neutral => 0.0,
            RwsGesture::Deciding => 1.0,
            RwsGesture::Steering => 2.0,
            RwsGesture::Flicked => 3.0,
        };
        [
            Signal::Vec4(glam::Vec4::new(self.stick.x, self.stick.y, self.heading_deg, self.turned_deg)),
            Signal::Vec4(glam::Vec4::new(flags as f32, gesture, self.gate_s, self.owed_deg)),
        ]
    }
}

/// The flick circle's state this tick. `stick_only` is a flick that goes out as stick
/// deflection alone, whose turn waits on the stick's top speed; merged into the outputs too,
/// the mouse has already turned it.
fn flick_view_now(state: &NodeState, stick: glam::Vec2, flick_on: bool, aim_on: bool, measuring: bool, stick_only: bool) -> RwsFlickView {
    let s = &state.aux_f32;
    let engaged = s[FLK_ENGAGED] > 0.5;
    let carry = if stick_only { s[FLK_STICK_CARRY] } else { 0.0 };
    let gesture = match s[GST] {
        g if g == GST_DECIDING => RwsGesture::Deciding,
        g if g == GST_STEERING => RwsGesture::Steering,
        g if g == GST_FLICKED => RwsGesture::Flicked,
        _ => RwsGesture::Neutral,
    };
    RwsFlickView {
        stick,
        engaged,
        heading_deg: s[FLK_PREV_ANGLE].to_degrees(),
        turned_deg: s[FLK_TURNED],
        owed_deg: s[FLK_PENDING_DEG] + s[FLK_TRACK_PENDING] + (s[FLK_STAB_RAW] - s[FLK_STAB_OUT]).to_degrees() + carry,
        no_snap: s[FLK_NO_SNAP] > 0.5,
        dipping: engaged && s[FLK_BELOW_S] > 0.0,
        flick_on,
        aim_on,
        measuring,
        gate_s: if gesture == RwsGesture::Deciding { s[GST_T] } else { 0.0 },
        gesture,
    }
}

/// The gains the Map output puts on the Device bus's gyro axes: `(roll and yaw, pitch)`.
/// `gyro_scale` is the calibrated constant - one pad degree, one game degree through the
/// game's own gyro aim - and Real World Scale (`rws`) the feel on top of it, as it is for
/// the mouse; the gyro V/H bias scales pitch. A calibration sweep drives the game at the
/// base scale through the output being calibrated only: the gyro at `gyro_scale` alone for
/// a Gyro sweep, and held still for a Mouse or Stick one.
pub fn rws_native_gyro_gain(params: &HashMap<String, Value>) -> (f32, f32) {
    let pf = |k: &str, d: f32| params.get(k).and_then(|v| v.as_f64()).map(|v| v as f32).unwrap_or(d);
    let gs = pf("gyro_scale", 1.0).max(0.0);
    let measuring = matches!(params.get("cal_measure").and_then(|v| v.as_str()), Some("pitch" | "yaw"));
    if measuring {
        let gyro = params.get("cal_output").and_then(|v| v.as_str()) == Some("gyro");
        return if gyro { (gs, gs) } else { (0.0, 0.0) };
    }
    let k = gs * pf("rws", 1.0);
    (k, k * pf("gyro_vh_ratio", 1.0).max(0.0))
}

/// The Map output: the Device input's bus handed on under `collector:{uid}`, its gyro
/// scaled by [`rws_native_gyro_gain`] - for a game that aims with the pad's own gyro. Does
/// nothing while the Device input is unwired (nothing to hand on).
fn publish_native_gyro(snap: &NodeSnap, uid: usize, dev_sigs: &BusMap, collector_sigs: &mut BusMap) {
    let wired = ["_automap_device_id", "_automap_collector_id"]
        .iter()
        .any(|k| snap.params.get(*k).and_then(|v| v.as_str()).is_some_and(|s| !s.is_empty()));
    if !wired {
        return;
    }
    republish_bus_as_collector(snap, uid, dev_sigs, collector_sigs);
    let (roll_yaw, pitch) = rws_native_gyro_gain(&snap.params);
    let key = format!("collector:{uid}");
    for (pin, k) in [("gyro_x", roll_yaw), ("gyro_y", pitch), ("gyro_z", roll_yaw)] {
        if let Some(v) = collector_sigs.get_pin(&key, &pin).map(|s| s.as_float()) {
            collector_sigs.set_pin(&key, &pin, Signal::Float(v * k));
        }
    }
}

/// Fit [`compute_rws`]'s full output to the node's pins: a node from before the
/// Gyro pin (or the Flick pin) has no slot for it, so it is dropped and the
/// display-only entries move up to sit right after the pins it does have.
pub(crate) fn rws_fit_outputs(mut out: Vec<Option<Signal>>, n_outputs: usize) -> Vec<Option<Signal>> {
    if n_outputs < 4 && out.len() > 3 {
        out.remove(3);
    }
    if n_outputs < 3 && out.len() > 2 {
        out.remove(2);
    }
    out
}

/// Compute one tick of the RWS module.
/// Outputs: [Mouse Vec2 (per-tick displacement), Stick Vec2 (right-stick
/// deflection, unit range), Flick Vec2 (the flick alone, when `flick_output`
/// sends it there — mouse counts or stick deflection), the Map slot (an AutoMap
/// pin: its bus rides in `collector_sigs`, see [`publish_native_gyro`]), then
/// DISPLAY-ONLY trailing entries no wire reads (wiring indexes by pin): measured
/// calibration degrees (Float), the sweep's peak unclamped stick deflection
/// (Float), and the flick circle's two Vec4s. The UI reads them from `last_out`
/// (same pattern as the Virtual Menu's open/hover entries) at
/// [`rws_cal_deg_out`] / [`rws_cal_peak_out`] / [`rws_flick_view`].
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
    while state.aux_f32.len() <= FLK_NO_SNAP {
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
    let mut flick_pin = match flick_output {
        "mouse" => glam::Vec2::new(flick_deg * scale, 0.0),
        "stick" => glam::Vec2::new(flick_defl, 0.0),
        _ => glam::Vec2::ZERO,
    };
    // Turns cards asked for, through the mouse or a stick as each one says, and
    // by the road this module's flicks take: a wired Flick pin of that kind if
    // there is one (it may bypass a selector gating the aim), else the main
    // output of that kind. 1:1 through `scale`, like a flick. A sweep has no
    // business turning anything but the sweep.
    if measuring {
        state.rws_turns.clear();
        state.aux_f32[TURN_STICK_CARRY] = 0.0;
    }
    let (turn_mouse, turn_stick) = tick_bound_turns(state, dt);
    let pin_wired = params.get("_rws_flick_out_wired").and_then(|v| v.as_bool()).unwrap_or(false);
    let turn_via_pin = |kind: &str| pin_wired && flick_output == kind;
    let mut turn_to_mouse = turn_mouse;
    if turn_via_pin("mouse") {
        flick_pin.x += turn_mouse * scale;
        turn_to_mouse = 0.0;
    }
    state.aux_f32[TURN_STICK_CARRY] += turn_stick;
    if turn_via_pin("stick") {
        let (defl, owed) = stick_turn_fit(state.aux_f32[TURN_STICK_CARRY], flick_pin.x, stick_max * dt);
        flick_pin.x += defl;
        state.aux_f32[TURN_STICK_CARRY] = owed;
    }
    let (flick_to_mouse, flick_to_stick) = if flick_output == "both" { (flick_deg, flick_defl) } else { (0.0, 0.0) };

    // Mouse output: per-tick displacement in mouse counts (scale = counts/degree).
    // Right-stick output: the desired turn RATE (rws applied) normalized by the
    // game's full-deflection turn rate, clamped below to the unit range. Both
    // accumulate the primary source first, then the optional stick-aim.
    let mut dx = (yaw_dps * dt * rws + flick_to_mouse + turn_to_mouse) * scale;
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

    // A bound turn with no Flick pin to take it: last onto the Stick output, so it
    // gets whatever deflection the aim has left and none of it clips away.
    if !turn_via_pin("stick") {
        let (defl, owed) = stick_turn_fit(state.aux_f32[TURN_STICK_CARRY], sx, stick_max * dt);
        sx += defl;
        state.aux_f32[TURN_STICK_CARRY] = owed;
    }

    // While MEASURING, drive ONLY the output being calibrated (`cal_output`) so a
    // patch that wires several outputs (with selectors deciding which is live)
    // can't move the game via the wrong one and skew the back-solve.
    if measuring {
        let keep = ps("cal_output", "mouse");
        if keep != "mouse" {
            dx = 0.0;
            dy = 0.0;
        }
        if keep != "stick" {
            sx = 0.0;
            sy = 0.0;
        }
    }

    let [view_a, view_b] = flick_view_now(state, flick_stick, flick_on, stick_aim, measuring, flick_output == "stick").encode();
    vec![
        Some(Signal::Vec2(glam::Vec2::new(dx, dy))),
        Some(Signal::Vec2(glam::Vec2::new(sx, sy))),
        Some(Signal::Vec2(flick_pin)),
        // The Map pin: an AutoMap bus, carried in `collector_sigs`.
        None,
        // Display-only (not a pin): see the doc comment above.
        Some(Signal::Float(state.aux_f32[MEAS_DEG])),
        Some(Signal::Float(state.aux_f32[MEAS_PEAK_DEFL])),
        Some(view_a),
        Some(view_b),
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
    while state.aux_f32.len() <= FLK_NO_SNAP {
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
    let release_dz = rws_flick_release_floor(deadzone);
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
            state.aux_f32[FLK_TURNED] += d.to_degrees();
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
            let no_snap = flick_deg.abs() <= fwd_dz_deg;
            let flick_deg = if no_snap { 0.0 } else { flick_deg };
            state.aux_f32[FLK_TURNED] = flick_deg;
            state.aux_f32[FLK_NO_SNAP] = f32::from(u8::from(no_snap));
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
        FLK_STAB_RAW, FLK_STAB_OUT, FLK_TURNED, FLK_NO_SNAP,
    ] {
        if let Some(v) = state.aux_f32.get_mut(i) {
            *v = 0.0;
        }
    }
    state.rws_flick_stab = None;
}
