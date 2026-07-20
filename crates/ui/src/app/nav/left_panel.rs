//! Gamepad-nav driving of the Easy-mode left panel: device selection,
//! output sinks, and slider editing.

use super::*;

impl FlexInputApp {

    /// Cursor-driven navigation of the left I/O panel. Returns true when it
    /// consumed this frame's input (so sub-patch nav is skipped).
    ///
    /// - Not editing: if the cursor is visible and South/RT rises over a target,
    ///   dispatch it — select input device, toggle output sink, toggle the
    ///   digital-triggers checkbox, or (for sliders) ENTER a left-edit.
    /// - Editing a slider: stick/dpad nudges the value, West toggles fine,
    ///   North resets to default, East/LT exits (committing one undo entry).
    pub(crate) fn nav_drive_left_panel(
        &mut self,
        ctx: &egui::Context,
        nav: &crate::gamepad_nav::NavInput,
        dt: f32,
        rt_rising: bool,
        lt_rising: bool,
    ) -> bool {
        use crate::gamepad_nav::{self as gn, LeftNavAction, NavDir};

        // Published targets (this frame, from io_panel). Used for hover-glow and
        // to recover the editing target's rect.
        let targets: Vec<gn::LeftNavTarget> = ctx
            .data(|d| d.get_temp::<(u64, Vec<gn::LeftNavTarget>)>(gn::left_targets_id()))
            .map(|(_, t)| t)
            .unwrap_or_default();

        // Glow helper: edge ring + OUTWARD bloom only — never fills the widget
        // interior, so the slider/checkbox underneath stays fully visible while
        // editing. The bloom is concentric outside-strokes with falling alpha (a
        // true outward gradient). `editing` brightens + widens it.
        let glow = |rect: egui::Rect, editing: bool| {
            let accent = ctx.style().visuals.selection.stroke.color;
            let [r, g, b, _] = accent.to_array();
            let round = 10.0_f32;
            let painter = ctx.layer_painter(egui::LayerId::new(
                egui::Order::Foreground, egui::Id::new("gp_nav_left_glow")));
            // Outward bloom: a handful of expanding rings fading to transparent.
            let rings = 7;
            let max_grow = if editing { 9.0 } else { 6.0 };
            let peak = if editing { 150.0 } else { 90.0 };
            for i in 0..rings {
                let t = (i as f32 + 1.0) / rings as f32; // 0..1 outward
                let grow = t * max_grow;
                let a = (peak * (1.0 - t)).round() as u8;
                if a == 0 { continue; }
                painter.rect_stroke(
                    rect.expand(grow), round + grow,
                    egui::Stroke::new(2.0, egui::Color32::from_rgba_unmultiplied(r, g, b, a)),
                    egui::StrokeKind::Outside,
                );
            }
            // Crisp edge ring on the widget border.
            painter.rect_stroke(rect.expand(1.0), round,
                egui::Stroke::new(if editing { 2.0 } else { 1.25 }, accent),
                egui::StrokeKind::Outside);
        };

        // ── Editing a left-panel slider ──────────────────────────────────
        if let Some(action) = self.gamepad_nav.left_edit.clone() {
            let LeftNavAction::AdjustParam { node, key, lo, hi, step, default, log } = action
            else {
                // Non-adjust actions never enter edit; clear defensively.
                self.gamepad_nav.left_edit = None;
                return false;
            };

            // Exit (East / LT) → commit one undo entry if changed.
            if nav.is_rising("btn_east") || lt_rising {
                self.gamepad_nav.left_edit = None;
                if let Some(baseline) = self.gamepad_nav.edit_baseline.take() {
                    self.tabs[self.active_tab].canvas.commit_undo_if_changed(*baseline);
                }
                return true;
            }
            // West → fine increments. North → reset to default.
            if nav.is_rising("btn_west") {
                self.gamepad_nav.fine_increment = !self.gamepad_nav.fine_increment;
            }
            if nav.is_rising("btn_north") {
                self.set_node_param_f32(node, &key, default);
                return true;
            }

            // Directional delta: dpad step + left-stick continuous.
            let fine = self.gamepad_nav.fine_increment;
            let span = hi - lo;
            // Per-press step is a fraction of the range; fine = ¼ of that.
            let base_step = span * if fine { 0.01 } else { 0.04 };
            let mut delta = 0.0f32;
            let mut step_dir: Option<NavDir> = None;
            if nav.is_rising("dpad_right") || nav.is_rising("dpad_up") { step_dir = Some(NavDir::Right); }
            else if nav.is_rising("dpad_left") || nav.is_rising("dpad_down") { step_dir = Some(NavDir::Left); }
            if let Some(d) = step_dir {
                delta += if matches!(d, NavDir::Right) { base_step } else { -base_step };
            }
            let mag = nav.lstick.length();
            if mag > 0.5 {
                let sens = span * if fine { 0.15 } else { 0.6 };
                delta += nav.lstick.x * sens * dt;
            }
            if delta != 0.0 {
                let cur = self.get_node_param_f32(node, &key).unwrap_or(default);
                let next = if log {
                    // Multiplicative step in log space for log-scaled sliders.
                    let factor = 1.0 + (delta / span);
                    (cur * factor).clamp(lo, hi)
                } else {
                    (cur + delta).clamp(lo, hi)
                };
                self.set_node_param_f32(node, &key, next);
            }
            // Glow the editing target (look its rect up by node+key).
            if let Some(t) = targets.iter().find(|t| matches!(&t.action,
                LeftNavAction::AdjustParam { node: n, key: k, .. } if *n == node && *k == key))
            {
                glow(t.rect, true);
            }
            let _ = (rt_rising, step);
            return true;
        }

        // ── Not editing: RS-cursor hover + LS/D-pad selection share the panel ──
        // Panel folded away (or no targets published) → drop any stale selection
        // and let sub-patch navigation run.
        if self.easy_left_panel_collapsed || targets.is_empty() {
            self.gamepad_nav.left_selected = None;
            return false;
        }

        // RS/gyro cursor: top-most target under the cursor (glow it so the user
        // sees what South/RT will act on). `enumerate().rev()` so a later
        // (top-most) target wins overlaps; we keep the index.
        let cursor_hit: Option<usize> = if self.gamepad_nav.cursor_visible {
            let cursor = self.gamepad_nav.cursor_pos;
            targets.iter().enumerate().rev()
                .find(|(_, t)| t.rect.contains(cursor)).map(|(i, _)| i)
        } else {
            None
        };
        if let Some(ci) = cursor_hit {
            glow(targets[ci].rect, false);
        }

        // LS/D-pad selection movement — only while focus lives in the panel.
        if let Some(sel0) = self.gamepad_nav.left_selected {
            let sel = sel0.min(targets.len() - 1);
            self.gamepad_nav.left_selected = Some(sel);

            // Directional intent: D-pad discrete + left-stick auto-repeat (mirrors
            // the sub-patch Widget nav so both regions feel identical).
            let mut step_dir: Option<NavDir> = None;
            if nav.is_rising("dpad_up") { step_dir = Some(NavDir::Up); }
            else if nav.is_rising("dpad_down") { step_dir = Some(NavDir::Down); }
            else if nav.is_rising("dpad_left") { step_dir = Some(NavDir::Left); }
            else if nav.is_rising("dpad_right") { step_dir = Some(NavDir::Right); }
            let mag = nav.lstick.length();
            if let Some(sd) = gn::stick_dir(nav.lstick) {
                if self.gamepad_nav.repeat_dir != Some(sd) {
                    self.gamepad_nav.repeat_dir = Some(sd);
                    self.gamepad_nav.repeat_accum = 1.0;
                }
                let rate = 6.0 + ((mag - 0.5) / 0.5).clamp(0.0, 1.0) * 12.0;
                self.gamepad_nav.repeat_accum += dt * rate;
                if self.gamepad_nav.repeat_accum >= 1.0 {
                    self.gamepad_nav.repeat_accum -= 1.0;
                    if step_dir.is_none() { step_dir = Some(sd); }
                }
            } else {
                self.gamepad_nav.repeat_dir = None;
                self.gamepad_nav.repeat_accum = 0.0;
            }

            if let Some(dir) = step_dir {
                match gn::nearest_target_rect_in_dir(&targets, Some(sel), dir) {
                    Some(next) => self.gamepad_nav.left_selected = Some(next),
                    // Off the right edge → hand focus back to the sub-patch (which
                    // seeds its left-most widget next frame). Consume this frame.
                    None if matches!(dir, NavDir::Right) => {
                        self.gamepad_nav.left_selected = None;
                        self.gamepad_nav.repeat_dir = None;
                        self.gamepad_nav.repeat_accum = 0.0;
                        return true;
                    }
                    None => {}
                }
            }
            if let Some(s) = self.gamepad_nav.left_selected {
                if let Some(t) = targets.get(s) { glow(t.rect, false); }
            }
        }

        // Activation (South / RT): the cursor target wins when the cursor is over
        // one; otherwise the LS/D-pad selection.
        if nav.is_rising("btn_south") || rt_rising {
            if let Some(idx) = cursor_hit.or(self.gamepad_nav.left_selected) {
                if let Some(t) = targets.get(idx) {
                    let action = t.action.clone();
                    self.nav_dispatch_left_action(action);
                }
                return true;
            }
        }

        // Consume the frame when focus lives in the panel (LS owns directional
        // input); otherwise let sub-patch navigation proceed.
        self.gamepad_nav.left_selected.is_some()
    }

