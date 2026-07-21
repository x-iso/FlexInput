//! Gyro 3DOF node body + lean mapping section.

use super::*;

/// Resolve the current (family, axis) tuple from a node, applying legacy
/// `mode` fallback for patches saved before the split.
pub(crate) fn gyro_read_family_axis(n: &NodeData) -> (String, String) {
    if let Some(fam) = n.params.get("family").and_then(|v| v.as_str()) {
        let axis = n.params.get("axis").and_then(|v| v.as_str()).unwrap_or("pitch_yaw");
        return (fam.to_string(), axis.to_string());
    }
    match n.params.get("mode").and_then(|v| v.as_str()).unwrap_or("local") {
        "player" => ("pointer".into(),  "player".into()),
        "world"  => ("pointer".into(),  "world".into()),
        "laser"  => ("steering".into(), "pitch_yaw".into()),
        _        => ("pointer".into(),  "pitch_yaw".into()),
    }
}

pub(crate) const GYRO_AXIS_OPTIONS: [(&str, &str); 4] = [
    ("pitch_yaw",  "Pitch+Yaw"),
    ("pitch_roll", "Pitch+Roll"),
    ("player",     "Player"),
    ("world",      "World"),
];

pub(crate) fn show_gyro_3dof_body(
    node_id: NodeId,
    inputs: &[InPin],
    ui: &mut egui::Ui,
    snarl: &mut Snarl<NodeData>,
    live_signals: &std::collections::HashMap<(String, String), Signal>,
    panic_shortcut: &crate::app::PanicShortcut,
    automap_parent: Option<&AutomapGlowParent<'_>>,
) {
    let _ = inputs; // wire-presence + upstream device id read below
    let snap = snarl.get_node(node_id);
    let (family, axis)       = snap.map(gyro_read_family_axis).unwrap_or_else(|| ("pointer".into(), "pitch_yaw".into()));
    let inv_yaw   = snap.and_then(|n| n.params.get("inv_yaw")   .and_then(|v| v.as_bool())).unwrap_or(false);
    let inv_pitch = snap.and_then(|n| n.params.get("inv_pitch")  .and_then(|v| v.as_bool())).unwrap_or(false);
    let inv_roll  = snap.and_then(|n| n.params.get("inv_roll")   .and_then(|v| v.as_bool())).unwrap_or(false);
    let inv_ax    = snap.and_then(|n| n.params.get("inv_accel_x").and_then(|v| v.as_bool())).unwrap_or(false);
    let inv_ay    = snap.and_then(|n| n.params.get("inv_accel_y").and_then(|v| v.as_bool())).unwrap_or(false);
    let inv_az    = snap.and_then(|n| n.params.get("inv_accel_z").and_then(|v| v.as_bool())).unwrap_or(false);
    let exclude_y = snap.and_then(|n| n.params.get("steering_exclude_y").and_then(|v| v.as_bool())).unwrap_or(false);
    let recenter_strength = snap.and_then(|n| n.params.get("recenter_strength").and_then(|v| v.as_f64())).unwrap_or(0.0) as f32;
    let reset_ease_in     = snap.and_then(|n| n.params.get("reset_ease_in").and_then(|v| v.as_f64())).unwrap_or(0.25) as f32;
    let lean_threshold    = snap.and_then(|n| n.params.get("lean_threshold").and_then(|v| v.as_f64())).unwrap_or(0.3) as f32;
    // Display scale for the 3D Orientation output only (applied to the
    // integration rate so it stays continuous). Does NOT affect the 2D outputs.
    let orient_scale      = snap.and_then(|n| n.params.get("orient_scale").and_then(|v| v.as_f64())).unwrap_or(1.0) as f32;
    let orient_drift      = snap.and_then(|n| n.params.get("orient_drift").and_then(|v| v.as_f64())).unwrap_or(0.0) as f32;
    let yaw_recenter      = snap.and_then(|n| n.params.get("orient_auto_recenter").and_then(|v| v.as_bool())).unwrap_or(false);
    let yaw_thresh        = snap.and_then(|n| n.params.get("orient_recenter_thresh").and_then(|v| v.as_f64())).unwrap_or(0.005) as f32;
    let out_x = snap.and_then(|n| match n.extra.last_out.get(1) { Some(Some(Signal::Float(f))) => Some(*f), _ => None }).unwrap_or(0.0);
    let out_y = snap.and_then(|n| match n.extra.last_out.get(2) { Some(Some(Signal::Float(f))) => Some(*f), _ => None }).unwrap_or(0.0);
    let lean_v = snap.and_then(|n| match n.extra.last_out.get(3) { Some(Some(Signal::Float(f))) => Some(*f), _ => None }).unwrap_or(0.0);

    let mut family = family;
    let mut axis = axis;
    let mut inv_gyro  = [inv_yaw, inv_pitch, inv_roll];
    let mut inv_accel = [inv_ax, inv_ay, inv_az];
    let mut exclude_y = exclude_y;
    let mut recenter_strength = recenter_strength;
    let mut reset_ease_in = reset_ease_in;
    let mut lean_threshold = lean_threshold;
    let mut orient_scale = orient_scale;
    let mut orient_drift = orient_drift;
    let mut yaw_recenter = yaw_recenter;
    let mut yaw_thresh   = yaw_thresh;
    let mut changed   = false;

    const GYR_LABELS: [(&str, &str); 3] = [
        ("yaw",   "gyro_z — invert if rotating right gives negative X\n(expected: right = positive X)"),
        ("pitch", "gyro_y — invert if tilting up gives negative Y\n(expected: up = positive Y)"),
        ("roll",  "gyro_x — affects Lean output and Player/World gravity correction"),
    ];
    const ACC_LABELS: [(&str, &str); 3] = [
        ("X",  "accel_x — invert if Player/World horizontal correction is backwards"),
        ("Y",  "accel_y — invert if Player/World vertical correction is backwards"),
        ("+Z", "accel_z — expected POSITIVE when controller is held flat face-up (≈ +1 G).\nInvert if your device reports negative when flat."),
    ];

    // Helper that draws a 4-button mode picker. Returns true if a selection
    // change occurred; updates `family` and `axis` in place.
    let draw_mode_row = |ui: &mut egui::Ui, label: &str, target_family: &str,
                              family: &mut String, axis: &mut String| -> bool {
        let mut row_changed = false;
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 2.0;
            ui.label(egui::RichText::new(label).small().weak());
            for (id, lbl) in GYRO_AXIS_OPTIONS {
                let selected = family == target_family && axis == id;
                if ui.selectable_label(selected, egui::RichText::new(lbl).small()).clicked() {
                    *family = target_family.to_string();
                    *axis   = id.to_string();
                    row_changed = true;
                }
            }
        });
        row_changed
    };

    let mut pointer_rect:    Option<egui::Rect> = None;
    let mut steering_rect:   Option<egui::Rect> = None;
    let mut stopts_rect:     Option<egui::Rect> = None;
    let mut gyr_rect:        Option<egui::Rect> = None;
    let mut acc_rect:        Option<egui::Rect> = None;
    let mut lean_rect:       Option<egui::Rect> = None;
    let mut lean_left_rect:  Option<egui::Rect> = None;
    let mut lean_right_rect: Option<egui::Rect> = None;

    ui.vertical(|ui| {
        ui.spacing_mut().item_spacing = egui::vec2(2.0, 2.0);

        let r = ui.scope(|ui| {
            if draw_mode_row(ui, "Pointer:", "pointer", &mut family, &mut axis) { changed = true; }
        });
        pointer_rect = Some(r.response.rect);

        let r = ui.scope(|ui| {
            if draw_mode_row(ui, "Steering:", "steering", &mut family, &mut axis) { changed = true; }
        });
        steering_rect = Some(r.response.rect);

        // Steering-only options. Grayed when family != steering.
        let r = ui.scope(|ui| {
            ui.add_enabled_ui(family == "steering", |ui| {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 4.0;
                    if ui.checkbox(&mut exclude_y, egui::RichText::new("excl. Y").small())
                        .on_hover_text("Suppress the Y steering output (keeps Y at 0).\nUseful when only the X axis matters — e.g. a steering wheel\nwhere pitching the controller shouldn't move Y.")
                        .changed() { changed = true; }
                    ui.label(egui::RichText::new("re-center").small().weak())
                        .on_hover_text("Pulls the steering accumulator toward a tilt-compensated\nheading derived from accel. Strength scales by how observable\nthat heading is — flat controller → no pull, tilted → strong\npull. 0 disables. Units: /s.");
                    if ui.add(egui::DragValue::new(&mut recenter_strength)
                        .speed(0.05).range(0.0..=4.0).suffix(" /s"))
                        .changed() { changed = true; }
                    ui.label(egui::RichText::new("ease").small().weak())
                        .on_hover_text("Reset eases steering toward 0 over this many seconds.");
                    if ui.add(egui::DragValue::new(&mut reset_ease_in)
                        .speed(0.05).range(0.0..=2.0).suffix(" s"))
                        .changed() { changed = true; }
                });
            });
        });
        stopts_rect = Some(r.response.rect);

        // 3D Orientation display scale (applied to the integration rate, so it's
        // continuous — no flipping at 180°). Affects the Orientation output that
        // feeds the Controller 3D viewer, NOT the 2D pointer/steering outputs.
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            ui.label(egui::RichText::new("3D rot ×").small().weak())
                .on_hover_text(
                    "Rotation scale for the 3D Orientation output only.\n\
                     1.0 = physically 1:1 on known controllers. If the\n\
                     on-screen model over- or under-rotates vs. your real\n\
                     controller, adjust here. Applied to the integration rate\n\
                     so it never flips; does NOT affect pointer/steering.",
                );
            if ui.add(egui::DragValue::new(&mut orient_scale)
                .speed(0.01).range(0.05..=5.0))
                .changed() { changed = true; }
            if ui.small_button("⟲").on_hover_text("Reset to 1.0").clicked() {
                orient_scale = 1.0;
                changed = true;
            }
            ui.label(egui::RichText::new("drift fix").small().weak())
                .on_hover_text(
                    "Accelerometer drift correction for the 3D Orientation\n\
                     output. While the controller is held steady, gravity\n\
                     pulls accumulated pitch/roll drift back out (yaw can't\n\
                     be corrected — reset for that). OFF by default: linear\n\
                     motion (shakes/swings) is indistinguishable from tilt\n\
                     and can rotate the model falsely — prefer Auto re-center.\n\
                     Higher = faster pull; reference pose captured on Reset.",
                );
            if ui.add(egui::DragValue::new(&mut orient_drift)
                .speed(0.01).range(0.0..=1.0))
                .changed() { changed = true; }
        });

        // Auto re-center — without an absolute reference the pose can end up
        // shifted on any axis. Below the threshold for 3 s, the whole
        // orientation eases back to identity until centered or the threshold
        // is exceeded again.
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            changed |= ui.checkbox(&mut yaw_recenter,
                egui::RichText::new("Auto re-center").small())
                .on_hover_text(
                    "3D Orientation output only: when gyro readings stay under\n\
                     the threshold for 3 s, gradually re-center the whole\n\
                     orientation (all axes) until it's centered or the\n\
                     threshold is exceeded again.",
                )
                .changed();
            ui.add_enabled_ui(yaw_recenter, |ui| {
                ui.label(egui::RichText::new("threshold").small().weak())
                    .on_hover_text(
                        "Gyro magnitude (normalized, 1.0 = 2000 dps) that counts\n\
                         as \"not moving\". With the noise-floor deadzone armed,\n\
                         resting readings are exactly 0, so the default is fine.",
                    );
                if ui.add(egui::DragValue::new(&mut yaw_thresh)
                    .speed(0.001).range(0.0005..=0.2)
                    .min_decimals(3).max_decimals(4))
                    .changed() { changed = true; }
            });
        });

        let r = ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 3.0;
            ui.label(egui::RichText::new("Gyr:").small().weak());
            for i in 0..3 {
                let (label, tip) = GYR_LABELS[i];
                changed |= ui.checkbox(&mut inv_gyro[i], egui::RichText::new(label).small())
                    .on_hover_text(tip).changed();
            }
        });
        gyr_rect = Some(r.response.rect);

        let r = ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 3.0;
            ui.label(egui::RichText::new("Acc:").small().weak());
            for i in 0..3 {
                let (label, tip) = ACC_LABELS[i];
                changed |= ui.checkbox(&mut inv_accel[i], egui::RichText::new(label).small())
                    .on_hover_text(tip).changed();
            }
        });
        acc_rect = Some(r.response.rect);

        // Lean threshold + live readout.
        let r = ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            ui.label(egui::RichText::new("Lean threshold").small().weak())
                .on_hover_text("|Lean| ≥ threshold triggers the Lean! Bool output.");
            if ui.add(egui::DragValue::new(&mut lean_threshold)
                .speed(0.02).range(0.01..=4.0))
                .changed() { changed = true; }
            ui.label(egui::RichText::new(format!("({:+.2})", lean_v)).small().weak());
        });
        lean_rect = Some(r.response.rect);

        ui.label(egui::RichText::new(format!("X:{:+.3}  Y:{:+.3}", out_x, out_y)).small().weak());

        // Lean mapping cards — two lists (left / right). Each section has
        // its own Learn/Add chord-capture state machine and renders mapping
        // cards using the shared pixel-accurate card from Remapper/Map
        // Action (with allow_analog_mode=true so the Analog press mode is
        // available in the dropdown).
        ui.separator();
        let lean_left_resp = ui.scope(|ui| {
            show_gyro_lean_mapping_section(
                node_id, ui, snarl, "left",  inputs, live_signals, panic_shortcut, automap_parent,
            );
        });
        lean_left_rect = Some(lean_left_resp.response.rect);
        let lean_right_resp = ui.scope(|ui| {
            show_gyro_lean_mapping_section(
                node_id, ui, snarl, "right", inputs, live_signals, panic_shortcut, automap_parent,
            );
        });
        lean_right_rect = Some(lean_right_resp.response.rect);
    });

    if let Some(r) = pointer_rect    { register_exposable_element(ui, node_id, "pointer_mode",  r); }
    if let Some(r) = steering_rect   { register_exposable_element(ui, node_id, "steering_mode", r); }
    if let Some(r) = stopts_rect     { register_exposable_element(ui, node_id, "steering_opts", r); }
    if let Some(r) = gyr_rect        { register_exposable_element(ui, node_id, "gyro_invert",   r); }
    if let Some(r) = acc_rect        { register_exposable_element(ui, node_id, "accel_invert",  r); }
    if let Some(r) = lean_rect       { register_exposable_element(ui, node_id, "lean_threshold", r); }
    if let Some(r) = lean_left_rect  { register_exposable_element(ui, node_id, "lean_left",     r); }
    if let Some(r) = lean_right_rect { register_exposable_element(ui, node_id, "lean_right",    r); }

    if changed {
        if let Some(node) = snarl.get_node_mut(node_id) {
            node.params.insert("family".into(),         Value::String(family));
            node.params.insert("axis".into(),           Value::String(axis));
            // Strip the legacy `mode` key so it can't drift out of sync.
            node.params.remove("mode");
            node.params.insert("inv_yaw".into(),        Value::Bool(inv_gyro[0]));
            node.params.insert("inv_pitch".into(),      Value::Bool(inv_gyro[1]));
            node.params.insert("inv_roll".into(),       Value::Bool(inv_gyro[2]));
            node.params.insert("inv_accel_x".into(),    Value::Bool(inv_accel[0]));
            node.params.insert("inv_accel_y".into(),    Value::Bool(inv_accel[1]));
            node.params.insert("inv_accel_z".into(),    Value::Bool(inv_accel[2]));
            node.params.insert("steering_exclude_y".into(),
                Value::Bool(exclude_y));
            node.params.remove("recenter_blend");
            node.params.insert("recenter_strength".into(),
                serde_json::Number::from_f64(recenter_strength as f64).map(Value::Number).unwrap_or(Value::Null));
            node.params.insert("reset_ease_in".into(),
                serde_json::Number::from_f64(reset_ease_in as f64).map(Value::Number).unwrap_or(Value::Null));
            // Spike-filter moved to the device polling layer; drop the
            // legacy per-module params so they can't drift out of sync.
            node.params.remove("spike_suppress");
            node.params.remove("spike_sensitivity");
            node.params.remove("spike_k");
            node.params.insert("lean_threshold".into(),
                serde_json::Number::from_f64(lean_threshold as f64).map(Value::Number).unwrap_or(Value::Null));
            node.params.insert("orient_scale".into(),
                serde_json::Number::from_f64(orient_scale as f64).map(Value::Number).unwrap_or(Value::Null));
            node.params.insert("orient_drift".into(),
                serde_json::Number::from_f64(orient_drift as f64).map(Value::Number).unwrap_or(Value::Null));
            node.params.insert("orient_auto_recenter".into(), Value::Bool(yaw_recenter));
            node.params.insert("orient_recenter_thresh".into(),
                serde_json::Number::from_f64(yaw_thresh as f64).map(Value::Number).unwrap_or(Value::Null));
        }
    }
}

