//! Lean (3DOF) dispatch into the collector signal map.

use super::*;

/// Shared lean-dispatch for the 3DOF module. Called from both the
/// top-level eval loop and the subgraph eval loop with the appropriate
/// UID (snap.node_uid for top-level, ns_uid for subpatches). Writes to
/// `collector_sigs[("lean:UID", pin_id)]` for every output pin named in
/// any `lean_left` / `lean_right` mapping.
pub(crate) fn lean_dispatch_into_collector_sigs(
    snap: &NodeSnap,
    uid: usize,
    node_outputs: &[Option<Signal>],
    node_state: &mut NodeState,
    collector_sigs: &mut HashMap<(String, String), Signal>,
    dt: f32,
) {
    let lean_val = node_outputs.get(3)
        .and_then(|s| *s)
        .map(|s| s.as_float())
        .unwrap_or(0.0);
    let lean_threshold = snap.params.get("lean_threshold")
        .and_then(|v| v.as_f64()).map(|f| f as f32).unwrap_or(0.3);
    let left_active  = lean_val <= -lean_threshold;
    let right_active = lean_val >=  lean_threshold;
    let lean_mag = lean_val.abs().min(1.0);

    let lean_left  = snap.params.get("lean_left")
        .and_then(|v| v.as_array()).cloned().unwrap_or_default();
    let lean_right = snap.params.get("lean_right")
        .and_then(|v| v.as_array()).cloned().unwrap_or_default();

    // Collect every output pin mentioned in any mapping so released ones
    // can publish false/0. Stick cardinals always also publish to their
    // underlying analog axis (Analog and non-Analog modes both emit on
    // the axis — cardinals aren't valid sink pin ids on their own, so
    // without the axis remap nothing reaches the destination device).
    let mut all_out_pins: HashSet<String> = HashSet::new();
    for m in lean_left.iter().chain(lean_right.iter()) {
        if let Some(arr) = m.get("out").and_then(|v| v.as_array()) {
            for v in arr {
                if let Some(s) = v.as_str() {
                    if touchpad_out_kind(s).is_some() { continue; } // synthesized below
                    // Macro pins skip the bus release pass — absent from the
                    // macro namespace = released.
                    if is_macro_style_target(s) { continue; }
                    all_out_pins.insert(s.to_string());
                    if let Some((axis_pin, _)) = analog_axis_for_cardinal(s) {
                        all_out_pins.insert(axis_pin.to_string());
                    }
                }
            }
        }
    }

    let mut asserted: HashMap<String, Signal> = HashMap::new();

    for (side_idx, side_pair) in [
        (left_active, &lean_left), (right_active, &lean_right),
    ].iter().enumerate() {
        let (active, mappings) = side_pair;
        let base_idx = if side_idx == 0 { 0 } else { lean_left.len() };
        for (i, m) in mappings.iter().enumerate() {
            let out_pins: Vec<String> = m.get("out").and_then(|v| v.as_array())
                .map(|a| a.iter().filter_map(|v| v.as_str().map(|s| s.to_string())).collect())
                .unwrap_or_default();
            if out_pins.is_empty() { continue; }
            let press = PressParams::from_card(m);

            let slots = press_state_get(node_state, base_idx + i);

            // Per-card response curve + manual threshold. The curve reshapes
            // the lean magnitude this card emits; a threshold replaces the
            // NODE-level lean_threshold for THIS card's activation, gating on
            // the curve-shaped OUTPUT (dips below → release). `side_sign_ok`
            // is the raw side test (any magnitude), so a card threshold can
            // sit below the node threshold too.
            let curve = mapping_curve_pts(m);
            let thr = mapping_threshold(m);
            let shaped = shape_mag(&curve, lean_mag);
            let side_sign_ok = if side_idx == 0 { lean_val < 0.0 } else { lean_val > 0.0 };

            let (held_now, analog_val_opt): (bool, Option<f32>) = if press.is_analog() {
                let gate = match thr {
                    Some(t) => side_sign_ok && shaped >= t,
                    None => *active && lean_mag >= 0.01,
                };
                if !gate {
                    slots[0] = 0.0;
                    (false, Some(0.0))
                } else {
                    // Manual threshold → plain hold while above the line
                    // (Turbo still taps). Otherwise the shared analog→digital
                    // modulation: Hold → PWM (duty = shaped), Turbo → ×2 max
                    // frequency, plain → tap train whose frequency tracks the
                    // shaped magnitude. Float destinations ignore `pulse_on`
                    // and use the shaped magnitude directly below.
                    let pulse_on = match thr {
                        Some(_) => press.turbo_only(true, slots, dt),
                        None => press.analog_pulse(shaped, slots, dt),
                    };
                    (pulse_on, Some(shaped))
                }
            } else {
                let card_active = match thr {
                    Some(t) => side_sign_ok && shaped >= t,
                    None => *active,
                };
                (press.gate(card_active, slots, dt), None)
            };

            let is_analog_mode = press.is_analog();
            for p in &out_pins {
                // Touchpad zone/swipe outputs are synthesized into touch points
                // after this loop, not emitted as axis/button pins.
                if touchpad_out_kind(p).is_some() { continue; }
                // Macro-port target: Analog mode passes the live lean
                // magnitude (unsigned — the port is bound per-side, so
                // direction is implied by which side's mapping fires); other
                // press modes assert while the shaped gate is open. Macro
                // pins never enter `asserted` — they aren't bus pins.
                if is_macro_style_target(p) {
                    if is_analog_mode {
                        // The activation gate is already encoded upstream:
                        // analog_val_opt is Some(0.0) when the card's gate
                        // (node or per-card threshold) didn't pass.
                        let mag = analog_val_opt.unwrap_or(0.0);
                        if mag > 0.0 {
                            merge_macro_scalar(collector_sigs, p, Signal::Float(mag.min(1.0)));
                        }
                    } else if held_now {
                        merge_macro_scalar(collector_sigs, p, Signal::Bool(true));
                    }
                    continue;
                }
                // Cardinal → analog-axis remap (all press modes):
                // A stick-cardinal like `left_stick_right` represents the
                // user's INTENT to drive that axis in that direction. The
                // cardinal pin id isn't a valid sink pin on any virtual
                // gamepad — the actual emit must go to the underlying
                // axis (left_stick_x / left_stick_y) with the cardinal's
                // sign (right/up = +, left/down = -). In Analog mode the
                // magnitude tracks lean_mag; in other press modes it's a
                // gated full-deflection write (±1.0 when held, 0 when not).
                if let Some((axis_pin, cardinal_sign)) = analog_axis_for_cardinal(p.as_str()) {
                    // Analog mode: analog_val_opt already carries the gated,
                    // curve-shaped magnitude (0.0 when the card's gate —
                    // node or per-card threshold — didn't pass).
                    let mag = if is_analog_mode {
                        analog_val_opt.unwrap_or(1.0)
                    } else if held_now {
                        1.0
                    } else {
                        0.0
                    };
                    if mag > 0.0 {
                        let new_v = cardinal_sign * mag;
                        let sig = Signal::Float(new_v);
                        // Combine if multiple mappings target the same axis
                        // — use the larger-magnitude write (winning sign).
                        asserted
                            .entry(axis_pin.to_string())
                            .and_modify(|existing| {
                                if let Signal::Float(prev) = existing {
                                    if new_v.abs() > prev.abs() {
                                        *existing = Signal::Float(new_v);
                                    }
                                }
                            })
                            .or_insert(sig);
                    }
                    continue;
                }
                let sig_type = automap::ALL_PINS.iter()
                    .find(|x| x.id == p.as_str())
                    .map(|x| x.signal_type).unwrap_or(SignalType::Bool);
                let emit = match (is_analog_mode, sig_type) {
                    // Gate already applied upstream: Some(>0) only while the
                    // card's (node- or threshold-based) activation holds.
                    (true, SignalType::Float) => analog_val_opt.map(|v| v > 0.0).unwrap_or(false),
                    (true, SignalType::Vec2)  => false,
                    (true, _)                 => held_now,
                    (false, _)                => held_now,
                };
                if !emit { continue; }
                let sig = match sig_type {
                    SignalType::Float => {
                        let mag = analog_val_opt.unwrap_or(1.0);
                        let signed = if is_analog_mode {
                            if side_idx == 0 { -mag } else { mag }
                        } else { mag };
                        Signal::Float(signed)
                    }
                    SignalType::Vec2 => continue,
                    SignalType::Int   => Signal::Int(1),
                    _                 => Signal::Bool(true),
                };
                asserted.entry(p.clone()).or_insert(sig);
            }
        }
    }

    let key = format!("lean:{}", uid);
    for p in &all_out_pins {
        let sig_type = automap::ALL_PINS.iter().find(|x| x.id == p.as_str())
            .map(|x| x.signal_type).unwrap_or(SignalType::Bool);
        let sig = asserted.get(p).copied().unwrap_or_else(|| {
            match sig_type {
                SignalType::Float => Signal::Float(0.0),
                SignalType::Vec2  => Signal::Vec2(Vec2::ZERO),
                SignalType::Int   => Signal::Int(0),
                _                 => Signal::Bool(false),
            }
        });
        collector_sigs.insert((key.clone(), p.clone()), sig);
    }

    // ── Touchpad output synthesis (zones + analog swipe) ──────────────────
    // Mirror of the Remapper's pass: if any lean mapping targets a touchpad
    // zone/swipe pin, synthesize up to 2 touch points from the ACTIVE side's
    // mappings (left side = negative X swipe, right side = positive).
    let has_touch_mappings = lean_left.iter().chain(lean_right.iter()).any(|m| {
        m.get("out").and_then(|v| v.as_array()).map(|a| a.iter().any(|v|
            v.as_str().map(|s| touchpad_out_kind(s).is_some()).unwrap_or(false)
        )).unwrap_or(false)
    });
    if has_touch_mappings {
        let mut fingers: Vec<(f32, f32)> = Vec::new();
        'sides: for (side_idx, (active, mappings)) in [
            (left_active, &lean_left), (right_active, &lean_right),
        ].iter().enumerate() {
            if !*active { continue; }
            let swipe_sign = if side_idx == 0 { -1.0 } else { 1.0 };
            for m in *mappings {
                if fingers.len() >= 2 { break 'sides; }
                let out_pins: Vec<String> = m.get("out").and_then(|v| v.as_array())
                    .map(|a| a.iter().filter_map(|v| v.as_str().map(|s| s.to_string())).collect())
                    .unwrap_or_default();
                let mut fx = 0.0f32;
                let mut fy = 0.0f32;
                let mut has = false;
                let mut needs_mag = false;
                for out_p in &out_pins {
                    match touchpad_out_kind(out_p) {
                        Some(TouchOutKind::Zone(zx)) => { fx = zx; has = true; }
                        Some(TouchOutKind::SwipeX) => { fx += swipe_sign * lean_mag; has = true; needs_mag = true; }
                        Some(TouchOutKind::SwipeY) => { fy += swipe_sign * lean_mag; has = true; needs_mag = true; }
                        None => {}
                    }
                }
                if has {
                    if needs_mag && fx.abs() < 1e-3 && fy.abs() < 1e-3 { continue; }
                    fingers.push((fx, fy));
                }
            }
        }
        publish_touch_points(&key, &fingers, collector_sigs);
    }
}

