//! Gamepad-nav driving of curve editing: the per-card response-curve
//! section on Remapper/Lean mapping cards, and dot-level editing of
//! curve modules (and the ASTH EQ, which shares the machinery).

use super::*;

impl FlexInputApp {

    /// Whether the entered card carries a per-card response curve section, and if
    /// so whether it also offers an activation THRESHOLD (`Some(show_threshold)`).
    /// Mirrors the body's gating: Remapper cards with any analog in pin, and Lean
    /// cards (always); Touch Zones cards get a curve when analog (no threshold) or
    /// a swipe direction (with threshold), matching the body's `card_analog ||
    /// is_swipe`. Map Action ("mappings" on a non-Remapper node) returns `None`.
    pub(crate) fn nav_card_curve_shape(&self, outer_id: egui_snarl::NodeId, idx: usize) -> Option<bool> {
        let scope = self.nav_remap_mappings_key(outer_id);
        let inner = self.nav_selected_inner_node(outer_id)?;
        let canvas = &self.tabs[self.active_tab].canvas;
        let node = canvas.snarl.get_node(outer_id)?.subpatch.as_ref()?.snarl.get_node(inner)?;
        match scope {
            "lean_left" | "lean_right" => Some(true),
            "mappings" => {
                if node.module_id != "module.remapper" { return None; }
                let analog = node.params.get("mappings").and_then(|v| v.as_array())
                    .and_then(|a| a.get(idx))
                    .and_then(|m| m.get("in").and_then(|v| v.as_array()))
                    .map(|a| a.iter().filter_map(|v| v.as_str())
                        .any(flexinput_engine::pin_is_analog_input))
                    .unwrap_or(false);
                if analog { Some(true) } else { None }
            }
            "zone_maps" => {
                if node.module_id != "module.touch_zones" { return None; }
                let card = node.params.get("zone_maps").and_then(|v| v.as_array())
                    .and_then(|a| a.get(idx))?;
                let is_swipe = card.get("in").and_then(|v| v.as_array())
                    .and_then(|a| a.first()).and_then(|v| v.as_str())
                    .map(|t| t.starts_with("tz_swipe")).unwrap_or(false);
                let analog = card.get("out").and_then(|v| v.as_array())
                    .map(|a| a.iter().filter_map(|v| v.as_str())
                        .any(crate::canvas::viewer::tz_out_pin_is_analog))
                    .unwrap_or(false);
                // Swipe → curve WITH threshold; analog → curve only; else no curve.
                if is_swipe { Some(true) } else if analog { Some(false) } else { None }
            }
            _ => None,
        }
    }

    /// Ctx-temp id for a card's response-curve open flag (matches the body's
    /// `mapping_card_curve_section` key exactly).
    pub(crate) fn nav_card_curve_open_id(inner: egui_snarl::NodeId, scope: &str, idx: usize) -> egui::Id {
        egui::Id::new(("card_curve_open", inner.0, scope.to_string(), idx))
    }
    pub(crate) fn nav_card_curve_open(&self, ctx: &egui::Context, inner: egui_snarl::NodeId, scope: &str, idx: usize) -> bool {
        ctx.data(|d| d.get_temp::<bool>(Self::nav_card_curve_open_id(inner, scope, idx))).unwrap_or(false)
    }
    pub(crate) fn nav_set_card_curve_open(&self, ctx: &egui::Context, inner: egui_snarl::NodeId, scope: &str, idx: usize, open: bool) {
        ctx.data_mut(|d| d.insert_temp(Self::nav_card_curve_open_id(inner, scope, idx), open));
    }

    /// Toggle the presence of a card's `threshold` param (default 0.5 when added).
    /// Returns whether it changed anything.
    pub(crate) fn nav_toggle_card_threshold(&mut self, outer_id: egui_snarl::NodeId, idx: usize) -> bool {
        self.nav_remap_card_obj_mut(outer_id, idx, |m| {
            if m.contains_key("threshold") {
                m.remove("threshold");
            } else {
                m.insert("threshold".to_string(), serde_json::json!(0.5));
            }
            true
        })
    }

