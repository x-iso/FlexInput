//! Vec Reshaper body: boundary/gain curve editor + live 2D pad.

use super::*;

// ── Vec Reshaper body ─────────────────────────────────────────────────────────
//
// Two coupled views:
//   • a curve editor (X = direction, 0 = cardinal axis → 1 = diagonal;
//     Y = boundary radius OR directional gain, selected by the Edit toggle),
//     reusing the response-curve drag/add/remove/bias idiom; and
//   • a live 2D pad showing the unit circle, the reshaped gate boundary, the
//     gain field, and the live input → output dots computed with the exact
//     engine transform (`vec_reshape_apply`).
//
// Params: boundary_pts / gain_pts (+ gain_biases), symmetry, renorm, in_max,
// out_max, grid_a, snap, trail_ms. One quadrant is edited; the pad mirrors it.

/// Read a reshaper control-point array from params, or a default when missing.
pub(crate) fn reshape_read_pts(node: &NodeData, key: &str, default: &[[f32; 2]]) -> Vec<[f32; 2]> {
    node.params.get(key).and_then(|v| v.as_array()).map(|arr| {
        arr.iter().filter_map(|p| {
            let a = p.as_array()?;
            Some([a.get(0)?.as_f64()? as f32, a.get(1)?.as_f64()? as f32])
        }).collect::<Vec<_>>()
    }).filter(|v: &Vec<[f32; 2]>| v.len() >= 2).unwrap_or_else(|| default.to_vec())
}

/// All Vec Reshaper transform params, pulled from a node in one shot. Shared by
/// the editor body and every pinned-element renderer so the pad/curve draw
/// identically wherever they appear.
pub(crate) struct ReshapeParams {
    boundary: Vec<[f32; 2]>,
    gain: Vec<[f32; 2]>,
    gain_biases: Vec<f32>,
    symmetry: String,
    renorm: bool,
    in_max: f32,
    out_max: f32,
    grid_x: usize,
    grid_y: usize,
    snap: bool,
    trail_ms: i64,
    edit_target: String,
}

impl ReshapeParams {
    fn read(node: &NodeData) -> Self {
        ReshapeParams {
            boundary: reshape_read_pts(node, "boundary_pts", VEC_RESHAPE_BOUNDARY_DEFAULT),
            gain: reshape_read_pts(node, "gain_pts", VEC_RESHAPE_GAIN_DEFAULT),
            gain_biases: node.params.get("gain_biases").and_then(|v| v.as_array())
                .map(|a| a.iter().filter_map(|x| x.as_f64().map(|f| f as f32)).collect()).unwrap_or_default(),
            symmetry: node.params.get("symmetry").and_then(|v| v.as_str()).unwrap_or("quad4").to_string(),
            renorm: node.params.get("renorm").and_then(|v| v.as_bool()).unwrap_or(true),
            in_max: node.params.get("in_max").and_then(|v| v.as_f64()).unwrap_or(1.0) as f32,
            out_max: node.params.get("out_max").and_then(|v| v.as_f64()).unwrap_or(1.0) as f32,
            grid_x: node.params.get("grid_x").and_then(|v| v.as_i64()).unwrap_or(4).max(1) as usize,
            grid_y: node.params.get("grid_y").and_then(|v| v.as_i64()).unwrap_or(4).max(1) as usize,
            snap: node.params.get("snap").and_then(|v| v.as_bool()).unwrap_or(false),
            trail_ms: node.params.get("trail_ms").and_then(|v| v.as_i64()).unwrap_or(300).clamp(0, 1000),
            edit_target: node.params.get("edit_target").and_then(|v| v.as_str()).unwrap_or("gain").to_string(),
        }
    }

    /// Output magnitude for a full-deflection input in direction `theta` (rad),
    /// normalised so 1.0 = the unit circle. This is the reachable ENVELOPE the
    /// pad plots as the green outline — it exceeds 1.0 on the diagonal for a
    /// square boundary.
    fn envelope_at(&self, theta: f32) -> f32 {
        let dir = glam::Vec2::new(theta.cos(), theta.sin());
        let out = vec_reshape_apply(dir * self.in_max, &self.boundary, &self.gain,
            &self.gain_biases, &self.symmetry, self.renorm, self.in_max, self.out_max);
        out.length() / self.out_max.max(f32::EPSILON)
    }

    /// Local radial stretch at input point `v` (input-space, pre-normalised to
    /// in_max): `(out_mag/in_mag) / (out_max/in_max)`. 1.0 = neutral, >1 =
    /// accelerated (blue), <1 = decelerated (red). Used for the gradient field.
    fn stretch_at(&self, v: glam::Vec2) -> f32 {
        let m = v.length();
        if m < 1e-4 { return 1.0; }
        let out = vec_reshape_apply(v, &self.boundary, &self.gain, &self.gain_biases,
            &self.symmetry, self.renorm, self.in_max, self.out_max);
        let nominal = self.out_max / self.in_max.max(f32::EPSILON);
        (out.length() / m) / nominal.max(f32::EPSILON)
    }
}

/// Map a stretch factor (1.0 = neutral) to a hue: blue = accelerate (>1),
/// red = decelerate (<1), transparent at neutral. `strength` scales the alpha.
pub(crate) fn reshape_stretch_color(stretch: f32, strength: f32) -> Color32 {
    // Log-symmetric so a 2× stretch and a ½× squeeze read equally strong.
    let s = stretch.max(1e-3).ln() / std::f32::consts::LN_2; // ±1 ≈ 2×/½×
    let t = (s.abs()).clamp(0.0, 1.0);
    let a = (t * strength).clamp(0.0, 1.0);
    let (r, g, b) = if s >= 0.0 {
        (70.0, 150.0, 255.0)   // blue — accelerate / stretched
    } else {
        (255.0, 80.0, 70.0)    // red — decelerate / squeezed
    };
    Color32::from_rgba_unmultiplied((r * a) as u8, (g * a) as u8, (b * a) as u8, (200.0 * a) as u8)
}

