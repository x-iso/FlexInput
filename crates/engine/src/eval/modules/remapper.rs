//! The Remapper evaluator, and the pass-through + suppression pass that
//! runs alongside it.
//!
//! Both the top-level loop and `eval_subgraph` call these, so the two can
//! never diverge.

use super::*;

/// Evaluate a Remapper node — shared by the top-level loop and the sub-patch
/// (`eval_subgraph`) loop so the two can never diverge. `uid` is the publishing
/// id: `snap.node_uid` at top level, the namespaced uid inside a sub-patch. It
/// keys `collector_sigs["remap:{uid}"]`, the per-node `state`, and `last_outputs`.
pub(crate) fn eval_remapper_node(
    snap: &NodeSnap,
    uid: usize,
    dev_sigs: &HashMap<(String, String), Signal>,
    collector_sigs: &mut HashMap<(String, String), Signal>,
    state: &mut HashMap<usize, NodeState>,
    dt: f32,
) {
            let dev_id = snap.params.get("_automap_device_id").and_then(|v| v.as_str()).unwrap_or("");
            let collector_id = snap.params.get("_automap_collector_id").and_then(|v| v.as_str()).unwrap_or("");
            let mappings = snap.params.get("mappings").and_then(|v| v.as_array()).cloned().unwrap_or_default();
            let key = format!("remap:{}", uid);

            // Snapshot upstream values for every canonical pin once, so we can
            // freely mutate collector_sigs below without aliasing the read side.
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
            vec2_authoritative_axis_fill(&mut upstream, collector_id, &*collector_sigs);
            // Derive synthetic cardinal-direction Bool pins from each stick's
            // (x, y) so they can participate in mapping triggers just like
            // buttons. See `derive_stick_cardinals` for the dominant-axis rule.
            derive_stick_cardinals(&mut upstream);

            // Derive touchpad zone pins. Two parallel variants:
            //   touch_*       — fire whenever a finger is in that zone, click
            //                   or not. Up to 2 zones at once (one per finger).
            //                   No accumulation; transient, instantaneous.
            //   touchpad_*    — fire only while btn_touchpad is held. While
            //                   held, every zone any finger has visited stays
            //                   asserted (swipe accumulation) so a drag
            //                   across all three zones produces a 3-pin chord.
            //                   Release of btn_touchpad clears the accumulator.
            // Per-zone override: if touchpad_N (click variant) fires, touch_N
            // (touch-only) is forced false so a click-mapped zone takes over
            // from a touch-mapped one rather than firing both.
            let touch_click = upstream.get("btn_touchpad")
                .map(|s| s.as_bool()).unwrap_or(false);
            let zone_of_x = |x: f32| -> usize {
                if x < -1.0/3.0 { 0 } else if x > 1.0/3.0 { 2 } else { 1 }
            };
            // Touch-only zones — each active finger asserts exactly one zone
            // (the one its X currently sits in). Moving a finger from zone A
            // to zone B drops A and asserts B for that finger. With two
            // fingers active, two zones can fire simultaneously. No swipe
            // accumulation here — that's reserved for the click variant.
            let mut touch_only = [false; 3];
            for (xpin, apin) in [("touch1_x","touch1_active"),
                                 ("touch2_x","touch2_active")] {
                let active = upstream.get(apin).map(|s| s.as_bool()).unwrap_or(false);
                if !active { continue; }
                let x = upstream.get(xpin).map(|s| sig_scalar(*s)).unwrap_or(0.0);
                touch_only[zone_of_x(x)] = true;
            }
            // Click-variant zones — accumulated in per-node aux_f32.
            let ns = state.entry(uid).or_insert_with(NodeState::default);
            if ns.aux_f32.len() < 3 { ns.aux_f32.resize(3, 0.0); }
            if !touch_click {
                ns.aux_f32[0] = 0.0;
                ns.aux_f32[1] = 0.0;
                ns.aux_f32[2] = 0.0;
            } else {
                for (xpin, apin) in [("touch1_x","touch1_active"),
                                     ("touch2_x","touch2_active")] {
                    let active = upstream.get(apin).map(|s| s.as_bool()).unwrap_or(false);
                    if !active { continue; }
                    let x = upstream.get(xpin).map(|s| sig_scalar(*s)).unwrap_or(0.0);
                    ns.aux_f32[zone_of_x(x)] = 1.0;
                }
            }
            let click_zone = [
                ns.aux_f32[0] > 0.5,
                ns.aux_f32[1] > 0.5,
                ns.aux_f32[2] > 0.5,
            ];
            let any_zone = click_zone[0] || click_zone[1] || click_zone[2];
            // Click suppresses all touch-only zones — once btn_touchpad
            // fires, the click variants own the touchpad.
            if touch_click {
                touch_only[0] = false;
                touch_only[1] = false;
                touch_only[2] = false;
            }
            upstream.insert("touchpad_left".to_string(),   Signal::Bool(click_zone[0]));
            upstream.insert("touchpad_center".to_string(), Signal::Bool(click_zone[1]));
            upstream.insert("touchpad_right".to_string(),  Signal::Bool(click_zone[2]));
            // touchpad_any — "click anywhere on the pad". Available via the
            // Special… dropdown only (not auto-captured) so users opt in.
            // Fires together with the specific-zone pin additively.
            upstream.insert("touchpad_any".to_string(),    Signal::Bool(touch_click && any_zone));
            upstream.insert("touch_left".to_string(),      Signal::Bool(touch_only[0]));
            upstream.insert("touch_center".to_string(),    Signal::Bool(touch_only[1]));
            upstream.insert("touch_right".to_string(),     Signal::Bool(touch_only[2]));

            let read_upstream = |pin_id: &str| -> Option<Signal> { upstream.get(pin_id).copied() };

            // Per-mapping press mode is stored under `mode` + `window_ms` +
            // `sustain` on each mapping. The state machine must run for every
            // mapping every tick (not just claimed ones) so Short / Long /
            // Double detect edges without dropouts. Compute `effective_held`
            // for each in original index order, then run the sort + claim pass
            // using those values instead of re-reading raw input state.
            //
            // Analog mode is gated differently from digital modes:
            //   - Non-cardinal `in` pins must all be held (combo gate).
            //   - If any cardinal `in` pin exists, its axis magnitude must
            //     exceed GESTURE_ACTIVATE_MAG so we know the stick is being
            //     pushed in (one of) the mapped direction(s).
            //   - Pure cardinal `in`: just magnitude check, no gesture trace.
            //   - Press-mode pipeline is bypassed; analog mode owns its own
            //     "active" definition. Turbo on analog button-outputs is
            //     applied during the publish pass below.
            let ns = state.entry(uid).or_insert_with(NodeState::default);
            let effective: Vec<bool> = mappings.iter().enumerate().map(|(i, m)| {
                let in_pins: Vec<&str> = m.get("in").and_then(|v| v.as_array())
                    .map(|a| a.iter().filter_map(|v| v.as_str()).collect())
                    .unwrap_or_default();
                if in_pins.is_empty() { return false; }
                let mode_s = m.get("mode").and_then(|v| v.as_str()).unwrap_or("down");
                if mode_s == "analog" {
                    // Buttons (non-cardinal) all held? Cardinals: any
                    // non-zero magnitude is enough — analog mode passes the
                    // live magnitude through, no activation threshold.
                    let mut has_cardinal = false;
                    let mut any_cardinal_active = false;
                    let mut all_buttons_held = true;
                    for p in &in_pins {
                        if analog_axis_for_cardinal(p).is_some() {
                            has_cardinal = true;
                            if analog_cardinal_input_value(&upstream, p) > 0.0 {
                                any_cardinal_active = true;
                            }
                        } else if !read_upstream(p).map(|s| s.as_bool()).unwrap_or(false) {
                            all_buttons_held = false;
                        }
                    }
                    // Pure-button analog mappings (no cardinal in) reduce to
                    // "all held" — same as Down mode. Reasonable fallback.
                    return all_buttons_held && (!has_cardinal || any_cardinal_active);
                }
                // Stick-gesture path: when every `in` pin is a stick cardinal,
                // the chord can never be "simultaneously held" (a single stick
                // can't be Left AND Right at the same instant). Instead we
                // track which cardinals have been visited during the active
                // gesture and fire when all required cardinals across both
                // sticks have been visited at least once.
                // Manual activation threshold: an explicit "fire at this
                // magnitude" instruction. It BYPASSES the stick-gesture
                // accumulator (visit-all-cardinals semantics conflict with a
                // hold-above-the-line gate) and replaces the built-in
                // cardinal derivation / 0.5 trigger coercion: each analog in
                // pin gates on the card's curve-shaped magnitude crossing the
                // line, releasing the moment it dips back below.
                let thr = mapping_threshold(m);
                let raw_held = if let (Some(required), None) = (gesture_required_bits(&in_pins), thr) {
                    let buttons_held = in_pins.iter().all(|p| {
                        if gesture_pin_to_bit(p).is_some() { return true; }
                        read_upstream(p).map(|s| s.as_bool()).unwrap_or(false)
                    });
                    let visited = gesture_state_get(ns, i);
                    buttons_held && gesture_tick(required, visited, &upstream)
                } else {
                    let curve = mapping_curve_pts(m);
                    in_pins.iter().all(|p| {
                        if let (Some(t), Some(v)) = (thr, analog_in_value(&upstream, p)) {
                            return shape_mag(&curve, v) >= t;
                        }
                        read_upstream(p).map(|s| s.as_bool()).unwrap_or(false)
                    })
                };
                let mode = PressMode::from_str(mode_s);
                let window_ms = m.get("window_ms").and_then(|v| v.as_f64()).unwrap_or(200.0) as f32;
                let sustain   = m.get("sustain").and_then(|v| v.as_bool()).unwrap_or(false);
                let turbo     = m.get("turbo").and_then(|v| v.as_bool()).unwrap_or(false);
                let slots = press_state_get(ns, i);
                let held = apply_press_mode(raw_held, mode, window_ms, sustain, slots, dt);
                if turbo { apply_turbo(held, window_ms, slots, dt) } else { held }
            }).collect();

            // Physical-hold state per mapping, INDEPENDENT of press mode — true
            // whenever the mapping's input chord is currently held/deflected.
            // Used for input SUPPRESSION: a consumed input must stay suppressed
            // for as long as it is held, even when the press-mode gate (on-press
            // pulse, double-tap window, etc.) is momentarily closed. Otherwise
            // the raw input would leak through while the user keeps holding it.
            let held_now: Vec<bool> = mappings.iter().map(|m| {
                let in_pins: Vec<&str> = m.get("in").and_then(|v| v.as_array())
                    .map(|a| a.iter().filter_map(|v| v.as_str()).collect())
                    .unwrap_or_default();
                if in_pins.is_empty() { return false; }
                // Touch-output combos can mix opposite cardinals of one axis
                // (left+right), which can never be "simultaneously held"; use the
                // touch-combo activation rule so their gate buttons + sticks get
                // consumed whenever the combo is active (gate buttons held, analog
                // deflection optional). Otherwise the generic all-held check below
                // would never fire and the buttons would leak through.
                if mapping_targets_touch(m) {
                    return eval_touch_combo(&in_pins, &upstream).active;
                }
                // With a manual threshold, suppression tracks the same
                // shaped-magnitude gate as activation so a below-threshold
                // deflection doesn't consume the input it isn't firing on.
                let thr = mapping_threshold(m);
                let curve = mapping_curve_pts(m);
                in_pins.iter().all(|p| {
                    if let (Some(t), Some(v)) = (thr, analog_in_value(&upstream, p)) {
                        return shape_mag(&curve, v) >= t;
                    }
                    if analog_axis_for_cardinal(p).is_some() {
                        analog_cardinal_input_value(&upstream, p) > 0.0
                    } else {
                        read_upstream(p).map(|s| s.as_bool()).unwrap_or(false)
                    }
                })
            }).collect();

            // Determine which mappings are currently triggered. Sort indices
            // by descending input-set size so longer combos win conflicts;
            // original indices are preserved so we can look up `effective`
            // and mapping fields afterwards.
            let mut sorted_idx: Vec<usize> = (0..mappings.len()).collect();
            sorted_idx.sort_by(|&a, &b| {
                let la = mappings[a].get("in").and_then(|v| v.as_array()).map(|a| a.len()).unwrap_or(0);
                let lb = mappings[b].get("in").and_then(|v| v.as_array()).map(|a| a.len()).unwrap_or(0);
                lb.cmp(&la)
            });

            // Trigger pass 1: identify triggered mappings and the pins they consume.
            //
            // Suppression rule for overlapping mappings:
            //   - A mapping is suppressed iff a STRICTLY LONGER triggered
            //     mapping has already claimed all of its inputs (longer
            //     chord wins over shorter sub-chord).
            //   - Mappings with the SAME input set are allowed to coexist
            //     so users can fan one button out to multiple outputs:
            //     `Y → X` and `Y → Y` both fire when Y is pressed.
            //
            // Analog mappings with IDENTICAL input chords have an extra
            // last-wins override applied during the publish pass below
            // (user-error guard for conflicting analog writes).
            let mut triggered: Vec<(Vec<String>, Vec<String>, bool, usize)> = Vec::new(); // (in, out, is_analog, orig_idx)
            let mut triggered_claims: Vec<(usize, Vec<String>)> = Vec::new();
            for &i in &sorted_idx {
                let m = &mappings[i];
                let in_pins: Vec<String> = m.get("in").and_then(|v| v.as_array())
                    .map(|a| a.iter().filter_map(|v| v.as_str().map(|s| s.to_string())).collect())
                    .unwrap_or_default();
                let out_pins: Vec<String> = m.get("out").and_then(|v| v.as_array())
                    .map(|a| a.iter().filter_map(|v| v.as_str().map(|s| s.to_string())).collect())
                    .unwrap_or_default();
                if in_pins.is_empty() { continue; }
                if !effective[i] { continue; }
                let my_len = in_pins.len();
                let suppressed = triggered_claims.iter().any(|(claim_len, claim_pins)| {
                    *claim_len > my_len && in_pins.iter().all(|p| claim_pins.contains(p))
                });
                if suppressed { continue; }
                let is_analog = m.get("mode").and_then(|v| v.as_str()) == Some("analog");
                let mut sorted_in = in_pins.clone();
                sorted_in.sort();
                triggered_claims.push((my_len, sorted_in));
                triggered.push((in_pins, out_pins, is_analog, i));
            }

            // Claimed inputs split by mode so pass-through suppression for
            // analog cardinal claims can use axis-side clamping rather than
            // hard-zeroing the entire axis.
            //
            // Suppression follows PHYSICAL HOLD (`held_now`), not the press-mode
            // gate (`effective`/`triggered`): once a mapping consumes an input,
            // that input is suppressed for as long as it's held, regardless of
            // press mode. EXCEPTION — an input a mapping routes back to ITSELF
            // (e.g. `dpad_left → dpad_left`, a deliberate pass-through) is NOT
            // suppressed, so the user can keep an input while also reacting to it.
            let mut self_mapped: HashSet<String> = HashSet::new();
            for m in &mappings {
                let ins: Vec<&str> = m.get("in").and_then(|v| v.as_array())
                    .map(|a| a.iter().filter_map(|v| v.as_str()).collect()).unwrap_or_default();
                let outs: Vec<&str> = m.get("out").and_then(|v| v.as_array())
                    .map(|a| a.iter().filter_map(|v| v.as_str()).collect()).unwrap_or_default();
                for p in &ins {
                    if outs.contains(p) { self_mapped.insert((*p).to_string()); }
                }
            }
            let mut claimed_inputs_digital: HashSet<String> = HashSet::new();
            let mut claimed_inputs_analog: HashSet<String>  = HashSet::new();
            for (i, m) in mappings.iter().enumerate() {
                if !held_now[i] { continue; }
                let is_analog = m.get("mode").and_then(|v| v.as_str()) == Some("analog");
                let in_pins: Vec<String> = m.get("in").and_then(|v| v.as_array())
                    .map(|a| a.iter().filter_map(|v| v.as_str().map(|s| s.to_string())).collect())
                    .unwrap_or_default();
                let target = if is_analog { &mut claimed_inputs_analog } else { &mut claimed_inputs_digital };
                for p in in_pins {
                    if self_mapped.contains(&p) { continue; }
                    target.insert(p);
                }
            }
            // Pass-through + per-side suppression (sticks + D-pad) + consumed
            // markers — shared with the sub-patch arm so they never diverge.
            remapper_pass_through_and_suppress(
                &key, &upstream,
                &claimed_inputs_digital, &claimed_inputs_analog,
                collector_sigs,
            );

            // ── Analog publish pass ──────────────────────────────────────
            //
            // Apply identical input+output-chord override (last wins). Build the
            // set of analog mappings to actually emit, suppressing any earlier
            // analog mapping that is a TRUE duplicate (same inputs AND same
            // outputs) of a later one. Mappings sharing an input but targeting
            // different outputs (e.g. left_stick_up→right_trigger alongside
            // left_stick_up→left_stick_up to keep the stick) both fire.
            let mut analog_emit_idx: Vec<usize> = Vec::new();
            {
                // Walk triggered in original mapping order so "later in the
                // user's list wins". `triggered` was built in sorted_idx
                // (longest-first) order; recover original order via the
                // orig_idx we stored.
                let mut analog_indices: Vec<usize> = (0..triggered.len())
                    .filter(|&t| triggered[t].2)
                    .collect();
                analog_indices.sort_by_key(|&t| triggered[t].3);
                let sorted_set = |v: &Vec<String>| -> Vec<String> {
                    let mut s = v.clone(); s.sort(); s
                };
                let mut keep: Vec<bool> = vec![true; analog_indices.len()];
                for a in 0..analog_indices.len() {
                    if !keep[a] { continue; }
                    let (ref ain, ref aout, _, _) = triggered[analog_indices[a]];
                    let (a_in, a_out) = (sorted_set(ain), sorted_set(aout));
                    for b in (a + 1)..analog_indices.len() {
                        let (ref bin, ref bout, _, _) = triggered[analog_indices[b]];
                        if a_in == sorted_set(bin) && a_out == sorted_set(bout) {
                            // Later (higher index) wins → suppress earlier dup.
                            keep[a] = false;
                            break;
                        }
                    }
                }
                for (a, t_idx) in analog_indices.iter().enumerate() {
                    if keep[a] { analog_emit_idx.push(*t_idx); }
                }
            }

            // Accumulate cardinal-axis writes additively; track button-output
            // emissions per output-pin for turbo / sustain handling.
            let mut analog_axis_acc: HashMap<&'static str, f32> = HashMap::new();
            let mut analog_button_out: HashSet<String> = HashSet::new();
            let mut analog_out_pins: HashSet<String> = HashSet::new();
            for &t_idx in &analog_emit_idx {
                let (ref in_pins, ref out_pins, _, orig_i) = triggered[t_idx];
                let m = &mappings[orig_i];
                let turbo  = m.get("turbo").and_then(|v| v.as_bool()).unwrap_or(false);
                let sustain = m.get("sustain").and_then(|v| v.as_bool()).unwrap_or(false);
                let window_ms = m.get("window_ms").and_then(|v| v.as_f64()).unwrap_or(200.0) as f32;
                let slots = press_state_get(ns, orig_i);
                // Per-card response curve + manual threshold: the curve
                // reshapes every magnitude this mapping emits (axis, trigger,
                // macro, pulse rate); the threshold turns digital outs into a
                // plain hold gate on the shaped value (see the button arm).
                let curve = mapping_curve_pts(m);
                let thr = mapping_threshold(m);
                // Zip in↔out by index; drop the excess from whichever side
                // is longer.
                let n = in_pins.len().min(out_pins.len());
                for (in_p, out_p) in in_pins[..n].iter().zip(out_pins[..n].iter()) {
                    // Touchpad zone/swipe outputs are handled by the touchpad
                    // synthesis pass below, not as axis/trigger/button writes.
                    if touchpad_out_kind(out_p).is_some() { continue; }
                    // Macro-port target: publish the live input magnitude into
                    // the macro namespace and skip the bus handling below
                    // (macro pins never reach sinks or the release pass).
                    if is_macro_style_target(out_p) {
                        let mag = if analog_axis_for_cardinal(in_p).is_some() {
                            analog_cardinal_input_value(&upstream, in_p)
                        } else {
                            1.0 // gate buttons all held (checked by effective[])
                        };
                        let mag = shape_mag(&curve, mag);
                        if mag > 0.0 {
                            merge_macro_scalar(collector_sigs, out_p, Signal::Float(mag.min(1.0)));
                        }
                        continue;
                    }
                    analog_out_pins.insert(out_p.clone());
                    let in_is_cardinal  = analog_axis_for_cardinal(in_p).is_some();
                    let out_axis_opt    = analog_axis_for_cardinal(out_p);
                    let out_trigger     = analog_trigger_out(out_p);
                    let mag_from_input = if in_is_cardinal {
                        analog_cardinal_input_value(&upstream, in_p)
                    } else {
                        // Non-cardinal in pin in this slot — when paired with
                        // a cardinal out, drive it at full magnitude while the
                        // gate is open (the effective[] check guaranteed all
                        // non-cardinal buttons are held).
                        1.0
                    };
                    let mag_from_input = shape_mag(&curve, mag_from_input);
                    if let Some((axis_pin, sign)) = out_axis_opt {
                        let contrib = sign * mag_from_input;
                        // Sum across all (mapping × in/out pair) contributions.
                        let entry = analog_axis_acc.entry(axis_pin).or_insert(0.0);
                        *entry += contrib;
                    } else if let Some(trigger_pin) = out_trigger {
                        // One-sided 0..1 trigger axis — drive it with the input's
                        // live magnitude (converts analog stick direction into
                        // analog trigger travel, incl. on pads lacking analog
                        // triggers like Switch Pro).
                        let entry = analog_axis_acc.entry(trigger_pin).or_insert(0.0);
                        *entry += mag_from_input.max(0.0);
                    } else {
                        // Non-cardinal out: button / key.
                        // With a manual threshold, the output is a PLAIN HOLD:
                        // pressed while the shaped magnitude sits on/above the
                        // line, released the moment it dips below (Turbo still
                        // taps while held). Without one, the legacy behaviour:
                        // a freq-modulated tap train (or PWM under Hold) so the
                        // digital destination reflects HOW FAR the stick is
                        // pushed — matching the 3DOF-Lean analog→digital path.
                        let active = if let Some(t) = thr {
                            let held = mag_from_input >= t;
                            if turbo { apply_turbo(held, window_ms, slots, dt) } else { held }
                        } else {
                            analog_digital_pulse(
                                mag_from_input, window_ms, sustain, turbo, slots, dt,
                            )
                        };
                        if active {
                            analog_button_out.insert(out_p.clone());
                        }
                    }
                }
            }
            // Commit axis accumulator: clamp ±1 then write.
            for (axis_pin, v) in &analog_axis_acc {
                let clamped = v.clamp(-1.0, 1.0);
                collector_sigs.insert((key.clone(), (*axis_pin).to_string()), Signal::Float(clamped));
            }
            // Update bundled Vec2 pins so downstream sinks that read the
            // Vec2 form (`left_stick`/`right_stick`) see the analog-driven
            // values too. Without this, the sink's Vec2-vs-axis conflict
            // resolver picks the Vec2 (which still carries the suppressed
            // pass-through) and drops the analog axis writes.
            for (vec2_pin, x_axis, y_axis) in [
                ("left_stick", "left_stick_x", "left_stick_y"),
                ("right_stick", "right_stick_x", "right_stick_y"),
            ] {
                let x_override = analog_axis_acc.get(&x_axis).copied();
                let y_override = analog_axis_acc.get(&y_axis).copied();
                if x_override.is_none() && y_override.is_none() { continue; }
                let cur = collector_sigs.get(&(key.clone(), vec2_pin.to_string()))
                    .and_then(|s| if let Signal::Vec2(v) = s { Some(*v) } else { None })
                    .unwrap_or(Vec2::ZERO);
                let x = x_override.map(|v| v.clamp(-1.0, 1.0)).unwrap_or(cur.x);
                let y = y_override.map(|v| v.clamp(-1.0, 1.0)).unwrap_or(cur.y);
                collector_sigs.insert((key.clone(), vec2_pin.to_string()), Signal::Vec2(Vec2::new(x, y)));
            }

            // ── Digital publish pass (existing semantics) ────────────────
            //
            // Collect every output pin mentioned in any DIGITAL mapping so
            // released ones can publish false/0. Analog-only out pins are
            // handled by the analog pass above.
            let mut digital_all_out_pins: HashSet<String> = HashSet::new();
            for (i, m) in mappings.iter().enumerate() {
                let is_analog = m.get("mode").and_then(|v| v.as_str()) == Some("analog");
                if is_analog { continue; }
                let _ = i;
                if let Some(arr) = m.get("out").and_then(|v| v.as_array()) {
                    for v in arr {
                        if let Some(s) = v.as_str() {
                            if touchpad_out_kind(s).is_some() { continue; } // synthesized below
                            // Macro pins skip the bus release pass entirely —
                            // absent from the macro namespace = released.
                            if is_macro_style_target(s) { continue; }
                            digital_all_out_pins.insert(s.to_string());
                        }
                    }
                }
            }
            let mut digital_asserted: HashSet<String> = HashSet::new();
            for (_, out_pins, is_analog, _) in &triggered {
                if *is_analog { continue; }
                for p in out_pins {
                    if touchpad_out_kind(p).is_some() { continue; } // synthesized below
                    digital_asserted.insert(p.clone());
                }
            }
            // Macro-port targets of triggered digital mappings: publish into
            // the macro namespace (press-mode shaping already applied via
            // `effective[]` → `triggered`). Bus pins continue below.
            for p in &digital_asserted {
                if is_macro_style_target(p) {
                    merge_macro_scalar(collector_sigs, p, Signal::Bool(true));
                }
            }
            for out_pin in &digital_all_out_pins {
                let sig_type = automap::ALL_PINS.iter()
                    .find(|p| p.id == out_pin.as_str())
                    .map(|p| p.signal_type)
                    .unwrap_or(SignalType::Bool);
                let on = digital_asserted.contains(out_pin);
                if on {
                    let sig = match sig_type {
                        SignalType::Float => Signal::Float(1.0),
                        SignalType::Vec2  => continue,
                        SignalType::Int   => Signal::Int(1),
                        _                 => Signal::Bool(true),
                    };
                    collector_sigs.insert((key.clone(), out_pin.clone()), sig);
                } else {
                    if upstream.contains_key(out_pin.as_str()) { continue; }
                    // If an analog mapping has already written this same out
                    // pin (e.g., the user fans a button to it from a different
                    // mapping), don't overwrite with zero.
                    if analog_button_out.contains(out_pin)
                        || analog_axis_acc.iter().any(|(ap, _)| *ap == out_pin.as_str())
                    {
                        continue;
                    }
                    let sig = match sig_type {
                        SignalType::Float => Signal::Float(0.0),
                        SignalType::Vec2  => continue,
                        SignalType::Int   => Signal::Int(0),
                        _                 => Signal::Bool(false),
                    };
                    collector_sigs.insert((key.clone(), out_pin.clone()), sig);
                }
            }

            // ── Analog button-out emissions + release pass ───────────────
            //
            // Bool/Int analog out pins: write true while active. For released
            // analog out pins (mapping inactive this tick), write false/0
            // only when upstream doesn't naturally emit it (mirrors digital
            // release rule).
            let mut analog_button_pins: HashSet<String> = HashSet::new();
            for m in &mappings {
                let is_analog = m.get("mode").and_then(|v| v.as_str()) == Some("analog");
                if !is_analog { continue; }
                if let Some(arr) = m.get("out").and_then(|v| v.as_array()) {
                    for v in arr {
                        if let Some(s) = v.as_str() {
                            // Triggers are analog axes (handled by analog_axis_acc),
                            // not buttons — exclude them from the binary on/off
                            // release pass or it would clobber the analog value.
                            // Macro pins are published via the macro namespace
                            // in the emit loop above, never as bus buttons.
                            if analog_axis_for_cardinal(s).is_none()
                                && analog_trigger_out(s).is_none()
                                && touchpad_out_kind(s).is_none()
                                && !is_macro_style_target(s)
                            {
                                analog_button_pins.insert(s.to_string());
                            }
                        }
                    }
                }
            }
            for out_pin in &analog_button_pins {
                if digital_asserted.contains(out_pin) { continue; } // digital wins for this pin
                let on = analog_button_out.contains(out_pin);
                let sig_type = automap::ALL_PINS.iter()
                    .find(|p| p.id == out_pin.as_str())
                    .map(|p| p.signal_type)
                    .unwrap_or(SignalType::Bool);
                if on {
                    let sig = match sig_type {
                        SignalType::Float => Signal::Float(1.0),
                        SignalType::Vec2  => continue,
                        SignalType::Int   => Signal::Int(1),
                        _                 => Signal::Bool(true),
                    };
                    collector_sigs.insert((key.clone(), out_pin.clone()), sig);
                } else {
                    if upstream.contains_key(out_pin.as_str()) { continue; }
                    let sig = match sig_type {
                        SignalType::Float => Signal::Float(0.0),
                        SignalType::Vec2  => continue,
                        SignalType::Int   => Signal::Int(0),
                        _                 => Signal::Bool(false),
                    };
                    collector_sigs.insert((key.clone(), out_pin.clone()), sig);
                }
            }

            // ── Touchpad output synthesis (zones + analog swipe) ──────────
            //
            // If ANY mapping targets a touchpad zone/swipe pin, the Remapper owns
            // the virtual touchpad. Each touch mapping yields ONE finger; stack up
            // to the 2 hardware touch points (original mapping order). Plain
            // `btn_touchpad` (click) / `btn_mute` are canonical and handled above.
            //
            // Input roles within a touch mapping (this is NOT index-zip):
            //   • BUTTONS gate the finger — all must be held for it to be active;
            //     they never contribute a value (fixes the "stuck at full" bug).
            //   • ANALOG inputs (stick cardinals / triggers) drive the swipe axes,
            //     routed by orientation: horizontal cardinals → swipe_x, vertical
            //     → swipe_y. Both directions of an axis cover both halves (e.g.
            //     left_stick_left AND left_stick_right → full −1..+1 on X).
            //   • A mapping with buttons + analog: the buttons gate (finger down
            //     while held, even centered) and the analog drives the position.
            //   • Analog-only: deflection both activates and positions.
            let has_touch_mappings = mappings.iter().any(mapping_targets_touch);
            if has_touch_mappings {
                let mut fingers: Vec<(f32, f32)> = Vec::new();
                for m in &mappings {
                    if fingers.len() >= 2 { break; }
                    if !mapping_targets_touch(m) { continue; }
                    let out_pins: Vec<&str> = m.get("out").and_then(|v| v.as_array())
                        .map(|a| a.iter().filter_map(|v| v.as_str()).collect()).unwrap_or_default();
                    let in_pins: Vec<&str> = m.get("in").and_then(|v| v.as_array())
                        .map(|a| a.iter().filter_map(|v| v.as_str()).collect()).unwrap_or_default();

                    // Evaluate inputs by role (buttons gate, analog drives axes).
                    let ev = eval_touch_combo(&in_pins, &upstream);
                    if !ev.active { continue; }

                    let mut fx = 0.0f32;
                    let mut fy = 0.0f32;
                    for p in &out_pins {
                        match touchpad_out_kind(p) {
                            Some(TouchOutKind::Zone(zx)) => { fx = zx; }
                            Some(TouchOutKind::SwipeX) => { fx += ev.axis_x; }
                            Some(TouchOutKind::SwipeY) => { fy += ev.axis_y; }
                            None => {}
                        }
                    }
                    fingers.push((fx.clamp(-1.0, 1.0), fy.clamp(-1.0, 1.0)));
                }
                publish_touch_points(&key, &fingers, collector_sigs);
            }
}


