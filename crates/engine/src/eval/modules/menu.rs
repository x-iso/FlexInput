//! The Virtual Menu evaluator: the open/navigate/select state machine and
//! the core pin namespace it publishes into.

use super::*;

/// Evaluate a Virtual Menu node — shared by the top-level and sub-patch loops.
/// Runs the open → hover → select state machine, fires mapping-mode cards,
/// publishes overrides + pointer suppression under `menumap:{uid}`, and
/// returns the typed outputs (Open / Hover + ports-mode zone pins) for wired
/// consumers and the UI mirror (the body's zone-live highlight).
///
/// Control resolution — macro-style targets first, wired pins as alternates:
///   Show    = ("macro", menu:{menu_id}_show) OR wired Show (slot 1)
///   Select  = ("macro", menu:{menu_id}_sel)  OR wired Select (slot 2)
///   Pointer = wired Pointer Vec2 (slot 3) when connected, else the SUM of
///             every enabled source checkbox (`ptr_ls`/`ptr_rs`/`ptr_touch`/
///             `ptr_gyro`): stick deflection past the deadzone maps onto the
///             menu rect (full deflection = rect edge), a touch point adds
///             its absolute pad position as a centered deflection, and gyro
///             integrates rotation rate while open (Pitch+Yaw or Pitch+Roll
///             pairs, matching the 3DOF→2D module, scaled by
///             `ptr_gyro_sens` where 1 ≈ 10×). By default the hover is
///             STICKY — a pointer inside the deadzone (or a lifted finger)
///             keeps the last highlighted zone so flick-and-release selection
///             works; `hover_sticky: false` clears the highlight instead.
pub(crate) fn eval_menu_node(
    snap: &NodeSnap,
    uid: usize,
    inputs: &[Option<Signal>],
    dev_sigs: &HashMap<(String, String), Signal>,
    collector_sigs: &mut HashMap<(String, String), Signal>,
    state: &mut HashMap<usize, NodeState>,
    dt: f32,
) -> Vec<Option<Signal>> {
    use flexinput_core::menu as fm;
    use flexinput_core::touchzones as tz;
    let dev_id = snap.params.get("_automap_device_id").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let collector_id = snap.params.get("_automap_collector_id").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let key = format!("menumap:{}", uid);

    // Upstream snapshot (collector override first, else raw device), owned so
    // the publish pass below can mutate collector_sigs freely.
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
    // Navigation reads: the source-block zeroed the menu's pointer pins in
    // `dev_sigs` (so nothing ELSE in the patch sees them), but the menu is the
    // reason they're blocked and must still steer from them — so it reads the
    // pre-block snapshot, falling back to the (unblocked) bus for everything else.
    let unblocked_nav: HashMap<(String, String), Signal> = state.get(&MACRO_CARRY_UID)
        .map(|s| s.unblocked_src.clone()).unwrap_or_default();
    let read_nav = |pin: &str| -> Option<Signal> {
        unblocked_nav.get(&(dev_id.clone(), pin.to_string())).copied()
            .or_else(|| upstream.get(pin).copied())
    };

    let pstr = |k: &str, d: &'static str| -> String {
        snap.params.get(k).and_then(|v| v.as_str()).unwrap_or(d).to_string()
    };
    let menu_id = pstr("menu_id", "");
    let act = pstr("activation_mode", "hold");
    let sel_on = pstr("select_on", "release");
    let deadzone = snap.params.get("pointer_deadzone").and_then(|v| v.as_f64()).unwrap_or(0.25) as f32;
    let suppress = snap.params.get("suppress_while_open").and_then(|v| v.as_bool()).unwrap_or(true);
    // Suppression scope: "full" blocks every enabled driver while the menu is
    // open; "partial" blocks a driver only while it's actually being used (past
    // its deadzone / tilting / touching), so an idle enabled driver still reaches
    // the game; "latch" gives the FIRST driver to engage exclusive ownership —
    // it alone steers and is blocked, every other driver passes through
    // untouched until the owner disengages (back to deadzone / finger up /
    // gyro cursor re-centred).
    let sup_mode = pstr("suppress_mode", "partial");
    let suppress_full = sup_mode == "full";
    let suppress_latch = sup_mode == "latch";
    // Sticky hover: keep the last highlighted zone when the pointer returns
    // to the deadzone (flick-and-release selection). Off = the highlight
    // clears, and a release inside the deadzone selects nothing.
    let sticky = snap.params.get("hover_sticky").and_then(|v| v.as_bool()).unwrap_or(true);
    // Pointer sources are ADDITIVE checkboxes (any combination sums into one
    // deflection vector). The legacy single-choice `pointer_source` seeds the
    // defaults so pre-existing patches keep their behaviour.
    let legacy_src = pstr("pointer_source", "left_stick");
    let pbp = |k: &str, d: bool| snap.params.get(k).and_then(|v| v.as_bool()).unwrap_or(d);
    let src_ls = pbp("ptr_ls", legacy_src == "left_stick");
    let src_rs = pbp("ptr_rs", legacy_src == "right_stick");
    let src_touch = pbp("ptr_touch", legacy_src == "touch1" || legacy_src == "touch2");
    let touch_which = pstr("ptr_touch_which",
        if legacy_src == "touch2" { "touch2" } else { "touch1" });
    let src_gyro = pbp("ptr_gyro", false);
    let gyro_axes = pstr("ptr_gyro_axes", "pitch_yaw");
    // Gyro pointer sensitivity: 1 ≈ 10× the raw rotation rate (the raw IMU
    // stream alone is too slow to sweep the menu). Range 0.5..8, default 4.
    let gyro_sens = snap.params.get("ptr_gyro_sens").and_then(|v| v.as_f64()).unwrap_or(4.0) as f32;

    // ── Controls (macro-style targets OR wired pins) ──
    // A macro target resolves to this tick's published value first, else the
    // previous tick's carry-over snapshot. The snapshot is what makes a Select /
    // Show mapping work when the menu sits UPSTREAM of the Remapper that targets
    // it: that Remapper is forced to evaluate AFTER the menu this tick (a
    // feedback cycle), so `collector_sigs` doesn't hold the value yet — one tick
    // stale is imperceptible at kHz. Captured before the mutable `state.entry`
    // below so the immutable borrow ends first (NLL).
    let macro_prev = state.get(&MACRO_CARRY_UID).map(|s| &s.macro_prev);
    let macro_on = |cs: &HashMap<(String, String), Signal>, pin: String| -> bool {
        let key = (flexinput_core::macros::SIGS_NS.to_string(), pin);
        cs.get(&key).map(|s| s.as_bool()).unwrap_or(false)
            || macro_prev.and_then(|m| m.get(&key)).map(|s| s.as_bool()).unwrap_or(false)
    };
    let show_raw = inputs.get(1).and_then(|s| *s).map(|s| s.as_bool()).unwrap_or(false)
        || (!menu_id.is_empty()
            && macro_on(collector_sigs, fm::target_pin(&menu_id, fm::TargetPin::Show)));
    let sel_raw = inputs.get(2).and_then(|s| *s).map(|s| s.as_bool()).unwrap_or(false)
        || (!menu_id.is_empty()
            && macro_on(collector_sigs, fm::target_pin(&menu_id, fm::TargetPin::Select)));
    let wired_ptr: Option<Vec2> = inputs.get(3).and_then(|s| *s)
        .and_then(|s| if let Signal::Vec2(v) = s { Some(v) } else { None });

    // ── State machine slots — created BEFORE the pointer resolves because
    // the gyro source integrates into per-node accumulators and needs
    // prev_open. aux_f32: [0] open, [1] prev_show, [2] prev_sel,
    // [3] hover+1 (0 = none), [4] select-pulse ms left, [5] selected zone+1,
    // [6] prev_click, [7] hover local x, [8] hover local y,
    // [9] selection sequence (increments on each accepted selection — the
    //     overlay's linger animation keys off changes, so it can't miss a
    //     short pulse at low overlay FPS),
    // [10]/[11] gyro pointer accumulator X/Y (integrated rad, reset while
    //     closed so the pointer always starts centered),
    // [12] latch-mode owner (0 = none, 1 = LS, 2 = RS, 3 = touch, 4 = gyro) ──
    const SLOTS: usize = 13;
    const SELECT_PULSE_MS: f32 = 120.0;
    let ns = state.entry(uid).or_insert_with(NodeState::default);
    while ns.aux_f32.len() < SLOTS { ns.aux_f32.push(0.0); }
    let prev_open = ns.aux_f32[0] > 0.5;
    let prev_show = ns.aux_f32[1] > 0.5;
    let prev_sel = ns.aux_f32[2] > 0.5;
    let prev_hover: i32 = ns.aux_f32[3] as i32 - 1;
    let prev_click = ns.aux_f32[6] > 0.5;
    // The touchpad click doubles as the Select gesture, and it may itself be a
    // blocked pin — read it unblocked.
    let click_now = read_nav("btn_touchpad").map(|s| s.as_bool()).unwrap_or(false);

    // ── Pointer → unit point in the menu rect (0..1, y down) + a "touching"
    // gate for activation_mode = touch. Enabled sources SUM into one
    // deflection vector (stick convention, +Y up); the wired Pointer inlet
    // overrides them all. ──
    let stick_read = |name: &str| -> Vec2 {
        if let Some(Signal::Vec2(v)) = read_nav(name) { return v; }
        Vec2::new(
            read_nav(&format!("{name}_x")).map(|s| s.as_float()).unwrap_or(0.0),
            read_nav(&format!("{name}_y")).map(|s| s.as_float()).unwrap_or(0.0),
        )
    };
    // Deflection vector (+Y up) → unit point: full deflection = rect edge.
    let deflect_to_unit = |v: Vec2| -> (f32, f32) {
        ((0.5 + v.x * 0.5).clamp(0.0, 1.0), (0.5 - v.y * 0.5).clamp(0.0, 1.0))
    };
    // Accumulated gyro tilt of this many radians = full deflection.
    const GYRO_FULL_RAD: f32 = 0.35;
    // Gyro rate (post-noise-floor) above this counts as "actively used" for
    // partial suppression — the source-block noise floor already zeros rest.
    const GYRO_ACTIVE_RATE: f32 = 0.05;
    // Per-source "actively used" flags (past deadzone / touching / tilting) —
    // partial suppression blocks only the sources that are actually steering.
    let mut ls_active = false;
    let mut rs_active = false;
    let mut touch_active_now = false;
    let mut gyro_active = false;
    // Latch-mode owner this tick (0 = none, 1 = LS, 2 = RS, 3 = touch,
    // 4 = gyro) — read by the suppression block below.
    let mut latched: u8 = 0;
    let (ptr_unit, touching): (Option<(f32, f32)>, bool) = if let Some(v) = wired_ptr {
        ns.aux_f32[12] = 0.0;
        let on = v.length() > deadzone;
        (if on { Some(deflect_to_unit(v)) } else { None }, on)
    } else {
        // Per-source candidate vectors, summed (or latch-selected) below.
        let mut touch_on = false;
        let ls_vec = if src_ls { stick_read("left_stick") } else { Vec2::ZERO };
        ls_active = src_ls && ls_vec.length() > deadzone;
        let rs_vec = if src_rs { stick_read("right_stick") } else { Vec2::ZERO };
        rs_active = src_rs && rs_vec.length() > deadzone;
        let mut touch_vec = Vec2::ZERO;
        if src_touch {
            let (px, py, pa) = if touch_which == "touch2" {
                ("touch2_x", "touch2_y", "touch2_active")
            } else {
                ("touch1_x", "touch1_y", "touch1_active")
            };
            if read_nav(pa).map(|s| s.as_bool()).unwrap_or(false) {
                touch_on = true;
                touch_active_now = true;
                let (ux, uy) = tz::pad_point_to_unit(
                    read_nav(px).map(|s| s.as_float()).unwrap_or(0.0),
                    read_nav(py).map(|s| s.as_float()).unwrap_or(0.0),
                );
                // Absolute pad position → centered deflection: a lone touch
                // source reproduces the old absolute mapping exactly.
                touch_vec = Vec2::new((ux - 0.5) * 2.0, (0.5 - uy) * 2.0);
            }
        }
        // Gyro rate is read BEFORE the latch decision — the decision needs the
        // gyro "engaged" signal, and integration below is gated on ownership.
        // Axis pairs mirror the 3DOF→2D module: X ← yaw (gz) or roll (gx),
        // Y ← pitch (gy).
        let (g_rate, g_delta) = if src_gyro && prev_open {
            let gx = read_nav("gyro_x").map(|s| s.as_float()).unwrap_or(0.0);
            let gy = read_nav("gyro_y").map(|s| s.as_float()).unwrap_or(0.0);
            let gz = read_nav("gyro_z").map(|s| s.as_float()).unwrap_or(0.0);
            let (dx, dy) = if gyro_axes == "pitch_roll" { (gx, gy) } else { (gz, gy) };
            ((gx * gx + gy * gy + gz * gz).sqrt(), Vec2::new(dx, dy))
        } else {
            (0.0, Vec2::ZERO)
        };
        let gyro_engaged = src_gyro && prev_open
            && (Vec2::new(ns.aux_f32[10], ns.aux_f32[11]).length() > deadzone
                || g_rate > GYRO_ACTIVE_RATE);

        // Latch mode: the FIRST driver to engage owns the menu — it alone
        // steers and gets blocked; the others are ignored here and keep
        // passing to the game until the owner disengages (stick back inside
        // the deadzone / finger up / gyro cursor re-centred), at which point
        // the next engaged driver can take over.
        if suppress_latch {
            latched = if prev_open { ns.aux_f32[12] as u8 } else { 0 };
            let engaged = [false, ls_active, rs_active, touch_active_now, gyro_engaged];
            if latched != 0 && !engaged[latched as usize] { latched = 0; }
            if latched == 0 && prev_open {
                latched = engaged.iter().position(|&e| e).map(|i| i as u8).unwrap_or(0);
            }
        }
        ns.aux_f32[12] = latched as f32;

        // Integrate rotation rate while gyro steers (tilt to point) — in latch
        // mode only while it owns the latch; closed / ignored resets so the
        // pointer starts centered whenever gyro (re)takes control.
        if src_gyro && prev_open && (!suppress_latch || latched == 4) {
            let gain = gyro_sens * 10.0 / GYRO_FULL_RAD;
            ns.aux_f32[10] = (ns.aux_f32[10] + g_delta.x * dt * gain).clamp(-1.5, 1.5);
            ns.aux_f32[11] = (ns.aux_f32[11] + g_delta.y * dt * gain).clamp(-1.5, 1.5);
            // "Actively used" latches off the gyro CURSOR being out of the
            // deadzone, not the rotation rate alone: the accumulator is an
            // integrator, so its deflection persists while the user holds
            // on a target even though the rate drops to ~0 — a rate-only
            // flag flickers there and leaks single ticks of gyro to e.g. a
            // mouse mapping between block requests. The rate term only
            // covers the first few ms of a tilt, before the cursor crosses
            // the deadzone.
            gyro_active = Vec2::new(ns.aux_f32[10], ns.aux_f32[11]).length() > deadzone
                || g_rate > GYRO_ACTIVE_RATE;
        } else {
            ns.aux_f32[10] = 0.0;
            ns.aux_f32[11] = 0.0;
        }
        let gyro_vec = Vec2::new(ns.aux_f32[10], ns.aux_f32[11]);

        let mut v = if suppress_latch {
            match latched {
                1 => ls_vec,
                2 => rs_vec,
                3 => touch_vec,
                4 => gyro_vec,
                _ => Vec2::ZERO,
            }
        } else {
            ls_vec + rs_vec + touch_vec + gyro_vec
        };
        if v.length() > 1.0 { v = v.normalize(); }
        let past_dz = v.length() > deadzone;
        (
            if past_dz || touch_on { Some(deflect_to_unit(v)) } else { None },
            past_dz || touch_on,
        )
    };

    // Zone geometry: explicit BSP tree once partial dividers exist, else the
    // legacy grid (identical to Touch Zones, single field).
    let read_edges = |which: &str| -> Vec<f32> {
        snap.params.get(which).and_then(|v| v.as_array())
            .map(|a| a.iter().filter_map(|x| x.as_f64().map(|f| f as f32)).collect())
            .unwrap_or_default()
    };
    let tree = snap.params.get("zone_tree").and_then(tz::ZoneNode::from_value)
        .unwrap_or_else(|| tz::ZoneNode::from_grid(
            &read_edges("col_edges"), &read_edges("row_edges")));
    // Radial mode: the SAME zone tree projected into polar space — x is the
    // angle (clockwise from 12 o'clock), y the radius past the dead center.
    // Columns are sectors, rows are concentric rings; ids and dividers are
    // shared with grid mode. The dead center — below `pointer_deadzone` of
    // the unit radius — hovers nothing, so a stick can rest without
    // committing.
    let radial = snap.params.get("menu_radial").and_then(|v| v.as_bool()).unwrap_or(false);
    // Angular origin offset (fraction, clockwise): the display rotates the
    // ring by this, so the input mapping must subtract it back out — pushing
    // toward a zone's on-screen direction has to select THAT zone.
    let radial_origin = snap.params.get("menu_radial_origin").and_then(|v| v.as_f64()).unwrap_or(0.0) as f32;

    let open = match act.as_str() {
        "toggle" => prev_open ^ (show_raw && !prev_show),
        "touch" => touching,
        _ => show_raw, // hold
    };

    // Hover: sticky while open (default) — the frame the menu closes still
    // sees the last hover (release-select reads it), then it resets to none.
    // With `hover_sticky` off, a pointer back inside the deadzone clears the
    // highlight instead, so releasing there selects nothing.
    let mut hover: i32 = if prev_open { prev_hover } else { -1 };
    let mut hover_local = (ns.aux_f32[7], ns.aux_f32[8]);
    if open {
        match ptr_unit {
            Some((ux, uy)) => {
                if radial {
                    // Centered vector (unit-rect coords → ±1, +y down); ptr_unit
                    // is already deadzone-gated for sticks, but touch pointers
                    // aren't — gate the dead center here for both.
                    let (cx, cy) = ((ux - 0.5) * 2.0, (uy - 0.5) * 2.0);
                    let (au, mag) = fm::radial_unit(cx, cy);
                    if mag > deadzone {
                        // Radius past the hub → tree y; angle (minus the origin
                        // offset) → tree x. Local coords come from the zone
                        // itself, exactly like grid mode: X across the zone's
                        // arc, Y across its ring band.
                        let rv = ((mag - deadzone) / (1.0 - deadzone).max(1e-3)).clamp(0.0, 0.999);
                        let ax = (au - radial_origin).rem_euclid(1.0).min(0.9999);
                        let (zid, lx, ly) = tree.locate(ax, rv);
                        hover = zid as i32;
                        hover_local = (lx, ly);
                    } else if !sticky {
                        hover = -1;
                    }
                } else {
                    let (zid, lx, ly) = tree.locate(ux, uy);
                    hover = zid as i32;
                    hover_local = (lx, ly);
                }
            }
            None => {
                if !sticky { hover = -1; }
            }
        }
    }

    let select_now = hover >= 0 && match sel_on.as_str() {
        "press" => open && sel_raw && !prev_sel,
        "click" => open && click_now && !prev_click,
        _ => prev_open && !open, // release: the closing edge selects
    };
    if select_now {
        ns.aux_f32[4] = SELECT_PULSE_MS;
        ns.aux_f32[5] = (hover + 1) as f32;
        ns.aux_f32[9] += 1.0;
    }
    let pulse_on = ns.aux_f32[4] > 0.0;
    let selected: i32 = ns.aux_f32[5] as i32 - 1;
    ns.aux_f32[4] = (ns.aux_f32[4] - dt * 1000.0).max(0.0);
    if !open { hover = -1; }

    ns.aux_f32[0] = if open { 1.0 } else { 0.0 };
    ns.aux_f32[1] = if show_raw { 1.0 } else { 0.0 };
    ns.aux_f32[2] = if sel_raw { 1.0 } else { 0.0 };
    ns.aux_f32[3] = (hover + 1) as f32;
    ns.aux_f32[6] = if click_now { 1.0 } else { 0.0 };
    ns.aux_f32[7] = hover_local.0;
    ns.aux_f32[8] = hover_local.1;

    // ── Mapping-mode cards (shared Remapper card schema; trigger tokens
    // "menu_sel" = the select pulse of this card's zone, "menu_hover" = held
    // while the zone is highlighted) ──
    let mapping = pstr("zone_mode", "mapping") == "mapping";
    let cards = snap.params.get("zone_maps").and_then(|v| v.as_array()).cloned().unwrap_or_default();
    let mut button_on: HashMap<String, bool> = HashMap::new();
    if mapping {
        for (i, card) in cards.iter().enumerate() {
            let zone = card.get("z").and_then(|v| v.as_u64()).unwrap_or(0) as i32;
            let trigger = card.get("in").and_then(|v| v.as_array())
                .and_then(|a| a.first()).and_then(|v| v.as_str()).unwrap_or("menu_sel");
            let raw_held = match trigger {
                "menu_hover" => open && hover == zone,
                _ => pulse_on && selected == zone, // menu_sel (default)
            };
            let press = PressParams::from_card(card);
            let held = press.gate(raw_held, press_state_get(ns, i), dt);
            for p in card.get("out").and_then(|v| v.as_array()).into_iter().flatten()
                .filter_map(|v| v.as_str())
            {
                // Macro-style targets (macro ports, OTHER menus) route into
                // the macro namespace; self-targeting is blocked in the UI.
                if is_macro_style_target(p) {
                    if held {
                        merge_macro_scalar(collector_sigs, p, Signal::Bool(true));
                    }
                    continue;
                }
                let e = button_on.entry(p.to_string()).or_insert(false);
                *e = *e || held;
            }
        }
    }

    // Republish the FULL upstream bus under `menumap:{uid}` — a passthrough,
    // exactly like Touch Zones' `touchmap:{uid}`. Previously only card overrides
    // and suppression zeros were written, leaving `menumap:` a SPARSE map: the
    // AutoMap output port had no bus to glow from, and downstream consumers had
    // to fall back to the raw device for every un-overridden pin (so suppression
    // leaked). Card overrides + suppression below overwrite specific pins on top
    // of this complete bus.
    for (pin, sig) in &upstream {
        collector_sigs.insert((key.clone(), pin.clone()), *sig);
    }

    // Card button pins. While the card is active, assert the pin. When the card
    // is INACTIVE we must still drive the pin to OFF unless the passthrough bus
    // already carries it: the select pulse is momentary, and a virtual sink
    // LATCHES the last value for any pin it stops receiving — so a card output
    // the source device never emits (not in `upstream`) would stick "pressed"
    // forever after one selection. A pin that IS on the bus keeps its passthrough
    // value (OR semantics — a real press of the same button still comes through).
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
        } else if !upstream.contains_key(pin) {
            let off = match sig_type {
                SignalType::Float => Signal::Float(0.0),
                SignalType::Int   => Signal::Int(0),
                SignalType::Vec2  => continue,
                _                 => Signal::Bool(false),
            };
            collector_sigs.insert((key.clone(), pin.clone()), off);
        }
    }

    // ── Suppress the pointing inputs steering the menu ──
    //
    // Two layers. (1) A SOURCE-BLOCK request keyed by the physical source device
    // — applied to `dev_sigs` at the start of NEXT tick so the blocked pins reach
    // ONLY the menu's navigation, not a mouse mapping, another module, or any
    // sink (the menu reads the pre-block snapshot to keep steering). (2) Zeroing
    // the same pins on the menu's OWN passthrough now, so a downstream module on
    // the menu's route doesn't react this tick before the 1-tick source-block
    // lands. `suppress_full` blocks every enabled driver; "latch" blocks ONLY
    // the driver currently owning the menu (the others pass untouched);
    // otherwise (partial) only the drivers actually being used (past deadzone /
    // touching / tilting) are blocked, so an idle enabled driver still reaches
    // the game.
    if suppress && open && wired_ptr.is_none() {
        let (block_ls, block_rs, block_touch, block_gyro) = if suppress_latch {
            (latched == 1, latched == 2, latched == 3, latched == 4)
        } else {
            (src_ls    && (suppress_full || ls_active),
             src_rs    && (suppress_full || rs_active),
             src_touch && (suppress_full || touch_active_now),
             src_gyro  && (suppress_full || gyro_active))
        };

        // (2) Zero the blocked pins on our own passthrough bus.
        for (on, name) in [(block_ls, "left_stick"), (block_rs, "right_stick")] {
            if !on { continue; }
            collector_sigs.insert((key.clone(), name.to_string()), Signal::Vec2(Vec2::ZERO));
            collector_sigs.insert((key.clone(), format!("{name}_x")), Signal::Float(0.0));
            collector_sigs.insert((key.clone(), format!("{name}_y")), Signal::Float(0.0));
        }
        if block_touch {
            collector_sigs.insert((key.clone(), format!("{touch_which}_active")), Signal::Bool(false));
            collector_sigs.insert((key.clone(), format!("{touch_which}_x")), Signal::Float(0.0));
            collector_sigs.insert((key.clone(), format!("{touch_which}_y")), Signal::Float(0.0));
            collector_sigs.insert((key.clone(), "btn_touchpad".to_string()), Signal::Bool(false));
        }
        if block_gyro {
            for pin in ["gyro_x", "gyro_y", "gyro_z"] {
                collector_sigs.insert((key.clone(), pin.to_string()), Signal::Float(0.0));
            }
        }

        // (1) Publish the SOURCE-BLOCK request (drained into NodeState::source_block
        // at tick end, applied to dev_sigs next tick).
        if !dev_id.is_empty() {
            let bk = format!("{SRC_BLOCK_PREFIX}{dev_id}");
            let mut blocked: Vec<String> = Vec::new();
            if block_ls { for p in ["left_stick", "left_stick_x", "left_stick_y"] { blocked.push(p.to_string()); } }
            if block_rs { for p in ["right_stick", "right_stick_x", "right_stick_y"] { blocked.push(p.to_string()); } }
            if block_touch {
                blocked.push(format!("{touch_which}_active"));
                blocked.push(format!("{touch_which}_x"));
                blocked.push(format!("{touch_which}_y"));
                blocked.push("btn_touchpad".to_string());
            }
            if block_gyro { for p in ["gyro_x", "gyro_y", "gyro_z"] { blocked.push(p.to_string()); } }
            for p in blocked {
                collector_sigs.insert((bk.clone(), p), Signal::Bool(true));
            }
        }

        // Re-derive stick cardinals from the (now zeroed) axes so a suppressed
        // stick can't leak through synthetic left_stick_up/down/... pins, which
        // a pass-through Collector would otherwise copy verbatim from the bus.
        let mut local: HashMap<String, Signal> = HashMap::new();
        for axis in ["left_stick_x", "left_stick_y", "right_stick_x", "right_stick_y"] {
            if let Some(&sig) = collector_sigs.get(&(key.clone(), axis.to_string())) {
                local.insert(axis.to_string(), sig);
            }
        }
        derive_stick_cardinals(&mut local);
        for (k, v) in local {
            if k.contains("_stick_") && (k.ends_with("_up") || k.ends_with("_down")
                || k.ends_with("_left") || k.ends_with("_right"))
            {
                collector_sigs.insert((key.clone(), k), v);
            }
        }
    }

    // ── Typed outputs: fixed Open/Hover + ports-mode zone pins (TZ vocabulary,
    // field 0). X/Y carry the hovered zone's local pointer coords. ──
    let mut out: Vec<Option<Signal>> = (0..snap.n_outputs).map(|i| {
        let pin_id = snap.output_pin_ids.get(i).map(|s| s.as_str()).unwrap_or("");
        match fm::parse_pin(pin_id) {
            Some(fm::Pin::Open) => return Some(Signal::Bool(open)),
            Some(fm::Pin::Hover) => return Some(Signal::Float(hover as f32)),
            _ => {}
        }
        match tz::parse_pin(pin_id)? {
            tz::Pin::Zone { idx, comp: tz::ZoneComp::Active, .. } =>
                Some(Signal::Bool(open && hover == idx as i32)),
            tz::Pin::Zone { idx, comp: tz::ZoneComp::X, .. } =>
                Some(Signal::Float(if hover == idx as i32 { hover_local.0 } else { 0.0 })),
            tz::Pin::Zone { idx, comp: tz::ZoneComp::Y, .. } =>
                Some(Signal::Float(if hover == idx as i32 { hover_local.1 } else { 0.0 })),
            tz::Pin::Click { .. } => Some(Signal::Bool(pulse_on)),
        }
    }).collect();
    // TWO extra trailing slots beyond the real ports (invisible to the port
    // UI, carried by the last_out mirror; the UI reads them from the END so
    // the count of real ports never matters):
    //   [len-2] last selection as Vec2(zone id, selection seq) — None until
    //           the first selection ever; the overlay lingers the selected
    //           cell when it sees the seq change (`menu_sel_info`).
    //   [len-1] the live pointer as a unit-rect Vec2 (0..1, y down) while the
    //           menu is open — the overlay / body fields draw the
    //           cursor-deflection indicator from it (`menu_pointer`). None
    //           when closed or centered.
    let sel_seq = ns.aux_f32[9];
    let sel_zone: i32 = ns.aux_f32[5] as i32 - 1;
    out.push(if sel_seq > 0.0 && sel_zone >= 0 {
        Some(Signal::Vec2(Vec2::new(sel_zone as f32, sel_seq)))
    } else {
        None
    });
    out.push(match (open, ptr_unit) {
        (true, Some((ux, uy))) => Some(Signal::Vec2(Vec2::new(ux, uy))),
        _ => None,
    });
    out
}