    /// Dispatch an activated left-panel target action (shared by the RS-cursor
    /// and LS/D-pad selection paths).
    pub(crate) fn nav_dispatch_left_action(&mut self, action: crate::gamepad_nav::LeftNavAction) {
        use crate::gamepad_nav::LeftNavAction;
        match action {
            LeftNavAction::SelectInput { device_id } => {
                self.nav_select_input_device(&device_id);
            }
            LeftNavAction::ToggleOutput { kind } => {
                self.nav_toggle_output_sink(&kind);
            }
            LeftNavAction::CycleGamepadOutput => {
                self.nav_cycle_gamepad_output();
            }
            LeftNavAction::ToggleParam { node, key } => {
                let base = self.tabs[self.active_tab].canvas.snapshot_for_undo();
                let cur = self.get_node_param_bool(node, &key).unwrap_or(false);
                self.set_node_param_bool(node, &key, !cur);
                self.tabs[self.active_tab].canvas.commit_undo_if_changed(base);
            }
            action @ LeftNavAction::AdjustParam { .. } => {
                // Enter slider edit; snapshot for a single coalesced undo entry.
                self.gamepad_nav.edit_baseline = Some(Box::new(
                    self.tabs[self.active_tab].canvas.snapshot_for_undo()));
                self.gamepad_nav.fine_increment = false;
                self.gamepad_nav.left_edit = Some(action);
            }
        }
    }

