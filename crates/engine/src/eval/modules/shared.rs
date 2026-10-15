//! Helpers used by more than one evaluator: analog input resolution, the
//! per-mapping curve/threshold triad, and touch-combo evaluation.
//!
//! These sit beside the evaluators rather than inside any one of them
//! because Remapper, Touch Zones, Map Action and Lean all reach for them.

use super::*;
use flexinput_core::imu::{imu_pin, is_imu_pin, ImuEstimator, ImuScale, IMU_PINS};

/// Map an analog-mode output pin to its one-sided trigger axis, if it is one.
/// Triggers are 0..1 (no negative side), so analog mappings drive them with the
/// input's unsigned magnitude. Returns the trigger pin id, or None for non-trigger
/// outputs (which the caller treats as cardinal axes or buttons).
///
/// The digital trigger buttons (`btn_lt_dig`/`btn_rt_dig`) also map here: a
/// Remapper captures its output by chord-learning, so on a pad whose trigger is
/// a digital button (Switch Pro ZL/ZR) the captured `out` pin is the digital
/// button, not the analog trigger. In ANALOG mode the user's intent is analog
/// travel, so we route the digital-trigger-button target to its analog pin.
pub(crate) fn analog_trigger_out(pin_id: &str) -> Option<&'static str> {
    match pin_id {
        "left_trigger"  | "btn_lt_dig" => Some("left_trigger"),
        "right_trigger" | "btn_rt_dig" => Some("right_trigger"),
        _ => None,
    }
}

/// Return the signed analog magnitude an input cardinal currently contributes
/// to its axis: 0.0 when the stick is neutral or pushed in the opposite
/// direction; up to ±1.0 at full deflection in the cardinal's direction.
/// Used by analog-mode Remapper / Map Action to drive output axes from
/// input cardinals' live magnitudes (no gesture gate).
pub(crate) fn analog_cardinal_input_value(upstream: &PinSigs, pin_id: &str) -> f32 {
    let Some((axis_pin, cardinal_sign)) = analog_axis_for_cardinal(pin_id) else { return 0.0; };
    let axis_val = upstream.get(axis_pin).map(|s| sig_scalar(*s)).unwrap_or(0.0);
    let signed = axis_val * cardinal_sign;
    signed.max(0.0).min(1.0)
}

// ── Per-mapping response curve + activation threshold ────────────────────────
//
// Every mapping card (Remapper `mappings`, Lean `lean_left`/`lean_right`,
// Touch Zones `zone_maps`) may carry:
//   curve:     [[x, y], …] — response curve over the analog input magnitude
//              (0..1 → 0..1). Absent = identity.
//   threshold: f32 0..1 — a HORIZONTAL line on the curve's OUTPUT: a digital
//              binding is held while the shaped magnitude sits on/above it
//              and releases the moment it dips below (manual activation
//              point). Absent = legacy behaviour (derived cardinal bools /
//              0.5 trigger coercion / freq-modulated pulse train).

/// A mapping card's response curve and its optional manual threshold.
///
/// They belong together because the threshold is always compared against the
/// CURVE-SHAPED magnitude, never the raw one — a card that reshapes its input
/// and then gates on the unshaped value would fire at the wrong deflection.
/// Four sites used to read the pair separately and re-spell that comparison by
/// hand; `analog_gate` is now the only place it is written.
pub(crate) struct MappingShape {
    pub(crate) curve: Vec<[f32; 2]>,
    pub(crate) threshold: Option<f32>,
    /// How this card reads an IMU input: its range per family and Log/Exp.
    pub(crate) imu: ImuScale,
}

/// Where an input with no held state of its own — an IMU input, a whole stick
/// — counts as pressed on a card with no threshold: half its (shaped) range, as
/// a trigger counts from half travel.
pub(crate) const ANALOG_DEFAULT_ON: f32 = 0.5;

impl MappingShape {
    pub(crate) fn from_card(m: &Value) -> Self {
        Self {
            curve: mapping_curve_pts(m),
            threshold: mapping_threshold(m),
            imu: ImuScale::from_card(m),
        }
    }