    /// Nudge a card's `threshold` value (only when present) by `delta`, clamped to
    /// 0.01..1.0. Returns whether it changed.
    pub(crate) fn nav_nudge_card_threshold(&mut self, outer_id: egui_snarl::NodeId, idx: usize, delta: f32) -> bool {
        self.nav_remap_card_obj_mut(outer_id, idx, |m| {
            let Some(cur) = m.get("threshold").and_then(|v| v.as_f64()) else { return false; };
            let next = ((cur as f32) + delta).clamp(0.01, 1.0);
            m.insert("threshold".to_string(), serde_json::json!(next as f64));
            true
        })
    }

    /// Read the entered card's response `curve` (≥2 pts) for the gamepad dot
    /// editor — the Remapper/Lean analogue of the Touch Zones per-zone curve.
    pub(crate) fn nav_card_curve_points(&self, outer_id: egui_snarl::NodeId)
        -> Option<(egui_snarl::NodeId, Vec<[f32; 2]>)>
    {
        let scope = self.nav_remap_mappings_key(outer_id);
        if !matches!(scope, "mappings" | "lean_left" | "lean_right" | "zone_maps") { return None; }
        let inner = self.nav_selected_inner_node(outer_id)?;
        let idx = self.gamepad_nav.remap_card;
        let canvas = &self.tabs[self.active_tab].canvas;
        let node = canvas.snarl.get_node(outer_id)?.subpatch.as_ref()?.snarl.get_node(inner)?;
        let pts: Vec<[f32; 2]> = node.params.get(scope).and_then(|v| v.as_array())
            .and_then(|a| a.get(idx))
            .and_then(|m| m.get("curve").and_then(|v| v.as_array()))
            .map(|a| a.iter().filter_map(|p| {
                let q = p.as_array()?;
                Some([q.first()?.as_f64()? as f32, q.get(1)?.as_f64()? as f32])
            }).collect())
            .unwrap_or_default();
        // No stored curve yet → seed identity so the editor has dots to grab.
        let pts = if pts.len() >= 2 { pts } else { vec![[0.0, 0.0], [1.0, 1.0]] };
        Some((inner, pts))
    }

    /// Write dot edits back onto the entered card's `curve` (identity collapses
    /// to no stored curve, matching the mouse editor).
    pub(crate) fn nav_card_curve_write(&mut self, outer_id: egui_snarl::NodeId, pts: &[[f32; 2]]) {
        let idx = self.gamepad_nav.remap_card;
        let identity = pts.len() == 2 && pts[0] == [0.0, 0.0] && pts[1] == [1.0, 1.0];
        self.nav_remap_card_obj_mut(outer_id, idx, |m| {
            if identity {
                m.remove("curve");
            } else {
                m.insert("curve".to_string(), serde_json::Value::Array(
                    pts.iter().map(|p| serde_json::json!([p[0] as f64, p[1] as f64])).collect()));
            }
            true
        });
    }

    /// Param keys (points, biases) for the curve's currently-edited lane. The
    /// two-way curve has an up lane (`points`) and a down lane (`points_dn`),
    /// switched by its `active_lane` param; the driver edits whichever is active
    /// so it matches the lane shown (and glowed) in the body. Other curves only
    /// have `points`.
    /// (points_key, Option<biases_key>) for the selected curve-like element. The
    /// Audio Stream Haptics EQ shares the curve-dot nav machinery but stores its
    /// points under `asth_eq_points` and has NO per-segment biases (linear EQ).
    pub(crate) fn nav_curve_keys(&self, outer_id: egui_snarl::NodeId, inner: egui_snarl::NodeId)
        -> (&'static str, Option<&'static str>)
    {
        let canvas = &self.tabs[self.active_tab].canvas;
        let node = canvas.snarl.get_node(outer_id)
            .and_then(|n| n.subpatch.as_ref())
            .and_then(|sp| sp.snarl.get_node(inner));
        if node.map(|n| n.module_id.as_str()) == Some("module.audio_stream_haptics") {
            return ("asth_eq_points", None);
        }
        if node.map(|n| n.module_id.as_str()) == Some("module.vec_reshape") {
            // Nav edits whichever curve the body's Edit toggle has active.
            let gain = node.and_then(|n| n.params.get("edit_target").and_then(|v| v.as_str()))
                != Some("boundary");
            return if gain { ("gain_pts", Some("gain_biases")) } else { ("boundary_pts", None) };
        }
        let lane_dn = node
            .filter(|node| node.module_id == "module.twoway_response_curve")
            .and_then(|node| node.params.get("active_lane").and_then(|v| v.as_str()))
            == Some("dn");
        if lane_dn { ("points_dn", Some("biases_dn")) } else { ("points", Some("biases")) }
    }