    /// Make `device_id` the active input source (mirrors the io_panel card
    /// click path: remove existing source nodes, add this device, rewire).
    pub(crate) fn nav_select_input_device(&mut self, device_id: &str) {
        let already = {
            let canvas = &self.tabs[self.active_tab].canvas;
            canvas.snarl.nodes_ids_data()
                .find(|(_, n)| n.value.module_id == "device.source")
                .and_then(|(_, n)| n.value.params.get("device_id")
                    .and_then(|v| v.as_str())) == Some(device_id)
        };
        if already { return; }
        let Some(dev) = self.devices.iter().find(|d| d.id == device_id).cloned()
        else { return; };
        let defaults = self.nav_device_defaults();
        let collapsed = self.settings.device_nodes_default_collapsed;
        let canvas = &mut self.tabs[self.active_tab].canvas;
        let to_remove: Vec<egui_snarl::NodeId> = canvas.snarl.nodes_ids_data()
            .filter(|(_, n)| n.value.module_id == "device.source")
            .map(|(id, _)| id)
            .collect();
        for id in to_remove { canvas.snarl.remove_node(id); }
        canvas.add_device_source(&dev, collapsed, defaults);
        crate::easy::layout::reposition_io_nodes(canvas);
        crate::easy::wiring::rewire(canvas);
    }