    /// An input's live analog value as THIS card reads it: an IMU input over
    /// the card's range and Log/Exp (signed for a two-sided axis), anything
    /// else as [`analog_in_value`].
    pub(crate) fn in_value(&self, upstream: &PinSigs, pin: &str) -> Option<f32> {
        if let Some(p) = imu_pin(pin) {
            let v = upstream.get(pin).map(|s| sig_scalar(*s)).unwrap_or(0.0);
            return Some(self.imu.normalize(p, v));
        }
        analog_in_value(upstream, pin)
    }

    /// Reshape a magnitude through this card's curve (identity without one).
    pub(crate) fn shaped(&self, mag: f32) -> f32 {
        shape_mag(&self.curve, mag)
    }

    /// Threshold verdict for one analog input pin. `None` means this card has
    /// no threshold, or the pin carries no analog value — the caller then falls
    /// back to the pin's plain held state. An IMU input or a whole stick
    /// always has a verdict: neither has a held state to fall back to, so
    /// without a threshold it counts from [`ANALOG_DEFAULT_ON`] — either way
    /// along a two-sided IMU axis, any way for a stick.
    pub(crate) fn analog_gate(
        &self,
        upstream: &PinSigs,
        pin: &str,
    ) -> Option<bool> {
        if is_imu_pin(pin) || whole_stick_axes(pin).is_some() {
            let v = self.in_value(upstream, pin)?.abs();
            return Some(self.shaped(v) >= self.threshold.unwrap_or(ANALOG_DEFAULT_ON));
        }
        // A note's threshold is on its VELOCITY — which strikes count — and is
        // compared as struck, not through the curve, which shapes the
        // aftertouch that follows.
        if let Some(vel) = note_velocity(upstream, pin) {
            let t = self.threshold?;
            return Some(vel > 0.0 && vel >= t);
        }
        match (self.threshold, analog_in_value(upstream, pin)) {
            (Some(t), Some(v)) => Some(self.shaped(v) >= t),
            _ => None,
        }
    }
}

/// The card's `curve` points, or empty when absent/malformed.
pub(crate) fn mapping_curve_pts(m: &Value) -> Vec<[f32; 2]> {
    m.get("curve").and_then(|v| v.as_array())
        .map(|a| a.iter().filter_map(|p| {
            let q = p.as_array()?;
            Some([q.first()?.as_f64()? as f32, q.get(1)?.as_f64()? as f32])
        }).collect())
        .unwrap_or_default()
}

/// The card's manual activation threshold, when set.
pub(crate) fn mapping_threshold(m: &Value) -> Option<f32> {
    m.get("threshold").and_then(|v| v.as_f64()).map(|v| (v as f32).clamp(0.0, 1.0))
}

/// Shape an input magnitude through a card's curve (identity when no curve).
pub(crate) fn shape_mag(pts: &[[f32; 2]], mag: f32) -> f32 {
    if pts.len() >= 2 {
        sample_curve(pts, mag.clamp(0.0, 1.0), &[]).clamp(0.0, 1.0)
    } else {
        mag
    }
}

/// Live analog INPUT value of a mapping in-pin: a stick cardinal's one-sided
/// deflection, a whole stick's length, an analog trigger's travel, or a MIDI
/// knob / pressure value (0..1) or pitch bend (SIGNED, −1..1 — its two halves
/// are one control). `None` for digital pins.
pub(crate) fn analog_in_value(upstream: &PinSigs, pin_id: &str) -> Option<f32> {
    if analog_axis_for_cardinal(pin_id).is_some() {
        return Some(analog_cardinal_input_value(upstream, pin_id));
    }
    // A whole stick: how far it's pushed, any way.
    if let Some(v) = whole_stick_value(upstream, pin_id) {
        return Some(v.length().min(1.0));
    }
    if matches!(pin_id, "left_trigger" | "right_trigger") {
        return Some(upstream.get(pin_id).map(|s| sig_scalar(*s)).unwrap_or(0.0).clamp(0.0, 1.0));
    }
    if let Some(v) = note_value(upstream, pin_id) {
        return Some(v);
    }
    if let Some(pin) = midi_analog_in(pin_id) {
        let v = upstream.get(pin_id).map(|s| sig_scalar(*s)).unwrap_or(0.0);
        let bend = matches!(pin, flexinput_core::midi::MidiPin::PitchBend { .. });
        return Some(if bend { v.clamp(-1.0, 1.0) } else { v.clamp(0.0, 1.0) });
    }
    None
}

