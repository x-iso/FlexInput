//! 3D controller view node body: model/scheme plumbing, live animation
//! state, colour scheme save/load.

use super::*;

/// Trace the AutoMap bus from `src` back to the ORIGINATING physical device id,
/// for the 3D viewer's skin auto-detection. Unlike
/// `find_automap_device_id_for_viewer` (which returns injector keys like
/// `remap:` / `lean:` / `forksel:` for signal lookup), this always follows the
/// bus through every module to the real `device.source`, so auto-detect works
/// when the Device input is wired through a Remapper, Gyro 3DOF, Fork, etc., or
/// across sub-patch boundaries. Returns `None` if no device is reachable.
pub(crate) fn controller3d_physical_device(
    snarl: &Snarl<NodeData>,
    src: OutPinId,
    parents: Option<&AutomapGlowParent<'_>>,
) -> Option<(String, f32)> {
    let node = snarl.get_node(src.node)?;
    match node.module_id.as_str() {
        // Returns the device id + the source's stick deadzone, so the viewer
        // shows the same post-deadzone stick deflection the engine feeds the
        // graph. 0.1 mirrors the engine default for an omitted param.
        "device.source" => {
            let dz = node
                .params
                .get("deadzone")
                .and_then(|v| v.as_f64())
                .unwrap_or(0.1) as f32;
            node.params
                .get("device_id")
                .and_then(|v| v.as_str())
                .map(|s| (s.to_string(), dz))
        }
        "subpatch" => {
            // Descend through the outlet feeding this output pin.
            let sp = node.subpatch.as_ref()?;
            let outlet_id: NodeId = sp
                .snarl
                .nodes_ids_data()
                .find(|(_, n)| {
                    n.value.module_id == "subpatch.outlet"
                        && n.value.params.get("pin_index").and_then(|v| v.as_u64())
                            == Some(src.output as u64)
                })
                .map(|(id, _)| id)?;
            let outlet_in = sp.snarl.in_pin(InPinId { node: outlet_id, input: 0 });
            let inner_upstream = *outlet_in.remotes.first()?;
            let frame = AutomapGlowParent { snarl, subpatch_node_id: src.node, prev: parents };
            controller3d_physical_device(&sp.snarl, inner_upstream, Some(&frame))
        }
        "subpatch.inlet" => {
            // Pop back out to the enclosing snarl through the matching inlet.
            let pin_idx = node.params.get("pin_index").and_then(|v| v.as_u64())? as usize;
            let p = parents?;
            let outer_in = p.snarl.in_pin(InPinId { node: p.subpatch_node_id, input: pin_idx });
            let upstream = *outer_in.remotes.first()?;
            controller3d_physical_device(p.snarl, upstream, p.prev)
        }
        _ => {
            // Any other module carrying the bus: follow its first AutoMap input.
            let am_idx = node.inputs.iter().position(|p| p.signal_type == SignalType::AutoMap)?;
            let in_pin = snarl.in_pin(InPinId { node: src.node, input: am_idx });
            let upstream = *in_pin.remotes.first()?;
            controller3d_physical_device(snarl, upstream, parents)
        }
    }
}

