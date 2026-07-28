//! Gamepad-nav driving of the Remapper family (Remapper / Map Action /
//! Lean): card selection and glow, per-card accessors, and header-field
//! editing of an entered mapping card.

use super::*;

/// Paint the mapping-card selection glow from the pass-stamped rect channels the
/// remapper/TZ body published this frame. Viewport-agnostic: it reads only ctx
/// temps and paints on whichever `ctx`'s foreground layer you hand it — the main
/// window (via [`FlexInputApp::nav_draw_remap_card_glow`]) or the config-overlay
/// viewport. `inner` is the mapping node; `scope` its mappings key
/// (`"mappings"` / `"zone_maps"` / `"lean_*"`).
pub(crate) fn draw_remap_card_glow(
    ctx: &egui::Context,
    outer_id: egui_snarl::NodeId,
    inner: egui_snarl::NodeId,
    scope: &str,
) {
    let cur_pass = ctx.cumulative_pass_nr();
    let accent = crate::widgets::NavHighlightStyle::of(ctx).accent;
    // The action-button glow uses the full-screen painter (the action row sits at
    // the top of the body and never scrolls); the card glow below re-clips to the
    // card-list viewport so a card scrolled partly out is cropped at its edge.
    let painter = crate::widgets::nav_highlight_painter(
        ctx, egui::Id::new(("gp_nav_remap_card_glow", outer_id.0)), ctx.content_rect());
    let ring = |rect: egui::Rect, round: f32, peak: f32, max_grow: f32| {
        crate::widgets::paint_nav_bloom(&painter, rect, accent, round, peak, max_grow, 6, 2.0, 1.0, 2.0);
    };

    // Card glow (selected/entered card + focused header field). Extend the
    // ring to also enclose the response-curve section (published separately)
    // so the header + its expanded curve read as ONE bordered card, not two.
    // Clip it to the card-list viewport so a tall expanded card scrolled partly
    // out of view has its ring cropped at the visible edge, not spilling past.
    if let Some((pass, card, field, entered)) = ctx.data(|d|
        d.get_temp::<(u64, egui::Rect, Option<egui::Rect>, bool)>(
            egui::Id::new(("gp_nav_remap_card_rects", inner.0, scope))))
    {
        if cur_pass.saturating_sub(pass) <= 2 {
            let mut whole = card;
            if let Some((sp, sect)) = ctx.data(|d| d.get_temp::<(u64, egui::Rect)>(
                egui::Id::new(("gp_nav_card_section_rect", inner.0, scope))))
            {
                // Only union when the section actually butts the bottom of THIS
                // card (flush, 0-gap) — rejects a rect left over from a
                // previously-selected card before its owner stops publishing.
                if cur_pass.saturating_sub(sp) <= 2 && sect.is_finite()
                    && (sect.top() - card.bottom()).abs() < 6.0
                {
                    whole = whole.union(sect);
                }
            }
            let clipped = ctx.data(|d| d.get_temp::<(u64, egui::Rect)>(
                    egui::Id::new(("gp_nav_remap_viewport", inner.0, scope))))
                .filter(|(p, r)| cur_pass.saturating_sub(*p) <= 2 && r.is_finite())
                .map(|(_, vp)| painter.with_clip_rect(vp));
            let cp = clipped.as_ref().unwrap_or(&painter);
            let ring_c = |rect: egui::Rect, round: f32, peak: f32, max_grow: f32| {
                crate::widgets::paint_nav_bloom(cp, rect, accent, round, peak, max_grow, 6, 2.0, 1.0, 2.0);
            };
            ring_c(whole, 5.0, if entered { 130.0 } else { 90.0 }, 7.0);
            if entered {
                if let Some(fr) = field { ring_c(fr, 4.0, 160.0, 6.0); }
            }
        }
    }

    // Action-button glow (Learn / Special / Add) when an action is focused. The
    // action index is nav-published, so gate it against the viewport-agnostic nav
    // pass (the rect channels below stay same-viewport on `cur_pass`).
    let act_sel: Option<usize> = ctx.data(|d|
        d.get_temp::<(u64, usize)>(egui::Id::new(("gp_nav_remap_action", inner.0, scope))))
        .filter(|(p, _)| crate::widgets::nav_pass_matches(ctx, *p))
        .map(|(_, i)| i)
        .filter(|i| *i != usize::MAX);
    if let Some(ai) = act_sel {
        if let Some((pass, rects)) = ctx.data(|d|
            d.get_temp::<(u64, Vec<egui::Rect>)>(egui::Id::new(("gp_nav_action_rects", inner.0, scope))))
        {
            if cur_pass.saturating_sub(pass) <= 2 {
                if let Some(rect) = rects.get(ai) { ring(*rect, 4.0, 130.0, 6.0); }
            }
        }
    }
}

impl FlexInputApp {

    /// Draw the mapping-card selection glow at top level using the GLOBAL rects
    /// the remapper body published last frame (`gp_nav_remap_card_rects`). Must
    /// run outside the body's child layer — painting from inside it deadlocks
    /// epaint's graphics lock.
    pub(crate) fn nav_draw_remap_card_glow(&self, ctx: &egui::Context, outer_id: egui_snarl::NodeId) {
        let Some(inner) = self.nav_selected_inner_node(outer_id) else { return; };
        let scope = self.nav_remap_mappings_key(outer_id);
        draw_remap_card_glow(ctx, outer_id, inner, scope);
    }