/// Build (or fetch from cache) a smooth stretch-field texture for the pad. The
/// field is computed at `RES`×`RES` in INPUT space over [-1,1]² and uploaded
/// once per unique parameter set; LINEAR filtering then gives curved, smooth
/// gradients at any pad size instead of a blocky per-vertex mesh. Cached in
/// `ctx.data` keyed by a signature of the transform params, so it only
/// recomputes when the curves/params actually change.
pub(crate) fn reshape_field_texture(ui: &egui::Ui, salt: egui::Id, p: &ReshapeParams) -> egui::TextureHandle {
    use std::hash::{Hash, Hasher};
    // Signature: quantised params + curve points. Quantising floats keeps the
    // key stable across sub-pixel repaint jitter while still busting on edits.
    let mut h = std::collections::hash_map::DefaultHasher::new();
    let q = |f: f32| (f * 4096.0).round() as i64;
    for pt in &p.boundary { q(pt[0]).hash(&mut h); q(pt[1]).hash(&mut h); }
    for pt in &p.gain     { q(pt[0]).hash(&mut h); q(pt[1]).hash(&mut h); }
    for b in &p.gain_biases { q(*b).hash(&mut h); }
    p.symmetry.hash(&mut h);
    p.renorm.hash(&mut h);
    q(p.in_max).hash(&mut h); q(p.out_max).hash(&mut h);
    let sig = h.finish();

    // One slot PER PAD (keyed by `salt`), holding (signature, handle). Only the
    // latest field texture is retained; when the signature changes we replace it,
    // dropping the previous handle so the GPU texture is freed — no per-edit leak.
    let cache_key = salt.with("vrs_field_tex");
    if let Some((cached_sig, t)) = ui.ctx().data(|d| d.get_temp::<(u64, egui::TextureHandle)>(cache_key)) {
        if cached_sig == sig { return t; }
    }

    const RES: usize = 128;
    let mut pixels = vec![Color32::TRANSPARENT; RES * RES];
    for iy in 0..RES {
        // Image row 0 = top = +Y; flip so math Y points up.
        let fy = 1.0 - (iy as f32 + 0.5) / RES as f32 * 2.0;
        for ix in 0..RES {
            let fx = (ix as f32 + 0.5) / RES as f32 * 2.0 - 1.0;
            let vin = glam::Vec2::new(fx, fy);
            let len = vin.length();
            if len > 1.32 { continue; }
            // Soft edge past the unit circle so the disc doesn't hard-clip.
            let disc = (1.0 - (len - 1.0).max(0.0) * 3.2).clamp(0.0, 1.0);
            pixels[iy * RES + ix] = reshape_stretch_color(p.stretch_at(vin), 0.95 * disc);
        }
    }
    let img = egui::ColorImage { size: [RES, RES], pixels, source_size: egui::Vec2::new(RES as f32, RES as f32) };
    let handle = ui.ctx().load_texture(format!("vrs_field_{sig:x}"), img, egui::TextureOptions::LINEAR);
    // Replace the slot; the previous handle drops here → its GPU texture frees.
    ui.ctx().data_mut(|d| d.insert_temp(cache_key, (sig, handle.clone())));
    handle
}

/// Draw the live 2D pad: gradient stretch field + reference circle + reshaped
/// envelope outline + gain heat ring + live input→output dots. Shared by the
/// body and the pinned "pad" renderer.
pub(crate) fn draw_reshape_pad(ui: &egui::Ui, salt: egui::Id, rect: egui::Rect, p: &ReshapeParams, live_vec: Option<glam::Vec2>) {
    let painter = ui.painter_at(rect);
    let c = rect.center();
    let r = rect.width().min(rect.height()) * 0.5 - 2.0;
    // Map a Vec2 in [-1,1]² (output-normalised) to screen (Y up).
    let v2s = |v: glam::Vec2| egui::pos2(c.x + v.x * r, c.y - v.y * r);
    painter.rect_filled(rect, 3.0, GRAPH_BG_DEFAULT);

    // ── Gradient stretch field (smooth cached texture, bilinear) ──────────────
    // Blue = accelerate/stretched, red = decelerate/squeezed, transparent at
    // neutral. Sampled in INPUT space; the texture covers the [-1,1]² box so we
    // paint it into the disc's bounding square.
    let tex = reshape_field_texture(ui, salt, p);
    let field_rect = egui::Rect::from_center_size(c, egui::vec2(r * 2.0, r * 2.0));
    let uv = egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0));
    painter.image(tex.id(), field_rect, uv, Color32::WHITE);

    // Reference unit circle + crosshair.
    let refc = Color32::from_rgba_unmultiplied(150, 150, 150, 110);
    painter.circle_stroke(c, r, egui::Stroke::new(1.0, refc));
    painter.line_segment([egui::pos2(rect.left(), c.y), egui::pos2(rect.right(), c.y)], egui::Stroke::new(0.5, refc));
    painter.line_segment([egui::pos2(c.x, rect.top()), egui::pos2(c.x, rect.bottom())], egui::Stroke::new(0.5, refc));
    // Full-square reference (the √2 envelope target) so "how close to square" reads.
    let sq = Color32::from_rgba_unmultiplied(150, 150, 150, 50);
    painter.rect_stroke(egui::Rect::from_center_size(c, egui::vec2(r * 2.0, r * 2.0)), 0.0,
        egui::Stroke::new(0.75, sq), egui::StrokeKind::Inside);

    // Reshaped envelope outline (the reachable output gate).
    let nseg = 128usize;
    let gate: Vec<egui::Pos2> = (0..=nseg).map(|i| {
        let th = i as f32 / nseg as f32 * std::f32::consts::TAU;
        v2s(glam::Vec2::new(th.cos(), th.sin()) * p.envelope_at(th))
    }).collect();
    for wv in gate.windows(2) {
        painter.line_segment([wv[0], wv[1]], egui::Stroke::new(1.5, Color32::from_rgb(120, 220, 140)));
    }

    // Live input (blue) → output (green) dots.
    if let Some(v) = live_vec {
        let inp = v / p.in_max.max(f32::EPSILON);
        painter.circle_filled(v2s(inp), 3.0, Color32::from_rgba_unmultiplied(120, 180, 255, 220));
        let out = vec_reshape_apply(v, &p.boundary, &p.gain, &p.gain_biases, &p.symmetry, p.renorm, p.in_max, p.out_max) / p.out_max.max(f32::EPSILON);
        painter.line_segment([v2s(inp), v2s(out)], egui::Stroke::new(0.75, Color32::from_rgba_unmultiplied(210, 210, 210, 130)));
        painter.circle_filled(v2s(out), 4.0, Color32::from_rgb(140, 255, 170));
        request_repaint_throttled(ui.ctx());
    }
}