/// A MIDI note's live value, 0..1: while it sounds, its poly aftertouch, else
/// its channel's pressure, else its velocity (`MidiPin::note_companions`); 0
/// once it is released. `None` for anything that isn't a note.
///
/// "Else" means ABSENT from the bus, not zero: a MIDI port keeps a held note's
/// pressure on the bus at 0 once it has been sent, so easing off aftertouch
/// reads 0 rather than jumping back to the velocity.
pub(crate) fn note_value(upstream: &PinSigs, pin_id: &str) -> Option<f32> {
    let pin = flexinput_core::midi::parse_pin(pin_id)?;
    let companions = pin.note_companions()?;
    if !upstream.get(pin_id).is_some_and(|s| s.as_bool()) {
        return Some(0.0);
    }
    Some(
        companions
            .iter()
            .find_map(|c| upstream.get(&c.to_id()))
            .map(|s| sig_scalar(*s).clamp(0.0, 1.0))
            .unwrap_or(0.0),
    )
}

/// A note's strike velocity while it sounds (0 once released). A card's
/// threshold on a note input is measured against this, not the live value: it
/// decides which strikes count, while the curve shapes the aftertouch after.
pub(crate) fn note_velocity(upstream: &PinSigs, pin_id: &str) -> Option<f32> {
    let pin = flexinput_core::midi::parse_pin(pin_id)?;
    let [_, _, vel] = pin.note_companions()?;
    if !upstream.get(pin_id).is_some_and(|s| s.as_bool()) {
        return Some(0.0);
    }
    Some(upstream.get(&vel.to_id()).map(|s| sig_scalar(*s)).unwrap_or(0.0))
}

/// A MIDI pin a mapping can read as an analog magnitude: a controller, 14-bit
/// controller, (N)RPN, pressure or bend. The tempo is continuous too, but it is
/// a BPM figure, not a 0..1 position, so it is no analog input.
fn midi_analog_in(pin_id: &str) -> Option<flexinput_core::midi::MidiPin> {
    flexinput_core::midi::parse_pin(pin_id)
        .filter(|p| p.is_continuous() && !matches!(p, flexinput_core::midi::MidiPin::Bpm))
}

/// True when a pin id is an analog INPUT source — a stick cardinal, an analog
/// trigger, a continuous MIDI value (see [`analog_in_value`]) or an IMU input
/// (see [`MappingShape::in_value`]).
///
/// It decides what "analog" can mean for a mapping: the Analog press mode is
/// only offered (and only kept on load) for a card with at least one such
/// input — see [`card_allows_analog_mode`] — and analog-only outputs (the
/// touchpad swipes) only once an analog input has been captured.
pub fn pin_is_analog_input(pin_id: &str) -> bool {
    analog_axis_for_cardinal(pin_id).is_some()
        || matches!(pin_id, "left_trigger" | "right_trigger")
        || midi_analog_in(pin_id).is_some()
        // A note is a gate AND a value (its aftertouch).
        || flexinput_core::midi::parse_pin(pin_id).is_some_and(|p| p.note_companions().is_some())
        || is_imu_pin(pin_id)
        || whole_stick_axes(pin_id).is_some()
}

/// A whole stick's axis pins, for `left_stick` / `right_stick`.
pub(crate) fn whole_stick_axes(pin: &str) -> Option<(&'static str, &'static str)> {
    match pin {
        "left_stick" => Some(("left_stick_x", "left_stick_y")),
        "right_stick" => Some(("right_stick_x", "right_stick_y")),
        _ => None,
    }
}

/// A whole stick's four directions — what taking the whole stick takes.
pub(crate) fn whole_stick_dirs(pin: &str) -> Option<[&'static str; 4]> {
    match pin {
        "left_stick" => Some(["left_stick_up", "left_stick_down", "left_stick_left", "left_stick_right"]),
        "right_stick" => Some(["right_stick_up", "right_stick_down", "right_stick_left", "right_stick_right"]),
        _ => None,
    }
}