    /// Read/parse the selected curve node's `points` (Vec<[f32;2]>), if the
    /// selection is a response-curve module. Returns (inner_node_id, points).
    pub(crate) fn nav_curve_points(&self, outer_id: egui_snarl::NodeId)
        -> Option<(egui_snarl::NodeId, Vec<[f32; 2]>)>
    {
        // Remapper/Lean per-card response curve: entered from a mapping card
        // (curve_return_level == RemapCard), so it lives on the entered card's
        // `curve`, not a node-level param.
        if matches!(self.gamepad_nav.curve_return_level, crate::gamepad_nav::EditLevel::RemapCard) {
            return self.nav_card_curve_points(outer_id);
        }
        let inner = self.nav_selected_inner_node(outer_id)?;
        let canvas = &self.tabs[self.active_tab].canvas;
        let sp = canvas.snarl.get_node(outer_id)?.subpatch.as_ref()?;
        let node = sp.snarl.get_node(inner)?;
        // Touch Zones per-zone response curve: not a top-level curve param — it
        // lives on the selected analog zone's card. Read it via the shared helper.
        if node.module_id == "module.touch_zones" {
            let field = node.params.get("sel_field").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
            let zone = node.params.get("sel_zone").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
            let zm = node.params.get("zone_maps").and_then(|v| v.as_array()).cloned().unwrap_or_default();
            if !crate::canvas::viewer::tz_zone_is_analog(&zm, field, zone) { return None; }
            let pts = crate::canvas::viewer::tz_zone_curve(&zm, field, zone);
            if pts.len() < 2 { return None; }
            return Some((inner, pts));
        }
        if !crate::module_ui_info::has_nav_response_curve(&node.module_id)
        { return None; }
        let (pts_key, _) = self.nav_curve_keys(outer_id, inner);
        let pts: Vec<[f32; 2]> = node.params.get(pts_key)?.as_array()?
            .iter().filter_map(|p| {
                let a = p.as_array()?;
                Some([a.get(0)?.as_f64()? as f32, a.get(1)?.as_f64()? as f32])
            }).collect();
        if pts.len() < 2 { return None; }
        Some((inner, pts))
    }