/// Shared 3D-viewer painter used by both the node body and pinned/overlay
/// instances. Draws the styled background, the model (cropped to the visible
/// area, tinted) or an error hint, and an optional outline into `full_rect`.
/// Does NOT allocate — the caller reserves `full_rect` first.
pub(crate) fn render_controller3d_core(
    ui: &mut egui::Ui,
    // Stable per-viewer id. ❗ GPU callback resources are a type-map SHARED by
    // every callback in a frame, so two viewers without distinct ids overwrite
    // each other's buffers and both render the last one prepared.
    instance: u64,
    full_rect: egui::Rect,
    resolved: &str,
    orientation: glam::Quat,
    bg: egui::Color32,
    outline: egui::Color32,
    outline_w: f32,
    scheme: [[f32; 4]; crate::model::N_GROUPS],
    global_alpha: f32,
    cam_pitch: f32,
    live: crate::model::ControllerLive,
    composite: f32,
) {
    use crate::model;
    match model::load_model_cached(resolved) {
        Some(m) => {
            ui.painter_at(full_rect).rect_filled(full_rect, 4.0, bg);
            // Crop to the visible area (egui clamps the GPU viewport to the
            // screen; painting the full-rect projection into the visible sub-rect
            // crops at the edge instead of squashing). `full_rect` still frames
            // the camera so the model keeps a stable size.
            let vis_rect = full_rect.intersect(ui.clip_rect());
            if vis_rect.width() > 1.0 && vis_rect.height() > 1.0 {
                model::paint_controller_model(
                    ui, instance, vis_rect, full_rect, m, orientation, scheme, global_alpha, cam_pitch,
                    live, composite,
                );
            }
            ui.ctx().request_repaint(); // live pose cadence
        }
        None => {
            let painter = ui.painter_at(full_rect);
            painter.rect_filled(full_rect, 4.0, egui::Color32::from_rgb(26, 20, 20));
            let msg = if model::models_base_dir().is_none() {
                "No models folder found\n(app/assets/models)".to_string()
            } else if resolved.is_empty() {
                "No controller models available".to_string()
            } else {
                format!("Couldn't load model:\n{resolved}")
            };
            painter.text(
                full_rect.center(),
                egui::Align2::CENTER_CENTER,
                msg,
                egui::FontId::proportional(12.0),
                egui::Color32::from_rgb(210, 160, 160),
            );
        }
    }
    if outline_w > 0.0 {
        ui.painter_at(full_rect).rect_stroke(
            full_rect,
            4.0,
            egui::Stroke::new(outline_w, outline),
            egui::StrokeKind::Inside,
        );
    }
}

/// Default highlight accent for the 3D viewer (matches the Input Viewer's
/// default amber so the style swatches agree with what renders).
pub(crate) const C3D_DEFAULT_ACCENT: [u8; 4] = [255, 196, 90, 255];

/// Resolve `(bg, outline, outline_w, highlight_rgb)` for a 3D viewer from its
/// per-pin style override, falling back to the node body's defaults when unset.
/// (Model colours live in the per-node material scheme, not here; the highlight
/// is the style `accent`, returned as shader-space 0..1 RGB.)
pub(crate) fn controller3d_style(
    ov: Option<&crate::canvas::node::IvStyleOverride>,
) -> (egui::Color32, egui::Color32, f32, [f32; 3]) {
    let c = |a: [u8; 4]| egui::Color32::from_rgba_unmultiplied(a[0], a[1], a[2], a[3]);
    let hl = |a: [u8; 4]| [a[0] as f32 / 255.0, a[1] as f32 / 255.0, a[2] as f32 / 255.0];
    match ov {
        Some(o) => (c(o.bg), c(o.outline), o.outline_px, hl(o.accent)),
        None => (
            egui::Color32::from_rgb(18, 18, 22), // bg
            egui::Color32::from_rgb(70, 70, 75), // outline colour (only if width > 0)
            0.0,                                 // outline width (none by default)
            hl(C3D_DEFAULT_ACCENT),
        ),
    }
}

/// Build the per-node material scheme (per-model default + per-group param
/// overrides) and overlay opacity for a 3D viewer node. Colours are converted to
/// the shader's 0..1 RGB; the fixed Mic colour is left at its default.
/// Default camera elevation (degrees above horizontal) — a 3/4 overhead view.
pub(crate) const C3D_DEFAULT_PITCH_DEG: f32 = 38.0;

/// The node's effective u8 colour scheme: per-model default with the node's
/// `materials` param overrides applied. Shared by the shader-space converter
/// below and the inspector-strip publisher.
pub(crate) fn controller3d_scheme_rgb(
    snarl: &Snarl<NodeData>,
    node_id: NodeId,
    model_name: &str,
) -> crate::model::Scheme {
    let mut rgb = crate::model::default_scheme(model_name);
    if let Some(mats) = snarl
        .get_node(node_id)
        .and_then(|n| n.params.get("materials"))
        .and_then(|v| v.as_object())
    {
        c3d_apply_materials(&mut rgb, mats);
    }
    rgb
}

