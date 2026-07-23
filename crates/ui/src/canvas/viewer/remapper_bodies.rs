//! Remapper and Map Action node bodies.

use super::*;


pub(crate) fn show_remapper_body(
    node_id: NodeId,
    inputs: &[InPin],
    ui: &mut egui::Ui,
    snarl: &mut Snarl<NodeData>,
    live_signals: &std::collections::HashMap<(String, String), Signal>,
    panic_shortcut: &crate::app::PanicShortcut,
    automap_parent: Option<&AutomapGlowParent<'_>>,
) {
    // ── Read current state ─────────────────────────────────────────────────
    let wired = inputs.first().map(|p| !p.remotes.is_empty()).unwrap_or(false);
    let upstream_dev_id = if wired {
        remapper_upstream_device_id(snarl, node_id, 0, automap_parent)
    } else { None };

    let (phase, draft_input, draft_output, mappings, pressed_prev) = snarl.get_node(node_id)
        .map(|n| (
            n.params.get("ui_phase").and_then(|v| v.as_str()).unwrap_or("idle").to_string(),
            remapper_read_str_array(n, "draft_input"),
            remapper_read_str_array(n, "draft_output"),
            n.params.get("mappings").and_then(|v| v.as_array()).cloned().unwrap_or_default(),
            remapper_read_str_array(n, "_pressed_prev"),
        ))
        .unwrap_or_else(|| ("idle".into(), vec![], vec![], vec![], vec![]));

    // ── Capture state machine ──────────────────────────────────────────────
    // Runs whenever a wire is connected. The phase transition idle→capturing
    // happens on connect; ready_to_learn / capture-done on release.
    let mut pressed_now: Vec<String> = match (&upstream_dev_id, wired) {
        (Some(dev), true) => remapper_pressed_now(live_signals, dev),
        _ => Vec::new(),
    };
    // During Learn, merge in live OS keyboard/mouse so the user can map to
    // keys/mouse buttons even when no virtual KB/M sink is in the graph.
    if phase == "learning" {
        for p in remapper_kbm_pressed_now(ui, panic_shortcut) {
            if !pressed_now.iter().any(|q| q == &p) {
                pressed_now.push(p);
            }
        }
    }

    // Is gamepad UI-nav active for the upstream device this frame? While it is,
    // the controller is driving FlexInput's own UI, so the capture state
    // machine must HOLD a latched combo instead of re-capturing every press.
    // The app's nav driver pass-stamps a temp flag per nav device each frame.
    let nav_active_for_device = upstream_dev_id.as_deref().map(|dev| {
        let stamp: Option<u64> = ui.ctx().data(|d|
            d.get_temp(egui::Id::new(("gp_nav_active", dev.to_string()))));
        stamp == Some(ui.ctx().cumulative_pass_nr())
    }).unwrap_or(false);

    // While UI-nav is active, the auto-capture state machine is suppressed (the
    // controller drives the UI). The nav driver arms a one-shot capture by
    // setting `_nav_capture_armed` when the user picks the Learn button with
    // South. CRUCIAL: the Learn press itself is on the controller, so we must
    // NOT begin capturing while that press (or anything) is still held — capture
    // may only open AFTER the device has gone fully idle once post-arm. We track
    // that with `_nav_arm_idle`: set true the first frame the device is empty
    // while armed; only then does `capture_ok` allow a capture to start.
    let nav_capture_armed = snarl.get_node(node_id)
        .and_then(|n| n.params.get("_nav_capture_armed"))
        .and_then(|v| v.as_bool()).unwrap_or(false);
    let nav_arm_idle = snarl.get_node(node_id)
        .and_then(|n| n.params.get("_nav_arm_idle"))
        .and_then(|v| v.as_bool()).unwrap_or(false);
    let capture_ok = !nav_active_for_device || (nav_capture_armed && nav_arm_idle);
    let mut clear_capture_arm = false;
    let mut set_arm_idle: Option<bool> = None;

    // Touchpad zones. Mirror of the rule in `flexinput_engine::eval`
    // (Remapper arm). Two parallel pin variants:
    //   touch_*    — transient: fires whenever a finger is in that zone.
    //                No state. Up to 2 zones at once.
    //   touchpad_* — accumulated: only while btn_touchpad is held; every
    //                zone any finger has visited stays asserted until the
    //                click is released. State held in node params (3-bit
    //                mask `_tp_zones`) so it survives across frames.
    // Per-zone override: touchpad_N firing forces touch_N false, so the
    // click-variant mapping takes over from a touch-only mapping cleanly.
    {
        let prev_mask: u8 = snarl.get_node(node_id)
            .and_then(|n| n.params.get("_tp_zones"))
            .and_then(|v| v.as_u64()).unwrap_or(0) as u8;
        // Read btn_touchpad directly from live_signals — the canonical-pin
        // sweep above filters it out of pressed_now so its presence here
        // wouldn't reflect device state.
        let touch_click = upstream_dev_id.as_deref()
            .and_then(|dev| live_signals.get(&(dev.to_string(), "btn_touchpad".to_string())))
            .map(|s| s.as_bool()).unwrap_or(false);
        let mut click_mask = if touch_click { prev_mask } else { 0 };
        let mut touch_mask: u8 = 0;
        if let Some(dev) = upstream_dev_id.as_deref() {
            let read_f = |pin: &str| -> Option<f32> {
                live_signals.get(&(dev.to_string(), pin.to_string()))
                    .map(|s| match s {
                        Signal::Float(v) => *v,
                        Signal::Vec2(v) => v.x,
                        _ => 0.0,
                    })
            };
            let read_b = |pin: &str| -> bool {
                live_signals.get(&(dev.to_string(), pin.to_string()))
                    .map(|s| s.as_bool()).unwrap_or(false)
            };
            for (xpin, apin) in [("touch1_x","touch1_active"),
                                 ("touch2_x","touch2_active")] {
                if !read_b(apin) { continue; }
                let x = match read_f(xpin) { Some(v) => v, None => continue };
                let idx = if x < -1.0/3.0 { 0 }
                          else if x >  1.0/3.0 { 2 }
                          else { 1 };
                touch_mask |= 1u8 << idx;
                if touch_click { click_mask |= 1u8 << idx; }
            }
        }
        if click_mask != prev_mask {
            if let Some(n) = snarl.get_node_mut(node_id) {
                n.params.insert("_tp_zones".to_string(), Value::from(click_mask as u64));
            }
        }
        // Click suppresses touch-only — see derive in eval.rs.
        let touch_mask = if touch_click { 0 } else { touch_mask };
        let push = |pn: &mut Vec<String>, pin: &str| {
            if !pn.iter().any(|p| p == pin) { pn.push(pin.to_string()); }
        };
        if click_mask & 1 != 0 { push(&mut pressed_now, "touchpad_left"); }
        if click_mask & 2 != 0 { push(&mut pressed_now, "touchpad_center"); }
        if click_mask & 4 != 0 { push(&mut pressed_now, "touchpad_right"); }
        // Click without a detected touch point (e.g. dielectric press, or
        // click registered before the finger contacts the surface) → fall
        // back to the bare btn_touchpad pin so the click still captures.
        // touchpad_any is NOT auto-captured here — it's the Special-dropdown
        // pin used when the user wants a "click anywhere" mapping that
        // additively fires alongside a specific-zone click mapping.
        if touch_click && click_mask == 0 { push(&mut pressed_now, "btn_touchpad"); }
        if touch_mask & 1 != 0 { push(&mut pressed_now, "touch_left"); }
        if touch_mask & 2 != 0 { push(&mut pressed_now, "touch_center"); }
        if touch_mask & 4 != 0 { push(&mut pressed_now, "touch_right"); }
    }

    let mut new_phase = phase.clone();
    let mut new_draft_input = draft_input.clone();
    let mut new_draft_output = draft_output.clone();

    // Click latches the capture into "click mode" for the rest of the session.
    //
    // Rule: once btn_touchpad has been pressed during this capture, the
    // capture is about clicking — any prior touch_* pins are wiped, and
    // touch_* pins are blocked from accumulating for the remainder of the
    // capture (so releasing the click while the finger still rests on the
    // pad doesn't tack a touch_* onto the click chord).
    //
    // The mode-flag is cleared whenever the capture restarts (capturing
    // re-enter from idle / ready_to_learn).
    let click_mode_before = snarl.get_node(node_id)
        .and_then(|n| n.params.get("_tp_click_mode"))
        .and_then(|v| v.as_bool()).unwrap_or(false);
    let touch_click_now = upstream_dev_id.as_deref()
        .and_then(|dev| live_signals.get(&(dev.to_string(), "btn_touchpad".to_string())))
        .map(|s| s.as_bool()).unwrap_or(false);
    let entering_click_mode = touch_click_now && !click_mode_before;
    if entering_click_mode {
        new_draft_input.retain(|p|
            p != "touch_left" && p != "touch_center" && p != "touch_right"
        );
    }
    let click_mode = click_mode_before || touch_click_now;
    if click_mode != click_mode_before {
        if let Some(n) = snarl.get_node_mut(node_id) {
            n.params.insert("_tp_click_mode".to_string(), Value::from(click_mode));
        }
    }
    // While in click mode, drop touch_* from pressed_now so they don't
    // accumulate into the draft during the click+release tail.
    if click_mode {
        pressed_now.retain(|p|
            p != "touch_left" && p != "touch_center" && p != "touch_right"
        );
    }

    // Auto-enter capturing when a wire is connected and we were idle.
    if wired && new_phase == "idle" {
        new_phase = "capturing".to_string();
    }
    // Drop back to idle when wire is disconnected.
    if !wired && new_phase != "idle" {
        new_phase = "idle".to_string();
        new_draft_input.clear();
        new_draft_output.clear();
        if let Some(n) = snarl.get_node_mut(node_id) {
            n.params.insert("_tp_click_mode".to_string(), Value::from(false));
        }
    }

    // The set rising from pressed_prev to pressed_now (new presses this frame).
    let rising: Vec<&String> = pressed_now.iter()
        .filter(|p| !pressed_prev.iter().any(|q| q == *p))
        .collect();
    let prev_was_empty = pressed_prev.is_empty();
    let now_empty = pressed_now.is_empty();

    // Arm-idle handshake: once armed, mark idle the first frame the device is
    // empty (so the Learn press has been released). `capture_ok` next frame then
    // permits a capture. While armed-but-not-idle, no capture starts.
    if nav_capture_armed && !nav_arm_idle && now_empty {
        set_arm_idle = Some(true);
    }

    // touch_* pins are transient (a finger occupies one zone at a time),
    // unlike buttons/sticks which are held. Capture must reflect the
    // current touch zones, not the union across the swipe — otherwise
    // sweeping a finger across all three zones latches all three.
    let is_transient = |p: &str| p == "touch_left" || p == "touch_center" || p == "touch_right";
    let mut reset_click_mode = false;
    match new_phase.as_str() {
        "capturing" => {
            if capture_ok && !rising.is_empty() && prev_was_empty && !new_draft_input.is_empty() {
                // New burst after a previous latched combo → replace. Skipped
                // while UI-nav is active (unless a one-shot capture is armed) so
                // further gamepad use (now driving the UI) doesn't overwrite the
                // in-progress capture.
                new_draft_input = rising.iter().map(|s| (*s).clone()).collect();
                reset_click_mode = true;
            } else if capture_ok && !pressed_now.is_empty() {
                // Drop any transient pins that are no longer asserted —
                // moving a finger between zones must replace, not accumulate.
                new_draft_input.retain(|p| {
                    !is_transient(p) || pressed_now.iter().any(|q| q == p)
                });
                // Accumulate the peak set (sticky for non-transient pins).
                for p in &pressed_now {
                    if !new_draft_input.iter().any(|q| q == p) {
                        new_draft_input.push(p.clone());
                    }
                }
            }
            // Latching: capture completes when nothing is pressed AND nothing
            // is on the touchpad. While click_mode is set, touch_* are
            // stripped from pressed_now, so a click-release with finger still
            // resting would otherwise look "empty" and latch prematurely —
            // wiping the click chord on the next finger movement. Hold the
            // latch until the touchpad is genuinely idle.
            let touchpad_idle = !touch_click_now
                && upstream_dev_id.as_deref().map(|dev| {
                    let a1 = live_signals.get(&(dev.to_string(), "touch1_active".into()))
                        .map(|s| s.as_bool()).unwrap_or(false);
                    let a2 = live_signals.get(&(dev.to_string(), "touch2_active".into()))
                        .map(|s| s.as_bool()).unwrap_or(false);
                    !a1 && !a2
                }).unwrap_or(true);
            if now_empty && touchpad_idle && !new_draft_input.is_empty() {
                new_phase = "ready_to_learn".to_string();
                // Capture is complete and latched — clear click_mode so a
                // fresh touch (with no click) on a new capture can be
                // captured as touch_*. Clear the one-shot arm here (on LATCH,
                // not at capture start) so the whole chord accumulates first.
                reset_click_mode = true;
                if nav_capture_armed { clear_capture_arm = true; }
            }
        }
        "ready_to_learn" => {
            // A new press from idle (prev empty) re-captures. Held frozen while
            // UI-nav is active so the latched combo survives gamepad UI use —
            // unless a one-shot capture was armed (North / Capture button).
            if capture_ok && !rising.is_empty() && prev_was_empty {
                new_phase = "capturing".to_string();
                new_draft_input = rising.iter().map(|s| (*s).clone()).collect();
                reset_click_mode = true;
            }
        }
        "learning" => {
            // Output capture. Gated by `capture_ok` so that in nav mode the
            // Learn-button press isn't captured — capture only opens after the
            // arm-idle handshake (Learn press released, device idle once).
            if capture_ok && !rising.is_empty() && prev_was_empty && !new_draft_output.is_empty() {
                new_draft_output = rising.iter().map(|s| (*s).clone()).collect();
            } else if capture_ok && !pressed_now.is_empty() {
                for p in &pressed_now {
                    if !new_draft_output.iter().any(|q| q == p) {
                        new_draft_output.push(p.clone());
                    }
                }
            }
            // Output latches (clears the one-shot arm) when the device returns to
            // idle with a non-empty output draft, so a held chord accumulates
            // fully first; the user then clicks Add. Stays in `learning`.
            if nav_capture_armed && now_empty && !new_draft_output.is_empty() {
                clear_capture_arm = true;
            }
        }
        _ => {}
    }

    // Persist state machine results before rendering controls.
    if let Some(node) = snarl.get_node_mut(node_id) {
        node.params.insert("ui_phase".to_string(), Value::String(new_phase.clone()));
        remapper_write_str_array(node, "draft_input", &new_draft_input);
        remapper_write_str_array(node, "draft_output", &new_draft_output);
        remapper_write_str_array(node, "_pressed_prev", &pressed_now);
        if reset_click_mode {
            node.params.insert("_tp_click_mode".to_string(), Value::from(false));
        }
        if let Some(v) = set_arm_idle {
            node.params.insert("_nav_arm_idle".to_string(), Value::from(v));
        }
        if clear_capture_arm {
            node.params.insert("_nav_capture_armed".to_string(), Value::from(false));
            node.params.insert("_nav_arm_idle".to_string(), Value::from(false));
        }
    }

    // ── Render ─────────────────────────────────────────────────────────────
    let skin_param = snarl.get_node(node_id)
        .and_then(|n| n.params.get("skin").and_then(|v| v.as_str()).map(|s| s.to_string()))
        .unwrap_or_else(|| "auto".to_string());
    let skin = remapper_resolve_skin(snarl, node_id, &skin_param, automap_parent);

    // Allocate a fixed-width sub-UI so the body's measured min_rect is
    // bounded — egui-snarl reports body width as body_ui.min_rect, and a
    // bare `ui.vertical` with set_min_width fills the parent's available
    // width, making the node permanently stuck wide once it grows.
    //
    // For HEIGHT: use a tiny sentinel (1px). egui's `allocate_ui_with_layout`
    // takes a *desired* size — when contents are larger the Ui grows to fit.
    // Using a small desired height means the body never reserves dead space
    // and the rect returned to snarl matches actual content height. (Earlier
    // versions read `available_height` which created a feedback loop: each
    // frame snarl reported a taller payload_rect, so the body grew by that.)
    const BODY_W: f32 = 380.0;
    let body_resp = ui.allocate_ui_with_layout(
        egui::vec2(BODY_W, 1.0),
        egui::Layout::top_down(egui::Align::Min),
        |ui| {
        ui.set_min_width(BODY_W);

        // Status line.
        let blue = Color32::from_rgb(106, 167, 255);
        let green = Color32::from_rgb(127, 201, 127);
        let (status_txt, status_col): (String, Color32) = if !wired {
            ("Connect Auto-Map wire to start mapping".into(), Color32::from_rgb(232, 180, 65))
        } else {
            match new_phase.as_str() {
                // Before capture is armed (gamepad nav: Learn not yet pressed)
                // we prompt for Learn; once armed/capture is open, prompt for the
                // button combo.
                "capturing" if new_draft_input.is_empty() => {
                    if nav_active_for_device && !capture_ok {
                        ("Press Learn to start input capture".into(), blue)
                    } else {
                        ("Press a button or combination".into(), blue)
                    }
                }
                "ready_to_learn" =>
                    ("Captured — click Learn (press again to re-capture)".into(), green),
                "learning" if new_draft_output.is_empty() =>
                    ("Press target key or button".into(), blue),
                "learning" =>
                    ("Captured output — click Add".into(), green),
                _ => (String::new(), Color32::TRANSPARENT),
            }
        };
        if !status_txt.is_empty() {
            ui.label(egui::RichText::new(status_txt).size(13.0).color(status_col));
        }
        let _ = upstream_dev_id;
        let _ = &pressed_now;

        // Draft input chips (only if non-empty).
        if !new_draft_input.is_empty() {
            ui.horizontal_wrapped(|ui| {
                remapper_render_chord(ui, &new_draft_input, skin);
            });
        }

        // Draft output row (during learn).
        if new_phase == "learning" {
            ui.horizontal_wrapped(|ui| {
                remapper_render_arrow(ui);
                if new_draft_output.is_empty() {
                    ui.label(egui::RichText::new("…").size(13.0).weak().italics());
                } else {
                    for (i, pin) in new_draft_output.iter().enumerate() {
                        if i > 0 { ui.label(egui::RichText::new("+").size(14.0).strong().color(Color32::WHITE)); }
                        remapper_render_chip(ui, pin, skin);
                    }
                }
            });
        }

        ui.add_space(2.0);

        // Action row. "Learn" is context-aware:
        //   • capturing + empty draft → arm INPUT capture (needed in nav mode
        //     where auto-capture is suppressed; harmless otherwise).
        //   • ready_to_learn → start OUTPUT learning.
        //   • learning → Stop.
        // The three controls (Learn / Special / Add) are also gamepad-activatable
        // via `_nav_act_learn|special|add` flags the nav driver sets on South,
        // and their rects are published so the driver can glow the focused one.
        let in_learning = new_phase == "learning";
        let learn_enabled = new_phase == "ready_to_learn";
        let need_input_arm = new_phase != "ready_to_learn" && new_phase != "learning"
            && new_draft_input.is_empty();
        let add_enabled = (in_learning && !new_draft_output.is_empty())
            || (learn_enabled && !new_draft_output.is_empty());

        // A draft exists if either chord has content — Clear is shown then.
        let has_draft = !new_draft_input.is_empty() || !new_draft_output.is_empty()
            || new_phase == "ready_to_learn" || new_phase == "learning";

        // Consume one-shot gamepad activation flags.
        let (act_learn, act_special, act_add, act_clear) = {
            let n = snarl.get_node(node_id);
            let g = |k: &str| n.and_then(|n| n.params.get(k)).and_then(|v| v.as_bool()).unwrap_or(false);
            (g("_nav_act_learn"), g("_nav_act_special"), g("_nav_act_add"), g("_nav_act_clear"))
        };
        if act_learn || act_special || act_add || act_clear {
            if let Some(node) = snarl.get_node_mut(node_id) {
                node.params.insert("_nav_act_learn".into(), Value::from(false));
                node.params.insert("_nav_act_special".into(), Value::from(false));
                node.params.insert("_nav_act_add".into(), Value::from(false));
                node.params.insert("_nav_act_clear".into(), Value::from(false));
            }
        }

        let mut learn_rect = egui::Rect::NOTHING;
        let mut special_rect = egui::Rect::NOTHING;
        let mut add_rect = egui::Rect::NOTHING;
        let mut clear_rect = egui::Rect::NOTHING;
        ui.horizontal(|ui| {
            let learn_label = if in_learning { "Stop" } else { "Learn" };
            let learn_btn = ui.add_enabled(
                true,
                egui::Button::new(egui::RichText::new(learn_label).size(13.0)),
            );
            learn_rect = learn_btn.rect;
            if learn_btn.clicked() || act_learn {
                if let Some(node) = snarl.get_node_mut(node_id) {
                    if in_learning {
                        // Stop → keep latched input, drop output draft.
                        node.params.insert("ui_phase".to_string(), Value::String("ready_to_learn".to_string()));
                        remapper_write_str_array(node, "draft_output", &[]);
                    } else if learn_enabled {
                        // Input latched → start output learning + arm capture.
                        // arm_idle=false so capture waits for the Learn press to
                        // release before it begins.
                        node.params.insert("ui_phase".to_string(), Value::String("learning".to_string()));
                        remapper_write_str_array(node, "draft_output", &[]);
                        node.params.insert("_nav_capture_armed".to_string(), Value::from(true));
                        node.params.insert("_nav_arm_idle".to_string(), Value::from(false));
                    } else {
                        // Start input capture: arm a one-shot so the next chord
                        // is captured (blocks nav until release+latch).
                        node.params.insert("ui_phase".to_string(), Value::String("capturing".to_string()));
                        remapper_write_str_array(node, "draft_input", &[]);
                        node.params.insert("_nav_capture_armed".to_string(), Value::from(true));
                        node.params.insert("_nav_arm_idle".to_string(), Value::from(false));
                    }
                }
                let _ = need_input_arm;
            }

            // Special button — opens the shared KB/M + touchpad picker (mouse OR
            // gamepad South via `_nav_act_special`). Available once input is
            // latched (ready_to_learn) AND during output learning, so the user
            // can pick a mouse/keyboard/touchpad action BEFORE (or instead of)
            // learning a gamepad output chord.
            if in_learning || learn_enabled {
                let special_btn = ui.add(egui::Button::new(
                    egui::RichText::new("Special…").size(13.0)));
                special_rect = special_btn.rect;
                if special_btn.clicked() || act_special {
                    crate::canvas::viewer::request_special_picker(ui.ctx(),
                        crate::canvas::viewer::SpecialPickerRequest {
                            inner: node_id,
                            path: crate::canvas::viewer::subpatch_path(automap_parent),
                            draft_key: "draft_output".to_string(),
                            phase_key: None,
                            touch_zones: false,
                            exclude_pin_prefix: None,
                        });
                }
            }

            // Clear button — abandons the in-progress capture/learn and starts
            // over (back to input capturing, drafts emptied). Shown whenever a
            // draft exists so a botched capture can be reset WITHOUT finishing.
            if has_draft {
                let clear_btn = ui.add(egui::Button::new(egui::RichText::new("Clear").size(13.0)));
                clear_rect = clear_btn.rect;
                if clear_btn.clicked() || act_clear {
                    if let Some(node) = snarl.get_node_mut(node_id) {
                        node.params.insert("ui_phase".to_string(), Value::String("capturing".to_string()));
                        remapper_write_str_array(node, "draft_input", &[]);
                        remapper_write_str_array(node, "draft_output", &[]);
                        remapper_write_str_array(node, "_pressed_prev", &[]);
                        node.params.insert("_nav_capture_armed".to_string(), Value::from(false));
                        node.params.insert("_nav_arm_idle".to_string(), Value::from(false));
                        node.params.insert("_tp_click_mode".to_string(), Value::from(false));
                    }
                }
            }

            // Add button — appends mapping and resets drafts.
            let add_btn = ui.add_enabled(add_enabled, egui::Button::new(egui::RichText::new("Add").size(13.0)));
            add_rect = add_btn.rect;
            if (add_btn.clicked() || act_add) && add_enabled {
                if let Some(node) = snarl.get_node_mut(node_id) {
                    let in_arr: Vec<Value> = new_draft_input.iter()
                        .map(|s| Value::String(s.clone())).collect();
                    let out_arr: Vec<Value> = new_draft_output.iter()
                        .map(|s| Value::String(s.clone())).collect();
                    let mut entry = serde_json::Map::new();
                    entry.insert("in".to_string(), Value::Array(in_arr));
                    entry.insert("out".to_string(), Value::Array(out_arr));
                    // A touchpad swipe output is continuous → force analog mode so
                    // the engine drives the finger by the input's magnitude.
                    if new_draft_output.iter().any(|p| remapper_out_is_swipe(p)) {
                        entry.insert("mode".to_string(), Value::String("analog".to_string()));
                    }
                    let mut all = mappings.clone();
                    all.push(Value::Object(entry));
                    node.params.insert("mappings".to_string(), Value::Array(all));
                    node.params.insert("ui_phase".to_string(), Value::String("capturing".to_string()));
                    remapper_write_str_array(node, "draft_input", &[]);
                    remapper_write_str_array(node, "draft_output", &[]);
                }
            }
        });
        // Publish action-button rects (global) so the nav driver can glow the
        // focused one. Order MUST match `nav_remap_action_items` AND the visual
        // layout: Learn, Special, Clear, Add (entries NOTHING where absent).
        publish_nav_action_rects(ui, node_id, &[learn_rect, special_rect, clear_rect, add_rect]);

        // Mapping list.
        if !mappings.is_empty() {
            ui.add_space(4.0);
            ui.separator();

            // Filter row. The live-input read here is independent of the
            // capture state machine above — the user filters by pressing an
            // input while NOT in learning. Read SOURCE pins only (the upstream
            // device on the wire) — deliberately NOT the OS keyboard/mouse: a
            // Remapper that maps a button → a key injects that key on the
            // virtual sink, and the OS would then report it as "pressed",
            // flickering the filter from source to destination. The source side
            // is all we want to filter by.
            // While UI-nav drives the controller, live presses are navigation,
            // not a filter intent — so filter relative to the LAST CAPTURED chord
            // (the Learn draft) instead. Outside nav, follow live source input.
            let filter_live: Vec<String> = if nav_active_for_device {
                new_draft_input.clone()
            } else {
                match (&upstream_dev_id, wired) {
                    (Some(dev), true) => remapper_pressed_now(live_signals, dev),
                    _ => Vec::new(),
                }
            };
            let filter = mapping_filter_row(
                ui,
                egui::Id::new(("fxi_remap_filter", node_id.0)),
                &format!("({})", mappings.len()),
                &filter_live,
                skin,
            );

            let mut to_remove: Option<usize> = None;
            // Card layout per mapping:
            //   ┌──────────────────────────────────────────────────┐
            //   │ [×] [↓ mode]  time gap [200ms]  hold✐ turbo✐     │
            //   │  in →  [chip] + [chip]                            │
            //   │  out → [chip]                                     │
            //   └──────────────────────────────────────────────────┘
            // Settings always render in the header; the ones that don't apply
            // to the current mode render disabled (grayed). The in/out rows
            // wrap chips when they overflow.
            // Collapse default item_spacing so cards pack tightly. Without
            // this, both the outer top-down layout and the inner horizontal
            // wrapper add ~3px each between siblings.
            ui.spacing_mut().item_spacing.y = 2.0;
            let mut press_mode_changed: Option<(usize, serde_json::Map<String, Value>)> = None;
            // Reordering operates on the full array; only enabled when no
            // filter is narrowing the visible set (so the dragged index maps
            // 1:1 to the underlying array).
            let reorder_enabled = filter.kind == MapFilterKind::All;
            // Output-conflict scan (once): which of this patch's cards write each
            // bus pin, so a card whose out pin is ALSO driven elsewhere gets a ⚠.
            let conflicts = scan_mapping_conflicts(snarl);
            let mut rv = ReorderView::begin(
                ui, egui::Id::new(("fxi_remap_reorder", node_id.0)), reorder_enabled,
            );
            let mut slot = 0usize; // display position among visible cards
            for (i, m) in mappings.iter().enumerate() {
                let in_pins: Vec<String> = m.get("in").and_then(|v| v.as_array())
                    .map(|a| a.iter().filter_map(|v| v.as_str().map(|s| s.to_string())).collect())
                    .unwrap_or_default();
                let out_pins: Vec<String> = m.get("out").and_then(|v| v.as_array())
                    .map(|a| a.iter().filter_map(|v| v.as_str().map(|s| s.to_string())).collect())
                    .unwrap_or_default();

                if !mapping_passes_filter(&filter, &in_pins) { continue; }
                let card_conf = card_conflict_for(&conflicts, node_id, "mappings", i, &out_pins);

                if let Some(h) = rv.gap_before(slot) { draw_insertion_gap(ui, h); }

                let mut working: serde_json::Map<String, Value> = m.as_object().cloned().unwrap_or_default();
                let mut working_changed = false;
                let drag_off = rv.offset_for(i);

                ui.push_id(("fxi_remap_card", node_id.0, i), |ui| {
                    ui.horizontal(|ui| {
                        ui.add_space(6.0);
                        ui.allocate_ui_with_layout(
                            egui::vec2((BODY_W - 18.0).min(358.0), 1.0),
                            egui::Layout::top_down(egui::Align::Min),
                            |ui| {
                                // Per-card response curve + manual activation
                                // threshold — offered whenever an in pin is
                                // analog (stick cardinal / trigger), since
                                // both analog-mode shaping and digital-mode
                                // thresholds key off the input magnitude.
                                let card_analog = in_pins.iter()
                                    .any(|p| flexinput_engine::pin_is_analog_input(p));
                                let result = remapper_mapping_card_pixel(
                                    ui, node_id, i, &mut working,
                                    &in_pins, Some(&out_pins), skin,
                                    true, reorder_enabled, drag_off, "mappings", card_analog,
                                    card_conf.as_ref(),
                                );
                                if result.delete_clicked { to_remove = Some(i); }
                                if result.changed { working_changed = true; }
                                rv.observe(i, &result);
                                if card_analog {
                                    let live = live_analog_in_mag(
                                        live_signals, upstream_dev_id.as_deref(), &in_pins);
                                    let nav_uid = curve_nav_uid(ui.ctx(), node_id, "mappings", i);
                                    if mapping_card_curve_section(
                                        ui, node_id, "mappings", i, &mut working,
                                        true, live, nav_uid, None,
                                    ) {
                                        working_changed = true;
                                    }
                                }
                            },
                        );
                    });
                });

                if working_changed {
                    press_mode_changed = Some((i, working));
                }
                slot += 1;
            }
            if let Some(h) = rv.gap_after_last(slot) { draw_insertion_gap(ui, h); }
            let reorder = rv.finish(ui);
            if let Some((from, to)) = reorder {
                if let Some(node) = snarl.get_node_mut(node_id) {
                    if let Some(Value::Array(arr)) = node.params.get_mut("mappings") {
                        reorder_array(arr, from, to);
                    }
                }
            }
            if let Some(idx) = to_remove {
                if let Some(node) = snarl.get_node_mut(node_id) {
                    if let Some(Value::Array(arr)) = node.params.get_mut("mappings") {
                        if idx < arr.len() { arr.remove(idx); }
                    }
                }
            }
            if let Some((i, obj)) = press_mode_changed {
                if let Some(node) = snarl.get_node_mut(node_id) {
                    if let Some(Value::Array(arr)) = node.params.get_mut("mappings") {
                        if let Some(slot) = arr.get_mut(i) {
                            *slot = Value::Object(obj);
                        }
                    }
                }
            }
        }
    });

    register_exposable_element(ui, node_id, "whole_module", body_resp.response.rect);

    // Request repaint so the state machine ticks each frame — both for
    // gamepad-driven capture (when wired) and OS-key learning (when in
    // learning phase regardless of wire state).
    if wired || new_phase == "learning" {
        request_repaint_throttled(ui.ctx());
    }
}