/// Shared Remapper pass-through + suppression pass, called identically by the
/// top-level and sub-patch Remapper arms (so the two never diverge). For every
/// canonical pin it writes `collector_sigs[(key, pin)]`:
///   - consumed input pins → explicit off
///   - claimed cardinals → per-side axis/Vec2 clamp + Bool off (sticks + D-pad)
///   - unmapped pins → raw pass-through
/// Then recomputes synthetic stick cardinals from the clamped axes and publishes
/// the consumed-pin markers for downstream Combiner hierarchy suppression.
pub(crate) fn remapper_pass_through_and_suppress(
    key: &str,
    upstream: &HashMap<String, Signal>,
    claimed_digital: &HashSet<String>,
    claimed_analog: &HashSet<String>,
    collector_sigs: &mut HashMap<(String, String), Signal>,
) {
    let mut all_claimed: HashSet<String> = claimed_digital.clone();
    all_claimed.extend(claimed_analog.iter().cloned());
    let suppression = cardinal_suppression(&all_claimed);

    for ap in automap::ALL_PINS {
        let raw = if all_claimed.contains(ap.id) {
            None
        } else {
            upstream.get(ap.id).copied()
        };
        let Some(raw) = raw else {
            if all_claimed.contains(ap.id) {
                let off = match ap.signal_type {
                    SignalType::Bool  => Signal::Bool(false),
                    SignalType::Float => Signal::Float(0.0),
                    SignalType::Vec2  => Signal::Vec2(Vec2::ZERO),
                    SignalType::Int   => Signal::Int(0),
                    _ => continue,
                };
                collector_sigs.insert((key.to_string(), ap.id.to_string()), off);
            }
            continue;
        };
        let sig = suppress_signal_for_pin(ap.id, raw, &suppression);
        collector_sigs.insert((key.to_string(), ap.id.to_string()), sig);
    }

    // Recompute synthetic stick cardinals from the (possibly clamped) axes so
    // downstream consumers see consistent cardinal bools.
    {
        let mut local_up: HashMap<String, Signal> = HashMap::new();
        for axis in ["left_stick_x", "left_stick_y", "right_stick_x", "right_stick_y"] {
            if let Some(&sig) = collector_sigs.get(&(key.to_string(), axis.to_string())) {
                local_up.insert(axis.to_string(), sig);
            }
        }
        derive_stick_cardinals(&mut local_up);
        for (k, v) in local_up {
            if k.contains("_stick_") && (k.ends_with("_up") || k.ends_with("_down")
                || k.ends_with("_left") || k.ends_with("_right"))
            {
                collector_sigs.insert((key.to_string(), k), v);
            }
        }
    }

    publish_consumed_markers(key, claimed_digital, claimed_analog, collector_sigs);
}

