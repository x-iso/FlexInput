//! Gamepad navigation + value-editing of the config overlay (M3.5 / M3.6).
//!
//! Focus: d-pad / left-stick move focus between the pinned tweak-pins, and the
//! right stick drives a virtual cursor that also focuses whatever pin it hovers.
//! The focused pin's upstream physical device passes through to the game (the
//! overlay resolves passthrough from `gamepad_nav.config_index`), so you feel and
//! steer the parameter while adjusting it.
//!
//! South acts on the focused pin: a Switch toggles, a Dropdown cycles, a Knob
//! enters value-edit (stick/d-pad adjust), and a Response Curve enters curve-edit
//! (d-pad selects a control point, left-stick moves it) — reusing the curve
//! renderer's published geometry (`gp_nav_curve_geom`) and selection-highlight
//! channel (`gp_nav_curve_sel`). East exits an edit.
//!
//! Output suppression here is the config overlay's own SELECTIVE engine
//! source-block, NOT `ui_nav_suppress`/`io_bypass` (which drop ALL output and
//! would also kill the passthrough).

use super::*;

impl FlexInputApp {
    /// Drive config-overlay focus + value-edit with the resolved nav device.
    /// Called from `run_gamepad_nav` while the overlay is visible; owns the frame.
    pub(crate) fn nav_drive_config_overlay(
        &mut self,
        ctx: &egui::Context,
        nav: &crate::gamepad_nav::NavInput,
        dt: f32,
    ) {
        use crate::gamepad_nav::{self as gn, NavDir};

        let targets = crate::config_overlay::config_nav_targets(ctx);
        if targets.is_empty() {
            self.gamepad_nav.config_index = None;
            self.gamepad_nav.config_editing = false;
            self.gamepad_nav.repeat_dir = None;
            return;
        }
        if let Some(i) = self.gamepad_nav.config_index {
            if !targets.iter().any(|(idx, _)| *idx == i) {
                self.gamepad_nav.config_index = None;
                self.gamepad_nav.config_editing = false;
            }
        }

        // ── Right-stick virtual cursor ───────────────────────────────────────
        self.config_update_cursor(ctx, nav, dt);
        // While NOT editing, the cursor focuses whatever pin it hovers (a
        // pointer-style alternative to d-pad stepping).
        if !self.gamepad_nav.config_editing && self.gamepad_nav.cursor_visible {
            let cp = self.gamepad_nav.cursor_pos;
            if let Some((idx, _)) = targets.iter().rev().find(|(_, r)| r.contains(cp)) {
                self.gamepad_nav.config_index = Some(*idx);
            }
        }

        let focused = self.config_focused_pin();

        // ── Editing modes ────────────────────────────────────────────────────
        if self.gamepad_nav.config_editing {
            if let Some((m, e, sp, inner)) = focused {
                if e == "curve" && is_curve_module(&m) {
                    self.nav_config_curve_edit(ctx, nav, dt, &sp, inner);
                    return;
                }
                // Knob value-edit: stick / d-pad adjust; East|South exit.
                if nav.is_rising("btn_east") || nav.is_rising("btn_south") {
                    self.gamepad_nav.config_editing = false;
                    ctx.request_repaint();
                    return;
                }
                let fine = nav.pressed.contains("btn_west");
                let coarse = if fine { 0.01 } else { 0.05 };
                let mut delta = 0.0f32;
                if nav.lstick.x.abs() > 0.2 {
                    delta += nav.lstick.x * (if fine { 0.005 } else { 0.02 });
                }
                if nav.is_rising("dpad_right") { delta += coarse; }
                if nav.is_rising("dpad_left") { delta -= coarse; }
                if delta != 0.0 {
                    if let Some(node) = self.config_pin_node_mut(&sp, inner) {
                        nav_config_adjust_scalar(node, &m, &e, delta);
                    }
                }
            } else {
                self.gamepad_nav.config_editing = false;
            }
            ctx.request_repaint();
            return;
        }

        // ── Nav: South acts on the focused pin ───────────────────────────────
        if nav.is_rising("btn_south") {
            if let Some((m, e, sp, inner)) = focused.clone() {
                if (m == "module.knob" && e == "value") || (e == "curve" && is_curve_module(&m)) {
                    self.gamepad_nav.config_editing = true;
                    self.gamepad_nav.config_curve_dot = 0;
                    ctx.request_repaint();
                    return;
                }
                if let Some(node) = self.config_pin_node_mut(&sp, inner) {
                    if nav_config_activate(node, &m, &e) {
                        ctx.request_repaint();
                        return;
                    }
                }
            }
        }

        // ── Nav: move focus (d-pad edges, else left-stick step on engage) ────
        let mut dir: Option<NavDir> = None;
        if nav.is_rising("dpad_up") {
            dir = Some(NavDir::Up);
        } else if nav.is_rising("dpad_down") {
            dir = Some(NavDir::Down);
        } else if nav.is_rising("dpad_left") {
            dir = Some(NavDir::Left);
        } else if nav.is_rising("dpad_right") {
            dir = Some(NavDir::Right);
        }
        if dir.is_none() {
            match gn::stick_dir(nav.lstick) {
                Some(d) => {
                    if self.gamepad_nav.repeat_dir != Some(d) {
                        self.gamepad_nav.repeat_dir = Some(d);
                        dir = Some(d);
                    }
                }
                None => self.gamepad_nav.repeat_dir = None,
            }
        }
        if let Some(dir) = dir {
            let cur_pos = self
                .gamepad_nav
                .config_index
                .and_then(|item| targets.iter().position(|(idx, _)| *idx == item));
            let rects: Vec<egui::Rect> = targets.iter().map(|(_, r)| *r).collect();
            if let Some(next_pos) = nearest_rect_in_dir(&rects, cur_pos, dir) {
                self.gamepad_nav.config_index = Some(targets[next_pos].0);
            }
            ctx.request_repaint();
        }
    }

