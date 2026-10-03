//! Gamepad-nav driving of the Area Mapper field: walk cells and borders, grab
//! and move a border, centre or remove it, toggle its gradient, split a cell.
//!
//! It runs on the Touch Zones line-editing levels (`TzLines` / `TzGrab`), focus
//! model and highlight channels — `nav_tz_enter` / `nav_drive_touch_zones` hand
//! an Area Mapper over to here — with a focused border's `tz_line` indexing
//! [`area_body::all_borders`](crate::canvas::area_body::all_borders). Every edit
//! goes through the same `apply_area_edits` the mouse uses, so symmetry and
//! card hand-over on removal behave identically. An analog layer's full-push
//! ring (target kind 2) takes the Touch Zones `Seam` focus, which an Area
//! Mapper has no other use for.

use super::*;
use crate::canvas::area_body::{self as ab, AreaEdit};
use flexinput_core::area::{AreaLayout, BorderRef, Shape};

/// One grab step: a sector border turns by this many turns, any other border
/// moves by this share of the area's half-width.
const STEP_TURN: f32 = 1.0 / 72.0;
const STEP_SPAN: f32 = 0.02;

impl FlexInputApp {
    /// Is the selected widget's module an Area Mapper?
    pub(crate) fn nav_is_area(&self, outer: egui_snarl::NodeId) -> bool {
        self.nav_selected_module_id(outer).as_deref() == Some("module.area_mapper")
    }

    fn nav_area_layout(&self, outer: egui_snarl::NodeId, inner: egui_snarl::NodeId) -> Option<AreaLayout> {
        nav_scope(&self.tabs[self.active_tab].canvas.snarl, outer)
            .and_then(|sp| sp.get_node(inner))
            .map(ab::area_layout)
    }

    fn nav_area_edit(&mut self, outer: egui_snarl::NodeId, inner: egui_snarl::NodeId, edits: &[AreaEdit])
        -> ab::EditResult
    {
        let Some(n) = nav_scope_mut(&mut self.tabs[self.active_tab].canvas.snarl, outer)
            .and_then(|sp| sp.get_node_mut(inner))
        else {
            return ab::EditResult::default();
        };
        let res = ab::apply_area_edits(n, edits);
        ab::area_sync(n);
        res
    }

    /// Set an analog layer's full-push radius from its current one (and the
    /// deadzone), kept just outside the deadzone and inside the rim.
    fn nav_area_set_full(&mut self, outer: egui_snarl::NodeId, inner: egui_snarl::NodeId,
        f: impl Fn(f32, f32) -> f32)
    {
        use flexinput_engine::eval::{AREA_ANALOG_FULL_DEFAULT, AREA_LAYER_FULL_PARAM};
        let Some(n) = nav_scope_mut(&mut self.tabs[self.active_tab].canvas.snarl, outer)
            .and_then(|sp| sp.get_node_mut(inner))
        else {
            return;
        };
        let layout = ab::area_layout(n);
        let dz = if layout.has_centre_disc() { layout.edges[0].pos } else { 0.0 };
        let cur = n.params.get(AREA_LAYER_FULL_PARAM).and_then(|v| v.as_f64()).map(|v| v as f32)
            .unwrap_or(AREA_ANALOG_FULL_DEFAULT);
        let next = (f(cur, dz).clamp(dz + 0.05, 1.0) * 100.0).round() / 100.0;
        n.params.insert(AREA_LAYER_FULL_PARAM.into(), serde_json::json!(next));
        ab::area_sync(n);
        let canvas = &mut self.tabs[self.active_tab].canvas;
        canvas.mutation_gen = canvas.mutation_gen.wrapping_add(1);
    }

    /// The focused border, if a border is focused and still exists.
    fn nav_area_border(&self, outer: egui_snarl::NodeId, inner: egui_snarl::NodeId) -> Option<BorderRef> {
        if !matches!(self.gamepad_nav.tz_focus, crate::gamepad_nav::TzFocus::Border) {
            return None;
        }
        let layout = self.nav_area_layout(outer, inner)?;
        ab::all_borders(&layout).get(self.gamepad_nav.tz_line).copied()
    }