    /// Device param defaults from settings — the single construction point
    /// (Easy panel, Advanced panels, gamepad nav, and sub-patch editors all
    /// share it).
    pub(crate) fn nav_device_defaults(&self) -> crate::canvas::DeviceParamDefaults {
        crate::canvas::DeviceParamDefaults {
            stick_deadzone: self.settings.default_stick_deadzone,
            gyro_mult: self.settings.default_gyro_mult,
            mouse_sensitivity: self.settings.default_mouse_sensitivity,
            rumble_floor: self.settings.default_rumble_floor,
            rumble_max: self.settings.default_rumble_max,
            rumble_exp: self.settings.default_rumble_exp,
        }
    }

    /// Toggle a virtual output sink on/off by kind prefix (xinput/ds4/keymouse),
    /// honoring the xinput⇄ds4 mutual exclusion the io_panel enforces.
    pub(crate) fn nav_toggle_output_sink(&mut self, kind: &str) {
        use flexinput_virtual::kind_prefix;
        let canvas = &mut self.tabs[self.active_tab].canvas;
        let has = canvas.snarl.nodes_ids_data().any(|(_, n)| {
            n.value.module_id == "device.sink"
                && n.value.params.get("device_id").and_then(|v| v.as_str())
                    .map(|d| kind_prefix(d) == kind).unwrap_or(false)
        });
        let remove_kind = |canvas: &mut Canvas, k: &str| {
            let ids: Vec<egui_snarl::NodeId> = canvas.snarl.nodes_ids_data()
                .filter(|(_, n)| n.value.module_id == "device.sink"
                    && n.value.params.get("device_id").and_then(|v| v.as_str())
                        .map(|d| kind_prefix(d) == k).unwrap_or(false))
                .map(|(id, _)| id).collect();
            for id in ids { canvas.snarl.remove_node(id); }
        };
        if has {
            remove_kind(canvas, kind);
        } else {
            // The Xbox and DS4 gamepad outputs are mutually exclusive (Easy mode
            // drives a single pad). Kinds are HIDMaestro now; clear the other one.
            if kind == "virtual.hm.xinput" { remove_kind(canvas, "virtual.hm.ds4"); }
            if kind == "virtual.hm.ds4" { remove_kind(canvas, "virtual.hm.xinput"); }
            let defaults = self.nav_device_defaults();
            let collapsed = self.settings.device_nodes_default_collapsed;
            let pool = std::sync::Arc::clone(&self.shared_virtual_devices);
            let canvas = &mut self.tabs[self.active_tab].canvas;
            crate::easy::io_panel::nav_ensure_sink(
                canvas, kind, &pool, collapsed, defaults);
        }
        let canvas = &mut self.tabs[self.active_tab].canvas;
        crate::easy::wiring::rewire(canvas);
    }

    /// Cycle the single gamepad output to the next model (Xbox 360 → DS4 →
    /// DualSense → None → …). Mirrors the Easy output card's selector so the
    /// gamepad-nav cursor and the mouse agree. Deploying a model removes any
    /// other gamepad sink first (Easy mode drives one pad).
    pub(crate) fn nav_cycle_gamepad_output(&mut self) {
        let next = {
            let canvas = &self.tabs[self.active_tab].canvas;
            crate::easy::io_panel::next_gamepad_kind(canvas)
        };
        // Remove every gamepad sink, then add the next model (None → add none).
        let collapsed = self.settings.device_nodes_default_collapsed;
        let defaults = self.nav_device_defaults();
        let pool = std::sync::Arc::clone(&self.shared_virtual_devices);
        {
            let canvas = &mut self.tabs[self.active_tab].canvas;
            crate::easy::io_panel::remove_all_gamepad_sinks(canvas);
            if let Some(kind) = next {
                crate::easy::io_panel::nav_ensure_sink(canvas, kind, &pool, collapsed, defaults);
            }
        }
        let canvas = &mut self.tabs[self.active_tab].canvas;
        crate::easy::wiring::rewire(canvas);
    }
}
