//! When does an input count as "pressed"? Press modes (tap / hold /
//! double / long), turbo, stick-gesture recognition, and the
//! analog→digital pulse conversion.
//!
//! Also the cardinal-direction machinery: stick axes derived as
//! individual up/down/left/right pins, and the suppression bookkeeping
//! that stops a claimed cardinal from also firing its parent vec2.

use super::*;

// ── Press-mode state machine (Remapper / Map Action mapping options) ─────────
//
// Each Remapper / Map Action mapping carries an optional "press mode" that
// transforms the raw held-state of the input chord into the gate that fires
// the mapping. State is allocated per-mapping in `state.aux_f32` (4 floats
// per mapping). Slots:
//   [0] prev_input        — 0/1 held-state from the previous tick (rising/
//                           falling edge detection).
//   [1] press_start       — accumulated seconds since the press began (Long
//                           uses this directly; Short and Double window-test
//                           this against the configured window_ms).
//   [2] trigger_remaining — seconds left for an artificial output pulse. The
//                           Short replay (replay actual press duration) and
//                           on-press / on-release 10 ms triggers all decrement
//                           this each tick.
//   [3] double_state      — 0 = idle, 1 = saw 1st rising, 2 = saw 1st falling,
//                           3 = saw 2nd rising (output ON during this state).
//                           Window-checked against [1] at each transition.

pub(crate) const PRESS_SLOTS_PER_MAPPING: usize = 5;
/// Short trigger duration emitted by on-press, on-release, and long-press
/// non-sustain modes. 10 ms gives downstream counters / edge detectors a
/// clean visible pulse without lingering as a held key.
pub(crate) const PRESS_TRIGGER_PULSE_S: f32 = 0.010;

#[derive(Copy, Clone)]
pub(crate) enum PressMode {
    Down,        // pass-through (default)
    Short,       // on-off within window → replay original held duration
    Long,        // held longer than window → sustain OR 10ms trigger
    Double,      // double-tap within window → ON during 2nd press
    OnPress,     // 10ms trigger on rising edge
    OnRelease,   // 10ms trigger on falling edge
}

impl PressMode {
    pub(crate) fn from_str(s: &str) -> Self {
        match s {
            "short"      => Self::Short,
            "long"       => Self::Long,
            "double"     => Self::Double,
            "on_press"   => Self::OnPress,
            "on_release" => Self::OnRelease,
            _            => Self::Down,
        }
    }
}

/// Read 4-slot state for a mapping. Resizes `press_state` if the node hasn't
/// allocated this mapping's slots yet so callers don't have to.
pub(crate) fn press_state_get(state: &mut NodeState, mapping_idx: usize) -> &mut [f32] {
    let need = (mapping_idx + 1) * PRESS_SLOTS_PER_MAPPING;
    if state.press_state.len() < need {
        state.press_state.resize(need, 0.0);
    }
    let start = mapping_idx * PRESS_SLOTS_PER_MAPPING;
    &mut state.press_state[start..start + PRESS_SLOTS_PER_MAPPING]
}

/// Bit indices used by gesture_state for stick-cardinal visited bitmaps.
/// Order: left(0), right(1), up(2), down(3). Same indexing for both
/// left_stick and right_stick — bitmap[0] = left_stick visited cardinals,
/// bitmap[1] = right_stick.
pub(crate) const GESTURE_BIT_LEFT:  u8 = 1 << 0;
pub(crate) const GESTURE_BIT_RIGHT: u8 = 1 << 1;
pub(crate) const GESTURE_BIT_UP:    u8 = 1 << 2;
pub(crate) const GESTURE_BIT_DOWN:  u8 = 1 << 3;

/// Stick deflection threshold to "activate" gesture tracking. Below this,
/// the stick is considered neutral and the visited bitmap resets.
pub(crate) const GESTURE_ACTIVATE_MAG: f32 = 0.5;
/// Hysteresis: once activated, the bitmap is only cleared when the stick
/// falls back below this (lower) threshold. Prevents spurious resets when
/// the stick passes through a quadrant boundary.
pub(crate) const GESTURE_RESET_MAG: f32 = 0.3;

