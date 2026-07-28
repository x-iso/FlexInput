//! Gamepad navigation + editing of the config overlay (M3.5 / M3.6).
//!
//! This is deliberately a THIN layer over the existing Easy-mode nav so the UX
//! is identical: d-pad / left-stick (and the shared RS cursor) move focus
//! between the pinned tweak-pins, then South enters the SAME widget/curve
//! editors the sub-patch canvas uses. It does that by pointing the shared nav
//! resolvers at the focused config pin via `gamepad_nav.config_nav_sel` (see
//! `nav_config_override`) and, once editing, delegating straight to
//! `nav_drive_subpatch` — so response curves get the full dot nav (highlight,
//! cursor-grab, add/delete, bias, undo), knobs/dropdowns the field editor, etc.
//!
//! Reuse requires a sub-patch `outer_id`, so only pins that live inside a
//! first-level sub-patch (`source_path == [sp]`, the Easy-mode norm) are
//! editable; top-level pins are focus-only for now.
//!
//! Output suppression here is the config overlay's own SELECTIVE engine
//! source-block, NOT `ui_nav_suppress`/`io_bypass` (which drop ALL output and
//! would also kill the passthrough).

use super::*;

impl FlexInputApp {
    /// Drive config-overlay focus + editing with the resolved nav device.
    /// Called from `run_gamepad_nav` while the overlay is visible; owns the frame.
    pub(crate) fn nav_drive_config_overlay(
        &mut self,
        ctx: &egui::Context,
        nav: &crate::gamepad_nav::NavInput,
        dt: f32,
    ) {
        use crate::gamepad_nav::{self as gn, EditLevel, NavDir};

        // The Special KB/M picker (opened from a pinned Remapper/TZ body) is modal
        // and renders in its own over-the-game viewport — drive IT, not the pin
        // nav, while it's open. run_gamepad_nav early-returns into here when the
        // overlay is visible, so this is the only place it can be driven.
        if self.gamepad_nav.kbm_picker_open {
            let step_dir = self.picker_step_dir(nav, dt);
            self.drive_kbm_picker(step_dir, nav);
            return;
        }

        let targets = crate::config_overlay::config_nav_targets(ctx);
        if targets.is_empty() {
            self.gamepad_nav.config_index = None;
            self.gamepad_nav.config_nav_sel = None;
            self.gamepad_nav.edit_level = EditLevel::Widget;
            self.gamepad_nav.repeat_dir = None;
            return;
        }
        if let Some(i) = self.gamepad_nav.config_index {
            if !targets.iter().any(|(idx, _)| *idx == i) {
                self.gamepad_nav.config_index = None;
                self.gamepad_nav.config_nav_sel = None;
                self.gamepad_nav.edit_level = EditLevel::Widget;
            }
        }

        // A physical stick that passes through (so the tweak's effect is FELT in
        // game) must NOT also drive the editor. Pick the control input from the
        // pin's passthrough set: right-stick felt → LEFT adjusts; left felt →
        // RIGHT adjusts; BOTH felt → the D-pad adjusts and neither stick is
        // repurposed. D-pad works as the discrete alternative in every case.
        use crate::app::ControlInput;
        let editing = self.gamepad_nav.edit_level != EditLevel::Widget;
        let control = if editing { self.config_control_input() } else { ControlInput::LeftStick };

        // Shared RS/gyro cursor (drives `cursor_pos`, monitor-clamped for the
        // fullscreen overlay). The shared editors read `cursor_pos` in screen
        // space — exactly the space the overlay + pin renderers publish in.
        // Freeze the RS while it's repurposed to drive the editor (or when both
        // sticks pass through) so aiming the game doesn't fling the cursor.
        let cursor_nav = match control {
            ControlInput::LeftStick => nav.clone(),
            ControlInput::RightStick | ControlInput::Dpad => {
                let mut n = nav.clone();
                n.rstick = egui::Vec2::ZERO;
                n
            }
        };
        self.config_update_cursor(ctx, &cursor_nav, dt);

        // ── Editing: delegate to the shared sub-patch nav dispatch ───────────
        // The override (config_nav_sel) makes `nav_drive_subpatch`'s resolvers
        // target the config pin, so the real curve/field/toggle drivers edit it
        // with identical UX. Its Widget-level branch is skipped here (edit_level
        // != Widget), so it never touches the sub-patch's own selection.
        if editing {
            if let Some((outer, _, _)) = self.gamepad_nav.config_nav_sel.clone() {
                let rt_rising = nav.rt > 0.5 && !self.gamepad_nav.prev_rt;
                let lt_rising = nav.lt > 0.5 && !self.gamepad_nav.prev_lt;
                let dnav = match control {
                    // LS already drives the editor's stick role.
                    ControlInput::LeftStick => nav.clone(),
                    // RS → the editor's "left stick" role; LS freed for passthrough.
                    ControlInput::RightStick => {
                        let mut n = nav.clone();
                        n.lstick = nav.rstick;
                        n.rstick = egui::Vec2::ZERO;
                        n
                    }
                    // Both sticks pass through: zero them so only the D-pad
                    // (still live via `step_dir`) adjusts the param.
                    ControlInput::Dpad => {
                        let mut n = nav.clone();
                        n.lstick = egui::Vec2::ZERO;
                        n.rstick = egui::Vec2::ZERO;
                        n
                    }
                };
                self.nav_drive_subpatch(ctx, outer, &dnav, dt, rt_rising, lt_rising);
            } else {
                self.gamepad_nav.edit_level = EditLevel::Widget;
            }
            return;
        }

        // ── Widget level: focus nav between config pins ──────────────────────
        // The cursor focuses whatever pin it hovers (pointer-style pick).
        if self.gamepad_nav.cursor_visible {
            let cp = self.gamepad_nav.cursor_pos;
            if let Some((idx, _)) = targets.iter().rev().find(|(_, r)| r.contains(cp)) {
                self.gamepad_nav.config_index = Some(*idx);
            }
        }

        // Point the shared resolvers at the focused pin (drives South-enter's
        // `nav_selected_kind`, and the editors once entered). None for top-level
        // pins — they can't drive the sub-patch-keyed editors.
        self.gamepad_nav.config_nav_sel = self.config_pin_outer_inner_elem();

        // South / RT → enter the SAME editor the sub-patch canvas would, chosen
        // by widget kind (mirrors the Widget-level enters in nav_drive_subpatch).
        if nav.is_rising("btn_south") || (nav.rt > 0.5 && !self.gamepad_nav.prev_rt) {
            if let Some((outer, _, _)) = self.gamepad_nav.config_nav_sel.clone() {
                match self.nav_selected_kind(outer) {
                    NavWidgetKind::Curve => {
                        self.gamepad_nav.edit_level = EditLevel::CurveDots;
                        self.gamepad_nav.curve_return_level = EditLevel::Widget;
                        self.gamepad_nav.curve_dot = 0;
                        self.gamepad_nav.curve_sustain_focus = false;
                        self.gamepad_nav.edit_baseline =
                            Some(Box::new(self.tabs[self.active_tab].canvas.snapshot_for_undo()));
                        ctx.request_repaint();
                        return;
                    }
                    NavWidgetKind::Value | NavWidgetKind::Dropdown | NavWidgetKind::MultiField => {
                        let kind = self.nav_selected_kind(outer);
                        self.gamepad_nav.edit_level = EditLevel::Editing;
                        self.gamepad_nav.fine_increment = false;
                        self.gamepad_nav.field_index = 0;
                        self.gamepad_nav.edit_baseline =
                            Some(Box::new(self.tabs[self.active_tab].canvas.snapshot_for_undo()));
                        if matches!(kind, NavWidgetKind::Dropdown) {
                            self.nav_set_dropdown_popup(ctx, outer, true);
                        }
                        ctx.request_repaint();
                        return;
                    }
                    NavWidgetKind::Toggle => {
                        let base = self.tabs[self.active_tab].canvas.snapshot_for_undo();
                        if self.nav_toggle_switch(outer) {
                            self.tabs[self.active_tab].canvas.commit_undo_if_changed(base);
                        }
                        ctx.request_repaint();
                        return;
                    }
                    NavWidgetKind::Remapper => {
                        // Remapper / Map Action / Combiner whole-module pin: enter
                        // scroll mode (Learn/assign a mapping live). Delegation to
                        // `nav_drive_subpatch` handles the rest via the override.
                        self.gamepad_nav.edit_level = EditLevel::RemapScroll;
                        self.gamepad_nav.edit_baseline =
                            Some(Box::new(self.tabs[self.active_tab].canvas.snapshot_for_undo()));
                        ctx.request_repaint();
                        return;
                    }
                    NavWidgetKind::TouchZones => {
                        // Touch Zones pad: enter line editing (seeds the focused
                        // line + snapshots one coalesced undo, like Easy mode).
                        self.nav_tz_enter(outer);
                        ctx.request_repaint();
                        return;
                    }
                    NavWidgetKind::TouchZoneCards => {
                        // Touch Zones mapping list: enter the zone-tab + Learn flow.
                        self.gamepad_nav.edit_level = EditLevel::TzCards;
                        self.gamepad_nav.edit_baseline =
                            Some(Box::new(self.tabs[self.active_tab].canvas.snapshot_for_undo()));
                        ctx.request_repaint();
                        return;
                    }
                    _ => {}
                }
            }
        }

        // Directional focus move (d-pad edges, else left-stick step on engage).
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
    /// rather than the main window's content rect, driving the shared cursor.
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

    /// `(outer sub-patch node, inner node, element_id)` for the gamepad-focused
    /// config pin, or `None` for a top-level pin (no sub-patch outer to drive
    /// the shared editors) or no focus.
    fn config_pin_outer_inner_elem(&self) -> Option<(egui_snarl::NodeId, egui_snarl::NodeId, String)> {
        use crate::canvas::node::LayoutItem;
        let i = self.gamepad_nav.config_index?;
        let tab = self.tabs.get(self.active_tab)?;
        let LayoutItem::Module(m) = tab.config.items.get(i)? else { return None };
        match m.source_path.as_slice() {
            [sp] => Some((
                egui_snarl::NodeId(*sp),
                egui_snarl::NodeId(m.inner_node_id),
                m.element_id.clone(),
            )),
            _ => None,
        }
    }

    /// While a Remapper / Touch Zones mapping-list pin is being card-navigated in
    /// the config overlay, `(outer, inner, mappings-scope)` so the overlay can
    /// republish the selection channels with ITS viewport pass and draw the card
    /// glow on ITS layer (the shared drawer is viewport-agnostic; the nav driver
    /// stamps the root viewport's pass, which never matches the overlay's).
    pub(crate) fn config_remap_glow(&self) -> Option<(egui_snarl::NodeId, egui_snarl::NodeId, String)> {
        use crate::gamepad_nav::EditLevel;
        if !matches!(
            self.gamepad_nav.edit_level,
            EditLevel::RemapScroll | EditLevel::RemapCard | EditLevel::TzCards
        ) {
            return None;
        }
        let (outer, inner, _) = self.gamepad_nav.config_nav_sel.as_ref()?;
        let scope = self.nav_remap_mappings_key(*outer).to_string();
        Some((*outer, *inner, scope))
    }

    /// Which physical input adjusts the currently-focused config pin, so it
    /// doesn't collide with the stick(s) passing through to be felt. Derived
    /// from the pin's resolved passthrough set (which already accounts for the
    /// live topology — see `config_route::control_input_from_pins`).
    pub(crate) fn config_control_input(&self) -> crate::app::ControlInput {
        use crate::app::ControlInput;
        let Some((outer, inner, _)) = self.gamepad_nav.config_nav_sel.as_ref() else {
            return ControlInput::LeftStick;
        };
        let snarl = &self.tabs[self.active_tab].canvas.snarl;
        match crate::app::config_passthrough_pins(snarl, &[outer.0], inner.0) {
            Some((_, pins)) => crate::app::control_input_from_pins(&pins),
            None => ControlInput::LeftStick,
        }
    }

    /// The focused value-field the config overlay should draw a glow ring on:
    /// `(inner_node_id, element_id, field_index)`, or `None` when not
    /// field-editing a config pin. Computed BEFORE the overlay viewport closure
    /// (which can't borrow `app`); the closure feeds it to `draw_config_field_glow`.
    pub(crate) fn config_field_glow_target(&self) -> Option<(usize, String, usize)> {
        if self.gamepad_nav.edit_level != crate::gamepad_nav::EditLevel::Editing {
            return None;
        }
        let (_, inner, elem) = self.gamepad_nav.config_nav_sel.as_ref()?;
        Some((inner.0, elem.clone(), self.gamepad_nav.field_index))
    }
}

/// Redraw the focused value-field's bloom ring in the config overlay's OWN
/// viewport. `nav_publish_field_hud` draws it on the main window, which never
/// reaches the overlay (a separate always-on-top window rendered after nav runs)
/// — so the "glow on specific values" was invisible over the game. The renderer
/// publishes the per-field rects in this viewport's pass, so reading them here
/// and painting on the overlay ctx lands the glow in the right spot. Free fn (not
/// a method) so it can run inside the viewport closure, like `draw_remap_card_glow`.
pub(crate) fn draw_config_field_glow(ctx: &egui::Context, target: Option<(usize, String, usize)>) {
    let Some((inner_uid, elem, field_idx)) = target else { return };
    let field_rects: Option<(u64, Vec<egui::Rect>)> =
        ctx.data(|d| d.get_temp(egui::Id::new(("gp_nav_field_rects", inner_uid, elem))));
    let Some((pass, frs)) = field_rects else { return };
    if pass != ctx.cumulative_pass_nr() {
        return;
    }
    if let Some(fr) = frs.get(field_idx) {
        let accent = ctx.style().visuals.selection.stroke.color;
        FlexInputApp::paint_field_glow_ring(ctx, *fr, accent);
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