/// Apply a `materials`-shaped map (`"group"` → `[r, g, b(, a)]`) onto a u8
/// scheme in place. Shared by the node scheme builder above and the per-pin
/// `IvStyleOverride::c3d_materials` override path.
pub(crate) fn c3d_apply_materials(
    rgb: &mut crate::model::Scheme,
    mats: &serde_json::Map<String, serde_json::Value>,
) {
    for (_, groups) in crate::model::material::ROWS {
        for &g in *groups {
            if let Some(arr) = mats.get(g.key()).and_then(|v| v.as_array()) {
                if arr.len() >= 3 {
                    rgb[g as usize] = [
                        arr[0].as_u64().unwrap_or(0) as u8,
                        arr[1].as_u64().unwrap_or(0) as u8,
                        arr[2].as_u64().unwrap_or(0) as u8,
                        arr.get(3).and_then(|v| v.as_u64()).unwrap_or(255) as u8,
                    ];
                }
            }
        }
    }
}

pub(crate) fn controller3d_scheme(
    snarl: &Snarl<NodeData>,
    node_id: NodeId,
    model_name: &str,
) -> ([[f32; 4]; crate::model::N_GROUPS], f32, f32) {
    controller3d_scheme_with(snarl, node_id, model_name, None)
}

/// Like `controller3d_scheme`, with a pin's `c3d_materials` override applied
/// over the node's shared scheme (missing groups fall through per group).
pub(crate) fn controller3d_scheme_with(
    snarl: &Snarl<NodeData>,
    node_id: NodeId,
    model_name: &str,
    pin_mats: Option<&serde_json::Map<String, serde_json::Value>>,
) -> ([[f32; 4]; crate::model::N_GROUPS], f32, f32) {
    let mut rgb = controller3d_scheme_rgb(snarl, node_id, model_name);
    if let Some(m) = pin_mats {
        c3d_apply_materials(&mut rgb, m);
    }
    let node = snarl.get_node(node_id);
    let alpha = node
        .and_then(|n| n.params.get("overlay_alpha").and_then(|v| v.as_f64()))
        .unwrap_or(1.0) as f32;
    let cam_pitch = node
        .and_then(|n| n.params.get("cam_pitch").and_then(|v| v.as_f64()))
        .unwrap_or(C3D_DEFAULT_PITCH_DEG as f64) as f32;
    let mut out = [[0.0f32; 4]; crate::model::N_GROUPS];
    for i in 0..crate::model::N_GROUPS {
        out[i] = [
            rgb[i][0] as f32 / 255.0,
            rgb[i][1] as f32 / 255.0,
            rgb[i][2] as f32 / 255.0,
            rgb[i][3] as f32 / 255.0,
        ];
    }
    (out, alpha.clamp(0.0, 1.0), cam_pitch.to_radians())
}

/// Smoothed highlight for one part: snaps up instantly, fades to zero over
/// roughly `tailoff` seconds. State is cached in egui temp memory per node+key.
pub(crate) fn c3d_glow(ctx: &egui::Context, node_uid: usize, key: &str, target: f32, tailoff: f32) -> f32 {
    let id = egui::Id::new(("c3d_glow", node_uid, key));
    let prev = ctx.data(|d| d.get_temp::<f32>(id)).unwrap_or(0.0);
    let dt = ctx.input(|i| i.stable_dt).clamp(0.0, 0.1);
    let v = if target >= prev {
        target
    } else {
        // ~4 time-constants to fade out over `tailoff` seconds.
        let rate = 4.0 / tailoff.max(0.05);
        prev + (target - prev) * (1.0 - (-rate * dt).exp())
    };
    ctx.data_mut(|d| d.insert_temp(id, v));
    v
}