/// Map a stick-cardinal pin id to (stick_index, bit). Returns None if the
/// pin isn't a stick cardinal.
pub(crate) fn gesture_pin_to_bit(pin_id: &str) -> Option<(usize, u8)> {
    match pin_id {
        "left_stick_left"   => Some((0, GESTURE_BIT_LEFT)),
        "left_stick_right"  => Some((0, GESTURE_BIT_RIGHT)),
        "left_stick_up"     => Some((0, GESTURE_BIT_UP)),
        "left_stick_down"   => Some((0, GESTURE_BIT_DOWN)),
        "right_stick_left"  => Some((1, GESTURE_BIT_LEFT)),
        "right_stick_right" => Some((1, GESTURE_BIT_RIGHT)),
        "right_stick_up"    => Some((1, GESTURE_BIT_UP)),
        "right_stick_down"  => Some((1, GESTURE_BIT_DOWN)),
        _ => None,
    }
}

/// Compute the set of cardinal bits currently active for a stick given its
/// (x, y) values. Uses the same 8-zone dominant-axis rule as
/// `derive_stick_cardinals`: pure-axis ±0.5+ when one axis dominates;
/// diagonal contributes BOTH neighboring cardinals when both axes are
/// large enough. Returns 0 when the stick is neutral.
pub(crate) fn gesture_active_bits(x: f32, y: f32) -> u8 {
    let mag = (x * x + y * y).sqrt();
    if mag < GESTURE_ACTIVATE_MAG { return 0; }
    let mut bits = 0u8;
    // 22.5° quadrant: a cardinal is "active" when its axis component is
    // at least 0.5× the other axis (i.e., the stick is in that octant).
    let ax = x.abs();
    let ay = y.abs();
    if x >  0.0 && ax > ay * 0.5 { bits |= GESTURE_BIT_RIGHT; }
    if x <  0.0 && ax > ay * 0.5 { bits |= GESTURE_BIT_LEFT; }
    if y >  0.0 && ay > ax * 0.5 { bits |= GESTURE_BIT_UP; }
    if y <  0.0 && ay > ax * 0.5 { bits |= GESTURE_BIT_DOWN; }
    bits
}

/// Read 2-slot gesture state for a mapping. Resizes if needed.
pub(crate) fn gesture_state_get(state: &mut NodeState, mapping_idx: usize) -> &mut [u8; 2] {
    if state.gesture_state.len() <= mapping_idx {
        state.gesture_state.resize(mapping_idx + 1, [0, 0]);
    }
    &mut state.gesture_state[mapping_idx]
}

/// If `in_pins` contains at least one stick cardinal, return the per-stick
/// required bitmaps (left, right) for the cardinal subset. Non-cardinal pins
/// (buttons, triggers) are ignored here — the caller must enforce their hold
/// state separately. Returns None when the chord has no stick cardinals, so
/// the caller falls back to the standard simultaneous-press rule.
pub(crate) fn gesture_required_bits(in_pins: &[&str]) -> Option<[u8; 2]> {
    if in_pins.is_empty() { return None; }
    let mut req = [0u8; 2];
    let mut any_cardinal = false;
    for &p in in_pins {
        if let Some((stick, bit)) = gesture_pin_to_bit(p) {
            req[stick] |= bit;
            any_cardinal = true;
        }
    }
    if any_cardinal { Some(req) } else { None }
}

/// Update the per-mapping gesture state for one tick and return whether the
/// gesture is "complete" (all required cardinals visited at least once
/// across both sticks). `upstream` provides current stick axis values.
pub(crate) fn gesture_tick(
    required: [u8; 2],
    visited: &mut [u8; 2],
    upstream: &HashMap<String, Signal>,
) -> bool {
    for (stick_idx, axis_pins) in [
        (0usize, ("left_stick_x",  "left_stick_y")),
        (1usize, ("right_stick_x", "right_stick_y")),
    ] {
        let req_bits = required[stick_idx];
        if req_bits == 0 { continue; }
        let x = upstream.get(axis_pins.0).map(|s| sig_scalar(*s)).unwrap_or(0.0);
        let y = upstream.get(axis_pins.1).map(|s| sig_scalar(*s)).unwrap_or(0.0);
        let mag = (x * x + y * y).sqrt();
        if mag < GESTURE_RESET_MAG {
            visited[stick_idx] = 0;
        } else {
            visited[stick_idx] |= gesture_active_bits(x, y);
        }
    }
    // Complete iff every required bit on every stick has been visited.
    (visited[0] & required[0]) == required[0]
        && (visited[1] & required[1]) == required[1]
}