    /// Write a points Vec back to a curve node (keeps `biases` length in sync
    /// at one-per-segment, padding/truncating with 0.0).
    pub(crate) fn nav_curve_write_points(&mut self, inner: egui_snarl::NodeId,
        outer_id: egui_snarl::NodeId, pts: &[[f32; 2]])
    {
        // Remapper/Lean per-card response curve (entered from a mapping card).
        if matches!(self.gamepad_nav.curve_return_level, crate::gamepad_nav::EditLevel::RemapCard) {
            self.nav_card_curve_write(outer_id, pts);
            return;
        }
        // Touch Zones: write back to the selected analog zone's card curve.
        {
            let canvas = &self.tabs[self.active_tab].canvas;
            let is_tz = canvas.snarl.get_node(outer_id).and_then(|n| n.subpatch.as_ref())
                .and_then(|sp| sp.snarl.get_node(inner))
                .map(|n| n.module_id == "module.touch_zones").unwrap_or(false);
            if is_tz {
                let (field, zone) = canvas.snarl.get_node(outer_id).and_then(|n| n.subpatch.as_ref())
                    .and_then(|sp| sp.snarl.get_node(inner))
                    .map(|n| (
                        n.params.get("sel_field").and_then(|v| v.as_u64()).unwrap_or(0) as usize,
                        n.params.get("sel_zone").and_then(|v| v.as_u64()).unwrap_or(0) as usize))
                    .unwrap_or((0, 0));
                if let Some(sp) = self.tabs[self.active_tab].canvas.snarl
                    .get_node_mut(outer_id).and_then(|n| n.subpatch.as_mut())
                {
                    crate::canvas::viewer::tz_set_zone_curve(&mut sp.snarl, inner, field, zone, pts);
                }
                return;
            }
        }
        let (pts_key, bias_key) = self.nav_curve_keys(outer_id, inner);
        let canvas = &mut self.tabs[self.active_tab].canvas;
        let Some(sp) = canvas.snarl.get_node_mut(outer_id).and_then(|n| n.subpatch.as_mut())
        else { return; };
        let Some(node) = sp.snarl.get_node_mut(inner) else { return; };
        let arr: Vec<serde_json::Value> = pts.iter()
            .map(|p| serde_json::json!([p[0] as f64, p[1] as f64])).collect();
        node.params.insert(pts_key.into(), serde_json::Value::Array(arr));
        // biases: one per segment (points-1). Curve-less EQ (ASTH) has no biases.
        let Some(bias_key) = bias_key else { return; };
        let want = pts.len().saturating_sub(1);
        let mut biases: Vec<f64> = node.params.get(bias_key)
            .and_then(|v| v.as_array())
            .map(|a| a.iter().filter_map(|b| b.as_f64()).collect())
            .unwrap_or_default();
        biases.resize(want, 0.0);
        node.params.insert(bias_key.into(),
            serde_json::Value::Array(biases.into_iter().map(serde_json::Value::from).collect()));
    }

    /// Graph X/Y span for a curve (absolute curves: 0..1; bipolar: -1..1).
    /// Derived from the published geometry bounds.
    pub(crate) fn nav_curve_bounds(&self, ctx: &egui::Context, inner: egui_snarl::NodeId)
        -> (f32, f32, f32, f32)
    {
        self.nav_curve_geom(ctx, inner)
            .map(|(_, xl, xh, yl, yh)| (xl, xh, yl, yh))
            .unwrap_or((0.0, 1.0, 0.0, 1.0))
    }

    /// Convert the cursor's screen pos to graph coords, if the cursor is over
    /// this curve's graph rect.
    pub(crate) fn nav_curve_cursor_graph(&self, ctx: &egui::Context, inner: egui_snarl::NodeId)
        -> Option<[f32; 2]>
    {
        let (rect, x_lo, x_hi, y_lo, y_hi) = self.nav_curve_geom(ctx, inner)?;
        let p = self.gamepad_nav.cursor_pos;
        if !rect.contains(p) { return None; }
        Some([
            x_lo + (p.x - rect.left()) / rect.width() * (x_hi - x_lo),
            y_lo + (rect.bottom() - p.y) / rect.height() * (y_hi - y_lo),
        ])
    }

    /// Index of the curve dot nearest the cursor (when over the graph), else None.
    pub(crate) fn nav_curve_dot_near_cursor(&self, ctx: &egui::Context,
        outer_id: egui_snarl::NodeId, inner: egui_snarl::NodeId) -> Option<usize>
    {
        let g = self.nav_curve_cursor_graph(ctx, inner)?;
        let (_, pts) = self.nav_curve_points(outer_id)?;
        let (x_lo, x_hi, y_lo, y_hi) = self.nav_curve_bounds(ctx, inner);
        let sx = (x_hi - x_lo).abs().max(f32::EPSILON);
        let sy = (y_hi - y_lo).abs().max(f32::EPSILON);
        pts.iter().enumerate()
            .min_by(|(_, a), (_, b)| {
                let da = ((a[0]-g[0])/sx).powi(2) + ((a[1]-g[1])/sy).powi(2);
                let db = ((b[0]-g[0])/sx).powi(2) + ((b[1]-g[1])/sy).powi(2);
                da.partial_cmp(&db).unwrap()
            })
            .map(|(i, _)| i)
    }