#[allow(clippy::too_many_lines)]
pub(crate) fn show_vec_reshape_body(node_id: NodeId, inputs: &[InPin], outputs: &[OutPin], ui: &mut egui::Ui, snarl: &mut Snarl<NodeData>) -> bool {
    let _ = (inputs, outputs);
    // ── Init params on first use ──────────────────────────────────────────────
    let needs_init = snarl.get_node(node_id).map(|n| !n.params.contains_key("boundary_pts")).unwrap_or(false);
    if needs_init {
        if let Some(node) = snarl.get_node_mut(node_id) {
            node.params.insert("boundary_pts".into(),
                serde_json::json!(VEC_RESHAPE_BOUNDARY_DEFAULT.iter().map(|p| serde_json::json!([p[0], p[1]])).collect::<Vec<_>>()));
            node.params.insert("gain_pts".into(),
                serde_json::json!(VEC_RESHAPE_GAIN_DEFAULT.iter().map(|p| serde_json::json!([p[0], p[1]])).collect::<Vec<_>>()));
            node.params.insert("gain_biases".into(), serde_json::json!([0.0]));
            node.params.insert("symmetry".into(),  Value::String("quad4".into()));
            node.params.insert("renorm".into(),    Value::Bool(true));
            node.params.insert("in_max".into(),    serde_json::json!(1.0f64));
            node.params.insert("out_max".into(),   serde_json::json!(1.0f64));
            node.params.insert("grid_x".into(),    serde_json::json!(4i64));
            node.params.insert("grid_y".into(),    serde_json::json!(4i64));
            node.params.insert("snap".into(),      Value::Bool(false));
            node.params.insert("trail_ms".into(),  serde_json::json!(300i64));
            node.params.insert("edit_target".into(), Value::String("gain".into()));
        }
    }

    let p = snarl.get_node(node_id).map(ReshapeParams::read)
        .unwrap_or_else(|| ReshapeParams {
            boundary: VEC_RESHAPE_BOUNDARY_DEFAULT.to_vec(), gain: VEC_RESHAPE_GAIN_DEFAULT.to_vec(),
            gain_biases: vec![], symmetry: "quad4".into(), renorm: true, in_max: 1.0, out_max: 1.0,
            grid_x: 4, grid_y: 4, snap: false, trail_ms: 300, edit_target: "gain".into() });
    let editing_gain = p.edit_target == "gain";
    // Editor Y range: boundary radius tops out at the square corner (√2); gain
    // ranges 0..3 (unity = 1). The curve stores REAL values; the editor draws in
    // 0..1 graph space (value / y_max) so grid/snap math is uniform.
    let y_max = if editing_gain { 3.0f32 } else { std::f32::consts::SQRT_2 };

    let live_vec: Option<glam::Vec2> = snarl.get_node(node_id)
        .and_then(|n| n.extra.last_signals.first().cloned().flatten())
        .and_then(|s| match s { Signal::Vec2(v) => Some(v), _ => None });

    let mut changed = false;   // any param mutated → request undo push
    let changed_inner = std::cell::Cell::new(false); // set inside Resize closures
    let mut pad_rect: Option<egui::Rect> = None;
    let mut graph_rect: Option<egui::Rect> = None;

    ui.vertical(|ui| {
        // ── Edit-target toggle ────────────────────────────────────────────────
        let mut new_target = p.edit_target.clone();
        let tgt_resp = ui.horizontal(|ui| {
            ui.label(egui::RichText::new("Edit").small().weak());
            if ui.selectable_label(editing_gain, egui::RichText::new("Gain").small())
                .on_hover_text("Redistribute deflection WITHIN the boundary (accelerate / decelerate a direction). Does not change the outer shape.").clicked() { new_target = "gain".into(); }
            if ui.selectable_label(!editing_gain, egui::RichText::new("Boundary").small())
                .on_hover_text("Shape the OUTER reach per direction — expand the circle out toward the square's corners (needs Renorm on).").clicked() { new_target = "boundary".into(); }
            ui.separator();
            ui.label(egui::RichText::new(if editing_gain { "▲ stretch / ▼ squeeze" } else { "▲ toward square" })
                .small().weak().color(Color32::from_gray(120)));
        });
        register_exposable_element(ui, node_id, "target_row", tgt_resp.response.rect);
        if new_target != p.edit_target {
            if let Some(node) = snarl.get_node_mut(node_id) {
                node.params.insert("edit_target".into(), Value::String(new_target.clone()));
            }
            changed = true;
        }

        // ── Curve editor (direction → radius / gain), grid + snap on BOTH axes ─
        // The BODY owns the Resize handle; the editor draws into the allocated
        // rect. (Pinned mirrors skip the Resize and pass their container rect.)
        let g_size = read_widget_size(snarl, node_id, "vrs_graph_size", egui::vec2(190.0, 130.0));
        let new_g = egui::Resize::default()
            .id_salt(("vreshape_graph_rs", node_id))
            .default_size(g_size)
            .min_size(egui::vec2(90.0, 70.0))
            .max_size(egui::vec2(600.0, 420.0))
            .show(ui, |ui| {
                let (rect, _) = ui.allocate_exact_size(ui.available_size(), egui::Sense::hover());
                graph_rect = Some(rect);
                if draw_reshape_curve_editor(ui, node_id, snarl, &p, editing_gain, y_max, live_vec, rect) {
                    changed_inner.set(true);
                }
                ui.min_rect().size()
            });
        if (new_g - g_size).length() > 0.5 { write_widget_size(snarl, node_id, "vrs_graph_size", new_g); }

        // ── Live 2D preview pad (re-read params so edits above show instantly) ─
        // Square widget, centred in a resizable square frame.
        let p2 = snarl.get_node(node_id).map(ReshapeParams::read).unwrap_or(p);
        let pad_sz = read_widget_size(snarl, node_id, "vrs_pad_size", egui::vec2(150.0, 150.0));
        let new_pad = egui::Resize::default()
            .id_salt(("vreshape_pad_rs", node_id))
            .default_size(pad_sz)
            .min_size(egui::vec2(80.0, 80.0))
            .max_size(egui::vec2(420.0, 420.0))
            .show(ui, |ui| {
                let side = ui.available_size().min_elem().max(40.0);
                let (frame, _) = ui.allocate_exact_size(egui::vec2(side, side), egui::Sense::hover());
                pad_rect = Some(frame);
                draw_reshape_pad(ui, egui::Id::new(("vrs_pad", node_id.0)), frame, &p2, live_vec);
                egui::vec2(side, side)
            });
        if (new_pad - pad_sz).length() > 0.5 { write_widget_size(snarl, node_id, "vrs_pad_size", new_pad); }

        // ── Controls ──────────────────────────────────────────────────────────
        let mut sym = p2.symmetry.clone();
        let mut rn = p2.renorm;
        let mut im = p2.in_max;
        let mut om = p2.out_max;
        let mut gx = p2.grid_x as f64;
        let mut gy = p2.grid_y as f64;
        let mut sn = p2.snap;
        let mut tm = p2.trail_ms;

        let opt_resp = ui.horizontal(|ui| {
            egui::ComboBox::from_id_salt(("vrs_sym", node_id))
                .width(78.0)
                .selected_text(match sym.as_str() { "xmirror" => "X-mirror", _ => "4-way" })
                .show_ui(ui, |ui| {
                    changed |= ui.selectable_value(&mut sym, "quad4".into(),   "4-way").changed();
                    changed |= ui.selectable_value(&mut sym, "xmirror".into(), "X-mirror").changed();
                });
            let rn_before = rn;
            ui.checkbox(&mut rn, egui::RichText::new("Renorm").small())
                .on_hover_text("On: expand output to the boundary shape (circle→square). Off: boundary is a display-only reference; only gain shapes the feel.");
            changed |= rn != rn_before;
        });
        register_exposable_element(ui, node_id, "options_row", opt_resp.response.rect);

        let range_resp = ui.horizontal(|ui| {
            ui.label(egui::RichText::new("In max").small().weak());
            changed |= ui.add(egui::DragValue::new(&mut im).speed(0.01).range(0.05..=2.0).max_decimals(2)).changed();
            ui.separator();
            ui.label(egui::RichText::new("Out max").small().weak());
            changed |= ui.add(egui::DragValue::new(&mut om).speed(0.01).range(0.05..=2.0).max_decimals(2)).changed();
        });
        register_exposable_element(ui, node_id, "range_row", range_resp.response.rect);

        let grid_resp = ui.horizontal(|ui| {
            ui.label(egui::RichText::new("Grid").small().weak());
            changed |= ui.add(egui::DragValue::new(&mut gx).speed(0.25).range(1.0..=16.0).max_decimals(0).prefix("H ")).changed();
            changed |= ui.add(egui::DragValue::new(&mut gy).speed(0.25).range(1.0..=16.0).max_decimals(0).prefix("V ")).changed();
            let sn_before = sn;
            ui.checkbox(&mut sn, egui::RichText::new("Snap").small());
            changed |= sn != sn_before;
            ui.separator();
            ui.label(egui::RichText::new("Trail").small().weak());
            changed |= ui.add(egui::DragValue::new(&mut tm).speed(5.0).range(0i64..=1000).suffix("ms")).changed();
        });
        register_exposable_element(ui, node_id, "grid_row", grid_resp.response.rect);

        // Presets.
        let preset_resp = ui.horizontal(|ui| {
            ui.label(egui::RichText::new("Preset").small().weak());
            let mut apply: Option<(&[[f32; 2]], &[[f32; 2]])> = None;
            if ui.small_button("Circle").on_hover_text("Identity — round gate, unity gain").clicked() {
                apply = Some((&[[0.0, 1.0], [1.0, 1.0]], &[[0.0, 1.0], [1.0, 1.0]]));
            }
            if ui.small_button("Square").on_hover_text("Circle→square: the boundary reaches √2 on the diagonal so a round stick fills a square (Renorm on)").clicked() {
                // Envelope follows sec(θ) across the octant → √2 at the diagonal.
                apply = Some((&[[0.0, 1.0], [0.5, 1.08], [1.0, std::f32::consts::SQRT_2]], &[[0.0, 1.0], [1.0, 1.0]]));
            }
            if ui.small_button("Diag+").on_hover_text("Diagonal boost: accelerate the vector toward the corners to kill diagonal stickiness (gain only)").clicked() {
                apply = Some((&[[0.0, 1.0], [1.0, 1.0]], &[[0.0, 1.0], [0.5, 1.25], [1.0, 1.6]]));
            }
            if let Some((b, g)) = apply {
                if let Some(node) = snarl.get_node_mut(node_id) {
                    node.params.insert("boundary_pts".into(), serde_json::json!(b.iter().map(|p| serde_json::json!([p[0], p[1]])).collect::<Vec<_>>()));
                    node.params.insert("gain_pts".into(),     serde_json::json!(g.iter().map(|p| serde_json::json!([p[0], p[1]])).collect::<Vec<_>>()));
                    node.params.insert("gain_biases".into(),  serde_json::json!([0.0]));
                }
                changed = true;
            }
        });
        register_exposable_element(ui, node_id, "preset_row", preset_resp.response.rect);

        if changed {
            if let Some(node) = snarl.get_node_mut(node_id) {
                node.params.insert("symmetry".into(), Value::String(sym.clone()));
                node.params.insert("renorm".into(),   Value::Bool(rn));
                if let Some(n) = Number::from_f64(im as f64) { node.params.insert("in_max".into(),  Value::Number(n)); }
                if let Some(n) = Number::from_f64(om as f64) { node.params.insert("out_max".into(), Value::Number(n)); }
                node.params.insert("grid_x".into(),   serde_json::json!(gx as i64));
                node.params.insert("grid_y".into(),   serde_json::json!(gy as i64));
                node.params.insert("snap".into(),     Value::Bool(sn));
                node.params.insert("trail_ms".into(), serde_json::json!(tm));
            }
        }
    });

    changed |= changed_inner.get();
    if let Some(rect) = graph_rect { register_exposable_element(ui, node_id, "curve", rect); }
    if let Some(rect) = pad_rect   { register_exposable_element(ui, node_id, "pad",   rect); }
    changed
}