/// Apply per-side `CardinalSuppression` to one pin's raw pass-through value:
///   - axis Float (`dpad_x`, `left_stick_y`, …): clamp the consumed side(s).
///   - bundled Vec2 (`dpad`, `left_stick`, …): clamp each component's side(s).
///   - claimed cardinal Bool: forced false.
///   - everything else: unchanged.
/// Only the directions the user mapped are affected; the rest pass through.
pub(crate) fn suppress_signal_for_pin(
    pin_id: &str,
    raw: Signal,
    sup: &CardinalSuppression,
) -> Signal {
    // Claimed cardinal Bool → off.
    if sup.bool_pins.contains(pin_id) {
        return Signal::Bool(false);
    }
    // Axis Float → side clamp.
    if let Some(&(neg, pos)) = sup.axis_sides.get(pin_id) {
        if let Signal::Float(v) = raw {
            return Signal::Float(apply_axis_clamp(v, (neg, pos)));
        }
        return raw;
    }
    // Bundled Vec2 → per-component side clamp.
    let axes: Option<(&str, &str)> = match pin_id {
        "left_stick"  => Some(("left_stick_x",  "left_stick_y")),
        "right_stick" => Some(("right_stick_x", "right_stick_y")),
        "dpad"        => Some(("dpad_x",         "dpad_y")),
        _ => None,
    };
    if let Some((xa, ya)) = axes {
        let xs = sup.axis_sides.get(xa).copied().unwrap_or((false, false));
        let ys = sup.axis_sides.get(ya).copied().unwrap_or((false, false));
        if xs == (false, false) && ys == (false, false) {
            return raw;
        }
        if let Signal::Vec2(v) = raw {
            return Signal::Vec2(Vec2::new(
                apply_axis_clamp(v.x, xs),
                apply_axis_clamp(v.y, ys),
            ));
        }
    }
    raw
}