/// Apply the configured press mode to the raw input gate. Returns the
/// transformed gate the mapping should treat as "held this tick".
///
/// `window_ms` is interpreted per mode (Short = max press duration to count
/// as a tap; Long = min hold duration; Double = max time from 1st rising to
/// 2nd rising). `sustain` is meaningful for Long only — when false, fire a
/// 10 ms trigger on threshold crossing instead of holding while pressed.
pub(crate) fn apply_press_mode(
    raw_held: bool,
    mode: PressMode,
    window_ms: f32,
    sustain: bool,
    slots: &mut [f32],
    dt: f32,
) -> bool {
    let prev_held = slots[0] > 0.5;
    let rising  = raw_held && !prev_held;
    let falling = !raw_held && prev_held;
    let window_s = (window_ms.max(0.0)) / 1000.0;

    // Trigger-pulse countdown is shared across modes; non-zero values force
    // the output ON until the timer expires regardless of input state.
    let mut trigger_remaining = (slots[2] - dt).max(0.0);

    let out = match mode {
        PressMode::Down => raw_held,
        PressMode::OnPress => {
            // `window_ms` sets the emitted trigger duration (floored at the
            // 10 ms minimum pulse so a 0/tiny value still registers).
            if rising { trigger_remaining = window_s.max(PRESS_TRIGGER_PULSE_S); }
            trigger_remaining > 0.0
        }
        PressMode::OnRelease => {
            if falling { trigger_remaining = window_s.max(PRESS_TRIGGER_PULSE_S); }
            trigger_remaining > 0.0
        }
        PressMode::Long => {
            // press_start (slots[1]) accumulates seconds while held.
            if rising {
                slots[1] = 0.0;
                // double_state field is repurposed as "armed" flag for the
                // non-sustain trigger so we fire once per press.
                slots[3] = 0.0;
            }
            if raw_held {
                slots[1] += dt;
            } else {
                slots[1] = 0.0;
                slots[3] = 0.0;
            }
            let threshold_crossed = slots[1] >= window_s && window_s > 0.0;
            if sustain {
                threshold_crossed
            } else {
                if threshold_crossed && slots[3] < 0.5 {
                    slots[3] = 1.0; // armed → fire exactly once per press
                    trigger_remaining = PRESS_TRIGGER_PULSE_S;
                }
                trigger_remaining > 0.0
            }
        }
        PressMode::Short => {
            // Tracks the live press; on release we know its duration and can
            // replay it. If the press never released within the window, the
            // chord is suppressed entirely (mapping never fires).
            //
            // double_state field stores remaining replay seconds. When > 0
            // we are in playback and the user's input is ignored until done.
            let mut replay_remaining = slots[3];
            if replay_remaining > 0.0 {
                replay_remaining = (replay_remaining - dt).max(0.0);
                slots[3] = replay_remaining;
                slots[0] = if raw_held { 1.0 } else { 0.0 };
                slots[1] = 0.0;
                slots[2] = trigger_remaining;
                return replay_remaining > 0.0;
            }
            if rising {
                slots[1] = 0.0;
            }
            if raw_held {
                slots[1] += dt;
                if slots[1] > window_s && window_s > 0.0 {
                    // Held too long — give up on this press; user has to
                    // release and tap again. Suppressed until release.
                    slots[1] = f32::INFINITY;
                }
            }
            if falling {
                let held_s = slots[1];
                slots[1] = 0.0;
                if held_s.is_finite() && (window_s <= 0.0 || held_s <= window_s) && held_s > 0.0 {
                    // Qualifying tap → replay the press duration as output.
                    slots[3] = held_s;
                    slots[0] = 0.0;
                    slots[2] = trigger_remaining;
                    return true;
                }
            }
            false
        }
        PressMode::Double => {
            // double_state: 0 idle / 1 after 1st rising / 2 after 1st falling
            //               / 3 during 2nd press (output ON)
            // press_start: seconds since 1st rising edge (window check).
            let mut s = slots[3] as i32;
            if s != 0 {
                slots[1] += dt;
                if window_s > 0.0 && slots[1] > window_s {
                    s = 0; // window expired before completing the gesture
                    slots[1] = 0.0;
                }
            }
            if rising {
                if s == 0 {
                    s = 1;
                    slots[1] = 0.0;
                } else if s == 2 {
                    s = 3; // 2nd rising → output starts
                }
            }
            if falling {
                if s == 1 { s = 2; }
                else if s == 3 {
                    // 2nd falling → output ends, gesture consumed.
                    s = 0;
                    slots[1] = 0.0;
                }
            }
            slots[3] = s as f32;
            s == 3
        }
    };

    slots[0] = if raw_held { 1.0 } else { 0.0 };
    slots[2] = trigger_remaining;
    out
}

