//! Gamepad-nav driving of generic widget fields: the unified multi-field
//! editor, the element/param accessors it edits through, and the cursor
//! hit-testing that maps a pointer position onto a pinned item.

use super::*;

impl FlexInputApp {

    /// Unified multi-field editor. Left/right move the focused field; up/down +
    /// stick-X edit the focused field (Value = nudge, Enum/EnumPair = cycle,
    /// Toggle = on South or up/down). West=fine, North=reset focused field.
    /// Publishes the focus index + a small HUD label so the user sees what's
    /// targeted (since the underlying body rows aren't gamepad-aware).
    pub(crate) fn nav_drive_fields(
        &mut self,
        ctx: &egui::Context,
        outer_id: egui_snarl::NodeId,
        nav: &crate::gamepad_nav::NavInput,
        dt: f32,
        step_dir: Option<crate::gamepad_nav::NavDir>,
        rt_rising: bool,
        mag: f32,
    ) {
        use crate::gamepad_nav::NavDir;
        let fields = self.nav_element_fields(outer_id);
        if fields.is_empty() { return; }
        let n = fields.len();
        self.gamepad_nav.field_index = self.gamepad_nav.field_index.min(n - 1);

        // West → fine. North → reset focused field.
        if nav.is_rising("btn_west") {
            self.gamepad_nav.fine_increment = !self.gamepad_nav.fine_increment;
        }
        let fine = self.gamepad_nav.fine_increment;

        // Left/right: move field focus (when multiple). Up/down: edit value.
        // For single-field elements, left/right also edit (no focus to move).
        let multi = n > 1;
        let mut edit_press = 0i32; // -1/+1 from dpad up/down or stick
        if let Some(dir) = step_dir {
            match dir {
                NavDir::Left if multi => {
                    self.gamepad_nav.field_index = self.gamepad_nav.field_index.saturating_sub(1);
                }
                NavDir::Right if multi => {
                    self.gamepad_nav.field_index = (self.gamepad_nav.field_index + 1).min(n - 1);
                }
                NavDir::Up   | NavDir::Right => edit_press = 1,
                NavDir::Down | NavDir::Left  => edit_press = -1,
            }
        }
        let idx = self.gamepad_nav.field_index;
        let def = fields[idx].clone();
        let Some(inner) = self.nav_selected_inner_node(outer_id) else { return; };

        // North → reset focused field.
        if nav.is_rising("btn_north") {
            match &def.field {
                NavField::Value { key, default, .. } =>
                    self.set_subpatch_param_f32(outer_id, inner, key, *default),
                NavField::Enum { key, opts } =>
                    self.set_subpatch_param_str(outer_id, inner, key, opts[0]),
                NavField::Toggle { key } =>
                    self.set_subpatch_param_bool(outer_id, inner, key, false),
                NavField::EnumPair { key_a, key_b, opts } => {
                    self.set_subpatch_param_str(outer_id, inner, key_a, opts[0].1);
                    self.set_subpatch_param_str(outer_id, inner, key_b, opts[0].2);
                }
            }
        }

        match &def.field {
            NavField::Value { key, lo, hi, default, step } => {
                let press = edit_press as f32;
                let cont = if mag > 0.5 { nav.lstick.x } else { 0.0 };
                if press != 0.0 || cont != 0.0 {
                    self.nav_adjust_field_value(outer_id, inner, *key, *lo, *hi, *default, *step,
                        press, cont, fine, dt);
                }
            }
            NavField::Enum { key, opts } => {
                // South or up/down cycles; up/right = +1, down/left = -1.
                let dir = if nav.is_rising("btn_south") || rt_rising { 1 } else { edit_press };
                if dir != 0 {
                    let cur = self.get_subpatch_param_str(outer_id, inner, key)
                        .unwrap_or_else(|| opts[0].to_string());
                    let cu = opts.iter().position(|o| *o == cur).unwrap_or(0) as i32;
                    let next = opts[(cu + dir).rem_euclid(opts.len() as i32) as usize];
                    self.set_subpatch_param_str(outer_id, inner, key, next);
                }
            }
            NavField::Toggle { key } => {
                if nav.is_rising("btn_south") || rt_rising || edit_press != 0 {
                    let cur = self.get_subpatch_param_bool(outer_id, inner, key).unwrap_or(false);
                    // up/right → on, down/left → off; South toggles.
                    let next = if nav.is_rising("btn_south") || rt_rising { !cur }
                               else { edit_press > 0 };
                    self.set_subpatch_param_bool(outer_id, inner, key, next);
                }
            }
            NavField::EnumPair { key_a, key_b, opts } => {
                let dir = if nav.is_rising("btn_south") || rt_rising { 1 } else { edit_press };
                if dir != 0 {
                    let a = self.get_subpatch_param_str(outer_id, inner, key_a).unwrap_or_default();
                    let b = self.get_subpatch_param_str(outer_id, inner, key_b).unwrap_or_default();
                    let cu = opts.iter().position(|o| o.1 == a && o.2 == b).unwrap_or(0) as i32;
                    let nx = &opts[(cu + dir).rem_euclid(opts.len() as i32) as usize];
                    self.set_subpatch_param_str(outer_id, inner, key_a, nx.1);
                    self.set_subpatch_param_str(outer_id, inner, key_b, nx.2);
                }
            }
        }

        // Publish a focus HUD near the selected item so the user sees which
        // field is targeted + its label/value.
        self.nav_publish_field_hud(ctx, outer_id, inner, &fields, idx, fine);
    }

