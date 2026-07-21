//! Helpers used by more than one evaluator: analog input resolution, the
//! per-mapping curve/threshold triad, and touch-combo evaluation.
//!
//! These sit beside the evaluators rather than inside any one of them
//! because Remapper, Touch Zones, Map Action and Lean all reach for them.

use super::*;

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
pub(crate) fn analog_cardinal_input_value(upstream: &HashMap<String, Signal>, pin_id: &str) -> f32 {
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
}

impl MappingShape {
    pub(crate) fn from_card(m: &Value) -> Self {
        Self {
            curve: mapping_curve_pts(m),
            threshold: mapping_threshold(m),
        }
    }

    /// Reshape a magnitude through this card's curve (identity without one).
    pub(crate) fn shaped(&self, mag: f32) -> f32 {
        shape_mag(&self.curve, mag)
    }

    /// Threshold verdict for one analog input pin. `None` means this card has
    /// no threshold, or the pin carries no analog value — the caller then falls
    /// back to the pin's plain held state.
    pub(crate) fn analog_gate(
        &self,
        upstream: &HashMap<String, Signal>,
        pin: &str,
    ) -> Option<bool> {
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
/// deflection or an analog trigger's travel. `None` for digital pins.
pub(crate) fn analog_in_value(upstream: &HashMap<String, Signal>, pin_id: &str) -> Option<f32> {
    if analog_axis_for_cardinal(pin_id).is_some() {
        return Some(analog_cardinal_input_value(upstream, pin_id));
    }
    if matches!(pin_id, "left_trigger" | "right_trigger") {
        return Some(upstream.get(pin_id).map(|s| sig_scalar(*s)).unwrap_or(0.0).clamp(0.0, 1.0));
    }
    None
}

/// True when a pin id is an analog INPUT source — a stick cardinal or an analog
/// trigger. The Remapper/Lean UI uses this to gate analog-only outputs (e.g. the
/// touchpad swipe bindings) so they're only offered once an analog input chord
/// has been captured.
pub fn pin_is_analog_input(pin_id: &str) -> bool {
    analog_axis_for_cardinal(pin_id).is_some()
        || matches!(pin_id, "left_trigger" | "right_trigger")
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
pub(crate) fn eval_touch_combo(in_pins: &[&str], upstream: &HashMap<String, Signal>) -> TouchComboEval {
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
    collector_sigs: &mut HashMap<(String, String), Signal>,
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
            collector_sigs.insert((key.to_string(), ak.to_string()), Signal::Bool(true));
        } else {
            collector_sigs.insert((key.to_string(), ak.to_string()), Signal::Bool(false));
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