/// Live input state driving the 3D model (touch dots, stick tilt, trigger pull,
/// button presses, and the fading active-input highlight) for the resolved
/// device. Reads the same live-signal pin keys the 2D input viewer uses; touch
/// is normalized via `pad_point_to_unit` (the codebase-standard mapping).
pub(crate) fn controller3d_live(
    live_signals: &std::collections::HashMap<(String, String), Signal>,
    dev_id: Option<&str>,
    ctx: &egui::Context,
    node_uid: usize,
    tailoff: f32,
    highlight: [f32; 3],
    deadzone: f32,
) -> crate::model::ControllerLive {
    let mut live = crate::model::ControllerLive::default();
    live.highlight = highlight;
    let Some(dev) = dev_id else { return live };
    let f = |pin: &str| {
        live_signals
            .get(&(dev.to_string(), pin.to_string()))
            .map(|s| s.as_float())
            .unwrap_or(0.0)
    };
    let b = |pin: &str| {
        live_signals
            .get(&(dev.to_string(), pin.to_string()))
            .map(|s| s.as_bool())
            .unwrap_or(false)
    };
    let has = |pin: &str| live_signals.contains_key(&(dev.to_string(), pin.to_string()));

    // Touch dots (normalized, y already flipped by pad_point_to_unit).
    for (k, (px, py, pa)) in [
        ("touch1_x", "touch1_y", "touch1_active"),
        ("touch2_x", "touch2_y", "touch2_active"),
    ]
    .into_iter()
    .enumerate()
    {
        if b(pa) {
            let (ux, uy) = flexinput_core::touchzones::pad_point_to_unit(f(px), f(py));
            live.touch[k] = Some(glam::Vec2::new(ux, uy));
        }
    }

    // Sticks (Vec2 pin or split x/y), triggers (analog + digital fallback).
    // The same radial deadzone the engine applies (`apply_deadzone`) is
    // reproduced here so the model's tilt matches what the graph receives.
    let stick = |vec_pin: &str, xp: &str, yp: &str| -> glam::Vec2 {
        let raw = if let Some(Signal::Vec2(v)) =
            live_signals.get(&(dev.to_string(), vec_pin.to_string()))
        {
            glam::Vec2::new(v.x, v.y)
        } else {
            glam::Vec2::new(f(xp), f(yp))
        };
        let len = raw.length();
        if deadzone <= 0.0 || len <= 0.0 {
            raw
        } else if len < deadzone {
            glam::Vec2::ZERO
        } else {
            raw / len * ((len - deadzone) / (1.0 - deadzone).max(f32::EPSILON))
        }
    };
    live.left_stick = stick("left_stick", "left_stick_x", "left_stick_y");
    live.right_stick = stick("right_stick", "right_stick_x", "right_stick_y");
    live.left_stick_press = if b("btn_ls") { 1.0 } else { 0.0 };
    live.right_stick_press = if b("btn_rs") { 1.0 } else { 0.0 };
    live.left_trigger = if has("left_trigger") { f("left_trigger") } else if b("btn_lt_dig") { 1.0 } else { 0.0 };
    live.right_trigger = if has("right_trigger") { f("right_trigger") } else if b("btn_rt_dig") { 1.0 } else { 0.0 };

    // Buttons keyed by MESH PART name → live pin (bool → 0/1).
    const BUTTON_PART_PINS: &[(&str, &str)] = &[
        ("a_button", "btn_south"),
        ("b_button", "btn_east"),
        ("x_button", "btn_west"),
        ("y_button", "btn_north"),
        ("dpad_up", "dpad_up"),
        ("dpad_down", "dpad_down"),
        ("dpad_left", "dpad_left"),
        ("dpad_right", "dpad_right"),
        ("start_button", "btn_start"),
        ("back_button", "btn_back"),
        ("guide_button", "btn_guide"),
        // Switch Pro capture / screenshot button (the model names it "misc").
        ("misc", "btn_capture"),
        ("left_bumper", "btn_lb"),
        ("right_bumper", "btn_rb"),
    ];
    for (part, pin) in BUTTON_PART_PINS {
        if b(pin) {
            live.buttons.insert((*part).to_string(), 1.0);
        }
    }

    // LED / lightbar relay: the feedback pins the graph sends toward the device
    // (carried on the AutoMap bus). Accept either 0..1 or 0..255 encodings.
    if has("lightbar_r") || has("lightbar_g") || has("lightbar_b") {
        let norm = |v: f32| if v > 1.5 { v / 255.0 } else { v };
        live.led = Some([norm(f("lightbar_r")), norm(f("lightbar_g")), norm(f("lightbar_b"))]);
    }

    // Active-input highlight (per mesh part, time-smoothed with tail-off). We
    // update every tracked part each frame so released inputs keep fading.
    let mut glow = |part: &str, target: f32| {
        let v = c3d_glow(ctx, node_uid, part, target.clamp(0.0, 1.0), tailoff);
        if v > 0.01 {
            live.glow.insert(part.to_string(), v);
        }
    };
    for (part, _) in BUTTON_PART_PINS {
        glow(part, if live.buttons.contains_key(*part) { 1.0 } else { 0.0 });
    }
    glow("left_trigger", live.left_trigger);
    glow("right_trigger", live.right_trigger);
    // Sticks: movement lights the dome + rim; the CAP lights on an L3/R3
    // click only (so a click reads distinctly from deflection).
    let lmag = live.left_stick.length().min(1.0);
    let rmag = live.right_stick.length().min(1.0);
    for p in ["left_stick", "left_ring"] {
        glow(p, lmag);
    }
    for p in ["right_stick", "right_ring"] {
        glow(p, rmag);
    }
    glow("left_cap", live.left_stick_press);
    glow("right_cap", live.right_stick_press);
    // Touchpad click lights the whole pad surface.
    glow("touchpad", if b("btn_touchpad") { 1.0 } else { 0.0 });
    live
}