/// A whole stick's position, from its axes (what its directions derive from),
/// else its Vec2; centred when the bus has neither. `None` for any other pin.
pub(crate) fn whole_stick_value(upstream: &PinSigs, pin: &str) -> Option<Vec2> {
    let (xa, ya) = whole_stick_axes(pin)?;
    let axis = |p: &str| upstream.get(p).map(|s| sig_scalar(*s));
    Some(match (axis(xa), axis(ya)) {
        (Some(x), Some(y)) => Vec2::new(x, y),
        _ => match upstream.get(pin) {
            Some(Signal::Vec2(v)) => *v,
            _ => Vec2::ZERO,
        },
    })
}

/// Whether any card reads an IMU input — only then is the gravity estimate run.
pub(crate) fn mappings_read_imu(mappings: &[Value]) -> bool {
    mappings.iter().any(|m| {
        m.get("in").and_then(|v| v.as_array())
            .is_some_and(|a| a.iter().any(|v| v.as_str().is_some_and(is_imu_pin)))
    })
}

/// Derive the IMU input pins (`imu_*`) from the canonical gyro / accel pins in
/// `upstream`, in place, advancing the node's gravity estimate by `dt`. Each
/// carries [`ImuPin::bus_value`]: the physical value over its family's default
/// range, which a card rescales to its own. Nothing is derived without a gyro.
///
/// These are evaluator-local, like the stick cardinals: never published, never
/// claimed — the gyro they come from passes through untouched.
pub(crate) fn derive_imu_pins(upstream: &mut PinSigs, est: &mut ImuEstimator, dt: f32) {
    let read = |p: &str| upstream.get(p).map(|s| sig_scalar(*s));
    let (Some(gx), Some(gy), Some(gz)) = (read("gyro_x"), read("gyro_y"), read("gyro_z")) else {
        return;
    };
    let accel = match (read("accel_x"), read("accel_y"), read("accel_z")) {
        (Some(x), Some(y), Some(z)) => Some(glam::Vec3::new(x, y, z)),
        _ => None,
    };
    let r = est.tick(glam::Vec3::new(gx, gy, gz), accel, dt);
    for p in IMU_PINS {
        upstream.insert(flexinput_core::pooled_string(p.id), Signal::Float(p.bus_value(&r)));
    }
}

/// Whether a Remapper / Map Action card may use the Analog press mode: it
/// needs an analog input to take the magnitude from. A card of buttons alone
/// has no movement to pass on, so Analog there is just Normal with extra
/// steps; an analog input chorded with buttons (hold L1, move the stick) is
/// the case it exists for. Accepts both card forms (`{ in: [...] }` and Map
/// Action's legacy bare pin list).
pub fn card_allows_analog_mode(card: &Value) -> bool {
    let pins = card.get("in").and_then(|v| v.as_array()).or_else(|| card.as_array());
    pins.is_some_and(|a| a.iter().filter_map(|v| v.as_str()).any(pin_is_analog_input))
}

/// Synthetic Remapper/Lean OUTPUT pins that drive the virtual touchpad rather
/// than a canonical sink pin. `touch_left/center/right` place a finger TOUCH at a
/// fixed X zone; `touch_swipe_x/_y` move a finger along an axis by the input's
/// signed analog magnitude (absolute-position model). These are translated into
/// canonical `touch1_*`/`touch2_*` points by [`publish_touch_points`]; the plain
/// `btn_touchpad` (click) and `btn_mute` outputs are canonical and need no
/// translation.
#[derive(Clone, Copy, PartialEq)]
pub(crate) enum TouchOutKind { Zone(f32), SwipeX, SwipeY }

/// Horizontal offset of the left/right touch zones (center = 0). Matches the
/// input-side `zone_of_x` thresholds (±1/3) comfortably.
pub(crate) const TOUCH_ZONE_X: f32 = 0.66;

pub(crate) fn touchpad_out_kind(pin_id: &str) -> Option<TouchOutKind> {
    match pin_id {
        "touch_left"   => Some(TouchOutKind::Zone(-TOUCH_ZONE_X)),
        "touch_center" => Some(TouchOutKind::Zone(0.0)),
        "touch_right"  => Some(TouchOutKind::Zone(TOUCH_ZONE_X)),
        "touch_swipe_x" => Some(TouchOutKind::SwipeX),
        "touch_swipe_y" => Some(TouchOutKind::SwipeY),
        _ => None,
    }
}