/// The direction→value curve editor (grid + snap on BOTH axes, draggable points,
/// Alt-bias on gain, live-direction marker, gamepad-nav geom publish). Draws
/// directly into the supplied `rect` (no inner Resize) so it fills whatever the
/// caller sized — an egui::Resize in the body, or a pinned container. Returns
/// true if any param changed.
#[allow(clippy::too_many_arguments)]
pub(crate) fn draw_reshape_curve_editor(
    ui: &mut egui::Ui,
    node_id: NodeId,
    snarl: &mut Snarl<NodeData>,
    p: &ReshapeParams,
    editing_gain: bool,
    y_max: f32,
    live_vec: Option<glam::Vec2>,
    rect: egui::Rect,
) -> bool {
    let (x_lo, x_hi, y_lo, y_hi) = (0.0f32, 1.0f32, 0.0f32, 1.0f32);
    let grid_x = p.grid_x;
    let grid_y = p.grid_y;
    let snap = p.snap;
    let symmetry = p.symmetry.clone();

    let mut pts = if editing_gain { p.gain.clone() } else { p.boundary.clone() };
    for q in pts.iter_mut() { q[1] = (q[1] / y_max).clamp(0.0, 1.0); }   // → 0..1 graph space
    let mut biases = if editing_gain { p.gain_biases.clone() } else { vec![] };
    let mut pts_changed = false;
    let mut bias_changed = false;

    // Draws directly into `rect` — no inner Resize. The BODY wraps this in an
    // egui::Resize (one handle there); a PINNED mirror passes its container rect
    // so the graph fills the user-sized container exactly (no second handle).
    {
            let bg_resp = ui.interact(rect, ui.id().with(("vrs_graph_bg", node_id)), egui::Sense::click());
            let painter = ui.painter_at(rect);
            let c2s = |x: f32, y: f32| egui::pos2(
                rect.left() + (x - x_lo) / (x_hi - x_lo) * rect.width(),
                rect.bottom() - (y - y_lo) / (y_hi - y_lo) * rect.height(),
            );
            let s2c = |pos: egui::Pos2| -> [f32; 2] {[
                (pos.x - rect.left()) / rect.width(),
                (rect.bottom() - pos.y) / rect.height(),
            ]};
            painter.rect_filled(rect, 2.0, GRAPH_BG_DEFAULT);

            let (grid_faint, grid_axis) = graph_grid_colors(None);
            let gs = egui::Stroke::new(0.5, grid_faint);
            // Vertical grid (direction).
            for i in 1..grid_x { let x = i as f32 / grid_x as f32; painter.line_segment([c2s(x, y_lo), c2s(x, y_hi)], gs); }
            // Horizontal grid (value) — the previously-missing Y axis.
            for i in 1..grid_y { let y = i as f32 / grid_y as f32; painter.line_segment([c2s(x_lo, y), c2s(x_hi, y)], gs); }
            // Unity reference line (radius/gain = 1) at 1/y_max.
            let unity = (1.0 / y_max).clamp(0.0, 1.0);
            painter.line_segment([c2s(x_lo, unity), c2s(x_hi, unity)], egui::Stroke::new(1.0, grid_axis));

            // Corner labels: axis↔diagonal (X), and value extremes (Y).
            let lbl = Color32::from_rgba_unmultiplied(170, 170, 170, 150);
            let f = egui::FontId::proportional(8.5);
            painter.text(egui::pos2(rect.left() + 2.0, rect.bottom() - 1.0), egui::Align2::LEFT_BOTTOM, "axis", f.clone(), lbl);
            painter.text(egui::pos2(rect.right() - 2.0, rect.bottom() - 1.0), egui::Align2::RIGHT_BOTTOM, "diag", f.clone(), lbl);
            painter.text(egui::pos2(rect.left() + 2.0, rect.top() + 1.0), egui::Align2::LEFT_TOP, &format!("{:.2}", y_max), f.clone(), lbl);

            // Snap helper: both axes, to the grid divisions.
            let do_snap = |x: f32, y: f32| -> (f32, f32) {
                if !snap { return (x, y); }
                let sx = (x * grid_x as f32).round() / grid_x as f32;
                let sy = (y * grid_y as f32).round() / grid_y as f32;
                (sx, sy)
            };

            // Curve polyline.
            if pts.len() >= 2 {
                let steps = 100usize;
                let line: Vec<egui::Pos2> = (0..=steps).map(|i| {
                    let x = i as f32 / steps as f32;
                    let y = sample_curve(&pts, x, &biases).clamp(y_lo, y_hi);
                    c2s(x, y)
                }).collect();
                let col = if editing_gain { Color32::from_rgb(120, 220, 140) } else { Color32::from_rgb(120, 180, 255) };
                for w in line.windows(2) { painter.line_segment([w[0], w[1]], egui::Stroke::new(1.5, col)); }
            }

            // Bias handles (Alt) — gain curve only.
            let alt_held = ui.input(|i| i.modifiers.alt);
            if editing_gain && alt_held && pts.len() >= 2 {
                while biases.len() < pts.len() - 1 { biases.push(0.0); }
                for seg in 0..(pts.len() - 1) {
                    let mid_x = (pts[seg][0] + pts[seg + 1][0]) * 0.5;
                    let mid_y = sample_curve(&pts, mid_x, &biases).clamp(y_lo, y_hi);
                    let hpos  = c2s(mid_x, mid_y);
                    let hid   = ui.id().with(("vrs_bias", node_id, seg));
                    let hr    = ui.interact(egui::Rect::from_center_size(hpos, egui::Vec2::splat(14.0)), hid, egui::Sense::click_and_drag());
                    if hr.double_clicked() { biases[seg] = 0.0; bias_changed = true; }
                    else if hr.dragged() { biases[seg] = (biases[seg] - hr.drag_delta().y / rect.height()).clamp(-2.0, 2.0); bias_changed = true; }
                    painter.circle_filled(hpos, 3.5, Color32::from_rgb(255, 220, 50));
                }
            }

            // Draggable points.
            let mut remove_idx: Option<usize> = None;
            for i in 0..pts.len() {
                let [px, py] = pts[i];
                let screen = c2s(px, py);
                let pid = ui.id().with(("vrs_pt", node_id, i));
                let r = ui.interact(egui::Rect::from_center_size(screen, egui::Vec2::splat(12.0)), pid, egui::Sense::click_and_drag());
                if r.dragged() && !alt_held {
                    let dd = r.drag_delta();
                    let nx_raw = (px + dd.x / rect.width()).clamp(
                        pts.get(i.wrapping_sub(1)).map(|q| q[0] + 0.001).unwrap_or(0.0),
                        pts.get(i + 1).map(|q| q[0] - 0.001).unwrap_or(1.0));
                    let ny_raw = (py - dd.y / rect.height()).clamp(0.0, 1.0);
                    let (sx, sy) = do_snap(nx_raw, ny_raw);
                    // Endpoints keep their angle pinned (0 / 1); only Y moves.
                    let nx = if i == 0 { 0.0 } else if i == pts.len() - 1 { 1.0 } else { sx };
                    pts[i] = [nx, sy];
                    pts_changed = true;
                }
                if r.secondary_clicked() && pts.len() > 2 && i != 0 && i != pts.len() - 1 {
                    remove_idx = Some(i); pts_changed = true;
                }
                let col = if r.hovered() || r.dragged() { Color32::WHITE } else { Color32::from_gray(200) };
                painter.circle_filled(screen, 4.0, col);
                painter.circle_stroke(screen, 4.0, egui::Stroke::new(1.0, Color32::from_gray(70)));
            }
            if let Some(idx) = remove_idx { pts.remove(idx); }
            if bg_resp.double_clicked() {
                if let Some(pos) = bg_resp.interact_pointer_pos() {
                    let [gx, gy] = s2c(pos);
                    let (gx, gy) = do_snap(gx.clamp(0.02, 0.98), gy.clamp(0.0, 1.0));
                    let idx = pts.partition_point(|q| q[0] < gx);
                    pts.insert(idx, [gx, gy]);
                    pts_changed = true;
                }
            }

            // Live direction marker.
            if let Some(v) = live_vec {
                if v.length() > 1e-4 {
                    let a01 = reshape_angle01_ui(v.y.atan2(v.x), &symmetry);
                    let y = sample_curve(&pts, a01, &biases).clamp(y_lo, y_hi);
                    let head = if editing_gain { Color32::from_rgb(140, 255, 170) } else { Color32::from_rgb(150, 200, 255) };
                    painter.line_segment([c2s(a01, y_lo), c2s(a01, y_hi)],
                        egui::Stroke::new(0.75, Color32::from_rgba_unmultiplied(head.r(), head.g(), head.b(), 90)));
                    painter.circle_filled(c2s(a01, y), 3.0, head);
                    request_repaint_throttled(ui.ctx());
                }
            }

            // Gamepad-nav geom publish (nav edits REAL values 0..y_max).
            let pass = ui.ctx().cumulative_pass_nr();
            let to_global = ui.ctx().layer_transform_to_global(ui.layer_id())
                .unwrap_or(egui::emath::TSTransform::IDENTITY);
            let screen_rect = to_global * rect;
            ui.ctx().data_mut(|d| d.insert_temp(
                egui::Id::new(("gp_nav_curve_geom", node_id.0)),
                (pass, screen_rect, x_lo, x_hi, 0.0f32, y_max)));
            let nav_sel: Option<(u64, usize, bool)> = ui.ctx()
                .data(|d| d.get_temp(egui::Id::new(("gp_nav_curve_sel", node_id.0))));
            if let Some((_, idx, editing)) = nav_sel.filter(|(pp, _, _)| *pp == pass) {
                if let Some(&[px, py]) = pts.get(idx) {
                    let ring = if editing { Color32::from_rgb(255, 210, 80) } else { Color32::from_rgb(120, 200, 255) };
                    painter.circle_stroke(c2s(px, py), 7.0, egui::Stroke::new(1.5, ring));
                }
            }
    }

    // De-normalise and write back.
    if pts_changed || bias_changed {
        for q in pts.iter_mut() { q[1] = (q[1] * y_max).max(0.0); }
        if let Some(node) = snarl.get_node_mut(node_id) {
            if pts_changed {
                let arr: Vec<Value> = pts.iter().map(|q| serde_json::json!([q[0], q[1]])).collect();
                node.params.insert(if editing_gain { "gain_pts" } else { "boundary_pts" }.into(), Value::Array(arr));
            }
            if editing_gain {
                biases.resize(pts.len().saturating_sub(1), 0.0);
                let bj: Vec<Value> = biases.iter().filter_map(|&b| Number::from_f64(b as f64).map(Value::Number)).collect();
                node.params.insert("gain_biases".into(), Value::Array(bj));
            }
        }
        return true;
    }
    false
}