    /// Enter the field from Widget level: focus the selected cell and snapshot
    /// for one coalesced undo entry across the whole edit.
    pub(crate) fn nav_area_enter(&mut self, outer_id: egui_snarl::NodeId) {
        use crate::gamepad_nav::{EditLevel, TzFocus};
        let Some(inner) = self.nav_selected_inner_node(outer_id) else { return };
        let sel = nav_scope(&self.tabs[self.active_tab].canvas.snarl, outer_id)
            .and_then(|sp| sp.get_node(inner))
            .and_then(|n| n.params.get("sel_zone").and_then(|v| v.as_u64()))
            .unwrap_or(0) as usize;
        self.gamepad_nav.tz_field = 0;
        self.gamepad_nav.tz_axis = 0;
        self.gamepad_nav.tz_line = 0;
        self.gamepad_nav.tz_focus = TzFocus::Zone(sel);
        self.gamepad_nav.tz_anchor = None;
        self.gamepad_nav.edit_level = EditLevel::TzLines;
        self.gamepad_nav.edit_baseline = Some(Box::new(
            self.tabs[self.active_tab].canvas.snapshot_for_undo()));
    }

    /// Focus a published target (kind 0 = cell, a = id; kind 1 = border,
    /// b = border index; kind 2 = an analog layer's full-push ring). A cell
    /// becomes the card list's cell; a border opens its settings.
    fn nav_area_focus(&mut self, outer: egui_snarl::NodeId, inner: egui_snarl::NodeId,
        t: (u8, u32, u32, egui::Pos2))
    {
        use crate::gamepad_nav::TzFocus;
        self.gamepad_nav.tz_anchor = Some(t.3);
        if t.0 == 2 {
            self.gamepad_nav.tz_focus = TzFocus::Seam;
        } else if t.0 == 0 {
            self.gamepad_nav.tz_focus = TzFocus::Zone(t.1 as usize);
            self.nav_area_edit(outer, inner, &[AreaEdit::Select(t.1)]);
        } else {
            self.gamepad_nav.tz_focus = TzFocus::Border;
            self.gamepad_nav.tz_axis = 0;
            self.gamepad_nav.tz_line = t.2 as usize;
            let r = self.nav_area_layout(outer, inner)
                .and_then(|l| ab::all_borders(&l).get(t.2 as usize).copied());
            self.nav_area_edit(outer, inner, &[AreaEdit::SelectBorder(r)]);
        }
    }

    /// Where the focus is this frame: of the focused target's points (a ring is
    /// reachable at several), the one nearest where the walk arrived.
    fn nav_area_focus_point(&self, targets: &[(u8, u32, u32, egui::Pos2)]) -> Option<egui::Pos2> {
        use crate::gamepad_nav::TzFocus;
        let hits = targets.iter().filter(|t| match self.gamepad_nav.tz_focus {
            TzFocus::Zone(z) => t.0 == 0 && t.1 == z as u32,
            TzFocus::Border => t.0 == 1 && t.2 == self.gamepad_nav.tz_line as u32,
            TzFocus::Seam => t.0 == 2,
        });
        match self.gamepad_nav.tz_anchor {
            Some(a) => hits.min_by(|x, y| (x.3 - a).length().total_cmp(&(y.3 - a).length())).map(|t| t.3),
            None => hits.map(|t| t.3).next(),
        }
    }