    /// Mappings array key for the selected remapper-family node. Remapper /
    /// Map Action / Combiner use `"mappings"`; gyro lean sections use
    /// `"lean_left"` / `"lean_right"` keyed by the pinned element id.
    pub(crate) fn nav_remap_mappings_key(&self, outer_id: egui_snarl::NodeId) -> &'static str {
        match self.nav_selected_element(outer_id).as_ref().map(|(m, e)| (m.as_str(), e.as_str())) {
            Some((_, "lean_left")) => "lean_left",
            Some((_, "lean_right")) => "lean_right",
            // Touch Zones AND Virtual Menu cards live in `zone_maps`. Routing them
            // through the same key lets the shared Remapper card machinery (glow,
            // scroll, field editing) operate on them unchanged. (Missing the menu
            // case published the selection highlight under the wrong scope, so the
            // menu's card glow never matched — the recurring "no highlight" report.)
            Some(("module.touch_zones", "cards")) | Some(("module.menu", "cards")) => "zone_maps",
            _ => "mappings",
        }
    }

    /// Number of mapping cards in the selected remapper-family widget.
    pub(crate) fn nav_remap_card_count(&self, outer_id: egui_snarl::NodeId) -> usize {
        let key = self.nav_remap_mappings_key(outer_id);
        let Some(inner) = self.nav_selected_inner_node(outer_id) else { return 0; };
        let canvas = &self.tabs[self.active_tab].canvas;
        canvas.snarl.get_node(outer_id)
            .and_then(|n| n.subpatch.as_ref())
            .and_then(|sp| sp.snarl.get_node(inner))
            .and_then(|node| node.params.get(key).and_then(|v| v.as_array()))
            .map(|a| a.len()).unwrap_or(0)
    }

    /// Mutable access to the selected node's mappings array, runs `f` on it, and
    /// returns whether `f` reported a change (writing back only then).
    pub(crate) fn nav_remap_with_mappings<F>(&mut self, outer_id: egui_snarl::NodeId, f: F) -> bool
    where F: FnOnce(&mut Vec<serde_json::Value>) -> bool {
        let key = self.nav_remap_mappings_key(outer_id);
        let Some(inner) = self.nav_selected_inner_node(outer_id) else { return false; };
        let canvas = &mut self.tabs[self.active_tab].canvas;
        let Some(sp) = canvas.snarl.get_node_mut(outer_id).and_then(|n| n.subpatch.as_mut()) else { return false; };
        let Some(node) = sp.snarl.get_node_mut(inner) else { return false; };
        let mut arr = node.params.get(key).and_then(|v| v.as_array()).cloned().unwrap_or_default();
        let changed = f(&mut arr);
        if changed {
            node.params.insert(key.to_string(), serde_json::Value::Array(arr));
        }
        changed
    }

    /// Mutate card `idx` as an object map, UPGRADING a legacy `Array<String>`
    /// entry (Map Action's old format) to `{ in: [strings] }` first so the edit
    /// lands. Without this, press-mode / toggle edits on never-yet-edited Map
    /// Action cards silently no-op (the entry isn't an object). Returns whether
    /// `f` reported a change.
    pub(crate) fn nav_remap_card_obj_mut<F>(&mut self, outer_id: egui_snarl::NodeId, idx: usize, f: F) -> bool
    where F: FnOnce(&mut serde_json::Map<String, serde_json::Value>) -> bool {
        self.nav_remap_with_mappings(outer_id, |arr| {
            let Some(slot) = arr.get_mut(idx) else { return false; };
            // Upgrade legacy [strings] → { in: [strings] }.
            if let Some(pins) = slot.as_array().map(|a| a.clone()) {
                let mut obj = serde_json::Map::new();
                obj.insert("in".to_string(), serde_json::Value::Array(pins));
                *slot = serde_json::Value::Object(obj);
            }
            match slot.as_object_mut() {
                Some(m) => f(m),
                None => false,
            }
        })
    }

    /// Read a card's `mode` string (default "down").
    pub(crate) fn nav_remap_card_mode(&self, outer_id: egui_snarl::NodeId, idx: usize) -> Option<String> {
        let key = self.nav_remap_mappings_key(outer_id);
        let inner = self.nav_selected_inner_node(outer_id)?;
        let canvas = &self.tabs[self.active_tab].canvas;
        let arr = canvas.snarl.get_node(outer_id)?.subpatch.as_ref()?
            .snarl.get_node(inner)?.params.get(key)?.as_array()?;
        let m = arr.get(idx)?;
        Some(m.get("mode").and_then(|v| v.as_str()).unwrap_or("down").to_string())
    }

    /// Read a card's bool field (default false).
    pub(crate) fn nav_remap_card_bool(&self, outer_id: egui_snarl::NodeId, idx: usize, key: &str) -> bool {
        let mkey = self.nav_remap_mappings_key(outer_id);
        let Some(inner) = self.nav_selected_inner_node(outer_id) else { return false; };
        let canvas = &self.tabs[self.active_tab].canvas;
        canvas.snarl.get_node(outer_id).and_then(|n| n.subpatch.as_ref())
            .and_then(|sp| sp.snarl.get_node(inner))
            .and_then(|node| node.params.get(mkey).and_then(|v| v.as_array()))
            .and_then(|a| a.get(idx))
            .and_then(|m| m.get(key).and_then(|v| v.as_bool()))
            .unwrap_or(false)
    }

    pub(crate) fn nav_remap_set_card_bool(&mut self, outer_id: egui_snarl::NodeId, idx: usize, key: &str, val: bool) {
        let key = key.to_string();
        self.nav_remap_card_obj_mut(outer_id, idx, |m| {
            m.insert(key, serde_json::Value::Bool(val));
            true
        });
    }

    /// Delete card `idx`.
    pub(crate) fn nav_remap_delete_card(&mut self, outer_id: egui_snarl::NodeId, idx: usize) -> bool {
        self.nav_remap_with_mappings(outer_id, |arr| {
            if idx < arr.len() { arr.remove(idx); true } else { false }
        })
    }

    /// Reset card `idx` to the default press mode (down) + clear its params.
    pub(crate) fn nav_remap_reset_card(&mut self, outer_id: egui_snarl::NodeId, idx: usize) -> bool {
        self.nav_remap_card_obj_mut(outer_id, idx, |m| {
            let had = m.contains_key("mode") || m.contains_key("window_ms")
                || m.contains_key("sustain") || m.contains_key("turbo");
            m.remove("mode");
            m.remove("window_ms");
            m.remove("sustain");
            m.remove("turbo");
            had
        })
    }

    /// Press modes in popup order (analog is offered for all remapper-family +
    /// lean widgets, which are the only ones with editable cards). Shared by the
    /// inline cycle and the press-mode picker modal.
    pub(crate) const PRESS_MODES: &'static [&'static str] =
        &["down","short","long","double","on_press","on_release","analog"];

    /// Set card `idx`'s press mode to `mode` directly, applying the same
    /// default-param fixups the renderer's popup does.
    pub(crate) fn nav_remap_set_mode(&mut self, outer_id: egui_snarl::NodeId, idx: usize, mode: &str) {
        let mode = mode.to_string();
        self.nav_remap_card_obj_mut(outer_id, idx, |m| {
            if mode == "down" {
                m.remove("mode");
                m.remove("window_ms");
                m.remove("sustain");
            } else {
                m.insert("mode".into(), serde_json::Value::String(mode.clone()));
                if !m.contains_key("window_ms") {
                    m.insert("window_ms".into(), serde_json::json!(200.0));
                }
            }
            true
        });
    }

    /// Cycle card `idx`'s press mode by `dir`, applying the same default-param
    /// fixups the card renderer's popup does (down clears window_ms/sustain;
    /// other modes seed window_ms). Mirrors the analog availability rule.
    pub(crate) fn nav_remap_cycle_mode(&mut self, outer_id: egui_snarl::NodeId, idx: usize, dir: i32) {
        let modes = Self::PRESS_MODES;
        self.nav_remap_card_obj_mut(outer_id, idx, |m| {
            let cur = m.get("mode").and_then(|v| v.as_str()).unwrap_or("down");
            let ci = modes.iter().position(|x| *x == cur).unwrap_or(0) as i32;
            let next = modes[(ci + dir).rem_euclid(modes.len() as i32) as usize];
            if next == "down" {
                m.remove("mode");
                m.remove("window_ms");
                m.remove("sustain");
            } else {
                m.insert("mode".into(), serde_json::Value::String(next.to_string()));
                if !m.contains_key("window_ms") {
                    m.insert("window_ms".into(), serde_json::json!(200.0));
                }
            }
            true
        });
    }

    /// Nudge card `idx`'s `window_ms` by `delta`, clamped to 10..5000.
    pub(crate) fn nav_remap_nudge_window(&mut self, outer_id: egui_snarl::NodeId, idx: usize, delta: f32) {
        self.nav_remap_card_obj_mut(outer_id, idx, |m| {
            let cur = m.get("window_ms").and_then(|v| v.as_f64()).unwrap_or(200.0) as f32;
            let next = (cur + delta).clamp(10.0, 5000.0);
            m.insert("window_ms".into(), serde_json::json!(next as f64));
            true
        });
    }

    // ── Touch Zones line editing (gamepad nav) ───────────────────────────────
    // Easy-mode-only: operates on the pinned "field" element of a Touch Zones
    // node in the tab-canvas sub-patch. Ports mode is MOVE-ONLY (dividers carry
    // typed wiring); mapping mode also allows add/remove.

    /// Param key for a field's interior-edge array (`col_edges`/`row_edges` for
    /// field 0, suffixed for field N) — matches the viewer/eval convention.
    pub(crate) fn tz_edge_key(field: usize, which: &str) -> String {
        if field == 0 { which.to_string() } else { format!("{which}{field}") }
    }

    /// Read a Touch Zones inner node's interior-edge array from the tab-canvas
    /// sub-patch (where gamepad nav operates on the body).
    pub(crate) fn tz_edges(&self, outer: egui_snarl::NodeId, inner: egui_snarl::NodeId,
        field: usize, which: &str) -> Vec<f32>
    {
        let key = Self::tz_edge_key(field, which);
        self.tabs[self.active_tab].canvas.snarl.get_node(outer)
            .and_then(|n| n.subpatch.as_ref())
            .and_then(|sp| sp.snarl.get_node(inner))
            .and_then(|n| n.params.get(&key).and_then(|v| v.as_array()))
            .map(|a| a.iter().filter_map(|x| x.as_f64().map(|f| f as f32)).collect())
            .unwrap_or_default()
    }

    /// Overwrite a field's interior-edge array (kept sorted by the caller). The
    /// engine reads the tab-canvas snarl each frame, so the change is live; the
    /// undo baseline snapshotted on enter is committed on exit.
    pub(crate) fn tz_set_edges(&mut self, outer: egui_snarl::NodeId, inner: egui_snarl::NodeId,
        field: usize, which: &str, vals: &[f32])
    {
        let key = Self::tz_edge_key(field, which);
        if let Some(sp) = self.tabs[self.active_tab].canvas.snarl
            .get_node_mut(outer).and_then(|n| n.subpatch.as_mut())
        {
            if let Some(node) = sp.snarl.get_node_mut(inner) {
                node.params.insert(key, serde_json::Value::Array(
                    vals.iter().map(|v| serde_json::json!(*v as f64)).collect()));
            }
        }
    }

    /// Mapping mode allows add/remove of dividers (no typed wiring to break);
    /// ports mode is move-only.
    pub(crate) fn tz_is_mapping(&self, outer: egui_snarl::NodeId, inner: egui_snarl::NodeId) -> bool {
        self.tabs[self.active_tab].canvas.snarl.get_node(outer)
            .and_then(|n| n.subpatch.as_ref())
            .and_then(|sp| sp.snarl.get_node(inner))
            .and_then(|n| n.params.get("zone_mode").and_then(|v| v.as_str()))
            == Some("mapping")
    }

    /// Number of pads (2 in split mode, else 1).
    pub(crate) fn tz_n_fields(&self, outer: egui_snarl::NodeId, inner: egui_snarl::NodeId) -> usize {
        let split = self.tabs[self.active_tab].canvas.snarl.get_node(outer)
            .and_then(|n| n.subpatch.as_ref())
            .and_then(|sp| sp.snarl.get_node(inner))
            .and_then(|n| n.params.get("field_mode").and_then(|v| v.as_str())) == Some("split");
        if split { 2 } else { 1 }
    }

    // ── Mapping-mode (BSP-tree) divider nav ──────────────────────────────────
    // Mapping mode draws dividers from the zone TREE, not the grid edge arrays, so
    // the gamepad line editor drives the tree by divider PATH. (axis, tz_line) index
    // per-axis into `dividers()` order, matching the renderer's per-axis numbering.

    /// Tree dividers for a field as (axis 0=V/1=H, pos, lo, hi, path), `dividers()`
    /// order preserved so the Nth of an axis matches the renderer's Nth.
    pub(crate) fn tz_tree_divs(&self, outer: egui_snarl::NodeId, inner: egui_snarl::NodeId, field: usize)
        -> Vec<(u8, f32, f32, f32, Vec<u8>)>
    {
        let Some(sp) = self.tabs[self.active_tab].canvas.snarl.get_node(outer)
            .and_then(|n| n.subpatch.as_ref()) else { return Vec::new(); };
        crate::canvas::viewer::tz_field_tree(&sp.snarl, inner, field).dividers().into_iter()
            .map(|d| (
                if d.axis == flexinput_core::touchzones::Axis::V { 0u8 } else { 1u8 },
                d.pos, d.lo, d.hi, d.path,
            )).collect()
    }

    /// Count of tree dividers of `axis` (0=V, 1=H) in the field.
    pub(crate) fn tz_tree_axis_len(&self, outer: egui_snarl::NodeId, inner: egui_snarl::NodeId,
        field: usize, axis: u8) -> usize
    {
        self.tz_tree_divs(outer, inner, field).iter().filter(|d| d.0 == axis).count()
    }

    /// The currently-focused tree divider (tz_axis + tz_line): (pos, lo, hi, path).
    pub(crate) fn tz_tree_sel(&self, outer: egui_snarl::NodeId, inner: egui_snarl::NodeId, field: usize)
        -> Option<(f32, f32, f32, Vec<u8>)>
    {
        let axis = self.gamepad_nav.tz_axis;
        self.tz_tree_divs(outer, inner, field).into_iter()
            .filter(|d| d.0 == axis).nth(self.gamepad_nav.tz_line)
            .map(|d| (d.1, d.2, d.3, d.4))
    }

    /// Set the tree divider at `path` to absolute `t`, writing the tree back.
    pub(crate) fn tz_tree_set_t(&mut self, outer: egui_snarl::NodeId, inner: egui_snarl::NodeId,
        field: usize, path: &[u8], t: f32)
    {
        if let Some(sp) = self.tabs[self.active_tab].canvas.snarl.get_node_mut(outer)
            .and_then(|n| n.subpatch.as_mut())
        {
            let mut tree = crate::canvas::viewer::tz_field_tree(&sp.snarl, inner, field);
            if tree.set_divider_t(path, t) {
                crate::canvas::viewer::tz_set_field_tree(&mut sp.snarl, inner, field, &tree);
            }
        }
    }

    /// Remove/merge the tree divider at `path` (raises the mapped-zone confirm popup
    /// when the merged zones carry cards — same as the mouse "−").
    pub(crate) fn tz_tree_remove(&mut self, outer: egui_snarl::NodeId, inner: egui_snarl::NodeId,
        field: usize, path: &[u8])
    {
        if let Some(sp) = self.tabs[self.active_tab].canvas.snarl.get_node_mut(outer)
            .and_then(|n| n.subpatch.as_mut())
        {
            let tree = crate::canvas::viewer::tz_field_tree(&sp.snarl, inner, field);
            crate::canvas::viewer::tz_request_or_apply_merge(&mut sp.snarl, inner, field, &tree, path);
        }
    }

    /// Divide the focused zone in PORTS mode (no BSP tree): insert a full grid cut
    /// through the zone's own column/row band — the same `GridOp` the mouse "+"
    /// applies, so typed zone-port wiring is remapped/reconnected identically.
    pub(crate) fn tz_ports_divide(&mut self, outer: egui_snarl::NodeId, inner: egui_snarl::NodeId,
        field: usize, zone: usize, axis: flexinput_core::touchzones::Axis)
    {
        use flexinput_core::touchzones::{Axis, GridOp};
        let ncols = self.tz_edges(outer, inner, field, "col_edges").len() + 1;
        let op = match axis {
            Axis::V => GridOp::InsertCol(zone % ncols),
            Axis::H => GridOp::InsertRow(zone / ncols),
        };
        if let Some(sp) = self.tabs[self.active_tab].canvas.snarl
            .get_node_mut(outer).and_then(|n| n.subpatch.as_mut())
        {
            crate::canvas::viewer::tz_restructure(inner, field, op, &mut sp.snarl);
        }
    }

    /// Subdivide the currently-selected zone along `axis` (gamepad "add"), the new
    /// cell on the high side — same primitive as the mouse "+".
    pub(crate) fn tz_tree_add(&mut self, outer: egui_snarl::NodeId, inner: egui_snarl::NodeId,
        field: usize, axis: flexinput_core::touchzones::Axis)
    {
        let zone = self.tabs[self.active_tab].canvas.snarl.get_node(outer)
            .and_then(|n| n.subpatch.as_ref()).and_then(|sp| sp.snarl.get_node(inner))
            .and_then(|n| n.params.get("sel_zone").and_then(|v| v.as_u64())).unwrap_or(0) as u32;
        if let Some(sp) = self.tabs[self.active_tab].canvas.snarl.get_node_mut(outer)
            .and_then(|n| n.subpatch.as_mut())
        {
            let tree = crate::canvas::viewer::tz_field_tree(&sp.snarl, inner, field);
            if let Some([x0, y0, x1, y1]) = tree.zone_rect(zone) {
                let (cx, cy) = ((x0 + x1) * 0.5, (y0 + y1) * 0.5);
                crate::canvas::viewer::tz_subdivide_at(&mut sp.snarl, inner, field, cx, cy, axis, false);
            }
        }
    }

    /// Drive a remapper-family widget the user has ENTERED (RemapScroll level):
    /// up/down moves the SELECTED CARD, North resets it (or arms Learn when the
    /// list is empty), West deletes it, South ENTERS it (`RemapCard`), LT/RT
    /// cycle the filter, East exits. The selected card is published for the body
    /// to glow + auto-scroll into view.
    pub(crate) fn nav_drive_remapper(
        &mut self,
        ctx: &egui::Context,
        outer_id: egui_snarl::NodeId,
        nav: &crate::gamepad_nav::NavInput,
        _dt: f32,
        step_dir: Option<crate::gamepad_nav::NavDir>,
        rt_rising: bool,
        lt_rising: bool,
    ) {
        use crate::gamepad_nav::{EditLevel, NavDir};
        let Some(inner) = self.nav_selected_inner_node(outer_id) else {
            self.gamepad_nav.edit_level = EditLevel::Widget;
            return;
        };

        // While capture is ARMED or IN PROGRESS (the user pressed Learn), the
        // nav driver goes INERT for this device so the raw button/combo reaches
        // the capture machine instead of being eaten as navigation. Capture
        // latches on release inside the body; we resume once it leaves the
        // capturing/learning phases (and the arm clears). Selection is still
        // published so the glow persists.
        // Inert ONLY while a nav-mode capture is actually ARMED. The arm now
        // persists through the whole chord (it clears on latch, not at capture
        // start), so `armed` alone covers the entire capture window. We must NOT
        // gate on phase=="capturing" alone — Map Action auto-sits in "capturing"
        // whenever wired, so doing so would block East/South forever (the user's
        // "stuck in Map Action" bug).
        // Lean sections arm via side-scoped `_lean_<side>_armed`; everything else
        // via `_nav_capture_armed`.
        let armed_key = match self.nav_lean_side(outer_id) {
            Some("left") => "_lean_left_armed",
            Some("right") => "_lean_right_armed",
            _ => "_nav_capture_armed",
        };
        let armed = self.get_subpatch_param_bool(outer_id, inner, armed_key).unwrap_or(false);
        if armed {
            self.nav_publish_remap_selection(ctx, outer_id, inner);
            ctx.request_repaint();
            return;
        }

        // East exits the widget (only when not mid-capture).
        if nav.is_rising("btn_east") {
            self.gamepad_nav.edit_level = EditLevel::Widget;
            return;
        }

        // LT/RT cycle the mapping filter.
        if rt_rising || lt_rising {
            let dir = if rt_rising { 1 } else { -1 };
            crate::canvas::viewer::nav_cycle_remapper_filter(ctx, inner.0, dir);
        }

        // Two navigation rows:
        //   • the ACTION row (Learn / Special / Add) — navigated LEFT/RIGHT,
        //     since the buttons sit side-by-side; cursor 0..n_actions.
        //   • the CARD list below it — navigated UP/DOWN; cursor n_actions..total.
        // Up from the first card lands on the action row (keeping the horizontal
        // sub-index where possible); Down from the action row lands on the first
        // card. South activates the focused item; West deletes a focused card.
        let actions = self.nav_remap_action_items(outer_id, inner);
        let n_actions = actions.len();
        let count = self.nav_remap_card_count(outer_id);
        let total = n_actions + count;
        if total == 0 { return; }
        if self.gamepad_nav.card_index >= total { self.gamepad_nav.card_index = total - 1; }

        let cur = self.gamepad_nav.card_index;
        let on_actions = cur < n_actions;
        let new_cur = match step_dir {
            Some(NavDir::Left) if on_actions => cur.saturating_sub(1),
            Some(NavDir::Right) if on_actions => (cur + 1).min(n_actions.saturating_sub(1)),
            // Down: from the action row → first card; within cards → next card.
            Some(NavDir::Down) => {
                if on_actions { n_actions.min(total - 1) }
                else { (cur + 1).min(total - 1) }
            }
            // Up: within cards → prev card; from the first card → action row.
            Some(NavDir::Up) => {
                if on_actions { cur } // already at the top row
                else if cur == n_actions { if n_actions > 0 { 0 } else { cur } }
                else { cur - 1 }
            }
            _ => cur,
        };
        self.gamepad_nav.card_index = new_cur;
        let sel = new_cur;

        if sel < n_actions {
            // An action button/dropdown is focused → South activates it.
            if nav.is_rising("btn_south") {
                let action = actions[sel];
                // Special (Remapper or Lean) opens the virtual KB/M picker
                // instead of the mouse-only ComboBox. The picker writes its chord
                // into the widget's draft param — `draft_output` for the
                // Remapper, `_lean_<side>_draft` for a Lean section.
                let lean_draft = match action {
                    "_nav_act_special_left" => Some("_lean_left_draft"),
                    "_nav_act_special_right" => Some("_lean_right_draft"),
                    _ => None,
                };
                if action == "_nav_act_special" || lean_draft.is_some() {
                    self.gamepad_nav.kbm_picker_open = true;
                    self.gamepad_nav.kbm_picker_idx = 0;
                    self.gamepad_nav.kbm_picker_node = Some(inner);
                    self.gamepad_nav.kbm_picker_path = vec![outer_id.0];
                    self.gamepad_nav.kbm_picker_touch_zones = false;
                    self.gamepad_nav.kbm_picker_exclude = None;
                    // When the config overlay owns nav (config_nav_sel set), draw
                    // the picker in its own always-on-top viewport over the game,
                    // not the main window (which is behind the game).
                    self.gamepad_nav.kbm_picker_viewport =
                        if self.gamepad_nav.config_nav_sel.is_some() {
                            Some(crate::config_overlay::picker_viewport_id())
                        } else {
                            None
                        };
                    self.gamepad_nav.kbm_picker_draft_key =
                        lean_draft.unwrap_or("draft_output").to_string();
                    self.gamepad_nav.kbm_picker_phase_key =
                        match action {
                            "_nav_act_special_left" => Some("_lean_left_phase"),
                            "_nav_act_special_right" => Some("_lean_right_phase"),
                            _ => None,
                        }.map(|s| s.to_string());
                } else {
                    self.set_subpatch_param_bool(outer_id, inner, action, true);
                }
                ctx.request_repaint();
            }
        } else {
            // A mapping card is focused.
            let card_idx = sel - n_actions;
            if nav.is_rising("btn_west") {
                let base = self.tabs[self.active_tab].canvas.snapshot_for_undo();
                if self.nav_remap_delete_card(outer_id, card_idx) {
                    self.tabs[self.active_tab].canvas.commit_undo_if_changed(base);
                }
            } else if nav.is_rising("btn_south") {
                self.gamepad_nav.edit_level = EditLevel::RemapCard;
                self.gamepad_nav.card_return_level = EditLevel::RemapScroll;
                self.gamepad_nav.remap_card = card_idx;
                self.gamepad_nav.card_field = 0;
                self.gamepad_nav.edit_baseline = Some(Box::new(
                    self.tabs[self.active_tab].canvas.snapshot_for_undo()));
            }
        }

        self.nav_publish_remap_selection(ctx, outer_id, inner);
    }

    /// Drive the virtual KB/M picker (modal). `step_dir` is the resolved
    /// dpad/stick direction this frame. South appends the focused pin to the
    /// remapper's `draft_output`; North resets `draft_output`; East closes.
    /// All macro ports defined on the active tab's canvas (including nested
    /// sub-patches), as the display entries the picker / chip renderers use.
    /// Order: canvas traversal order, so the picker cluster stays stable
    /// frame-to-frame.
    pub(crate) fn macro_display_entries(&self) -> Vec<crate::macro_icons::MacroDisplayEntry> {
        use flexinput_core::macros as mac;
        fn scan(snarl: &Snarl<NodeData>, out: &mut Vec<crate::macro_icons::MacroDisplayEntry>) {
            use flexinput_core::menu as fm;
            for (_, node_ref) in snarl.nodes_ids_data() {
                let n = &node_ref.value;
                if n.module_id == "module.macro" {
                    for p in mac::ports_from_params(&n.params) {
                        out.push(crate::macro_icons::MacroDisplayEntry {
                            pin: mac::macro_pin_id(&p.id),
                            name: p.name,
                            icon: p.icon,
                            icon_svg: p.icon_svg,
                            signal_type: p.signal_type,
                        });
                    }
                }
                // Virtual Menus are macro-style targets too: one Show + one
                // Select entry per menu, wearing the menu's name + icon.
                if n.module_id == "module.menu" {
                    if let Some(mid) = n.params.get("menu_id").and_then(|v| v.as_str()) {
                        let name = n.params.get("menu_name").and_then(|v| v.as_str()).unwrap_or("");
                        let name = if name.is_empty() { "Menu" } else { name };
                        let icon = n.params.get("menu_icon").and_then(|v| v.as_str()).unwrap_or("");
                        let icon_svg = n.params.get("menu_icon_svg").and_then(|v| v.as_str()).unwrap_or("");
                        for (t, verb) in [(fm::TargetPin::Show, "Show"), (fm::TargetPin::Select, "Select")] {
                            out.push(crate::macro_icons::MacroDisplayEntry {
                                pin: fm::target_pin(mid, t),
                                name: format!("{name} — {verb}"),
                                icon: icon.to_string(),
                                icon_svg: icon_svg.to_string(),
                                signal_type: flexinput_core::SignalType::Bool,
                            });
                        }
                    }
                }
                if let Some(sp) = n.subpatch.as_ref() {
                    scan(&sp.snarl, out);
                }
            }
        }
        let mut v = Vec::new();
        scan(&self.tabs[self.active_tab].canvas.snarl, &mut v);
        v
    }

    /// Read a string-array draft as a Vec<String>.
    pub(crate) fn nav_remap_draft_vec(&self, outer: egui_snarl::NodeId, inner: egui_snarl::NodeId, key: &str)
        -> Vec<String>
    {
        let canvas = &self.tabs[self.active_tab].canvas;
        canvas.snarl.get_node(outer).and_then(|n| n.subpatch.as_ref())
            .and_then(|sp| sp.snarl.get_node(inner))
            .and_then(|node| node.params.get(key).and_then(|v| v.as_array()))
            .map(|a| a.iter().filter_map(|v| v.as_str().map(|s| s.to_string())).collect())
            .unwrap_or_default()
    }

    /// Write a string-array param on an inner sub-patch node.
    pub(crate) fn set_subpatch_param_str_array(&mut self, outer: egui_snarl::NodeId,
        inner: egui_snarl::NodeId, key: &str, vals: &[String])
    {
        let canvas = &mut self.tabs[self.active_tab].canvas;
        let Some(sp) = canvas.snarl.get_node_mut(outer).and_then(|n| n.subpatch.as_mut()) else { return; };
        if let Some(node) = sp.snarl.get_node_mut(inner) {
            let arr: Vec<serde_json::Value> = vals.iter()
                .map(|s| serde_json::Value::String(s.clone())).collect();
            node.params.insert(key.to_string(), serde_json::Value::Array(arr));
        }
    }

    // ── KB/M picker addressing (arbitrary sub-patch depth) ────────────────
    // The Special picker can target a node sitting directly on the tab canvas
    // (`path = []`) or nested through any chain of sub-patches (`path` = the
    // subpatch node ids, outermost first). The old single-level `outer`
    // addressing silently wrote drafts into the wrong snarl for nodes more
    // than one level deep — the picker "picked" but the body never saw it.

    /// Resolve the picker's target node through its sub-patch path.
    pub(crate) fn picker_node(&self, path: &[usize], inner: egui_snarl::NodeId)
        -> Option<&crate::canvas::NodeData>
    {
        let mut snarl = &self.tabs[self.active_tab].canvas.snarl;
        for &p in path {
            snarl = &snarl.get_node(egui_snarl::NodeId(p))?.subpatch.as_ref()?.snarl;
        }
        snarl.get_node(inner)
    }

    /// Mutable resolve through the TAB canvas only. While a sub-patch editor
    /// window is open, the tab node's `sp.snarl` is a MIRROR — the editor's
    /// own `canvas` is the live copy, cloned over the mirror on every editor
    /// mutation (see `show_subpatch_editors`' write-back). Writing only here
    /// gets silently wiped by that clone, so picker writes must go through
    /// [`Self::picker_write`]; this is its building block.
    pub(crate) fn picker_tab_node_mut(&mut self, path: &[usize], inner: egui_snarl::NodeId)
        -> Option<&mut crate::canvas::NodeData>
    {
        let mut snarl = &mut self.tabs[self.active_tab].canvas.snarl;
        for &p in path {
            snarl = &mut snarl.get_node_mut(egui_snarl::NodeId(p))?.subpatch.as_mut()?.snarl;
        }
        snarl.get_node_mut(inner)
    }

    /// Open editor windows holding a live copy of the picker target: for each
    /// prefix of `path` whose sub-patch chain is open as an editor chain
    /// (top-level editor parented to the tab canvas, then children matched via
    /// `parent_editor_idx`), the deepest matching editor's canvas contains the
    /// target — reached by descending the REMAINING path elements as plain
    /// subpatch fields. Returns `(editor index, consumed prefix len)` pairs.
    pub(crate) fn picker_editor_hits(&self, path: &[usize]) -> Vec<(usize, usize)> {
        let mut hits = Vec::new();
        let mut parent: Option<usize> = None;
        for (k, &p) in path.iter().enumerate() {
            let Some(idx) = self.sub_patch_editors.iter().position(|e| {
                e.tab_idx == self.active_tab
                    && e.parent_editor_idx == parent
                    && e.node_id.0 == p
            }) else { break; };
            hits.push((idx, k + 1));
            parent = Some(idx);
        }
        hits
    }

    /// Apply `f` to EVERY live copy of the picker's target node: the tab
    /// canvas (mirror) plus each open sub-patch editor canvas along `path`.
    /// Keeping all copies identical means neither the editors' write-back
    /// (editor → parent, on editor mutation) nor their pre-sync (parent →
    /// editor, on parent mutation) can clobber a picked draft — the whole-
    /// snarl clone in either direction carries the same data.
    pub(crate) fn picker_write(&mut self, path: &[usize], inner: egui_snarl::NodeId,
        f: &dyn Fn(&mut crate::canvas::NodeData))
    {
        fn descend<'a>(
            mut snarl: &'a mut egui_snarl::Snarl<crate::canvas::NodeData>,
            path: &[usize],
        ) -> Option<&'a mut egui_snarl::Snarl<crate::canvas::NodeData>> {
            for &p in path {
                snarl = &mut snarl.get_node_mut(egui_snarl::NodeId(p))?
                    .subpatch.as_mut()?.snarl;
            }
            Some(snarl)
        }
        if let Some(node) = self.picker_tab_node_mut(path, inner) { f(node); }
        for (idx, consumed) in self.picker_editor_hits(path) {
            let root = &mut self.sub_patch_editors[idx].canvas.snarl;
            if let Some(node) = descend(root, &path[consumed..])
                .and_then(|s| s.get_node_mut(inner))
            { f(node); }
        }
    }

    /// Read a string-array param as a Vec from the picker's target node.
    pub(crate) fn picker_draft_vec(&self, path: &[usize], inner: egui_snarl::NodeId, key: &str) -> Vec<String> {
        self.picker_node(path, inner)
            .and_then(|node| node.params.get(key).and_then(|v| v.as_array()))
            .map(|a| a.iter().filter_map(|v| v.as_str().map(|s| s.to_string())).collect())
            .unwrap_or_default()
    }

    /// Write a string-array param on the picker's target node (all live copies).
    pub(crate) fn picker_set_draft(&mut self, path: &[usize], inner: egui_snarl::NodeId,
        key: &str, vals: &[String])
    {
        let arr: Vec<serde_json::Value> = vals.iter()
            .map(|s| serde_json::Value::String(s.clone())).collect();
        let val = serde_json::Value::Array(arr);
        let key = key.to_string();
        self.picker_write(path, inner, &move |node| {
            node.params.insert(key.clone(), val.clone());
        });
    }

    /// Read a string param from the picker's target node.
    pub(crate) fn picker_target_param_str(&self, path: &[usize], inner: egui_snarl::NodeId,
        key: &str) -> Option<String>
    {
        self.picker_node(path, inner)
            .and_then(|n| n.params.get(key).and_then(|v| v.as_str().map(String::from)))
    }

    /// Write a string param on the picker's target node (all live copies).
    pub(crate) fn picker_set_param_str(&mut self, path: &[usize], inner: egui_snarl::NodeId,
        key: &str, val: &str)
    {
        let key = key.to_string();
        let val = val.to_string();
        self.picker_write(path, inner, &move |node| {
            node.params.insert(key.clone(), serde_json::Value::String(val.clone()));
        });
    }

    /// Open the shared KB/M + touchpad picker for a mouse-driven Special click.
    /// `viewport` is the egui viewport the click came from (`None` = the main
    /// window) — the modal is drawn inside THAT viewport so it appears on the
    /// window the user is actually working in.
    pub(crate) fn open_special_picker(&mut self, req: crate::canvas::viewer::SpecialPickerRequest,
        viewport: Option<egui::ViewportId>)
    {
        self.gamepad_nav.kbm_picker_open = true;
        self.gamepad_nav.kbm_picker_idx = 0;
        self.gamepad_nav.kbm_picker_node = Some(req.inner);
        self.gamepad_nav.kbm_picker_path = req.path;
        self.gamepad_nav.kbm_picker_draft_key = req.draft_key;
        self.gamepad_nav.kbm_picker_phase_key = req.phase_key;
        self.gamepad_nav.kbm_picker_touch_zones = req.touch_zones;
        self.gamepad_nav.kbm_picker_exclude = req.exclude_pin_prefix;
        self.gamepad_nav.kbm_picker_viewport = viewport;
    }

    /// Whether the picker's current target accepts analog-only outputs (swipe).
    /// Lean sections are always analog (the lean gesture is the input); Remapper
    /// is analog only when its captured `draft_input` holds an analog source.
    pub(crate) fn picker_analog_input_ok(&self) -> bool {
        let Some(inner) = self.gamepad_nav.kbm_picker_node else { return false; };
        if self.gamepad_nav.kbm_picker_touch_zones {
            // A Touch Zones / touchpad zone yields a continuous position, so
            // analog outputs (stick deflection, mouse-delta) apply. A Virtual
            // Menu zone instead triggers a DISCRETE selection, so analog outputs
            // make no sense — grey them out for menus only.
            let is_menu = self.picker_node(&self.gamepad_nav.kbm_picker_path.clone(), inner)
                .map(|n| n.module_id == "module.menu").unwrap_or(false);
            return !is_menu;
        }
        if self.gamepad_nav.kbm_picker_phase_key.is_some() {
            return true; // Lean: the gesture itself is analog.
        }
        self.picker_draft_vec(&self.gamepad_nav.kbm_picker_path.clone(), inner, "draft_input").iter()
            .any(|p| flexinput_engine::pin_is_analog_input(p))
    }

    /// Append one picked pin to the picker's draft chord (de-duped) and flip the
    /// target into the draft-visible phase. Shared by gamepad South and mouse
    /// click. Analog-only pins (swipe) are ignored unless the input is analog.
    pub(crate) fn picker_append_pin(&mut self, pin: &str) {
        // Gate analog-only outputs.
        let analog_only = matches!(pin,
            "touch_swipe_x" | "touch_swipe_y" | "scroll_x" | "scroll_y");
        if analog_only && !self.picker_analog_input_ok() { return; }
        let path = self.gamepad_nav.kbm_picker_path.clone();
        let Some(inner) = self.gamepad_nav.kbm_picker_node else { return; };
        let draft_key = self.gamepad_nav.kbm_picker_draft_key.clone();
        let phase_key = self.gamepad_nav.kbm_picker_phase_key.clone();
        let mut out = self.picker_draft_vec(&path, inner, &draft_key);
        if !out.iter().any(|p| p == pin) { out.push(pin.to_string()); }
        self.picker_set_draft(&path, inner, &draft_key, &out);
        // Swipe outputs require analog mode; the viewer's Add detects a swipe pin
        // in the draft and stamps `mode = "analog"` on the committed mapping.
        // The `ui_phase` flip only means something to the Remapper's own state
        // machine — a TZ/menu session (draft `_tz_draft_out`) must NOT get it.
        match phase_key.as_deref() {
            Some(pk) => self.picker_set_param_str(&path, inner, pk, "ready"),
            None if draft_key == "draft_output" => {
                self.picker_set_param_str(&path, inner, "ui_phase", "learning");
            }
            None => {}
        }
    }

    /// Ordered list of active action-button activation keys for the current
    /// phase. MUST match, in order and presence, the buttons the body renders
    /// (and the order it publishes their rects): Learn, Clear, Special, Add —
    /// each present only when the body shows it.
    pub(crate) fn nav_remap_action_items(&self, outer_id: egui_snarl::NodeId, inner: egui_snarl::NodeId)
        -> Vec<&'static str>
    {
        // Gyro Lean sections use a SEPARATE state model (`_lean_<side>_phase`
        // "idle"/"learning" + `_lean_<side>_draft`), and their body consumes
        // side-scoped `_nav_act_learn_<side>` / `_nav_act_add_<side>` flags.
        if let Some(side) = self.nav_lean_side(outer_id) {
            let phase = self.nav_lean_phase(outer_id, inner, side);
            let addable = matches!(phase.as_str(), "learning" | "ready");
            let draft = self.nav_lean_draft_len(outer_id, inner, side) > 0;
            // has_draft mirrors the body: any captured/picked output OR mid-learn.
            let has_draft = draft || matches!(phase.as_str(), "learning" | "ready");
            // Visual order (must match the body + published rects):
            // Learn (always), Special (always), Clear(has_draft),
            // Add ((learning||ready) && draft non-empty).
            let mut v = vec![if side == "left" { "_nav_act_learn_left" } else { "_nav_act_learn_right" }];
            v.push(if side == "left" { "_nav_act_special_left" } else { "_nav_act_special_right" });
            if has_draft {
                v.push(if side == "left" { "_nav_act_clear_left" } else { "_nav_act_clear_right" });
            }
            if addable && draft {
                v.push(if side == "left" { "_nav_act_add_left" } else { "_nav_act_add_right" });
            }
            return v;
        }

        let mid = self.nav_selected_module_id(outer_id);
        let phase = self.nav_remap_phase(outer_id, inner);
        let in_draft = self.nav_remap_draft_len(outer_id, inner, "draft_input") > 0;
        let out_draft = self.nav_remap_draft_len(outer_id, inner, "draft_output") > 0;
        let latched = matches!(phase.as_str(), "ready_to_learn" | "learning");
        let has_draft = in_draft || out_draft || latched;
        match mid.as_deref() {
            Some("module.remapper") => {
                // Visual order (must match the body + published rects):
                // Learn, Special(latched), Clear(has_draft), Add(out_draft&&latched).
                let mut v = vec!["_nav_act_learn"];
                if latched { v.push("_nav_act_special"); }
                if has_draft { v.push("_nav_act_clear"); }
                if out_draft && latched { v.push("_nav_act_add"); }
                v
            }
            // Map Action / Combiner: Learn, Clear(has_draft), Add(in_draft).
            _ => {
                let mut v = vec!["_nav_act_learn"];
                if has_draft { v.push("_nav_act_clear"); }
                if in_draft { v.push("_nav_act_add"); }
                v
            }
        }
    }

    /// If the selected element is a gyro Lean section, return its side
    /// (`"left"`/`"right"`).
    pub(crate) fn nav_lean_side(&self, outer_id: egui_snarl::NodeId) -> Option<&'static str> {
        match self.nav_selected_element(outer_id).as_ref().map(|(_, e)| e.as_str()) {
            Some("lean_left") => Some("left"),
            Some("lean_right") => Some("right"),
            _ => None,
        }
    }

    /// Read a Lean section's phase param (`_lean_<side>_phase`, default "idle").
    pub(crate) fn nav_lean_phase(&self, outer: egui_snarl::NodeId, inner: egui_snarl::NodeId, side: &str) -> String {
        let key = if side == "left" { "_lean_left_phase" } else { "_lean_right_phase" };
        self.get_subpatch_param_str(outer, inner, key).unwrap_or_else(|| "idle".to_string())
    }

    /// Length of a Lean section's capture draft (`_lean_<side>_draft`).
    pub(crate) fn nav_lean_draft_len(&self, outer: egui_snarl::NodeId, inner: egui_snarl::NodeId, side: &str) -> usize {
        let key = if side == "left" { "_lean_left_draft" } else { "_lean_right_draft" };
        let canvas = &self.tabs[self.active_tab].canvas;
        canvas.snarl.get_node(outer).and_then(|n| n.subpatch.as_ref())
            .and_then(|sp| sp.snarl.get_node(inner))
            .and_then(|node| node.params.get(key).and_then(|v| v.as_array()))
            .map(|a| a.len()).unwrap_or(0)
    }

    /// Publish the RemapScroll selection (action vs card) so the body glows the
    /// focused item. Action items glow their button rect (published by the body);
    /// cards glow via the existing card-rect publish.
    pub(crate) fn nav_publish_remap_selection(&self, ctx: &egui::Context,
        outer_id: egui_snarl::NodeId, inner: egui_snarl::NodeId)
    {
        let n_actions = self.nav_remap_action_items(outer_id, inner).len();
        let sel = self.gamepad_nav.card_index;
        let pass = ctx.cumulative_pass_nr();
        let scope = self.nav_remap_mappings_key(outer_id);
        let entered = matches!(self.gamepad_nav.edit_level,
            crate::gamepad_nav::EditLevel::RemapCard);
        // (selected_action_index or usize::MAX, card_index or usize::MAX, entered)
        let (act_sel, card_sel) = if entered {
            (usize::MAX, self.gamepad_nav.remap_card)
        } else if sel < n_actions {
            (sel, usize::MAX)
        } else {
            (usize::MAX, sel - n_actions)
        };
        ctx.data_mut(|d| {
            d.insert_temp(egui::Id::new(("gp_nav_remap_card", inner.0, scope)),
                (pass, card_sel, entered));
            d.insert_temp(egui::Id::new(("gp_nav_remap_action", inner.0, scope)),
                (pass, act_sel));
        });
        ctx.request_repaint();
    }

    /// Read the remapper-family node's `ui_phase` (default "capturing").
    pub(crate) fn nav_remap_phase(&self, outer_id: egui_snarl::NodeId, inner: egui_snarl::NodeId) -> String {
        self.get_subpatch_param_str(outer_id, inner, "ui_phase")
            .unwrap_or_else(|| "capturing".to_string())
    }
    /// Length of the captured input/output drafts.
    pub(crate) fn nav_remap_draft_len(&self, outer_id: egui_snarl::NodeId, inner: egui_snarl::NodeId, key: &str) -> usize {
        let canvas = &self.tabs[self.active_tab].canvas;
        canvas.snarl.get_node(outer_id).and_then(|n| n.subpatch.as_ref())
            .and_then(|sp| sp.snarl.get_node(inner))
            .and_then(|node| node.params.get(key).and_then(|v| v.as_array()))
            .map(|a| a.len()).unwrap_or(0)
    }


    /// Drive header-field editing of the entered mapping card (`RemapCard`):
    /// left/right move between the fields that APPLY for the current mode
    /// (press-mode / time-gap / hold / turbo — grayed-out ones skipped), up/down
    /// or South edit the focused field, North resets the card, East exits.
    /// Analog cards extend past the header onto the response-curve section:
    /// field 4 = show/hide graph, 5 = enter dot editing (when open), 6 = threshold
    /// toggle (when open + applicable).
    pub(crate) fn nav_drive_remap_card(
        &mut self,
        ctx: &egui::Context,
        outer_id: egui_snarl::NodeId,
        nav: &crate::gamepad_nav::NavInput,
        dt: f32,
        step_dir: Option<crate::gamepad_nav::NavDir>,
        rt_rising: bool,
        mag: f32,
    ) {
        use crate::gamepad_nav::{EditLevel, NavDir};
        // Where to pop back to on exit — RemapScroll for the Remapper, TzCards for
        // Touch Zones (set at entry). Lets this driver be shared by both.
        let ret = self.gamepad_nav.card_return_level;
        if nav.is_rising("btn_east") {
            self.gamepad_nav.edit_level = ret;
            if let Some(baseline) = self.gamepad_nav.edit_baseline.take() {
                self.tabs[self.active_tab].canvas.commit_undo_if_changed(*baseline);
            }
            return;
        }
        let Some(inner) = self.nav_selected_inner_node(outer_id) else {
            self.gamepad_nav.edit_level = ret;
            return;
        };
        let _ = inner;
        // Clamp the entered-card index to the live card range so a stale index
        // (count shrank, e.g. a card was deleted) edits a valid card instead of
        // silently bailing to RemapScroll (the "modes/toggles don't work" bug).
        let count = self.nav_remap_card_count(outer_id);
        if count == 0 {
            self.gamepad_nav.edit_level = ret;
            return;
        }
        if self.gamepad_nav.remap_card >= count {
            self.gamepad_nav.remap_card = count - 1;
        }
        let idx = self.gamepad_nav.remap_card;
        // North resets the entered card's press mode + params to default.
        if nav.is_rising("btn_north") {
            self.nav_remap_reset_card(outer_id, idx);
        }
        // Header fields, in display order: 0=press-mode, 1=time-gap, 2=hold,
        // 3=turbo. Compute which apply for the current mode (mirror the card
        // renderer's gray-out rules) so we can skip the inert ones.
        let Some(mode) = self.nav_remap_card_mode(outer_id, idx) else {
            self.gamepad_nav.edit_level = ret;
            return;
        };
        let turbo_on = self.nav_remap_card_bool(outer_id, idx, "turbo");
        let gap_applies = matches!(mode.as_str(),
            "short"|"long"|"double"|"analog"|"on_press"|"on_release") || turbo_on;
        let hold_applies = mode == "long" || mode == "analog";
        let turbo_applies = !matches!(mode.as_str(), "short"|"double"|"on_press"|"on_release");
        // Field 0 (press-mode) always applies.
        let applies = [true, gap_applies, hold_applies, turbo_applies];

        // Response-curve section fields extend past the header for analog cards
        // (and, for Touch Zones, swipe-direction cards): 4 = show/hide graph, 5 =
        // enter dot editing (only when shown), 6 = threshold toggle (only when
        // shown + applicable). Touch Zones cards reach their curve/threshold the
        // SAME way as the Remapper — this shared driver, not a separate flow.
        let scope = self.nav_remap_mappings_key(outer_id);
        let is_tz = matches!(ret, EditLevel::TzCards);
        let curve_shape = self.nav_card_curve_shape(outer_id, idx);
        let curve_open = curve_shape.is_some()
            && self.nav_card_curve_open(ctx, inner, scope, idx);
        // Ordered list of reachable field ids for this card.
        let mut nav_fields: Vec<usize> = (0..4).filter(|&f| applies[f]).collect();
        if let Some(show_threshold) = curve_shape {
            nav_fields.push(4);
            if curve_open {
                nav_fields.push(5);
                if show_threshold { nav_fields.push(6); }
            }
        }
        // Field 7 (Touch Zones only): the per-card "Rel. center" (adaptive) slider,
        // shown for the same cards the body reveals it on — so it's finally gamepad-
        // reachable, changed with up/down like any other field.
        if is_tz && self.nav_tz_card_shows_adaptive(outer_id, inner, idx) {
            nav_fields.push(7);
        }

        // Left/right move within the reachable field list (wrapping).
        let move_dir = match step_dir {
            Some(NavDir::Left) => -1i32,
            Some(NavDir::Right) => 1,
            _ => 0,
        };
        let want = self.gamepad_nav.card_field;
        // Current position in the list; if the focused field is no longer
        // reachable (mode changed / section collapsed), snap to the nearest one.
        let cur_pos = nav_fields.iter().position(|&f| f == want)
            .unwrap_or_else(|| nav_fields.iter().filter(|&&f| f < want).count()
                .min(nav_fields.len().saturating_sub(1)));
        let pos = if move_dir != 0 && !nav_fields.is_empty() {
            ((cur_pos as i32 + move_dir).rem_euclid(nav_fields.len() as i32)) as usize
        } else {
            cur_pos
        };
        let field = nav_fields.get(pos).copied().unwrap_or(0);
        self.gamepad_nav.card_field = field;

        // Edit the focused field with up/down (and South for toggles / mode).
        let edit_press = match step_dir {
            Some(NavDir::Up) => 1i32,
            Some(NavDir::Down) => -1,
            _ => 0,
        };
        let south = nav.is_rising("btn_south") || rt_rising;
        match field {
            0 => {
                // Press mode: South OPENS the press-mode picker (a modal list of
                // the available modes with glyphs + labels) so the user can see
                // what each option does. Up/down still nudge the mode inline as a
                // quick alternative.
                if south {
                    let cur = self.nav_remap_card_mode(outer_id, idx)
                        .unwrap_or_else(|| "down".to_string());
                    self.gamepad_nav.press_mode_open = true;
                    self.gamepad_nav.press_mode_card = idx;
                    self.gamepad_nav.press_mode_outer = Some(outer_id);
                    self.gamepad_nav.press_mode_idx =
                        Self::PRESS_MODES.iter().position(|m| *m == cur).unwrap_or(0);
                } else if edit_press != 0 {
                    self.nav_remap_cycle_mode(outer_id, idx, edit_press);
                }
            }
            1 => {
                // Time gap (window_ms): decade-ish nudge, 10..5000.
                let mut delta = 0.0f32;
                let fine = self.gamepad_nav.fine_increment;
                if nav.is_rising("btn_west") {
                    self.gamepad_nav.fine_increment = !self.gamepad_nav.fine_increment;
                }
                let step = if fine { 1.0 } else { 5.0 };
                // Discrete dpad step OR continuous stick — not both, so the two
                // don't double-count / fight. Up = increase for both. In this
                // codebase up-stick is +y (see gamepad_nav::stick_dir), so use
                // +lstick.y directly (the prior `-y` inverted the stick relative
                // to the dpad).
                if mag > 0.5 {
                    let accel = self.settings.cursor_accel.max(1.0);
                    let c = nav.lstick.y;
                    delta += c.signum() * c.abs().powf(accel) * step * 40.0 * dt;
                } else if edit_press != 0 {
                    delta += edit_press as f32 * step;
                }
                if delta != 0.0 {
                    self.nav_remap_nudge_window(outer_id, idx, delta);
                }
            }
            2 => {
                if south || edit_press != 0 {
                    let cur = self.nav_remap_card_bool(outer_id, idx, "sustain");
                    let next = if south { !cur } else { edit_press > 0 };
                    self.nav_remap_set_card_bool(outer_id, idx, "sustain", next);
                }
            }
            3 => {
                if south || edit_press != 0 {
                    let cur = self.nav_remap_card_bool(outer_id, idx, "turbo");
                    let next = if south { !cur } else { edit_press > 0 };
                    self.nav_remap_set_card_bool(outer_id, idx, "turbo", next);
                }
            }
            4 => {
                // Show/hide the response-curve graph.
                if south || edit_press != 0 {
                    let now = self.nav_card_curve_open(ctx, inner, scope, idx);
                    let next = if south { !now } else { edit_press > 0 };
                    self.nav_set_card_curve_open(ctx, inner, scope, idx, next);
                }
            }
            5 => {
                // Focused on the graph: publish the curve-focus flag so the body
                // rings the graph (and keeps the section open); South enters the
                // shared dot editor (returns here on exit).
                let fpass = ctx.cumulative_pass_nr();
                ctx.data_mut(|d| d.insert_temp(
                    egui::Id::new(("gp_nav_tz_curve_focus", inner.0)), fpass));
                if south {
                    self.gamepad_nav.edit_level = EditLevel::CurveDots;
                    self.gamepad_nav.curve_return_level = EditLevel::RemapCard;
                    self.gamepad_nav.curve_dot = 0;
                    self.gamepad_nav.curve_sustain_focus = false;
                    if self.gamepad_nav.edit_baseline.is_none() {
                        self.gamepad_nav.edit_baseline = Some(Box::new(
                            self.tabs[self.active_tab].canvas.snapshot_for_undo()));
                    }
                }
            }
            6 => {
                // South toggles the activation threshold on/off; up/down + stick-Y
                // nudge its value (only while present), so it's fully gamepad-editable.
                if south {
                    let base = self.tabs[self.active_tab].canvas.snapshot_for_undo();
                    if self.nav_toggle_card_threshold(outer_id, idx) {
                        self.tabs[self.active_tab].canvas.commit_undo_if_changed(base);
                    }
                } else {
                    let mut delta = 0.0f32;
                    if mag > 0.5 {
                        let accel = self.settings.cursor_accel.max(1.0);
                        let c = nav.lstick.y;
                        delta += c.signum() * c.abs().powf(accel) * 0.6 * dt;
                    } else if edit_press != 0 {
                        delta += edit_press as f32 * 0.02;
                    }
                    if delta != 0.0 {
                        self.nav_nudge_card_threshold(outer_id, idx, delta);
                    }
                }
            }
            7 => {
                // Touch Zones "Rel. center" (adaptive): up/down + stick-Y nudge 0..1.
                let mut delta = 0.0f32;
                if mag > 0.5 {
                    let accel = self.settings.cursor_accel.max(1.0);
                    let c = nav.lstick.y;
                    delta += c.signum() * c.abs().powf(accel) * 0.6 * dt;
                } else if edit_press != 0 {
                    delta += edit_press as f32 * 0.05;
                }
                if delta != 0.0 {
                    self.nav_tz_nudge_adaptive(outer_id, idx, delta);
                }
            }
            _ => {}
        }

        // Publish selected card + focused field so the body glows them.
        let pass = ctx.cumulative_pass_nr();
        ctx.data_mut(|d| {
            d.insert_temp(egui::Id::new(("gp_nav_remap_card", inner.0, scope)), (pass, idx, true));
            d.insert_temp(egui::Id::new(("gp_nav_remap_card_field", inner.0, scope)), (pass, field as u64));
        });
        ctx.request_repaint();
    }
}