/// UI-side mirror of `eval::reshape_angle01` (kept private in the engine). Folds
/// a direction angle into the edited quadrant → 0 (cardinal) .. 1 (diagonal).
pub(crate) fn reshape_angle01_ui(theta: f32, symmetry: &str) -> f32 {
    use std::f32::consts::{FRAC_PI_2, FRAC_PI_4};
    match symmetry {
        "xmirror" => theta.sin().abs().clamp(0.0, 1.0).asin() / FRAC_PI_2,
        _ => {
            let s = theta.rem_euclid(FRAC_PI_2);
            let d = (s - FRAC_PI_4).abs();
            1.0 - (d / FRAC_PI_4)
        }
    }
}

// ── Vec Reshaper pinned-element renderers ─────────────────────────────────────
// Each renders ONE exposed element scaled to the pinned container, matching the
// Audio-Stream-Haptics / response-curve pattern (no whole-body crop).

/// Pinned live pad: the gradient field + envelope + dots. Fills the container
/// exactly (no inner Resize); the pad disc is a centred square within it, so the
/// widget stays 1:1 regardless of the container's aspect.
pub(crate) fn render_vec_reshape_pad(inner_id: NodeId, ui: &mut egui::Ui, snarl: &mut Snarl<NodeData>, container: egui::Vec2) {
    let p = match snarl.get_node(inner_id).map(ReshapeParams::read) { Some(p) => p, None => return };
    let live_vec = snarl.get_node(inner_id)
        .and_then(|n| n.extra.last_signals.first().cloned().flatten())
        .and_then(|s| match s { Signal::Vec2(v) => Some(v), _ => None });
    // Take the whole container, then centre a square inside it.
    let (rect, _) = ui.allocate_exact_size(egui::vec2(container.x.max(40.0), container.y.max(40.0)), egui::Sense::hover());
    let side = rect.width().min(rect.height());
    let square = egui::Rect::from_center_size(rect.center(), egui::vec2(side, side));
    draw_reshape_pad(ui, egui::Id::new(("vrs_pad_pin", inner_id.0)), square, &p, live_vec);
}