/// Material colour swatch (RGBA — alpha renders the group as translucent
/// plastic) with a right-click Copy/Paste menu (app-wide colour clipboard in
/// egui temp memory; Copy also puts a hex string on the system clipboard).
/// Returns true when the colour changed (picked OR pasted). Alpha is edited.
pub(crate) fn c3d_color_swatch(ui: &mut egui::Ui, col: &mut [u8; 4], tooltip: &str) -> bool {
    fxi_color_swatch(ui, col, tooltip, true)
}


/// Save a controller colour scheme to a `.fxcol` JSON file. The native dialog
/// runs on its OWN thread — a blocking dialog on the UI thread starves the
/// overlay viewport's rendering (white bars, dead hotkeys) until dismissed.
pub(crate) fn controller3d_scheme_save(scheme: &crate::model::Scheme) {
    let mut map = serde_json::Map::new();
    for (_, groups) in crate::model::material::ROWS {
        for &g in *groups {
            let c = scheme[g as usize];
            // Opaque colours save as RGB (hand-edit friendly); translucent
            // ones carry the alpha as a 4th component.
            let v = if c[3] == 255 {
                serde_json::json!([c[0], c[1], c[2]])
            } else {
                serde_json::json!([c[0], c[1], c[2], c[3]])
            };
            map.insert(g.key().to_string(), v);
        }
    }
    std::thread::spawn(move || {
        if let Some(path) = rfd::FileDialog::new()
            .add_filter("FlexInput Colours", &["fxcol"])
            .set_file_name("controller.fxcol")
            .save_file()
        {
            if let Ok(json) = serde_json::to_string_pretty(&serde_json::Value::Object(map)) {
                let _ = std::fs::write(path, json);
            }
        }
    });
}

/// Pick + parse a `.fxcol` on a worker thread (same UI-starvation reason as
/// above); the map is delivered via the `(chan, uid)` temp channel. Each
/// caller drains its OWN channel — the module body uses `"c3d_matload"`
/// (→ node params), a pin's inspector strip uses `"c3d_pin_matload"`
/// (→ that pin's `c3d_materials` override) — so a module-side load can never
/// be swallowed by a pinned instance or vice versa.
pub(crate) fn controller3d_scheme_load_async(ctx: &egui::Context, chan: &'static str, uid: usize) {
    let ctx = ctx.clone();
    std::thread::spawn(move || {
        let Some(path) = rfd::FileDialog::new()
            .add_filter("FlexInput Colours", &["fxcol"])
            .pick_file()
        else {
            return;
        };
        let Ok(text) = std::fs::read_to_string(path) else { return };
        let Some(map) = serde_json::from_str::<serde_json::Value>(&text)
            .ok()
            .and_then(|v| v.as_object().cloned())
        else {
            return;
        };
        ctx.data_mut(|d| d.insert_temp(egui::Id::new((chan, uid)), map));
        ctx.request_repaint();
    });
}