/// Does this output pin go on the shared signal bus?
///
/// Two kinds of pin do not. Touchpad zone/swipe outputs are SYNTHESIZED into
/// touch points after the mapping loop, and macro-style targets are published
/// into the macro namespace instead — where absence means released, so they
/// must also stay out of the bus release pass or it would fight them.
///
/// Both exclusions were checked as an adjacent pair at three sites; naming the
/// pair says what the two have in common, which is that neither is a bus pin.
pub(crate) fn is_bus_out_pin(pin: &str) -> bool {
    touchpad_out_kind(pin).is_none() && !is_macro_style_target(pin)
}

/// True when any of a mapping's `out` pins drives the touchpad (zone or swipe).
pub(crate) fn mapping_targets_touch(m: &serde_json::Value) -> bool {
    m.get("out").and_then(|v| v.as_array()).map(|a| a.iter().any(|v|
        v.as_str().map(|s| touchpad_out_kind(s).is_some()).unwrap_or(false)
    )).unwrap_or(false)
}

/// Result of evaluating a touch-output combo's inputs by role.
pub(crate) struct TouchComboEval {
    /// Whether the finger should be down this tick.
    pub(crate) active: bool,
    /// Signed horizontal contribution (sum of `*_x` cardinals + triggers, ±1 range).
    pub(crate) axis_x: f32,
    /// Signed vertical contribution (sum of `*_y` cardinals, ±1 range).
    pub(crate) axis_y: f32,
}

/// Evaluate a touch-output combo's inputs by ROLE — the single source of truth
/// shared by the synthesis pass (positions the finger) and the suppression pass
/// (`held_now`, decides when to consume the combo's inputs from pass-through).
///
/// Inputs split into:
///   • BUTTONS — gate the finger: ALL must be held for it to activate; they
///     contribute no axis value.
///   • ANALOG cardinals / triggers — drive the axes, routed by orientation
///     (`*_x` → axis_x, `*_y` → axis_y; triggers → axis_x). Opposite cardinals
///     of one axis (left+right) sum with their signs to cover both halves.
///
/// Activation: gate buttons held AND (a gate button present → always; else any
/// analog deflected). This must NOT require every cardinal at once — a combo
/// mixing left+right of one axis can never be "simultaneously held", which is
/// exactly why a generic all-held check would never suppress its gate buttons.
pub(crate) fn eval_touch_combo(in_pins: &[&str], upstream: &PinSigs) -> TouchComboEval {
    let mut gate_buttons_held = true;
    let mut has_gate_button = false;
    let mut has_analog = false;
    let mut any_analog_active = false;
    let mut axis_x = 0.0f32;
    let mut axis_y = 0.0f32;
    for ip in in_pins {
        if let Some((axis, sign)) = analog_axis_for_cardinal(ip) {
            has_analog = true;
            let v = analog_cardinal_input_value(upstream, ip); // 0..1
            if v > 0.0 { any_analog_active = true; }
            if axis.ends_with("_x") { axis_x += sign * v; } else { axis_y += sign * v; }
        } else if matches!(*ip, "left_trigger" | "right_trigger") {
            has_analog = true;
            let v = upstream.get(*ip).map(|s| sig_scalar(*s)).unwrap_or(0.0).clamp(0.0, 1.0);
            if v > 0.0 { any_analog_active = true; }
            axis_x += v; // one-sided; drives the positive side
        } else {
            has_gate_button = true;
            if !upstream.get(*ip).map(|s| s.as_bool()).unwrap_or(false) {
                gate_buttons_held = false;
            }
        }
    }
    let active = if !gate_buttons_held {
        false
    } else if has_gate_button {
        true // buttons gate: finger down while held (analog only positions)
    } else if has_analog {
        any_analog_active // analog-only: deflection activates
    } else {
        false
    };
    TouchComboEval { active, axis_x, axis_y }
}