/// Post-process the press-mode output with a turbo on/off cycle. When `held`
/// is true the output cycles based on `gap_ms` as the full period (half on,
/// half off). When `held` is false the phase resets so the next press starts
/// at the ON portion. State lives in `slots[4]` (turbo phase seconds).
pub(crate) fn apply_turbo(held: bool, gap_ms: f32, slots: &mut [f32], dt: f32) -> bool {
    if !held {
        slots[4] = 0.0;
        return false;
    }
    let period_s = (gap_ms.max(20.0)) / 1000.0;
    let mut phase = slots[4] + dt;
    if phase >= period_s { phase -= period_s * (phase / period_s).floor(); }
    slots[4] = phase;
    phase < period_s * 0.5
}

/// Maximum tap/PWM frequency for analog→digital modulation at full deflection.
/// Shared by the Remapper, Map Action, and 3DOF-Lean analog dispatch so all
/// three feel identical. Turbo doubles this.
pub const ANALOG_DIGITAL_MAX_FREQ_HZ: f32 = 20.0;

/// Drive a DIGITAL (button/key) destination from an analog input magnitude.
/// Three behaviours, selected by `sustain` (Hold) and `turbo`:
///
///   - **Plain** (Hold off): a tap train. `window_ms` is the *minimum* tap
///     period (lowest frequency); the period shortens as `mag → 1` up to
///     `MAX_FREQ_HZ` (×2 with Turbo). Each tap is a clean 50%-duty square
///     wave so it always reads as a distinct tap rather than a held key.
///   - **Hold** (sustain on): PWM. `window_ms` is the fixed pulse PERIOD and
///     `mag` is the duty cycle — `mag=0` → flat off, `mag=1` → full gate.
///     With Turbo the period also shortens with `mag` (PWM + freq-mod).
///   - **Turbo, Hold off**: same tap train as Plain but at ×2 max frequency.
///
/// `mag` is the post-deadzone input deflection in 0..1. `slots[0]` holds the
/// phase accumulator (seconds in `[0, period)`); it is reset when `mag` is ~0.
/// Returns whether the digital output is asserted this tick.
pub(crate) fn analog_digital_pulse(
    mag: f32,
    window_ms: f32,
    sustain: bool,
    turbo: bool,
    slots: &mut [f32],
    dt: f32,
) -> bool {
    let mag = mag.clamp(0.0, 1.0);
    if mag < 0.01 {
        slots[0] = 0.0;
        return false;
    }
    let max_freq = if turbo {
        ANALOG_DIGITAL_MAX_FREQ_HZ * 2.0
    } else {
        ANALOG_DIGITAL_MAX_FREQ_HZ
    };

    if sustain {
        // ── Hold = PWM: duty cycle tracks amplitude ──────────────────────
        // Period is fixed by window_ms; Turbo additionally scales it down
        // with amplitude so a harder push pulses faster as well as wider.
        let base_period = (window_ms / 1000.0).max(0.020);
        let period = if turbo {
            (1.0 / (mag * max_freq)).clamp(0.020, base_period)
        } else {
            base_period
        };
        let on_s = (mag * period).clamp(0.0, period);
        slots[0] += dt;
        if slots[0] >= period { slots[0] -= period; }
        // mag≈1 → on_s≈period → effectively always on (full gate).
        slots[0] < on_s || on_s >= period
    } else {
        // ── Plain / Turbo = tap train: frequency tracks amplitude ────────
        // `window_ms` sets ONLY the minimum period (lowest tap frequency);
        // a harder push shortens the period up to `max_freq`. Each tap is a
        // clean 50%-duty square wave so it always reads as a distinct tap —
        // NOT a near-held key (the old `tap_on = window_ms` made the duty
        // ~90% at the period floor, which felt held).
        let min_period = (window_ms / 1000.0).max(1.0 / max_freq);
        let period = (1.0 / (mag * max_freq)).max(min_period);
        slots[0] += dt;
        if slots[0] >= period { slots[0] -= period; }
        slots[0] < period * 0.5
    }
}

