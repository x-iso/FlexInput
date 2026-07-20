//! Sub-patch editor windows: the .fxsp file format, transient capture-state
//! clearing, keyboard/mouse cell labels, and inner-canvas port syncing.

use super::*;

// ── Sub-patch editor windows ──────────────────────────────────────────────────

pub(crate) const PINNED_PAD: f32  = 4.0;

/// On-disk representation of a single sub-patch (.fxsp). Distinct from the
/// top-level patch format (.fxp) so the save/load dialog filters cleanly.
#[derive(serde::Serialize, serde::Deserialize)]
pub(crate) struct SubPatchFile {
    pub(crate) version: u32,
    pub(crate) sub_patch: UiSubPatch,
}

/// Short human label for a KB/M pin (fallback when no icon, or for the chord
/// preview). Strips the `key_`/`mouse_`/`scroll_` prefix and prettifies a few.
pub(crate) fn kbm_pin_label(pin: &str) -> String {
    match pin {
        "mouse_left" => "LMB".into(),
        "mouse_right" => "RMB".into(),
        "mouse_middle" => "MMB".into(),
        "mouse_back" => "MB4".into(),
        "mouse_forward" => "MB5".into(),
        "scroll_up" => "Scroll↑".into(),
        "scroll_down" => "Scroll↓".into(),
        "scroll_left" => "Scroll←".into(),
        "scroll_right" => "Scroll→".into(),
        "scroll_y" => "Scroll⇅".into(),
        "scroll_x" => "Scroll⇄".into(),
        "key_pageup" => "PgUp".into(),
        "key_pagedown" => "PgDn".into(),
        "key_insert" => "Ins".into(),
        "key_delete" => "Del".into(),
        "key_printscreen" => "PrtSc".into(),
        "key_pause" => "Pause".into(),
        "key_space" => "Space".into(),
        "key_enter" => "Enter".into(),
        "key_backspace" => "Bksp".into(),
        "key_escape" => "Esc".into(),
        "key_capslock" => "Caps".into(),
        "key_backtick" => "`".into(),
        "key_arrowup" => "↑".into(),
        "key_arrowdown" => "↓".into(),
        "key_arrowleft" => "←".into(),
        "key_arrowright" => "→".into(),
        "touch_left" => "TP◧".into(),
        "touch_center" => "TP▣".into(),
        "touch_right" => "TP◨".into(),
        "btn_touchpad" => "TP Click".into(),
        "btn_mute" => "Mic".into(),
        "touch_swipe_x" => "Swipe↔".into(),
        "touch_swipe_y" => "Swipe↕".into(),
        "mouse" => "Mouse⤢".into(),
        "mouse_x" => "Mouse↔".into(),
        "mouse_y" => "Mouse↕".into(),
        "left_stick" => "L-Stick".into(),
        "right_stick" => "R-Stick".into(),
        _ => {
            let s = pin.strip_prefix("key_").or_else(|| pin.strip_prefix("mouse_"))
                .unwrap_or(pin);
            let s = s.strip_prefix("num").unwrap_or(s);
            let mut cs = s.chars();
            cs.next().map(|f| f.to_uppercase().collect::<String>() + cs.as_str())
                .unwrap_or_else(|| s.to_string())
        }
    }
}

/// Short textual token for a gamepad pin used in the legend bar when no glyph
/// is available under the active skin (sticks/triggers don't always have a
/// directional SVG). Keeps the hint readable as a fallback.
pub(crate) fn gp_pin_token(pin: &str) -> &'static str {
    match pin {
        "left_stick" | "left_stick_left" | "left_stick_up"
            | "left_stick_horizontal" | "left_stick_vertical" => "LS",
        "right_stick" => "RS",
        "dpad_up" | "dpad_left" | "dpad_down" | "dpad_right"
            | "dpad" | "dpad_horizontal" | "dpad_vertical" => "Dpad",
        "btn_south" => "A", "btn_east" => "B", "btn_west" => "X", "btn_north" => "Y",
        "btn_lb" => "LB", "btn_rb" => "RB",
        "left_trigger" => "LT", "right_trigger" => "RT",
        "btn_ls" => "LS▾", "btn_rs" => "RS▾",
        "btn_start" => "Start", "btn_back" => "Back",
        _ => "•",
    }
}

/// Rasterize a KB/M pin's SVG icon to a cached texture (white, transparent bg).
pub(crate) fn kbm_cell_texture(ctx: &egui::Context, skin: crate::canvas::remapper_icons::Skin, pin: &str)
    -> Option<egui::TextureHandle>
{
    let bytes = crate::canvas::remapper_icons::pin_svg(skin, pin)?;
    let size_px = (26.0 * ctx.pixels_per_point()).round() as u32;
    let cache_key = egui::Id::new(("kbm_picker_icon", bytes.as_ptr() as usize, size_px));
    if let Some(tex) = ctx.data(|d| d.get_temp::<egui::TextureHandle>(cache_key)) {
        return Some(tex);
    }
    let text = std::str::from_utf8(bytes).ok()?;
    let img = crate::canvas::viewer::rasterize_svg_recolored(
        text, size_px, size_px, "override", egui::Color32::TRANSPARENT)?;
    let handle = ctx.load_texture(
        format!("kbm_picker_icon_{:p}", bytes.as_ptr()), img, egui::TextureOptions::LINEAR);
    ctx.data_mut(|d| d.insert_temp(cache_key, handle.clone()));
    Some(handle)
}

