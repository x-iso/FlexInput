//! Gamepad-nav driving of Touch Zones: divider line editing, zone cycling,
//! and the per-zone action cards.

use super::*;

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
        let grabbed = matches!(self.gamepad_nav.edit_level, crate::gamepad_nav::EditLevel::TzGrab);
        let pass = ctx.cumulative_pass_nr();
        ctx.data_mut(|d| d.insert_temp(
            egui::Id::new(("gp_nav_tz", inner.0)),
            (pass, self.gamepad_nav.tz_field as u64, self.gamepad_nav.tz_axis as u64,
             self.gamepad_nav.tz_line as u64, grabbed)));
        ctx.request_repaint();
    }

    /// Hit-test the RS/gyro cursor against the divider hit-rects the pinned pad
    /// published (both pads). Returns (field, axis, line) of the divider under
    /// the cursor. Rects are slightly expanded so thin lines are easy to target.
    pub(crate) fn nav_tz_cursor_hit(&self, ctx: &egui::Context, inner: egui_snarl::NodeId, n_fields: usize)
        -> Option<(usize, u8, usize)>
    {
        let cursor = self.gamepad_nav.cursor_pos;
        let cur_pass = ctx.cumulative_pass_nr();
        let mut best: Option<(f32, usize, u8, usize)> = None; // (dist, field, axis, line)
        for field in 0..n_fields {
            let entry: Option<(u64, Vec<(u8, u32, egui::Rect)>)> = ctx.data(|d|
                d.get_temp(egui::Id::new(("gp_nav_tz_lines", inner.0, field))));
            let Some((pass, rects)) = entry else { continue; };
            if cur_pass.saturating_sub(pass) > 3 { continue; }
            for (axis, line, rect) in rects {
                if !rect.expand(6.0).contains(cursor) { continue; }
                let d = (rect.center() - cursor).length();
                if best.map_or(true, |(bd, ..)| d < bd) {
                    best = Some((d, field, axis, line as usize));
                }
            }
        }
        best.map(|(_, f, a, l)| (f, a, l))
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

                // RS/gyro cursor hover-select: when the cursor is visible and
                // over a divider (across either pad), focus it — the visual
                // pointer picks a line without dpad cycling.
                if self.gamepad_nav.cursor_visible {
                    if let Some((f, a, l)) = self.nav_tz_cursor_hit(ctx, inner, n_fields) {
                        self.gamepad_nav.tz_field = f;
                        self.gamepad_nav.tz_axis = a;
                        self.gamepad_nav.tz_line = l;
                    }
                }

                // Left/Right → column dividers; Up/Down → row dividers.
                match step_dir {
                    Some(NavDir::Left)  => self.nav_tz_cycle(0, -1, n_col, n_row),
                    Some(NavDir::Right) => self.nav_tz_cycle(0,  1, n_col, n_row),
                    Some(NavDir::Up)    => self.nav_tz_cycle(1, -1, n_col, n_row),
                    Some(NavDir::Down)  => self.nav_tz_cycle(1,  1, n_col, n_row),
                    None => {}
                }

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
                // RT/LT subdivide the selected zone (vertical / horizontal).
                if mapping && (rt_rising || lt_rising) {
                    use flexinput_core::touchzones::Axis;
                    let axis = if rt_rising { Axis::V } else { Axis::H };
                    self.tz_tree_add(outer_id, inner, field, axis);
                }
            }
            EditLevel::TzGrab => {
                if nav.is_rising("btn_south") || nav.is_rising("btn_east") {
                    self.gamepad_nav.edit_level = EditLevel::TzLines;
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

    /// Move the line selection: set the focused axis and step within it. A no-op
    /// when the requested axis has no dividers.
    pub(crate) fn nav_tz_cycle(&mut self, axis: u8, dir: i32, n_cols: usize, n_rows: usize) {
        let n = if axis == 0 { n_cols } else { n_rows };
        if n == 0 { return; }
        if self.gamepad_nav.tz_axis != axis {
            self.gamepad_nav.tz_axis = axis;
            self.gamepad_nav.tz_line = if dir >= 0 { 0 } else { n - 1 };
        } else {
            let next = (self.gamepad_nav.tz_line as i32 + dir).clamp(0, n as i32 - 1);
            self.gamepad_nav.tz_line = next as usize;
        }
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
    pub(crate) fn nav_tz_action_items(phase: &str, has_analog: bool, has_mouse: bool) -> Vec<&'static str> {
        let mut v = match phase {
            "idle"     => vec!["learn"],
            "learning" => vec!["cancel"],
            _          => vec!["assign", "gamepad", "add", "cancel"], // captured
        };
        // Order MUST match the body's act_rects push order: tp_mode, mouse_speed,
        // then hold LAST. tp_mode is a VALUE cycled with LT/RT (like mouse_speed),
        // not a South-activated button.
        if has_analog { v.push("tp_mode"); }
        if has_mouse { v.push("mouse_speed"); }
        v.push("hold");
        v
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

    /// Nudge the node-global `mouse_speed` param, clamped to the UI range.
    pub(crate) fn nav_tz_nudge_mouse_speed(&mut self, outer: egui_snarl::NodeId,
        inner: egui_snarl::NodeId, delta: f32)
    {
        let Some(sp) = self.tabs[self.active_tab].canvas.snarl.get_node_mut(outer)
            .and_then(|n| n.subpatch.as_mut()) else { return; };
        let Some(n) = sp.snarl.get_node_mut(inner) else { return; };
        let cur = n.params.get("mouse_speed").and_then(|v| v.as_f64()).unwrap_or(1.0) as f32;
        let next = (cur + delta).clamp(0.1, 10.0);
        n.params.insert("mouse_speed".into(), serde_json::Value::from(next as f64));
    }

    /// Cycle the node "Touchpad mode" param (synced → percard → touchpad → …) by
    /// `dir` (+1 / −1), for the gamepad-nav path (mirrors the body ComboBox).
    pub(crate) fn nav_tz_cycle_mode(&mut self, outer: egui_snarl::NodeId,
        inner: egui_snarl::NodeId, dir: i32)
    {
        let Some(sp) = self.tabs[self.active_tab].canvas.snarl.get_node_mut(outer)
            .and_then(|n| n.subpatch.as_mut()) else { return; };
        let Some(n) = sp.snarl.get_node_mut(inner) else { return; };
        const MODES: [&str; 3] = ["synced", "percard", "touchpad"];
        let cur = n.params.get("tp_mode").and_then(|v| v.as_str()).unwrap_or("synced");
        let i = MODES.iter().position(|m| *m == cur).unwrap_or(0) as i32;
        let next = MODES[(((i + dir) % 3 + 3) % 3) as usize];
        n.params.insert("tp_mode".into(), serde_json::Value::from(next));
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

    /// Whether the SELECTED zone has an editable response curve (i.e. it drives an
    /// analog output) — gates the curve pseudo-row in the TzCards nav.
    pub(crate) fn nav_tz_zone_has_curve(&self, outer: egui_snarl::NodeId, inner: egui_snarl::NodeId) -> bool {
        let node = self.tabs[self.active_tab].canvas.snarl.get_node(outer)
            .and_then(|n| n.subpatch.as_ref()).and_then(|sp| sp.snarl.get_node(inner));
        let Some(node) = node else { return false; };
        let field = node.params.get("sel_field").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
        let zone = node.params.get("sel_zone").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
        let zm = node.params.get("zone_maps").and_then(|v| v.as_array()).cloned().unwrap_or_default();
        crate::canvas::viewer::tz_zone_is_analog(&zm, field, zone)
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
        let has_mouse = self.nav_tz_has_mouse_card(outer, inner);
        let n_actions = Self::nav_tz_action_items(phase, has_analog, has_mouse).len();
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
        let has_mouse = self.nav_tz_has_mouse_card(outer_id, inner);

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
        let actions = Self::nav_tz_action_items(&phase, has_analog, has_mouse);
        let n_actions = actions.len();
        let card_idxs = self.nav_tz_zone_card_indices(outer_id, inner);
        let count = card_idxs.len();
        // A trailing "curve" pseudo-row when the selected zone drives an analog
        // output (the response-curve editor shown below the cards). South enters
        // the shared curve dot-editor (CurveDots).
        let has_curve = self.nav_tz_zone_has_curve(outer_id, inner);
        let total = n_actions + count + if has_curve { 1 } else { 0 };
        if total == 0 {
            self.nav_tz_publish_selection(ctx, outer_id, inner, &phase);
            ctx.request_repaint();
            return;
        }
        if self.gamepad_nav.card_index >= total { self.gamepad_nav.card_index = total - 1; }

        let cur = self.gamepad_nav.card_index;
        let on_actions = cur < n_actions;
        let new_cur = match step_dir {
            Some(NavDir::Left) if on_actions => cur.saturating_sub(1),
            Some(NavDir::Right) if on_actions => (cur + 1).min(n_actions.saturating_sub(1)),
            Some(NavDir::Down) => {
                if on_actions { n_actions.min(total - 1) } else { (cur + 1).min(total - 1) }
            }
            Some(NavDir::Up) => {
                if on_actions { cur }
                else if cur == n_actions { 0 }
                else { cur - 1 }
            }
            _ => cur,
        };
        self.gamepad_nav.card_index = new_cur;
        let sel = new_cur;

        if sel < n_actions {
            // The mouse-speed item is a VALUE, not a button: LT/RT nudge it in
            // place (no enter). Everything else activates on South.
            if actions[sel] == "mouse_speed" {
                let d = if rt_rising { 0.1 } else if lt_rising { -0.1 } else { 0.0 };
                if d != 0.0 { self.nav_tz_nudge_mouse_speed(outer_id, inner, d); }
            }
            if actions[sel] == "tp_mode" {
                let dir = if rt_rising { 1 } else if lt_rising { -1 } else { 0 };
                if dir != 0 { self.nav_tz_cycle_mode(outer_id, inner, dir); }
            }
            // An action button is focused → South activates it.
            if nav.is_rising("btn_south") {
                match actions[sel] {
                    "learn" => {
                        self.set_subpatch_param_str(outer_id, inner, "_tz_phase", "learning");
                        self.set_subpatch_param_str(outer_id, inner, "_tz_trig", "");
                        self.gamepad_nav.card_index = 0; // action list changed
                    }
                    "cancel" => {
                        self.set_subpatch_param_str(outer_id, inner, "_tz_phase", "idle");
                        self.set_subpatch_param_str_array(outer_id, inner, "_tz_draft_out", &[]);
                        self.set_subpatch_param_bool(outer_id, inner, "_tz_gp_arm", false);
                        self.gamepad_nav.card_index = 0;
                    }
                    "assign" => {
                        // Gamepad-navigable KB/M picker on the main window.
                        self.open_special_picker(crate::canvas::viewer::SpecialPickerRequest {
                            inner,
                            path: vec![outer_id.0],
                            draft_key: "_tz_draft_out".to_string(),
                            phase_key: None,
                            touch_zones: true,
                            exclude_pin_prefix: None,
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
                    // "mouse_speed" is nudged with LT/RT above — South does nothing.
                    _ => {}
                }
            }
        } else if sel < n_actions + count {
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
        } else {
            // The curve pseudo-row is focused → publish a focus flag so the graph
            // rings itself; South ENTERS the shared curve dot-editor (returns to
            // TzCards on exit).
            let pass = ctx.cumulative_pass_nr();
            ctx.data_mut(|d| d.insert_temp(
                egui::Id::new(("gp_nav_tz_curve_focus", inner.0)), pass));
            if nav.is_rising("btn_south") {
                self.gamepad_nav.edit_level = EditLevel::CurveDots;
                self.gamepad_nav.curve_return_level = EditLevel::TzCards;
                self.gamepad_nav.curve_dot = 0;
                self.gamepad_nav.edit_baseline = Some(Box::new(
                    self.tabs[self.active_tab].canvas.snapshot_for_undo()));
            }
        }

        self.nav_tz_publish_selection(ctx, outer_id, inner, &phase);
        ctx.request_repaint();
    }
}