/// Pinned curve editor: fully interactive, fills the container (no inner Resize).
pub(crate) fn render_vec_reshape_curve(inner_id: NodeId, ui: &mut egui::Ui, snarl: &mut Snarl<NodeData>, container: egui::Vec2) {
    let p = match snarl.get_node(inner_id).map(ReshapeParams::read) { Some(p) => p, None => return };
    let editing_gain = p.edit_target == "gain";
    let y_max = if editing_gain { 3.0f32 } else { std::f32::consts::SQRT_2 };
    let live_vec = snarl.get_node(inner_id)
        .and_then(|n| n.extra.last_signals.first().cloned().flatten())
        .and_then(|s| match s { Signal::Vec2(v) => Some(v), _ => None });
    let (rect, _) = ui.allocate_exact_size(egui::vec2(container.x.max(60.0), container.y.max(50.0)), egui::Sense::hover());
    draw_reshape_curve_editor(ui, inner_id, snarl, &p, editing_gain, y_max, live_vec, rect);
}

/// Pinned Edit-target toggle (Gain / Boundary).
pub(crate) fn render_vec_reshape_target_row(inner_id: NodeId, ui: &mut egui::Ui, snarl: &mut Snarl<NodeData>, container: egui::Vec2) {
    let cur = snarl.get_node(inner_id)
        .and_then(|n| n.params.get("edit_target").and_then(|v| v.as_str()).map(str::to_string))
        .unwrap_or_else(|| "gain".into());
    ui.set_max_width(container.x);
    apply_widget_scale(ui, container, egui::vec2(160.0, 22.0));
    let mut fr = [egui::Rect::NOTHING; 2];
    let mut set: Option<&str> = None;
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new("Edit").weak());
        let g = ui.selectable_label(cur == "gain", "Gain");     fr[0] = g.rect; if g.clicked() { set = Some("gain"); }
        let b = ui.selectable_label(cur != "gain", "Boundary"); fr[1] = b.rect; if b.clicked() { set = Some("boundary"); }
    });
    publish_nav_field_rects(ui, inner_id, &fr);
    if let (Some(s), Some(node)) = (set, snarl.get_node_mut(inner_id)) {
        node.params.insert("edit_target".into(), Value::String(s.into()));
    }
}