/// Clear the transient capture/learn state on every remapper-family inner node
/// of a sub-patch. These params (`ui_phase`, `draft_input`, `draft_output`,
/// `_pressed_prev`, `_nav_capture_armed`, `_nav_arm_idle`, `_tp_click_mode`,
/// `_tp_zones`) describe an in-progress capture gesture, not saved configuration
/// — they must never carry across a patch open or app launch (otherwise a
/// half-captured chord from a previous session blocks starting a fresh Learn).
/// The committed `mappings` array is left untouched.
pub(crate) fn clear_transient_capture_state(sp: &mut UiSubPatch) {
    const TRANSIENT: &[&str] = &[
        "ui_phase", "draft_input", "draft_output", "_pressed_prev",
        "_nav_capture_armed", "_nav_arm_idle", "_nav_act_learn",
        "_nav_act_special", "_nav_act_add", "_nav_act_clear",
        "_tp_click_mode", "_tp_zones",
        // Gyro Lean per-side capture transients.
        "_lean_left_phase", "_lean_left_draft", "_lean_left_pressed_prev",
        "_lean_left_armed", "_lean_left_arm_idle",
        "_nav_act_learn_left", "_nav_act_special_left", "_nav_act_add_left",
        "_nav_act_clear_left",
        "_lean_right_phase", "_lean_right_draft", "_lean_right_pressed_prev",
        "_lean_right_armed", "_lean_right_arm_idle",
        "_nav_act_learn_right", "_nav_act_special_right", "_nav_act_add_right",
        "_nav_act_clear_right",
    ];
    for (_, node_ref) in sp.snarl.nodes_ids_data_mut() {
        let node = &mut node_ref.value;
        if matches!(node.module_id.as_str(),
            "module.remapper" | "module.map_action" | "module.automap_combiner"
            | "processing.gyro_3dof")
        {
            for k in TRANSIENT { node.params.remove(*k); }
        }
    }
}

/// Clear transient capture state across every sub-patch in a canvas (the outer
/// snarl's nodes that carry a `.subpatch`). Used on patch/workspace load.
pub(crate) fn clear_canvas_capture_state(canvas: &mut crate::canvas::Canvas) {
    for (_, node_ref) in canvas.snarl.nodes_ids_data_mut() {
        if let Some(sp) = node_ref.value.subpatch.as_mut() {
            clear_transient_capture_state(sp);
        }
    }
}

pub(crate) fn save_subpatch_file(sp: &UiSubPatch) -> Option<std::path::PathBuf> {
    let default_name = if sp.display_name.is_empty() {
        "sub-patch.fxsp".to_string()
    } else {
        format!("{}.fxsp", sp.display_name)
    };
    let path = crate::overlay::with_overlay_not_topmost(|| {
        rfd::FileDialog::new()
            .add_filter("FlexInput Sub-Patch", &["fxsp"])
            .set_file_name(default_name)
            .save_file()
    })?;
    let file = SubPatchFile { version: 1, sub_patch: sp.clone() };
    if let Ok(json) = serde_json::to_string_pretty(&file) {
        let _ = std::fs::write(&path, json);
    }
    Some(path)
}

pub(crate) fn load_subpatch_file() -> Option<UiSubPatch> {
    let path = crate::overlay::with_overlay_not_topmost(|| {
        rfd::FileDialog::new()
            .add_filter("FlexInput Sub-Patch", &["fxsp"])
            .pick_file()
    })?;
    let json = std::fs::read_to_string(&path).ok()?;
    let file: SubPatchFile = serde_json::from_str(&json).ok()?;
    Some(file.sub_patch)
}