    /// Value-field nudge: decade or linear stepping (shared with the standalone
    /// numeric widgets), per-field bounds.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn nav_adjust_field_value(&mut self, outer_id: egui_snarl::NodeId, inner: egui_snarl::NodeId,
        key: &str, lo: f32, hi: f32, default: f32, step: NavStep,
        press: f32, cont: f32, fine: bool, dt: f32)
    {
        let cur = self.get_subpatch_param_f32(outer_id, inner, key).unwrap_or(default);
        let press_step = match step {
            NavStep::Decade => {
                let v = (cur - lo).abs().max(1e-6);
                let decade = 10f32.powf(v.log10().floor()).max(1.0);
                let coarse = if decade <= 1.0 { 1.0 } else { decade * 0.5 };
                if fine { coarse * 0.1 } else { coarse }
            }
            NavStep::Linear => {
                let span = (hi - lo).abs().max(f32::EPSILON);
                span * if fine { 0.005 } else { 0.02 }
            }
            NavStep::Fixed(coarse) => {
                if fine { coarse * 0.1 } else { coarse }
            }
        };
        let accel = self.settings.cursor_accel.max(1.0);
        let cont_curved = cont.signum() * cont.abs().clamp(0.0, 1.0).powf(accel);
        let cont_per_s = press_step * if fine { 8.0 } else { 25.0 };
        let mut delta = 0.0f32;
        if press != 0.0 { delta += press * press_step; }
        if cont != 0.0  { delta += cont_curved * cont_per_s * dt; }
        if delta == 0.0 { return; }
        // Whole-unit params read better rounded — but a small linear step (e.g.
        // grid 1..20 → step ~0.38) rounds back to the current value and looks
        // dead. For those, a discrete dpad/stick press must move at least one
        // whole unit in the press direction.
        let whole = matches!(key, "buf_size" | "grid_x" | "grid_y" | "trail_ms");
        let mut next = (cur + delta).clamp(lo, hi);
        if whole {
            next = next.round();
            if press != 0.0 && (next - cur.round()).abs() < 0.5 {
                next = (cur.round() + press.signum()).clamp(lo, hi);
            }
            // These renderers read the param with `as_i64()` and ignore a JSON
            // float, so whole-unit params MUST be stored as integers.
            self.set_subpatch_param_i64(outer_id, inner, key, next as i64);
            return;
        }
        self.set_subpatch_param_f32(outer_id, inner, key, next);
    }

    /// Outward-bloom ring on a focused sub-control's rect, drawn on the given
    /// context's Foreground layer. Shared by `nav_publish_field_hud` (main window)
    /// and the config overlay (`config_draw_field_glow`) so the value-field glow
    /// shows in BOTH viewports — the overlay is a separate window, so nav's own
    /// root-viewport draw never reaches it. No-op on a degenerate rect.
    pub(crate) fn paint_field_glow_ring(ctx: &egui::Context, fr: egui::Rect, accent: egui::Color32) {
        if !fr.is_finite() || fr.width() <= 0.5 { return; }
        let p = ctx.layer_painter(egui::LayerId::new(
            egui::Order::Foreground, egui::Id::new("gp_nav_field_glow")));
        crate::widgets::paint_nav_bloom(&p, fr, accent, 5.0, 150.0, 7.0, 6, 2.0, 1.5, 2.0);
    }

    /// Publish the multi-field focus HUD (pass-stamped) for the renderer overlay
    /// + a foreground text label so the user sees the focused field.
    pub(crate) fn nav_publish_field_hud(&self, ctx: &egui::Context, outer_id: egui_snarl::NodeId,
        inner: egui_snarl::NodeId, fields: &[NavFieldDef], idx: usize, fine: bool)
    {
        // While the config overlay is up it draws its OWN field glow + HUD in its
        // viewport (`draw_config_field_glow`); drawing here too would double the
        // ring as a ghost on the main window when it sits in front of the overlay.
        if crate::config_overlay::config_overlay_visible(ctx) {
            return;
        }
        let def = &fields[idx];
        // Read the focused field's current value as a string for the HUD.
        let val_str = match &def.field {
            NavField::Value { key, .. } =>
                self.get_subpatch_param_f32(outer_id, inner, key)
                    .map(|v| format!("{:.3}", v)).unwrap_or_default(),
            NavField::Enum { key, .. } | NavField::EnumPair { key_a: key, .. } =>
                self.get_subpatch_param_str(outer_id, inner, key).unwrap_or_default(),
            NavField::Toggle { key } =>
                if self.get_subpatch_param_bool(outer_id, inner, key).unwrap_or(false) { "ON".into() } else { "OFF".into() },
        };
        // Find the item's screen rect (published by render_subpatch_body) to
        // anchor the HUD just above it.
        let rects: Option<(u64, Vec<(usize, egui::Rect)>)> =
            ctx.data(|d| d.get_temp(egui::Id::new(("gp_nav_item_rects", outer_id.0))));
        let sel = {
            let canvas = &self.tabs[self.active_tab].canvas;
            canvas.snarl.get_node(outer_id).and_then(|n| n.subpatch.as_ref())
                .and_then(|sp| sp.selected_item)
        };
        let rect = rects.and_then(|(_, rs)| sel.and_then(|s| rs.iter().find(|(i,_)| *i == s).map(|(_,r)| *r)));
        let Some(rect) = rect else { return; };
        let accent = crate::widgets::NavHighlightStyle::of(ctx).accent;

        // Per-field inner glow: if the row renderer published per-control rects
        // this frame, draw an outward bloom ring on the focused field's rect so
        // the user sees WHICH sub-control is targeted (the HUD pill names it; the
        // ring points at it). Falls back silently to pill-only when a renderer
        // hasn't been instrumented.
        let element = self.nav_selected_element(outer_id)
            .map(|(_, e)| e).unwrap_or_default();
        let field_rects: Option<(u64, Vec<egui::Rect>)> =
            ctx.data(|d| d.get_temp(egui::Id::new(("gp_nav_field_rects", inner.0, element))));
        if let Some((_, frs)) = field_rects {
            if let Some(fr) = frs.get(idx) {
                Self::paint_field_glow_ring(ctx, *fr, accent);
            }
        }
        let painter = ctx.layer_painter(egui::LayerId::new(
            egui::Order::Foreground, egui::Id::new(("gp_nav_field_hud", outer_id.0))));
        let n = fields.len();
        let label = if n > 1 {
            format!("[{}/{}] {} = {}{}", idx + 1, n, def.label, val_str,
                if fine { "  (fine)" } else { "" })
        } else {
            format!("{} = {}{}", def.label, val_str, if fine { "  (fine)" } else { "" })
        };
        let pos = egui::pos2(rect.center().x, rect.top() - 6.0);
        // Background pill for legibility.
        let galley = painter.layout_no_wrap(label.clone(),
            egui::FontId::proportional(12.0), egui::Color32::WHITE);
        let pad = egui::vec2(6.0, 3.0);
        let bg = egui::Rect::from_center_size(
            egui::pos2(pos.x, pos.y - galley.size().y * 0.5),
            galley.size() + pad * 2.0);
        painter.rect_filled(bg, 4.0, egui::Color32::from_rgba_unmultiplied(20, 20, 24, 230));
        painter.rect_stroke(bg, 4.0, egui::Stroke::new(1.0, accent), egui::StrokeKind::Outside);
        painter.text(egui::pos2(pos.x, pos.y - galley.size().y * 0.5),
            egui::Align2::CENTER_CENTER, label, egui::FontId::proportional(12.0), egui::Color32::WHITE);
    }

    /// Read/write a String param on an inner sub-patch node.
    pub(crate) fn get_subpatch_param_str(&self, outer_id: egui_snarl::NodeId,
        inner: egui_snarl::NodeId, key: &str) -> Option<String>
    {
        let canvas = &self.tabs[self.active_tab].canvas;
        let sp = canvas.snarl.get_node(outer_id)?.subpatch.as_ref()?;
        sp.snarl.get_node(inner)?.params.get(key)?.as_str().map(|s| s.to_string())
    }
    pub(crate) fn set_subpatch_param_str(&mut self, outer_id: egui_snarl::NodeId,
        inner: egui_snarl::NodeId, key: &str, val: &str)
    {
        let canvas = &mut self.tabs[self.active_tab].canvas;
        let Some(sp) = canvas.snarl.get_node_mut(outer_id).and_then(|n| n.subpatch.as_mut()) else { return; };
        if let Some(node) = sp.snarl.get_node_mut(inner) {
            node.params.insert(key.to_string(), serde_json::Value::String(val.to_string()));
        }
    }
    pub(crate) fn set_subpatch_param_bool(&mut self, outer_id: egui_snarl::NodeId,
        inner: egui_snarl::NodeId, key: &str, val: bool)
    {
        let canvas = &mut self.tabs[self.active_tab].canvas;
        let Some(sp) = canvas.snarl.get_node_mut(outer_id).and_then(|n| n.subpatch.as_mut()) else { return; };
        if let Some(node) = sp.snarl.get_node_mut(inner) {
            node.params.insert(key.to_string(), serde_json::Value::Bool(val));
        }
    }
    /// Remove a param on a sub-patch inner node (mirrors the mouse handlers'
    /// `node.params.remove(k)` for capture-state keys the nav needs to clear).
    pub(crate) fn remove_subpatch_param(&mut self, outer_id: egui_snarl::NodeId,
        inner: egui_snarl::NodeId, key: &str)
    {
        let canvas = &mut self.tabs[self.active_tab].canvas;
        let Some(sp) = canvas.snarl.get_node_mut(outer_id).and_then(|n| n.subpatch.as_mut()) else { return; };
        if let Some(node) = sp.snarl.get_node_mut(inner) {
            node.params.remove(key);
        }
    }
    pub(crate) fn get_subpatch_param_bool(&self, outer_id: egui_snarl::NodeId,
        inner: egui_snarl::NodeId, key: &str) -> Option<bool>
    {
        let canvas = &self.tabs[self.active_tab].canvas;
        let sp = canvas.snarl.get_node(outer_id)?.subpatch.as_ref()?;
        sp.snarl.get_node(inner)?.params.get(key)?.as_bool()
    }

    /// Hit-test the RS/gyro cursor against the sub-patch item screen rects
    /// published by `render_subpatch_body` last frame. Returns the index of the
    /// topmost item under the cursor (last in paint order wins), or None.
    pub(crate) fn nav_cursor_hit_item(&self, ctx: &egui::Context, outer_id: egui_snarl::NodeId) -> Option<usize> {
        let cursor = self.gamepad_nav.cursor_pos;
        let rects: Option<(u64, Vec<(usize, egui::Rect)>)> =
            ctx.data(|d| d.get_temp(egui::Id::new(("gp_nav_item_rects", outer_id.0))));
        let (_pass, rects) = rects?;
        // Only consider items that are actually interactable — skip text titles,
        // graphs, svgs and other decorative elements the cursor passes over.
        let canvas = &self.tabs[self.active_tab].canvas;
        let sp = canvas.snarl.get_node(outer_id)?.subpatch.as_ref()?;
        rects
            .iter()
            .rev()
            .find(|(i, r)| r.contains(cursor) && Self::sp_item_is_editable(sp, *i))
            .map(|(i, _)| *i)
    }

    /// True if sub-patch item `idx` is a gamepad-editable widget (knob/constant/
    /// dropdown/switch/curve/remapper), not a decorative element. Only the
    /// `"curve"` element of a response curve is selectable (its scale/range/grid
    /// rows are separate, non-dot elements).
    pub(crate) fn sp_item_is_editable(
        sp: &crate::canvas::node::UiSubPatch,
        idx: usize,
    ) -> bool {
        use crate::canvas::node::LayoutItem;
        let Some(LayoutItem::Module(m)) = sp.items.get(idx) else { return false; };
        let inner = egui_snarl::NodeId(m.inner_node_id);
        let Some(mid) = sp.snarl.get_node(inner).map(|n| n.module_id.clone()) else { return false; };
        Self::elem_is_nav_target(&mid, &m.element_id)
    }

    /// Pure (module, element) test for "the cursor can target this and the nav
    /// driver has a handler for it". Single source of truth shared by the cursor
    /// hit-test and any other place that needs targetability without selection
    /// context. Mirrors the arms of `nav_selected_kind`.
    pub(crate) fn elem_is_nav_target(mid: &str, elem: &str) -> bool {
        match (mid, elem) {
            // Single-actuation widgets.
            ("module.knob", _) | ("module.constant", _)
            | ("module.dropdown", _) | ("module.switch", _) => true,
            // Curves: the dot graph plus every editable option row.
            ("module.response_curve", "curve")
            | ("module.vec_response_curve", "curve")
            | ("module.vec_reshape", "curve")
            | ("module.twoway_response_curve", "curve")
            // Envelope's ADSR graph shares the dot-editing path (+ a sustain line).
            | ("generator.envelope", "curve") => true,
            // ASTH scope's EQ is dot-editable via the shared curve-dot path.
            ("module.audio_stream_haptics", "asth_scope") => true,
            // Remapper-family mapping widgets (filter cycle + in-body capture).
            ("module.remapper", _) | ("module.map_action", _)
            | ("module.automap_combiner", _) => true,
            // Touch Zones AND Virtual Menu: "field" = grid/ring line editing,
            // "cards" = the zone-tab mapping list. The menu reuses TZ's storage
            // (col_edges/row_edges/tree/zone_maps), so both drive the same nav.
            // MUST mirror the corresponding arms in `nav_selected_kind`.
            ("module.touch_zones", "field") | ("module.touch_zones", "cards")
            | ("module.menu", "field") | ("module.menu", "cards") => true,
            ("processing.gyro_3dof", "lean_left")
            | ("processing.gyro_3dof", "lean_right") => true,
            // Everything else is a field row — targetable iff it has fields.
            _ => Self::elem_has_fields(mid, elem),
        }
    }

    /// Static mirror of `nav_element_fields`' coverage (pure (module, element)
    /// test) — used by the cursor hit-test, which has no selection context.
    /// MUST stay in sync with `nav_element_fields`.
    pub(crate) fn elem_has_fields(mid: &str, elem: &str) -> bool {
        matches!(
            (mid, elem),
            ("module.delay", "ms")
            | ("module.average", "samples") | ("module.average", "spike_mad")
            | ("module.dc_filter", "window_ms") | ("module.dc_filter", "decay_ms")
            | ("logic.delay", "time") | ("logic.delay", "mode")
            | ("generator.oscillator", "freq") | ("generator.oscillator", "phase")
            | ("generator.oscillator", "shape")
            | ("processing.gyro_3dof", "lean_threshold")
            | ("processing.gyro_3dof", "pointer_mode") | ("processing.gyro_3dof", "mode")
            | ("processing.gyro_3dof", "steering_mode")
            | ("processing.gyro_3dof", "steering_opts")
            | ("processing.gyro_3dof", "gyro_invert") | ("processing.gyro_3dof", "accel_invert")
            | ("logic.counter", "mode") | ("logic.counter", "range_mode")
            | ("logic.counter", "step") | ("logic.counter", "min_max")
            | ("module.selector", "mode") | ("module.selector", "range_mode")
            | ("module.selector", "step") | ("module.selector", "min_max")
            | ("module.response_curve", "scale_row") | ("module.response_curve", "range_row")
            | ("module.response_curve", "grid_row") | ("module.response_curve", "grid_options_row")
            | ("module.vec_response_curve", "scale_row") | ("module.vec_response_curve", "range_row")
            | ("module.vec_response_curve", "grid_row") | ("module.vec_response_curve", "grid_options_row")
            | ("module.vec_reshape", "target_row") | ("module.vec_reshape", "options_row")
            | ("module.vec_reshape", "range_row") | ("module.vec_reshape", "grid_row")
            | ("module.vec_reshape", "preset_row")
            | ("module.twoway_response_curve", "scale_row") | ("module.twoway_response_curve", "range_row")
            | ("module.twoway_response_curve", "grid_row") | ("module.twoway_response_curve", "grid_options_row")
            | ("module.twoway_response_curve", "hyst_row") | ("module.twoway_response_curve", "interp_row")
            | ("module.twoway_response_curve", "lane_toggle")
            | ("generator.envelope", "time_row") | ("generator.envelope", "sustain_row")
            | ("generator.envelope", "mode_row") | ("generator.envelope", "grid_row")
            | ("generator.envelope", "grid_options_row")
            | ("display.oscilloscope", "controls")
            | ("display.trigscope", "controls")
            | ("module.audio_stream_haptics", "asth_mode_row")
            | ("module.audio_stream_haptics", "asth_volume")
            | ("module.audio_stream_haptics", "asth_release")
            | ("module.audio_stream_haptics", "asth_crossover")
            | ("module.audio_stream_haptics", "asth_amplitude")
            | ("module.audio_stream_haptics", "asth_balance")
            | ("module.audio_stream_haptics", "asth_swap_row")
            | ("module.audio_stream_haptics", "asth_rumble_mix")
        )
    }

    /// Read an f32 param on an active-tab node (top-level snarl).
    pub(crate) fn get_node_param_f32(&self, node: egui_snarl::NodeId, key: &str) -> Option<f32> {
        let canvas = &self.tabs[self.active_tab].canvas;
        canvas.snarl.get_node(node)?.params.get(key)?.as_f64().map(|v| v as f32)
    }
    /// Write an f32 param on an active-tab node (top-level snarl).
    pub(crate) fn set_node_param_f32(&mut self, node: egui_snarl::NodeId, key: &str, val: f32) {
        let canvas = &mut self.tabs[self.active_tab].canvas;
        if let Some(n) = canvas.snarl.get_node_mut(node) {
            n.params.insert(key.to_string(), serde_json::Value::from(val as f64));
        }
    }
    pub(crate) fn get_node_param_bool(&self, node: egui_snarl::NodeId, key: &str) -> Option<bool> {
        let canvas = &self.tabs[self.active_tab].canvas;
        canvas.snarl.get_node(node)?.params.get(key)?.as_bool()
    }
    pub(crate) fn set_node_param_bool(&mut self, node: egui_snarl::NodeId, key: &str, val: bool) {
        let canvas = &mut self.tabs[self.active_tab].canvas;
        if let Some(n) = canvas.snarl.get_node_mut(node) {
            n.params.insert(key.to_string(), serde_json::Value::Bool(val));
        }
    }

    /// Config-overlay nav override (M3.6): when the config overlay is editing a
    /// tweak-pin, the shared nav resolvers point at THAT pin instead of the
    /// sub-patch's own `selected_item`, so the existing widget/curve drivers edit
    /// the config pin with identical UX. Returns `(inner node, module_id,
    /// element_id)` when the override is active for `outer_id`.
    fn nav_config_override(
        &self,
        outer_id: egui_snarl::NodeId,
    ) -> Option<(egui_snarl::NodeId, String, String)> {
        let (o, inner, elem) = self.gamepad_nav.config_nav_sel.as_ref()?;
        if *o != outer_id {
            return None;
        }
        let canvas = &self.tabs[self.active_tab].canvas;
        let sp = canvas.snarl.get_node(outer_id)?.subpatch.as_ref()?;
        let mid = sp.snarl.get_node(*inner)?.module_id.clone();
        Some((*inner, mid, elem.clone()))
    }

    /// Resolve the inner module id of the selected sub-patch item, if it's a
    /// Module item.
    #[allow(dead_code)]
    pub(crate) fn nav_selected_module_id(&self, outer_id: egui_snarl::NodeId) -> Option<String> {
        if let Some((_, mid, _)) = self.nav_config_override(outer_id) {
            return Some(mid);
        }
        let canvas = &self.tabs[self.active_tab].canvas;
        let sp = canvas.snarl.get_node(outer_id)?.subpatch.as_ref()?;
        let sel = sp.selected_item?;
        let item = sp.items.get(sel)?;
        if let crate::canvas::node::LayoutItem::Module(m) = item {
            let inner = egui_snarl::NodeId(m.inner_node_id);
            sp.snarl.get_node(inner).map(|n| n.module_id.clone())
        } else {
            None
        }
    }

    /// Resolve the inner node id of the selected sub-patch item, if it's a
    /// Module item.
    pub(crate) fn nav_selected_inner_node(&self, outer_id: egui_snarl::NodeId) -> Option<egui_snarl::NodeId> {
        if let Some((inner, _, _)) = self.nav_config_override(outer_id) {
            return Some(inner);
        }
        let canvas = &self.tabs[self.active_tab].canvas;
        let sp = canvas.snarl.get_node(outer_id)?.subpatch.as_ref()?;
        let sel = sp.selected_item?;
        if let crate::canvas::node::LayoutItem::Module(m) = sp.items.get(sel)? {
            Some(egui_snarl::NodeId(m.inner_node_id))
        } else {
            None
        }
    }

    /// (module_id, element_id) of the selected sub-patch item.
    pub(crate) fn nav_selected_element(&self, outer_id: egui_snarl::NodeId) -> Option<(String, String)> {
        if let Some((_, mid, elem)) = self.nav_config_override(outer_id) {
            return Some((mid, elem));
        }
        let canvas = &self.tabs[self.active_tab].canvas;
        let sp = canvas.snarl.get_node(outer_id)?.subpatch.as_ref()?;
        let sel = sp.selected_item?;
        if let crate::canvas::node::LayoutItem::Module(m) = sp.items.get(sel)? {
            let mid = sp.snarl.get_node(egui_snarl::NodeId(m.inner_node_id))?.module_id.clone();
            Some((mid, m.element_id.clone()))
        } else {
            None
        }
    }

    /// Numeric-edit descriptor for the selected element, if it edits a single
    /// scalar param: (param_key, lo, hi, step, default). `step` is the per-press
    /// nudge in param units; the continuous stick path scales off (hi-lo). This
    /// table is what lets a generic Value editor drive every numeric widget
    /// (delay, average, dc_filter, oscillator, logic thresholds, …) by its
    /// exposed `element_id`, not just knob/constant.
    pub(crate) fn nav_value_param(&self, outer_id: egui_snarl::NodeId) -> Option<NavParamSpec> {
        let (mid, elem) = self.nav_selected_element(outer_id)?;
        // Param keys + element_ids verified against each module body's pinned
        // dispatch (`render_dragvalue_param` / oscillator rows). Decade stepping
        // for time/sample counts (wide ranges); Linear for 0..1-ish params.
        use NavStep::*;
        let (key, lo, hi, default, step) = match (mid.as_str(), elem.as_str()) {
            ("module.delay", "ms")            => ("delay_ms",   0.0,  60_000.0, 100.0, Decade),
            ("module.average", "samples")     => ("buf_size",   1.0,  10_000.0, 10.0,  Decade),
            ("module.average", "spike_mad")   => ("spike_mad",  0.0,  20.0,     0.0,   Linear),
            ("module.dc_filter", "window_ms") => ("window_ms",  10.0, 60_000.0, 500.0, Decade),
            ("module.dc_filter", "decay_ms")  => ("decay_ms",   10.0, 60_000.0, 200.0, Decade),
            ("logic.delay", "time")           => ("time",       0.0,  60_000.0, 100.0, Decade),
            ("generator.oscillator", "freq")  => ("freq_param", 0.01, 200.0,    1.0,   Decade),
            ("generator.oscillator", "phase") => ("phase_param",0.0,  1.0,      0.0,   Linear),
            ("logic.counter", "step")         => ("step_param", 0.001,10_000.0, 1.0,   Decade),
            ("processing.gyro_3dof", "lean_threshold") => ("lean_threshold", 0.01, 4.0, 0.3, Linear),
            _ => return None,
        };
        Some(NavParamSpec { key, lo, hi, default, step })
    }

    /// Enum-cycle descriptor for a selected element backed by a String param
    /// with an ordered option set: (param_key, options). South/dpad cycles it.
    /// Superseded by the unified field editor; kept for reference.
    #[allow(dead_code)]
    pub(crate) fn nav_enum_spec(&self, outer_id: egui_snarl::NodeId)
        -> Option<(&'static str, &'static [&'static str])>
    {
        let (mid, elem) = self.nav_selected_element(outer_id)?;
        let d: (&'static str, &'static [&'static str]) = match (mid.as_str(), elem.as_str()) {
            ("generator.oscillator", "shape") =>
                ("shape", &["sine", "triangle", "saw", "square"]),
            ("logic.delay", "mode") =>
                ("mode", &["delay_true", "delay_false"]),
            ("logic.counter", "mode") =>
                ("mode", &["loop", "limit", "bounce", "unlimited"]),
            _ => return None,
        };
        Some(d)
    }

    /// Cycle the selected enum-string element by `dir` (+1/-1), wrapping.
    #[allow(dead_code)]
    pub(crate) fn nav_cycle_enum(&mut self, outer_id: egui_snarl::NodeId, dir: i32) {
        let Some((key, opts)) = self.nav_enum_spec(outer_id) else { return; };
        let Some(inner) = self.nav_selected_inner_node(outer_id) else { return; };
        let canvas = &mut self.tabs[self.active_tab].canvas;
        let Some(sp) = canvas.snarl.get_node_mut(outer_id).and_then(|n| n.subpatch.as_mut()) else { return; };
        let Some(node) = sp.snarl.get_node_mut(inner) else { return; };
        let cur = node.params.get(key).and_then(|v| v.as_str()).unwrap_or(opts[0]);
        let idx = opts.iter().position(|o| *o == cur).unwrap_or(0) as i32;
        let next = opts[(idx + dir).rem_euclid(opts.len() as i32) as usize];
        node.params.insert(key.to_string(), serde_json::Value::String(next.to_string()));
    }

    /// Master field table: every multi-control (and single-control) widget
    /// element maps to its ordered list of editable fields. The unified
    /// multi-field editor (`nav_drive_fields`) walks these. Returns empty for
    /// elements with no nav-editable fields. Verified against the pinned render
    /// functions' param keys.
    pub(crate) fn nav_element_fields(&self, outer_id: egui_snarl::NodeId) -> Vec<NavFieldDef> {
        use NavField::*;
        use NavStep::{Decade, Fixed, Linear};
        let Some((mid, elem)) = self.nav_selected_element(outer_id) else { return vec![]; };
        // Gyro axis options (family, axis) for the pointer/steering mode rows.
        const GYRO_PTR: &[(&str, &str, &str)] = &[
            ("Pitch+Yaw", "pointer", "pitch_yaw"),
            ("Pitch+Roll", "pointer", "pitch_roll"),
            ("Player", "pointer", "player"),
            ("World", "pointer", "world"),
        ];
        const GYRO_STEER: &[(&str, &str, &str)] = &[
            ("Pitch+Yaw", "steering", "pitch_yaw"),
            ("Pitch+Roll", "steering", "pitch_roll"),
            ("Player", "steering", "player"),
            ("World", "steering", "world"),
        ];
        let v = |key, lo, hi, default, step| NavField::Value { key, lo, hi, default, step };
        macro_rules! f { ($l:expr, $field:expr) => { NavFieldDef { label: $l, field: $field } }; }
        match (mid.as_str(), elem.as_str()) {
            // ── single-field elements (also driven by the unified editor) ──
            ("module.delay", "ms")            => vec![f!("ms", v("delay_ms",0.0,60_000.0,100.0,Decade))],
            ("module.average", "samples")     => vec![f!("Samples", v("buf_size",1.0,10_000.0,10.0,Decade))],
            ("module.average", "spike_mad")   => vec![f!("Spike MAD", v("spike_mad",0.0,20.0,0.0,Linear))],
            ("module.dc_filter", "window_ms") => vec![f!("Window ms", v("window_ms",10.0,60_000.0,500.0,Decade))],
            ("module.dc_filter", "decay_ms")  => vec![f!("Decay ms", v("decay_ms",10.0,60_000.0,200.0,Decade))],
            ("logic.delay", "time")           => vec![f!("Time", v("time",0.0,60_000.0,100.0,Decade))],
            ("logic.delay", "mode")           => vec![f!("Mode", Enum{key:"mode",opts:&["delay_true","delay_false"]})],
            ("generator.oscillator", "freq")  => vec![f!("Freq", v("freq_param",0.01,200.0,1.0,Decade))],
            ("generator.oscillator", "phase") => vec![f!("Phase", v("phase_param",0.0,1.0,0.0,Linear))],
            ("generator.oscillator", "shape") => vec![f!("Shape", Enum{key:"shape",opts:&["sine","triangle","saw","square"]})],
            ("processing.gyro_3dof", "lean_threshold") => vec![f!("Lean", v("lean_threshold",0.01,4.0,0.3,Linear))],
            // ── multi-field rows ──
            ("logic.counter", "mode") => vec![f!("Mode", Enum{key:"mode",opts:&["loop","limit","bounce","unlimited"]})],
            ("logic.counter", "range_mode") => vec![f!("Normalized", Toggle{key:"normalized"})],
            ("logic.counter", "step") => vec![f!("Step", v("step_param",0.001,10_000.0,1.0,Decade))],
            ("logic.counter", "min_max") => vec![
                f!("Min", v("min_param",-1_000_000.0,1_000_000.0,0.0,Linear)),
                f!("Max", v("max_param",-1_000_000.0,1_000_000.0,10.0,Linear)),
            ],
            ("processing.gyro_3dof", "pointer_mode") | ("processing.gyro_3dof", "mode") =>
                vec![f!("Pointer", EnumPair{key_a:"family",key_b:"axis",opts:GYRO_PTR})],
            ("processing.gyro_3dof", "steering_mode") =>
                vec![f!("Steering", EnumPair{key_a:"family",key_b:"axis",opts:GYRO_STEER})],
            ("processing.gyro_3dof", "steering_opts") => vec![
                f!("excl. Y", Toggle{key:"steering_exclude_y"}),
                f!("re-center", v("recenter_strength",0.0,4.0,0.0,Linear)),
                f!("ease", v("reset_ease_in",0.0,2.0,0.25,Linear)),
            ],
            ("processing.gyro_3dof", "gyro_invert") => vec![
                f!("yaw", Toggle{key:"inv_yaw"}),
                f!("pitch", Toggle{key:"inv_pitch"}),
                f!("roll", Toggle{key:"inv_roll"}),
            ],
            ("processing.gyro_3dof", "accel_invert") => vec![
                f!("accX", Toggle{key:"inv_accel_x"}),
                f!("accY", Toggle{key:"inv_accel_y"}),
                f!("accZ", Toggle{key:"inv_accel_z"}),
            ],
            // ── curve option rows ──
            ("module.response_curve", "scale_row") | ("module.twoway_response_curve", "scale_row") => vec![
                f!("Log/Exp", v("scale_t",-1.0,1.0,0.0,Linear)),
                f!("Abs", Toggle{key:"absolute"}),
                f!("Snap", Toggle{key:"snap"}),
            ],
            ("module.vec_response_curve", "scale_row") => vec![
                f!("Log/Exp", v("scale_t",-1.0,1.0,0.0,Linear)),
                f!("Snap", Toggle{key:"snap"}),
            ],
            // Bounds are ±100 to allow extremes, but tuning happens near ±1 — use a
            // fixed 0.1 step (fine 0.01) so dpad/stick can fine-tune, not span-scaled.
            ("module.response_curve", "range_row") | ("module.twoway_response_curve", "range_row") => vec![
                f!("In↓", v("in_min",-100.0,100.0,-1.0,Fixed(0.1))),
                f!("In↑", v("in_max",-100.0,100.0,1.0,Fixed(0.1))),
                f!("Out↓", v("out_min",-100.0,100.0,-1.0,Fixed(0.1))),
                f!("Out↑", v("out_max",-100.0,100.0,1.0,Fixed(0.1))),
            ],
            ("module.vec_response_curve", "range_row") => vec![
                f!("In max", v("in_max",-100.0,100.0,1.0,Fixed(0.1))),
                f!("Out max", v("out_max",-100.0,100.0,1.0,Fixed(0.1))),
            ],
            // ── Vec Reshaper option rows ──
            ("module.vec_reshape", "target_row") => vec![
                f!("Edit", Enum{key:"edit_target",opts:&["gain","boundary"]}),
            ],
            ("module.vec_reshape", "options_row") => vec![
                f!("Symmetry", Enum{key:"symmetry",opts:&["quad4","xmirror"]}),
                f!("Renorm", Toggle{key:"renorm"}),
            ],
            ("module.vec_reshape", "range_row") => vec![
                f!("In max", v("in_max",0.05,2.0,1.0,Linear)),
                f!("Out max", v("out_max",0.05,2.0,1.0,Linear)),
            ],
            ("module.vec_reshape", "grid_row") => vec![
                f!("Grid H", v("grid_x",1.0,16.0,4.0,Linear)),
                f!("Grid V", v("grid_y",1.0,16.0,4.0,Linear)),
                f!("Snap", Toggle{key:"snap"}),
                f!("Trail ms", v("trail_ms",0.0,1000.0,300.0,Decade)),
            ],
            ("module.response_curve", "grid_row") | ("module.vec_response_curve", "grid_row")
            | ("module.twoway_response_curve", "grid_row") => vec![
                f!("Grid H", v("grid_x",1.0,20.0,4.0,Linear)),
                f!("Grid V", v("grid_y",1.0,20.0,4.0,Linear)),
                f!("Trail ms", v("trail_ms",0.0,1000.0,300.0,Decade)),
            ],
            ("module.response_curve", "grid_options_row") | ("module.vec_response_curve", "grid_options_row")
            | ("module.twoway_response_curve", "grid_options_row") => vec![
                f!("Scale grid", Toggle{key:"show_scaled_grid"}),
                f!("Labels", Toggle{key:"show_grid_labels"}),
            ],
            // ── Envelope Generator option rows ──
            // Field order MUST match render_envelope_time_row's published rects:
            // [unit segment, value box] left→right.
            ("generator.envelope", "time_row") => vec![
                f!("Base", Enum{key:"timebase", opts:&["ms","s","hz"]}),
                f!("Time", v("time_mul", 0.01, 60_000.0, 500.0, Decade)),
            ],
            ("generator.envelope", "sustain_row") => vec![
                f!("Sustain", v("sustain", 0.0, 1.0, 0.3, Fixed(0.02))),
            ],
            ("generator.envelope", "mode_row") => vec![
                f!("Hold", Toggle{key:"hold"}),
                f!("Bounce", Toggle{key:"bounce"}),
                f!("Loop", Toggle{key:"loop"}),
            ],
            ("generator.envelope", "grid_row") => vec![
                f!("Grid H", v("grid_x",1.0,20.0,4.0,Linear)),
                f!("Grid V", v("grid_y",1.0,20.0,4.0,Linear)),
                f!("Snap", Toggle{key:"snap"}),
            ],
            ("generator.envelope", "grid_options_row") => vec![
                f!("Grid", Toggle{key:"show_grid"}),
                f!("Labels", Toggle{key:"show_grid_labels"}),
            ],
            // ── Trigger scope controls (mirror the oscilloscope controls row) ──
            ("display.trigscope", "controls") => vec![
                f!("Win ms", v("ts_win_ms",10.0,10_000.0,200.0,Decade)),
                f!("Scale", v("ts_scale",0.001,100.0,1.0,Decade)),
                f!("Auto", Toggle{key:"ts_auto"}),
                f!("Uni", Toggle{key:"ts_uni"}),
            ],
            ("module.twoway_response_curve", "hyst_row") => vec![
                f!("Hyst %", v("hysteresis_pct",0.001,10.0,0.5,Linear)),
                f!("Hyst ms", v("hysteresis_ms",0.02,50.0,20.0,Linear)),
            ],
            ("module.twoway_response_curve", "interp_row") => vec![
                f!("Interp ms", v("interp_ms",0.0,500.0,50.0,Decade)),
            ],
            ("module.twoway_response_curve", "lane_toggle") => vec![
                f!("Lane", Enum{key:"active_lane",opts:&["up","dn"]}),
            ],
            // Oscilloscope controls row: Win (log ms) / Scale / Auto / Bi-Uni.
            ("display.oscilloscope", "controls") => vec![
                f!("Win ms", v("osc_win_ms",10.0,10_000.0,200.0,Decade)),
                f!("Scale", v("osc_scale",0.001,100.0,1.0,Decade)),
                f!("Auto", Toggle{key:"osc_auto"}),
                f!("Uni", Toggle{key:"osc_uni"}),
            ],
            // Audio Stream Haptics calibration rows + mode block. Ranges/defaults
            // mirror the sliders in viewer.rs `asth_draw_row` / `asth_draw_mode_block`.
            ("module.audio_stream_haptics", "asth_mode_row") => vec![
                f!("Capture", Enum{key:"asth_mode",opts:&["process","focused","system"]}),
                f!("Children", Toggle{key:"asth_include_tree"}),
            ],
            ("module.audio_stream_haptics", "asth_volume") =>
                vec![f!("Volume", v("asth_volume",0.0,2.0,1.0,Linear))],
            ("module.audio_stream_haptics", "asth_release") =>
                vec![f!("Release", v("asth_release",1.0,500.0,30.0,Decade))],
            ("module.audio_stream_haptics", "asth_crossover") =>
                vec![f!("Crossover", v("asth_crossover",60.0,800.0,250.0,Decade))],
            ("module.audio_stream_haptics", "asth_amplitude") => vec![
                f!("Floor", v("asth_amp_min",0.0,1.0,0.0,Linear)),
                f!("Ceiling", v("asth_amp_max",0.0,1.0,1.0,Linear)),
                f!("Curve", v("asth_curve",0.3,3.0,1.0,Linear)),
            ],
            ("module.audio_stream_haptics", "asth_balance") =>
                vec![f!("Balance", v("asth_freq_bias",-1.0,1.0,0.0,Linear))],
            ("module.audio_stream_haptics", "asth_swap_row") =>
                vec![f!("Swap", Toggle{key:"asth_swap"})],
            ("module.audio_stream_haptics", "asth_rumble_mix") =>
                vec![f!("Rumble mix", v("asth_modulator",0.0,1.0,1.0,Linear))],
            // Selector mirrors counter's controls.
            ("module.selector", "mode") => vec![f!("Mode", Enum{key:"mode",opts:&["loop","limit","bounce","unlimited"]})],
            ("module.selector", "range_mode") => vec![f!("Normalized", Toggle{key:"normalized"})],
            ("module.selector", "step") => vec![f!("Step", v("step_param",0.001,10_000.0,1.0,Decade))],
            ("module.selector", "min_max") => vec![
                f!("Min", v("min_param",-1_000_000.0,1_000_000.0,0.0,Linear)),
                f!("Max", v("max_param",-1_000_000.0,1_000_000.0,10.0,Linear)),
            ],
            _ => vec![],
        }
    }

    /// Kind of gamepad interaction the selected widget supports.
    pub(crate) fn nav_selected_kind(&self, outer_id: egui_snarl::NodeId) -> NavWidgetKind {
        // The selected item's element_id: a curve module can be pinned as the
        // dot-graph ("curve") or as separate scale/range/grid rows — only the
        // graph element supports dot editing.
        let elem = if let Some((_, _, e)) = self.nav_config_override(outer_id) {
            Some(e)
        } else {
            let canvas = &self.tabs[self.active_tab].canvas;
            canvas.snarl.get_node(outer_id)
                .and_then(|n| n.subpatch.as_ref())
                .and_then(|sp| sp.selected_item.and_then(|i| sp.items.get(i)))
                .and_then(|it| match it {
                    crate::canvas::node::LayoutItem::Module(m) => Some(m.element_id.clone()),
                    _ => None,
                })
        };
        match self.nav_selected_module_id(outer_id).as_deref() {
            Some("module.knob") | Some("module.constant") => NavWidgetKind::Value,
            Some("module.dropdown") => NavWidgetKind::Dropdown,
            Some("module.switch") => NavWidgetKind::Toggle,
            Some("module.response_curve")
            | Some("module.vec_response_curve")
            | Some("module.vec_reshape")
            | Some("module.twoway_response_curve")
            | Some("generator.envelope")
                if elem.as_deref() == Some("curve") => NavWidgetKind::Curve,
            // Audio Stream Haptics scope: its EQ points are dot-editable exactly
            // like a response curve (shared curve-dot nav path).
            Some("module.audio_stream_haptics")
                if elem.as_deref() == Some("asth_scope") => NavWidgetKind::Curve,
            Some("module.remapper") | Some("module.map_action")
            | Some("module.automap_combiner") => NavWidgetKind::Remapper,
            // Touch Zones pad "field" element = the grid → line editing; the
            // "cards" element = the mapping list → zone-tab + Learn/Assign flow.
            // Virtual Menu shares both: its field (grid OR radial ring) and its
            // zone-tab cards drive the same TZ nav over the same storage.
            Some("module.touch_zones") | Some("module.menu")
                if elem.as_deref() == Some("field") => NavWidgetKind::TouchZones,
            Some("module.touch_zones") | Some("module.menu")
                if elem.as_deref() == Some("cards") => NavWidgetKind::TouchZoneCards,
            // Gyro lean sections are remapper-family mapping rows (Learn/capture +
            // filter), unlike gyro's other elements which are plain field rows.
            Some("processing.gyro_3dof")
                if matches!(elem.as_deref(), Some("lean_left") | Some("lean_right"))
                => NavWidgetKind::Remapper,
            // Everything else with a field definition (single- or multi-control)
            // routes through the unified multi-field editor.
            _ if !self.nav_element_fields(outer_id).is_empty() => NavWidgetKind::MultiField,
            _ => NavWidgetKind::None,
        }
    }

    /// Adjust the selected widget by a directional `delta`. For value widgets
    /// (knob/constant) this is a continuous nudge; for dropdowns it cycles the
    /// selection by sign(delta).
    pub(crate) fn nav_adjust_selected(&mut self, outer_id: egui_snarl::NodeId, delta: f32) {
        // `delta` arrives normalized to a 0..1-style range by the caller. For
        // generic params we rescale it to the param's own (hi-lo) span so the
        // feel is consistent regardless of units (ms, samples, Hz, …).
        let generic = self.nav_value_param(outer_id);
        let Some(inner) = self.nav_selected_inner_node(outer_id) else { return; };
        if let Some(spec) = generic {
            let span = (spec.hi - spec.lo).abs().max(f32::EPSILON);
            let cur = self.get_subpatch_param_f32(outer_id, inner, spec.key).unwrap_or(spec.default);
            let next = (cur + delta * span).clamp(spec.lo, spec.hi);
            self.set_subpatch_param_f32(outer_id, inner, spec.key, next);
            return;
        }
        let canvas = &mut self.tabs[self.active_tab].canvas;
        let Some(sp) = canvas.snarl.get_node_mut(outer_id).and_then(|n| n.subpatch.as_mut()) else { return; };
        let Some(node) = sp.snarl.get_node_mut(inner) else { return; };
        match node.module_id.as_str() {
            "module.knob" => {
                let bipolar = node.params.get("bipolar").and_then(|v| v.as_bool()).unwrap_or(false);
                let (lo, hi) = if bipolar { (-1.0f32, 1.0f32) } else { (0.0f32, 1.0f32) };
                let cur = node.params.get("value").and_then(|v| v.as_f64()).unwrap_or(0.0) as f32;
                let next = (cur + delta).clamp(lo, hi);
                node.params.insert("value".to_string(), serde_json::Value::from(next as f64));
            }
            "module.constant" => {
                // Constants are unbounded; scale the nudge a bit larger.
                let cur = node.params.get("value").and_then(|v| v.as_f64()).unwrap_or(0.0) as f32;
                node.params.insert("value".to_string(),
                    serde_json::Value::from((cur + delta) as f64));
            }
            _ => {}
        }
    }

    /// Proportional (log-ish) adjust for a generic numeric widget. `press` is a
    /// per-frame discrete impulse (±1 from dpad rising), `cont` is the stick X
    /// (-1..1), `fine` halves the rates. The step scales with the value's own
    /// magnitude so wide ranges (0..60000) are usable: ~6%/press coarse,
    /// ~1.5%/press fine; continuous ~120%/s coarse, ~25%/s fine at full stick.
    /// A range-derived floor lets the value climb off exactly 0.
    #[allow(dead_code)]
    pub(crate) fn nav_adjust_generic(&mut self, outer_id: egui_snarl::NodeId,
        press: f32, cont: f32, fine: bool, dt: f32)
    {
        let Some(spec) = self.nav_value_param(outer_id) else { return; };
        let Some(inner) = self.nav_selected_inner_node(outer_id) else { return; };
        let cur = self.get_subpatch_param_f32(outer_id, inner, spec.key).unwrap_or(spec.default);
        let (lo, hi) = (spec.lo, spec.hi);

        // Per-press discrete step.
        let press_step = match spec.step {
            // Decade: step = magnitude's decade (1/10/100/…) × {1 coarse, 0.1 fine}.
            // <10 → 1 (fine 0.1); 10–100 → 5? — user wants 5 in the 10–100 band.
            // Use: decade d = 10^floor(log10(v)); coarse = d (with a 5× bump in
            // the [1,10)·d upper half? no — keep simple decade), fine = d/10.
            NavStep::Decade => {
                // Band by the value's decade: 1,10,100,… Coarse step per band:
                //   <10 → 1   |  10–100 → 5  |  100–1000 → 50  |  1000+ → 500 …
                // i.e. 1 in the first band, half-decade above. Fine = coarse/10
                // (so <10 fine = 0.1 ms sub-ms, 10–100 fine = 0.5, etc.).
                let v = (cur - lo).abs().max(1e-6);
                let decade = 10f32.powf(v.log10().floor()).max(1.0); // 1,10,100,…
                let coarse = if decade <= 1.0 { 1.0 } else { decade * 0.5 };
                if fine { coarse * 0.1 } else { coarse }
            }
            // Linear (phase etc.): fixed fraction of the 0..1-ish range.
            NavStep::Linear => {
                let span = (hi - lo).abs().max(f32::EPSILON);
                span * if fine { 0.005 } else { 0.02 }
            }
            NavStep::Fixed(coarse) => {
                if fine { coarse * 0.1 } else { coarse }
            }
        };

        // Continuous stick step: an accelerated curve on |cont| (gentle low,
        // fast at full deflection), scaled to the same step magnitude per second.
        let accel = self.settings.cursor_accel.max(1.0);
        let cont_curved = cont.signum() * cont.abs().clamp(0.0, 1.0).powf(accel);
        // At full deflection, ~ (press_step × steps_per_sec). Coarse ≈ 25/s of
        // the press step, fine ≈ 8/s.
        let cont_per_s = press_step * if fine { 8.0 } else { 25.0 };

        let mut delta = 0.0f32;
        if press != 0.0 { delta += press * press_step; }
        if cont != 0.0  { delta += cont_curved * cont_per_s * dt; }
        if delta == 0.0 { return; }

        let next = (cur + delta).clamp(lo, hi);
        // Decade params that represent whole units (samples) read better rounded;
        // ms/Hz tolerate fractions. Round sample counts to integers.
        let next = if spec.key == "buf_size" { next.round() } else { next };
        self.set_subpatch_param_f32(outer_id, inner, spec.key, next);
    }

    /// Read/write an f32 param on an INNER sub-patch node (inside outer_id's
    /// sub-patch snarl).
    pub(crate) fn get_subpatch_param_f32(&self, outer_id: egui_snarl::NodeId,
        inner: egui_snarl::NodeId, key: &str) -> Option<f32>
    {
        let canvas = &self.tabs[self.active_tab].canvas;
        let sp = canvas.snarl.get_node(outer_id)?.subpatch.as_ref()?;
        sp.snarl.get_node(inner)?.params.get(key)?.as_f64().map(|v| v as f32)
    }
    pub(crate) fn set_subpatch_param_f32(&mut self, outer_id: egui_snarl::NodeId,
        inner: egui_snarl::NodeId, key: &str, val: f32)
    {
        let canvas = &mut self.tabs[self.active_tab].canvas;
        let Some(sp) = canvas.snarl.get_node_mut(outer_id).and_then(|n| n.subpatch.as_mut()) else { return; };
        if let Some(node) = sp.snarl.get_node_mut(inner) {
            node.params.insert(key.to_string(), serde_json::Value::from(val as f64));
        }
    }
    /// Store an integer-valued param as a JSON integer (renderers that read with
    /// `as_i64()` ignore a JSON float).
    pub(crate) fn set_subpatch_param_i64(&mut self, outer_id: egui_snarl::NodeId,
        inner: egui_snarl::NodeId, key: &str, val: i64)
    {
        let canvas = &mut self.tabs[self.active_tab].canvas;
        let Some(sp) = canvas.snarl.get_node_mut(outer_id).and_then(|n| n.subpatch.as_mut()) else { return; };
        if let Some(node) = sp.snarl.get_node_mut(inner) {
            node.params.insert(key.to_string(), serde_json::Value::from(val));
        }
    }

    /// Open or close the selected dropdown's pinned popup (the real options
    /// list), matching the click-to-toggle popup the renderer manages in egui
    /// memory under `("dropdown_pinned_popup", inner_id.0)`.
    pub(crate) fn nav_set_dropdown_popup(&self, ctx: &egui::Context, outer_id: egui_snarl::NodeId, open: bool) {
        if !matches!(self.nav_selected_module_id(outer_id).as_deref(), Some("module.dropdown")) {
            return;
        }
        let Some(inner) = self.nav_selected_inner_node(outer_id) else { return; };
        let popup_id = egui::Id::new(("dropdown_pinned_popup", inner.0));
        let is_open = egui::Popup::is_id_open(ctx, popup_id);
        if open && !is_open { egui::Popup::open_id(ctx, popup_id); }
        else if !open && is_open { egui::Popup::close_id(ctx, popup_id); }
    }

    /// Cycle the selected dropdown by `dir` (+1/-1), wrapping. No-op otherwise.
    pub(crate) fn nav_cycle_dropdown(&mut self, outer_id: egui_snarl::NodeId, dir: i32) {
        let Some(inner) = self.nav_selected_inner_node(outer_id) else { return; };
        let canvas = &mut self.tabs[self.active_tab].canvas;
        let Some(sp) = canvas.snarl.get_node_mut(outer_id).and_then(|n| n.subpatch.as_mut()) else { return; };
        let Some(node) = sp.snarl.get_node_mut(inner) else { return; };
        if node.module_id != "module.dropdown" { return; }
        let n = node.params.get("options").and_then(|v| v.as_array()).map(|a| a.len()).unwrap_or(0);
        if n == 0 { return; }
        let cur = node.params.get("selected_index").and_then(|v| v.as_u64()).unwrap_or(0) as i32;
        let next = (cur + dir).rem_euclid(n as i32);
        node.params.insert("selected_index".to_string(), serde_json::Value::from(next as u64));
    }

    /// Toggle the selected switch's `active` state. Returns true if a switch was
    /// toggled (so the caller can record undo).
    pub(crate) fn nav_toggle_switch(&mut self, outer_id: egui_snarl::NodeId) -> bool {
        let Some(inner) = self.nav_selected_inner_node(outer_id) else { return false; };
        let canvas = &mut self.tabs[self.active_tab].canvas;
        let Some(sp) = canvas.snarl.get_node_mut(outer_id).and_then(|n| n.subpatch.as_mut()) else { return false; };
        let Some(node) = sp.snarl.get_node_mut(inner) else { return false; };
        if node.module_id != "module.switch" { return false; }
        // Current state must come from the engine's last emitted value when
        // present (it reconciles UI clicks with direct/latch inputs); the
        // persisted `active` is only a fallback. Toggling the stale param alone
        // gets overwritten by `last_out` next frame, so we also bump the
        // `ui_toggle_seq` the engine watches — exactly like a mouse click
        // (`switch_handle_click`).
        let cur = match node.extra.last_out.first() {
            Some(Some(Signal::Bool(b))) => *b,
            _ => node.params.get("active").and_then(|v| v.as_bool()).unwrap_or(false),
        };
        let seq = node.params.get("ui_toggle_seq").and_then(|v| v.as_u64()).unwrap_or(0);
        node.params.insert("ui_toggle_seq".to_string(), serde_json::Value::from(seq.wrapping_add(1)));
        node.params.insert("active".to_string(), serde_json::Value::Bool(!cur));
        true
    }


    /// Reset the selected widget's value to its default (0.0 for knob/constant;
    /// the descriptor default for generic numeric widgets).
    pub(crate) fn nav_reset_selected(&mut self, outer_id: egui_snarl::NodeId) {
        if let Some(spec) = self.nav_value_param(outer_id) {
            if let Some(inner) = self.nav_selected_inner_node(outer_id) {
                self.set_subpatch_param_f32(outer_id, inner, spec.key, spec.default);
            }
            return;
        }
        let Some(inner) = self.nav_selected_inner_node(outer_id) else { return; };
        let canvas = &mut self.tabs[self.active_tab].canvas;
        let Some(sp) = canvas.snarl.get_node_mut(outer_id).and_then(|n| n.subpatch.as_mut()) else { return; };
        let Some(node) = sp.snarl.get_node_mut(inner) else { return; };
        if matches!(node.module_id.as_str(), "module.knob" | "module.constant") {
            node.params.insert("value".to_string(), serde_json::Value::from(0.0f64));
        }
    }
}