/// Pinned Symmetry + Renorm row.
pub(crate) fn render_vec_reshape_options_row(inner_id: NodeId, ui: &mut egui::Ui, snarl: &mut Snarl<NodeData>, container: egui::Vec2) {
    let (mut sym, mut rn) = snarl.get_node(inner_id).map(|n| (
        n.params.get("symmetry").and_then(|v| v.as_str()).unwrap_or("quad4").to_string(),
        n.params.get("renorm").and_then(|v| v.as_bool()).unwrap_or(true),
    )).unwrap_or(("quad4".into(), true));
    ui.set_max_width(container.x);
    let s = apply_widget_scale(ui, container, egui::vec2(210.0, 22.0));
    let mut changed = false;
    let mut fr: Vec<egui::Rect> = Vec::with_capacity(2);
    ui.horizontal(|ui| {
        let cb = egui::ComboBox::from_id_salt(("vrs_sym_pin", inner_id))
            .width(80.0 * s)
            .selected_text(match sym.as_str() { "xmirror" => "X-mirror", _ => "4-way" })
            .show_ui(ui, |ui| {
                changed |= ui.selectable_value(&mut sym, "quad4".into(),   "4-way").changed();
                changed |= ui.selectable_value(&mut sym, "xmirror".into(), "X-mirror").changed();
            });
        fr.push(cb.response.rect);
        let r = ui.checkbox(&mut rn, egui::RichText::new("Renorm"));
        fr.push(r.rect); changed |= r.changed();
    });
    publish_nav_field_rects(ui, inner_id, &fr);
    if changed {
        if let Some(node) = snarl.get_node_mut(inner_id) {
            node.params.insert("symmetry".into(), Value::String(sym));
            node.params.insert("renorm".into(), Value::Bool(rn));
        }
    }
}