pub(crate) fn show_map_action_body(
    node_id: NodeId,
    inputs: &[InPin],
    ui: &mut egui::Ui,
    snarl: &mut Snarl<NodeData>,
    live_signals: &std::collections::HashMap<(String, String), Signal>,
    _panic_shortcut: &crate::app::PanicShortcut,
    automap_parent: Option<&AutomapGlowParent<'_>>,
) {
    // Read current state
    let wired = inputs.first().map(|p| !p.remotes.is_empty()).unwrap_or(false);
    let upstream_dev_id = if wired {
        remapper_upstream_device_id(snarl, node_id, 0, automap_parent)
    } else { None };

    let (phase, draft_input, mappings, pressed_prev) = snarl.get_node(node_id)
        .map(|n| (
            n.params.get("ui_phase").and_then(|v| v.as_str()).unwrap_or("idle").to_string(),
            remapper_read_str_array(n, "draft_input"),
            n.params.get("mappings").and_then(|v| v.as_array()).cloned().unwrap_or_default(),
            remapper_read_str_array(n, "_pressed_prev"),
        ))
        .unwrap_or_else(|| ("idle".into(), vec![], vec![], vec![]));

    // Capture state machine (input side only)
    let mut pressed_now: Vec<String> = match (&upstream_dev_id, wired) {
        (Some(dev), true) => remapper_pressed_now(live_signals, dev),
        _ => Vec::new(),
    };

    // Hold the latched combo while gamepad UI-nav is active for this device
    // (mirror of the Remapper guard). Pass-stamped per device by the app.
    let nav_active_for_device = upstream_dev_id.as_deref().map(|dev| {
        let stamp: Option<u64> = ui.ctx().data(|d|
            d.get_temp(egui::Id::new(("gp_nav_active", dev.to_string()))));
        stamp == Some(ui.ctx().cumulative_pass_nr())
    }).unwrap_or(false);
    // One-shot capture arm: in nav mode, auto-capture is suppressed so the
    // gamepad can drive the UI without polluting the mapping. The "Capture"
    // button (clickable via gamepad South) sets `_nav_capture_armed`, which lets
    // the very next combo be captured despite nav mode; it auto-clears once a
    // combo latches (ready_to_add).
    let nav_capture_armed = snarl.get_node(node_id)
        .and_then(|n| n.params.get("_nav_capture_armed"))
        .and_then(|v| v.as_bool()).unwrap_or(false);
    // Arm-idle handshake (see remapper body): capture only opens after the Learn
    // press has released and the device went idle once post-arm.
    let nav_arm_idle = snarl.get_node(node_id)
        .and_then(|n| n.params.get("_nav_arm_idle"))
        .and_then(|v| v.as_bool()).unwrap_or(false);
    // Capture is allowed when nav isn't active, OR when armed AND idle-seen.
    let capture_ok = !nav_active_for_device || (nav_capture_armed && nav_arm_idle);
    let mut clear_capture_arm = false;
    let mut set_arm_idle: Option<bool> = None;

    // Prepare draft state for capture logic.
    let mut new_phase = phase.clone();
    let mut new_draft_input = draft_input.clone();

    // Touchpad zones & click accumulation (mirror remapper logic).
    let mut reset_click_mode = false;
    {
        let prev_mask: u8 = snarl.get_node(node_id)
            .and_then(|n| n.params.get("_tp_zones"))
            .and_then(|v| v.as_u64()).unwrap_or(0) as u8;
        // Read btn_touchpad directly from live_signals
        let touch_click_now = upstream_dev_id.as_deref()
            .and_then(|dev| live_signals.get(&(dev.to_string(), "btn_touchpad".to_string())))
            .map(|s| s.as_bool()).unwrap_or(false);
        let mut click_mask = if touch_click_now { prev_mask } else { 0 };
        let mut touch_mask: u8 = 0;
        if let Some(dev) = upstream_dev_id.as_deref() {
            let read_f = |pin: &str| -> Option<f32> {
                live_signals.get(&(dev.to_string(), pin.to_string()))
                    .map(|s| match s {
                        Signal::Float(v) => *v,
                        Signal::Vec2(v) => v.x,
                        _ => 0.0,
                    })
            };
            let read_b = |pin: &str| -> bool {
                live_signals.get(&(dev.to_string(), pin.to_string()))
                    .map(|s| s.as_bool()).unwrap_or(false)
            };
            for (xpin, apin) in [("touch1_x","touch1_active"), ("touch2_x","touch2_active")] {
                if !read_b(apin) { continue; }
                let x = match read_f(xpin) { Some(v) => v, None => continue };
                let idx = if x < -1.0/3.0 { 0 } else if x > 1.0/3.0 { 2 } else { 1 };
                touch_mask |= 1u8 << idx;
                if touch_click_now { click_mask |= 1u8 << idx; }
            }
        }
        if click_mask != prev_mask {
            if let Some(n) = snarl.get_node_mut(node_id) {
                n.params.insert("_tp_zones".to_string(), Value::from(click_mask as u64));
            }
        }
        // Click suppresses touch-only — see derive in eval.rs.
        let touch_mask = if touch_click_now { 0 } else { touch_mask };
        let push = |pn: &mut Vec<String>, pin: &str| {
            if !pn.iter().any(|p| p == pin) { pn.push(pin.to_string()); }
        };
        if click_mask & 1 != 0 { push(&mut pressed_now, "touchpad_left"); }
        if click_mask & 2 != 0 { push(&mut pressed_now, "touchpad_center"); }
        if click_mask & 4 != 0 { push(&mut pressed_now, "touchpad_right"); }
        if touch_click_now && click_mask == 0 { push(&mut pressed_now, "btn_touchpad"); }
        if touch_mask & 1 != 0 { push(&mut pressed_now, "touch_left"); }
        if touch_mask & 2 != 0 { push(&mut pressed_now, "touch_center"); }
        if touch_mask & 4 != 0 { push(&mut pressed_now, "touch_right"); }

        // Click-mode handling: if we just entered click mode, evict transient
        // touch_* pins from the draft so the click-variant mapping takes over.
        let click_mode_before = snarl.get_node(node_id)
            .and_then(|n| n.params.get("_tp_click_mode"))
            .and_then(|v| v.as_bool()).unwrap_or(false);
        let entering_click_mode = touch_click_now && !click_mode_before;
        if entering_click_mode {
            new_draft_input.retain(|p| p != "touch_left" && p != "touch_center" && p != "touch_right");
        }
        let click_mode = click_mode_before || touch_click_now;
        if click_mode != click_mode_before {
            if let Some(n) = snarl.get_node_mut(node_id) {
                n.params.insert("_tp_click_mode".to_string(), Value::from(click_mode));
            }
        }
        // While in click mode, drop touch_* from pressed_now so they don't
        // accumulate into the draft during the click+release tail.
        if click_mode {
            pressed_now.retain(|p| p != "touch_left" && p != "touch_center" && p != "touch_right");
        }
    }

    // On capture: accumulate peak set; latch on full release
    let rising: Vec<&String> = pressed_now.iter()
        .filter(|p| !pressed_prev.iter().any(|q| q == *p))
        .collect();
    let prev_was_empty = pressed_prev.is_empty();
    let now_empty = pressed_now.is_empty();

    // Arm-idle: mark idle the first frame the device is empty after arming, so
    // the Learn press has released before capture opens.
    if nav_capture_armed && !nav_arm_idle && now_empty {
        set_arm_idle = Some(true);
    }

    let is_transient = |p: &str| p == "touch_left" || p == "touch_center" || p == "touch_right";
    match new_phase.as_str() {
        "capturing" => {
            if capture_ok && !rising.is_empty() && prev_was_empty && !new_draft_input.is_empty() {
                new_draft_input = rising.iter().map(|s| (*s).clone()).collect();
                reset_click_mode = true;
            } else if capture_ok && !pressed_now.is_empty() {
                new_draft_input.retain(|p| { !is_transient(p) || pressed_now.iter().any(|q| q == p) });
                for p in &pressed_now {
                    if !new_draft_input.iter().any(|q| q == p) { new_draft_input.push(p.clone()); }
                }
            }
            // Latching: capture completes only when nothing is pressed AND
            // the touchpad is genuinely idle (no fingers, no click held).
            // Mirrors Remapper — without the `!touch_click_now` guard, a
            // click held with no finger would look "empty" and latch early,
            // wiping the click chord the next time a finger lands.
            let touch_click_now_latch = upstream_dev_id.as_deref()
                .and_then(|dev| live_signals.get(&(dev.to_string(), "btn_touchpad".to_string())))
                .map(|s| s.as_bool()).unwrap_or(false);
            let touchpad_idle = !touch_click_now_latch
                && upstream_dev_id.as_deref().map(|dev| {
                    let a1 = live_signals.get(&(dev.to_string(), "touch1_active".into()))
                        .map(|s| s.as_bool()).unwrap_or(false);
                    let a2 = live_signals.get(&(dev.to_string(), "touch2_active".into()))
                        .map(|s| s.as_bool()).unwrap_or(false);
                    !a1 && !a2
                }).unwrap_or(true);
            if now_empty && touchpad_idle && !new_draft_input.is_empty() {
                new_phase = "ready_to_add".to_string();
                // Clear sticky click_mode so the next capture (e.g. a fresh
                // touch without click) can register touch_* zones again.
                reset_click_mode = true;
                // A combo latched → disarm the one-shot nav capture.
                if nav_capture_armed { clear_capture_arm = true; }
            }
        }
        "ready_to_add" => {
            if capture_ok && !rising.is_empty() && prev_was_empty {
                new_phase = "capturing".to_string();
                new_draft_input = rising.iter().map(|s| (*s).clone()).collect();
                reset_click_mode = true;
            }
        }
        _ => {}
    }

    // Auto-enter capturing when a wire is connected and we were idle.
    if wired && new_phase == "idle" {
        new_phase = "capturing".to_string();
    }
    // Drop back to idle when wire is disconnected.
    if !wired && new_phase != "idle" {
        new_phase = "idle".to_string();
        new_draft_input.clear();
        if let Some(n) = snarl.get_node_mut(node_id) {
            remapper_write_str_array(n, "draft_input", &[]);
            remapper_write_str_array(n, "_pressed_prev", &[]);
            n.params.insert("_tp_click_mode".to_string(), Value::from(false));
        }
    }

    if let Some(node) = snarl.get_node_mut(node_id) {
        node.params.insert("ui_phase".to_string(), Value::String(new_phase.clone()));
        remapper_write_str_array(node, "draft_input", &new_draft_input);
        remapper_write_str_array(node, "_pressed_prev", &pressed_now);
        if reset_click_mode {
            node.params.insert("_tp_click_mode".to_string(), Value::from(false));
        }
        if let Some(v) = set_arm_idle {
            node.params.insert("_nav_arm_idle".to_string(), Value::from(v));
        }
        if clear_capture_arm {
            node.params.insert("_nav_capture_armed".to_string(), Value::from(false));
            node.params.insert("_nav_arm_idle".to_string(), Value::from(false));
        }
    }

    // Render
    let skin_param = snarl.get_node(node_id)
        .and_then(|n| n.params.get("skin").and_then(|v| v.as_str()).map(|s| s.to_string()))
        .unwrap_or_else(|| "auto".to_string());
    let skin = remapper_resolve_skin(snarl, node_id, &skin_param, automap_parent);

    const BODY_W: f32 = 380.0;
    let body_resp = ui.allocate_ui_with_layout(
        egui::vec2(BODY_W, 1.0),
        egui::Layout::top_down(egui::Align::Min),
        |ui| {
        ui.set_min_width(BODY_W);

        // Status line.
        let blue = Color32::from_rgb(106, 167, 255);
        let green = Color32::from_rgb(127, 201, 127);
        let (status_txt, status_col): (String, Color32) = if !wired {
            ("Connect Auto-Map wire to start mapping".into(), Color32::from_rgb(232, 180, 65))
        } else {
            match new_phase.as_str() {
                "capturing" if new_draft_input.is_empty() => {
                    if nav_active_for_device && !capture_ok {
                        ("Press Learn to start input capture".into(), blue)
                    } else {
                        ("Press a button or combination".into(), blue)
                    }
                }
                "capturing" => ("Press your input chord; release to capture".into(), blue),
                "ready_to_add" => ("Captured — click Add".into(), green),
                _ => (String::new(), Color32::TRANSPARENT),
            }
        };
        if !status_txt.is_empty() { ui.label(egui::RichText::new(status_txt).size(13.0).color(status_col)); }
        let _ = upstream_dev_id;
        let _ = &pressed_now;

        if !new_draft_input.is_empty() {
            ui.horizontal_wrapped(|ui| { remapper_render_chord(ui, &new_draft_input, skin); });
        }

        ui.add_space(2.0);

        // Action row: Learn (arm input capture), Clear, Add. All gamepad-
        // activatable via `_nav_act_*` flags; rects published for nav glow.
        let add_enabled = wired && !new_draft_input.is_empty();
        let has_draft = !new_draft_input.is_empty();
        let (act_learn, act_add, act_clear) = {
            let n = snarl.get_node(node_id);
            let g = |k: &str| n.and_then(|n| n.params.get(k)).and_then(|v| v.as_bool()).unwrap_or(false);
            (g("_nav_act_learn"), g("_nav_act_add"), g("_nav_act_clear"))
        };
        if act_learn || act_add || act_clear {
            if let Some(node) = snarl.get_node_mut(node_id) {
                node.params.insert("_nav_act_learn".into(), Value::from(false));
                node.params.insert("_nav_act_add".into(), Value::from(false));
                node.params.insert("_nav_act_clear".into(), Value::from(false));
            }
        }
        let mut learn_rect = egui::Rect::NOTHING;
        let mut clear_rect = egui::Rect::NOTHING;
        let mut add_rect = egui::Rect::NOTHING;
        ui.horizontal(|ui| {
            let learn_btn = ui.add_enabled(wired,
                egui::Button::new(egui::RichText::new("Learn").size(13.0)));
            learn_rect = learn_btn.rect;
            if (learn_btn.clicked() || act_learn) && wired {
                if let Some(node) = snarl.get_node_mut(node_id) {
                    node.params.insert("ui_phase".to_string(), Value::String("capturing".to_string()));
                    remapper_write_str_array(node, "draft_input", &[]);
                    remapper_write_str_array(node, "_pressed_prev", &[]);
                    node.params.insert("_nav_capture_armed".to_string(), Value::from(true));
                    node.params.insert("_nav_arm_idle".to_string(), Value::from(false));
                }
            }
            if has_draft {
                let clear_btn = ui.add(egui::Button::new(egui::RichText::new("Clear").size(13.0)));
                clear_rect = clear_btn.rect;
                if clear_btn.clicked() || act_clear {
                    if let Some(node) = snarl.get_node_mut(node_id) {
                        node.params.insert("ui_phase".to_string(), Value::String("capturing".to_string()));
                        remapper_write_str_array(node, "draft_input", &[]);
                        remapper_write_str_array(node, "_pressed_prev", &[]);
                        node.params.insert("_nav_capture_armed".to_string(), Value::from(false));
                        node.params.insert("_nav_arm_idle".to_string(), Value::from(false));
                        node.params.insert("_tp_click_mode".to_string(), Value::from(false));
                    }
                }
            }
            let add_btn = ui.add_enabled(add_enabled, egui::Button::new(egui::RichText::new("Add").size(13.0)));
            add_rect = add_btn.rect;
            if (add_btn.clicked() || act_add) && add_enabled {
                if let Some(node) = snarl.get_node_mut(node_id) {
                    let arr: Vec<Value> = new_draft_input.iter().map(|s| Value::String(s.clone())).collect();
                    let mut all = mappings.clone();
                    all.push(Value::Array(arr));
                    node.params.insert("mappings".to_string(), Value::Array(all));
                    node.params.insert("ui_phase".to_string(), Value::String("capturing".to_string()));
                    remapper_write_str_array(node, "draft_input", &[]);
                    remapper_write_str_array(node, "_pressed_prev", &[]);
                    // Clear sticky click_mode so the next capture starts fresh.
                    node.params.insert("_tp_click_mode".to_string(), Value::from(false));
                    node.params.insert("_nav_capture_armed".to_string(), Value::from(false));
                }
            }
            // Show a "Capturing…" hint while a one-shot arm is pending so the
            // user knows to press their chord now.
            if nav_capture_armed && new_draft_input.is_empty() {
                ui.label(egui::RichText::new("Capturing… press your input")
                    .size(12.0).color(Color32::from_rgb(106, 167, 255)));
            }
        });
        // Map Action action order: Learn, Clear, Add (no Special).
        publish_nav_action_rects(ui, node_id, &[learn_rect, clear_rect, add_rect]);

        // Mapping list: each mapping is Array<String> (input chord)
        if !mappings.is_empty() {
            ui.add_space(4.0);
            ui.separator();

            // Filter row — SOURCE pins only (upstream device on the wire), not
            // OS KB/M, so an injected destination key never flickers the
            // filter. See the Remapper note for the full rationale. In UI-nav
            // mode, filter by the last captured chord (Learn) rather than live
            // navigation presses.
            let filter_live: Vec<String> = if nav_active_for_device {
                new_draft_input.clone()
            } else {
                match (&upstream_dev_id, wired) {
                    (Some(dev), true) => remapper_pressed_now(live_signals, dev),
                    _ => Vec::new(),
                }
            };
            let filter = mapping_filter_row(
                ui,
                egui::Id::new(("fxi_mapact_filter", node_id.0)),
                &format!("({})", mappings.len()),
                &filter_live,
                skin,
            );

            let mut to_remove: Option<usize> = None;
            // Card layout per mapping (Map Action variant): no in/out labels,
            // just header + a single row listing the captured chord chips.
            ui.spacing_mut().item_spacing.y = 2.0;
            let mut press_mode_changed: Option<(usize, serde_json::Map<String, Value>)> = None;
            let reorder_enabled = filter.kind == MapFilterKind::All;
            let mut rv = ReorderView::begin(
                ui, egui::Id::new(("fxi_mapact_reorder", node_id.0)), reorder_enabled,
            );
            let mut slot = 0usize;
            for (i, m) in mappings.iter().enumerate() {
                // Legacy Array<String> → upgrade to Object{ in, … } once edited.
                let (in_pins, mut working): (Vec<String>, serde_json::Map<String, Value>) =
                    if let Some(arr) = m.as_array() {
                        let pins: Vec<String> = arr.iter()
                            .filter_map(|v| v.as_str().map(|s| s.to_string())).collect();
                        let in_arr: Vec<Value> = pins.iter()
                            .map(|s| Value::String(s.clone())).collect();
                        let mut obj = serde_json::Map::new();
                        obj.insert("in".to_string(), Value::Array(in_arr));
                        (pins, obj)
                    } else if let Some(obj) = m.as_object() {
                        let pins: Vec<String> = obj.get("in").and_then(|v| v.as_array())
                            .map(|a| a.iter().filter_map(|v| v.as_str().map(|s| s.to_string())).collect())
                            .unwrap_or_default();
                        (pins, obj.clone())
                    } else {
                        (Vec::new(), serde_json::Map::new())
                    };

                if !mapping_passes_filter(&filter, &in_pins) { continue; }

                if let Some(h) = rv.gap_before(slot) { draw_insertion_gap(ui, h); }

                let mut working_changed = false;
                let drag_off = rv.offset_for(i);

                ui.push_id(("fxi_mapact_card", node_id.0, i), |ui| {
                    ui.horizontal(|ui| {
                        ui.add_space(6.0);
                        ui.allocate_ui_with_layout(
                            egui::vec2((BODY_W - 18.0).min(358.0), 1.0),
                            egui::Layout::top_down(egui::Align::Min),
                            |ui| {
                                let result = remapper_mapping_card_pixel(
                                    ui, node_id, i, &mut working,
                                    &in_pins, None, skin,
                                    true, reorder_enabled, drag_off, "mappings", false,
                                    None, // Map Action rows aren't bus writers (out_pins None)
                                );
                                if result.delete_clicked { to_remove = Some(i); }
                                if result.changed { working_changed = true; }
                                rv.observe(i, &result);
                            },
                        );
                    });
                });

                if working_changed {
                    press_mode_changed = Some((i, working));
                }
                slot += 1;
            }
            if let Some(h) = rv.gap_after_last(slot) { draw_insertion_gap(ui, h); }
            if let Some((from, to)) = rv.finish(ui) {
                if let Some(node) = snarl.get_node_mut(node_id) {
                    if let Some(Value::Array(arr)) = node.params.get_mut("mappings") {
                        reorder_array(arr, from, to);
                    }
                }
            }
            if let Some(idx) = to_remove {
                if let Some(node) = snarl.get_node_mut(node_id) {
                    if let Some(Value::Array(arr)) = node.params.get_mut("mappings") { if idx < arr.len() { arr.remove(idx); } }
                }
            }
            if let Some((i, obj)) = press_mode_changed {
                if let Some(node) = snarl.get_node_mut(node_id) {
                    if let Some(Value::Array(arr)) = node.params.get_mut("mappings") {
                        if let Some(slot) = arr.get_mut(i) {
                            *slot = Value::Object(obj);
                        }
                    }
                }
            }
        }
    });

    register_exposable_element(ui, node_id, "whole_module", body_resp.response.rect);

    // Request repaint so capture ticks each frame while wired
    if wired { request_repaint_throttled(ui.ctx()); }
}