pub(crate) fn show_subpatch_editors(
    app: &mut FlexInputApp,
    ctx: &egui::Context,
    live_device_ids: &std::collections::HashSet<String>,
) {
    let active = app.active_tab;
    // Fast path: no editors and no pending open request → nothing to do.
    // Called every frame from update(), so skip all per-frame work in the
    // common case (empty workspace / no editor open).
    if app.sub_patch_editors.is_empty() && app.tabs[active].canvas.pending_edit_subpatch.is_none() {
        return;
    }
    // Snapshot once so the inner-canvas show call can borrow this immutably while
    // `app` is borrowed mutably for sub-patch editor state.
    // NOTE: `live_signals` is NOT cloned here — `app.last_signals` is already
    // a fresh per-frame clone from `proc_device_signals` (see update()), and
    // we borrow it inside the closure. Saves one HashMap clone per editor
    // frame at large patches.
    let panic_shortcut = app.panic_shortcut.clone();
    let device_defaults_inner = app.nav_device_defaults();

    // Open new editors requested this frame by the canvas viewer.
    let pending = app.tabs[active].canvas.pending_edit_subpatch.take();
    if let Some(node_id) = pending {
        let already_open = app.sub_patch_editors.iter()
            .any(|e| e.tab_idx == active && e.node_id == node_id);
        if !already_open {
            let inner_snarl = app.tabs[active].canvas.snarl
                .get_node(node_id)
                .and_then(|n| n.subpatch.as_ref())
                .map(|sp| *sp.snarl.clone())
                .unwrap_or_else(egui_snarl::Snarl::new);
            let mut editor_canvas = Canvas::new();
            editor_canvas.snarl = inner_snarl;
            editor_canvas.is_inner = true;
            // Derive a STABLE view salt from the owning tab's salt + this
            // sub-patch node's id, so the editor's pan/zoom is remembered
            // across close→reopen and stays distinct from every tab and other
            // sub-patch. (Top-level editor → parent is the tab canvas.)
            editor_canvas.set_view_salt(
                flexinput_engine::namespaced_uid(app.tabs[active].view_salt as usize, node_id.0) as u64,
            );
            app.sub_patch_editors.push(SubPatchEditor {
                tab_idx: active,
                node_id,
                parent_editor_idx: None,
                canvas: editor_canvas,
                open: true,
                last_clipboard_gen: 0,
                last_synced_parent_gen: None,
                last_inner_gen: None,
            });
        }
    }

    // Render each open editor window.
    let mut to_close: Vec<usize> = Vec::new();
    // Collect nested-open requests outside the loop to avoid borrow issues.
    let mut pending_nested: Vec<(usize, NodeId)> = Vec::new(); // (parent_editor_idx, child_node_id)

    // Process in reverse index order so nested (child) editors write-back their
    // inner snarl into the parent editor's canvas BEFORE the parent's iteration
    // reads and propagates it upward. Without this, a child's changes (e.g. pin)
    // would be clobbered by the parent's earlier write-back to the tab canvas.
    for i in (0..app.sub_patch_editors.len()).rev() {
        if app.sub_patch_editors[i].tab_idx != active { continue; }
        let node_id = app.sub_patch_editors[i].node_id;
        let parent_editor_idx = app.sub_patch_editors[i].parent_editor_idx;

        // Close editor if the sub-patch node was deleted.
        // For nested editors, check the parent editor's canvas; for top-level, check tab canvas.
        let node_exists = match parent_editor_idx {
            None => app.tabs[active].canvas.snarl
                .get_node(node_id).map(|n| n.module_id == "subpatch").unwrap_or(false),
            Some(p) => app.sub_patch_editors[p].canvas.snarl
                .get_node(node_id).map(|n| n.module_id == "subpatch").unwrap_or(false),
        };
        if !node_exists {
            to_close.push(i);
            continue;
        }

        // Window title: resolve from parent snarl.
        let display_name = {
            let node_opt = match parent_editor_idx {
                None    => app.tabs[active].canvas.snarl.get_node(node_id),
                Some(p) => app.sub_patch_editors[p].canvas.snarl.get_node(node_id),
            };
            node_opt.map(|n| {
                if !n.display_name.is_empty() { n.display_name.clone() }
                else { n.subpatch.as_ref().map(|sp| sp.display_name.clone()).unwrap_or_else(|| "Sub-patch".to_string()) }
            }).unwrap_or_else(|| "Sub-patch".to_string())
        };

        // Extract inner canvas.
        let mut inner_canvas = std::mem::replace(&mut app.sub_patch_editors[i].canvas, Canvas::new());
        let mut open = app.sub_patch_editors[i].open;

        let descriptors: &[ModuleDescriptor] = &app.descriptors;
        let devices: &[flexinput_devices::PhysicalDevice] = &app.devices;

        // Pre-sync: pull pinned-body param changes from the parent snarl so that
        // sliders/knobs on the sub-patch body remain interactive.
        //
        // Gated on the PARENT canvas's mutation generation. The pre-sync exists
        // only to catch changes made to this sub-patch's contents from OUTSIDE
        // the editor — e.g. an Easy-mode pinned widget on the parent body writing
        // into `node.subpatch` (those edits bump the parent canvas's
        // `mutation_gen` via `track_value_edits`). When the parent hasn't changed
        // since we last synced, the editor's own snarl is the authority and we
        // must NOT overwrite it: doing so every frame reverted in-progress inner
        // gestures (node drags, knob/curve-point drags) because those don't bump
        // `mutation_gen` until they settle on pointer-release, so the write-back
        // below was skipped mid-gesture and this pre-sync clobbered the
        // un-written change next frame. Net effect was "modules won't move and
        // param handles can't be edited inside a sub-patch". (Regression from the
        // write-back gating in e224eab.)
        //
        // Skip when a child editor is open: the child will write-back into this
        // editor's canvas this same frame (reverse loop order), and pre-sync would
        // overwrite those changes with the stale parent state.
        let has_active_child = app.sub_patch_editors.iter().enumerate().any(|(j, e)| {
            j != i && e.tab_idx == active && e.parent_editor_idx == Some(i)
        });
        let parent_gen = match parent_editor_idx {
            None    => app.tabs[active].canvas.mutation_gen,
            Some(p) => app.sub_patch_editors[p].canvas.mutation_gen,
        };
        let parent_changed = app.sub_patch_editors[i].last_synced_parent_gen != Some(parent_gen);
        if !has_active_child && parent_changed {
            puffin::profile_scope!("editor_presync");
            let outer_inner = match parent_editor_idx {
                None    => app.tabs[active].canvas.snarl.get_node(node_id),
                Some(p) => app.sub_patch_editors[p].canvas.snarl.get_node(node_id),
            }.and_then(|n| n.subpatch.as_ref()).map(|sp| *sp.snarl.clone());
            if let Some(snarl) = outer_inner { inner_canvas.snarl = snarl; }
            app.sub_patch_editors[i].last_synced_parent_gen = Some(parent_gen);
        }

        // Live display state (`extra`) is refreshed on the parent's `sp.snarl`
        // every frame by `apply_display_state`, but the editor renders a separate
        // snarl, so its scopes/readouts would freeze at open-time. Push just the
        // `extra` across each frame — independent of the structural-presync gate
        // above, which only fires when the parent's editable state changed. (When
        // a child editor is open the parent's display state may be one frame
        // stale; that self-corrects next frame and is imperceptible.)
        {
            puffin::profile_scope!("editor_display_sync");
            // Borrow the parent's inner snarl directly (no clone): `inner_canvas`
            // was `mem::replace`d out of `app.sub_patch_editors[i].canvas`, so it
            // doesn't alias the tab canvas nor any OTHER editor (parent index p≠i).
            let src_snarl: Option<&Snarl<NodeData>> = match parent_editor_idx {
                None    => app.tabs[active].canvas.snarl.get_node(node_id),
                Some(p) => app.sub_patch_editors[p].canvas.snarl.get_node(node_id),
            }.and_then(|n| n.subpatch.as_ref()).map(|sp| &*sp.snarl);
            if let Some(src_snarl) = src_snarl {
                sync_display_state_into(&mut inner_canvas.snarl, src_snarl);
            }
        }

        // Pinned IDs from parent.
        inner_canvas.pinned_inner_ids = {
            let node_opt = match parent_editor_idx {
                None    => app.tabs[active].canvas.snarl.get_node(node_id),
                Some(p) => app.sub_patch_editors[p].canvas.snarl.get_node(node_id),
            };
            node_opt.and_then(|n| n.subpatch.as_ref())
                .map(|sp| sp.iter_module_pins().map(|(_, m)| m.inner_node_id).collect())
                .unwrap_or_default()
        };

        let mut save_clicked = false;
        let mut load_clicked = false;
        let mut close_self = false; // for "← Back" button on nested editors

        let outer_layout_mode = match parent_editor_idx {
            None    => app.tabs[active].canvas.snarl.get_node(node_id),
            Some(p) => app.sub_patch_editors[p].canvas.snarl.get_node(node_id),
        }.map(|n| n.extra.layout_unlocked).unwrap_or(false);

        // Seed cross-boundary clipboard (outer→inner direction) so the user can
        // paste nodes copied from the outer canvas into this editor.
        // Only seed when the canvas has no clipboard yet (first frame after open).
        // Subsequent frames: the canvas retains its clipboard across mem::replace.
        if inner_canvas.clipboard().is_none() {
            if let Some(ref cb) = app.app_clipboard.clone() {
                inner_canvas.set_clipboard(cb.clone());
            }
        }
        // Snapshot gen before show() so we can detect a real user copy after.
        let gen_before = inner_canvas.clipboard_gen;

        // Captured from the editor viewport's own context if a Special… button
        // inside this editor requested the shared picker (opened after the
        // viewport closure returns, where `app` is mutably available again).
        let mut special_req: Option<crate::canvas::viewer::SpecialPickerRequest> = None;
        // Picker interaction collected inside the viewport closure (which only
        // holds `&app`); applied after it returns.
        let mut picker_result: (Option<String>, bool) = (None, false);

        let viewport_id = egui::ViewportId::from_hash_of(("subpatch_editor", active, node_id.0));
        // Build breadcrumb: "Parent Name > This Name" for nested editors.
        let window_title = if let Some(p) = parent_editor_idx {
            let parent_name = app.sub_patch_editors[p].canvas.snarl
                .get_node(app.sub_patch_editors[p].node_id)
                .map(|n| n.display_name.clone())
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| "Sub-patch".to_string());
            format!("✦ {} › {}", parent_name, display_name)
        } else {
            format!("✦ {}", display_name)
        };

        puffin::profile_scope!("editor_viewport_show");
        ctx.show_viewport_immediate(
            viewport_id,
            egui::ViewportBuilder::default()
                .with_title(window_title)
                .with_inner_size([720.0, 540.0])
                .with_min_inner_size([400.0, 300.0]),
            |vctx, _class| {
                if vctx.input(|i| i.viewport().close_requested()) {
                    open = false;
                }
                crate::canvas::viewer::set_layout_mode_active(vctx, outer_layout_mode);
                // Overlay pick can only address first-level sub-patches (path
                // schema is one level deep in MVP) — keep nested editors'
                // elements cold instead of amber-but-inert.
                let pick_armed = crate::canvas::viewer::overlay_pick_active(vctx);
                if pick_armed && parent_editor_idx.is_some() {
                    crate::canvas::viewer::set_overlay_pick_suppressed(vctx, true);
                }

                egui::TopBottomPanel::top("subpatch_editor_header").show(vctx, |ui| {
                    ui.horizontal(|ui| {
                        // "← Back" for nested editors — closes this level.
                        if parent_editor_idx.is_some()
                            && ui.small_button("← Back").on_hover_text("Close this sub-patch and return to parent").clicked()
                        {
                            close_self = true;
                        }
                        if ui.small_button("💾 Save…")
                            .on_hover_text("Save this sub-patch to a .fxsp file")
                            .clicked() { save_clicked = true; }
                        if ui.small_button("📂 Load…")
                            .on_hover_text("Replace this sub-patch's contents from a .fxsp file")
                            .clicked() { load_clicked = true; }
                        if outer_layout_mode {
                            ui.separator();
                            ui.label(egui::RichText::new("LAYOUT MODE — click highlighted elements to pin")
                                .small().color(egui::Color32::from_rgb(150, 220, 255)));
                        } else if pick_armed && parent_editor_idx.is_none() {
                            ui.separator();
                            ui.label(egui::RichText::new("OVERLAY PICK — click a highlighted element to pin it to the overlay (Esc cancels)")
                                .small().color(egui::Color32::from_rgb(230, 170, 60)));
                        }
                    });
                });
                // Build a one-level AutoMap glow parent frame so the inner
                // canvas can resolve inlet AutoMap activity through the
                // sub-patch boundary. Deeper chains (grandparents) degrade
                // gracefully — the walk just bottoms out one frame earlier.
                let automap_parent = match parent_editor_idx {
                    None => Some(crate::canvas::viewer::AutomapGlowParent {
                        snarl: &app.tabs[active].canvas.snarl,
                        subpatch_node_id: node_id,
                        prev: None,
                    }),
                    Some(p) => Some(crate::canvas::viewer::AutomapGlowParent {
                        snarl: &app.sub_patch_editors[p].canvas.snarl,
                        subpatch_node_id: node_id,
                        prev: None,
                    }),
                };
                egui::CentralPanel::default().show(vctx, |ui| {
                    puffin::profile_scope!("editor_inner_canvas_show");
                    // Borrow `app.last_signals` directly (no clone). For
                    // device_rates we hold a short-lived read guard across
                    // the show call so it borrows the underlying map rather
                    // than cloning it. Canvas::show only reads, never
                    // touches the RwLock itself, so no deadlock risk.
                    let empty = std::collections::HashMap::new();
                    let guard = app.device_rates.read();
                    let device_rates_ref: &std::collections::HashMap<String, u32> =
                        match &guard { Ok(r) => r, Err(_) => &empty };
                    let _ = inner_canvas.show(
                        descriptors, live_device_ids, &app.last_signals,
                        &panic_shortcut, devices, device_rates_ref,
                        device_defaults_inner, ui, automap_parent, None,
                    );
                });

                if let Some((inner_uid, eid, size)) = crate::canvas::viewer::take_layout_pending(vctx) {
                    inner_canvas.pending_expose_module = Some((NodeId(inner_uid), eid, size));
                }
                if pick_armed {
                    if parent_editor_idx.is_some() {
                        crate::canvas::viewer::set_overlay_pick_suppressed(vctx, false);
                    } else if let Some((inner_uid, eid, size)) =
                        crate::canvas::viewer::take_overlay_pick_pending(vctx)
                    {
                        // A click inside THIS first-level editor: the pin's
                        // path is the editor's node on the tab canvas.
                        crate::canvas::viewer::put_overlay_pick_result(
                            vctx, vec![node_id.0], inner_uid, eid, size,
                        );
                    }
                    // Esc in an editor window cancels the pick, same as in
                    // the main window.
                    if vctx.input(|i| i.key_pressed(egui::Key::Escape)) {
                        crate::canvas::viewer::set_overlay_pick_active(vctx, false);
                    }
                }
                special_req = crate::canvas::viewer::take_special_picker_request(vctx);
                // When this editor's viewport owns the KB/M picker session,
                // the modal is drawn HERE (immediate viewports can't share the
                // main window's egui Windows). Interactions are applied after
                // the closure, where `app` is mutable again.
                if app.gamepad_nav.kbm_picker_open
                    && app.gamepad_nav.kbm_picker_viewport == Some(viewport_id)
                {
                    picker_result = app.kbm_picker_window(vctx);
                }
                crate::canvas::viewer::set_layout_mode_active(vctx, false);
            },
        );

        if close_self { open = false; }

        // A Special… button inside this editor requested the picker. The
        // request's path was built from the editor's ONE-LEVEL AutoMap frame,
        // so it starts at this editor's own node in its parent — prefix the
        // ancestor editor chain so the picker helpers can resolve the target
        // from the tab canvas at any nesting depth. The session is owned by
        // THIS viewport so the modal opens on the editor window the click
        // came from, not the main window.
        if let Some(mut req) = special_req {
            let mut full_path: Vec<usize> = Vec::new();
            let mut cur = parent_editor_idx;
            while let Some(p) = cur {
                full_path.push(app.sub_patch_editors[p].node_id.0);
                cur = app.sub_patch_editors[p].parent_editor_idx;
            }
            full_path.reverse();
            full_path.extend(req.path.iter().copied());
            req.path = full_path;
            app.open_special_picker(req, Some(viewport_id));
        }

        // Collect nested edit requests before putting inner_canvas back.
        if let Some(child_id) = inner_canvas.pending_edit_subpatch.take() {
            pending_nested.push((i, child_id));
        }
        // Sync clipboard upward only when the user actually copied inside this editor.
        // clipboard_gen is incremented by copy_selected(); comparing before/after show()
        // is exact regardless of node count, content, or number of copies in a row.
        let user_copied = inner_canvas.clipboard_gen != gen_before;
        if user_copied {
            app.sub_patch_editors[i].last_clipboard_gen = inner_canvas.clipboard_gen;
            if let Some(cb) = inner_canvas.clipboard() {
                app.app_clipboard = Some(cb);
                app.app_clipboard_from_inner = true;
            }
        }

        if save_clicked {
            let sp_opt = match parent_editor_idx {
                None    => app.tabs[active].canvas.snarl.get_node(node_id),
                Some(p) => app.sub_patch_editors[p].canvas.snarl.get_node(node_id),
            }.and_then(|n| n.subpatch.as_ref());
            if let Some(sp) = sp_opt { let _ = save_subpatch_file(sp); }
        }
        if load_clicked {
            if let Some(loaded) = load_subpatch_file() {
                let node_opt = match parent_editor_idx {
                    None    => app.tabs[active].canvas.snarl.get_node_mut(node_id),
                    Some(p) => app.sub_patch_editors[p].canvas.snarl.get_node_mut(node_id),
                };
                if let Some(node) = node_opt {
                    if node.subpatch.is_some() {
                        node.display_name = loaded.display_name.clone();
                        node.subpatch = Some(Box::new(loaded.clone()));
                        node.extra.layout_unlocked = false;
                    }
                }
                inner_canvas.snarl = *loaded.snarl;
                crate::canvas::migrate_loaded_snarl(&mut inner_canvas.snarl);
            }
        }

        // Handle pin/unpin.
        if let Some((inner_id, element_id, src_size)) = inner_canvas.pending_expose_module.take() {
            let any_existing = match parent_editor_idx {
                None    => app.tabs[active].canvas.snarl.get_node(node_id),
                Some(p) => app.sub_patch_editors[p].canvas.snarl.get_node(node_id),
            }.and_then(|n| n.subpatch.as_ref())
             .map(|sp| sp.has_module_pin_for(inner_id.0))
             .unwrap_or(false);
            let unpin = any_existing && element_id == "default";
            if unpin {
                let node_opt = match parent_editor_idx {
                    None    => app.tabs[active].canvas.snarl.get_node_mut(node_id),
                    Some(p) => app.sub_patch_editors[p].canvas.snarl.get_node_mut(node_id),
                };
                if let Some(node) = node_opt {
                    if let Some(sp) = node.subpatch.as_mut() {
                        sp.remove_module_pins_for(inner_id.0);
                    }
                    if node.subpatch.as_ref().map(|sp| sp.is_layout_empty()).unwrap_or(true) {
                        node.extra.layout_unlocked = false;
                    }
                }
            } else {
                let init_size = if src_size[0] >= 1.0 && src_size[1] >= 1.0 { src_size } else { [220.0, 100.0] };
                let next_y = match parent_editor_idx {
                    None    => app.tabs[active].canvas.snarl.get_node(node_id),
                    Some(p) => app.sub_patch_editors[p].canvas.snarl.get_node(node_id),
                }.and_then(|n| n.subpatch.as_ref())
                 .map(|sp| sp.module_pins_bottom_y())
                 .unwrap_or(0.0);
                let node_opt = match parent_editor_idx {
                    None    => app.tabs[active].canvas.snarl.get_node_mut(node_id),
                    Some(p) => app.sub_patch_editors[p].canvas.snarl.get_node_mut(node_id),
                };
                if let Some(node) = node_opt {
                    if let Some(sp) = node.subpatch.as_mut() {
                        sp.push_module_pin(ExposedModule {
                            inner_node_id: inner_id.0,
                            element_id,
                            pos: [PINNED_PAD, next_y + PINNED_PAD],
                            size: init_size,
                            text_override: None,
                            switch_override: None,
                            graph_override: None,
                            source_path: vec![],
                            iv_style_override: None,
                            menu_style_override: None,
                        });
                    }
                }
            }
        }

        app.sub_patch_editors[i].open = open;
        app.sub_patch_editors[i].canvas = inner_canvas;

        // Apply picker interactions collected inside the viewport closure.
        // Deliberately AFTER the canvas put-back above: `picker_write` fans a
        // picked pin out to every live copy of the target node, including
        // THIS editor's canvas — which until this point was mem::replace'd
        // out, so an earlier apply would write into the empty placeholder
        // and the draft would vanish when the real canvas returned.
        app.apply_kbm_picker_result(picker_result.0, picker_result.1);

        // Port sync: always against parent snarl.
        match parent_editor_idx {
            None => {
                let (editors, tabs) = (&mut app.sub_patch_editors, &mut app.tabs);
                sync_inner_canvas_ports(&mut tabs[active].canvas.snarl, node_id, &mut editors[i].canvas);
            }
            Some(p) => {
                // Split borrow: editors[p] and editors[i] are different indices.
                let (left, right) = app.sub_patch_editors.split_at_mut(p.max(i));
                let (parent_ed, child_ed) = if p < i {
                    (&mut left[p], &mut right[i - p - 1])
                } else {
                    (&mut right[p - i - 1], &mut left[i])
                };
                sync_inner_canvas_ports(&mut parent_ed.canvas.snarl, node_id, &mut child_ed.canvas);
            }
        }

        // Write-back inner snarl to parent node.
        // Gated on inner canvas mutation: if the editor didn't mutate
        // anything this frame (no user input, no structural change),
        // there's nothing new to write back — the parent's sp.snarl
        // already matches what we'd write. Skip the full snarl clone
        // entirely on idle frames. `last_inner_gen` is bumped any time
        // the inner canvas mutates (push_undo/push_snapshot/undo/redo).
        let cur_inner_gen = app.sub_patch_editors[i].canvas.mutation_gen;
        let prev_inner_gen = app.sub_patch_editors[i].last_inner_gen;
        if prev_inner_gen != Some(cur_inner_gen) {
            puffin::profile_scope!("editor_writeback");
            let inner_snarl = app.sub_patch_editors[i].canvas.snarl.clone();
            let node_opt = match parent_editor_idx {
                None    => app.tabs[active].canvas.snarl.get_node_mut(node_id),
                Some(p) => app.sub_patch_editors[p].canvas.snarl.get_node_mut(node_id),
            };
            if let Some(node) = node_opt {
                if let Some(sp) = node.subpatch.as_mut() { *sp.snarl = inner_snarl; }
            }
            app.sub_patch_editors[i].last_inner_gen = Some(cur_inner_gen);
        }

        if !open { to_close.push(i); }
    }

    // Open any nested editors collected during rendering.
    for (parent_idx, child_node_id) in pending_nested {
        let already_open = app.sub_patch_editors.iter()
            .any(|e| e.tab_idx == active && e.node_id == child_node_id && e.parent_editor_idx == Some(parent_idx));
        if !already_open {
            let inner_snarl = app.sub_patch_editors[parent_idx].canvas.snarl
                .get_node(child_node_id)
                .and_then(|n| n.subpatch.as_ref())
                .map(|sp| *sp.snarl.clone())
                .unwrap_or_else(egui_snarl::Snarl::new);
            let mut editor_canvas = Canvas::new();
            editor_canvas.snarl = inner_snarl;
            editor_canvas.is_inner = true;
            // Stable salt folded through the PARENT editor's salt + this child
            // node id, so nested editors are remembered and never collide with
            // their parent, sibling sub-patches, or any tab.
            editor_canvas.set_view_salt(
                flexinput_engine::namespaced_uid(
                    app.sub_patch_editors[parent_idx].canvas.view_salt() as usize,
                    child_node_id.0,
                ) as u64,
            );
            app.sub_patch_editors.push(SubPatchEditor {
                tab_idx: active,
                node_id: child_node_id,
                parent_editor_idx: Some(parent_idx),
                canvas: editor_canvas,
                open: true,
                last_clipboard_gen: 0,
                last_synced_parent_gen: None,
                last_inner_gen: None,
            });
        }
    }

    for i in to_close.into_iter().rev() {
        app.sub_patch_editors.remove(i);
    }
}