    /// Add a dot at the cursor's graph position (when the cursor is over the
    /// graph). Returns the inserted index, or None if the cursor isn't on it.
    pub(crate) fn nav_curve_add_at_cursor(&mut self, ctx: &egui::Context,
        outer_id: egui_snarl::NodeId, inner: egui_snarl::NodeId) -> Option<usize>
    {
        let g = self.nav_curve_cursor_graph(ctx, inner)?;
        let (x_lo, x_hi, y_lo, y_hi) = self.nav_curve_bounds(ctx, inner);
        let (_, mut pts) = self.nav_curve_points(outer_id)?;
        let gx = g[0].clamp(x_lo, x_hi);
        let gy = g[1].clamp(y_lo, y_hi);
        let idx = pts.partition_point(|p| p[0] < gx);
        pts.insert(idx, [gx, gy]);
        self.nav_curve_write_points(inner, outer_id, &pts);
        Some(idx)
    }

    /// Delete a specific dot index (guards endpoints / min 2 points).
    pub(crate) fn nav_curve_delete_index(&mut self, outer_id: egui_snarl::NodeId, idx: usize) -> bool {
        let Some((inner, mut pts)) = self.nav_curve_points(outer_id) else { return false; };
        if pts.len() <= 2 { return false; }
        // Keep the two endpoints; only interior dots are deletable.
        if idx == 0 || idx >= pts.len() - 1 { return false; }
        pts.remove(idx);
        self.nav_curve_write_points(inner, outer_id, &pts);
        true
    }

    /// Move dot `i` by (dx, dy) in graph space. Endpoints keep their fixed X
    /// (only Y moves); interior dots clamp X between neighbors.
    pub(crate) fn nav_curve_move_dot(&mut self, outer_id: egui_snarl::NodeId, i: usize, dx: f32, dy: f32) {
        let Some((inner, mut pts)) = self.nav_curve_points(outer_id) else { return; };
        if i >= pts.len() { return; }
        // Bounds from the node: absolute → 0..1 ; bipolar → -1..1. Infer from
        // the existing endpoints' x.
        let x_lo = pts.first().map(|p| p[0]).unwrap_or(0.0);
        let (y_lo, y_hi) = if x_lo < 0.0 { (-1.0, 1.0) } else { (0.0, 1.0) };
        let is_end = i == 0 || i == pts.len() - 1;
        let new_x = if is_end {
            pts[i][0] // endpoints fixed in X
        } else {
            let lo = pts[i - 1][0] + 0.001;
            let hi = pts[i + 1][0] - 0.001;
            (pts[i][0] + dx).clamp(lo, hi)
        };
        let new_y = (pts[i][1] + dy).clamp(y_lo, y_hi);
        pts[i] = [new_x, new_y];
        self.nav_curve_write_points(inner, outer_id, &pts);
    }

    /// Adjust the bias (curvature) of the segment to the RIGHT of dot `i` by
    /// `db`, clamped to [-1, 1]. (Biases are one-per-segment.)
    pub(crate) fn nav_curve_adjust_bias(&mut self, outer_id: egui_snarl::NodeId, i: usize, db: f32) {
        let Some(inner) = self.nav_selected_inner_node(outer_id) else { return; };
        let (pts_key, bias_key) = self.nav_curve_keys(outer_id, inner);
        // No per-segment biases (ASTH EQ) → nothing to adjust.
        let Some(bias_key) = bias_key else { return; };
        let canvas = &mut self.tabs[self.active_tab].canvas;
        let Some(sp) = canvas.snarl.get_node_mut(outer_id).and_then(|n| n.subpatch.as_mut()) else { return; };
        let Some(node) = sp.snarl.get_node_mut(inner) else { return; };
        let n_pts = node.params.get(pts_key).and_then(|v| v.as_array()).map(|a| a.len()).unwrap_or(0);
        if n_pts < 2 { return; }
        let seg = i.min(n_pts - 2); // segment index to the right of dot i
        let mut biases: Vec<f64> = node.params.get(bias_key)
            .and_then(|v| v.as_array())
            .map(|a| a.iter().filter_map(|b| b.as_f64()).collect())
            .unwrap_or_default();
        biases.resize(n_pts - 1, 0.0);
        biases[seg] = (biases[seg] as f32 + db).clamp(-1.0, 1.0) as f64;
        node.params.insert(bias_key.into(),
            serde_json::Value::Array(biases.into_iter().map(serde_json::Value::from).collect()));
    }