pub(crate) fn show_controller3d_body(
    node_id: NodeId,
    ui: &mut egui::Ui,
    snarl: &mut Snarl<NodeData>,
    live_signals: &std::collections::HashMap<(String, String), Signal>,
    parents: Option<&AutomapGlowParent<'_>>,
) {
    use crate::model;

    // Async .fxcol load result (worker-thread file dialog) → node params.
    if let Some(map) = ui.ctx().data_mut(|d| {
        d.remove_temp::<serde_json::Map<String, serde_json::Value>>(egui::Id::new((
            "c3d_matload",
            node_id.0,
        )))
    }) {
        if let Some(node) = snarl.get_node_mut(node_id) {
            node.params
                .insert("materials".into(), serde_json::Value::Object(map));
        }
    }

    // Model override ("" / "auto" = auto-detect from the connected device).
    let override_name = snarl
        .get_node(node_id)
        .and_then(|n| n.params.get("model").and_then(|v| v.as_str()))
        .unwrap_or("")
        .to_string();

    // Auto-detect the skin by tracing the Device bus (input 0) back to the
    // physical source — through Remapper/Gyro/Fork/sub-patches, not just a
    // direct device.source wire. Also yields the source's stick deadzone.
    // `parents` is what lets the walk pop OUT through a `subpatch.inlet` into
    // the enclosing canvas — without it a viewer living inside a sub-patch can
    // never reach its device, and the model renders inert (the pinned renderer
    // has always passed its bridged parent; the body used to pass `None`).
    let traced = snarl
        .in_pin(InPinId { node: node_id, input: 0 })
        .remotes
        .first()
        .copied()
        .and_then(|src| controller3d_physical_device(snarl, src, parents));
    let (dev_id, deadzone) = match traced {
        Some((d, z)) => (Some(d), z),
        None => (None, 0.1),
    };
    let resolved = if !override_name.is_empty() && override_name != "auto" {
        override_name.clone()
    } else if let Some(dev) = dev_id.as_deref() {
        model::model_for_device(dev)
    } else {
        model::available_models().into_iter().next().unwrap_or_default()
    };

    // Orientation quaternion from the Vec4 input (index 1), else identity. The
    // Gyro module already applies any orientation scale to the integration rate
    // (the only place scaling a full 3D pose is continuous), so the viewer just
    // displays the quaternion as received.
    let orientation = snarl
        .get_node(node_id)
        .and_then(|n| n.extra.last_signals.get(1).copied().flatten())
        .and_then(|s| match s {
            Signal::Vec4(v) => Some(glam::Quat::from_xyzw(v.x, v.y, v.z, v.w)),
            _ => None,
        })
        .filter(|q| q.length_squared() > 1e-6)
        .map(|q| q.normalize())
        .unwrap_or(glam::Quat::IDENTITY);

    ui.vertical(|ui| {
        // Model picker: Auto (device) + every available model folder.
        let models = model::available_models();
        let mut sel = if override_name.is_empty() { "auto".to_string() } else { override_name.clone() };
        let mut new_sel: Option<String> = None;
        egui::ComboBox::from_id_salt(("c3d_model", node_id))
            .selected_text(if sel == "auto" { "Auto (device)".to_string() } else { sel.clone() })
            .show_ui(ui, |ui| {
                if ui.selectable_value(&mut sel, "auto".to_string(), "Auto (device)").changed() {
                    new_sel = Some(String::new());
                }
                for m in &models {
                    if ui.selectable_value(&mut sel, m.clone(), m).changed() {
                        new_sel = Some(m.clone());
                    }
                }
            });
        if let Some(val) = new_sel {
            // Unless the user opted to keep custom colours, swapping the model
            // resets the scheme to the new model's defaults.
            let keep = snarl
                .get_node(node_id)
                .and_then(|n| n.params.get("keep_colors").and_then(|v| v.as_bool()))
                .unwrap_or(false);
            if let Some(node) = snarl.get_node_mut(node_id) {
                node.params.insert("model".to_string(), serde_json::Value::String(val));
                if !keep {
                    node.params.remove("materials");
                }
            }
        }

        // Per-node colour scheme + overlay opacity. Colours default to the
        // model's tuned palette; overrides persist under `materials`.
        egui::CollapsingHeader::new("🎨 Colours")
            .id_salt(("c3d_colors", node_id))
            .default_open(false)
            .show(ui, |ui| {
                let defaults = crate::model::default_scheme(&resolved);
                let mut mats: serde_json::Map<String, serde_json::Value> = snarl
                    .get_node(node_id)
                    .and_then(|n| n.params.get("materials"))
                    .and_then(|v| v.as_object().cloned())
                    .unwrap_or_default();
                let mut mats_changed = false;
                let read_col = |mats: &serde_json::Map<String, serde_json::Value>,
                                g: crate::model::MaterialGroup,
                                def: [u8; 4]| -> [u8; 4] {
                    mats.get(g.key())
                        .and_then(|v| v.as_array())
                        .and_then(|a| {
                            if a.len() >= 3 {
                                Some([
                                    a[0].as_u64()? as u8,
                                    a[1].as_u64()? as u8,
                                    a[2].as_u64()? as u8,
                                    a.get(3).and_then(|v| v.as_u64()).unwrap_or(255) as u8,
                                ])
                            } else {
                                None
                            }
                        })
                        .unwrap_or(def)
                };
                // One row per general group; multi-material groups (Shell,
                // Sticks) show their swatches side by side on that row.
                egui::Grid::new(("c3d_col_grid", node_id))
                    .num_columns(2)
                    .spacing([8.0, 4.0])
                    .show(ui, |ui| {
                        for (row_label, groups) in crate::model::material::ROWS {
                            ui.label(egui::RichText::new(*row_label).small());
                            ui.horizontal(|ui| {
                                for &g in *groups {
                                    let mut col = read_col(&mats, g, defaults[g as usize]);
                                    if c3d_color_swatch(ui, &mut col, g.label()) {
                                        mats.insert(
                                            g.key().to_string(),
                                            serde_json::json!([col[0], col[1], col[2], col[3]]),
                                        );
                                        mats_changed = true;
                                    }
                                }
                            });
                            ui.end_row();
                        }
                    });
                ui.horizontal(|ui| {
                    if ui
                        .small_button("Reset")
                        .on_hover_text("Reset to this model's default colours")
                        .clicked()
                    {
                        if let Some(node) = snarl.get_node_mut(node_id) {
                            node.params.remove("materials");
                        }
                    }
                    if ui
                        .small_button("Save…")
                        .on_hover_text("Save these colours to a .fxcol file")
                        .clicked()
                    {
                        // Effective scheme = model default with the user's overrides applied.
                        let mut eff = defaults;
                        for (_, groups) in crate::model::material::ROWS {
                            for &g in *groups {
                                eff[g as usize] = read_col(&mats, g, defaults[g as usize]);
                            }
                        }
                        controller3d_scheme_save(&eff);
                    }
                    if ui
                        .small_button("Load…")
                        .on_hover_text("Load colours from a .fxcol file")
                        .clicked()
                    {
                        controller3d_scheme_load_async(ui.ctx(), "c3d_matload", node_id.0);
                    }
                });

                // Keep-custom-colours toggle: when off, swapping the model resets
                // the scheme to the new model's defaults.
                let mut keep = snarl
                    .get_node(node_id)
                    .and_then(|n| n.params.get("keep_colors").and_then(|v| v.as_bool()))
                    .unwrap_or(false);
                if ui
                    .checkbox(&mut keep, "Keep custom colours on model change")
                    .changed()
                {
                    if let Some(node) = snarl.get_node_mut(node_id) {
                        node.params.insert("keep_colors".into(), serde_json::json!(keep));
                    }
                }

                if mats_changed {
                    if let Some(node) = snarl.get_node_mut(node_id) {
                        node.params.insert("materials".into(), serde_json::Value::Object(mats));
                    }
                }
            });

        // Camera view angle: elevation above the controller (0° = level/front,
        // higher = more overhead). Defaults to a 3/4 overhead view.
        let mut pitch_deg = snarl
            .get_node(node_id)
            .and_then(|n| n.params.get("cam_pitch").and_then(|v| v.as_f64()))
            .unwrap_or(C3D_DEFAULT_PITCH_DEG as f64) as f32;
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("View angle").small())
                .on_hover_text("Camera elevation above the controller.\n0° = front/level, higher = more overhead.");
            if ui
                .add(egui::Slider::new(&mut pitch_deg, 0.0..=85.0).suffix("°"))
                .changed()
            {
                if let Some(node) = snarl.get_node_mut(node_id) {
                    node.params.insert("cam_pitch".into(), serde_json::json!(pitch_deg));
                }
            }
        });

        // Model opacity: whole-model see-through (per-fragment alpha). Distinct
        // from the overlay's 2D composite alpha (an Overlay-style option). Kept
        // here as a module option — useful for seeing activated inputs behind
        // the shell.
        let mut opacity = snarl
            .get_node(node_id)
            .and_then(|n| n.params.get("overlay_alpha").and_then(|v| v.as_f64()))
            .unwrap_or(1.0) as f32;
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("Model opacity").small())
                .on_hover_text("Whole-model transparency (see-through).");
            if ui
                .add(egui::Slider::new(&mut opacity, 0.1..=1.0))
                .changed()
            {
                if let Some(node) = snarl.get_node_mut(node_id) {
                    node.params
                        .insert("overlay_alpha".into(), serde_json::json!(opacity));
                }
            }
        });

        // Active-input highlight fade time (how long a press glow lingers).
        let mut tail = snarl
            .get_node(node_id)
            .and_then(|n| n.params.get("highlight_tailoff").and_then(|v| v.as_f64()))
            .unwrap_or(0.25) as f32;
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("Highlight fade").small())
                .on_hover_text("How long the active-input highlight takes to fade out.");
            if ui
                .add(egui::Slider::new(&mut tail, 0.05..=2.0).suffix(" s"))
                .changed()
            {
                if let Some(node) = snarl.get_node_mut(node_id) {
                    node.params
                        .insert("highlight_tailoff".into(), serde_json::json!(tail));
                }
            }
        });

        // Resizable rectangular viewport (persisted like the scopes).
        let size_id = egui::Id::new(("c3d_size", node_id));
        let mut size = ui
            .ctx()
            .data_mut(|d| d.get_persisted::<[f32; 2]>(size_id))
            .unwrap_or([200.0, 160.0]);
        size[0] = size[0].max(80.0);
        size[1] = size[1].max(60.0);
        let rect = egui::Rect::from_min_size(ui.cursor().min, egui::vec2(size[0], size[1]));
        // Reserve the full rect for layout so the node keeps a stable size (the
        // painter/callback don't allocate). The body always renders with default
        // style; per-pin styling applies to pinned/overlay instances.
        ui.allocate_rect(rect, egui::Sense::hover());
        let (bg, outline, outline_w, accent) = controller3d_style(None);
        let (scheme, alpha, cam_pitch) = controller3d_scheme(snarl, node_id, &resolved);
        let tailoff = snarl
            .get_node(node_id)
            .and_then(|n| n.params.get("highlight_tailoff").and_then(|v| v.as_f64()))
            .unwrap_or(0.25) as f32;
        let ctx = ui.ctx().clone();
        let live = controller3d_live(
            live_signals, dev_id.as_deref(), &ctx, node_id.0, tailoff, accent, deadzone,
        );
        render_controller3d_core(
            ui, node_id.0 as u64, rect, &resolved, orientation, bg, outline, outline_w, scheme,
            alpha, cam_pitch, live, 1.0,
        );
        // Expose the viewport so it can be pinned to a sub-patch layout / overlay.
        register_exposable_element(ui, node_id, "viewer", rect);

        // Bottom-right resize handle.
        let hs = 12.0;
        let handle = egui::Rect::from_min_size(
            egui::pos2(rect.right() - hs, rect.bottom() - hs),
            egui::vec2(hs, hs),
        );
        let hr = ui.interact(handle, size_id.with("h"), egui::Sense::click_and_drag());
        if hr.hovered() || hr.dragged() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeNwSe);
        }
        if hr.dragged() {
            let d = hr.drag_delta();
            size[0] = (size[0] + d.x).max(80.0);
            size[1] = (size[1] + d.y).max(60.0);
            ui.ctx().data_mut(|d2| d2.insert_persisted(size_id, size));
        }
    });
}
