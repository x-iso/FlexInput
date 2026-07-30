//! Gamepad-nav driving of Touch Zones: divider line editing, zone cycling,
//! and the per-zone action cards.

use super::*;

/// Pick the nearest candidate point strictly in screen direction `dir` from
/// `cur`, using the same primary-axis + cross-penalty scorer the picker/left-panel
/// nav use. Returns the index into `cands`. Screen-space, so radial angular wrap
/// is handled naturally (a sector across 12 o'clock is just screen-adjacent).
fn nav_tz_nearest_in_dir(cur: egui::Pos2, dir: crate::gamepad_nav::NavDir,
    cands: &[(usize, egui::Pos2)]) -> Option<usize>
{
    use crate::gamepad_nav::NavDir;
    let mut best: Option<(usize, f32)> = None;
    for &(idx, o) in cands {
        let (dx, dy) = (o.x - cur.x, o.y - cur.y);
        let (primary, cross) = match dir {
            NavDir::Right => (dx, dy),
            NavDir::Left => (-dx, dy),
            NavDir::Down => (dy, dx),
            NavDir::Up => (-dy, dx),
        };
        if primary <= 0.5 { continue; }
        let score = primary + 2.0 * cross.abs();
        if best.map(|(_, s)| score < s).unwrap_or(true) { best = Some((idx, score)); }
    }
    best.map(|(i, _)| i)
}

impl FlexInputApp {

    /// Enter line editing from Widget level: focus the first interior divider of
    /// pad 0 and snapshot for one coalesced undo entry across the whole edit.
    pub(crate) fn nav_tz_enter(&mut self, outer_id: egui_snarl::NodeId) {
        use crate::gamepad_nav::EditLevel;
        let Some(inner) = self.nav_selected_inner_node(outer_id) else { return; };
        self.gamepad_nav.tz_field = 0;
        let (cols, rows) = if self.tz_is_mapping(outer_id, inner) {
            (self.tz_tree_axis_len(outer_id, inner, 0, 0),
             self.tz_tree_axis_len(outer_id, inner, 0, 1))
        } else {
            (self.tz_edges(outer_id, inner, 0, "col_edges").len(),
             self.tz_edges(outer_id, inner, 0, "row_edges").len())
        };
        if cols > 0 { self.gamepad_nav.tz_axis = 0; }
        else if rows > 0 { self.gamepad_nav.tz_axis = 1; }
        else { self.gamepad_nav.tz_axis = 0; }
        self.gamepad_nav.tz_line = 0;
        // The spatial walk starts on the current zone (so the cards widget's
        // context is set from the moment you enter the field).
        let sel_zone = self.tabs[self.active_tab].canvas.snarl.get_node(outer_id)
            .and_then(|n| n.subpatch.as_ref()).and_then(|sp| sp.snarl.get_node(inner))
            .and_then(|n| n.params.get("sel_zone").and_then(|v| v.as_u64()))
            .unwrap_or(0) as usize;
        self.gamepad_nav.tz_focus = crate::gamepad_nav::TzFocus::Zone(sel_zone);
        self.gamepad_nav.edit_level = EditLevel::TzLines;
        self.gamepad_nav.edit_baseline = Some(Box::new(
            self.tabs[self.active_tab].canvas.snapshot_for_undo()));
    }

    /// Exit line editing to Widget level, committing the coalesced undo entry.
    pub(crate) fn nav_tz_exit(&mut self) {
        self.gamepad_nav.edit_level = crate::gamepad_nav::EditLevel::Widget;
        if let Some(baseline) = self.gamepad_nav.edit_baseline.take() {
            self.tabs[self.active_tab].canvas.commit_undo_if_changed(*baseline);
        }
    }

    /// Publish the focused/grabbed divider so the pinned field renderer can
    /// highlight it (keyed by inner node id; consumed by `tz_draw_field`).
    pub(crate) fn nav_tz_publish(&self, ctx: &egui::Context, inner: egui_snarl::NodeId) {
        use crate::gamepad_nav::TzFocus;
        let grabbed = matches!(self.gamepad_nav.edit_level, crate::gamepad_nav::EditLevel::TzGrab);
        let pass = ctx.cumulative_pass_nr();
        // A border highlights only while a BORDER is focused; when a zone/seam is
        // focused we publish an out-of-range (axis,line) so no divider matches.
        let (b_axis, b_line) = match self.gamepad_nav.tz_focus {
            TzFocus::Border => (self.gamepad_nav.tz_axis as u64, self.gamepad_nav.tz_line as u64),
            _ => (u64::MAX, u64::MAX),
        };
        let zone = match self.gamepad_nav.tz_focus {
            TzFocus::Zone(z) => z as u64,
            _ => u64::MAX,
        };
        let seam_focus = matches!(self.gamepad_nav.tz_focus, TzFocus::Seam);
        ctx.data_mut(|d| {
            d.insert_temp(
                egui::Id::new(("gp_nav_tz", inner.0)),
                (pass, self.gamepad_nav.tz_field as u64, b_axis, b_line, grabbed));
            // Focused-zone highlight channel (usize::MAX as u64 = none). Read by
            // both field renderers, gated via the viewport-agnostic nav pass.
            d.insert_temp(
                egui::Id::new(("gp_nav_tz_zone", inner.0)),
                (pass, self.gamepad_nav.tz_field as u64, zone));
            // Radial "root border" (origin seam) focus highlight.
            d.insert_temp(
                egui::Id::new(("gp_nav_tz_seam", inner.0)),
                (pass, seam_focus));
        });
        ctx.request_repaint();
    }

    /// Read the field's gamepad nav-walk targets (global space), published by the
    /// field renderer: `(kind, a, b, center)` where kind 0 = zone (a = zone id),
    /// 1 = border (a = axis, b = per-axis line), 2 = seam. Stamped with the
    /// viewport-agnostic nav pass so this matches even when the field renders in
    /// the config overlay's own viewport.
    pub(crate) fn nav_tz_targets(&self, ctx: &egui::Context, inner: egui_snarl::NodeId, field: usize)
        -> Vec<(u8, u32, u32, egui::Pos2)>
    {
        ctx.data(|d| d.get_temp::<(u64, Vec<(u8, u32, u32, egui::Pos2)>)>(
                egui::Id::new(("gp_nav_tz_targets", inner.0, field))))
            .filter(|(p, _)| crate::widgets::nav_pass_matches(ctx, *p))
            .map(|(_, v)| v)
            .unwrap_or_default()
    }