    /// Read the curve geometry the curve body published last frame:
    /// (graph rect, x_lo, x_hi, y_lo, y_hi). Lets the driver map graph↔screen.
    pub(crate) fn nav_curve_geom(&self, ctx: &egui::Context, inner: egui_snarl::NodeId)
        -> Option<(egui::Rect, f32, f32, f32, f32)>
    {
        let g: (u64, egui::Rect, f32, f32, f32, f32) =
            ctx.data(|d| d.get_temp(egui::Id::new(("gp_nav_curve_geom", inner.0))))?;
        Some((g.1, g.2, g.3, g.4, g.5))
    }

    /// Publish the highlighted dot + editing flag so the curve body rings it.
    pub(crate) fn nav_publish_curve_sel(&self, ctx: &egui::Context, inner: egui_snarl::NodeId, editing: bool) {
        let pass = ctx.cumulative_pass_nr();
        let idx = self.gamepad_nav.curve_dot;
        ctx.data_mut(|d| {
            d.insert_temp(egui::Id::new(("gp_nav_curve_sel", inner.0)), (pass, idx, editing));
            // Envelope sustain-line focus (dpad up/down toggle) — the painter reads
            // this to highlight the sustain line and dim the dots when focused.
            d.insert_temp(
                egui::Id::new(("gp_nav_curve_sustain", inner.0)),
                (pass, self.gamepad_nav.curve_sustain_focus),
            );
        });
    }

    /// While dot-editing a Remapper/Lean PER-CARD curve, keep the entered-card
    /// channel fresh (field 5) so the body keeps publishing the graph geometry —
    /// otherwise `nav_drive_remap_card` (which normally publishes it) isn't running
    /// and the geometry goes stale, breaking dot placement. No-op for module /
    /// Touch Zones curves, which publish geometry unconditionally.
    pub(crate) fn nav_keep_card_curve_focus(&self, ctx: &egui::Context,
        outer_id: egui_snarl::NodeId, inner: egui_snarl::NodeId)
    {
        if !matches!(self.gamepad_nav.curve_return_level, crate::gamepad_nav::EditLevel::RemapCard) {
            return;
        }
        let scope = self.nav_remap_mappings_key(outer_id);
        let pass = ctx.cumulative_pass_nr();
        let idx = self.gamepad_nav.remap_card;
        ctx.data_mut(|d| {
            d.insert_temp(egui::Id::new(("gp_nav_remap_card", inner.0, scope)), (pass, idx, true));
            d.insert_temp(egui::Id::new(("gp_nav_remap_card_field", inner.0, scope)), (pass, 5u64));
        });
    }