/// Pinned In max / Out max row.
pub(crate) fn render_vec_reshape_range_row(inner_id: NodeId, ui: &mut egui::Ui, snarl: &mut Snarl<NodeData>, container: egui::Vec2) {
    let (mut im, mut om) = snarl.get_node(inner_id).map(|n| (
        n.params.get("in_max").and_then(|v| v.as_f64()).unwrap_or(1.0),
        n.params.get("out_max").and_then(|v| v.as_f64()).unwrap_or(1.0),
    )).unwrap_or((1.0, 1.0));
    ui.set_max_width(container.x);
    apply_widget_scale(ui, container, egui::vec2(200.0, 22.0));
    let mut changed = false;
    let mut fr = [egui::Rect::NOTHING; 2];
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new("In max").weak());
        let a = ui.add(egui::DragValue::new(&mut im).speed(0.01).range(0.05..=2.0).max_decimals(2));
        fr[0] = a.rect; changed |= a.changed();
        ui.separator();
        ui.label(egui::RichText::new("Out max").weak());
        let b = ui.add(egui::DragValue::new(&mut om).speed(0.01).range(0.05..=2.0).max_decimals(2));
        fr[1] = b.rect; changed |= b.changed();
    });
    publish_nav_field_rects(ui, inner_id, &fr);
    if changed {
        if let Some(node) = snarl.get_node_mut(inner_id) {
            if let Some(n) = Number::from_f64(im) { node.params.insert("in_max".into(), Value::Number(n)); }
            if let Some(n) = Number::from_f64(om) { node.params.insert("out_max".into(), Value::Number(n)); }
        }
    }
}

/// Pinned Grid H/V + Snap + Trail row.
pub(crate) fn render_vec_reshape_grid_row(inner_id: NodeId, ui: &mut egui::Ui, snarl: &mut Snarl<NodeData>, container: egui::Vec2) {
    let (mut gx, mut gy, mut sn, mut tm) = snarl.get_node(inner_id).map(|n| (
        n.params.get("grid_x").and_then(|v| v.as_i64()).unwrap_or(4) as f64,
        n.params.get("grid_y").and_then(|v| v.as_i64()).unwrap_or(4) as f64,
        n.params.get("snap").and_then(|v| v.as_bool()).unwrap_or(false),
        n.params.get("trail_ms").and_then(|v| v.as_i64()).unwrap_or(300),
    )).unwrap_or((4.0, 4.0, false, 300));
    ui.set_max_width(container.x);
    apply_widget_scale(ui, container, egui::vec2(300.0, 22.0));
    let mut changed = false;
    let mut fr = [egui::Rect::NOTHING; 4];
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new("Grid").weak());
        let h = ui.add(egui::DragValue::new(&mut gx).speed(0.25).range(1.0..=16.0).max_decimals(0).prefix("H "));
        fr[0] = h.rect; changed |= h.changed();
        let v = ui.add(egui::DragValue::new(&mut gy).speed(0.25).range(1.0..=16.0).max_decimals(0).prefix("V "));
        fr[1] = v.rect; changed |= v.changed();
        let c = ui.checkbox(&mut sn, egui::RichText::new("Snap"));
        fr[2] = c.rect; changed |= c.changed();
        ui.separator();
        ui.label(egui::RichText::new("Trail").weak());
        let t = ui.add(egui::DragValue::new(&mut tm).speed(5.0).range(0i64..=1000).suffix("ms"));
        fr[3] = t.rect; changed |= t.changed();
    });
    publish_nav_field_rects(ui, inner_id, &fr);
    if changed {
        if let Some(node) = snarl.get_node_mut(inner_id) {
            node.params.insert("grid_x".into(), serde_json::json!(gx as i64));
            node.params.insert("grid_y".into(), serde_json::json!(gy as i64));
            node.params.insert("snap".into(), Value::Bool(sn));
            node.params.insert("trail_ms".into(), serde_json::json!(tm));
        }
    }
}

/// Pinned preset buttons (Circle / Square / Diag+).
pub(crate) fn render_vec_reshape_preset_row(inner_id: NodeId, ui: &mut egui::Ui, snarl: &mut Snarl<NodeData>, container: egui::Vec2) {
    ui.set_max_width(container.x);
    apply_widget_scale(ui, container, egui::vec2(200.0, 22.0));
    let mut apply: Option<(&[[f32; 2]], &[[f32; 2]])> = None;
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new("Preset").weak());
        if ui.small_button("Circle").clicked() { apply = Some((&[[0.0, 1.0], [1.0, 1.0]], &[[0.0, 1.0], [1.0, 1.0]])); }
        if ui.small_button("Square").clicked() { apply = Some((&[[0.0, 1.0], [0.5, 1.08], [1.0, std::f32::consts::SQRT_2]], &[[0.0, 1.0], [1.0, 1.0]])); }
        if ui.small_button("Diag+").clicked()  { apply = Some((&[[0.0, 1.0], [1.0, 1.0]], &[[0.0, 1.0], [0.5, 1.25], [1.0, 1.6]])); }
    });
    if let (Some((b, g)), Some(node)) = (apply, snarl.get_node_mut(inner_id)) {
        node.params.insert("boundary_pts".into(), serde_json::json!(b.iter().map(|p| serde_json::json!([p[0], p[1]])).collect::<Vec<_>>()));
        node.params.insert("gain_pts".into(),     serde_json::json!(g.iter().map(|p| serde_json::json!([p[0], p[1]])).collect::<Vec<_>>()));
        node.params.insert("gain_biases".into(),  serde_json::json!([0.0]));
    }
}