    /// The screen point of the CURRENT focus, looked up in this frame's published
    /// targets (geometry can shift between frames, and the focused divider/zone may
    /// have been removed — `None` then, so the walk re-seeds).
    fn nav_tz_focus_point(&self, targets: &[(u8, u32, u32, egui::Pos2)]) -> Option<egui::Pos2> {
        use crate::gamepad_nav::TzFocus;
        match self.gamepad_nav.tz_focus {
            TzFocus::Zone(z) => targets.iter()
                .find(|t| t.0 == 0 && t.1 == z as u32).map(|t| t.3),
            TzFocus::Seam => targets.iter().find(|t| t.0 == 2).map(|t| t.3),
            TzFocus::Border => targets.iter().find(|t| t.0 == 1
                && t.1 == self.gamepad_nav.tz_axis as u32
                && t.2 == self.gamepad_nav.tz_line as u32).map(|t| t.3),
        }
    }

    /// Apply a focused nav target to the focus model (and, for a zone, the
    /// mapping-card context `sel_zone`/`sel_field`). `field` is the pad the
    /// target belongs to.
    fn nav_tz_focus_target(&mut self, outer: egui_snarl::NodeId, inner: egui_snarl::NodeId,
        field: usize, t: (u8, u32, u32, egui::Pos2))
    {
        use crate::gamepad_nav::TzFocus;
        self.gamepad_nav.tz_field = field;
        match t.0 {
            0 => {
                self.gamepad_nav.tz_focus = TzFocus::Zone(t.1 as usize);
                // Focusing a zone re-targets the separate cards widget live.
                if let Some(sp) = self.tabs[self.active_tab].canvas.snarl
                    .get_node_mut(outer).and_then(|n| n.subpatch.as_mut())
                {
                    if let Some(n) = sp.snarl.get_node_mut(inner) {
                        n.params.insert("sel_field".into(), serde_json::Value::from(field as u64));
                        n.params.insert("sel_zone".into(), serde_json::Value::from(t.1 as u64));
                    }
                }
            }
            2 => { self.gamepad_nav.tz_focus = TzFocus::Seam; }
            _ => {
                self.gamepad_nav.tz_focus = TzFocus::Border;
                self.gamepad_nav.tz_axis = t.1 as u8;
                self.gamepad_nav.tz_line = t.2 as usize;
            }
        }
    }

    /// Rotate a radial menu's origin seam by `delta` fraction (wraps 0..1).
    pub(crate) fn nav_menu_rotate_origin(&mut self, outer: egui_snarl::NodeId,
        inner: egui_snarl::NodeId, delta: f32)
    {
        let Some(sp) = self.tabs[self.active_tab].canvas.snarl
            .get_node_mut(outer).and_then(|n| n.subpatch.as_mut()) else { return; };
        let Some(n) = sp.snarl.get_node_mut(inner) else { return; };
        let cur = n.params.get("menu_radial_origin").and_then(|v| v.as_f64()).unwrap_or(0.0) as f32;
        n.params.insert("menu_radial_origin".into(),
            serde_json::json!((cur + delta).rem_euclid(1.0) as f64));
    }