/// Publish up to TWO synthesized touch points (`fingers`, ordered, in -1..1) into
/// `collector_sigs[(key, "touch{1,2}_{x,y,active}")]`. Extra requests beyond the
/// hardware's 2 simultaneous points are dropped. Unused slots publish
/// `*_active = false` so a released synthesized touch doesn't latch on the
/// virtual pad. Callers gate this on the patch actually having touchpad-output
/// mappings, so a patch that never targets the touchpad leaves the pass-through
/// touch pins untouched.
pub(crate) fn publish_touch_points(
    key: &str,
    fingers: &[(f32, f32)],
    collector_sigs: &mut SigMap,
) {
    for (i, (xk, yk, ak)) in [
        ("touch1_x", "touch1_y", "touch1_active"),
        ("touch2_x", "touch2_y", "touch2_active"),
    ].iter().enumerate() {
        if let Some((x, y)) = fingers.get(i) {
            collector_sigs.insert((key.to_string(), xk.to_string()),
                Signal::Float(x.clamp(-1.0, 1.0)));
            collector_sigs.insert((key.to_string(), yk.to_string()),
                Signal::Float(y.clamp(-1.0, 1.0)));
            collector_sigs.set_pin(&key, &ak, Signal::Bool(true));
        } else {
            collector_sigs.set_pin(&key, &ak, Signal::Bool(false));
        }
    }
}


/// Apply axis-side suppression to a stick axis Float value. `(neg, pos)` —
/// when `neg` is true, clamp negative values to 0; when `pos` is true,
/// clamp positive values to 0.
pub(crate) fn apply_axis_clamp(v: f32, suppress: (bool, bool)) -> f32 {
    let (neg, pos) = suppress;
    let mut out = v;
    if neg && out < 0.0 { out = 0.0; }
    if pos && out > 0.0 { out = 0.0; }
    out
}


// ── Timed outputs ────────────────────────────────────────────────────────────
//
// A card's `out` is a chord: every pin on while the card's gate is. Two
// optional arrays beside it turn it into a timeline, item i of `out` at:
//   out_gap: [ms, …] — the wait before item i starts, after the item before
//            it started (0 = together). One entry past the last item is a gap
//            waiting for the next pick and changes nothing here.
//   out_dur: [ms | null, …] — how long item i stays on; null (or absent) =
//            as long as the card's gate is — the input held in Normal mode,
//            the time-gap pulse of On press, the replayed press of Short —
//            shifted by the item's start.
// "Ctrl, 50 ms, C" presses Ctrl, then C 50 ms later with Ctrl still down.
// A sequence started plays out after the input lets go, unless the card is in
// Normal mode with Hold on: then letting go before the last item has started
// cancels it. Analog outputs ignore timing.

/// Card param: the gap before each `out` item, in ms.
pub const OUT_GAP_KEY: &str = "out_gap";
/// Card param: each `out` item's own duration in ms, or null for the gate's.
pub const OUT_DUR_KEY: &str = "out_dur";

/// A card's output timeline: each item's start, and its own duration if set,
/// in seconds.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct OutTiming {
    starts: Vec<f64>,
    durs: Vec<Option<f64>>,
    /// When each item must be off by, whatever its duration: a pin picked
    /// again lets go before its next press (infinite otherwise).
    ends: Vec<f64>,
}

/// The most a repeated pin is let go for before its next press, seconds.
/// Less when the presses are closer: it gets half the gap.
const REPRESS_RELEASE: f64 = 0.03;

impl OutTiming {
    /// The timeline for a card with `n` outputs, or `None` when it has no
    /// timing — every item together, for the gate's length: a plain chord.
    pub(crate) fn from_card(m: &Value, n: usize) -> Option<Self> {
        let ms = |key: &str, i: usize| -> Option<f64> {
            m.get(key)?.as_array()?.get(i)?.as_f64().map(|v| v.max(0.0) / 1000.0)
        };
        let mut starts = Vec::with_capacity(n);
        let mut durs = Vec::with_capacity(n);
        let mut at = 0.0;
        for i in 0..n {
            at += ms(OUT_GAP_KEY, i).unwrap_or(0.0);
            starts.push(at);
            durs.push(ms(OUT_DUR_KEY, i));
        }
        let timed = starts.iter().any(|&s| s > 0.0) || durs.iter().any(Option::is_some);
        let ends = vec![f64::INFINITY; n];
        timed.then_some(OutTiming { starts, durs, ends })
    }

    /// Let each pin picked again go before its next press ("East, 50 ms,
    /// East" is two presses, not one long one).
    fn with_repeats(mut self, pins: &[String]) -> Self {
        for i in 0..pins.len().min(self.starts.len()) {
            let Some(j) = (i + 1..pins.len()).find(|&j| pins[j] == pins[i]) else { continue };
            let gap = self.starts[j] - self.starts[i];
            self.ends[i] = self.starts[j] - (gap * 0.5).min(REPRESS_RELEASE);
        }
        self
    }