/// When a processed Vec2 (`left_stick`/`right_stick`/`dpad`) arrives on the
/// AutoMap collector but its X/Y axis pins did NOT (they fell back to raw
/// device samples), the Vec2 is authoritative — a Vec Response Curve wired on
/// the whole stick into a Collector port must drive the axes (and the cardinals
/// derived from them). Split such a Vec2 into its axis pins in `upstream`.
///
/// Per-axis overrides are untouched: if the axes ARE present on the collector,
/// they win (the user processed them individually).
pub(crate) fn vec2_authoritative_axis_fill(
    upstream: &mut HashMap<String, Signal>,
    collector_id: &str,
    collector_sigs: &HashMap<(String, String), Signal>,
) {
    if collector_id.is_empty() { return; }
    let coll = |pin: &str| collector_sigs.get(&(collector_id.to_string(), pin.to_string())).copied();
    for (vec2, xa, ya) in [
        ("left_stick",  "left_stick_x",  "left_stick_y"),
        ("right_stick", "right_stick_x", "right_stick_y"),
        ("dpad",        "dpad_x",        "dpad_y"),
    ] {
        // Need a Vec2 from the collector to be authoritative about.
        let Some(Signal::Vec2(v)) = coll(vec2) else { continue; };
        // Axes absent on the collector → derive them from the Vec2 outright.
        // Axes present but DISAGREE with the Vec2 → the Vec2 was processed
        // (e.g. a Vec Response Curve on the whole stick) while the axes are the
        // unprocessed pass-through, so the Vec2 wins. When they agree (the
        // common raw case, or per-axis processing that also updated the Vec2),
        // leave the axes alone.
        let ax = coll(xa).map(|s| s.as_float());
        let ay = coll(ya).map(|s| s.as_float());
        let disagree = |axis: Option<f32>, comp: f32| match axis {
            Some(a) => (a - comp).abs() > 1e-3,
            None => true,
        };
        if disagree(ax, v.x) { upstream.insert(xa.to_string(), Signal::Float(v.x)); }
        if disagree(ay, v.y) { upstream.insert(ya.to_string(), Signal::Float(v.y)); }
    }
}

/// Derive synthetic cardinal-direction Bool pins from the analog stick axes
/// in `upstream`, in place. Used by the Remapper so a user can map e.g.
/// "L.Stick Up" → "key_w". A cardinal fires when its axis crosses 0.5 and
/// dominates the perpendicular axis by 1.5× — so pushing slightly off-axis
/// still captures a single direction, but a genuine diagonal fires both
/// cardinals as a chord.
pub(crate) fn derive_stick_cardinals(upstream: &mut HashMap<String, Signal>) {
    // Tuned for round-gate sticks where 45° physically caps at ~0.707.
    //   T_CARDINAL — minimum push for a single cardinal to fire when
    //     the perpendicular axis is quiet.
    //   T_DIAGONAL — when BOTH axes exceed this, fire both cardinals as
    //     a chord. Lower than T_CARDINAL so a 45° push at ~0.5/0.5 still
    //     registers as a diagonal (avoid the narrow band that the
    //     dominance rule alone couldn't cover on a circular gate).
    //   DOM — perpendicular-dominance ratio so a slight off-axis push
    //     still counts as a single cardinal.
    const T_CARDINAL: f32 = 0.5;
    const T_DIAGONAL: f32 = 0.4;
    const DOM: f32 = 1.5;
    for (xpin, ypin, up, down, left, right) in [
        ("left_stick_x",  "left_stick_y",
         "left_stick_up",  "left_stick_down",
         "left_stick_left", "left_stick_right"),
        ("right_stick_x", "right_stick_y",
         "right_stick_up", "right_stick_down",
         "right_stick_left", "right_stick_right"),
    ] {
        let x = upstream.get(xpin).map(|s| sig_scalar(*s)).unwrap_or(0.0);
        let y = upstream.get(ypin).map(|s| sig_scalar(*s)).unwrap_or(0.0);
        let ax = x.abs();
        let ay = y.abs();
        let diagonal = ax > T_DIAGONAL && ay > T_DIAGONAL;
        let right_on = diagonal && x >  T_DIAGONAL
            || x >  T_CARDINAL && (ay < T_CARDINAL ||  x >  DOM * ay);
        let left_on  = diagonal && x < -T_DIAGONAL
            || x < -T_CARDINAL && (ay < T_CARDINAL || -x >  DOM * ay);
        let up_on    = diagonal && y >  T_DIAGONAL
            || y >  T_CARDINAL && (ax < T_CARDINAL ||  y >  DOM * ax);
        let down_on  = diagonal && y < -T_DIAGONAL
            || y < -T_CARDINAL && (ax < T_CARDINAL || -y >  DOM * ax);
        upstream.insert(up.to_string(),    Signal::Bool(up_on));
        upstream.insert(down.to_string(),  Signal::Bool(down_on));
        upstream.insert(left.to_string(),  Signal::Bool(left_on));
        upstream.insert(right.to_string(), Signal::Bool(right_on));
    }
}