    /// Drive Touch Zones line editing (`TzLines`/`TzGrab`). See the EditLevel
    /// docs for the full control map.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn nav_drive_touch_zones(
        &mut self,
        ctx: &egui::Context,
        outer_id: egui_snarl::NodeId,
        nav: &crate::gamepad_nav::NavInput,
        _dt: f32,
        step_dir: Option<crate::gamepad_nav::NavDir>,
        rt_rising: bool,
        lt_rising: bool,
        _mag: f32,
    ) {
        use crate::gamepad_nav::{EditLevel, NavDir};
        let Some(inner) = self.nav_selected_inner_node(outer_id) else {
            self.nav_tz_exit();
            return;
        };
        let mapping = self.tz_is_mapping(outer_id, inner);
        let n_fields = self.tz_n_fields(outer_id, inner);
        let mut field = self.gamepad_nav.tz_field.min(n_fields - 1);

        // LB/RB switch the focused pad (split mode only).
        if n_fields > 1 && (nav.is_rising("btn_lb") || nav.is_rising("btn_rb")) {
            field = (field + 1) % n_fields;
        }
        self.gamepad_nav.tz_field = field; // keep state reclamped to valid pads

        // Line counts + edits differ by mode: mapping mode drives the BSP tree by
        // divider PATH; ports mode drives the grid edge arrays (move-only).
        let cols = if mapping { Vec::new() } else { self.tz_edges(outer_id, inner, field, "col_edges") };
        let rows = if mapping { Vec::new() } else { self.tz_edges(outer_id, inner, field, "row_edges") };
        let n_col = if mapping { self.tz_tree_axis_len(outer_id, inner, field, 0) } else { cols.len() };
        let n_row = if mapping { self.tz_tree_axis_len(outer_id, inner, field, 1) } else { rows.len() };
        // Clamp the focused line to the current axis's count (edits may shrink it).
        let axis_len = |a: u8| if a == 0 { n_col } else { n_row };
        if axis_len(self.gamepad_nav.tz_axis) == 0 {
            // Current axis emptied: jump to the other if it has lines.
            let other = 1 - self.gamepad_nav.tz_axis;
            if axis_len(other) > 0 { self.gamepad_nav.tz_axis = other; }
        }
        let cur_len = axis_len(self.gamepad_nav.tz_axis).max(1);
        self.gamepad_nav.tz_line = self.gamepad_nav.tz_line.min(cur_len - 1);

        // Recenter the focused divider (tree path in mapping mode, grid edge in ports).
        let recenter = |s: &mut Self, field: usize| {
            if mapping {
                if let Some((_, lo, hi, path)) = s.tz_tree_sel(outer_id, inner, field) {
                    s.tz_tree_set_t(outer_id, inner, field, &path, (lo + hi) * 0.5);
                }
            } else {
                s.nav_tz_recenter(outer_id, inner, field, &cols, &rows);
            }
        };

        match self.gamepad_nav.edit_level {
            EditLevel::TzLines => {
                if nav.is_rising("btn_east") { self.nav_tz_exit(); return; }
                use crate::gamepad_nav::TzFocus;

                // Screen-space nav targets for the focused pad (kind 0 = zone,
                // 1 = border, 2 = seam). The spatial walk and cursor hover both
                // pick from these, so grid + radial share one geometry path.
                let targets = self.nav_tz_targets(ctx, inner, field);

                // RS/gyro cursor hover-select: the visual pointer picks a target
                // directly — a border/seam within reach wins, else the nearest
                // zone centroid. (Zone rects aren't published, so nearest-centroid
                // is the approximation; borders keep their tolerance.)
                if self.gamepad_nav.cursor_visible && !targets.is_empty() {
                    let cur = self.gamepad_nav.cursor_pos;
                    let near_border = targets.iter()
                        .filter(|t| t.0 != 0)
                        .map(|t| (*t, (t.3 - cur).length()))
                        .filter(|(_, d)| *d <= 16.0)
                        .min_by(|a, b| a.1.total_cmp(&b.1))
                        .map(|(t, _)| t);
                    let pick = near_border.or_else(|| targets.iter()
                        .filter(|t| t.0 == 0)
                        .map(|t| (*t, (t.3 - cur).length()))
                        .min_by(|a, b| a.1.total_cmp(&b.1))
                        .map(|(t, _)| t));
                    if let Some(t) = pick {
                        self.nav_tz_focus_target(outer_id, inner, field, t);
                    }
                }

                // Spatial-alternating walk: from a ZONE, a dpad/LS direction picks
                // the nearest BORDER/SEAM in that screen direction; from a
                // BORDER/SEAM it picks the nearest ZONE. Because a border sits
                // between adjacent zones, this yields zone↔border↔zone everywhere.
                if let Some(dir) = step_dir {
                    let on_zone = matches!(self.gamepad_nav.tz_focus, TzFocus::Zone(_));
                    let cur_pt = self.nav_tz_focus_point(&targets);
                    if let Some(cur) = cur_pt {
                        let cands: Vec<(usize, egui::Pos2)> = targets.iter().enumerate()
                            .filter(|(_, t)| if on_zone { t.0 != 0 } else { t.0 == 0 })
                            .map(|(i, t)| (i, t.3))
                            .collect();
                        if let Some(ti) = nav_tz_nearest_in_dir(cur, dir, &cands) {
                            self.nav_tz_focus_target(outer_id, inner, field, targets[ti]);
                        }
                    } else if let Some(&t) = targets.first() {
                        // Focus was orphaned (target vanished) — re-seed.
                        self.nav_tz_focus_target(outer_id, inner, field, t);
                    }
                }

                // Actions depend on what's focused.
                match self.gamepad_nav.tz_focus {
                    TzFocus::Border => {
                        let has_line = axis_len(self.gamepad_nav.tz_axis) > 0;
                        if nav.is_rising("btn_south") && has_line {
                            self.gamepad_nav.edit_level = EditLevel::TzGrab;
                        }
                        if nav.is_rising("btn_north") && has_line {
                            recenter(self, field);
                        }
                        if mapping && nav.is_rising("btn_west") && has_line {
                            if let Some((_, _, _, path)) = self.tz_tree_sel(outer_id, inner, field) {
                                self.tz_tree_remove(outer_id, inner, field, &path);
                            }
                        }
                    }
                    TzFocus::Seam => {
                        // South grabs the seam to rotate the whole ring.
                        if nav.is_rising("btn_south") {
                            self.gamepad_nav.edit_level = EditLevel::TzGrab;
                        }
                    }
                    TzFocus::Zone(_) => {
                        // RT/LT divide the FOCUSED zone via the BSP tree in BOTH modes
                        // (V = radius line / vertical = "straight from centre";
                        // H = ring arc / horizontal = "along the circle"). Ports mode
                        // used to do a full grid cut; now it splits just this zone and
                        // its typed ports are rebuilt with wiring preserved.
                        if rt_rising || lt_rising {
                            use flexinput_core::touchzones::Axis;
                            let axis = if rt_rising { Axis::V } else { Axis::H };
                            self.tz_tree_add(outer_id, inner, field, axis);
                        }
                    }
                }
            }
            EditLevel::TzGrab => {
                if nav.is_rising("btn_south") || nav.is_rising("btn_east") {
                    self.gamepad_nav.edit_level = EditLevel::TzLines;
                } else if matches!(self.gamepad_nav.tz_focus, crate::gamepad_nav::TzFocus::Seam) {
                    // Seam grabbed → dpad/LS rotates the ring's origin.
                    const RSTEP: f32 = 0.02;
                    let delta = match step_dir {
                        Some(NavDir::Left) | Some(NavDir::Down) => -RSTEP,
                        Some(NavDir::Right) | Some(NavDir::Up) => RSTEP,
                        None => 0.0,
                    };
                    if delta != 0.0 {
                        self.nav_menu_rotate_origin(outer_id, inner, delta);
                    }
                } else if nav.is_rising("btn_north") {
                    recenter(self, field);
                } else {
                    // dpad/LS along the line's perpendicular axis nudges it.
                    let axis = self.gamepad_nav.tz_axis;
                    const STEP: f32 = 0.02;
                    let delta = match (axis, step_dir) {
                        (0, Some(NavDir::Left))  => -STEP,
                        (0, Some(NavDir::Right)) =>  STEP,
                        (1, Some(NavDir::Up))    => -STEP,
                        (1, Some(NavDir::Down))  =>  STEP,
                        _ => 0.0,
                    };
                    if delta != 0.0 {
                        if mapping {
                            if let Some((pos, lo, hi, path)) = self.tz_tree_sel(outer_id, inner, field) {
                                self.tz_tree_set_t(outer_id, inner, field, &path, (pos + delta).clamp(lo, hi));
                            }
                        } else {
                            self.nav_tz_move(outer_id, inner, field, delta, &cols, &rows);
                        }
                    }
                }
            }
            _ => {}
        }
        self.nav_tz_publish(ctx, inner);
    }

    /// Nudge the focused divider by `delta` (clamped between its neighbours,
    /// matching the mouse-drag bounds).
    pub(crate) fn nav_tz_move(&mut self, outer: egui_snarl::NodeId, inner: egui_snarl::NodeId,
        field: usize, delta: f32, cols: &[f32], rows: &[f32])
    {
        let axis = self.gamepad_nav.tz_axis;
        let line = self.gamepad_nav.tz_line;
        let (which, edges) = if axis == 0 { ("col_edges", cols) } else { ("row_edges", rows) };
        if line >= edges.len() { return; }
        let mut e = edges.to_vec();
        let lo = if line == 0 { 0.05 } else { e[line - 1] + 0.04 };
        let hi = if line + 1 == e.len() { 0.95 } else { e[line + 1] - 0.04 };
        // Crowded neighbours can invert lo/hi (would panic in clamp).
        e[line] = if lo <= hi { (e[line] + delta).clamp(lo, hi) } else { (lo + hi) * 0.5 };
        self.tz_set_edges(outer, inner, field, which, &e);
    }

    /// Recenter the focused divider between its two adjacent borders (mirrors the
    /// mouse double-click / "North" recenter action).
    pub(crate) fn nav_tz_recenter(&mut self, outer: egui_snarl::NodeId, inner: egui_snarl::NodeId,
        field: usize, cols: &[f32], rows: &[f32])
    {
        let axis = self.gamepad_nav.tz_axis;
        let line = self.gamepad_nav.tz_line;
        let (which, edges) = if axis == 0 { ("col_edges", cols) } else { ("row_edges", rows) };
        if line >= edges.len() { return; }
        let mut e = edges.to_vec();
        let lo = if line == 0 { 0.0 } else { e[line - 1] };
        let hi = if line + 1 == e.len() { 1.0 } else { e[line + 1] };
        e[line] = (lo + hi) * 0.5;
        self.tz_set_edges(outer, inner, field, which, &e);
    }


    /// Cycle the SELECTED zone of the Touch Zones cards widget by `dir`, wrapping
    /// within the current pad's zone count.
    pub(crate) fn nav_tz_cycle_zone(&mut self, outer: egui_snarl::NodeId, inner: egui_snarl::NodeId, dir: i32) {
        let node = self.tabs[self.active_tab].canvas.snarl.get_node(outer)
            .and_then(|n| n.subpatch.as_ref()).and_then(|sp| sp.snarl.get_node(inner));
        let field = node.and_then(|n| n.params.get("sel_field").and_then(|v| v.as_u64())).unwrap_or(0) as usize;
        let zone = node.and_then(|n| n.params.get("sel_zone").and_then(|v| v.as_u64())).unwrap_or(0) as usize;
        let col = self.tz_edges(outer, inner, field, "col_edges");
        let row = self.tz_edges(outer, inner, field, "row_edges");
        let zn = flexinput_core::touchzones::zone_count(&col, &row).max(1) as i32;
        let new = (zone as i32 + dir).rem_euclid(zn) as u64;
        if let Some(sp) = self.tabs[self.active_tab].canvas.snarl.get_node_mut(outer).and_then(|n| n.subpatch.as_mut()) {
            if let Some(n) = sp.snarl.get_node_mut(inner) {
                n.params.insert("sel_zone".into(), serde_json::Value::from(new));
            }
        }
    }

    /// The action-row items for the Touch Zones cards widget, in the SAME visual
    /// order the body publishes their rects (`render_touch_zone_cards`, Row 2):
    /// idle → [Learn]; learning → [Cancel]; captured → [Assign, 🎮, ＋Add, Cancel].
    /// When any card drives a relative-mouse output the node-global mouse-speed
    /// control is appended LAST (matching the right-aligned DragValue), navigable
    /// like a button and nudged with LT/RT. Keeping this identical to the body
    /// keeps the nav glow on the right item.
    pub(crate) fn nav_tz_action_items(phase: &str, has_analog: bool, show_mouse_speed: bool, menu: bool) -> Vec<&'static str> {
        // A Virtual Menu zone is triggered by its own selection (no touch/swipe),
        // so it skips the touch-"learning" phase: idle offers Learn (arm a
        // gamepad-button capture) AND Assign (pick a specific output) directly,
        // matching the mouse menu card. Touch Zones demonstrates a gesture first.
        let mut v = match (menu, phase) {
            (true, "idle")  => vec!["learn", "assign"],
            (false, "idle") => vec!["learn"],
            (_, "learning") => vec!["cancel"],
            _               => vec!["assign", "gamepad", "add", "cancel"], // captured
        };
        // Order MUST match the body's act_rects push order: tp_mode, mouse_speed,
        // then hold LAST. tp_mode is a VALUE cycled with LT/RT (like mouse_speed),
        // not a South-activated button. `show_mouse_speed` mirrors the body's
        // `show_speed` gate (a mouse card, or a touchpad-mode stick zone).
        if has_analog { v.push("tp_mode"); }
        if show_mouse_speed { v.push("mouse_speed"); }
        // Hold-zone is a Touch Zones concept; the menu card body omits the Hold
        // checkbox, so the action row must not reserve a slot for it.
        if !menu { v.push("hold"); }
        v
    }

    /// Whether the SELECTED zone is in Touchpad mode (its first card's `tp_mode`).
    /// Used with `has_analog`/`has_mouse` to mirror the body's mouse-speed gate.
    pub(crate) fn nav_tz_selected_zone_touchpad(&self, outer: egui_snarl::NodeId, inner: egui_snarl::NodeId) -> bool {
        let node = self.tabs[self.active_tab].canvas.snarl.get_node(outer)
            .and_then(|n| n.subpatch.as_ref()).and_then(|sp| sp.snarl.get_node(inner));
        let Some(node) = node else { return false; };
        let sel_f = node.params.get("sel_field").and_then(|v| v.as_u64()).unwrap_or(0);
        let sel_z = node.params.get("sel_zone").and_then(|v| v.as_u64()).unwrap_or(0);
        node.params.get("zone_maps").and_then(|v| v.as_array())
            .and_then(|cards| cards.iter()
                .filter(|c| c.get("f").and_then(|v| v.as_u64()).unwrap_or(0) == sel_f
                         && c.get("z").and_then(|v| v.as_u64()).unwrap_or(0) == sel_z)
                .find_map(|c| c.get("tp_mode").and_then(|v| v.as_str())))
            .map(|m| m == "touchpad").unwrap_or(false)
    }

    /// Mirrors the body's `show_speed`: a mouse card, OR a touchpad-mode stick zone.
    pub(crate) fn nav_tz_shows_mouse_speed(&self, outer: egui_snarl::NodeId, inner: egui_snarl::NodeId) -> bool {
        self.nav_tz_has_mouse_card(outer, inner)
            || (self.nav_tz_has_analog_card(outer, inner)
                && self.nav_tz_selected_zone_touchpad(outer, inner))
    }

    /// Toggle the SELECTED zone's "hold" flag in the `hold_zones` param (mirrors
    /// the header checkbox), for the gamepad-nav path.
    pub(crate) fn nav_tz_toggle_hold(&mut self, outer: egui_snarl::NodeId, inner: egui_snarl::NodeId) {
        let (field, zone) = self.tabs[self.active_tab].canvas.snarl.get_node(outer)
            .and_then(|n| n.subpatch.as_ref()).and_then(|sp| sp.snarl.get_node(inner))
            .map(|n| (
                n.params.get("sel_field").and_then(|v| v.as_u64()).unwrap_or(0) as usize,
                n.params.get("sel_zone").and_then(|v| v.as_u64()).unwrap_or(0) as usize))
            .unwrap_or((0, 0));
        let Some(sp) = self.tabs[self.active_tab].canvas.snarl.get_node_mut(outer)
            .and_then(|n| n.subpatch.as_mut()) else { return; };
        let held = crate::canvas::viewer::tz_zone_held(&sp.snarl, inner, field, zone);
        crate::canvas::viewer::tz_set_zone_held(&mut sp.snarl, inner, field, zone, !held);
    }

    /// Whether any card in the node's `zone_maps` drives a relative-mouse output
    /// (gates the mouse-speed nav item, mirroring the body's `has_mouse_card`).
    pub(crate) fn nav_tz_has_mouse_card(&self, outer: egui_snarl::NodeId, inner: egui_snarl::NodeId) -> bool {
        self.tabs[self.active_tab].canvas.snarl.get_node(outer)
            .and_then(|n| n.subpatch.as_ref()).and_then(|sp| sp.snarl.get_node(inner))
            .and_then(|n| n.params.get("zone_maps").and_then(|v| v.as_array()))
            .map(|cards| cards.iter().any(|c| c.get("out").and_then(|o| o.as_array())
                .map(|a| a.iter().any(|p| matches!(p.as_str(),
                    Some("mouse") | Some("mouse_x") | Some("mouse_y"))))
                .unwrap_or(false)))
            .unwrap_or(false)
    }

    /// Nudge the SELECTED zone's `mouse_speed` (per-zone, stored on the zone's cards
    /// like the body's DragValue), clamped to the UI range.
    pub(crate) fn nav_tz_nudge_mouse_speed(&mut self, outer: egui_snarl::NodeId,
        inner: egui_snarl::NodeId, delta: f32)
    {
        let Some(sp) = self.tabs[self.active_tab].canvas.snarl.get_node_mut(outer)
            .and_then(|n| n.subpatch.as_mut()) else { return; };
        let Some(n) = sp.snarl.get_node_mut(inner) else { return; };
        let sel_f = n.params.get("sel_field").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
        let sel_z = n.params.get("sel_zone").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
        let in_zone = |c: &serde_json::Value| -> bool {
            c.get("f").and_then(|v| v.as_u64()).unwrap_or(0) as usize == sel_f
                && c.get("z").and_then(|v| v.as_u64()).unwrap_or(0) as usize == sel_z
        };
        let cur = n.params.get("zone_maps").and_then(|v| v.as_array())
            .and_then(|cards| cards.iter().filter(|c| in_zone(c))
                .find_map(|c| c.get("mouse_speed").and_then(|v| v.as_f64())))
            .or_else(|| n.params.get("mouse_speed").and_then(|v| v.as_f64()))
            .unwrap_or(1.0) as f32;
        let next = (cur + delta).clamp(0.1, 10.0);
        if let Some(cards) = n.params.get_mut("zone_maps").and_then(|v| v.as_array_mut()) {
            for c in cards.iter_mut() {
                if in_zone(c) {
                    if let Some(o) = c.as_object_mut() {
                        o.insert("mouse_speed".into(), serde_json::Value::from(next as f64));
                    }
                }
            }
        }
    }

    /// Nudge the entered card's per-card `adaptive` (Rel. center) value 0..1 — the
    /// gamepad path for the "Rel. center" slider (RemapCard field 7).
    pub(crate) fn nav_tz_nudge_adaptive(&mut self, outer: egui_snarl::NodeId, idx: usize, delta: f32) {
        self.nav_remap_card_obj_mut(outer, idx, |m| {
            let cur = m.get("adaptive").and_then(|v| v.as_f64()).unwrap_or(0.30) as f32;
            let next = (cur + delta).clamp(0.0, 1.0);
            m.insert("adaptive".to_string(), serde_json::json!(next as f64));
            true
        });
    }

    /// Whether the card at absolute `zone_maps` index `idx` shows its own "Rel.
    /// center" (adaptive) slider — mirrors the body's `show_adaptive` gating so the
    /// gamepad field list matches what's on screen. Swipe cards show it unless the
    /// zone is in Touchpad mode; analog cards show it per mode (synced → only the
    /// zone's TOP analog card; percard → all; touchpad → none).
    pub(crate) fn nav_tz_card_shows_adaptive(&self, outer: egui_snarl::NodeId,
        inner: egui_snarl::NodeId, idx: usize) -> bool
    {
        let node = self.tabs[self.active_tab].canvas.snarl.get_node(outer)
            .and_then(|n| n.subpatch.as_ref()).and_then(|sp| sp.snarl.get_node(inner));
        let Some(node) = node else { return false; };
        let cards = node.params.get("zone_maps").and_then(|v| v.as_array());
        let Some(cards) = cards else { return false; };
        let Some(card) = cards.get(idx) else { return false; };
        let f = card.get("f").and_then(|v| v.as_u64()).unwrap_or(0);
        let z = card.get("z").and_then(|v| v.as_u64()).unwrap_or(0);
        let is_swipe = card.get("in").and_then(|v| v.as_array())
            .and_then(|a| a.first()).and_then(|v| v.as_str())
            .map(|t| t.starts_with("tz_swipe")).unwrap_or(false);
        let analog = card.get("out").and_then(|v| v.as_array())
            .map(|a| a.iter().filter_map(|v| v.as_str())
                .any(crate::canvas::viewer::tz_out_pin_is_analog)).unwrap_or(false);
        if !is_swipe && !analog { return false; }
        let in_zone = |c: &&serde_json::Value| -> bool {
            c.get("f").and_then(|v| v.as_u64()).unwrap_or(0) == f
                && c.get("z").and_then(|v| v.as_u64()).unwrap_or(0) == z
        };
        let tp_mode = cards.iter().filter(in_zone)
            .find_map(|c| c.get("tp_mode").and_then(|v| v.as_str()).map(String::from))
            .unwrap_or_else(|| "synced".into());
        if is_swipe { return tp_mode != "touchpad"; }
        // The zone's top analog card is the first in-zone card driving an analog out.
        let top_analog_idx = cards.iter().enumerate().find(|(_, c)| in_zone(c)
            && c.get("out").and_then(|v| v.as_array())
                .map(|a| a.iter().filter_map(|v| v.as_str())
                    .any(crate::canvas::viewer::tz_out_pin_is_analog)).unwrap_or(false))
            .map(|(i, _)| i);
        match tp_mode.as_str() {
            "touchpad" => false,
            "synced"   => top_analog_idx == Some(idx),
            _          => true, // percard
        }
    }

    /// Cycle the SELECTED ZONE's "Touchpad mode" (synced → percard → touchpad → …)
    /// by `dir` (+1 / −1), for the gamepad-nav path (mirrors the body ComboBox).
    /// Stored per-zone on that zone's cards.
    pub(crate) fn nav_tz_cycle_mode(&mut self, outer: egui_snarl::NodeId,
        inner: egui_snarl::NodeId, dir: i32)
    {
        let Some(sp) = self.tabs[self.active_tab].canvas.snarl.get_node_mut(outer)
            .and_then(|n| n.subpatch.as_mut()) else { return; };
        let Some(n) = sp.snarl.get_node_mut(inner) else { return; };
        let sel_f = n.params.get("sel_field").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
        let sel_z = n.params.get("sel_zone").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
        const MODES: [&str; 3] = ["synced", "percard", "touchpad"];
        let in_zone = |c: &serde_json::Value| -> bool {
            c.get("f").and_then(|v| v.as_u64()).unwrap_or(0) as usize == sel_f
                && c.get("z").and_then(|v| v.as_u64()).unwrap_or(0) as usize == sel_z
        };
        let cur = n.params.get("zone_maps").and_then(|v| v.as_array())
            .and_then(|cards| cards.iter().filter(|c| in_zone(c))
                .find_map(|c| c.get("tp_mode").and_then(|v| v.as_str()).map(String::from)))
            .unwrap_or_else(|| "synced".into());
        let i = MODES.iter().position(|m| *m == cur).unwrap_or(0) as i32;
        let next = MODES[(((i + dir) % 3 + 3) % 3) as usize];
        if let Some(cards) = n.params.get_mut("zone_maps").and_then(|v| v.as_array_mut()) {
            for c in cards.iter_mut() {
                if in_zone(c) {
                    if let Some(o) = c.as_object_mut() {
                        o.insert("tp_mode".into(), serde_json::Value::from(next));
                    }
                }
            }
        }
    }

    /// Whether any card drives an analog output (stick/mouse/scroll) — gates the
    /// tp_mode nav item, mirroring the body's `has_analog_card`.
    pub(crate) fn nav_tz_has_analog_card(&self, outer: egui_snarl::NodeId, inner: egui_snarl::NodeId) -> bool {
        self.tabs[self.active_tab].canvas.snarl.get_node(outer)
            .and_then(|n| n.subpatch.as_ref()).and_then(|sp| sp.snarl.get_node(inner))
            .and_then(|n| n.params.get("zone_maps").and_then(|v| v.as_array()))
            .map(|cards| cards.iter().any(|c| c.get("out").and_then(|o| o.as_array())
                .map(|a| a.iter().any(|p| p.as_str()
                    .map(crate::canvas::viewer::tz_out_pin_is_analog).unwrap_or(false)))
                .unwrap_or(false)))
            .unwrap_or(false)
    }

    /// Full-array indices into the node's `zone_maps` that belong to the SELECTED
    /// zone (`sel_field`/`sel_zone`) — i.e. the cards the body actually shows for
    /// this zone, in display order. These absolute indices are what the shared
    /// card renderer glows (`mapping_idx`) and what delete/enter operate on.
    pub(crate) fn nav_tz_zone_card_indices(&self, outer: egui_snarl::NodeId, inner: egui_snarl::NodeId)
        -> Vec<usize>
    {
        let node = self.tabs[self.active_tab].canvas.snarl.get_node(outer)
            .and_then(|n| n.subpatch.as_ref()).and_then(|sp| sp.snarl.get_node(inner));
        let Some(node) = node else { return Vec::new(); };
        let field = node.params.get("sel_field").and_then(|v| v.as_u64()).unwrap_or(0);
        let zone = node.params.get("sel_zone").and_then(|v| v.as_u64()).unwrap_or(0);
        node.params.get("zone_maps").and_then(|v| v.as_array()).map(|arr| {
            arr.iter().enumerate().filter_map(|(i, c)| {
                let cf = c.get("f").and_then(|v| v.as_u64()).unwrap_or(0);
                let cz = c.get("z").and_then(|v| v.as_u64()).unwrap_or(0);
                (cf == field && cz == zone).then_some(i)
            }).collect()
        }).unwrap_or_default()
    }

    /// Publish the TzCards selection so the shared Remapper glow overlay rings the
    /// focused action button OR card. Card selection is published as the ABSOLUTE
    /// `zone_maps` index (what the card renderer tags each card with); action
    /// selection as the action-row index. Scope is `zone_maps`, matching the body.
    pub(crate) fn nav_tz_publish_selection(&self, ctx: &egui::Context,
        outer: egui_snarl::NodeId, inner: egui_snarl::NodeId, phase: &str)
    {
        let has_analog = self.nav_tz_has_analog_card(outer, inner);
        let show_speed = self.nav_tz_shows_mouse_speed(outer, inner);
        let menu = self.nav_selected_module_id(outer).as_deref() == Some("module.menu");
        let n_actions = Self::nav_tz_action_items(phase, has_analog, show_speed, menu).len();
        let card_idxs = self.nav_tz_zone_card_indices(outer, inner);
        let sel = self.gamepad_nav.card_index;
        let entered = matches!(self.gamepad_nav.edit_level,
            crate::gamepad_nav::EditLevel::RemapCard);
        let pass = ctx.cumulative_pass_nr();
        let scope = self.nav_remap_mappings_key(outer); // "zone_maps"
        let (act_sel, card_sel) = if entered {
            (usize::MAX, self.gamepad_nav.remap_card)
        } else if sel < n_actions {
            (sel, usize::MAX)
        } else {
            (usize::MAX, card_idxs.get(sel - n_actions).copied().unwrap_or(usize::MAX))
        };
        ctx.data_mut(|d| {
            d.insert_temp(egui::Id::new(("gp_nav_remap_card", inner.0, scope)),
                (pass, card_sel, entered));
            d.insert_temp(egui::Id::new(("gp_nav_remap_action", inner.0, scope)),
                (pass, act_sel));
        });
        ctx.request_repaint();
    }

    /// Drive the Touch Zones mapping CARDS widget (`TzCards`). Adapts the
    /// Remapper's proven two-row model (`nav_drive_remapper`): the ACTION row
    /// (Learn / Assign / 🎮 / ＋Add / Cancel) is navigated LEFT/RIGHT; the card
    /// list below it UP/DOWN. Down from the actions lands on the first card, Up
    /// from the first card returns to the actions. South activates a focused
    /// action or ENTERS a focused card for field editing (shared `RemapCard`
    /// driver); West deletes a focused card; LB/RB cycle the selected zone; East
    /// exits. While gamepad-learn is armed the driver goes INERT so the pressed
    /// button reaches the body's capture machine (which ignores the still-held
    /// arming button via a baseline).
    pub(crate) fn nav_drive_tz_cards(
        &mut self,
        ctx: &egui::Context,
        outer_id: egui_snarl::NodeId,
        nav: &crate::gamepad_nav::NavInput,
        step_dir: Option<crate::gamepad_nav::NavDir>,
        rt_rising: bool,
        lt_rising: bool,
    ) {
        use crate::gamepad_nav::{EditLevel, NavDir};
        let Some(inner) = self.nav_selected_inner_node(outer_id) else {
            self.gamepad_nav.edit_level = EditLevel::Widget;
            return;
        };
        let phase = self.picker_target_param_str(&[outer_id.0], inner, "_tz_phase")
            .unwrap_or_else(|| "idle".into());
        let has_analog = self.nav_tz_has_analog_card(outer_id, inner);
        let show_speed = self.nav_tz_shows_mouse_speed(outer_id, inner);
        let menu = self.nav_selected_module_id(outer_id).as_deref() == Some("module.menu");

        // INERT while gamepad-learn is armed: the raw button must reach the body's
        // capture (baseline-ignore there stops the arming button self-capturing).
        // Keep publishing the selection so the glow persists.
        if self.get_subpatch_param_bool(outer_id, inner, "_tz_gp_arm").unwrap_or(false) {
            self.nav_tz_publish_selection(ctx, outer_id, inner, &phase);
            ctx.request_repaint();
            return;
        }

        // East exits the widget.
        if nav.is_rising("btn_east") {
            self.gamepad_nav.edit_level = EditLevel::Widget;
            ctx.request_repaint();
            return;
        }

        // LB/RB cycle the selected zone (only while idle — a capture freezes it).
        // Reset the cursor to the action row so the new zone's card list is
        // approached from the top.
        if phase == "idle" {
            let zdir = if nav.is_rising("btn_rb") { 1 }
                       else if nav.is_rising("btn_lb") { -1 } else { 0 };
            if zdir != 0 {
                self.nav_tz_cycle_zone(outer_id, inner, zdir);
                self.gamepad_nav.card_index = 0;
            }
        }

        // Two-row cursor: [0..n_actions) = action buttons, [n_actions..total) =
        // the selected zone's cards.
        let actions = Self::nav_tz_action_items(&phase, has_analog, show_speed, menu);
        let n_actions = actions.len();
        let card_idxs = self.nav_tz_zone_card_indices(outer_id, inner);
        let count = card_idxs.len();
        // The response curve, threshold and Rel.-center are reached by ENTERING a
        // card (South) and cycling its fields — exactly like the Remapper — so there
        // is no separate curve row here.
        let total = n_actions + count;
        let _ = (rt_rising, lt_rising); // no LT/RT actions at this level anymore
        if total == 0 {
            self.nav_tz_publish_selection(ctx, outer_id, inner, &phase);
            ctx.request_repaint();
            return;
        }
        if self.gamepad_nav.card_index >= total { self.gamepad_nav.card_index = total - 1; }

        let cur = self.gamepad_nav.card_index;
        let on_actions = cur < n_actions;
        // Mode / mouse-speed are VALUE items in the action row: focus them with
        // left/right like the buttons, then change with up/down (dpad or left
        // stick) — the same select-then-U/D pattern as the entered-card fields.
        let cur_is_value = on_actions && matches!(actions[cur], "tp_mode" | "mouse_speed");
        if cur_is_value {
            let d = match step_dir { Some(NavDir::Up) => 1i32, Some(NavDir::Down) => -1, _ => 0 };
            if d != 0 {
                match actions[cur] {
                    "tp_mode" => self.nav_tz_cycle_mode(outer_id, inner, d),
                    "mouse_speed" => self.nav_tz_nudge_mouse_speed(outer_id, inner, d as f32 * 0.1),
                    _ => {}
                }
            }
        }
        let new_cur = match step_dir {
            Some(NavDir::Left) if on_actions => cur.saturating_sub(1),
            Some(NavDir::Right) if on_actions => (cur + 1).min(n_actions.saturating_sub(1)),
            // Down leaves the action row for the cards — but on a value item it
            // edits the value instead of transitioning (stay put).
            Some(NavDir::Down) if on_actions => if cur_is_value { cur } else { n_actions.min(total - 1) },
            Some(NavDir::Up) if on_actions => cur,
            Some(NavDir::Down) => (cur + 1).min(total - 1),
            Some(NavDir::Up) => if cur == n_actions { 0 } else { cur - 1 },
            _ => cur,
        };
        self.gamepad_nav.card_index = new_cur;
        let sel = new_cur;

        if sel < n_actions {
            // An action button is focused → South activates it (value items are
            // changed with up/down above, so South does nothing on them).
            if nav.is_rising("btn_south") {
                match actions[sel] {
                    "learn" => {
                        if menu {
                            // Menu: the trigger is the zone's own selection, so
                            // arm a gamepad-button capture of the OUTPUT (mirrors
                            // the mouse menu "Learn"). No touch demonstration. The
                            // driver goes INERT while `_tz_gp_arm` is set so the
                            // next button press reaches the body's capture; clear
                            // the stale baseline/seen so it re-captures fresh.
                            self.set_subpatch_param_str(outer_id, inner, "_tz_phase", "captured");
                            self.set_subpatch_param_str(outer_id, inner, "_tz_trig", "menu_sel");
                            self.set_subpatch_param_bool(outer_id, inner, "_tz_gp_arm", true);
                            self.set_subpatch_param_str_array(outer_id, inner, "_tz_draft_out", &[]);
                            self.remove_subpatch_param(outer_id, inner, "_tz_gp_base");
                            self.remove_subpatch_param(outer_id, inner, "_tz_gp_seen");
                        } else {
                            self.set_subpatch_param_str(outer_id, inner, "_tz_phase", "learning");
                            self.set_subpatch_param_str(outer_id, inner, "_tz_trig", "");
                        }
                        self.gamepad_nav.card_index = 0; // action list changed
                    }
                    "cancel" => {
                        self.set_subpatch_param_str(outer_id, inner, "_tz_phase", "idle");
                        self.set_subpatch_param_str_array(outer_id, inner, "_tz_draft_out", &[]);
                        self.set_subpatch_param_bool(outer_id, inner, "_tz_gp_arm", false);
                        self.gamepad_nav.card_index = 0;
                    }
                    "assign" => {
                        // Menu idle→Assign commits the captured phase with the
                        // menu-select trigger first (the mouse menu "Assign…"
                        // flow), so picking outputs binds them to the zone's
                        // selection. Touch Zones is already in `captured` here.
                        if menu {
                            self.set_subpatch_param_str(outer_id, inner, "_tz_phase", "captured");
                            self.set_subpatch_param_str(outer_id, inner, "_tz_trig", "menu_sel");
                            self.set_subpatch_param_str_array(outer_id, inner, "_tz_draft_out", &[]);
                        }
                        // Exclude the menu's OWN output pins so a zone can't map to
                        // itself (mirrors the mouse menu path's `menu_excl`).
                        let exclude_pin_prefix = if menu {
                            self.get_subpatch_param_str(outer_id, inner, "menu_id")
                                .map(|id| format!("menu:{id}"))
                        } else {
                            None
                        };
                        // Gamepad-navigable KB/M picker (routed over the game when
                        // the config overlay is up — see the central promotion in
                        // update()).
                        self.open_special_picker(crate::canvas::viewer::SpecialPickerRequest {
                            inner,
                            path: vec![outer_id.0],
                            draft_key: "_tz_draft_out".to_string(),
                            phase_key: None,
                            touch_zones: true,
                            exclude_pin_prefix,
                        }, None);
                    }
                    "gamepad" => {
                        let armed = self.get_subpatch_param_bool(outer_id, inner, "_tz_gp_arm").unwrap_or(false);
                        self.set_subpatch_param_bool(outer_id, inner, "_tz_gp_arm", !armed);
                    }
                    "add" => {
                        // Commit only when there's a draft (mirrors the ＋Add button's
                        // enable rule); the body consumes `_tz_commit_add`.
                        if !self.nav_remap_draft_vec(outer_id, inner, "_tz_draft_out").is_empty() {
                            self.set_subpatch_param_bool(outer_id, inner, "_tz_commit_add", true);
                        }
                    }
                    "hold" => self.nav_tz_toggle_hold(outer_id, inner),
                    _ => {}
                }
            }
        } else {
            // A card is focused → West deletes, South ENTERS it for field editing
            // (shared RemapCard driver, returning to TzCards on exit).
            let card_idx = card_idxs[sel - n_actions];
            if nav.is_rising("btn_west") {
                let base = self.tabs[self.active_tab].canvas.snapshot_for_undo();
                if self.nav_remap_delete_card(outer_id, card_idx) {
                    self.tabs[self.active_tab].canvas.commit_undo_if_changed(base);
                }
            } else if nav.is_rising("btn_south") {
                self.gamepad_nav.edit_level = EditLevel::RemapCard;
                self.gamepad_nav.card_return_level = EditLevel::TzCards;
                self.gamepad_nav.remap_card = card_idx;
                self.gamepad_nav.card_field = 0;
                self.gamepad_nav.edit_baseline = Some(Box::new(
                    self.tabs[self.active_tab].canvas.snapshot_for_undo()));
            }
        }

        self.nav_tz_publish_selection(ctx, outer_id, inner, &phase);
        ctx.request_repaint();
    }
}