    fn last_start(&self) -> f64 {
        self.starts.iter().copied().fold(0.0, f64::max)
    }
}

/// One card's sequence in flight.
#[derive(Clone, Debug, Default)]
pub struct OutSeqRun {
    active: bool,
    /// Seconds since the gate rose. f64: a held sequence can run for hours
    /// (see the no-f32-clocks rule).
    elapsed: f64,
    /// When the gate fell, in `elapsed` time.
    fell: Option<f64>,
    prev_gate: bool,
}

/// Advance a card's timeline by `dt` with this tick's `gate`, and say which
/// of its items are on. `cancel_early`: letting go before the last item has
/// started drops the rest.
pub(crate) fn out_seq_tick(
    run: &mut OutSeqRun,
    timing: &OutTiming,
    gate: bool,
    cancel_early: bool,
    dt: f32,
) -> Vec<bool> {
    if gate && !run.prev_gate {
        *run = OutSeqRun { active: true, elapsed: 0.0, fell: None, prev_gate: true };
    } else if !gate && run.prev_gate && run.active {
        run.fell = Some(run.elapsed);
        if cancel_early && run.elapsed < timing.last_start() {
            run.active = false;
        }
    }
    run.prev_gate = gate;
    // A microsecond's slack on every edge: `dt` arrives as f32, and five
    // 10 ms ticks don't quite add up to 50 ms.
    const EPS: f64 = 1e-6;
    let t = run.elapsed + EPS;
    let on: Vec<bool> = timing.starts.iter().zip(&timing.durs).zip(&timing.ends).map(|((&s, d), &e)| {
        run.active && t >= s && t < e && match d {
            Some(d) => t < s + d,
            None => run.fell.map_or(true, |f| t < f + s),
        }
    }).collect();
    // Over once every item has had its time — after the gate fell, for any
    // item that lasts as long as the gate.
    if run.active {
        let done = timing.starts.iter().zip(&timing.durs).zip(&timing.ends).all(|((&s, d), &e)| t >= e || match d {
            Some(d) => t >= s + d,
            None => run.fell.is_some_and(|f| t >= f + s),
        });
        if done {
            run.active = false;
        }
        run.elapsed += dt as f64;
    }
    on
}

/// The `out` pins a card has on this tick: all of them while its gate is,
/// or — for a card with timed outputs — the ones its timeline has on.
/// `list` and `idx` key the card's sequence in `ns.out_seq`.
pub(crate) fn card_outs_on<'a>(
    m: &Value,
    out_pins: &'a [String],
    gate: bool,
    ns: &mut NodeState,
    list: u8,
    idx: usize,
    dt: f32,
) -> Vec<&'a String> {
    let Some(timing) = OutTiming::from_card(m, out_pins.len()).map(|t| t.with_repeats(out_pins)) else {
        // No timeline: a plain chord. Any sequence left from an edit goes.
        ns.out_seq.remove(&(list, idx));
        return if gate { out_pins.iter().collect() } else { Vec::new() };
    };
    let press = PressParams::from_card(m);
    let cancel_early = matches!(press.mode(), PressMode::Down) && !press.is_analog() && press.sustain;
    let run = ns.out_seq.entry((list, idx)).or_default();
    let on = out_seq_tick(run, &timing, gate, cancel_early, dt);
    out_pins.iter().zip(on).filter(|(_, on)| *on).map(|(p, _)| p).collect()
}

#[cfg(test)]
mod out_timing_tests {
    use super::*;

    const DT: f32 = 0.01;

    fn run_for(timing: &OutTiming, gate: impl Fn(usize) -> bool, ticks: usize, cancel: bool) -> Vec<Vec<bool>> {
        let mut run = OutSeqRun::default();
        (0..ticks).map(|i| out_seq_tick(&mut run, timing, gate(i), cancel, DT)).collect()
    }

    #[test]
    fn a_card_without_timing_is_a_plain_chord() {
        let m = serde_json::json!({ "out": ["key_a", "key_b"] });
        assert_eq!(OutTiming::from_card(&m, 2), None);
        let m = serde_json::json!({ "out": ["key_a", "key_b"], "out_gap": [0, 0], "out_dur": [null, null] });
        assert_eq!(OutTiming::from_card(&m, 2), None, "all-default timing is still a chord");
    }