    /// Right-stick virtual cursor, clamped to the monitor (the overlay covers it
    /// at origin). Mirrors `update_nav_cursor` but bounds to the whole screen
    /// rather than the main window's content rect.
    fn config_update_cursor(&mut self, ctx: &egui::Context, nav: &crate::gamepad_nav::NavInput, dt: f32) {
        let mon = ctx
            .input(|i| i.viewport().monitor_size)
            .filter(|s| s.x > 1.0 && s.y > 1.0)
            .unwrap_or(egui::vec2(1920.0, 1080.0));
        let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, mon);
        let rs_max = self.settings.cursor_max_speed;
        let rs_exp = self.settings.cursor_accel;
        let gn = &mut self.gamepad_nav;
        if !gn.cursor_visible {
            gn.cursor_pos = screen.center();
        }
        let rs = nav.rstick;
        let mag = rs.length();
        if mag > 0.08 {
            let speed = rs_max * mag.clamp(0.0, 1.0).powf(rs_exp);
            let dir = rs / mag;
            gn.cursor_pos += egui::vec2(dir.x, -dir.y) * speed * dt; // stick +y = up
            gn.cursor_visible = true;
            gn.cursor_last_move = std::time::Instant::now();
        }
        if gn.cursor_visible {
            gn.cursor_pos.x = gn.cursor_pos.x.clamp(screen.min.x, screen.max.x);
            gn.cursor_pos.y = gn.cursor_pos.y.clamp(screen.min.y, screen.max.y);
            if gn.cursor_last_move.elapsed().as_secs_f32() > 3.0 {
                gn.cursor_visible = false;
            }
        }
    }

    /// Curve-edit a pinned response curve: d-pad L/R select a control point,
    /// left-stick moves it (in graph units, read from the renderer's published
    /// geometry); East exits. Publishes the selection so the curve highlights it.
    fn nav_config_curve_edit(
        &mut self,
        ctx: &egui::Context,
        nav: &crate::gamepad_nav::NavInput,
        dt: f32,
        source_path: &[usize],
        inner: usize,
    ) {
        if nav.is_rising("btn_east") {
            self.gamepad_nav.config_editing = false;
            ctx.request_repaint();
            return;
        }
        // Graph rect + axis bounds published by the curve renderer last frame.
        let geom: Option<(u64, egui::Rect, f32, f32, f32, f32)> =
            ctx.data(|d| d.get_temp(egui::Id::new(("gp_nav_curve_geom", inner))));
        let Some((_, _rect, x_lo, x_hi, y_lo, y_hi)) = geom else {
            ctx.request_repaint();
            return;
        };
        let mut pts = self.config_curve_points(source_path, inner);
        if pts.len() < 2 {
            ctx.request_repaint();
            return;
        }
        // Select a dot.
        let mut sel = self.gamepad_nav.config_curve_dot.min(pts.len() - 1);
        if nav.is_rising("dpad_right") && sel + 1 < pts.len() { sel += 1; }
        if nav.is_rising("dpad_left") && sel > 0 { sel -= 1; }
        self.gamepad_nav.config_curve_dot = sel;

        // Move the selected dot with the left stick, in graph units. Endpoints
        // keep their fixed X (only Y moves); interior points move in both axes,
        // clamped to stay between their neighbours (monotonic X).
        let fine = nav.pressed.contains("btn_west");
        let rate = if fine { 0.25 } else { 0.9 }; // fraction of range / sec
        let (dx, dy) = (nav.lstick.x, nav.lstick.y);
        if dx.abs() > 0.15 || dy.abs() > 0.15 {
            let is_endpoint = sel == 0 || sel + 1 == pts.len();
            let mut p = pts[sel];
            if !is_endpoint && dx.abs() > 0.15 {
                p[0] = (p[0] + dx * (x_hi - x_lo) * rate * dt).clamp(x_lo, x_hi);
                let lo_n = pts[sel - 1][0];
                let hi_n = pts[sel + 1][0];
                p[0] = p[0].clamp(lo_n, hi_n);
            }
            if dy.abs() > 0.15 {
                p[1] = (p[1] + dy * (y_hi - y_lo) * rate * dt).clamp(y_lo, y_hi);
            }
            pts[sel] = p;
            self.config_curve_write_points(source_path, inner, &pts);
        }

        // Publish the selection so the curve renderer highlights the dot THIS
        // frame (the overlay renders after this driver in `update`).
        let pass = ctx.cumulative_pass_nr();
        ctx.data_mut(|d| d.insert_temp(egui::Id::new(("gp_nav_curve_sel", inner)), (pass, sel, true)));
        ctx.request_repaint();
    }

    /// Read a curve pin's `points` param as `Vec<[f32; 2]>`.
    fn config_curve_points(&self, source_path: &[usize], inner: usize) -> Vec<[f32; 2]> {
        self.config_pin_node(source_path, inner)
            .and_then(|n| n.params.get("points"))
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|p| {
                        let a = p.as_array()?;
                        Some([a.first()?.as_f64()? as f32, a.get(1)?.as_f64()? as f32])
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Write a curve pin's `points` (same length — only moves existing dots, so
    /// the segment `biases` stay valid and need no resync).
    fn config_curve_write_points(&mut self, source_path: &[usize], inner: usize, pts: &[[f32; 2]]) {
        if let Some(node) = self.config_pin_node_mut(source_path, inner) {
            let arr: Vec<serde_json::Value> = pts
                .iter()
                .map(|p| serde_json::json!([p[0] as f64, p[1] as f64]))
                .collect();
            node.params.insert("points".to_string(), serde_json::Value::Array(arr));
        }
    }

    /// Identity of the gamepad-focused config pin:
    /// `(module_id, element_id, source_path, inner_node_id)`.
    fn config_focused_pin(&self) -> Option<(String, String, Vec<usize>, usize)> {
        use crate::canvas::node::LayoutItem;
        let i = self.gamepad_nav.config_index?;
        let tab = self.tabs.get(self.active_tab)?;
        let LayoutItem::Module(m) = tab.config.items.get(i)? else { return None };
        let node = crate::canvas::overlay_body::resolve_overlay_module(
            &tab.canvas.snarl,
            &m.source_path,
            m.inner_node_id,
        )?;
        Some((node.module_id.clone(), m.element_id.clone(), m.source_path.clone(), m.inner_node_id))
    }

    /// Immutable access to a config pin's inner node.
    fn config_pin_node(&self, source_path: &[usize], inner_node_id: usize) -> Option<&crate::canvas::NodeData> {
        crate::canvas::overlay_body::resolve_overlay_module(
            &self.tabs.get(self.active_tab)?.canvas.snarl,
            source_path,
            inner_node_id,
        )
    }

    /// Mutable access to a config pin's inner node (tab canvas or first-level
    /// sub-patch).
    fn config_pin_node_mut(
        &mut self,
        source_path: &[usize],
        inner_node_id: usize,
    ) -> Option<&mut crate::canvas::NodeData> {
        let snarl = &mut self.tabs[self.active_tab].canvas.snarl;
        match source_path {
            [] => snarl.get_node_mut(egui_snarl::NodeId(inner_node_id)),
            [sp] => snarl
                .get_node_mut(egui_snarl::NodeId(*sp))
                .and_then(|n| n.subpatch.as_mut())
                .and_then(|s| s.snarl.get_node_mut(egui_snarl::NodeId(inner_node_id))),
            _ => None,
        }
    }
}

/// The response-curve module family whose `curve` element is gamepad-editable.
fn is_curve_module(module_id: &str) -> bool {
    matches!(
        module_id,
        "module.response_curve" | "module.vec_response_curve" | "module.twoway_response_curve"
    )
}

/// Nudge a scalar pin's value by `delta` (normalized units) — the Knob's `value`.
fn nav_config_adjust_scalar(
    node: &mut crate::canvas::NodeData,
    module_id: &str,
    element_id: &str,
    delta: f32,
) {
    if (module_id, element_id) == ("module.knob", "value") {
        let bipolar = node.params.get("bipolar").and_then(|v| v.as_bool()).unwrap_or(false);
        let (lo, hi) = if bipolar { (-1.0f32, 1.0f32) } else { (0.0f32, 1.0f32) };
        let v = node.params.get("value").and_then(|v| v.as_f64()).unwrap_or(0.0) as f32;
        let nv = (v + delta * (hi - lo)).clamp(lo, hi);
        if let Some(n) = serde_json::Number::from_f64(nv as f64) {
            node.params.insert("value".to_string(), serde_json::Value::Number(n));
        }
    }
}

/// Act on a pin with South (no edit mode): toggle a Switch, cycle a Dropdown.
/// Returns true if the press was consumed.
fn nav_config_activate(node: &mut crate::canvas::NodeData, module_id: &str, element_id: &str) -> bool {
    match (module_id, element_id) {
        ("module.switch", "toggle") => {
            let active = crate::canvas::viewer::read_switch_active(node);
            node.params.insert("active".to_string(), serde_json::Value::Bool(!active));
            true
        }
        ("module.dropdown", "selection") => {
            let options = crate::canvas::viewer::dropdown_read_options(node);
            if options.is_empty() {
                return false;
            }
            let cur = crate::canvas::viewer::dropdown_read_selected(node, options.len());
            crate::canvas::viewer::dropdown_write_selected(node, (cur + 1) % options.len());
            true
        }
        _ => false,
    }
}

/// Nearest rect in a direction from the current one (top-left-most when none).
/// A bare-rect mirror of `gamepad_nav::nearest_target_rect_in_dir`.
fn nearest_rect_in_dir(
    rects: &[egui::Rect],
    cur: Option<usize>,
    dir: crate::gamepad_nav::NavDir,
) -> Option<usize> {
    use crate::gamepad_nav::NavDir;
    if rects.is_empty() {
        return None;
    }
    let cur = match cur.filter(|&i| i < rects.len()) {
        Some(i) => i,
        None => {
            return rects
                .iter()
                .enumerate()
                .min_by(|(_, a), (_, b)| {
                    let (ca, cb) = (a.center(), b.center());
                    (ca.y, ca.x)
                        .partial_cmp(&(cb.y, cb.x))
                        .unwrap_or(std::cmp::Ordering::Equal)
                })
                .map(|(i, _)| i);
        }
    };
    let c = rects[cur].center();
    let mut best: Option<(usize, f32)> = None;
    for (i, r) in rects.iter().enumerate() {
        if i == cur {
            continue;
        }
        let o = r.center();
        let (dx, dy) = (o.x - c.x, o.y - c.y);
        let (primary, cross) = match dir {
            NavDir::Right => (dx, dy),
            NavDir::Left => (-dx, dy),
            NavDir::Down => (dy, dx),
            NavDir::Up => (-dy, dx),
        };
        if primary <= 0.0 {
            continue;
        }
        let score = primary + 2.0 * cross.abs();
        if best.map(|(_, s)| score < s).unwrap_or(true) {
            best = Some((i, score));
        }
    }
    best.map(|(i, _)| i)
}
