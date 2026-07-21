//! The Map Action evaluator — a single mapping row without the Remapper's
//! full pin table.

use super::*;

/// Evaluate a Map Action node — shared by the top-level and sub-patch loops.
/// Returns the 2-element output vec [Bool gate, Float analog]. `uid` is the
/// publishing id (snap.node_uid at top level, namespaced uid in a sub-patch);
/// it keys the per-node `state`.
pub(crate) fn eval_map_action_node(
    snap: &NodeSnap,
    uid: usize,
    dev_sigs: &HashMap<(String, String), Signal>,
    collector_sigs: &HashMap<(String, String), Signal>,
    state: &mut HashMap<usize, NodeState>,
    dt: f32,
) -> Vec<Option<Signal>> {
            let dev_id = snap.params.get("_automap_device_id").and_then(|v| v.as_str()).unwrap_or("");
            let collector_id = snap.params.get("_automap_collector_id").and_then(|v| v.as_str()).unwrap_or("");
            let mappings = snap.params.get("mappings").and_then(|v| v.as_array()).cloned().unwrap_or_default();
            // Snapshot upstream values for every canonical pin once.
            let mut upstream: HashMap<String, Signal> = HashMap::new();
            for ap in automap::ALL_PINS {
                let sig = if !collector_id.is_empty() {
                    collector_sigs.get(&(collector_id.to_string(), ap.id.to_string())).copied()
                } else { None }
                .or_else(|| {
                    if !dev_id.is_empty() {
                        dev_sigs.get(&(dev_id.to_string(), ap.id.to_string())).copied()
                    } else { None }
                });
                if let Some(s) = sig { upstream.insert(ap.id.to_string(), s); }
            }
            // A processed Vec2 on the collector is authoritative over raw axes.
            vec2_authoritative_axis_fill(&mut upstream, collector_id, &collector_sigs);
            // Derive synthetic pins (stick cardinals + touchpad variants)
            derive_stick_cardinals(&mut upstream);
            // Touchpad handling mirrors Remapper's behaviour (click accumulation)
            let touch_click = upstream.get("btn_touchpad").map(|s| s.as_bool()).unwrap_or(false);
            let zone_of_x = |x: f32| -> usize {
                if x < -1.0/3.0 { 0 } else if x > 1.0/3.0 { 2 } else { 1 }
            };
            let mut touch_only = [false; 3];
            for (xpin, apin) in [("touch1_x","touch1_active"), ("touch2_x","touch2_active")] {
                let active = upstream.get(apin).map(|s| s.as_bool()).unwrap_or(false);
                if !active { continue; }
                let x = upstream.get(xpin).map(|s| sig_scalar(*s)).unwrap_or(0.0);
                touch_only[zone_of_x(x)] = true;
            }
            // Click-variant zones are stored in per-node state; reuse NodeState aux_f32
            let ns = state.entry(uid).or_insert_with(NodeState::default);
            if ns.aux_f32.len() < 3 { ns.aux_f32.resize(3, 0.0); }
            if !touch_click {
                ns.aux_f32[0] = 0.0; ns.aux_f32[1] = 0.0; ns.aux_f32[2] = 0.0;
            } else {
                for (xpin, apin) in [("touch1_x","touch1_active"), ("touch2_x","touch2_active")] {
                    let active = upstream.get(apin).map(|s| s.as_bool()).unwrap_or(false);
                    if !active { continue; }
                    let x = upstream.get(xpin).map(|s| sig_scalar(*s)).unwrap_or(0.0);
                    ns.aux_f32[zone_of_x(x)] = 1.0;
                }
            }
            let click_zone = [ ns.aux_f32[0] > 0.5, ns.aux_f32[1] > 0.5, ns.aux_f32[2] > 0.5 ];
            let any_zone = click_zone[0] || click_zone[1] || click_zone[2];
            if touch_click { touch_only = [false;3]; }
            upstream.insert("touchpad_left".to_string(),   Signal::Bool(click_zone[0]));
            upstream.insert("touchpad_center".to_string(), Signal::Bool(click_zone[1]));
            upstream.insert("touchpad_right".to_string(),  Signal::Bool(click_zone[2]));
            upstream.insert("touchpad_any".to_string(),    Signal::Bool(touch_click && any_zone));
            upstream.insert("touch_left".to_string(),      Signal::Bool(touch_only[0]));
            upstream.insert("touch_center".to_string(),    Signal::Bool(touch_only[1]));
            upstream.insert("touch_right".to_string(),     Signal::Bool(touch_only[2]));

            let read_upstream = |pin_id: &str| -> Option<Signal> { upstream.get(pin_id).copied() };

            // Mappings may be in legacy Array<String> form (chord only, mode=down)
            // or in the new Object form `{ in, mode, window_ms, sustain }`.
            //
            // Output signal kind depends on which mode(s) are present:
            //   - All-digital mappings → emit Bool ("any active").
            //   - Any analog mapping present → emit Float (max magnitude
            //     across all active analog mappings, falling back to 1.0
            //     when only a digital mapping is active so digital triggers
            //     still drive Float-consuming wires at full deflection).
            let ns_map = state.entry(uid).or_insert_with(NodeState::default);
            let mut any_trigger = false;
            let mut any_analog_present = false;
            let mut max_analog_mag: f32 = 0.0;
            for (i, m) in mappings.iter().enumerate() {
                let (in_pins, mode_s, window_ms, sustain, turbo) = if let Some(arr) = m.as_array() {
                    let pins: Vec<&str> = arr.iter().filter_map(|v| v.as_str()).collect();
                    (pins, "down", 200.0_f32, false, false)
                } else {
                    let pins: Vec<&str> = m.get("in").and_then(|v| v.as_array())
                        .map(|a| a.iter().filter_map(|v| v.as_str()).collect())
                        .unwrap_or_default();
                    let mode = m.get("mode").and_then(|v| v.as_str()).unwrap_or("down");
                    let win  = m.get("window_ms").and_then(|v| v.as_f64()).unwrap_or(200.0) as f32;
                    let sus  = m.get("sustain").and_then(|v| v.as_bool()).unwrap_or(false);
                    let tur  = m.get("turbo").and_then(|v| v.as_bool()).unwrap_or(false);
                    (pins, mode, win, sus, tur)
                };
                if in_pins.is_empty() { continue; }
                if mode_s == "analog" {
                    any_analog_present = true;
                    // Combo gate (same as Remapper): all non-cardinal pins
                    // held AND any cardinal contributing a non-zero mag.
                    // Track the strongest cardinal magnitude for Float out.
                    let mut has_cardinal = false;
                    let mut any_cardinal_active = false;
                    let mut all_buttons_held = true;
                    let mut local_max: f32 = 0.0;
                    for p in &in_pins {
                        if analog_axis_for_cardinal(p).is_some() {
                            has_cardinal = true;
                            let mag = analog_cardinal_input_value(&upstream, p);
                            if mag > 0.0 { any_cardinal_active = true; }
                            if mag > local_max { local_max = mag; }
                        } else if !read_upstream(p).map(|s| s.as_bool()).unwrap_or(false) {
                            all_buttons_held = false;
                        }
                    }
                    let active = all_buttons_held && (!has_cardinal || any_cardinal_active);
                    // For pure-button analog (no cardinal), magnitude defaults
                    // to 1.0 while gated so the Float output reads full.
                    let mag = if !active {
                        0.0
                    } else if has_cardinal { local_max } else { 1.0 };
                    // out_analog: pure magnitude (max across active mappings).
                    if mag > max_analog_mag { max_analog_mag = mag; }
                    // out (Bool): freq-modulated tap train / PWM (Hold) / ×2
                    // (Turbo) driven by the magnitude, so a digital destination
                    // reflects how far the input is pushed.
                    let slots = press_state_get(ns_map, i);
                    if analog_digital_pulse(mag, window_ms, sustain, turbo, slots, dt) {
                        any_trigger = true;
                    }
                    continue;
                }
                // All-cardinal chords on a single stick can't be
                // simultaneously held — use the gesture-visited bitmap so
                // half-circles and full sweeps complete the combo. Mirrors
                // Remapper's digital path.
                let raw_held = if let Some(required) = gesture_required_bits(&in_pins) {
                    let buttons_held = in_pins.iter().all(|p| {
                        if gesture_pin_to_bit(p).is_some() { return true; }
                        read_upstream(p).map(|s| s.as_bool()).unwrap_or(false)
                    });
                    let visited = gesture_state_get(ns_map, i);
                    buttons_held && gesture_tick(required, visited, &upstream)
                } else {
                    in_pins.iter().all(|p| {
                        read_upstream(p).map(|s| s.as_bool()).unwrap_or(false)
                    })
                };
                let mode = PressMode::from_str(mode_s);
                let slots = press_state_get(ns_map, i);
                let held = apply_press_mode(raw_held, mode, window_ms, sustain, slots, dt);
                let held = if turbo { apply_turbo(held, window_ms, slots, dt) } else { held };
                if held { any_trigger = true; }
            }

            // Two outputs: out (Bool gate/tap-train) + out_analog (Float mag).
            // out_analog falls back to 1.0 when only digital mappings drove the
            // gate so a Float-consuming wire still sees full deflection.
            let analog_mag = if max_analog_mag > 0.0 {
                max_analog_mag
            } else if any_trigger && !any_analog_present { 1.0 } else { max_analog_mag };
            return vec![
                Some(Signal::Bool(any_trigger)),
                Some(Signal::Float(analog_mag.clamp(0.0, 1.0))),
            ];
}

