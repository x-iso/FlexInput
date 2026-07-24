//! Gamepad navigation + value-editing of the config overlay (M3.5 / M3.6).
//!
//! Nav level: d-pad / left-stick move focus between the pinned tweak-pins; the
//! focused pin's upstream physical device passes through to the game (the
//! overlay resolves passthrough from `gamepad_nav.config_index`), so you feel
//! and steer the parameter while adjusting it. South acts on the focused pin:
//! a Switch toggles, a Dropdown cycles, a Knob enters value-edit (stick/d-pad
//! adjust, East exits). Other pin types (response curves…) stay mouse-edited
//! for now — the passthrough still lets you feel them.
//!
//! Unlike the main-UI nav areas, output suppression here is the config overlay's
//! own SELECTIVE engine source-block, NOT `ui_nav_suppress`/`io_bypass` (which
//! drop ALL output and would also kill the passthrough).

use super::*;

impl FlexInputApp {
    /// Drive config-overlay focus + value-edit with the resolved nav device.
    /// Called from `run_gamepad_nav` while the overlay is visible; owns the frame.
    pub(crate) fn nav_drive_config_overlay(
        &mut self,
        ctx: &egui::Context,
        nav: &crate::gamepad_nav::NavInput,
    ) {
        use crate::gamepad_nav::{self as gn, NavDir};

        // Navigable pins published by the overlay this frame: (item_index, rect).
        let targets = crate::config_overlay::config_nav_targets(ctx);
        if targets.is_empty() {
            self.gamepad_nav.config_index = None;
            self.gamepad_nav.config_editing = false;
            self.gamepad_nav.repeat_dir = None;
            return;
        }
        // Drop focus/edit if the focused item is no longer a published target.
        if let Some(i) = self.gamepad_nav.config_index {
            if !targets.iter().any(|(idx, _)| *idx == i) {
                self.gamepad_nav.config_index = None;
                self.gamepad_nav.config_editing = false;
            }
        }
        let focused = self.config_focused_pin();

        // ── Value-edit a scalar (Knob): stick / d-pad adjust; East|South exit ──
        if self.gamepad_nav.config_editing {
            if nav.is_rising("btn_east") || nav.is_rising("btn_south") {
                self.gamepad_nav.config_editing = false;
                ctx.request_repaint();
                return;
            }
            if let Some((m, e, sp, inner)) = focused {
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
            }
            ctx.request_repaint();
            return;
        }

        // ── Nav: South acts on the focused pin ───────────────────────────────
        if nav.is_rising("btn_south") {
            if let Some((m, e, sp, inner)) = focused.clone() {
                if m == "module.knob" && e == "value" {
                    self.gamepad_nav.config_editing = true;
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

/// Nudge a scalar pin's value by `delta` (in normalized units). Currently the
/// Knob's `value` (clamped to its bipolar/unipolar range).
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
fn nav_config_activate(
    node: &mut crate::canvas::NodeData,
    module_id: &str,
    element_id: &str,
) -> bool {
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
            let next = (cur + 1) % options.len();
            crate::canvas::viewer::dropdown_write_selected(node, next);
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
