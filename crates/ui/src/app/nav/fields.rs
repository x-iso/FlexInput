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
        lt_rising: bool,
        mag: f32,
    ) {
        use crate::gamepad_nav::NavDir;
        // A pinned JSM editor is two panes sharing one dpad: the faders, and the
        // config text walked a token at a time. LB/RB switch between them, and
        // the text pane has its own driver from here on.
        if self.nav_is_jsm_editor(outer_id) {
            if nav.is_rising("btn_lb") || nav.is_rising("btn_rb") {
                self.gamepad_nav.jsm_pane = self.gamepad_nav.jsm_pane.other();
            }
            // LT/RT walk the editor's own config tabs. They are free here: in
            // this widget RT confirms nothing and LT's usual "back out" is
            // covered by East, which is the button people reach for anyway.
            let tab_step = (rt_rising as i32) - (lt_rising as i32);
            if tab_step != 0 {
                self.nav_cycle_jsm_tab(outer_id, tab_step);
            }
            if let Some(inner) = self.nav_selected_inner_node(outer_id) {
                crate::canvas::viewer::publish_jsm_pane(
                    ctx, inner, self.gamepad_nav.jsm_pane.label(),
                );
            }
            if self.gamepad_nav.jsm_pane == crate::gamepad_nav::JsmPane::Text {
                self.nav_drive_jsm_text(ctx, outer_id, nav, step_dir);
                return;
            }
        }
        let fields = self.nav_element_fields(outer_id);
        if fields.is_empty() { return; }
        let n = fields.len();
        self.gamepad_nav.field_index = self.gamepad_nav.field_index.min(n - 1);

        // West → fine. North → reset focused field.
        if nav.is_rising("btn_west") {
            self.gamepad_nav.fine_increment = !self.gamepad_nav.fine_increment;
        }
        let fine = self.gamepad_nav.fine_increment;

        // Which axis walks the fields follows how they are actually laid out. Most
        // pinned elements are one ROW of controls, so left/right walks and up/down
        // edits. A JSM editor is one COLUMN of faders, where that would have you
        // pressing left to move down — so there the axes swap. For single-field
        // elements either axis edits, since there is no focus to move.
        let column = self.nav_fields_are_a_column(outer_id);
        // When the focused setting hands the left stick to the game, that stick
        // can't walk the list either — pushing it would aim AND change which
        // setting you were about to adjust. The dpad still walks.
        let lstick_is_the_games = match &fields[self.gamepad_nav.field_index].field {
            NavField::JsmValue { name } => self.nav_tuning_takes_lstick(outer_id, name),
            _ => false,
        };
        let from_dpad = ["dpad_up", "dpad_down", "dpad_left", "dpad_right"]
            .iter()
            .any(|p| nav.is_rising(p));
        let multi = n > 1;
        let mut edit_press = 0i32; // -1/+1 from the dpad or the stick
        if let Some(dir) = step_dir {
            // Walking is the destructive one: slipping onto the next setting and
            // then editing THAT is the failure worth preventing, so a stick held
            // near a diagonal doesn't count as a step along the walking axis.
            let walk = |v: NavDir| matches!(
                (column, v),
                (true, NavDir::Up) | (true, NavDir::Down)
                    | (false, NavDir::Left) | (false, NavDir::Right)
            );
            let clear = Self::nav_axis_is_clear(nav.lstick, column)
                && (from_dpad || !lstick_is_the_games);
            match dir {
                d if multi && walk(d) && clear => {
                    let back = matches!(d, NavDir::Up | NavDir::Left);
                    self.gamepad_nav.field_index = if back {
                        self.gamepad_nav.field_index.saturating_sub(1)
                    } else {
                        (self.gamepad_nav.field_index + 1).min(n - 1)
                    };
                }
                // A walking direction that wasn't clear enough is dropped, not
                // turned into an edit — the pad was pointed between the two.
                d if multi && walk(d) => {}
                NavDir::Up | NavDir::Right => edit_press = 1,
                NavDir::Down | NavDir::Left => edit_press = -1,
            }
        }
        let idx = self.gamepad_nav.field_index;
        let def = fields[idx].clone();
        let Some(inner) = self.nav_selected_inner_node(outer_id) else { return; };
        // Tell the body which field has focus. The rects come the other way; this
        // is what lets a strip that scrolls bring the focused row into view, so a
        // selection can't sit below the fold with only a highlight ring — drawn
        // over the container, not inside it — to say where it went.
        if let Some((_, element)) = self.nav_selected_element(outer_id) {
            crate::canvas::viewer::publish_nav_focus_field(ctx, inner, &element, idx);
        }

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
                // Back to the value this setting held before the pad started
                // nudging it. See `GamepadNav::jsm_baseline` for why that, and
                // not JSM's own default.
                NavField::JsmValue { name } => self.nav_restore_jsm_baseline(outer_id, inner, name),
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
            NavField::JsmValue { name } => {
                self.nav_note_jsm_baseline(outer_id, name);
                let press = edit_press as f32;
                // The stick this setting hands to the game is not one nav may
                // use — you would be aiming and adjusting with the same thumb.
                let stick = self.nav_free_stick(outer_id, name, nav);
                // In a column the faders are walked with up/down, so the value is
                // driven by the stick's X — and vice versa, or holding the stick
                // to adjust would also be holding it to change focus.
                let cont = if stick.length() > 0.5 {
                    if column { stick.x } else { stick.y }
                } else {
                    0.0
                };
                if press != 0.0 || cont != 0.0 {
                    // A fraction of the setting's own range, so one rate suits a
                    // 0..1 deadzone and a 0..3600 unwind rate alike.
                    let scale = if fine { 0.25 } else { 1.0 };
                    let delta = (press * 0.02 + cont * 0.6 * dt) * scale;
                    self.nav_adjust_jsm_knob(outer_id, inner, name, delta);
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
        // A display-scaled field is nudged entirely in display units (steps +
        // grid snap), then written back in param units.
        let (factor, step) = match step {
            NavStep::FixedScaled { coarse, factor } => (factor.max(f32::EPSILON), NavStep::Fixed(coarse)),
            s => (1.0, s),
        };
        let cur = self.get_subpatch_param_f32(outer_id, inner, key).unwrap_or(default) * factor;
        let (lo, hi) = (lo * factor, hi * factor);
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
            NavStep::Fixed(coarse) | NavStep::FixedScaled { coarse, .. } => {
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

        // Fixed-step fields SNAP onto their step grid on a discrete dpad press,
        // so a value lands cleanly (RWS → 5.0, 6.0; fine → 5.1; FOV → whole
        // degrees) instead of drifting to 4.999 as a span-scaled linear step
        // would. Continuous stick adjustment stays smooth, and the mouse
        // DragValue still accepts any typed value for edge cases.
        if press != 0.0 {
            if let NavStep::Fixed(_) = step {
                let s = press_step.max(1e-6);
                let idx = cur / s;
                let on_grid = (idx.round() * s - cur).abs() < s * 0.25;
                let target = if on_grid {
                    idx.round() + press.signum()
                } else if press > 0.0 {
                    idx.ceil()
                } else {
                    idx.floor()
                };
                // Round to the step's own precision to shed float fuzz.
                let m = if s >= 1.0 { 1.0 } else { 10f32.powf((-s.log10()).ceil().max(0.0)) };
                next = (((target * s) * m).round() / m).clamp(lo, hi);
            }
        }
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
        self.set_subpatch_param_f32(outer_id, inner, key, next / factor);
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
            NavField::Value { key, step, .. } =>
                self.get_subpatch_param_f32(outer_id, inner, key)
                    .map(|v| match step {
                        // Display-scaled fields are large whole-unit readings.
                        NavStep::FixedScaled { factor, .. } => format!("{:.0}", v * factor),
                        _ => format!("{:.3}", v),
                    })
                    .unwrap_or_default(),
            NavField::Enum { key, .. } | NavField::EnumPair { key_a: key, .. } =>
                self.get_subpatch_param_str(outer_id, inner, key).unwrap_or_default(),
            NavField::Toggle { key } =>
                if self.get_subpatch_param_bool(outer_id, inner, key).unwrap_or(false) { "ON".into() } else { "OFF".into() },
            NavField::JsmValue { name } => self
                .nav_jsm_knobs(outer_id)
                .into_iter()
                .find(|k| k.name == *name)
                .map(|k| if k.integral {
                    format!("{}", k.value.round() as i64)
                } else {
                    format!("{:.2}", k.value)
                })
                .unwrap_or_default(),
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
            if let Some(fr) = frs.get(idx).filter(|r| Self::nav_ring_is_worth_drawing(r)) {
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
            // JSM Config: a pinned setting's fader is driven like a Knob. The
            // editor and the curve are not targets — there is nothing on either
            // a pad can usefully do, and a target you cannot act on just makes
            // the overlay harder to get through.
            // A fader pinned on its own is a Value widget; the whole editor is a
            // multi-field one you step through. The curve is neither — nothing a
            // pad can do there.
            ("module.jsm", e) => crate::canvas::viewer::jsm_knob_name_of(e).is_some()
                || e == "editor",
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
            ("module.jsm", "editor")
            | ("module.delay", "ms")
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
            | ("module.menu", "options")
            | ("processing.rws", "scale") | ("processing.rws", "rws")
            | ("processing.rws", "stick_dps") | ("processing.rws", "vh")
            | ("processing.rws", "measure")
            | ("processing.rws", "field") | ("processing.rws", "style")
            | ("processing.rws", "flick") | ("processing.rws", "suppress")
            | ("math.negate", "unipolar")
            | ("math.quantize", "factor") | ("math.quantize", "mode")
            | ("module.vec_to_deflection", "angle_unit")
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
            ("math.quantize", "factor")       => ("factor",     0.0,  10_000.0, 1.0,   Decade),
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
        use NavStep::{Decade, Fixed, FixedScaled, Linear};
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
        macro_rules! f { ($l:expr, $field:expr) => { NavFieldDef { label: $l.into(), field: $field } }; }
        // RWS Mouse Scale steps in the unit the header shows it in: 1 dot/°
        // (fine 0.1), or 100 dots/360° (fine 10).
        let rws_scale_step = || {
            let per_360 = self.nav_selected_inner_node(outer_id)
                .and_then(|inner| self.get_subpatch_param_str(outer_id, inner, "scale_unit"))
                .as_deref() == Some("360");
            if per_360 { FixedScaled { coarse: 100.0, factor: 360.0 } } else { Fixed(1.0) }
        };
        // JSM Config's editor pin: one field per numeric setting the config sets,
        // discovered from the TEXT each frame rather than from a table — which is
        // the whole point of the module, and means a setting added while the
        // overlay is open is navigable straight away.
        if mid == "module.jsm" && elem == "editor" {
            return self
                .nav_jsm_knobs(outer_id)
                .into_iter()
                .map(|k| NavFieldDef {
                    label: k.name.clone().into(),
                    field: NavField::JsmValue { name: k.name },
                })
                .collect();
        }
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
            // ── RWS Aim ──
            // The pinned ruler ("field") carries the full calibration control set
            // so it can be run AND stopped from the gamepad with only the ruler
            // pinned (mouse output is busy driving the game during calibration).
            // Fixed steps → clean stepping: coarse lands on integers, fine on the
            // 0.1 (or 1) grid. `scale` fine (0.1) matters for calibration; the
            // °/s fields step by 10 coarse / 1 fine (whole degrees per second).
            ("processing.rws", "scale") => vec![f!("Mouse Scale", v("scale",0.0,100_000.0,100.0,rws_scale_step()))],
            ("processing.rws", "rws")   => vec![f!("RWS", v("rws",0.01,50.0,1.0,Fixed(1.0)))],
            ("processing.rws", "stick_dps") => vec![f!("Stick °/s", v("stick_out_dps",1.0,100_000.0,360.0,Fixed(10.0)))],
            ("processing.rws", "vh") => vec![
                f!("Gyro V/H", v("gyro_vh_ratio",0.0,8.0,1.0,Fixed(0.1))),
                f!("Stick V/H", v("stick_vh_ratio",0.0,8.0,1.0,Fixed(0.1))),
            ],
            // Measure auto-cal. One nominal field so it classifies as MultiField
            // (South at the pin ENTERS it); the actual in-widget controls are
            // handled specially in `nav_drive_rws_measure` (◄► pick method, A
            // start / A finish, B back / B cancel) because South/East are far
            // clearer here than field-walking.
            ("processing.rws", "measure") => vec![
                f!("Auto-cal", Enum{key:"cal_measure",opts:&["off","pitch","yaw"]}),
            ],
            ("processing.rws", "field") => vec![
                f!("Mouse Scale", v("scale",0.0,100_000.0,100.0,rws_scale_step())),
                f!("RWS", v("rws",0.01,50.0,1.0,Fixed(1.0))),
            ],
            // FOV strictly whole degrees (Fixed(10) → 10 coarse / 1 fine).
            ("processing.rws", "style") => vec![
                f!("View", Enum{key:"field_mode",opts:&["ruler","room","both"]}),
                f!("FOV", v("field_fov",30.0,140.0,90.0,Fixed(10.0))),
                f!("BG", v("field_bg_alpha",0.0,1.0,0.0,Linear)),
                f!("Ticks°", v("field_tick_deg",5.0,90.0,15.0,Fixed(5.0))),
                f!("Labels", Toggle{key:"field_labels"}),
            ],
            ("processing.rws", "flick") => vec![
                f!("Flick", Toggle{key:"flick_enabled"}),
                f!("Deadzone", v("flick_deadzone",0.1,0.99,0.85,Linear)),
                f!("Smooth ms", v("flick_smooth_ms",0.0,500.0,100.0,Fixed(10.0))),
            ],
            ("processing.rws", "suppress") => vec![
                f!("Suppress", Enum{key:"suppress_source",opts:&["off","full","deadzone"]}),
            ],
            // ── multi-field rows ──
            ("logic.counter", "mode") => vec![f!("Mode", Enum{key:"mode",opts:&["loop","limit","bounce","unlimited"]})],
            ("math.negate", "unipolar") => vec![
                f!("Unipolar", Toggle{key:"unipolar"}),
                f!("Max", v("unipolar_max",0.0,1000.0,1.0,Linear)),
            ],
            ("math.quantize", "factor") => vec![f!("Factor", v("factor",0.0,10_000.0,1.0,Decade))],
            ("math.quantize", "mode")   =>
                vec![f!("Mode", Enum{key:"mode",opts:&["round","floor","ceil","trunc"]})],
            ("module.vec_to_deflection", "angle_unit") => vec![f!("Degrees", Toggle{key:"degrees"})],
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
            // ── Virtual Menu "options" element (the TWO-row session block) ──
            // Field order MUST match `show_menu_options_row`'s published rects,
            // INCLUDING the conditional touch/gyro sub-fields — both this arm and
            // the renderer gate on the SAME derived `src_touch`/`src_gyro` (with
            // the legacy `pointer_source` fallback), so the lists stay in lockstep
            // however the checkboxes are set. Enum option orders mirror the
            // renderer's ComboBox value lists exactly.
            ("module.menu", "options") => {
                let Some(inner) = self.nav_selected_inner_node(outer_id) else { return vec![]; };
                // Same legacy-derived defaults as the renderer, so a pre-checkbox
                // patch exposes the same conditional fields the body shows.
                let legacy = self.get_subpatch_param_str(outer_id, inner, "pointer_source")
                    .unwrap_or_else(|| "left_stick".to_string());
                let src_touch = self.get_subpatch_param_bool(outer_id, inner, "ptr_touch")
                    .unwrap_or(legacy == "touch1" || legacy == "touch2");
                let src_gyro = self.get_subpatch_param_bool(outer_id, inner, "ptr_gyro")
                    .unwrap_or(false);
                let mut fs = vec![
                    f!("LS", Toggle{key:"ptr_ls"}),
                    f!("RS", Toggle{key:"ptr_rs"}),
                    f!("Touch", Toggle{key:"ptr_touch"}),
                ];
                if src_touch {
                    fs.push(f!("Touch#", Enum{key:"ptr_touch_which",opts:&["touch1","touch2"]}));
                }
                fs.push(f!("Gyro", Toggle{key:"ptr_gyro"}));
                if src_gyro {
                    fs.push(f!("Gyro ax", Enum{key:"ptr_gyro_axes",opts:&["pitch_yaw","pitch_roll"]}));
                    fs.push(f!("Gyro ×", v("ptr_gyro_sens",0.5,8.0,4.0,Linear)));
                }
                fs.push(f!("Show", Enum{key:"activation_mode",opts:&["hold","toggle","touch"]}));
                fs.push(f!("Select", Enum{key:"select_on",opts:&["release","press","click"]}));
                fs.push(f!("Deadzone", v("pointer_deadzone",0.0,0.9,0.25,Linear)));
                fs.push(f!("Linger", v("select_linger",0.0,10.0,0.5,Fixed(0.05))));
                fs.push(f!("Sticky", Toggle{key:"hover_sticky"}));
                fs
            }
            _ => vec![],
        }
    }

    /// Kind of gamepad interaction the selected widget supports.
    pub(crate) fn nav_selected_kind(&self, outer_id: egui_snarl::NodeId) -> NavWidgetKind {
        // Which element this module is pinned as decides what a pad can do with
        // it: a curve module as its dot-graph or as an option row, a JSM config
        // as its editor or as one setting's fader.
        let elem = self.nav_selected_element(outer_id).map(|(_, e)| e);
        match self.nav_selected_module_id(outer_id).as_deref() {
            Some("module.knob") | Some("module.constant") => NavWidgetKind::Value,
            // A JSM setting is a value widget whose value happens to live in the
            // config text rather than in a param.
            Some("module.jsm")
                if elem
                    .as_deref()
                    .and_then(crate::canvas::viewer::jsm_knob_name_of)
                    .is_some() => NavWidgetKind::Value,
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
        let jsm_knob = self
            .nav_selected_element(outer_id)
            .and_then(|(_, e)| crate::canvas::viewer::jsm_knob_name_of(&e).map(str::to_string));
        // Armed before the first change, so North can undo the whole tuning pass
        // on this setting rather than just the last nudge.
        if let Some(name) = &jsm_knob {
            self.nav_note_jsm_baseline(outer_id, name);
        }
        let canvas = &mut self.tabs[self.active_tab].canvas;
        let Some(sp) = canvas.snarl.get_node_mut(outer_id).and_then(|n| n.subpatch.as_mut()) else { return; };
        let Some(node) = sp.snarl.get_node_mut(inner) else { return; };
        match node.module_id.as_str() {
            "module.jsm" => {
                if let Some(name) = jsm_knob {
                    crate::canvas::viewer::jsm_nav_nudge_knob(node, &name, delta);
                }
            }
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
            NavStep::Fixed(coarse) | NavStep::FixedScaled { coarse, .. } => {
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
}

/// The config tab `step` away from `cur`, wrapping.
///
/// Wrapping rather than stopping: there are usually two or three tabs and they
/// are a ring you cycle, not a list you scroll to the end of. Fewer than two and
/// there is nowhere to go, so the triggers do nothing rather than appearing
/// broken by landing back where they started.
pub(crate) fn next_config_tab(cur: usize, step: i32, count: usize) -> usize {
    if count < 2 {
        return cur.min(count.saturating_sub(1));
    }
    (cur as i32 + step).rem_euclid(count as i32) as usize
}

impl crate::app::FlexInputApp {
    /// Move the JSM editor to the next or previous config tab, wrapping.
    ///
    /// Wrapping rather than stopping: there are usually two or three tabs and
    /// they are a ring you cycle, not a list you scroll to the end of.
    fn nav_cycle_jsm_tab(&mut self, outer_id: egui_snarl::NodeId, step: i32) {
        let Some(inner) = self.nav_selected_inner_node(outer_id) else { return };
        let canvas = &mut self.tabs[self.active_tab].canvas;
        let Some(sp) = canvas.snarl.get_node_mut(outer_id).and_then(|n| n.subpatch.as_mut())
        else { return };
        let Some(node) = sp.snarl.get_node_mut(inner) else { return };
        let count = node
            .params
            .get("jsm_tabs")
            .and_then(|v| v.as_array())
            .map(|a| a.len())
            .unwrap_or(0);
        let cur = node.params.get("jsm_active_tab").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
        let next = next_config_tab(cur, step, count);
        node.params.insert("jsm_active_tab".into(), serde_json::Value::from(next as u64));
        // A different tab is a different config, so the cursor starts again
        // rather than pointing at a token that may not exist there.
        self.gamepad_nav.jsm_cursor = Default::default();
    }

    /// The sub-patch whose pin nav is driving: the config overlay's selection
    /// when one is up, else the tab canvas's own sub-patch.
    ///
    /// The plain "first sub-patch on the canvas" answer is wrong whenever the
    /// overlay is driving a pin from somewhere else, and a shortcut guard that
    /// asks the wrong node simply never fires.
    pub(crate) fn nav_driving_outer_id(&self) -> Option<egui_snarl::NodeId> {
        if let Some((o, _, _)) = self.gamepad_nav.config_nav_sel.as_ref() {
            return Some(*o);
        }
        self.nav_active_outer_id()
    }

    /// Is a JSM editor the current selection, at any level?
    ///
    /// Selection is enough — not "entered". The bumpers pick which of the
    /// editor's two panes you are about to work in, so they have to belong to it
    /// before you commit to one, and a shortcut that flips the app's tabs out
    /// from under a selected editor is never what was meant.
    pub(crate) fn nav_jsm_editor_selected(&self) -> bool {
        self.nav_driving_outer_id().is_some_and(|o| self.nav_is_jsm_editor(o))
    }

    /// Is the selection a pinned JSM editor (rather than one of its faders)?
    pub(crate) fn nav_is_jsm_editor(&self, outer_id: egui_snarl::NodeId) -> bool {
        matches!(
            self.nav_selected_element(outer_id).as_ref().map(|(m, e)| (m.as_str(), e.as_str())),
            Some(("module.jsm", "editor"))
        )
    }

    /// Walk the config with the token cursor.
    ///
    /// Up/down is a line, left/right is a token — the shape of a JSM line is a
    /// name, an `=` and a value, and every edit worth making from a pad replaces
    /// one of those whole. A character cursor would turn each into a dozen
    /// presses and leave you guessing which side of a word you were on.
    fn nav_drive_jsm_text(
        &mut self,
        ctx: &egui::Context,
        outer_id: egui_snarl::NodeId,
        nav: &crate::gamepad_nav::NavInput,
        step_dir: Option<crate::gamepad_nav::NavDir>,
    ) {
        use crate::gamepad_nav::NavDir;
        let Some(inner) = self.nav_selected_inner_node(outer_id) else { return };
        let text = self.nav_jsm_text(outer_id);
        let mut cur = flexinput_engine::eval::jsm_cursor_clamped(&text, self.gamepad_nav.jsm_cursor);

        // South is the editor's modifier, held rather than tapped, because East
        // is spoken for: it backs out of the widget everywhere in this app, and
        // a delete that shared it would be one slip from losing your place.
        // Holding South and tapping West is a thumb-roll you can do without
        // looking, which is the bar an editing chord has to clear.
        let holding = nav.pressed.contains("btn_south");
        // South + left/right opens an empty slot on that side and moves onto it.
        // Replacing is what the cursor does on its own, so without this there is
        // no way to ADD anything — every pick would land on the token you are
        // standing on, and `N = 2` could never become `N = 2 1`.
        //
        // The dpad only, and only on the press. The stick auto-repeats, and a
        // held stick would spray slots down the line faster than you could see.
        if holding {
            let right = nav.is_rising("dpad_right");
            let left = nav.is_rising("dpad_left");
            if right || left {
                let (edited, at) = flexinput_engine::eval::jsm_insert_slot(&text, cur, right);
                self.nav_set_jsm_text(outer_id, &edited);
                self.gamepad_nav.jsm_cursor = at;
                crate::canvas::viewer::publish_jsm_cursor(ctx, inner, at);
                crate::canvas::viewer::publish_jsm_chord(ctx, inner, true);
                return;
            }
            // Up and down open a whole line instead, on that side, and stand on
            // it. A config is a list of lines, so "another one of these" is the
            // edit you make right after getting one line right — and until now
            // the pad could only reach a line that happened to be blank already.
            let down = nav.is_rising("dpad_down");
            let up = nav.is_rising("dpad_up");
            if down || up {
                let (edited, at) = flexinput_engine::eval::jsm_insert_line(&text, cur, down);
                self.nav_set_jsm_text(outer_id, &edited);
                self.gamepad_nav.jsm_cursor = at;
                crate::canvas::viewer::publish_jsm_cursor(ctx, inner, at);
                crate::canvas::viewer::publish_jsm_chord(ctx, inner, true);
                return;
            }
        }
        if holding && nav.is_rising("btn_west") {
            let edited = flexinput_engine::eval::jsm_cursor_delete(&text, cur);
            if edited != text {
                self.nav_set_jsm_text(outer_id, &edited);
                self.gamepad_nav.jsm_cursor =
                    flexinput_engine::eval::jsm_cursor_clamped(&edited, cur);
                crate::canvas::viewer::publish_jsm_cursor(ctx, inner, self.gamepad_nav.jsm_cursor);
            }
            return;
        }

        // The stick, held down, is how a number gets typed: the command list can
        // offer names and only names, so without this every numeric setting a
        // pad wrote would stay `GYRO_SENS = ?`.
        //
        // One deflection is one step, which is why the latch exists — an axis
        // held for a fifth of a second would run the value off the end of its
        // range before you could let go. The step comes from the setting's own
        // range, and the stick has to be pushed clearly along one axis, for the
        // same reason walking the faders does: a diagonal must not quietly mean
        // both things.
        if holding {
            let y = nav.lstick.y;
            /// How far past centre counts as a deflection, and how far back
            /// counts as letting go. Two thresholds, so a stick resting just
            /// shy of the line can't chatter.
            const ENGAGE: f32 = 0.6;
            const RELEASE: f32 = 0.35;
            if y.abs() < RELEASE {
                self.gamepad_nav.jsm_scrub = 0;
            }
            let dir = if y.abs() >= ENGAGE && Self::nav_axis_is_clear(nav.lstick, true) {
                if y > 0.0 { 1 } else { -1 }
            } else {
                0
            };
            if dir != 0 && dir != self.gamepad_nav.jsm_scrub {
                self.gamepad_nav.jsm_scrub = dir;
                if let Some(edited) = flexinput_engine::eval::jsm_scrub_number(&text, cur, dir) {
                    self.nav_set_jsm_text(outer_id, &edited);
                    // The token keeps its place: the text under it changed, and
                    // the cursor has to stay on the number to walk it again.
                    crate::canvas::viewer::publish_jsm_cursor(ctx, inner, cur);
                    crate::canvas::viewer::publish_jsm_chord(ctx, inner, true);
                    return;
                }
            }
        } else {
            self.gamepad_nav.jsm_scrub = 0;
        }

        // While the modifier is down the directions belong to the chord, so the
        // cursor stays put — otherwise opening a slot would also walk away from
        // the one you just opened.
        if let (Some(dir), false) = (step_dir, holding) {
            let (dx, dy) = match dir {
                NavDir::Left => (-1, 0),
                NavDir::Right => (1, 0),
                NavDir::Up => (0, -1),
                NavDir::Down => (0, 1),
            };
            cur = flexinput_engine::eval::jsm_cursor_moved(&text, cur, dx, dy);
        }
        self.gamepad_nav.jsm_cursor = cur;
        // While South is held the pad is composing a chord, so tell the body to
        // say so — a modifier with no sign it is down is a modifier people press
        // twice.
        crate::canvas::viewer::publish_jsm_chord(ctx, inner, holding);
        // The body draws the highlight from this; publishing it here keeps the
        // cursor in one place rather than a copy per viewport.
        crate::canvas::viewer::publish_jsm_cursor(ctx, inner, cur);
    }

    /// Write the config text back to the selected JSM node's active tab.
    fn nav_set_jsm_text(&mut self, outer_id: egui_snarl::NodeId, text: &str) {
        let Some(inner) = self.nav_selected_inner_node(outer_id) else { return };
        let canvas = &mut self.tabs[self.active_tab].canvas;
        let Some(sp) = canvas.snarl.get_node_mut(outer_id).and_then(|n| n.subpatch.as_mut())
        else { return };
        let Some(node) = sp.snarl.get_node_mut(inner) else { return };
        crate::canvas::viewer::jsm_set_active_text(node, text);
    }

    /// The active tab's text for the selected JSM node.
    fn nav_jsm_text(&self, outer_id: egui_snarl::NodeId) -> String {
        let Some(inner) = self.nav_selected_inner_node(outer_id) else { return String::new() };
        let canvas = &self.tabs[self.active_tab].canvas;
        canvas
            .snarl
            .get_node(outer_id)
            .and_then(|n| n.subpatch.as_ref())
            .and_then(|sp| sp.snarl.get_node(inner))
            .map(crate::canvas::viewer::jsm_active_text)
            .unwrap_or_default()
    }

    /// Are the selected element's fields stacked in a column rather than laid out
    /// in a row? Decides which stick/dpad axis walks between them.
    fn nav_fields_are_a_column(&self, outer_id: egui_snarl::NodeId) -> bool {
        matches!(
            self.nav_selected_element(outer_id).as_ref().map(|(m, e)| (m.as_str(), e.as_str())),
            Some(("module.jsm", "editor"))
        )
    }

    /// Is the stick pointed clearly enough along one axis to mean it?
    ///
    /// A stick pushed near a diagonal used to register on both axes, so adjusting
    /// one setting could slip focus onto the next and carry on editing THAT —
    /// with nothing to show it had happened until the config was wrong. Below the
    /// engage threshold the direction came from the dpad, which is unambiguous.
    fn nav_axis_is_clear(stick: egui::Vec2, column: bool) -> bool {
        /// How much the walking axis must beat the other by.
        const MARGIN: f32 = 1.6;
        if stick.length() < 0.5 {
            return true;
        }
        let (walk, cross) = if column {
            (stick.y.abs(), stick.x.abs())
        } else {
            (stick.x.abs(), stick.y.abs())
        };
        walk >= cross * MARGIN
    }

    /// Does tuning this setting hand the LEFT stick to the game?
    ///
    /// If it does, that stick is not nav's to use — you would be aiming and
    /// adjusting with the same thumb, and the adjustment would drive the game.
    pub(crate) fn nav_tuning_takes_lstick(&self, outer_id: egui_snarl::NodeId, name: &str) -> bool {
        use flexinput_engine::eval::{JsmFeel, JsmHand};
        let Some(inner) = self.nav_selected_inner_node(outer_id) else { return false };
        let canvas = &self.tabs[self.active_tab].canvas;
        let Some(node) = canvas
            .snarl
            .get_node(outer_id)
            .and_then(|n| n.subpatch.as_ref())
            .and_then(|sp| sp.snarl.get_node(inner))
        else {
            return false;
        };
        let text = crate::canvas::viewer::jsm_active_text(node);
        let cfg = flexinput_engine::eval::jsm_compile(&text, &[]);
        matches!(
            flexinput_engine::eval::jsm_feel_of(&cfg, name),
            JsmFeel::Stick(JsmHand::Left)
        )
    }

    /// The stick nav may use while `name` is being tuned — the one the setting is
    /// not handing to the game.
    fn nav_free_stick(
        &self,
        outer_id: egui_snarl::NodeId,
        name: &str,
        nav: &crate::gamepad_nav::NavInput,
    ) -> egui::Vec2 {
        if self.nav_tuning_takes_lstick(outer_id, name) {
            nav.rstick
        } else {
            nav.lstick
        }
    }

    /// Is there enough of this field left on screen to ring?
    ///
    /// A scrolling strip publishes each row clipped to the band actually visible,
    /// so a row scrolled out comes back empty. Ringing that drew a highlight
    /// across the container's edge, pointing at a control nobody could see.
    pub(crate) fn nav_ring_is_worth_drawing(r: &egui::Rect) -> bool {
        r.width() > 2.0 && r.height() > 2.0
    }

    #[cfg(test)]
    pub(crate) fn nav_axis_is_clear_for_test(stick: egui::Vec2, column: bool) -> bool {
        Self::nav_axis_is_clear(stick, column)
    }

    /// Remember what a JSM setting was worth before the pad started changing it,
    /// so North can put it back. Re-armed whenever the focus moves to a different
    /// setting.
    fn nav_note_jsm_baseline(&mut self, outer_id: egui_snarl::NodeId, name: &str) {
        if self.gamepad_nav.jsm_baseline.as_ref().is_some_and(|(n, _)| n == name) {
            return;
        }
        let value = self.nav_jsm_knobs(outer_id).into_iter().find(|k| k.name == name).map(|k| k.value);
        self.gamepad_nav.jsm_baseline = value.map(|v| (name.to_string(), v));
    }

    /// Put a JSM setting back to what it was before this tuning pass.
    fn nav_restore_jsm_baseline(&mut self, outer_id: egui_snarl::NodeId,
        inner: egui_snarl::NodeId, name: &str)
    {
        let Some((_, want)) = self.gamepad_nav.jsm_baseline.clone().filter(|(n, _)| n == name)
        else { return; };
        let Some(k) = self.nav_jsm_knobs(outer_id).into_iter().find(|k| k.name == name)
        else { return; };
        if (k.value - want).abs() < f32::EPSILON {
            return;
        }
        // Expressed as a nudge, so it goes through the one path that rewrites the
        // text — there is no second way to set a JSM value.
        let span = (k.hi - k.lo).abs().max(f32::EPSILON);
        self.nav_adjust_jsm_knob(outer_id, inner, name, (want - k.value) / span);
    }

    /// The numeric settings the selected JSM node's active tab sets.
    ///
    /// Read fresh from the config text every call — it is the source of truth,
    /// and a setting typed into the editor while the overlay is open should be
    /// navigable on the next frame without any registration step.
    pub(crate) fn nav_jsm_knobs(
        &self,
        outer_id: egui_snarl::NodeId,
    ) -> Vec<flexinput_engine::eval::JsmKnob> {
        let Some(inner) = self.nav_selected_inner_node(outer_id) else { return vec![] };
        let canvas = &self.tabs[self.active_tab].canvas;
        canvas
            .snarl
            .get_node(outer_id)
            .and_then(|n| n.subpatch.as_ref())
            .and_then(|sp| sp.snarl.get_node(inner))
            .map(crate::canvas::viewer::jsm_knobs_of)
            .unwrap_or_default()
    }

    /// Nudge one JSM setting by a fraction of its range. Unlike every other nav
    /// field this writes the config TEXT, since that is where the value lives.
    pub(crate) fn nav_adjust_jsm_knob(&mut self, outer_id: egui_snarl::NodeId,
        inner: egui_snarl::NodeId, name: &str, delta: f32)
    {
        let canvas = &mut self.tabs[self.active_tab].canvas;
        let Some(sp) = canvas.snarl.get_node_mut(outer_id).and_then(|n| n.subpatch.as_mut()) else { return; };
        if let Some(node) = sp.snarl.get_node_mut(inner) {
            crate::canvas::viewer::jsm_nav_nudge_knob(node, name, delta);
        }
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
        // A JSM setting pinned on its own: back to what it was before tuning.
        if let Some(name) = self
            .nav_selected_element(outer_id)
            .and_then(|(m, e)| (m == "module.jsm")
                .then(|| crate::canvas::viewer::jsm_knob_name_of(&e).map(str::to_string))
                .flatten())
        {
            if let Some(inner) = self.nav_selected_inner_node(outer_id) {
                self.nav_restore_jsm_baseline(outer_id, inner, &name);
            }
            return;
        }
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

#[cfg(test)]
mod nav_axis_tests {
    use crate::app::FlexInputApp;
    use eframe::egui;

    fn clear(x: f32, y: f32, column: bool) -> bool {
        FlexInputApp::nav_axis_is_clear_for_test(egui::vec2(x, y), column)
    }

    // The bug this exists to stop: adjusting one setting with a stick held a few
    // degrees off the horizontal also counted as a step DOWN the list, so focus
    // slipped to the next setting and kept editing that one — with nothing on
    // screen to say it had happened until the config was wrong.
    #[test]
    fn a_stick_held_near_a_diagonal_does_not_walk_the_list() {
        // A column (the JSM editor): up/down walks, so Y must dominate.
        assert!(clear(0.0, 1.0, true), "straight down the list");
        assert!(clear(0.2, 0.95, true), "a slight lean still means down");
        assert!(!clear(0.75, 0.66, true), "a diagonal does not");
        assert!(!clear(1.0, 0.0, true), "and sideways certainly does not");

        // A row (every other multi-field element): left/right walks, so X must.
        assert!(clear(1.0, 0.0, false));
        assert!(!clear(0.66, 0.75, false));

        // Below the engage threshold the direction came from the dpad, which is
        // unambiguous — gating it there would make the dpad feel broken.
        assert!(clear(0.0, 0.0, true), "the dpad");
        assert!(clear(0.3, 0.3, true), "a barely-touched stick is the dpad's case");
    }

    // A strip that scrolls publishes each row clipped to the band on screen, so a
    // row scrolled out comes back empty. Ringing that drew a highlight lying
    // across the container's edge, pointing at a control nobody could see.
    #[test]
    fn a_field_scrolled_out_of_view_gets_no_highlight_ring() {
        let band = egui::Rect::from_min_size(egui::pos2(0.0, 100.0), egui::vec2(200.0, 80.0));
        let row_at = |y: f32| {
            egui::Rect::from_min_size(egui::pos2(0.0, y), egui::vec2(200.0, 24.0)).intersect(band)
        };
        let worth = FlexInputApp::nav_ring_is_worth_drawing;

        assert!(worth(&row_at(120.0)), "a row inside the band rings");
        assert!(worth(&row_at(96.0)), "and one straddling the edge still does");
        assert!(!worth(&row_at(20.0)), "one scrolled off the top does not");
        assert!(!worth(&row_at(400.0)), "nor one below the fold");
        // Exactly flush with the edge is nothing to ring either.
        assert!(!worth(&row_at(76.0)), "a row ending on the band's top edge");
    }
}

#[cfg(test)]
mod jsm_tab_cycle_tests {
    use super::next_config_tab;

    /// Cycling the editor's config tabs wraps, and a config with one tab doesn't
    /// move — the triggers only mean something when there is somewhere to go.
    #[test]
    fn config_tabs_cycle_round_and_stay_put_when_there_is_one() {
        assert_eq!(next_config_tab(0, 1, 3), 1);
        assert_eq!(next_config_tab(2, 1, 3), 0, "past the last is the first");
        assert_eq!(next_config_tab(0, -1, 3), 2, "and before the first is the last");
        assert_eq!(next_config_tab(1, -1, 3), 0);
        assert_eq!(next_config_tab(0, 1, 1), 0, "one tab has nowhere to go");
        assert_eq!(next_config_tab(0, -1, 0), 0, "nor has none");
        // A stored index past the end (a tab was deleted) comes back in range
        // rather than staying out of it.
        assert_eq!(next_config_tab(9, 0, 1), 0);
        assert_eq!(next_config_tab(9, 1, 3), 1, "wrapped into range");
    }
}