    // "Ctrl, 50 ms, C": C comes in 50 ms after Ctrl, with Ctrl still down,
    // and both follow the input's hold, each shifted by its start.
    #[test]
    fn a_gap_delays_what_follows_and_both_follow_the_hold() {
        let m = serde_json::json!({ "out_gap": [0, 50] });
        let t = OutTiming::from_card(&m, 2).unwrap();
        // Held for 20 ticks (200 ms).
        let on = run_for(&t, |i| i < 20, 40, false);
        assert_eq!(on[0], [true, false]);
        assert_eq!(on[4], [true, false], "40 ms: not yet");
        assert_eq!(on[5], [true, true], "50 ms: C joins Ctrl");
        assert_eq!(on[19], [true, true]);
        assert_eq!(on[20], [false, true], "Ctrl lets go with the input; C 50 ms later");
        assert_eq!(on[24], [false, true]);
        assert_eq!(on[25], [false, false]);
    }

    // An item with its own duration taps for that long, however long the
    // input is held; the sequence plays out after a quick tap.
    #[test]
    fn an_own_duration_taps_and_a_quick_tap_still_plays_it_all() {
        let m = serde_json::json!({ "out_gap": [0, 100], "out_dur": [30, 30] });
        let t = OutTiming::from_card(&m, 2).unwrap();
        let on = run_for(&t, |i| i < 2, 20, false);
        assert_eq!(on[0], [true, false]);
        assert_eq!(on[3], [false, false], "the first tap is 30 ms");
        assert_eq!(on[10], [false, true], "the second comes 100 ms in, input long gone");
        assert_eq!(on[13], [false, false]);
        // Held the whole time: each tap still lasts its 30 ms, once per press.
        let on = run_for(&t, |_| true, 20, false);
        assert_eq!(on[15], [false, false]);
    }

    // Normal mode with Hold: letting go before the last item has started
    // cancels the rest; holding through plays it.
    #[test]
    fn hold_cancels_a_sequence_let_go_early() {
        let m = serde_json::json!({ "out_gap": [0, 100] });
        let t = OutTiming::from_card(&m, 2).unwrap();
        let on = run_for(&t, |i| i < 5, 20, true);
        assert!(on[5..].iter().all(|o| o == &[false, false]), "cancelled at 50 ms");
        let on = run_for(&t, |i| i < 12, 30, true);
        assert_eq!(on[10], [true, true], "held past 100 ms: it plays");
        assert_eq!(on[21], [false, true], "and lets go as it would without Hold");
    }

    // "East, 50 ms, East" held: East goes on, lets go 25 ms in (half the
    // gap), and presses again at 50 ms — two presses, not one long one.
    #[test]
    fn a_repeated_pin_lets_go_before_its_next_press() {
        let outs = vec!["btn_east".to_string(), "btn_east".to_string()];
        let m = serde_json::json!({ "out": outs, "out_gap": [0, 50] });
        let mut ns = NodeState::default();
        let on: Vec<usize> = (0..10).map(|_| card_outs_on(&m, &outs, true, &mut ns, 0, 0, DT).len()).collect();
        assert_eq!(on, [1, 1, 1, 0, 0, 1, 1, 1, 1, 1]);
    }

    // The card-level helper: which pins, and Hold read from the card.
    #[test]
    fn card_outs_on_follows_the_timeline() {
        let outs = vec!["key_ctrl".to_string(), "key_c".to_string()];
        let m = serde_json::json!({ "out": outs, "out_gap": [0, 20] });
        let mut ns = NodeState::default();
        let a = card_outs_on(&m, &outs, true, &mut ns, 0, 0, DT);
        assert_eq!(a, vec![&outs[0]]);
        card_outs_on(&m, &outs, true, &mut ns, 0, 0, DT);
        let c = card_outs_on(&m, &outs, true, &mut ns, 0, 0, DT);
        assert_eq!(c, vec![&outs[0], &outs[1]]);
        let plain = serde_json::json!({ "out": outs });
        assert_eq!(card_outs_on(&plain, &outs, true, &mut ns, 0, 1, DT).len(), 2);
        assert!(card_outs_on(&plain, &outs, false, &mut ns, 0, 1, DT).is_empty());
    }
}