/// Reserved collector-pin-name prefix marking a pin that an upstream consumer
/// (Remapper) CLAIMED. A downstream Combiner reads these to suppress the same
/// pin on lower-priority inputs (hierarchy), unless that port's policy is ADD.

/// For Analog mode, a synthetic stick-cardinal Bool (left_stick_right, etc.)
/// captured during Learn is reinterpreted as "drive the underlying analog
/// axis in that direction." Returns (axis_pin_id, sign) where sign is +1
/// for the positive direction (right/up) or -1 for the negative direction
/// (left/down). Returns None when the pin isn't a stick cardinal — those
/// fall through to normal pulse-train Bool emission.
/// Pub for the UI too: mapping-card curve editors read the live cardinal
/// deflection from `live_signals` to draw the input→output preview dot.
pub fn analog_axis_for_cardinal(pin_id: &str) -> Option<(&'static str, f32)> {
    match pin_id {
        "left_stick_right"  => Some(("left_stick_x",   1.0)),
        "left_stick_left"   => Some(("left_stick_x",  -1.0)),
        "left_stick_up"     => Some(("left_stick_y",   1.0)),
        "left_stick_down"   => Some(("left_stick_y",  -1.0)),
        "right_stick_right" => Some(("right_stick_x",  1.0)),
        "right_stick_left"  => Some(("right_stick_x", -1.0)),
        "right_stick_up"    => Some(("right_stick_y",  1.0)),
        "right_stick_down"  => Some(("right_stick_y", -1.0)),
        _ => None,
    }
}

/// Like `analog_axis_for_cardinal` but ALSO covers D-pad directions. Used only
/// for SUPPRESSION (zeroing the underlying axis + Vec2 when a cardinal is
/// claimed), NOT for analog output routing — the D-pad is a quantized digital
/// hat, so we don't want to drive `dpad_x/y` as a continuous analog axis. But
/// when a Remapper consumes `dpad_up`, the raw `dpad_y`/`dpad` Vec2 must be
/// suppressed too, otherwise the virtual device regenerates the Bool D-pad from
/// them and the claimed direction leaks straight through.
pub(crate) fn cardinal_axis_for_suppression(pin_id: &str) -> Option<(&'static str, f32)> {
    match pin_id {
        "dpad_right" => Some(("dpad_x",  1.0)),
        "dpad_left"  => Some(("dpad_x", -1.0)),
        "dpad_up"    => Some(("dpad_y",  1.0)),
        "dpad_down"  => Some(("dpad_y", -1.0)),
        _ => analog_axis_for_cardinal(pin_id),
    }
}

/// The bundled Vec2 pin id and component (true = y) for a stick/dpad axis pin.
pub(crate) fn vec2_pin_for_axis(axis_pin: &str) -> Option<&'static str> {
    match axis_pin {
        "left_stick_x"  | "left_stick_y"  => Some("left_stick"),
        "right_stick_x" | "right_stick_y" => Some("right_stick"),
        "dpad_x"        | "dpad_y"        => Some("dpad"),
        _ => None,
    }
}

/// Per-side suppression derived from a Remapper's CLAIMED cardinals (sticks +
/// D-pad). For each affected axis pin we record `(neg, pos)` — which side to
/// clamp to zero. Claiming `dpad_left` clamps only the negative side of
/// `dpad_x`, leaving `dpad_x` positive and `dpad_y` entirely untouched. The
/// matching cardinal Bool pins are returned separately to be zeroed directly.
/// Works for both digital and analog claims — the D-pad is digital but the
/// per-side rule is identical, and it preserves the directions the user did
/// NOT map.
pub(crate) struct CardinalSuppression {
    /// axis pin → (clamp_negative, clamp_positive)
    pub(crate) axis_sides: HashMap<&'static str, (bool, bool)>,
    /// cardinal Bool pins to force false (only the claimed directions)
    pub(crate) bool_pins: HashSet<&'static str>,
}

