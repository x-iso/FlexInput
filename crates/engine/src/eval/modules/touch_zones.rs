//! The Touch Zones evaluator (mapping mode): touchpad position resolved
//! to a zone, then to that zone's mapped output.

use super::*;

/// Evaluate a Touch Zones node in MAPPING mode — shared by the top-level and
/// sub-patch loops. Resolves each active finger to its zone (per field), then
/// applies every mapping card, publishing bus overrides into
/// `collector_sigs[("touchmap:{uid}", pin)]` (mirrors [`eval_remapper_node`]).
///
/// Card schema (node.params["zone_maps"], array of objects):
///   { "f": field, "z": zone, "behavior": "button"|"analog"|..., ... }
///   button → { "src": "touch"|"click", "out": [bus_pin, …] }
///   analog → { "out_stick": "left_stick"|"right_stick" }  (absolute: zone-local
///            X/Y → axis pair, +Y = up)
/// Stateful gestures (tap / double-tap / hold / swipe) are handled by a later
/// pass; only `button` and `analog` are wired here.
pub(crate) fn eval_touch_zones_map_node(
    snap: &NodeSnap,
    uid: usize,
    dev_sigs: &HashMap<(String, String), Signal>,
    collector_sigs: &mut HashMap<(String, String), Signal>,
    state: &mut HashMap<usize, NodeState>,
    dt: f32,
) {
    use flexinput_core::touchzones as tz;
    let dev_id = snap.params.get("_automap_device_id").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let collector_id = snap.params.get("_automap_collector_id").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let key = format!("touchmap:{}", uid);

    // Snapshot every canonical upstream pin once (collector override first, else
    // raw device) into an owned map, so the publish pass can mutate collector_sigs
    // without aliasing the read side. Mirrors eval_remapper_node's `upstream`.
    let mut upstream: HashMap<String, Signal> = HashMap::new();
    for ap in automap::ALL_PINS {
        let sig = if !collector_id.is_empty() {
            collector_sigs.get(&(collector_id.clone(), ap.id.to_string())).copied()
        } else { None }
        .or_else(|| {
            if !dev_id.is_empty() {
                dev_sigs.get(&(dev_id.clone(), ap.id.to_string())).copied()
            } else { None }
        });
        if let Some(s) = sig { upstream.insert(ap.id.to_string(), s); }
    }
    let read = |pin: &str| -> Option<Signal> { upstream.get(pin).copied() };
    let read_edges = |field: usize, which: &str| -> Vec<f32> {
        let k = if field == 0 { which.to_string() } else { format!("{which}{field}") };
        snap.params.get(&k).and_then(|v| v.as_array())
            .map(|a| a.iter().filter_map(|x| x.as_f64().map(|f| f as f32)).collect())
            .unwrap_or_default()
    };

    // Resolve which zone each active finger occupies, per field, keeping local
    // coords — identical to the ports-mode arm in compute_node.
    let split = snap.params.get("field_mode").and_then(|v| v.as_str()) == Some("split");
    const SLOTS_PER: usize = 9; // per-finger aux slots (see per-finger loop below)
    // Zones the user marked "hold": once a gesture STARTS in one, that finger
    // stays attributed to it for the whole touch even if it slides into a
    // neighbour — so the neighbour doesn't also fire ("hold zone" option). Only
    // the `zone_hit` gate (tz_touch / tz_click) needs this; analog + swipe are
    // already attributed to the start zone.
    let hold_zones: std::collections::HashSet<(usize, usize)> =
        snap.params.get("hold_zones").and_then(|v| v.as_array()).map(|a| {
            a.iter().filter_map(|p| {
                let q = p.as_array()?;
                Some((q.first()?.as_u64()? as usize, q.get(1)?.as_u64()? as usize))
            }).collect()
        }).unwrap_or_default();
    // Read-only peek at last frame's per-finger tracking (start zone lives in
    // aux_f32[base+4]); absent on the first frame → no holds yet.
    let prev_aux: Vec<f32> = state.get(&uid).map(|s| s.aux_f32.clone()).unwrap_or_default();
    // Zone geometry: an explicit BSP tree (`zone_tree`/`zone_tree{field}`) once the
    // user has added partial dividers, else derived from the legacy grid (lossless
    // migration — leaf ids == the old row-major indices, so cards keep binding).
    let field_tree = |field: usize| -> tz::ZoneNode {
        let key = if field == 0 { "zone_tree".to_string() } else { format!("zone_tree{field}") };
        snap.params.get(&key).and_then(tz::ZoneNode::from_value)
            .unwrap_or_else(|| tz::ZoneNode::from_grid(
                &read_edges(field, "col_edges"), &read_edges(field, "row_edges")))
    };
    let trees = [field_tree(0), field_tree(1)];
    let mut zone_hit: HashMap<(usize, usize), (f32, f32)> = HashMap::new();
    for finger in 0..2usize {
        let (px, py, pa) = [("touch1_x", "touch1_y", "touch1_active"),
                            ("touch2_x", "touch2_y", "touch2_active")][finger];
        let field = if split { finger } else { 0 };
        if !read(pa).map(|s| s.as_bool()).unwrap_or(false) { continue; }
        let (x, y) = tz::pad_point_to_unit(
            read(px).map(|s| s.as_float()).unwrap_or(0.0),
            read(py).map(|s| s.as_float()).unwrap_or(0.0),
        );
        let (idx, lx, ly) = { let (i, lx, ly) = trees[field].locate(x, y); (i as usize, lx, ly) };
        // If this finger was already down and its START zone is a hold zone, lock
        // the hit to that start zone; the wandered-into zone gets no hit from it.
        let base = finger * SLOTS_PER;
        let prev_active = prev_aux.get(base).copied().unwrap_or(0.0) > 0.5;
        let start_zone = prev_aux.get(base + 4).copied().unwrap_or(0.0) as usize;
        let eff = if prev_active && hold_zones.contains(&(field, start_zone)) {
            start_zone
        } else { idx };
        zone_hit.insert((field, eff), (lx, ly));
    }
    let click = |field: usize| -> bool {
        let pin = if field == 0 { "btn_touchpad" } else { "btn_touchpad2" };
        read(pin).map(|s| s.as_bool()).unwrap_or(false)
    };

    // ── Apply mapping cards ───────────────────────────────────────────────
    let cards = snap.params.get("zone_maps").and_then(|v| v.as_array()).cloned().unwrap_or_default();
    // Button out pins: OR every card targeting the same pin so two zones can
    // share a button. `button_pins` tracks the full set for the release pass.
    let mut button_on: HashMap<String, bool> = HashMap::new();
    // Relative analog (adaptive-center): stick target → (x, y). Last card wins.
    let mut sticks: HashMap<&'static str, (f32, f32)> = HashMap::new();
    // Relative mouse delta accumulator. The `mouse`/`mouse_x`/`mouse_y` pins use
    // the +Y-UP convention (the keymouse sink negates y to screen space itself),
    // so we accumulate the deflection directly WITHOUT flipping y.
    let mut mouse_dx = 0.0f32;
    let mut mouse_dy = 0.0f32;
    let mut mouse_active = false;
    // Analog scroll rate from a zone deflection (+Y up, +X right). Published as
    // the Float scroll_y/scroll_x pins; the KB/M sink integrates them over time.
    let mut scroll_vx = 0.0f32;
    let mut scroll_vy = 0.0f32;
    let mut scroll_active = false;
    // Mouse gain. The emitted value stacks with the SINK's own mouse_sensitivity
    // (like gyro / right-stick sources do), so a raw ±1 deflection would be wildly
    // hot at typical sink sensitivities. `TZ_MOUSE_BASE` attenuates a full-zone
    // deflection to a firm-but-controlled velocity comparable to gyro/RS at the
    // same sink sensitivity; the per-node `mouse_speed` multiplier (default 1.0)
    // tunes it from there.
    const TZ_MOUSE_BASE: f32 = 0.03;
    let mouse_speed = snap.params.get("mouse_speed").and_then(|v| v.as_f64()).unwrap_or(1.0) as f32;
    let mouse_gain = mouse_speed * TZ_MOUSE_BASE;
    // Analog scroll shares the same node multiplier so the "Relative sensitivity"
    // slider also scales max scroll speed. The sink applies the per-notch base
    // rate (SCROLL_REF), so here we pass the shaped deflection × the multiplier.

    // Cards use the shared Remapper schema: "in" = trigger token(s), "out" =
    // target bus pins, "mode"/"window_ms"/"sustain"/"turbo" = the Remapper press
    // pipeline. Per card: derive a raw gate from the zone trigger (touch/click),
    // run it through the SAME `apply_press_mode` (+`apply_turbo`) the Remapper
    // uses, then assert the target — button gets the shaped gate, a stick target
    // is driven with the absolute zone-local position while active.
    let ns = state.entry(uid).or_insert_with(NodeState::default);

    // ── Per-finger tracking: swipe detection + relative analog ─────────────
    // Track each finger (touch1/touch2) across frames. On touch-down record its
    // start field/zone/position AND an ADAPTIVE CENTER: if the finger lands in the
    // inner 30% of the zone, that landing point is the center (relative from where
    // you touched); otherwise the zone's geometric center is used. While held we
    // (a) latch a swipe direction once displacement passes a threshold (attributed
    // to the START zone), and (b) emit a relative analog deflection = (current −
    // center) / zone-half-extent, clamped to ±1. 9 aux_f32 slots per finger:
    // [active, sx, sy, field, zone, dir, pulse_ms, cx, cy].
    const SWIPE_THRESH: f32 = 0.18;   // fraction of the field
    const SWIPE_PULSE_MS: f32 = 120.0;
    // Per-zone "adaptive centre" inner fraction (0..1): the central region within
    // which a touchdown becomes the RELATIVE centre. 0 = always the zone centre
    // (absolute deflection across the whole zone); 1 = wherever you land is the
    // centre (fully relative). Stored on the zone's analog card ("adaptive"),
    // edited below the response-curve graph. Default 0.30.
    let adaptive_for = |field: usize, zone: usize| -> f32 {
        cards.iter().filter(|c|
            c.get("f").and_then(|v| v.as_u64()).unwrap_or(0) == field as u64 &&
            c.get("z").and_then(|v| v.as_u64()).unwrap_or(0) == zone as u64)
            .find_map(|c| c.get("adaptive").and_then(|v| v.as_f64()))
            .map(|v| (v as f32).clamp(0.0, 1.0)).unwrap_or(0.30)
    };
    let slots_per = SLOTS_PER;
    while ns.aux_f32.len() < 2 * slots_per { ns.aux_f32.push(0.0); }
    let mut swipes: Vec<(usize, usize, u8)> = Vec::new(); // (field, zone, dir 1=U 2=D 3=L 4=R)
    let mut analog_by_zone: HashMap<(usize, usize), (f32, f32)> = HashMap::new(); // deflection, +Y up
    for finger in 0..2 {
        let (px, py, pa) = [("touch1_x", "touch1_y", "touch1_active"),
                            ("touch2_x", "touch2_y", "touch2_active")][finger];
        let field = if split { finger } else { 0 };
        let base = finger * slots_per;
        let active = read(pa).map(|s| s.as_bool()).unwrap_or(false);
        let prev_active = ns.aux_f32[base] > 0.5;
        if active {
            let (ux, uy) = tz::pad_point_to_unit(
                read(px).map(|s| s.as_float()).unwrap_or(0.0),
                read(py).map(|s| s.as_float()).unwrap_or(0.0));
            if !prev_active {
                let (zid, _, _) = trees[field].locate(ux, uy);
                let zidx = zid as usize;
                let [x0, y0, x1, y1] = trees[field].zone_rect(zid).unwrap_or([0.0, 0.0, 1.0, 1.0]);
                let (zcx, zcy) = ((x0 + x1) * 0.5, (y0 + y1) * 0.5);
                let (hw, hh) = ((x1 - x0) * 0.5, (y1 - y0) * 0.5);
                // Adaptive centre: landing inside the (configurable) inner region
                // → centre = landing (relative); otherwise the zone's centre.
                let inner = adaptive_for(field, zidx);
                let (cx, cy) = if (ux - zcx).abs() <= inner * hw && (uy - zcy).abs() <= inner * hh {
                    (ux, uy)
                } else { (zcx, zcy) };
                ns.aux_f32[base + 1] = ux;
                ns.aux_f32[base + 2] = uy;
                ns.aux_f32[base + 3] = field as f32;
                ns.aux_f32[base + 4] = zidx as f32;
                ns.aux_f32[base + 5] = 0.0;
                ns.aux_f32[base + 6] = 0.0;
                ns.aux_f32[base + 7] = cx;
                ns.aux_f32[base + 8] = cy;
            } else if ns.aux_f32[base + 5] < 0.5 {
                let dx = ux - ns.aux_f32[base + 1];
                let dy = uy - ns.aux_f32[base + 2];
                if dx.abs().max(dy.abs()) > SWIPE_THRESH {
                    // Field space is y-down, so an upward swipe has dy < 0.
                    let dir: u8 = if dx.abs() >= dy.abs() {
                        if dx > 0.0 { 4 } else { 3 }
                    } else if dy < 0.0 { 1 } else { 2 };
                    ns.aux_f32[base + 5] = dir as f32;
                    ns.aux_f32[base + 6] = SWIPE_PULSE_MS;
                }
            }
            ns.aux_f32[base] = 1.0;

            // Relative analog deflection from the adaptive centre, scaled by the
            // START zone's half-extent (so a half-zone move = full deflection).
            let sz = ns.aux_f32[base + 4] as usize;
            let (cx, cy) = (ns.aux_f32[base + 7], ns.aux_f32[base + 8]);
            let [x0, y0, x1, y1] = trees[field].zone_rect(sz as u32).unwrap_or([0.0, 0.0, 1.0, 1.0]);
            let hw = ((x1 - x0) * 0.5).max(1e-3);
            let hh = ((y1 - y0) * 0.5).max(1e-3);
            let ax = ((ux - cx) / hw).clamp(-1.0, 1.0);
            let ay = (-(uy - cy) / hh).clamp(-1.0, 1.0); // +Y up
            analog_by_zone.insert((field, sz), (ax, ay));
        } else {
            ns.aux_f32[base] = 0.0;
        }
        if ns.aux_f32[base + 6] > 0.0 {
            swipes.push((ns.aux_f32[base + 3] as usize,
                         ns.aux_f32[base + 4] as usize,
                         ns.aux_f32[base + 5] as u8));
            ns.aux_f32[base + 6] = (ns.aux_f32[base + 6] - dt * 1000.0).max(0.0);
        }
    }

    for (i, card) in cards.iter().enumerate() {
        let field = card.get("f").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
        let zone = card.get("z").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
        let hit = zone_hit.get(&(field, zone)).copied();
        let trigger = card.get("in").and_then(|v| v.as_array())
            .and_then(|a| a.first()).and_then(|v| v.as_str()).unwrap_or("tz_touch");
        let swipe_code: Option<u8> = match trigger {
            "tz_swipe_up" => Some(1), "tz_swipe_down" => Some(2),
            "tz_swipe_left" => Some(3), "tz_swipe_right" => Some(4),
            _ => None,
        };
        let raw_held = match swipe_code {
            Some(code) => swipes.iter().any(|&(f, z, d)| f == field && z == zone && d == code),
            None => match trigger {
                "tz_click" => hit.is_some() && click(field),
                _          => hit.is_some(), // tz_touch (default)
            },
        };

        let press = PressParams::from_card(card);
        let held = press.gate(raw_held, press_state_get(ns, i), dt);

        // Relative analog deflection for this card's zone (present only while a
        // finger is down in it). Analog outputs ignore the press-mode gate — the
        // contact itself drives them. A per-card response `curve` (points over the
        // 0..1 deflection MAGNITUDE) reshapes the response while keeping direction
        // — the touch-zone analog can't have a Response Curve module wired onto it.
        let curve_pts: Vec<[f32; 2]> = card.get("curve").and_then(|v| v.as_array())
            .map(|a| a.iter().filter_map(|p| {
                let q = p.as_array()?;
                Some([q.first()?.as_f64()? as f32, q.get(1)?.as_f64()? as f32])
            }).collect())
            .unwrap_or_default();
        let deflect = analog_by_zone.get(&(field, zone)).copied().map(|(ax, ay)| {
            if curve_pts.len() >= 2 {
                let mag = (ax * ax + ay * ay).sqrt().min(1.0);
                if mag > 1e-4 {
                    let m2 = sample_curve(&curve_pts, mag, &[]).clamp(0.0, 1.0);
                    let s = m2 / mag;
                    (ax * s, ay * s)
                } else { (ax, ay) }
            } else { (ax, ay) }
        });
        for p in card.get("out").and_then(|v| v.as_array()).into_iter().flatten()
            .filter_map(|v| v.as_str())
        {
            match p {
                "left_stick" | "right_stick" => {
                    if let Some((ax, ay)) = deflect {
                        sticks.insert(if p == "left_stick" { "left_stick" } else { "right_stick" }, (ax, ay));
                    }
                }
                // Relative mouse: deflection → velocity, +Y up (the sink flips to
                // screen). "mouse" drives both axes.
                "mouse" | "mouse_x" | "mouse_y" => {
                    if let Some((ax, ay)) = deflect {
                        if p == "mouse" || p == "mouse_x" { mouse_dx += ax * mouse_gain; }
                        if p == "mouse" || p == "mouse_y" { mouse_dy += ay * mouse_gain; }
                        mouse_active = true;
                    }
                }
                // Analog scroll: the (curve-shaped) deflection IS the scroll rate.
                // +Y up, +X right; the sink applies its own per-notch scaling.
                "scroll_x" | "scroll_y" => {
                    if let Some((ax, ay)) = deflect {
                        if p == "scroll_x" { scroll_vx += ax * mouse_speed; }
                        if p == "scroll_y" { scroll_vy += ay * mouse_speed; }
                        scroll_active = true;
                    }
                }
                _ => {
                    // Macro-port target: the shaped gate drives the Bool
                    // aspect; the zone's (curve-shaped) relative deflection
                    // publishes the Vec2 aspect for Vec2/Float ports. Macro
                    // pins never enter `button_on` — they aren't bus pins.
                    if is_macro_style_target(p) {
                        if held {
                            merge_macro_scalar(collector_sigs, p, Signal::Bool(true));
                        }
                        if let Some((ax, ay)) = deflect {
                            merge_macro_vec2(collector_sigs, p, Vec2::new(ax, ay));
                        }
                        continue;
                    }
                    let e = button_on.entry(p.to_string()).or_insert(false);
                    *e = *e || held;
                }
            }
        }
    }

    // Publish button pins. We OWN each targeted pin: assert true when any card
    // is active, else write the released value only if upstream doesn't already
    // emit it (matches the Remapper release rule so passthrough stays intact).
    for (pin, on) in &button_on {
        let sig_type = automap::ALL_PINS.iter()
            .find(|ap| ap.id == pin.as_str())
            .map(|ap| ap.signal_type).unwrap_or(SignalType::Bool);
        if *on {
            let sig = match sig_type {
                SignalType::Float => Signal::Float(1.0),
                SignalType::Int   => Signal::Int(1),
                SignalType::Vec2  => continue,
                _                 => Signal::Bool(true),
            };
            collector_sigs.insert((key.clone(), pin.clone()), sig);
        } else {
            // Upstream already carries this pin (e.g. a real gamepad button) →
            // leave it to passthrough instead of forcing a released value.
            if read(pin).is_some() { continue; }
            let sig = match sig_type {
                SignalType::Float => Signal::Float(0.0),
                SignalType::Int   => Signal::Int(0),
                SignalType::Vec2  => continue,
                _                 => Signal::Bool(false),
            };
            collector_sigs.insert((key.clone(), pin.clone()), sig);
        }
    }

    // Publish analog sticks (Vec2 authoritative + component floats). Only when a
    // finger is in the zone this frame; absent, the pin falls back to upstream so
    // the physical stick still passes through.
    for (target, (x, y)) in &sticks {
        let (xp, yp) = match *target {
            "left_stick" => ("left_stick_x", "left_stick_y"),
            _            => ("right_stick_x", "right_stick_y"),
        };
        collector_sigs.insert((key.clone(), target.to_string()), Signal::Vec2(Vec2::new(*x, *y)));
        collector_sigs.insert((key.clone(), xp.to_string()), Signal::Float(*x));
        collector_sigs.insert((key.clone(), yp.to_string()), Signal::Float(*y));
    }
    // Publish relative mouse delta (Vec2 authoritative + component floats) while
    // a finger drives it. Absent, the pins fall back to upstream.
    if mouse_active {
        collector_sigs.insert((key.clone(), "mouse".to_string()), Signal::Vec2(Vec2::new(mouse_dx, mouse_dy)));
        collector_sigs.insert((key.clone(), "mouse_x".to_string()), Signal::Float(mouse_dx));
        collector_sigs.insert((key.clone(), "mouse_y".to_string()), Signal::Float(mouse_dy));
    }
    // Publish analog scroll rate while a finger drives it; else fall back upstream.
    if scroll_active {
        collector_sigs.insert((key.clone(), "scroll_x".to_string()), Signal::Float(scroll_vx));
        collector_sigs.insert((key.clone(), "scroll_y".to_string()), Signal::Float(scroll_vy));
    }
}