/// Render one of the two lean mapping lists (left / right). Each section
/// owns an independent chord-capture state machine keyed by side so the
/// two can be edited simultaneously. Mappings are stored on the node as
/// `lean_left` / `lean_right` arrays of objects shaped like Map Action
/// mappings BUT with `out` (destination chord) instead of `in` — the lean
/// trigger IS the section, so what we capture is what to FIRE.
///
/// Schema: `{ out: [pin_id...], mode?, window_ms?, sustain?, turbo? }`.
/// Legacy entries with `in:[]` get upgraded in-place on first display.
pub(crate) fn show_gyro_lean_mapping_section(
    node_id: NodeId,
    ui: &mut egui::Ui,
    snarl: &mut Snarl<NodeData>,
    side: &'static str,
    inputs: &[InPin],
    live_signals: &std::collections::HashMap<(String, String), Signal>,
    panic_shortcut: &crate::app::PanicShortcut,
    automap_parent: Option<&AutomapGlowParent<'_>>,
) {
    let key            = if side == "left" { "lean_left" } else { "lean_right" };
    let phase_key      = if side == "left" { "_lean_left_phase" } else { "_lean_right_phase" };
    let draft_key      = if side == "left" { "_lean_left_draft" } else { "_lean_right_draft" };
    let prev_key       = if side == "left" { "_lean_left_pressed_prev" } else { "_lean_right_pressed_prev" };
    let armed_key      = if side == "left" { "_lean_left_armed" } else { "_lean_right_armed" };
    let arm_idle_key   = if side == "left" { "_lean_left_arm_idle" } else { "_lean_right_arm_idle" };
    let title          = if side == "left" { "Lean Left → " } else { "Lean Right → " };

    // ── Migration: if legacy entries with `in` exist, rewrite to `out`.
    {
        let needs_upgrade = snarl.get_node(node_id)
            .and_then(|n| n.params.get(key).and_then(|v| v.as_array()))
            .map(|arr| arr.iter().any(|m| m.as_object()
                .map(|o| o.contains_key("in") && !o.contains_key("out")).unwrap_or(false)))
            .unwrap_or(false);
        if needs_upgrade {
            if let Some(node) = snarl.get_node_mut(node_id) {
                if let Some(Value::Array(arr)) = node.params.get_mut(key) {
                    for v in arr.iter_mut() {
                        if let Some(obj) = v.as_object_mut() {
                            if let Some(ins) = obj.remove("in") {
                                obj.insert("out".to_string(), ins);
                            }
                        }
                    }
                }
            }
        }
    }

    // ── Read state ───────────────────────────────────────────────────────
    let wired = inputs.first().map(|p| !p.remotes.is_empty()).unwrap_or(false);
    let upstream_dev_id = if wired {
        remapper_upstream_device_id(snarl, node_id, 0, automap_parent)
    } else { None };

    let (phase, draft, pressed_prev, mappings) = snarl.get_node(node_id).map(|n| (
        n.params.get(phase_key).and_then(|v| v.as_str()).unwrap_or("idle").to_string(),
        remapper_read_str_array(n, draft_key),
        remapper_read_str_array(n, prev_key),
        n.params.get(key).and_then(|v| v.as_array()).cloned().unwrap_or_default(),
    )).unwrap_or_else(|| ("idle".into(), vec![], vec![], vec![]));

    // Gamepad UI-nav active for the upstream device this frame? While it is, the
    // controller drives FlexInput's UI, so capture must not begin until the
    // Learn press has been released. Mirrors the Remapper's arm/arm-idle
    // handshake but scoped per Lean side.
    let nav_active_for_device = upstream_dev_id.as_deref().map(|dev| {
        let stamp: Option<u64> = ui.ctx().data(|d|
            d.get_temp(egui::Id::new(("gp_nav_active", dev.to_string()))));
        stamp == Some(ui.ctx().cumulative_pass_nr())
    }).unwrap_or(false);
    let nav_capture_armed = snarl.get_node(node_id)
        .and_then(|n| n.params.get(armed_key)).and_then(|v| v.as_bool()).unwrap_or(false);
    let nav_arm_idle = snarl.get_node(node_id)
        .and_then(|n| n.params.get(arm_idle_key)).and_then(|v| v.as_bool()).unwrap_or(false);
    // Capture may proceed when not in nav mode, OR (nav mode) once armed AND the
    // device has gone idle once since arming (so the Learn press is released).
    let capture_ok = !nav_active_for_device || (nav_capture_armed && nav_arm_idle);
    let mut clear_capture_arm = false;
    let mut set_arm_idle: Option<bool> = None;

    // ── Capture state machine ───────────────────────────────────────────
    // Only ticks while `phase == "learning"` (active capture session).
    // No auto-enter-on-wire — Learn is button-gated, otherwise the gyro
    // module's normal AutoMap-in wire would constantly try to capture.
    let mut pressed_now: Vec<String> = Vec::new();
    if phase == "learning" {
        if let (Some(dev), true) = (&upstream_dev_id, wired) {
            pressed_now = remapper_pressed_now(live_signals, dev);
        }
        // Always merge global KB/M during learning.
        for p in remapper_kbm_pressed_now(ui, panic_shortcut) {
            if !pressed_now.iter().any(|q| q == &p) { pressed_now.push(p); }
        }
    }

    // Arm-idle latch: first frame the device is empty while armed, flip arm_idle
    // true so the NEXT non-empty press begins the capture (post Learn release).
    let now_empty = pressed_now.is_empty();
    if nav_capture_armed && !nav_arm_idle && now_empty {
        set_arm_idle = Some(true);
    }

    let mut new_phase = phase.clone();
    let mut new_draft = draft.clone();

    if new_phase == "learning" {
        let rising: Vec<&String> = pressed_now.iter()
            .filter(|p| !pressed_prev.iter().any(|q| q == *p))
            .collect();
        let prev_was_empty = pressed_prev.is_empty();
        // Capture gated on `capture_ok` so a still-held Learn press (nav mode)
        // doesn't get captured; once the chord lands and releases, the arm
        // clears so subsequent nav presses don't overwrite it.
        if capture_ok && !rising.is_empty() && prev_was_empty && !new_draft.is_empty() {
            // Re-capture: new burst after a previous chord.
            new_draft = rising.iter().map(|s| (*s).clone()).collect();
        } else if capture_ok && !pressed_now.is_empty() {
            for p in &pressed_now {
                if !new_draft.iter().any(|q| q == p) { new_draft.push(p.clone()); }
            }
        }
        // Latch on release: once a chord was captured and the device goes idle,
        // clear the arm so navigation resumes (the draft is kept for Add).
        if nav_capture_armed && now_empty && !new_draft.is_empty() {
            clear_capture_arm = true;
        }
        // No auto-latch of the mapping itself; user explicitly clicks Add/Stop.
    }

    // Persist state machine results before rendering.
    if let Some(node) = snarl.get_node_mut(node_id) {
        node.params.insert(phase_key.to_string(), Value::String(new_phase.clone()));
        remapper_write_str_array(node, draft_key, &new_draft);
        remapper_write_str_array(node, prev_key, &pressed_now);
        if let Some(v) = set_arm_idle { node.params.insert(arm_idle_key.to_string(), Value::from(v)); }
        if clear_capture_arm {
            node.params.insert(armed_key.to_string(), Value::from(false));
            node.params.insert(arm_idle_key.to_string(), Value::from(false));
        }
    }

    // ── Render ──────────────────────────────────────────────────────────
    let skin_param = snarl.get_node(node_id)
        .and_then(|n| n.params.get("skin").and_then(|v| v.as_str()).map(|s| s.to_string()))
        .unwrap_or_else(|| "auto".to_string());
    let skin = remapper_resolve_skin(snarl, node_id, &skin_param, automap_parent);

    // Force a top-down layout in a fixed-width sub-UI so we lay out the
    // same way regardless of parent layout (some pinned-widget contexts
    // hand us a horizontal parent — without this the header + each card
    // would lay out left-to-right instead of stacking).
    const BODY_W: f32 = 380.0;
    ui.allocate_ui_with_layout(
        egui::vec2(BODY_W, 1.0),
        egui::Layout::top_down(egui::Align::Min),
        |ui| {
    ui.set_min_width(BODY_W);

    // Side-scoped `_nav_act_*` flag keys (set by the nav driver on South).
    let lk = if side == "left" { "_nav_act_learn_left" }   else { "_nav_act_learn_right" };
    let ak = if side == "left" { "_nav_act_add_left" }     else { "_nav_act_add_right" };
    let sk = if side == "left" { "_nav_act_special_left" } else { "_nav_act_special_right" };
    let ck = if side == "left" { "_nav_act_clear_left" }   else { "_nav_act_clear_right" };
    // Consume side-scoped one-shot gamepad activation flags. Mirrors the
    // Remapper/Map Action `_nav_act_*` pattern so a controller can drive
    // Learn / Special / Clear / Add in the Lean sections too.
    let (act_learn, act_add, act_special, act_clear) = {
        let n = snarl.get_node(node_id);
        let g = |k: &str| n.and_then(|n| n.params.get(k)).and_then(|v| v.as_bool()).unwrap_or(false);
        (g(lk), g(ak), g(sk), g(ck))
    };
    if act_learn || act_add || act_special || act_clear {
        if let Some(node) = snarl.get_node_mut(node_id) {
            node.params.insert(lk.to_string(), Value::from(false));
            node.params.insert(ak.to_string(), Value::from(false));
            node.params.insert(sk.to_string(), Value::from(false));
            node.params.insert(ck.to_string(), Value::from(false));
        }
    }

    // Status line. Before Learn: prompt to Learn or pick Special. During
    // learning: prompt for the chord (or show the draft).
    {
        let blue = Color32::from_rgb(106, 167, 255);
        let green = Color32::from_rgb(127, 201, 127);
        // idle (no draft) → prompt for Learn/Special; "ready" (Special-picked
        // draft) → prompt to Add; "learning" handled by the draft preview below.
        if new_phase != "learning" {
            if new_phase == "ready" && !draft.is_empty() {
                ui.label(egui::RichText::new("Picked — click Add (or Learn to add a chord)")
                    .size(13.0).color(green));
            } else {
                let txt = if !wired {
                    "Connect the gyro Device input, then Learn (or select Special)"
                } else {
                    "Press Learn to start capture or select Special"
                };
                ui.label(egui::RichText::new(txt).size(13.0).color(blue));
            }
        }
    }

    let mut learn_rect = egui::Rect::NOTHING;
    let mut special_rect = egui::Rect::NOTHING;
    let mut clear_rect = egui::Rect::NOTHING;
    let mut add_rect = egui::Rect::NOTHING;
    // A draft exists if the section captured/picked any output, or is mid-learn.
    let has_draft = !new_draft.is_empty()
        || new_phase == "learning" || new_phase == "ready";
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new(title).small().weak());
        ui.label(egui::RichText::new(format!("({})", mappings.len())).small().weak());

        let in_learning = new_phase == "learning";
        let learn_label = if in_learning { "Stop" } else { "Learn" };
        let learn_resp = ui.add_enabled(
            wired || in_learning,
            egui::Button::new(egui::RichText::new(learn_label).size(13.0)),
        ).on_hover_text(if wired {
            "Capture a gamepad / keyboard chord to fire when leaning this direction"
        } else {
            "Connect the gyro module's Device input to enable gamepad capture\n(keyboard can be captured without a wire once Learn is active)"
        });
        learn_rect = learn_resp.rect;
        if (learn_resp.clicked() || act_learn) && (wired || in_learning) {
            // Coming from "ready" (Special pins already picked) keeps the draft
            // so a gamepad chord SUMS onto the Special pins; from idle, start
            // fresh.
            let from_ready = new_phase == "ready";
            if let Some(node) = snarl.get_node_mut(node_id) {
                if in_learning {
                    node.params.insert(phase_key.to_string(), Value::String("idle".to_string()));
                    node.params.insert(armed_key.to_string(), Value::from(false));
                    node.params.insert(arm_idle_key.to_string(), Value::from(false));
                } else {
                    node.params.insert(phase_key.to_string(), Value::String("learning".to_string()));
                    if !from_ready {
                        remapper_write_str_array(node, draft_key, &[]);
                    }
                    remapper_write_str_array(node, prev_key, &[]);
                    // Arm a one-shot nav capture: arm_idle=false so capture waits
                    // for the Learn press to release before it begins.
                    node.params.insert(armed_key.to_string(), Value::from(true));
                    node.params.insert(arm_idle_key.to_string(), Value::from(false));
                }
            }
        }

        // Special button — opens the shared KB/M + touchpad picker (mouse OR
        // gamepad South via `_nav_act_special_<side>`). The picker writes into
        // this section's `_lean_<side>_draft`. Lean inputs are always analog (the
        // lean gesture), so the swipe bindings are always available here.
        {
            let special_btn = ui.add(egui::Button::new(
                egui::RichText::new("Special…").size(13.0)));
            special_rect = special_btn.rect;
            if special_btn.clicked() || act_special {
                crate::canvas::viewer::request_special_picker(ui.ctx(),
                    crate::canvas::viewer::SpecialPickerRequest {
                        inner: node_id,
                        path: crate::canvas::viewer::subpatch_path(automap_parent),
                        draft_key: draft_key.to_string(),
                        phase_key: Some(phase_key.to_string()),
                        touch_zones: false,
                        exclude_pin_prefix: None,
                    });
            }
        }

        // Clear button — abandons the captured/picked output and starts over
        // (back to idle, draft emptied, capture disarmed). Shown whenever a draft
        // exists so a botched capture/pick can be reset WITHOUT finishing.
        if has_draft {
            let clear_btn = ui.add(egui::Button::new(egui::RichText::new("Clear").size(13.0)));
            clear_rect = clear_btn.rect;
            if clear_btn.clicked() || act_clear {
                if let Some(node) = snarl.get_node_mut(node_id) {
                    node.params.insert(phase_key.to_string(), Value::String("idle".to_string()));
                    remapper_write_str_array(node, draft_key, &[]);
                    remapper_write_str_array(node, prev_key, &[]);
                    node.params.insert(armed_key.to_string(), Value::from(false));
                    node.params.insert(arm_idle_key.to_string(), Value::from(false));
                }
                new_draft.clear();
                new_phase = "idle".to_string();
            }
        }

        let add_enabled = !new_draft.is_empty()
            && (new_phase == "learning" || new_phase == "ready");
        let add_resp = ui.add_enabled(add_enabled,
            egui::Button::new(egui::RichText::new("Add").size(13.0)));
        add_rect = add_resp.rect;
        if (add_resp.clicked() || act_add) && add_enabled {
            if let Some(node) = snarl.get_node_mut(node_id) {
                let out_arr: Vec<Value> = new_draft.iter().map(|s| Value::String(s.clone())).collect();
                let mut entry = serde_json::Map::new();
                entry.insert("out".to_string(), Value::Array(out_arr));
                // Touchpad swipe outputs are continuous → analog mode.
                if new_draft.iter().any(|p| remapper_out_is_swipe(p)) {
                    entry.insert("mode".to_string(), Value::String("analog".to_string()));
                }
                let mut all = mappings.clone();
                all.push(Value::Object(entry));
                node.params.insert(key.to_string(), Value::Array(all));
                remapper_write_str_array(node, draft_key, &[]);
                remapper_write_str_array(node, prev_key, &[]);
                // Disarm so a held nav press after Add doesn't re-capture.
                node.params.insert(armed_key.to_string(), Value::from(false));
                node.params.insert(arm_idle_key.to_string(), Value::from(false));
                // If we Added a Special-only pick ("ready"), return to idle (no
                // active capture). From "learning" (gamepad), stay learning so
                // the user can chain more captures with the same Learn session.
                if new_phase == "ready" {
                    node.params.insert(phase_key.to_string(), Value::String("idle".to_string()));
                }
            }
        }
    });
    // Publish action-button rects (global) so the nav driver can glow the
    // focused one. Order MUST match `nav_remap_action_items` for Lean: Learn,
    // Special, Clear(has_draft), Add((learning||ready) && draft). Use the cards'
    // scope.
    publish_nav_action_rects_scoped(ui, node_id, key,
        &[learn_rect, special_rect, clear_rect, add_rect]);

    // Status / draft preview line. During "learning" with an empty draft, prompt
    // for the chord; otherwise (learning or "ready" with a draft) show the
    // captured/picked chord chips so the user sees what Add will commit.
    if new_phase == "learning" && new_draft.is_empty() {
        ui.label(egui::RichText::new("Press a button or combination")
            .size(13.0).color(Color32::from_rgb(106, 167, 255)));
        request_repaint_throttled(ui.ctx());
    } else if (new_phase == "learning" || new_phase == "ready") && !new_draft.is_empty() {
        ui.horizontal_wrapped(|ui| {
            remapper_render_chord(ui, &new_draft, skin);
        });
        if new_phase == "learning" { request_repaint_throttled(ui.ctx()); }
    }

    // ── Mapping cards ───────────────────────────────────────────────────
    if mappings.is_empty() { return; }

    // Filter row. Lean cards capture OUTPUTS (the lean direction is the
    // trigger), so the filter matches the live-pressed input against each
    // card's assigned outputs. The grouped chip also catches analog
    // destinations (stick axes/cardinals) on the output side. Read SOURCE pins
    // only (the upstream device) — not OS KB/M — so injected output keys don't
    // flicker the filter.
    let filter_live: Vec<String> = match (&upstream_dev_id, wired) {
        (Some(dev), true) => remapper_pressed_now(live_signals, dev),
        _ => Vec::new(),
    };
    let filter = mapping_filter_row(
        ui,
        egui::Id::new(("fxi_lean_filter", node_id.0, side)),
        &format!("({})", mappings.len()),
        &filter_live,
        skin,
    );

    let mut to_remove: Option<usize> = None;
    let mut to_update: Option<(usize, serde_json::Map<String, Value>)> = None;
    ui.spacing_mut().item_spacing.y = 2.0;
    let reorder_enabled = filter.kind == MapFilterKind::All;
    // Live lean magnitude for THIS side (gyro output slot 3, exported to the
    // UI via last_out) — the preview dot on any open card curve editor.
    let lean_live: Option<f32> = snarl.get_node(node_id)
        .and_then(|n| n.extra.last_out.get(3).copied().flatten())
        .map(|s| s.as_float())
        .map(|v| if side == "left" { (-v).max(0.0) } else { v.max(0.0) });
    let mut rv = ReorderView::begin(
        ui, egui::Id::new(("fxi_lean_reorder", node_id.0, side)), reorder_enabled,
    );
    let mut slot = 0usize;
    for (i, m) in mappings.iter().enumerate() {
        let out_pins: Vec<String> = m.get("out").and_then(|v| v.as_array())
            .map(|a| a.iter().filter_map(|v| v.as_str().map(|s| s.to_string())).collect())
            .unwrap_or_default();

        if !mapping_passes_filter(&filter, &out_pins) { continue; }

        if let Some(h) = rv.gap_before(slot) { draw_insertion_gap(ui, h); }

        let mut working: serde_json::Map<String, Value> = m.as_object().cloned().unwrap_or_default();
        let mut working_changed = false;
        let drag_off = rv.offset_for(i);
        ui.push_id(("fxi_lean_card", node_id.0, side, i), |ui| {
            // Cap card width at 358 (Figma design width) so the painter's
            // scale factor `s` stays = 1.0. Above that, all painter-drawn
            // text + pills scale up while built-in widgets (DragValue)
            // keep their default size, so the time-gap value box ends up
            // looking oversized relative to the labels.
            ui.allocate_ui_with_layout(
                egui::vec2(358.0_f32.min(ui.available_width()), 1.0),
                egui::Layout::top_down(egui::Align::Min),
                |ui| {
                    let result = remapper_mapping_card_pixel(
                        ui, node_id, i, &mut working,
                        &out_pins, None, skin, true,
                        reorder_enabled, drag_off, key, true,
                    );
                    if result.delete_clicked { to_remove = Some(i); }
                    if result.changed { working_changed = true; }
                    rv.observe(i, &result);
                    // Per-card response curve + manual activation threshold —
                    // the lean gesture is inherently analog, so every card
                    // qualifies. A card threshold replaces the node-level
                    // lean threshold for that card.
                    let nav_uid = curve_nav_uid(ui.ctx(), node_id, key, i);
                    if mapping_card_curve_section(
                        ui, node_id, key, i, &mut working,
                        true, lean_live, nav_uid,
                    ) {
                        working_changed = true;
                    }
                },
            );
        });
        if working_changed { to_update = Some((i, working)); }
        slot += 1;
    }
    if let Some(h) = rv.gap_after_last(slot) { draw_insertion_gap(ui, h); }
    let reorder = rv.finish(ui);
    if let Some((from, to)) = reorder {
        if let Some(node) = snarl.get_node_mut(node_id) {
            if let Some(Value::Array(arr)) = node.params.get_mut(key) {
                reorder_array(arr, from, to);
            }
        }
    }
    if let Some(idx) = to_remove {
        if let Some(node) = snarl.get_node_mut(node_id) {
            if let Some(Value::Array(arr)) = node.params.get_mut(key) {
                if idx < arr.len() { arr.remove(idx); }
            }
        }
    }
    if let Some((i, obj)) = to_update {
        if let Some(node) = snarl.get_node_mut(node_id) {
            if let Some(Value::Array(arr)) = node.params.get_mut(key) {
                if let Some(slot) = arr.get_mut(i) { *slot = Value::Object(obj); }
            }
        }
    }
        }, // close allocate_ui_with_layout closure
    );
}