pub(crate) fn cardinal_suppression(claimed: &HashSet<String>) -> CardinalSuppression {
    let mut axis_sides: HashMap<&'static str, (bool, bool)> = HashMap::new();
    let mut bool_pins: HashSet<&'static str> = HashSet::new();
    for cardinal in claimed {
        if let Some((axis, sign)) = cardinal_axis_for_suppression(cardinal) {
            let entry = axis_sides.entry(axis).or_insert((false, false));
            if sign > 0.0 { entry.1 = true; } else { entry.0 = true; }
            // Canonical cardinal name for the claimed direction.
            if let Some(name) = CARDINAL_PIN_IDS.iter().find(|n| *n == cardinal) {
                bool_pins.insert(name);
            }
        }
    }
    CardinalSuppression { axis_sides, bool_pins }
}

/// All stick + D-pad cardinal Bool pin ids (used to resolve `&'static str`).
pub(crate) const CARDINAL_PIN_IDS: &[&str] = &[
    "left_stick_up", "left_stick_down", "left_stick_left", "left_stick_right",
    "right_stick_up", "right_stick_down", "right_stick_left", "right_stick_right",
    "dpad_up", "dpad_down", "dpad_left", "dpad_right",
];

// ── Macro-port publish helpers ────────────────────────────────────────────────
//
// Mapping evaluators (Remapper / Touch Zones cards / 3DOF-Lean) can target a
// macro port by putting its pin id ("macro:{id}") into a mapping's `out`
// array, exactly like a bus pin. Macro pins are NOT bus pins though: they are
// intercepted at each publish site and routed into reserved per-tick
// namespaces in `collector_sigs` — `("macro", pin)` for the scalar/bool
// aspect, `("macro#v2", pin)` for the Vec2 aspect (zone deflection) — instead
// of the evaluator's own `remap:{uid}`-style key, so they never leak onto the
// AutoMap bus or reach sinks. `module.macro`'s compute (see compute_node)
// reads them back and coerces to each port's declared type.
//
// Only ASSERTED values are written (absent = released = the port's off
// value), and multiple writers to one port merge by larger magnitude, so an
// active mapping always wins over an idle one regardless of evaluation order.

pub(crate) fn sig_magnitude(s: Signal) -> f32 {
    match s {
        Signal::Vec2(v) => v.length(),
        other => other.as_float().abs(),
    }
}

/// True for any out-pin that routes into the macro collector namespaces
/// instead of the AutoMap bus: Macro ports (`macro:{id}`) and Virtual-Menu
/// targets (`menu:{id}_show` / `_sel`). Every publish/skip gate in the mapping
/// evaluators goes through this so the two pin families behave identically.
pub(crate) fn is_macro_style_target(pin: &str) -> bool {
    flexinput_core::macros::parse_macro_pin(pin).is_some()
        || flexinput_core::menu::parse_target_pin(pin).is_some()
}

pub(crate) fn merge_macro_ns(
    collector_sigs: &mut HashMap<(String, String), Signal>,
    ns: &str,
    pin: &str,
    sig: Signal,
) {
    let key = (ns.to_string(), pin.to_string());
    match collector_sigs.get(&key) {
        Some(&prev) if sig_magnitude(prev) >= sig_magnitude(sig) => {}
        _ => { collector_sigs.insert(key, sig); }
    }
}

/// Publish the scalar/bool aspect of a macro-port write.
pub(crate) fn merge_macro_scalar(
    collector_sigs: &mut HashMap<(String, String), Signal>,
    pin: &str,
    sig: Signal,
) {
    merge_macro_ns(collector_sigs, flexinput_core::macros::SIGS_NS, pin, sig);
}

/// Publish the Vec2 aspect of a macro-port write (zone-local deflection).
pub(crate) fn merge_macro_vec2(
    collector_sigs: &mut HashMap<(String, String), Signal>,
    pin: &str,
    v: Vec2,
) {
    merge_macro_ns(collector_sigs, flexinput_core::macros::SIGS_NS_VEC2, pin, Signal::Vec2(v));
}