    /// Drive the Area Mapper field (`TzLines` / `TzGrab`):
    ///   walk   — dpad / LS steps cell ↔ border in screen direction; the RS /
    ///            gyro cursor picks directly.
    ///   cell   — RT adds a sector / column border through it, LT a ring / row
    ///            border through its band.
    ///   border — South grabs, North centres, West removes, RT toggles its
    ///            gradient.
    ///   grab   — dpad / LS moves it (right / up = clockwise / out / right /
    ///            up), North centres, South / East drop.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn nav_drive_area_field(
        &mut self,
        ctx: &egui::Context,
        outer_id: egui_snarl::NodeId,
        nav: &crate::gamepad_nav::NavInput,
        step_dir: Option<crate::gamepad_nav::NavDir>,
        rt_rising: bool,
        lt_rising: bool,
    ) {
        use crate::gamepad_nav::{EditLevel, NavDir, TzFocus};
        let Some(inner) = self.nav_selected_inner_node(outer_id) else {
            self.nav_tz_exit();
            return;
        };
        let Some(layout) = self.nav_area_layout(outer_id, inner) else {
            self.nav_tz_exit();
            return;
        };
        let borders = ab::all_borders(&layout);
        if matches!(self.gamepad_nav.tz_focus, TzFocus::Border) && self.gamepad_nav.tz_line >= borders.len() {
            // The focused border went away (removed elsewhere): back to a cell.
            self.gamepad_nav.tz_focus = TzFocus::Zone(0);
            self.gamepad_nav.tz_anchor = None;
            if self.gamepad_nav.edit_level == EditLevel::TzGrab {
                self.gamepad_nav.edit_level = EditLevel::TzLines;
            }
        }
        let targets = self.nav_tz_targets(ctx, inner, 0);

        match self.gamepad_nav.edit_level {
            EditLevel::TzLines => {
                if nav.is_rising("btn_east") {
                    self.nav_tz_exit();
                    return;
                }
                // Cursor hover-select: a border within reach wins, else the
                // nearest cell centre.
                if self.gamepad_nav.cursor_visible && !targets.is_empty() {
                    let cur = self.gamepad_nav.cursor_pos;
                    let near = |kind: u8, reach: f32| targets.iter()
                        .filter(|t| t.0 == kind)
                        .map(|t| (*t, (t.3 - cur).length()))
                        .filter(|(_, d)| *d <= reach)
                        .min_by(|a, b| a.1.total_cmp(&b.1))
                        .map(|(t, _)| t);
                    if let Some(t) = near(1, 16.0).or_else(|| near(2, 16.0)).or_else(|| near(0, f32::INFINITY)) {
                        self.nav_area_focus(outer_id, inner, t);
                    }
                }
                // Walk: from a cell to the nearest border in that direction, from
                // a border to the nearest cell.
                if let Some(dir) = step_dir {
                    let on_cell = matches!(self.gamepad_nav.tz_focus, TzFocus::Zone(_));
                    // An analog layer shows no borders between its cells (only
                    // the deadzone's): walk straight from cell to cell.
                    let analog = nav_scope(&self.tabs[self.active_tab].canvas.snarl, outer_id)
                        .and_then(|sp| sp.get_node(inner))
                        .and_then(|n| n.params.get(flexinput_engine::eval::AREA_LAYER_ANALOG_PARAM))
                        .and_then(|v| v.as_bool()).unwrap_or(false);
                    match self.nav_area_focus_point(&targets) {
                        Some(cur) => {
                            let cands: Vec<(usize, egui::Pos2)> = targets.iter().enumerate()
                                .filter(|(_, t)| if analog { (t.3 - cur).length() > 1.0 } else if on_cell { t.0 == 1 } else { t.0 == 0 })
                                .map(|(i, t)| (i, t.3))
                                .collect();
                            if let Some(ti) = super::touch_zones::nav_tz_nearest_in_dir(cur, dir, &cands) {
                                self.nav_area_focus(outer_id, inner, targets[ti]);
                            }
                        }
                        None => {
                            if let Some(&t) = targets.iter().find(|t| t.0 == 0) {
                                self.nav_area_focus(outer_id, inner, t);
                            }
                        }
                    }
                }
                match self.gamepad_nav.tz_focus {
                    TzFocus::Border => {
                        if let Some(r) = self.nav_area_border(outer_id, inner) {
                            if nav.is_rising("btn_south") {
                                self.gamepad_nav.edit_level = EditLevel::TzGrab;
                            } else if nav.is_rising("btn_north") {
                                self.nav_area_edit(outer_id, inner, &[AreaEdit::Recenter(r)]);
                            } else if nav.is_rising("btn_west") {
                                self.nav_area_edit(outer_id, inner, &[AreaEdit::Remove(r)]);
                                let sel = self.nav_area_layout(outer_id, inner)
                                    .and_then(|l| l.cell_ids().first().copied()).unwrap_or(0);
                                self.gamepad_nav.tz_focus = TzFocus::Zone(sel as usize);
                                self.gamepad_nav.tz_anchor = None;
                            } else if rt_rising {
                                self.nav_area_edit(outer_id, inner,
                                    &[AreaEdit::ToggleGradient(r), AreaEdit::SelectBorder(Some(r))]);
                            }
                        }
                    }
                    TzFocus::Zone(z) => {
                        if rt_rising || lt_rising {
                            if let Some(edits) = split_cell(&layout, z as u32, rt_rising) {
                                self.nav_area_edit(outer_id, inner, &edits);
                            }
                        }
                    }
                    TzFocus::Seam => {
                        if nav.is_rising("btn_south") {
                            self.gamepad_nav.edit_level = EditLevel::TzGrab;
                        } else if nav.is_rising("btn_north") {
                            self.nav_area_set_full(outer_id, inner, |_, _| flexinput_engine::eval::AREA_ANALOG_FULL_DEFAULT);
                        }
                    }
                }
            }
            EditLevel::TzGrab if matches!(self.gamepad_nav.tz_focus, TzFocus::Seam) => {
                // The full-push ring: out / in by a step, North resets it.
                if nav.is_rising("btn_south") || nav.is_rising("btn_east") {
                    self.gamepad_nav.edit_level = EditLevel::TzLines;
                } else if nav.is_rising("btn_north") {
                    self.nav_area_set_full(outer_id, inner, |_, _| flexinput_engine::eval::AREA_ANALOG_FULL_DEFAULT);
                } else if let Some(dir) = step_dir {
                    let delta = match dir {
                        NavDir::Right | NavDir::Up => STEP_SPAN,
                        NavDir::Left | NavDir::Down => -STEP_SPAN,
                    };
                    self.nav_area_set_full(outer_id, inner, |cur, _| cur + delta);
                }
            }
            EditLevel::TzGrab => {
                let Some(r) = self.nav_area_border(outer_id, inner) else {
                    self.gamepad_nav.edit_level = EditLevel::TzLines;
                    self.nav_tz_publish(ctx, inner);
                    return;
                };
                if nav.is_rising("btn_south") || nav.is_rising("btn_east") {
                    self.gamepad_nav.edit_level = EditLevel::TzLines;
                } else if nav.is_rising("btn_north") {
                    self.nav_area_edit(outer_id, inner, &[AreaEdit::Recenter(r)]);
                } else if let Some(dir) = step_dir {
                    let step = if layout.shape == Shape::Circle && matches!(r, BorderRef::Cut { .. }) {
                        STEP_TURN
                    } else {
                        STEP_SPAN
                    };
                    let delta = match dir {
                        NavDir::Right | NavDir::Up => step,
                        NavDir::Left | NavDir::Down => -step,
                    };
                    let res = self.nav_area_edit(outer_id, inner, &[AreaEdit::Nudge(r, delta)]);
                    // A circle cut moved past 12 o'clock renumbers its band:
                    // keep holding the same border.
                    if let (Some(to), Some(l)) = (res.moved_to, self.nav_area_layout(outer_id, inner)) {
                        if let Some(i) = ab::all_borders(&l).iter().position(|b| *b == to) {
                            self.gamepad_nav.tz_line = i;
                        }
                    }
                }
            }
            _ => {}
        }
        self.nav_tz_publish(ctx, inner);
    }
}