    /// `CurveDots` level: dpad/LS highlights a dot, RT adds (at cursor if
    /// visible else largest gap), LT deletes (nearest cursor / highlighted),
    /// South enters dot move, East exits the curve.
    pub(crate) fn nav_drive_curve_dots(
        &mut self,
        ctx: &egui::Context,
        outer_id: egui_snarl::NodeId,
        nav: &crate::gamepad_nav::NavInput,
        step_dir: Option<crate::gamepad_nav::NavDir>,
        rt_rising: bool,
        lt_rising: bool,
    ) {
        use crate::gamepad_nav::{EditLevel, NavDir};
        // Where to pop back to on exit — Widget for the Response Curve family,
        // TzCards for a Touch Zones per-zone curve (set at entry).
        let ret = self.gamepad_nav.curve_return_level;
        let Some((inner, pts)) = self.nav_curve_points(outer_id) else {
            // Not a curve any more — bail out.
            self.gamepad_nav.edit_level = ret;
            return;
        };
        self.nav_keep_card_curve_focus(ctx, outer_id, inner);
        // Clamp highlight to valid range.
        self.gamepad_nav.curve_dot = self.gamepad_nav.curve_dot.min(pts.len() - 1);

        // East → exit the curve (commit one undo entry for the whole session).
        if nav.is_rising("btn_east") {
            self.gamepad_nav.edit_level = ret;
            if let Some(b) = self.gamepad_nav.edit_baseline.take() {
                self.tabs[self.active_tab].canvas.commit_undo_if_changed(*b);
            }
            return;
        }

        // Envelope only: dpad up/down toggles focus between the SUSTAIN line and
        // the dots; while the sustain line is focused, left/right MOVES it instead
        // of walking dots (and add/delete/enter-dot are suppressed).
        let is_env =
            self.nav_selected_module_id(outer_id).as_deref() == Some("generator.envelope");
        if !is_env {
            self.gamepad_nav.curve_sustain_focus = false;
        } else if matches!(step_dir, Some(NavDir::Up) | Some(NavDir::Down)) {
            self.gamepad_nav.curve_sustain_focus = !self.gamepad_nav.curve_sustain_focus;
            self.nav_publish_curve_sel(ctx, inner, false);
            return;
        }
        if is_env && self.gamepad_nav.curve_sustain_focus {
            let d = match step_dir {
                Some(NavDir::Left) => -1.0,
                Some(NavDir::Right) => 1.0,
                _ => 0.0,
            };
            if d != 0.0 {
                let step = if self.gamepad_nav.fine_increment { 0.01 } else { 0.03 };
                let cur = self.get_subpatch_param_f32(outer_id, inner, "sustain").unwrap_or(0.5);
                let next = (cur + d * step).clamp(0.0, 1.0);
                self.set_subpatch_param_f32(outer_id, inner, "sustain", next);
            }
            self.nav_publish_curve_sel(ctx, inner, false);
            return;
        }

        // dpad/LS left-right steps the highlight between dots.
        match step_dir {
            Some(NavDir::Left) => {
                self.gamepad_nav.curve_dot = self.gamepad_nav.curve_dot.saturating_sub(1);
            }
            Some(NavDir::Right) => {
                self.gamepad_nav.curve_dot = (self.gamepad_nav.curve_dot + 1).min(pts.len() - 1);
            }
            _ => {}
        }

        // RT/LT are CURSOR-DRIVEN: they require the RS/gyro cursor to be over
        // the graph. RT adds a dot at the cursor's graph position; LT deletes the
        // dot nearest the cursor. When the cursor isn't over the graph (e.g. the
        // user is stepping dots with the dpad), the triggers do nothing — so the
        // action is always exactly where the cursor points.
        let cursor_on_graph =
            self.gamepad_nav.cursor_visible
            && self.nav_curve_cursor_graph(ctx, inner).is_some();
        if rt_rising && cursor_on_graph {
            let base = self.tabs[self.active_tab].canvas.snapshot_for_undo();
            if let Some(idx) = self.nav_curve_add_at_cursor(ctx, outer_id, inner) {
                self.gamepad_nav.curve_dot = idx;
                self.tabs[self.active_tab].canvas.commit_undo_if_changed(base);
            }
        }
        if lt_rising && cursor_on_graph {
            if let Some(target) = self.nav_curve_dot_near_cursor(ctx, outer_id, inner) {
                let base = self.tabs[self.active_tab].canvas.snapshot_for_undo();
                if self.nav_curve_delete_index(outer_id, target) {
                    self.gamepad_nav.curve_dot = self.gamepad_nav.curve_dot.min(
                        self.nav_curve_points(outer_id).map(|(_, p)| p.len().saturating_sub(1)).unwrap_or(0));
                    self.tabs[self.active_tab].canvas.commit_undo_if_changed(base);
                }
            }
        }

        // South → grab the highlighted dot under the cursor first (if visible).
        if nav.is_rising("btn_south") {
            if self.gamepad_nav.cursor_visible {
                if let Some(idx) = self.nav_curve_dot_near_cursor(ctx, outer_id, inner) {
                    self.gamepad_nav.curve_dot = idx;
                }
            }
            self.gamepad_nav.edit_level = EditLevel::CurveDot;
            self.gamepad_nav.fine_increment = false;
        }

        self.nav_publish_curve_sel(ctx, inner, false);
    }