/// Derives the outer sub-patch node's input/output port list from inlet/outlet
/// nodes in the inner canvas. Called every frame while the editor window is open.
///
/// New inlet/outlet nodes that lack a `pin_index` param are auto-assigned the
/// next available index. Existing indices are stable — they don't reorder when
/// nodes are moved. Port names come from the node's `display_name`; types come
/// from `params["signal_type"]` set via the node's body type-selector.
pub(crate) fn sync_inner_canvas_ports(
    outer_snarl: &mut egui_snarl::Snarl<NodeData>,
    node_id: NodeId,
    inner_canvas: &mut Canvas,
) {
    // ── Migrate legacy outlets: strip the obsolete output pin (and any wires) ─
    // Older builds gave subpatch.outlet a redundant "out" output pin inside the
    // sub-patch. The current descriptor has no output — values forward straight
    // to the outer meta-module. Drop stray output pins and any wires from them
    // so loaded patches converge on the new shape.
    let stale_outlet_ids: Vec<NodeId> = inner_canvas.snarl.nodes_ids_data()
        .filter(|(_, n)| n.value.module_id == "subpatch.outlet" && !n.value.outputs.is_empty())
        .map(|(id, _)| id)
        .collect();
    for id in stale_outlet_ids {
        let stale_wires: Vec<(OutPinId, InPinId)> = inner_canvas.snarl.wires()
            .filter(|(out, _)| out.node == id)
            .collect();
        for (out, inp) in stale_wires {
            inner_canvas.snarl.disconnect(out, inp);
        }
        if let Some(node) = inner_canvas.snarl.get_node_mut(id) {
            node.outputs.clear();
        }
    }

    // ── Auto-assign pin_index to newly-added inlet / outlet nodes ─────────────

    for role in ["subpatch.inlet", "subpatch.outlet"] {
        let max_idx: usize = inner_canvas.snarl.nodes_ids_data()
            .filter(|(_, n)| n.value.module_id == role)
            .filter_map(|(_, n)| n.value.params.get("pin_index").and_then(|v| v.as_u64()))
            .max()
            .map(|m| m as usize + 1)
            .unwrap_or(0);

        let unindexed: Vec<NodeId> = inner_canvas.snarl.nodes_ids_data()
            .filter(|(_, n)| n.value.module_id == role
                && !n.value.params.contains_key("pin_index"))
            .map(|(id, _)| id)
            .collect();

        for (offset, id) in unindexed.iter().enumerate() {
            if let Some(node) = inner_canvas.snarl.get_node_mut(*id) {
                let idx = max_idx + offset;
                node.params.insert("pin_index".into(),
                    serde_json::Value::Number(idx.into()));
            }
        }
    }

    // ── Sync inlet/outlet pin descriptors to match declared type and name ─────

    for (_, node) in inner_canvas.snarl.nodes_ids_data_mut() {
        let t: SignalType = node.value.params.get("signal_type")
            .and_then(|v| serde_json::from_value(v.clone()).ok())
            .unwrap_or(SignalType::Any);
        let name = node.value.display_name.clone();
        match node.value.module_id.as_str() {
            "subpatch.inlet" => {
                if let Some(pin) = node.value.outputs.get_mut(0) {
                    pin.signal_type = t;
                    pin.name = name;
                }
            }
            "subpatch.outlet" => {
                if let Some(pin) = node.value.inputs.get_mut(0) {
                    pin.signal_type = t;
                    pin.name = name;
                }
            }
            _ => {}
        }
    }

    // ── Rebuild outer node's ports from sorted inlet / outlet lists ───────────

    let mut inlets: Vec<(usize, String, SignalType)> = inner_canvas.snarl.nodes_ids_data()
        .filter(|(_, n)| n.value.module_id == "subpatch.inlet")
        .filter_map(|(_, n)| {
            let idx = n.value.params.get("pin_index")?.as_u64()? as usize;
            let t   = n.value.params.get("signal_type")
                .and_then(|v| serde_json::from_value(v.clone()).ok())
                .unwrap_or(SignalType::Any);
            Some((idx, n.value.display_name.clone(), t))
        })
        .collect();
    inlets.sort_by_key(|(i, _, _)| *i);

    let mut outlets: Vec<(usize, String, SignalType)> = inner_canvas.snarl.nodes_ids_data()
        .filter(|(_, n)| n.value.module_id == "subpatch.outlet")
        .filter_map(|(_, n)| {
            let idx = n.value.params.get("pin_index")?.as_u64()? as usize;
            let t   = n.value.params.get("signal_type")
                .and_then(|v| serde_json::from_value(v.clone()).ok())
                .unwrap_or(SignalType::Any);
            Some((idx, n.value.display_name.clone(), t))
        })
        .collect();
    outlets.sort_by_key(|(i, _, _)| *i);

    if let Some(outer_node) = outer_snarl.get_node_mut(node_id) {
        outer_node.inputs = inlets.iter()
            .map(|(_, name, t)| PinDescriptor::new(name.as_str(), *t))
            .collect();
        outer_node.outputs = outlets.iter()
            .map(|(_, name, t)| PinDescriptor::new(name.as_str(), *t))
            .collect();
        if let Some(sp) = outer_node.subpatch.as_mut() {
            sp.pins_in = inlets.iter()
                .map(|(_, name, t)| SubPatchPin { name: name.clone(), signal_type: *t })
                .collect();
            sp.pins_out = outlets.iter()
                .map(|(_, name, t)| SubPatchPin { name: name.clone(), signal_type: *t })
                .collect();
        }
    }
}