/// The edits that split cell `id`: through its middle across the band
/// (`across` — a sector / column border), else along it (a ring / row border
/// through its band). A whole circular ring's first cut halves it (the layout
/// makes it a diameter).
fn split_cell(layout: &AreaLayout, id: u32, across: bool) -> Option<Vec<AreaEdit>> {
    let (band, index) = layout.find_cell(id)?;
    if across {
        if layout.shape == Shape::Circle && layout.bands[band].cuts.len() <= 1 {
            return Some(vec![AreaEdit::AddCut(band, 0.0)]);
        }
        let [v0, v1] = layout.cell_span(band, index);
        Some(vec![AreaEdit::AddCut(band, (v0 + v1) * 0.5)])
    } else {
        // Through the cell's middle, so a squared ring's new neighbour takes
        // its shape.
        let [x, y] = layout.region_point(flexinput_core::area::Region::Cell { band, index }, 0.5, 0.5);
        Some(vec![AreaEdit::AddEdgeAt(x, y)])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splitting_a_cell_halves_it() {
        let l = AreaLayout::default_for(Shape::Circle);
        // Up (cell 1) spans 337.5°..382.5°: split at 360° = 0.
        let edits = split_cell(&l, 1, true).unwrap();
        assert!(matches!(edits[..], [AreaEdit::AddCut(1, v)] if (v - 1.0).abs() < 1e-5 || v.abs() < 1e-5));
        // The centre disc has no cuts: one cut, which the layout makes a diameter.
        let mut l2 = l.clone();
        assert_eq!(split_cell(&l, 0, true).unwrap().len(), 1);
        l2.add_cut(0, 0.0, true);
        assert_eq!(l2.bands[0].cells.len(), 2);
        // Along: a ring border through the middle of the outer band.
        assert!(matches!(split_cell(&l, 1, false).unwrap()[..],
            [AreaEdit::AddEdgeAt(x, y)] if x.abs() < 1e-5 && (y - 0.65).abs() < 1e-5));
    }
}