    /// `CurveDot` level: dpad/LS moves the highlighted dot in X/Y; hold-North
    /// edits the segment curvature (bias); East/South returns to dot nav.
    pub(crate) fn nav_drive_curve_dot(
        &mut self,
        ctx: &egui::Context,
        outer_id: egui_snarl::NodeId,
        nav: &crate::gamepad_nav::NavInput,
        dt: f32,
        step_dir: Option<crate::gamepad_nav::NavDir>,
        rt_rising: bool,
        lt_rising: bool,
    ) {
        use crate::gamepad_nav::EditLevel;
        let _ = (rt_rising, lt_rising, step_dir);
        let Some((inner, pts)) = self.nav_curve_points(outer_id) else {
            self.gamepad_nav.edit_level = EditLevel::Widget;
            return;
        };
        self.nav_keep_card_curve_focus(ctx, outer_id, inner);
        let i = self.gamepad_nav.curve_dot.min(pts.len() - 1);
        // Cleared each frame; set true below only while North is held (bias mode).
        self.gamepad_nav.curve_bias = false;

        // East / South → back to dot navigation.
        if nav.is_rising("btn_east") || nav.is_rising("btn_south") {
            self.gamepad_nav.edit_level = EditLevel::CurveDots;
            self.nav_publish_curve_sel(ctx, inner, false);
            return;
        }
        // West → toggle fine increments.
        if nav.is_rising("btn_west") {
            self.gamepad_nav.fine_increment = !self.gamepad_nav.fine_increment;
        }

        let fine = self.gamepad_nav.fine_increment;
        let mag = nav.lstick.length();

        // Hold North → adjust segment curvature (bias) instead of moving the
        // dot. Bias spans only [-1, 1], so rates are deliberately gentle — a
        // full-deflection hold takes several seconds to cross the range, and
        // fine is much slower again for precise shaping.
        if nav.pressed.contains("btn_north") {
            self.gamepad_nav.curve_bias = true;
            let mut db = 0.0f32;
            let s = if fine { 0.003 } else { 0.012 }; // per dpad press
            // Discrete: dpad rising edges only (stick is the continuous path).
            if nav.is_rising("dpad_right") || nav.is_rising("dpad_up") { db += s; }
            if nav.is_rising("dpad_left") || nav.is_rising("dpad_down") { db -= s; }
            if mag > 0.3 {
                // Continuous: ~0.15/s coarse, ~0.03/s fine at full deflection.
                let rate = if fine { 0.03 } else { 0.15 };
                db += nav.lstick.y * rate * dt;
            }
            if db != 0.0 { self.nav_curve_adjust_bias(outer_id, i, db); }
            self.nav_publish_curve_sel(ctx, inner, true);
            // Tell the body to show its bias (curvature) handles this frame.
            let pass = ctx.cumulative_pass_nr();
            ctx.data_mut(|d| d.insert_temp(
                egui::Id::new(("gp_nav_curve_bias", inner.0)), pass));
            return;
        }

        // Move the dot in X/Y. dpad = one discrete step (rising edge only);
        // LS = continuous. Using dpad rising avoids double-applying when the
        // stick auto-repeat would also fill `step_dir`.
        let _ = step_dir;
        let step = if fine { 0.0015 } else { 0.015 };
        let mut dx = 0.0f32;
        let mut dy = 0.0f32;
        if nav.is_rising("dpad_left") { dx -= step; }
        if nav.is_rising("dpad_right") { dx += step; }
        if nav.is_rising("dpad_up") { dy += step; }
        if nav.is_rising("dpad_down") { dy -= step; }
        if mag > 0.08 {
            // Accelerated stick response (gentle first half, fast toward full
            // deflection) — speed scales with |axis|^accel per axis, matching the
            // cursor feel. Coarse top ≈0.5/s, fine top ≈0.07/s (graph units/s).
            let accel = self.settings.cursor_accel.max(1.0);
            let top = if fine { 0.07 } else { 0.5 };
            let curve = |a: f32| -> f32 {
                a.signum() * a.abs().clamp(0.0, 1.0).powf(accel) * top * dt
            };
            dx += curve(nav.lstick.x);
            dy += curve(nav.lstick.y); // +y up
        }
        if dx != 0.0 || dy != 0.0 {
            self.nav_curve_move_dot(outer_id, i, dx, dy);
        }
        self.nav_publish_curve_sel(ctx, inner, true);
    }
}
