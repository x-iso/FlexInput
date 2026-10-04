//! Response Curve / Vec Response Curve / Two-way Response Curve bodies.

use super::*;
use flexinput_engine::eval::{TwowayHyst, TWOWAY_HYST_MAX_PCT};

/// AutoMap Response Curve / AutoMap Two-way Curve: the plain curve's body, run
/// on the signal picked in the header (see `curve_ui_module_id`).
pub(crate) fn show_automap_curve_body(node_id: NodeId, inputs: &[InPin], outputs: &[OutPin], ui: &mut egui::Ui, snarl: &mut Snarl<NodeData>) -> bool {
    let Some(node) = snarl.get_node(node_id) else { return false };
    match curve_ui_module_id(node) {
        "module.vec_response_curve"    => show_vec_response_curve_body(node_id, inputs, outputs, ui, snarl),
        "module.twoway_response_curve" => show_twoway_response_curve_body(node_id, inputs, outputs, ui, snarl),
        _                              => show_response_curve_body(node_id, inputs, outputs, ui, snarl),
    }
}

pub(crate) fn show_response_curve_body(node_id: NodeId, inputs: &[InPin], outputs: &[OutPin], ui: &mut egui::Ui, snarl: &mut Snarl<NodeData>) -> bool {
    let automap = snarl.get_node(node_id).is_some_and(|n| is_automap_curve(&n.module_id));
    // Curve graphs intentionally do NOT force vsync repaint. The curve
    // itself is static and the only animated element is the input/output
    // tracer dot, which is plenty smooth at the user's chosen base
    // Repaint rate (30 Hz is imperceptibly different from 60 Hz for a
    // single moving dot). Forcing vsync here previously was the main
    // reason a multi-curve Easy patch sat at 17 % CPU — every visible
    // curve ratcheted the whole window up to monitor refresh rate.
    // ── Initialise params on first use ────────────────────────────────────────
    let needs_init = snarl.get_node(node_id).map(|n| !n.params.contains_key("points")).unwrap_or(false);
    if needs_init {
        if let Some(node) = snarl.get_node_mut(node_id) {
            node.params.insert("points".into(), serde_json::json!([[0.0, 0.0], [1.0, 1.0]]));
            node.params.insert("biases".into(),  serde_json::json!([0.0]));
            node.params.insert("absolute".into(), Value::Bool(true));
            node.params.insert("in_min".into(),   serde_json::json!(-1.0));
            node.params.insert("in_max".into(),   serde_json::json!( 1.0));
            node.params.insert("out_min".into(),  serde_json::json!(-1.0));
            node.params.insert("out_max".into(),  serde_json::json!( 1.0));
            node.params.insert("grid_x".into(),   serde_json::json!(4i64));
            node.params.insert("grid_y".into(),   serde_json::json!(4i64));
            node.params.insert("snap".into(),     Value::Bool(false));
            node.params.insert("scale_t".into(),  serde_json::json!(0.0f64));
            node.params.insert("trail_ms".into(), serde_json::json!(300i64));
        }
    }

    // ── Read params ───────────────────────────────────────────────────────────
    let (points, biases, absolute, in_min, in_max, out_min, out_max, grid_x, grid_y, snap, scale_t, trail_ms, show_scaled_grid, show_grid_labels) = snarl
        .get_node(node_id)
        .map(|n| {
            let pts: Vec<[f32; 2]> = n.params.get("points")
                .and_then(|v| v.as_array())
                .map(|arr| arr.iter().filter_map(|p| {
                    let a = p.as_array()?;
                    Some([a.get(0)?.as_f64()? as f32, a.get(1)?.as_f64()? as f32])
                }).collect())
                .unwrap_or_else(|| vec![[0.0, 0.0], [1.0, 1.0]]);
            let bss: Vec<f32> = n.params.get("biases")
                .and_then(|v| v.as_array())
                .map(|arr| arr.iter().filter_map(|b| b.as_f64().map(|f| f as f32)).collect())
                .unwrap_or_default();
            let abs  = n.params.get("absolute").and_then(|v| v.as_bool()).unwrap_or(true);
            let i0   = n.params.get("in_min") .and_then(|v| v.as_f64()).unwrap_or(-1.0) as f32;
            let i1   = n.params.get("in_max") .and_then(|v| v.as_f64()).unwrap_or( 1.0) as f32;
            let o0   = n.params.get("out_min").and_then(|v| v.as_f64()).unwrap_or(-1.0) as f32;
            let o1   = n.params.get("out_max").and_then(|v| v.as_f64()).unwrap_or( 1.0) as f32;
            let gx   = n.params.get("grid_x").and_then(|v| v.as_i64()).unwrap_or(4).max(1) as usize;
            let gy   = n.params.get("grid_y").and_then(|v| v.as_i64()).unwrap_or(4).max(1) as usize;
            let sn   = n.params.get("snap").and_then(|v| v.as_bool()).unwrap_or(false);
            let sc   = n.params.get("scale_t").and_then(|v| v.as_f64()).map(|f| f as f32)
                .unwrap_or_else(|| match n.params.get("in_scale").and_then(|v| v.as_i64()).unwrap_or(0) {
                    1 => -0.5, 2 => 0.5, _ => 0.0,
                });
            let tm   = n.params.get("trail_ms").and_then(|v| v.as_i64()).unwrap_or(300).clamp(0, 1000);
            let ssg  = n.params.get("show_scaled_grid").and_then(|v| v.as_bool()).unwrap_or(false);
            let sgl  = n.params.get("show_grid_labels").and_then(|v| v.as_bool()).unwrap_or(false);
            (pts, bss, abs, i0, i1, o0, o1, gx, gy, sn, sc, tm, ssg, sgl)
        })
        .unwrap_or_else(|| (vec![[0.0, 0.0], [1.0, 1.0]], vec![], true, -1.0, 1.0, -1.0, 1.0, 4, 4, false, 0.0f32, 300, false, false));

    let n_channels = snarl.get_node(node_id).map(curve_channels).unwrap_or(1);
    let legend = snarl.get_node(node_id).and_then(curve_channel_labels);
    let live_inputs: Vec<Option<f32>> = (0..n_channels)
        .map(|ch| snarl.get_node(node_id)
            .and_then(|n| n.extra.last_signals.get(ch)?.as_ref())
            .map(sig_f32))
        .collect();

    let (x_lo, x_hi): (f32, f32) = if absolute { (0.0, 1.0) } else { (-1.0, 1.0) };
    let (y_lo, y_hi): (f32, f32) = if absolute { (0.0, 1.0) } else { (-1.0, 1.0) };
    let x_range = x_hi - x_lo;
    let y_range = y_hi - y_lo;

    let mut new_points   = points.clone();
    let mut new_biases   = biases.clone();
    let mut pts_changed  = false;
    let mut bias_changed = false;

    let mut curve_graph_rect: Option<egui::Rect> = None;
    ui.vertical(|ui| {
        // ── Graph ─────────────────────────────────────────────────────────────
        egui::Resize::default()
            .id_salt(("crv", node_id))
            .default_size([180.0, 180.0])
            .min_size([80.0, 80.0])
            .show(ui, |ui| {
                let (rect, bg_resp) =
                    ui.allocate_exact_size(ui.available_size(), egui::Sense::click());
                curve_graph_rect = Some(rect);
                let painter = ui.painter_at(rect);

                let c2s = |x: f32, y: f32| egui::pos2(
                    rect.left() + (x - x_lo) / x_range * rect.width(),
                    rect.bottom() - (y - y_lo) / y_range * rect.height(),
                );
                let s2c = |pos: egui::Pos2| -> [f32; 2] {[
                    x_lo + (pos.x - rect.left()) / rect.width() * x_range,
                    y_lo + (rect.bottom() - pos.y) / rect.height() * y_range,
                ]};
                // Publish geometry for gamepad-nav (graph↔screen mapping), using
                // the same temp ids the multi-channel curve body uses. The rect
                // is transformed to GLOBAL (screen) space — in Easy mode the body
                // renders on a scaled/scrolled sub-layer, so the raw rect is in
                // body-local coords and would never match the screen-space cursor.
                let pass = ui.ctx().cumulative_pass_nr();
                let to_global = ui.ctx().layer_transform_to_global(ui.layer_id())
                    .unwrap_or(egui::emath::TSTransform::IDENTITY);
                let screen_rect = to_global * rect;
                ui.ctx().data_mut(|d| d.insert_temp(
                    egui::Id::new(("gp_nav_curve_geom", node_id.0)),
                    (pass, screen_rect, x_lo, x_hi, y_lo, y_hi)));
                // Read the nav-selected dot (pass, idx, editing) for the highlight.
                let nav_sel: Option<(u64, usize, bool)> = ui.ctx()
                    .data(|d| d.get_temp(egui::Id::new(("gp_nav_curve_sel", node_id.0))));
                let nav_sel_dot: Option<usize> = nav_sel
                    .filter(|(p, _, _)| crate::widgets::nav_pass_matches(ui.ctx(), *p))
                    .map(|(_, i, _)| i);
                // Compute grid node positions (including 0 and 1 endpoints) in
                // normalized [0,1] graph space, with redistribution of crowded lines.
                // In bidirectional mode (not absolute) scaling is applied symmetrically
                // from the centre (u=0.5 = value 0) outward, so each half is scaled
                // independently then merged.
                let redistribute = |mut nodes: Vec<f32>, n: usize| -> Vec<f32> {
                    let min_gap = 1.0f32 / n as f32 * 0.5;
                    for _ in 0..n {
                        nodes.sort_by(|a, b| a.partial_cmp(b).unwrap());
                        let crowded = (1..nodes.len().saturating_sub(1))
                            .filter(|&i| (nodes[i]-nodes[i-1]).min(nodes[i+1]-nodes[i]) < min_gap)
                            .min_by(|&a, &b| {
                                let ga = (nodes[a]-nodes[a-1]).min(nodes[a+1]-nodes[a]);
                                let gb = (nodes[b]-nodes[b-1]).min(nodes[b+1]-nodes[b]);
                                ga.partial_cmp(&gb).unwrap()
                            });
                        let Some(ci) = crowded else { break; };
                        nodes.remove(ci);
                        let (li, _) = nodes.windows(2).enumerate()
                            .max_by(|(_, a), (_, b)| (a[1]-a[0]).partial_cmp(&(b[1]-b[0])).unwrap())
                            .unwrap();
                        nodes.insert(li + 1, (nodes[li] + nodes[li+1]) * 0.5);
                    }
                    nodes.sort_by(|a, b| a.partial_cmp(b).unwrap());
                    nodes
                };
                let scaled_grid_positions = |n: usize| -> Vec<f32> {
                    if n == 0 { return vec![0.0, 1.0]; }
                    if !show_scaled_grid {
                        return (0..=n).map(|i| i as f32 / n as f32).collect();
                    }
                    if absolute {
                        // One-sided: scale the full [0,1] range (Log→dense near max).
                        let nodes = (0..=n).map(|i| {
                            let t = i as f32 / n as f32;
                            1.0 - curve_scale_inv(1.0 - t, scale_t)
                        }).collect();
                        redistribute(nodes, n)
                    } else {
                        // Bidirectional: each half is an independent abs-style scale
                        // expanding outward from the centre (u=0.5, value=0).
                        // Log→dense near ±max edges; Exp→dense near 0.
                        let half_lo = n / 2;
                        let half_hi = n - half_lo;
                        // lo half: t=0 is centre (u=0.5), t=1 is left edge (u=0).
                        let lo_nodes: Vec<f32> = (0..=half_lo).map(|i| {
                            let t = i as f32 / half_lo as f32;
                            let s = 1.0 - curve_scale_inv(1.0 - t, scale_t);
                            0.5 - s * 0.5
                        }).collect();
                        // hi half: t=0 is centre (u=0.5), t=1 is right edge (u=1).
                        let hi_nodes: Vec<f32> = (0..=half_hi).map(|i| {
                            let t = i as f32 / half_hi as f32;
                            let s = 1.0 - curve_scale_inv(1.0 - t, scale_t);
                            0.5 + s * 0.5
                        }).collect();
                        let mut merged = redistribute(lo_nodes, half_lo);
                        for v in redistribute(hi_nodes, half_hi).iter().skip(1) { merged.push(*v); }
                        merged.sort_by(|a, b| a.partial_cmp(b).unwrap());
                        merged
                    }
                };
                let snap_nodes_x = scaled_grid_positions(grid_x);
                let snap_nodes_y = scaled_grid_positions(grid_y);

                let do_snap = |x: f32, y: f32| -> (f32, f32) {
                    if !snap { return (x, y); }
                    let u = ((x - x_lo) / x_range).clamp(0.0, 1.0);
                    let v = ((y - y_lo) / y_range).clamp(0.0, 1.0);
                    let snap_u = snap_nodes_x.iter().copied()
                        .min_by(|a, b| (a - u).abs().partial_cmp(&(b - u).abs()).unwrap())
                        .unwrap_or(u);
                    let snap_v = snap_nodes_y.iter().copied()
                        .min_by(|a, b| (a - v).abs().partial_cmp(&(b - v).abs()).unwrap())
                        .unwrap_or(v);
                    (x_lo + snap_u * x_range, y_lo + snap_v * y_range)
                };

                painter.rect_filled(rect, 2.0, GRAPH_BG_DEFAULT);

                let grid_x_positions: Vec<f32> = (1..grid_x)
                    .map(|i| x_lo + snap_nodes_x[i] * x_range)
                    .collect();
                let grid_y_positions: Vec<f32> = (1..grid_y)
                    .map(|i| y_lo + snap_nodes_y[i] * y_range)
                    .collect();

                let (grid_faint, grid_axis) = graph_grid_colors(None);
                let gs = egui::Stroke::new(0.5, grid_faint);
                for &x in &grid_x_positions {
                    painter.line_segment([c2s(x, y_lo), c2s(x, y_hi)], gs);
                }
                for &y in &grid_y_positions {
                    painter.line_segment([c2s(x_lo, y), c2s(x_hi, y)], gs);
                }
                painter.line_segment([c2s(x_lo, y_lo), c2s(x_hi, y_hi)],
                    egui::Stroke::new(0.5, grid_axis));

                // Labels: show the real input/output value at each grid line.
                // Graph x is the SCALED domain (curve_scale maps real→graph), so
                // the real value at graph position x is curve_scale_inv(x) * abs_max
                // for abs mode, or sign*curve_scale_inv(|x|)*abs_max for bipolar.
                if show_grid_labels {
                    const MIN_LABEL_PX: f32 = 20.0;
                    let label_col = Color32::from_rgba_unmultiplied(180, 180, 180, 160);
                    let font = egui::FontId::proportional(9.0);
                    let abs_max_in  = in_max.abs().max(in_min.abs());
                    let abs_max_out = out_max.abs().max(out_min.abs());
                    // Convert a normalized graph position u∈[0,1] to the real input value.
                    let graph_to_real_in = |u: f32| -> f32 {
                        if absolute {
                            curve_scale_inv(u, scale_t) * abs_max_in
                        } else {
                            // u=0.5 is value 0; each half scaled independently
                            let centered = u * 2.0 - 1.0; // [-1,1]
                            let sign = if centered < 0.0 { -1.0f32 } else { 1.0 };
                            sign * curve_scale_inv(centered.abs(), scale_t) * abs_max_in
                        }
                    };
                    let graph_to_real_out = |v: f32| -> f32 {
                        if absolute {
                            curve_scale_inv(v, scale_t) * abs_max_out
                        } else {
                            let centered = v * 2.0 - 1.0;
                            let sign = if centered < 0.0 { -1.0f32 } else { 1.0 };
                            sign * curve_scale_inv(centered.abs(), scale_t) * abs_max_out
                        }
                    };
                    let mut last_sx = f32::NEG_INFINITY;
                    for &x in &grid_x_positions {
                        let sx = c2s(x, y_hi).x;
                        if sx - last_sx < MIN_LABEL_PX { continue; }
                        last_sx = sx;
                        let u = (x - x_lo) / x_range;
                        let val = graph_to_real_in(u);
                        let label = if abs_max_in <= 1.01 {
                            format!("{:.0}%", val * 100.0)
                        } else {
                            format!("{:.2}", val)
                        };
                        painter.text(egui::pos2(sx + 1.0, rect.top() + 1.0),
                            egui::Align2::LEFT_TOP, &label, font.clone(), label_col);
                    }
                    let mut last_sy = f32::INFINITY;
                    for &y in &grid_y_positions {
                        let sy = c2s(x_lo, y).y;
                        if last_sy - sy < MIN_LABEL_PX { continue; }
                        last_sy = sy;
                        let v = (y - y_lo) / y_range;
                        let val = graph_to_real_out(v);
                        let label = if abs_max_out <= 1.01 {
                            format!("{:.0}%", val * 100.0)
                        } else {
                            format!("{:.2}", val)
                        };
                        painter.text(egui::pos2(rect.left() + 1.0, sy - 9.0),
                            egui::Align2::LEFT_TOP, &label, font.clone(), label_col);
                    }
                }

                if new_points.len() >= 2 {
                    let steps = 120usize;
                    let curve_pts: Vec<egui::Pos2> = (0..=steps)
                        .map(|i| {
                            let x = x_lo + x_range * i as f32 / steps as f32;
                            let y = sample_curve(&new_points, x, &new_biases).clamp(y_lo, y_hi);
                            c2s(x, y)
                        })
                        .collect();
                    for w in curve_pts.windows(2) {
                        painter.line_segment([w[0], w[1]],
                            egui::Stroke::new(1.5, Color32::from_gray(200)));
                    }
                }

                // Bias handles show on mouse Alt OR when the gamepad driver is in
                // bias mode this frame (hold-North in CurveDot level).
                let nav_bias = ui.ctx().data(|d|
                    d.get_temp::<u64>(egui::Id::new(("gp_nav_curve_bias", node_id.0))))
                    .map_or(false, |p| crate::widgets::nav_pass_matches(ui.ctx(), p));
                let alt_held = ui.input(|i| i.modifiers.alt) || nav_bias;
                if alt_held && new_points.len() >= 2 {
                    while new_biases.len() < new_points.len() - 1 { new_biases.push(0.0); }
                    for seg in 0..(new_points.len() - 1) {
                        let mid_x = (new_points[seg][0] + new_points[seg + 1][0]) * 0.5;
                        let mid_y = sample_curve(&new_points, mid_x, &new_biases).clamp(y_lo, y_hi);
                        let hpos  = c2s(mid_x, mid_y);
                        let hid   = ui.id().with(("bias_h", node_id, seg));
                        let hresp = ui.interact(
                            egui::Rect::from_center_size(hpos, egui::Vec2::splat(14.0)),
                            hid, egui::Sense::click_and_drag());
                        if hresp.double_clicked() {
                            new_biases[seg] = 0.0;
                            bias_changed = true;
                        } else if hresp.dragged() {
                            let dy = -hresp.drag_delta().y / rect.height() * y_range;
                            new_biases[seg] = (new_biases[seg] + dy).clamp(-2.0, 2.0);
                            bias_changed = true;
                        }
                        let hcol = if hresp.hovered() || hresp.dragged() {
                            Color32::from_rgb(255, 220, 50)
                        } else {
                            Color32::from_rgb(180, 140, 20)
                        };
                        painter.circle_filled(hpos, 4.0, hcol);
                        painter.circle_stroke(hpos, 4.0,
                            egui::Stroke::new(1.0, Color32::from_gray(100)));
                    }
                }

                let mut remove_idx: Option<usize> = None;
                for i in 0..new_points.len() {
                    let [px, py] = new_points[i];
                    let screen   = c2s(px, py);
                    let pt_id    = ui.id().with(("cpt", node_id, i));
                    let pt_resp  = ui.interact(
                        egui::Rect::from_center_size(screen, egui::Vec2::splat(12.0)),
                        pt_id, egui::Sense::click_and_drag());

                    let origin_id = ui.id().with(("crv_pt_origin_inline", node_id, i));
                    if pt_resp.drag_started() && !alt_held {
                        ui.ctx().data_mut(|d| d.insert_temp(origin_id, [px, py, 0.0f32, 0.0f32]));
                    }
                    if pt_resp.dragged() && !alt_held {
                        let prev = ui.ctx().data(|d| d.get_temp::<[f32;4]>(origin_id))
                            .unwrap_or([px, py, 0.0, 0.0]);
                        let dd  = pt_resp.drag_delta();
                        let acc_x_px = prev[2] + dd.x;
                        let acc_y_px = prev[3] + dd.y;
                        ui.ctx().data_mut(|d| d.insert_temp(origin_id, [prev[0], prev[1], acc_x_px, acc_y_px]));
                        let nx_raw = prev[0] + acc_x_px * x_range / rect.width();
                        let ny_raw = prev[1] - acc_y_px * y_range / rect.height();
                        let lo_x   = new_points.get(i.wrapping_sub(1)).map(|p| p[0] + 0.001).unwrap_or(x_lo);
                        let hi_x   = new_points.get(i + 1).map(|p| p[0] - 0.001).unwrap_or(x_hi);
                        let (sx, sy) = do_snap(nx_raw, ny_raw);
                        new_points[i] = [sx.clamp(lo_x, hi_x), sy.clamp(y_lo, y_hi)];
                        pts_changed = true;
                    }
                    if pt_resp.drag_stopped() {
                        ui.ctx().data_mut(|d| d.remove_temp::<[f32;4]>(origin_id));
                    }
                    if pt_resp.secondary_clicked() && new_points.len() > 2 {
                        remove_idx = Some(i);
                        pts_changed = true;
                    }
                    // Gamepad-nav selected dot: accent glow ring so the user can
                    // see which point is targeted for move/delete.
                    if nav_sel_dot == Some(i) {
                        let accent = ui.visuals().selection.stroke.color;
                        let [r, g, b, _] = accent.to_array();
                        for k in 1..=4 {
                            let rad = 6.0 + k as f32 * 2.5;
                            let a = (120.0 * (1.0 - k as f32 / 5.0)) as u8;
                            painter.circle_stroke(screen, rad,
                                egui::Stroke::new(2.0, Color32::from_rgba_unmultiplied(r, g, b, a)));
                        }
                        painter.circle_stroke(screen, 7.0, egui::Stroke::new(2.0, accent));
                    }
                    let nav_here = nav_sel_dot == Some(i);
                    let col = if pt_resp.hovered() || pt_resp.dragged() || nav_here { Color32::WHITE } else { Color32::from_gray(190) };
                    painter.circle_filled(screen, 5.0, col);
                    painter.circle_stroke(screen, 5.0, egui::Stroke::new(1.0, Color32::from_gray(80)));
                }

                if let Some(idx) = remove_idx { curve_remove_point(&mut new_points, &mut new_biases, idx); }
                if bg_resp.double_clicked() {
                    if let Some(pos) = bg_resp.interact_pointer_pos() {
                        let [gx_raw, gy_raw] = s2c(pos);
                        let (gx_sn, gy_sn)   = do_snap(gx_raw, gy_raw);
                        let gx = gx_sn.clamp(x_lo, x_hi);
                        let gy = gy_sn.clamp(y_lo, y_hi);
                        let idx = new_points.partition_point(|p| p[0] < gx);
                        curve_insert_point(&mut new_points, &mut new_biases, idx, [gx, gy]);
                        pts_changed = true;
                    }
                }

                // Live-position trails — trail_ms history, y always recomputed
                // from the live curve so dragging control points leaves no streaks.
                let abs_max   = in_max.abs().max(in_min.abs()).max(f32::EPSILON);
                let trail_dur = std::time::Duration::from_millis(trail_ms as u64);
                let now       = std::time::Instant::now();
                let mut has_active = false;
                for (ch, raw_opt) in live_inputs.iter().enumerate() {
                    let Some(raw) = raw_opt else { continue; };
                    has_active = true;
                    let graph_x = if absolute {
                        curve_scale((raw.abs() / abs_max).clamp(0.0, 1.0), scale_t)
                    } else {
                        let in_range = (in_max - in_min).abs().max(f32::EPSILON);
                        let norm     = ((raw - in_min) / in_range * 2.0 - 1.0).clamp(-1.0, 1.0);
                        let sign     = if norm < 0.0 { -1.0f32 } else { 1.0 };
                        sign * curve_scale(norm.abs(), scale_t)
                    };
                    // Store only graph_x; y is recomputed at draw time from the current curve.
                    type Trail = std::collections::VecDeque<(f32, std::time::Instant)>;
                    let trail_id = ui.id().with(("trail", node_id, ch as u32));
                    let mut trail: Trail = ui.data(|d| d.get_temp::<Trail>(trail_id).clone().unwrap_or_default());
                    if trail_ms > 0 {
                        trail.push_back((graph_x, now));
                        while trail.front().map(|&(_, t)| now.duration_since(t) > trail_dur).unwrap_or(false) {
                            trail.pop_front();
                        }
                    } else {
                        trail.clear();
                    }
                    let trail_pts: Vec<(f32, std::time::Instant)> = trail.iter().cloned().collect();
                    ui.data_mut(|d| d.insert_temp(trail_id, trail));
                    let ch_col = MULTI_COLORS[ch % MULTI_COLORS.len()];
                    for w in trail_pts.windows(2) {
                        let (x0, _)  = w[0];
                        let (x1, t1) = w[1];
                        let age   = now.duration_since(t1).as_secs_f32() / trail_dur.as_secs_f32();
                        let alpha = ((1.0 - age.clamp(0.0, 1.0)) * 220.0) as u8;
                        let col   = Color32::from_rgba_unmultiplied(ch_col.r(), ch_col.g(), ch_col.b(), alpha);
                        let steps = (((x1 - x0).abs() / x_range * 80.0) as usize).max(1);
                        let x0_y  = sample_curve(&new_points, x0, &new_biases).clamp(y_lo, y_hi);
                        let mut prev = c2s(x0, x0_y);
                        for s in 1..=steps {
                            let t  = s as f32 / steps as f32;
                            let ix = x0 + (x1 - x0) * t;
                            let iy = sample_curve(&new_points, ix, &new_biases).clamp(y_lo, y_hi);
                            let next = c2s(ix, iy);
                            painter.line_segment([prev, next], egui::Stroke::new(1.5, col));
                            prev = next;
                        }
                    }
                    let graph_y = sample_curve(&new_points, graph_x, &new_biases).clamp(y_lo, y_hi);
                    let head_col = Color32::from_rgba_unmultiplied(ch_col.r(), ch_col.g(), ch_col.b(), 220);
                    painter.circle_filled(c2s(graph_x, graph_y), 3.5, head_col);
                }
                if has_active {
                    request_repaint_throttled(ui.ctx());
                }
                if let Some(labels) = legend {
                    paint_curve_legend(&painter, rect, labels, |ch| MULTI_COLORS[ch % MULTI_COLORS.len()]);
                }

                // Right-click on empty graph space → save/load/copy/paste/reset
                // (same handlers the header buttons use, so file format and
                // semantics are identical). A right-click on a control point
                // is captured by `pt_resp.secondary_clicked()` above, so this
                // menu only opens for clicks on graph background. On success
                // we resync the local working buffers so the writeback block
                // below doesn't clobber the change.
                let mut menu_mutated = false;
                bg_resp.context_menu(|ui| {
                    if curve_context_menu(ui, node_id, snarl, None) {
                        menu_mutated = true;
                    }
                });
                if menu_mutated {
                    if let Some(node) = snarl.get_node(node_id) {
                        let (pk, bk) = curve_param_keys(node);
                        new_points = node.params.get(pk).and_then(|v| v.as_array())
                            .map(|arr| arr.iter().filter_map(|p| {
                                let a = p.as_array()?;
                                Some([a.get(0)?.as_f64()? as f32, a.get(1)?.as_f64()? as f32])
                            }).collect())
                            .unwrap_or_else(|| vec![[0.0, 0.0], [1.0, 1.0]]);
                        new_biases = node.params.get(bk).and_then(|v| v.as_array())
                            .map(|arr| arr.iter().filter_map(|b| b.as_f64().map(|f| f as f32)).collect())
                            .unwrap_or_default();
                    }
                    pts_changed = false;
                    bias_changed = false;
                }
            });

        // ── Write back curve points / biases ──────────────────────────────────
        if pts_changed || bias_changed {
            if let Some(node) = snarl.get_node_mut(node_id) {
                if pts_changed {
                    new_biases.resize(new_points.len().saturating_sub(1), 0.0);
                    let json: Vec<Value> = new_points.iter().map(|p| serde_json::json!([p[0], p[1]])).collect();
                    node.params.insert("points".into(), Value::Array(json));
                }
                let bj: Vec<Value> = new_biases.iter()
                    .filter_map(|&b| Number::from_f64(b as f64).map(Value::Number))
                    .collect();
                node.params.insert("biases".into(), Value::Array(bj));
            }
        }

        // ── Controls below graph ──────────────────────────────────────────────
        let mut i0       = in_min;
        let mut i1       = in_max;
        let mut o0       = out_min;
        let mut o1       = out_max;
        let mut gx_f     = grid_x as f64;
        let mut gy_f     = grid_y as f64;
        let mut abs      = absolute;
        let mut snap_on  = snap;
        let mut sc_t     = scale_t;
        let mut tm       = trail_ms;
        let mut ssg      = show_scaled_grid;
        let mut sgl      = show_grid_labels;
        let mut changed  = false;

        // Row 1: Scale slider (Log←──●──→Exp, double-click resets) + Absolute + Snap
        let scale_resp = ui.horizontal(|ui| {
            ui.label(egui::RichText::new("Log").small().weak());
            let (slider_rect, slider_resp) = ui.allocate_exact_size(
                egui::vec2(80.0, 14.0), egui::Sense::click_and_drag(),
            );
            if slider_resp.double_clicked() {
                sc_t = 0.0;
                changed = true;
            } else if slider_resp.dragged() {
                sc_t = (sc_t + slider_resp.drag_delta().x / slider_rect.width() * 2.0).clamp(-1.0, 1.0);
                changed = true;
            }
            let painter = ui.painter_at(slider_rect);
            painter.rect_filled(slider_rect, 3.0, Color32::from_gray(35));
            let cx = slider_rect.center().x;
            painter.line_segment(
                [egui::pos2(cx, slider_rect.top() + 2.0), egui::pos2(cx, slider_rect.bottom() - 2.0)],
                egui::Stroke::new(1.0, Color32::from_gray(70)),
            );
            let knob_x = slider_rect.left() + (sc_t + 1.0) * 0.5 * slider_rect.width();
            painter.circle_filled(
                egui::pos2(knob_x, slider_rect.center().y), 5.0,
                if slider_resp.hovered() || slider_resp.dragged() { Color32::WHITE } else { Color32::from_gray(190) },
            );
            ui.label(egui::RichText::new("Exp").small().weak());
            ui.separator();
            let abs_before = abs;
            ui.checkbox(&mut abs, egui::RichText::new("Abs").small());
            changed |= abs != abs_before;
            let snap_before = snap_on;
            ui.checkbox(&mut snap_on, egui::RichText::new("Snap").small());
            changed |= snap_on != snap_before;
        });
        register_exposable_element(ui, node_id, "scale_row", scale_resp.response.rect);

        // Row 2: In/Out range
        let range_resp = ui.scope(|ui| {
            egui::Grid::new(("crv_rng", node_id)).num_columns(5).spacing([4.0, 2.0]).show(ui, |ui| {
                ui.label(egui::RichText::new("In").small().weak());
                changed |= ui.add(egui::DragValue::new(&mut i0).speed(0.01).prefix("↓").max_decimals(2)).changed();
                changed |= ui.add(egui::DragValue::new(&mut i1).speed(0.01).prefix("↑").max_decimals(2)).changed();
                ui.label(egui::RichText::new("Out").small().weak());
                changed |= ui.add(egui::DragValue::new(&mut o0).speed(0.01).prefix("↓").max_decimals(2)).changed();
                ui.end_row();
                ui.label(egui::RichText::new("").small());
                ui.label(egui::RichText::new("").small());
                ui.label(egui::RichText::new("").small());
                ui.label(egui::RichText::new("").small());
                changed |= ui.add(egui::DragValue::new(&mut o1).speed(0.01).prefix("↑").max_decimals(2)).changed();
                ui.end_row();
            });
        });
        register_exposable_element(ui, node_id, "range_row", range_resp.response.rect);

        // Row 3: Grid + Trail
        let grid_resp = ui.horizontal(|ui| {
            ui.label(egui::RichText::new("Grid").small().weak());
            changed |= ui.add(egui::DragValue::new(&mut gx_f).speed(0.25)
                .range(1.0..=20.0).max_decimals(0).prefix("H ")).changed();
            changed |= ui.add(egui::DragValue::new(&mut gy_f).speed(0.25)
                .range(1.0..=20.0).max_decimals(0).prefix("V ")).changed();
            ui.separator();
            ui.label(egui::RichText::new("Trail").small().weak());
            changed |= ui.add(egui::DragValue::new(&mut tm).speed(5.0)
                .range(0i64..=1000).suffix("ms")).changed();
        });
        register_exposable_element(ui, node_id, "grid_row", grid_resp.response.rect);

        // Row 4: Grid display options
        let grid_opts_resp = ui.horizontal(|ui| {
            let ssg_before = ssg;
            ui.checkbox(&mut ssg, egui::RichText::new("Scale grid").small())
                .on_hover_text("Adapt grid lines to the current Log/Exp scaling (Log compresses toward max, Exp toward min)");
            changed |= ssg != ssg_before;
            let sgl_before = sgl;
            ui.checkbox(&mut sgl, egui::RichText::new("Labels").small())
                .on_hover_text("Show value labels on grid lines");
            changed |= sgl != sgl_before;
        });
        register_exposable_element(ui, node_id, "grid_options_row", grid_opts_resp.response.rect);

        if changed {
            if let Some(node) = snarl.get_node_mut(node_id) {
                for (k, v) in [
                    ("in_min", i0 as f64), ("in_max", i1 as f64),
                    ("out_min", o0 as f64), ("out_max", o1 as f64),
                ] {
                    if let Some(n) = Number::from_f64(v) { node.params.insert(k.into(), Value::Number(n)); }
                }
                node.params.insert("absolute".into(), Value::Bool(abs));
                node.params.insert("grid_x".into(),   serde_json::json!(gx_f as i64));
                node.params.insert("grid_y".into(),   serde_json::json!(gy_f as i64));
                node.params.insert("snap".into(),     Value::Bool(snap_on));
                if let Some(n) = Number::from_f64(sc_t as f64) { node.params.insert("scale_t".into(), Value::Number(n)); }
                node.params.insert("trail_ms".into(),          serde_json::json!(tm));
                node.params.insert("show_scaled_grid".into(),  Value::Bool(ssg));
                node.params.insert("show_grid_labels".into(),  Value::Bool(sgl));
            }
        }

        // An AutoMap curve has the one channel its picked signal makes.
        if !automap { ui.horizontal(|ui| {
            ui.label(egui::RichText::new("Ch").small().weak());
            if ui.small_button("+").on_hover_text("Add parallel channel").clicked() {
                if let Some(node) = snarl.get_node_mut(node_id) {
                    let next = node.inputs.len() + 1;
                    node.inputs.push(PinDescriptor::new(format!("In {}", next), SignalType::Float));
                    node.outputs.push(PinDescriptor::new(format!("Out {}", next), SignalType::Float));
                }
            }
            if n_channels > 1 && ui.small_button("−").on_hover_text("Remove last channel").clicked() {
                remove_input_pin(node_id, n_channels - 1, inputs, snarl);
                remove_output_pin(node_id, n_channels - 1, outputs, snarl);
            }
        }); }
    });
    if let Some(rect) = curve_graph_rect {
        register_exposable_element(ui, node_id, "curve", rect);
    }
    false
}

pub(crate) fn show_vec_response_curve_body(node_id: NodeId, inputs: &[InPin], outputs: &[OutPin], ui: &mut egui::Ui, snarl: &mut Snarl<NodeData>) -> bool {
    let automap = snarl.get_node(node_id).is_some_and(|n| is_automap_curve(&n.module_id));
    // ── Initialise params on first use ────────────────────────────────────────
    let needs_init = snarl.get_node(node_id).map(|n| !n.params.contains_key("points")).unwrap_or(false);
    if needs_init {
        if let Some(node) = snarl.get_node_mut(node_id) {
            node.params.insert("points".into(),   serde_json::json!([[0.0, 0.0], [1.0, 1.0]]));
            node.params.insert("biases".into(),   serde_json::json!([0.0]));
            node.params.insert("in_max".into(),   serde_json::json!(1.0f64));
            node.params.insert("out_max".into(),  serde_json::json!(1.0f64));
            node.params.insert("grid_x".into(),   serde_json::json!(4i64));
            node.params.insert("grid_y".into(),   serde_json::json!(4i64));
            node.params.insert("snap".into(),     Value::Bool(false));
            node.params.insert("scale_t".into(),  serde_json::json!(0.0f64));
            node.params.insert("trail_ms".into(), serde_json::json!(300i64));
        }
    }

    // ── Read params ───────────────────────────────────────────────────────────
    let (points, biases, in_max, out_max, grid_x, grid_y, snap, scale_t, trail_ms, show_scaled_grid, show_grid_labels) = snarl
        .get_node(node_id)
        .map(|n| {
            let pts: Vec<[f32; 2]> = n.params.get("points")
                .and_then(|v| v.as_array())
                .map(|arr| arr.iter().filter_map(|p| {
                    let a = p.as_array()?;
                    Some([a.get(0)?.as_f64()? as f32, a.get(1)?.as_f64()? as f32])
                }).collect())
                .unwrap_or_else(|| vec![[0.0, 0.0], [1.0, 1.0]]);
            let bss: Vec<f32> = n.params.get("biases")
                .and_then(|v| v.as_array())
                .map(|arr| arr.iter().filter_map(|b| b.as_f64().map(|f| f as f32)).collect())
                .unwrap_or_default();
            let i1  = n.params.get("in_max") .and_then(|v| v.as_f64()).unwrap_or(1.0) as f32;
            let o1  = n.params.get("out_max").and_then(|v| v.as_f64()).unwrap_or(1.0) as f32;
            let gx  = n.params.get("grid_x").and_then(|v| v.as_i64()).unwrap_or(4).max(1) as usize;
            let gy  = n.params.get("grid_y").and_then(|v| v.as_i64()).unwrap_or(4).max(1) as usize;
            let sn  = n.params.get("snap").and_then(|v| v.as_bool()).unwrap_or(false);
            let sc  = n.params.get("scale_t").and_then(|v| v.as_f64()).map(|f| f as f32).unwrap_or(0.0);
            let tm  = n.params.get("trail_ms").and_then(|v| v.as_i64()).unwrap_or(300).clamp(0, 1000);
            let ssg = n.params.get("show_scaled_grid").and_then(|v| v.as_bool()).unwrap_or(false);
            let sgl = n.params.get("show_grid_labels").and_then(|v| v.as_bool()).unwrap_or(false);
            (pts, bss, i1, o1, gx, gy, sn, sc, tm, ssg, sgl)
        })
        .unwrap_or_else(|| (vec![[0.0, 0.0], [1.0, 1.0]], vec![], 1.0, 1.0, 4, 4, false, 0.0f32, 300, false, false));

    let n_channels = snarl.get_node(node_id).map(curve_channels).unwrap_or(1);
    // sig_f32 returns v.length() for Vec2, giving deflection magnitude
    let live_inputs: Vec<Option<f32>> = (0..n_channels)
        .map(|ch| snarl.get_node(node_id)
            .and_then(|n| n.extra.last_signals.get(ch)?.as_ref())
            .map(sig_f32))
        .collect();

    // Vec curve always operates in [0,1] × [0,1] (magnitude space)
    let (x_lo, x_hi) = (0.0f32, 1.0f32);
    let (y_lo, y_hi) = (0.0f32, 1.0f32);
    let x_range = x_hi - x_lo;
    let y_range = y_hi - y_lo;

    let mut new_points  = points.clone();
    let mut new_biases  = biases.clone();
    let mut pts_changed  = false;
    let mut bias_changed = false;
    let mut curve_graph_rect: Option<egui::Rect> = None;

    ui.vertical(|ui| {
        // ── Graph ─────────────────────────────────────────────────────────────
        egui::Resize::default()
            .id_salt(("vcrv", node_id))
            .default_size([180.0, 180.0])
            .min_size([80.0, 80.0])
            .show(ui, |ui| {
                let (rect, bg_resp) =
                    ui.allocate_exact_size(ui.available_size(), egui::Sense::click());
                curve_graph_rect = Some(rect);
                let painter = ui.painter_at(rect);

                let c2s = |x: f32, y: f32| egui::pos2(
                    rect.left() + (x - x_lo) / x_range * rect.width(),
                    rect.bottom() - (y - y_lo) / y_range * rect.height(),
                );
                let s2c = |pos: egui::Pos2| -> [f32; 2] {[
                    x_lo + (pos.x - rect.left()) / rect.width() * x_range,
                    y_lo + (rect.bottom() - pos.y) / rect.height() * y_range,
                ]};
                // Vec curve is always one-sided [0,1] magnitude space; no bidirectional case.
                let redistribute_v = |mut nodes: Vec<f32>, n: usize| -> Vec<f32> {
                    let min_gap = 1.0f32 / n as f32 * 0.5;
                    for _ in 0..n {
                        nodes.sort_by(|a, b| a.partial_cmp(b).unwrap());
                        let crowded = (1..nodes.len().saturating_sub(1))
                            .filter(|&i| (nodes[i]-nodes[i-1]).min(nodes[i+1]-nodes[i]) < min_gap)
                            .min_by(|&a, &b| {
                                let ga = (nodes[a]-nodes[a-1]).min(nodes[a+1]-nodes[a]);
                                let gb = (nodes[b]-nodes[b-1]).min(nodes[b+1]-nodes[b]);
                                ga.partial_cmp(&gb).unwrap()
                            });
                        let Some(ci) = crowded else { break; };
                        nodes.remove(ci);
                        let (li, _) = nodes.windows(2).enumerate()
                            .max_by(|(_, a), (_, b)| (a[1]-a[0]).partial_cmp(&(b[1]-b[0])).unwrap())
                            .unwrap();
                        nodes.insert(li + 1, (nodes[li] + nodes[li+1]) * 0.5);
                    }
                    nodes.sort_by(|a, b| a.partial_cmp(b).unwrap());
                    nodes
                };
                let scaled_grid_positions_v = |n: usize| -> Vec<f32> {
                    if n == 0 { return vec![0.0, 1.0]; }
                    if !show_scaled_grid {
                        return (0..=n).map(|i| i as f32 / n as f32).collect();
                    }
                    let nodes = (0..=n).map(|i| {
                        let t = i as f32 / n as f32;
                        1.0 - curve_scale_inv(1.0 - t, scale_t)
                    }).collect();
                    redistribute_v(nodes, n)
                };
                let snap_nodes_x = scaled_grid_positions_v(grid_x);
                let snap_nodes_y = scaled_grid_positions_v(grid_y);

                let do_snap = |x: f32, y: f32| -> (f32, f32) {
                    if !snap { return (x, y); }
                    let u = ((x - x_lo) / x_range).clamp(0.0, 1.0);
                    let v = ((y - y_lo) / y_range).clamp(0.0, 1.0);
                    let snap_u = snap_nodes_x.iter().copied()
                        .min_by(|a, b| (a - u).abs().partial_cmp(&(b - u).abs()).unwrap())
                        .unwrap_or(u);
                    let snap_v = snap_nodes_y.iter().copied()
                        .min_by(|a, b| (a - v).abs().partial_cmp(&(b - v).abs()).unwrap())
                        .unwrap_or(v);
                    (x_lo + snap_u * x_range, y_lo + snap_v * y_range)
                };

                painter.rect_filled(rect, 2.0, GRAPH_BG_DEFAULT);

                let grid_x_positions: Vec<f32> = (1..grid_x).map(|i| x_lo + snap_nodes_x[i] * x_range).collect();
                let grid_y_positions: Vec<f32> = (1..grid_y).map(|i| y_lo + snap_nodes_y[i] * y_range).collect();

                let (grid_faint, grid_axis) = graph_grid_colors(None);
                let gs = egui::Stroke::new(0.5, grid_faint);
                for &x in &grid_x_positions {
                    painter.line_segment([c2s(x, y_lo), c2s(x, y_hi)], gs);
                }
                for &y in &grid_y_positions {
                    painter.line_segment([c2s(x_lo, y), c2s(x_hi, y)], gs);
                }
                painter.line_segment([c2s(x_lo, y_lo), c2s(x_hi, y_hi)],
                    egui::Stroke::new(0.5, grid_axis));

                if show_grid_labels {
                    const MIN_LABEL_PX: f32 = 20.0;
                    let label_col = Color32::from_rgba_unmultiplied(180, 180, 180, 160);
                    let font = egui::FontId::proportional(9.0);
                    let mut last_sx = f32::NEG_INFINITY;
                    for &x in &grid_x_positions {
                        let sx = c2s(x, y_hi).x;
                        if sx - last_sx < MIN_LABEL_PX { continue; }
                        last_sx = sx;
                        let val = curve_scale_inv(x, scale_t) * in_max;
                        let label = if in_max <= 1.01 {
                            format!("{:.0}%", val * 100.0)
                        } else {
                            format!("{:.2}", val)
                        };
                        painter.text(egui::pos2(sx + 1.0, rect.top() + 1.0),
                            egui::Align2::LEFT_TOP, &label, font.clone(), label_col);
                    }
                    let mut last_sy = f32::INFINITY;
                    for &y in &grid_y_positions {
                        let sy = c2s(x_lo, y).y;
                        if last_sy - sy < MIN_LABEL_PX { continue; }
                        last_sy = sy;
                        let val = curve_scale_inv(y, scale_t) * out_max;
                        let label = if out_max <= 1.01 {
                            format!("{:.0}%", val * 100.0)
                        } else {
                            format!("{:.2}", val)
                        };
                        painter.text(egui::pos2(rect.left() + 1.0, sy - 9.0),
                            egui::Align2::LEFT_TOP, &label, font.clone(), label_col);
                    }
                }

                if new_points.len() >= 2 {
                    let steps = 120usize;
                    let curve_pts: Vec<egui::Pos2> = (0..=steps)
                        .map(|i| {
                            let x = x_lo + x_range * i as f32 / steps as f32;
                            let y = sample_curve(&new_points, x, &new_biases).clamp(y_lo, y_hi);
                            c2s(x, y)
                        })
                        .collect();
                    for w in curve_pts.windows(2) {
                        painter.line_segment([w[0], w[1]],
                            egui::Stroke::new(1.5, Color32::from_gray(200)));
                    }
                }

                let alt_held = ui.input(|i| i.modifiers.alt);
                if alt_held && new_points.len() >= 2 {
                    while new_biases.len() < new_points.len() - 1 { new_biases.push(0.0); }
                    for seg in 0..(new_points.len() - 1) {
                        let mid_x = (new_points[seg][0] + new_points[seg + 1][0]) * 0.5;
                        let mid_y = sample_curve(&new_points, mid_x, &new_biases).clamp(y_lo, y_hi);
                        let hpos  = c2s(mid_x, mid_y);
                        let hid   = ui.id().with(("vbias_h", node_id, seg));
                        let hresp = ui.interact(
                            egui::Rect::from_center_size(hpos, egui::Vec2::splat(14.0)),
                            hid, egui::Sense::click_and_drag());
                        if hresp.double_clicked() {
                            new_biases[seg] = 0.0;
                            bias_changed = true;
                        } else if hresp.dragged() {
                            let dy = -hresp.drag_delta().y / rect.height() * y_range;
                            new_biases[seg] = (new_biases[seg] + dy).clamp(-2.0, 2.0);
                            bias_changed = true;
                        }
                        let hcol = if hresp.hovered() || hresp.dragged() {
                            Color32::from_rgb(255, 220, 50)
                        } else { Color32::from_rgb(180, 140, 20) };
                        painter.circle_filled(hpos, 4.0, hcol);
                        painter.circle_stroke(hpos, 4.0,
                            egui::Stroke::new(1.0, Color32::from_gray(100)));
                    }
                }

                let mut remove_idx: Option<usize> = None;
                for i in 0..new_points.len() {
                    let [px, py] = new_points[i];
                    let screen   = c2s(px, py);
                    let pt_id    = ui.id().with(("vcpt", node_id, i));
                    let pt_resp  = ui.interact(
                        egui::Rect::from_center_size(screen, egui::Vec2::splat(12.0)),
                        pt_id, egui::Sense::click_and_drag());

                    let origin_id = ui.id().with(("vcrv_pt_origin_inline", node_id, i));
                    if pt_resp.drag_started() && !alt_held {
                        ui.ctx().data_mut(|d| d.insert_temp(origin_id, [px, py, 0.0f32, 0.0f32]));
                    }
                    if pt_resp.dragged() && !alt_held {
                        let prev = ui.ctx().data(|d| d.get_temp::<[f32;4]>(origin_id))
                            .unwrap_or([px, py, 0.0, 0.0]);
                        let dd  = pt_resp.drag_delta();
                        let acc_x_px = prev[2] + dd.x;
                        let acc_y_px = prev[3] + dd.y;
                        ui.ctx().data_mut(|d| d.insert_temp(origin_id, [prev[0], prev[1], acc_x_px, acc_y_px]));
                        let nx_raw = prev[0] + acc_x_px * x_range / rect.width();
                        let ny_raw = prev[1] - acc_y_px * y_range / rect.height();
                        let lo_x   = new_points.get(i.wrapping_sub(1)).map(|p| p[0] + 0.001).unwrap_or(x_lo);
                        let hi_x   = new_points.get(i + 1).map(|p| p[0] - 0.001).unwrap_or(x_hi);
                        let (sx, sy) = do_snap(nx_raw, ny_raw);
                        new_points[i] = [sx.clamp(lo_x, hi_x), sy.clamp(y_lo, y_hi)];
                        pts_changed = true;
                    }
                    if pt_resp.drag_stopped() {
                        ui.ctx().data_mut(|d| d.remove_temp::<[f32;4]>(origin_id));
                    }
                    if pt_resp.secondary_clicked() && new_points.len() > 2 {
                        remove_idx = Some(i);
                        pts_changed = true;
                    }
                    let col = if pt_resp.hovered() || pt_resp.dragged() { Color32::WHITE } else { Color32::from_gray(190) };
                    painter.circle_filled(screen, 5.0, col);
                    painter.circle_stroke(screen, 5.0, egui::Stroke::new(1.0, Color32::from_gray(80)));
                }

                if let Some(idx) = remove_idx { curve_remove_point(&mut new_points, &mut new_biases, idx); }
                if bg_resp.double_clicked() {
                    if let Some(pos) = bg_resp.interact_pointer_pos() {
                        let [gx_raw, gy_raw] = s2c(pos);
                        let (gx_sn, gy_sn)   = do_snap(gx_raw, gy_raw);
                        let gx = gx_sn.clamp(x_lo, x_hi);
                        let gy = gy_sn.clamp(y_lo, y_hi);
                        let idx = new_points.partition_point(|p| p[0] < gx);
                        curve_insert_point(&mut new_points, &mut new_biases, idx, [gx, gy]);
                        pts_changed = true;
                    }
                }

                // Live-position trails (magnitude of Vec2 input → position on curve)
                let abs_max   = in_max.abs().max(f32::EPSILON);
                let trail_dur = std::time::Duration::from_millis(trail_ms as u64);
                let now       = std::time::Instant::now();
                let mut has_active = false;
                for (ch, raw_opt) in live_inputs.iter().enumerate() {
                    let Some(raw) = raw_opt else { continue; };
                    has_active = true;
                    let graph_x = curve_scale((raw.abs() / abs_max).clamp(0.0, 1.0), scale_t);
                    type Trail = std::collections::VecDeque<(f32, std::time::Instant)>;
                    let trail_id = ui.id().with(("vtrail", node_id, ch as u32));
                    let mut trail: Trail = ui.data(|d| d.get_temp::<Trail>(trail_id).clone().unwrap_or_default());
                    if trail_ms > 0 {
                        trail.push_back((graph_x, now));
                        while trail.front().map(|&(_, t)| now.duration_since(t) > trail_dur).unwrap_or(false) {
                            trail.pop_front();
                        }
                    } else { trail.clear(); }
                    let trail_pts: Vec<(f32, std::time::Instant)> = trail.iter().cloned().collect();
                    ui.data_mut(|d| d.insert_temp(trail_id, trail));
                    let ch_col = MULTI_COLORS[ch % MULTI_COLORS.len()];
                    for w in trail_pts.windows(2) {
                        let (x0, _)  = w[0];
                        let (x1, t1) = w[1];
                        let age   = now.duration_since(t1).as_secs_f32() / trail_dur.as_secs_f32();
                        let alpha = ((1.0 - age.clamp(0.0, 1.0)) * 220.0) as u8;
                        let col   = Color32::from_rgba_unmultiplied(ch_col.r(), ch_col.g(), ch_col.b(), alpha);
                        let steps = (((x1 - x0).abs() / x_range * 80.0) as usize).max(1);
                        let x0_y  = sample_curve(&new_points, x0, &new_biases).clamp(y_lo, y_hi);
                        let mut prev = c2s(x0, x0_y);
                        for s in 1..=steps {
                            let t  = s as f32 / steps as f32;
                            let ix = x0 + (x1 - x0) * t;
                            let iy = sample_curve(&new_points, ix, &new_biases).clamp(y_lo, y_hi);
                            let next = c2s(ix, iy);
                            painter.line_segment([prev, next], egui::Stroke::new(1.5, col));
                            prev = next;
                        }
                    }
                    let graph_y = sample_curve(&new_points, graph_x, &new_biases).clamp(y_lo, y_hi);
                    let head_col = Color32::from_rgba_unmultiplied(ch_col.r(), ch_col.g(), ch_col.b(), 220);
                    painter.circle_filled(c2s(graph_x, graph_y), 3.5, head_col);
                }
                if has_active { request_repaint_throttled(ui.ctx()); }

                // Right-click on empty graph → save/load/copy/paste/reset
                // (shared with the header buttons; uses .fxc format).
                let mut menu_mutated = false;
                bg_resp.context_menu(|ui| {
                    if curve_context_menu(ui, node_id, snarl, None) {
                        menu_mutated = true;
                    }
                });
                if menu_mutated {
                    if let Some(node) = snarl.get_node(node_id) {
                        let (pk, bk) = curve_param_keys(node);
                        new_points = node.params.get(pk).and_then(|v| v.as_array())
                            .map(|arr| arr.iter().filter_map(|p| {
                                let a = p.as_array()?;
                                Some([a.get(0)?.as_f64()? as f32, a.get(1)?.as_f64()? as f32])
                            }).collect())
                            .unwrap_or_else(|| vec![[0.0, 0.0], [1.0, 1.0]]);
                        new_biases = node.params.get(bk).and_then(|v| v.as_array())
                            .map(|arr| arr.iter().filter_map(|b| b.as_f64().map(|f| f as f32)).collect())
                            .unwrap_or_default();
                    }
                    pts_changed = false;
                    bias_changed = false;
                }
            });

        // ── Write back curve points / biases ──────────────────────────────────
        if pts_changed || bias_changed {
            if let Some(node) = snarl.get_node_mut(node_id) {
                if pts_changed {
                    new_biases.resize(new_points.len().saturating_sub(1), 0.0);
                    let json: Vec<Value> = new_points.iter().map(|p| serde_json::json!([p[0], p[1]])).collect();
                    node.params.insert("points".into(), Value::Array(json));
                }
                let bj: Vec<Value> = new_biases.iter()
                    .filter_map(|&b| Number::from_f64(b as f64).map(Value::Number))
                    .collect();
                node.params.insert("biases".into(), Value::Array(bj));
            }
        }

        // ── Controls below graph ──────────────────────────────────────────────
        let mut i1      = in_max;
        let mut o1      = out_max;
        let mut gx_f    = grid_x as f64;
        let mut gy_f    = grid_y as f64;
        let mut snap_on = snap;
        let mut sc_t    = scale_t;
        let mut tm      = trail_ms;
        let mut ssg     = show_scaled_grid;
        let mut sgl     = show_grid_labels;
        let mut changed = false;

        // Row 1: Scale slider (Log←──●──→Exp, double-click resets) + Snap
        let scale_resp = ui.horizontal(|ui| {
            ui.label(egui::RichText::new("Log").small().weak());
            let (slider_rect, slider_resp) = ui.allocate_exact_size(
                egui::vec2(80.0, 14.0), egui::Sense::click_and_drag(),
            );
            if slider_resp.double_clicked() {
                sc_t = 0.0;
                changed = true;
            } else if slider_resp.dragged() {
                sc_t = (sc_t + slider_resp.drag_delta().x / slider_rect.width() * 2.0).clamp(-1.0, 1.0);
                changed = true;
            }
            let painter = ui.painter_at(slider_rect);
            painter.rect_filled(slider_rect, 3.0, Color32::from_gray(35));
            let cx = slider_rect.center().x;
            painter.line_segment(
                [egui::pos2(cx, slider_rect.top() + 2.0), egui::pos2(cx, slider_rect.bottom() - 2.0)],
                egui::Stroke::new(1.0, Color32::from_gray(70)),
            );
            let knob_x = slider_rect.left() + (sc_t + 1.0) * 0.5 * slider_rect.width();
            painter.circle_filled(
                egui::pos2(knob_x, slider_rect.center().y), 5.0,
                if slider_resp.hovered() || slider_resp.dragged() { Color32::WHITE } else { Color32::from_gray(190) },
            );
            ui.label(egui::RichText::new("Exp").small().weak());
            ui.separator();
            let snap_before = snap_on;
            ui.checkbox(&mut snap_on, egui::RichText::new("Snap").small());
            changed |= snap_on != snap_before;
        });
        register_exposable_element(ui, node_id, "scale_row", scale_resp.response.rect);

        // Row 2: In/Out max
        let range_resp = ui.horizontal(|ui| {
            ui.label(egui::RichText::new("In max").small().weak());
            changed |= ui.add(egui::DragValue::new(&mut i1).speed(0.01).max_decimals(2)).changed();
            ui.separator();
            ui.label(egui::RichText::new("Out max").small().weak());
            changed |= ui.add(egui::DragValue::new(&mut o1).speed(0.01).max_decimals(2)).changed();
        });
        register_exposable_element(ui, node_id, "range_row", range_resp.response.rect);

        // Row 3: Grid + Trail
        let grid_resp = ui.horizontal(|ui| {
            ui.label(egui::RichText::new("Grid").small().weak());
            changed |= ui.add(egui::DragValue::new(&mut gx_f).speed(0.25)
                .range(1.0..=20.0).max_decimals(0).prefix("H ")).changed();
            changed |= ui.add(egui::DragValue::new(&mut gy_f).speed(0.25)
                .range(1.0..=20.0).max_decimals(0).prefix("V ")).changed();
            ui.separator();
            ui.label(egui::RichText::new("Trail").small().weak());
            changed |= ui.add(egui::DragValue::new(&mut tm).speed(5.0)
                .range(0i64..=1000).suffix("ms")).changed();
        });
        register_exposable_element(ui, node_id, "grid_row", grid_resp.response.rect);

        // Row 4: Grid display options
        let grid_opts_resp = ui.horizontal(|ui| {
            let ssg_before = ssg;
            ui.checkbox(&mut ssg, egui::RichText::new("Scale grid").small())
                .on_hover_text("Adapt grid lines to the current Log/Exp scaling (Log compresses toward max, Exp toward min)");
            changed |= ssg != ssg_before;
            let sgl_before = sgl;
            ui.checkbox(&mut sgl, egui::RichText::new("Labels").small())
                .on_hover_text("Show value labels on grid lines");
            changed |= sgl != sgl_before;
        });
        register_exposable_element(ui, node_id, "grid_options_row", grid_opts_resp.response.rect);

        if changed {
            if let Some(node) = snarl.get_node_mut(node_id) {
                if let Some(n) = Number::from_f64(i1 as f64)   { node.params.insert("in_max".into(),  Value::Number(n)); }
                if let Some(n) = Number::from_f64(o1 as f64)   { node.params.insert("out_max".into(), Value::Number(n)); }
                if let Some(n) = Number::from_f64(sc_t as f64) { node.params.insert("scale_t".into(), Value::Number(n)); }
                node.params.insert("grid_x".into(),            serde_json::json!(gx_f as i64));
                node.params.insert("grid_y".into(),            serde_json::json!(gy_f as i64));
                node.params.insert("snap".into(),              Value::Bool(snap_on));
                node.params.insert("trail_ms".into(),          serde_json::json!(tm));
                node.params.insert("show_scaled_grid".into(),  Value::Bool(ssg));
                node.params.insert("show_grid_labels".into(),  Value::Bool(sgl));
            }
        }

        if !automap { ui.horizontal(|ui| {
            ui.label(egui::RichText::new("Ch").small().weak());
            if ui.small_button("+").on_hover_text("Add Vec2 channel").clicked() {
                if let Some(node) = snarl.get_node_mut(node_id) {
                    let next = node.inputs.len() + 1;
                    node.inputs.push(PinDescriptor::new(format!("In {}", next), SignalType::Vec2));
                    node.outputs.push(PinDescriptor::new(format!("Out {}", next), SignalType::Vec2));
                }
            }
            if n_channels > 1 && ui.small_button("−").on_hover_text("Remove last channel").clicked() {
                remove_input_pin(node_id, n_channels - 1, inputs, snarl);
                remove_output_pin(node_id, n_channels - 1, outputs, snarl);
            }
        }); }
    });
    if let Some(rect) = curve_graph_rect {
        register_exposable_element(ui, node_id, "curve", rect);
    }
    false
}

// ── Two-way Response Curve body ───────────────────────────────────────────────

#[allow(clippy::too_many_lines)]
pub(crate) fn show_twoway_response_curve_body(node_id: NodeId, inputs: &[InPin], outputs: &[OutPin], ui: &mut egui::Ui, snarl: &mut Snarl<NodeData>) -> bool {
    let automap = snarl.get_node(node_id).is_some_and(|n| is_automap_curve(&n.module_id));
    // No vsync bypass — same rationale as show_response_curve_body.
    let needs_init = snarl.get_node(node_id).map(|n| !n.params.contains_key("points")).unwrap_or(false);
    if needs_init {
        if let Some(node) = snarl.get_node_mut(node_id) {
            node.params.insert("points".into(),    serde_json::json!([[0.0, 0.0], [1.0, 1.0]]));
            node.params.insert("biases".into(),    serde_json::json!([0.0]));
            node.params.insert("points_dn".into(), serde_json::json!([[0.0, 0.0], [1.0, 1.0]]));
            node.params.insert("biases_dn".into(), serde_json::json!([0.0]));
            node.params.insert("absolute".into(),  Value::Bool(true));
            node.params.insert("in_min".into(),    serde_json::json!(-1.0));
            node.params.insert("in_max".into(),    serde_json::json!( 1.0));
            node.params.insert("out_min".into(),   serde_json::json!(-1.0));
            node.params.insert("out_max".into(),   serde_json::json!( 1.0));
            node.params.insert("grid_x".into(),    serde_json::json!(4i64));
            node.params.insert("grid_y".into(),    serde_json::json!(4i64));
            node.params.insert("snap".into(),      Value::Bool(false));
            node.params.insert("scale_t".into(),   serde_json::json!(0.0f64));
            node.params.insert("trail_ms".into(),  serde_json::json!(300i64));
            node.params.insert("active_lane".into(), Value::String("up".into()));
            node.params.insert("vec_mode".into(),    Value::Bool(false));
            node.params.insert("hysteresis_pct".into(), serde_json::json!(0.5f64));
            node.params.insert("interp_ms".into(),      serde_json::json!(50.0f64));
        }
    }

    let node_data = match snarl.get_node(node_id).cloned() { Some(n) => n, None => return false };

    let read_pts = |key: &str| -> Vec<[f32; 2]> {
        node_data.params.get(key).and_then(|v| v.as_array())
            .map(|arr| arr.iter().filter_map(|p| {
                let a = p.as_array()?;
                Some([a.get(0)?.as_f64()? as f32, a.get(1)?.as_f64()? as f32])
            }).collect())
            .unwrap_or_else(|| vec![[0.0, 0.0], [1.0, 1.0]])
    };
    let read_biases = |key: &str| -> Vec<f32> {
        node_data.params.get(key).and_then(|v| v.as_array())
            .map(|arr| arr.iter().filter_map(|b| b.as_f64().map(|f| f as f32)).collect())
            .unwrap_or_default()
    };

    let pts_up    = read_pts("points");
    let biases_up = read_biases("biases");
    let pts_dn    = read_pts("points_dn");
    let biases_dn = read_biases("biases_dn");

    let absolute  = node_data.params.get("absolute").and_then(|v| v.as_bool()).unwrap_or(true);
    let in_min    = node_data.params.get("in_min") .and_then(|v| v.as_f64()).unwrap_or(-1.0) as f32;
    let in_max    = node_data.params.get("in_max") .and_then(|v| v.as_f64()).unwrap_or( 1.0) as f32;
    let out_min   = node_data.params.get("out_min").and_then(|v| v.as_f64()).unwrap_or(-1.0) as f32;
    let out_max   = node_data.params.get("out_max").and_then(|v| v.as_f64()).unwrap_or( 1.0) as f32;
    let grid_x    = node_data.params.get("grid_x").and_then(|v| v.as_i64()).unwrap_or(4).max(1) as usize;
    let grid_y    = node_data.params.get("grid_y").and_then(|v| v.as_i64()).unwrap_or(4).max(1) as usize;
    let snap      = node_data.params.get("snap").and_then(|v| v.as_bool()).unwrap_or(false);
    let scale_t   = node_data.params.get("scale_t").and_then(|v| v.as_f64()).map(|f| f as f32).unwrap_or(0.0);
    let trail_ms  = node_data.params.get("trail_ms").and_then(|v| v.as_i64()).unwrap_or(300).clamp(0, 1000);
    let active_lane = node_data.params.get("active_lane").and_then(|v| v.as_str()).unwrap_or("up").to_string();
    let vec_mode  = twoway_vec_mode(&node_data);
    let hyst_pct  = node_data.params.get("hysteresis_pct").and_then(|v| v.as_f64()).unwrap_or(0.5) as f32;
    let interp_ms = node_data.params.get("interp_ms")     .and_then(|v| v.as_f64()).unwrap_or(50.0) as f32;
    let show_scaled_grid = node_data.params.get("show_scaled_grid").and_then(|v| v.as_bool()).unwrap_or(false);
    let show_grid_labels = node_data.params.get("show_grid_labels").and_then(|v| v.as_bool()).unwrap_or(false);

    let n_channels = curve_channels(&node_data);
    let legend = curve_channel_labels(&node_data);

    let absolute_eff = absolute || vec_mode;
    let (x_lo, x_hi): (f32, f32) = if absolute_eff { (0.0, 1.0) } else { (-1.0, 1.0) };
    // The Hyst graph's Y is a band width, 0..TWOWAY_HYST_MAX_PCT, in any mode.
    let hyst_lane = active_lane == "hy";
    let (y_lo, y_hi): (f32, f32) = if absolute_eff || hyst_lane { (0.0, 1.0) } else { (-1.0, 1.0) };
    let x_range = x_hi - x_lo;
    let y_range = y_hi - y_lo;
    let lane_up  = active_lane == "up";
    let lane_ix: u8 = if lane_up { 0 } else if hyst_lane { 2 } else { 1 };

    let mut new_pts_up    = pts_up.clone();
    let mut new_biases_up = biases_up.clone();
    let mut new_pts_dn    = pts_dn.clone();
    let mut new_biases_dn = biases_dn.clone();
    let mut pts_up_changed  = false;
    let mut bias_up_changed = false;
    let mut pts_dn_changed  = false;
    let mut bias_dn_changed = false;
    let (pts_hy, biases_hy) = twoway_hyst_points(&node_data);
    let mut new_pts_hy    = pts_hy.clone();
    let mut new_biases_hy = biases_hy.clone();
    let mut pts_hy_changed  = false;
    let mut bias_hy_changed = false;
    let mut params_changed  = false;
    let mut undo_requested  = false;

    let mut gx_f    = grid_x;
    let mut gy_f    = grid_y;
    let mut snap_on = snap;
    let mut sc_t    = scale_t;
    let mut i1      = in_max;
    let mut o1      = out_max;
    let mut abs_on  = absolute;
    let mut vm      = vec_mode;
    let mut h_pct   = hyst_pct;
    let mut h_dir   = node_data.params.get("hyst_dir").and_then(|v| v.as_str()).unwrap_or("both").to_string();
    let mut h_start = node_data.params.get("hyst_start_up").and_then(|v| v.as_bool()).unwrap_or(false);
    let mut i_ms    = interp_ms;
    let mut ph_ms   = node_data.params.get("peak_hold_ms").and_then(|v| v.as_f64()).unwrap_or(0.0) as f32;
    let mut ph_dir  = node_data.params.get("peak_hold_dir").and_then(|v| v.as_str()).unwrap_or("up").to_string();
    let mut tm      = trail_ms;
    let mut ssg     = show_scaled_grid;
    let mut sgl     = show_grid_labels;
    let mut lane_sel = active_lane.clone();
    let mut curve_graph_rect: Option<egui::Rect> = None;

    ui.vertical(|ui| {
        // Lane toggle
        let lane_toggle_resp = ui.horizontal(|ui| {
            ui.label(egui::RichText::new("Edit:").small().weak());
            let up_sel = lane_sel == "up";
            let dn_sel = lane_sel == "dn";
            if ui.selectable_label(up_sel, egui::RichText::new("↑ Up").small()).on_hover_text("Edit the rising-input curve").clicked() && !up_sel { lane_sel = "up".into(); params_changed = true; }
            if ui.selectable_label(dn_sel, egui::RichText::new("↓ Down").small()).on_hover_text("Edit the falling-input curve").clicked() && !dn_sel { lane_sel = "dn".into(); params_changed = true; }
            let hy_sel = lane_sel == "hy";
            if ui.selectable_label(hy_sel, egui::RichText::new("Hyst").small()).on_hover_text(HYST_GRAPH_TAB_HINT).clicked() && !hy_sel { lane_sel = "hy".into(); params_changed = true; }
        });
        register_exposable_element(ui, node_id, "lane_toggle", lane_toggle_resp.response.rect);

        egui::Resize::default()
            .id_salt(("twcrv", node_id))
            .default_size([180.0, 180.0])
            .min_size([80.0, 80.0])
            .show(ui, |ui| {
                // bg uses Sense::click only so child interact() calls can capture drags
                let (rect, bg_resp) = ui.allocate_exact_size(ui.available_size(), egui::Sense::click());
                curve_graph_rect = Some(rect);

                let c2s = |x: f32, y: f32| egui::pos2(
                    rect.left() + (x - x_lo) / x_range * rect.width(),
                    rect.bottom() - (y - y_lo) / y_range * rect.height(),
                );
                let s2c = |pos: egui::Pos2| -> [f32; 2] {[
                    x_lo + (pos.x - rect.left()) / rect.width() * x_range,
                    y_lo + (rect.bottom() - pos.y) / rect.height() * y_range,
                ]};

                // Gamepad-nav: publish graph geometry (global/screen space) + read
                // the selected-dot index, same as the float/vec curve bodies.
                let pass = ui.ctx().cumulative_pass_nr();
                let to_global = ui.ctx().layer_transform_to_global(ui.layer_id())
                    .unwrap_or(egui::emath::TSTransform::IDENTITY);
                ui.ctx().data_mut(|d| d.insert_temp(
                    egui::Id::new(("gp_nav_curve_geom", node_id.0)),
                    (pass, to_global * rect, x_lo, x_hi, y_lo, y_hi)));
                let nav_sel: Option<(u64, usize, bool)> = ui.ctx()
                    .data(|d| d.get_temp(egui::Id::new(("gp_nav_curve_sel", node_id.0))));
                let nav_sel_dot: Option<usize> = nav_sel
                    .filter(|(p, _, _)| crate::widgets::nav_pass_matches(ui.ctx(), *p))
                    .map(|(_, i, _)| i);
                let nav_editing_dot: bool = nav_sel.map(|(_, _, e)| e).unwrap_or(false);

                let painter = ui.painter_at(rect);
                painter.rect_filled(rect, 2.0, GRAPH_BG_DEFAULT);

                let abs_max_in  = in_max.abs().max(in_min.abs()).max(f32::EPSILON);
                let abs_max_out = out_max.abs().max(out_min.abs()).max(f32::EPSILON);

                // Grid positions (same redistribute algorithm as float body)
                let redistribute = |mut nodes: Vec<f32>, n: usize| -> Vec<f32> {
                    let min_gap = 1.0f32 / n as f32 * 0.5;
                    for _ in 0..n {
                        nodes.sort_by(|a, b| a.partial_cmp(b).unwrap());
                        let crowded = (1..nodes.len().saturating_sub(1))
                            .filter(|&i| (nodes[i]-nodes[i-1]).min(nodes[i+1]-nodes[i]) < min_gap)
                            .min_by(|&a, &b| {
                                let ga = (nodes[a]-nodes[a-1]).min(nodes[a+1]-nodes[a]);
                                let gb = (nodes[b]-nodes[b-1]).min(nodes[b+1]-nodes[b]);
                                ga.partial_cmp(&gb).unwrap()
                            });
                        let Some(ci) = crowded else { break; };
                        nodes.remove(ci);
                        let (li, _) = nodes.windows(2).enumerate()
                            .max_by(|(_, a), (_, b)| (a[1]-a[0]).partial_cmp(&(b[1]-b[0])).unwrap()).unwrap();
                        nodes.insert(li + 1, (nodes[li] + nodes[li+1]) * 0.5);
                    }
                    nodes.sort_by(|a, b| a.partial_cmp(b).unwrap());
                    nodes
                };
                let scaled_grid = |n: usize| -> Vec<f32> {
                    if n == 0 { return vec![0.0, 1.0]; }
                    if !ssg { return (0..=n).map(|i| i as f32 / n as f32).collect(); }
                    if absolute_eff {
                        redistribute((0..=n).map(|i| 1.0 - curve_scale_inv(1.0 - i as f32 / n as f32, sc_t)).collect(), n)
                    } else {
                        let hlo = n / 2; let hhi = n - hlo;
                        let lo: Vec<f32> = (0..=hlo).map(|i| 0.5 - (1.0 - curve_scale_inv(1.0 - i as f32 / hlo as f32, sc_t)) * 0.5).collect();
                        let hi: Vec<f32> = (0..=hhi).map(|i| 0.5 + (1.0 - curve_scale_inv(1.0 - i as f32 / hhi as f32, sc_t)) * 0.5).collect();
                        let mut m = redistribute(lo, hlo);
                        for v in redistribute(hi, hhi).iter().skip(1) { m.push(*v); }
                        m.sort_by(|a, b| a.partial_cmp(b).unwrap()); m
                    }
                };
                let sx = scaled_grid(gx_f);
                let sy = scaled_grid(gy_f);
                let do_snap = |x: f32, y: f32| -> (f32, f32) {
                    if !snap_on { return (x, y); }
                    let u = ((x-x_lo)/x_range).clamp(0.0, 1.0);
                    let v = ((y-y_lo)/y_range).clamp(0.0, 1.0);
                    let su = sx.iter().copied().min_by(|a, b| (a-u).abs().partial_cmp(&(b-u).abs()).unwrap()).unwrap_or(u);
                    let sv = sy.iter().copied().min_by(|a, b| (a-v).abs().partial_cmp(&(b-v).abs()).unwrap()).unwrap_or(v);
                    (x_lo + su * x_range, y_lo + sv * y_range)
                };
                let gxp: Vec<f32> = (1..gx_f).map(|i| x_lo + sx[i] * x_range).collect();
                let gyp: Vec<f32> = (1..gy_f).map(|i| y_lo + sy[i] * y_range).collect();
                let (grid_faint, grid_axis) = graph_grid_colors(None);
                let gs = egui::Stroke::new(0.5, grid_faint);
                for &x in &gxp { painter.line_segment([c2s(x, y_lo), c2s(x, y_hi)], gs); }
                for &y in &gyp { painter.line_segment([c2s(x_lo, y), c2s(x_hi, y)], gs); }
                if !hyst_lane { painter.line_segment([c2s(x_lo, y_lo), c2s(x_hi, y_hi)], egui::Stroke::new(0.5, grid_axis)); }

                if sgl {
                    const MPX: f32 = 20.0;
                    let lc = Color32::from_rgba_unmultiplied(180, 180, 180, 160);
                    let fnt = egui::FontId::proportional(9.0);
                    let gri = |u: f32| -> f32 { if absolute_eff { curve_scale_inv(u, sc_t) * abs_max_in } else { let c = u*2.0-1.0; (if c<0.0{-1.0f32}else{1.0}) * curve_scale_inv(c.abs(), sc_t) * abs_max_in } };
                    let gro = |v: f32| -> f32 { if absolute_eff { curve_scale_inv(v, sc_t) * abs_max_out } else { let c = v*2.0-1.0; (if c<0.0{-1.0f32}else{1.0}) * curve_scale_inv(c.abs(), sc_t) * abs_max_out } };
                    let mut lsx = f32::NEG_INFINITY;
                    for &x in &gxp { let sx2 = c2s(x, y_hi).x; if sx2-lsx < MPX { continue; } lsx = sx2; let val = gri((x-x_lo)/x_range); let lbl = if abs_max_in<=1.01{format!("{:.0}%",val*100.0)}else{format!("{:.2}",val)}; painter.text(egui::pos2(sx2+1.0, rect.top()+1.0), egui::Align2::LEFT_TOP, &lbl, fnt.clone(), lc); }
                    let mut lsy = f32::INFINITY;
                    for &y in &gyp { let sy2 = c2s(x_lo, y).y; if lsy-sy2 < MPX { continue; } lsy = sy2; let val = gro((y-y_lo)/y_range); let lbl = if hyst_lane{format!("{:.1}%",(y-y_lo)/y_range*TWOWAY_HYST_MAX_PCT)}else if abs_max_out<=1.01{format!("{:.0}%",val*100.0)}else{format!("{:.2}",val)}; painter.text(egui::pos2(rect.left()+1.0, sy2-9.0), egui::Align2::LEFT_TOP, &lbl, fnt.clone(), lc); }
                }

                // Inactive lane (dimmed)
                for (inact_pts, inact_bias) in twoway_dim_curves(lane_up, hyst_lane, absolute_eff, (&pts_up, &biases_up), (&pts_dn, &biases_dn)) {
                if inact_pts.len() >= 2 {
                    let ic = Color32::from_rgba_unmultiplied(130, 130, 130, 70);
                    let mut pp = c2s(x_lo, sample_curve(inact_pts, x_lo, inact_bias).clamp(y_lo, y_hi));
                    for s in 1..=120usize { let t = s as f32/120.0; let ix = x_lo+t*x_range; let np = c2s(ix, sample_curve(inact_pts, ix, inact_bias).clamp(y_lo, y_hi)); painter.line_segment([pp, np], egui::Stroke::new(1.0, ic)); pp = np; }
                }
                }

                // Active lane (solid gray, same as float body)
                let edit_pts_r = if hyst_lane { &pts_hy } else if lane_up { &pts_up } else { &pts_dn };
                let (new_edit_pts, new_edit_biases, pts_changed_ref, bias_changed_ref) = if hyst_lane {
                    (&mut new_pts_hy, &mut new_biases_hy, &mut pts_hy_changed, &mut bias_hy_changed)
                } else if lane_up {
                    (&mut new_pts_up, &mut new_biases_up, &mut pts_up_changed, &mut bias_up_changed)
                } else {
                    (&mut new_pts_dn, &mut new_biases_dn, &mut pts_dn_changed, &mut bias_dn_changed)
                };
                // A one-dot Hyst graph draws as its flat band.
                if !new_edit_pts.is_empty() {
                    let cp: Vec<egui::Pos2> = (0..=120).map(|i| { let x = x_lo + x_range * i as f32 / 120.0; c2s(x, sample_curve(new_edit_pts, x, new_edit_biases).clamp(y_lo, y_hi)) }).collect();
                    for w in cp.windows(2) { painter.line_segment([w[0], w[1]], egui::Stroke::new(1.5, Color32::from_gray(200))); }
                }

                // Alt-drag bias handles (mouse Alt OR gamepad bias mode).
                let nav_bias = ui.ctx().data(|d|
                    d.get_temp::<u64>(egui::Id::new(("gp_nav_curve_bias", node_id.0))))
                    .map_or(false, |p| crate::widgets::nav_pass_matches(ui.ctx(), p));
                let alt_held = ui.input(|i| i.modifiers.alt) || nav_bias;
                if alt_held && new_edit_pts.len() >= 2 {
                    while new_edit_biases.len() < new_edit_pts.len() - 1 { new_edit_biases.push(0.0); }
                    for seg in 0..(new_edit_pts.len() - 1) {
                        let mid_x = (new_edit_pts[seg][0] + new_edit_pts[seg+1][0]) * 0.5;
                        let mid_y = sample_curve(new_edit_pts, mid_x, new_edit_biases).clamp(y_lo, y_hi);
                        let hpos  = c2s(mid_x, mid_y);
                        let hresp = ui.interact(egui::Rect::from_center_size(hpos, egui::Vec2::splat(14.0)), ui.id().with(("twbh", node_id, lane_ix, seg as u32)), egui::Sense::click_and_drag());
                        if hresp.double_clicked() { new_edit_biases[seg] = 0.0; *bias_changed_ref = true; }
                        else if hresp.dragged() { let dy = -hresp.drag_delta().y / rect.height() * y_range; new_edit_biases[seg] = (new_edit_biases[seg] + dy).clamp(-2.0, 2.0); *bias_changed_ref = true; }
                        let hcol = if hresp.hovered() || hresp.dragged() { Color32::from_rgb(255,220,50) } else { Color32::from_rgb(180,140,20) };
                        painter.circle_filled(hpos, 4.0, hcol);
                        painter.circle_stroke(hpos, 4.0, egui::Stroke::new(1.0, Color32::from_gray(100)));
                    }
                }

                // Control point handles
                let mut remove_idx: Option<usize> = None;
                for i in 0..edit_pts_r.len() {
                    let [px, py] = edit_pts_r[i];
                    let screen   = c2s(px, py);
                    let pt_id    = ui.id().with(("twpt", node_id, lane_ix, i as u32));
                    let pt_resp  = ui.interact(egui::Rect::from_center_size(screen, egui::Vec2::splat(12.0)), pt_id, egui::Sense::click_and_drag());
                    let oid      = ui.id().with(("twpt_orig", node_id, lane_ix, i as u32));
                    if pt_resp.drag_started() && !alt_held { ui.ctx().data_mut(|d| d.insert_temp(oid, [px, py, 0.0f32, 0.0f32])); }
                    if pt_resp.dragged() && !alt_held {
                        let prev = ui.ctx().data(|d| d.get_temp::<[f32;4]>(oid)).unwrap_or([px, py, 0.0, 0.0]);
                        let dd = pt_resp.drag_delta();
                        let (ax, ay) = (prev[2]+dd.x, prev[3]+dd.y);
                        ui.ctx().data_mut(|d| d.insert_temp(oid, [prev[0], prev[1], ax, ay]));
                        let nx = prev[0] + ax * x_range / rect.width();
                        let ny = prev[1] - ay * y_range / rect.height();
                        let lox = new_edit_pts.get(i.wrapping_sub(1)).map(|p| p[0]+0.001).unwrap_or(x_lo);
                        let hix = new_edit_pts.get(i+1).map(|p| p[0]-0.001).unwrap_or(x_hi);
                        let (sx2, sy2) = do_snap(nx, ny);
                        new_edit_pts[i] = [sx2.clamp(lox, hix), sy2.clamp(y_lo, y_hi)];
                        *pts_changed_ref = true;
                    }
                    if pt_resp.drag_stopped() { ui.ctx().data_mut(|d| d.remove_temp::<[f32;4]>(oid)); }
                    if pt_resp.secondary_clicked() && edit_pts_r.len() > if hyst_lane { 1 } else { 2 } { remove_idx = Some(i); *pts_changed_ref = true; }
                    // Gamepad-nav selected-dot highlight (active lane only).
                    if nav_sel_dot == Some(i) {
                        let accent = ui.visuals().selection.stroke.color;
                        let [r8, g8, b8, _] = accent.to_array();
                        for k in 0..5 {
                            let t = (k as f32 + 1.0) / 5.0;
                            let rr = (if nav_editing_dot { 16.0 } else { 12.0 }) * t;
                            let a = ((if nav_editing_dot { 170.0 } else { 120.0 }) * (1.0 - t)) as u8;
                            if a == 0 { continue; }
                            painter.circle_stroke(screen, rr,
                                egui::Stroke::new(2.0, Color32::from_rgba_unmultiplied(r8, g8, b8, a)));
                        }
                        painter.circle_filled(screen, if nav_editing_dot { 6.0 } else { 5.0 }, accent);
                        painter.circle_stroke(screen, if nav_editing_dot { 6.0 } else { 5.0 },
                            egui::Stroke::new(1.5, Color32::WHITE));
                    }
                    let nav_here = nav_sel_dot == Some(i);
                    let col = if pt_resp.hovered() || pt_resp.dragged() || nav_here { Color32::WHITE } else { Color32::from_gray(190) };
                    painter.circle_filled(screen, 5.0, col);
                    painter.circle_stroke(screen, 5.0, egui::Stroke::new(1.0, Color32::from_gray(80)));
                }

                if let Some(idx) = remove_idx { curve_remove_point(new_edit_pts, new_edit_biases, idx); }
                if bg_resp.double_clicked() {
                    if let Some(pos) = bg_resp.interact_pointer_pos() {
                        let [gx_raw, gy_raw] = s2c(pos);
                        let (gxs, gys) = do_snap(gx_raw, gy_raw);
                        let gx = gxs.clamp(x_lo, x_hi); let gy = gys.clamp(y_lo, y_hi);
                        let idx = new_edit_pts.partition_point(|p| p[0] < gx);
                        curve_insert_point(new_edit_pts, new_edit_biases, idx, [gx, gy]);
                        *pts_changed_ref = true; undo_requested = true;
                    }
                }

                // Live layer: band, raw tick, and the held point on the engine's lane.
                let geom = TwowayGeom::from_params(rect, &node_data);
                let has_active = paint_twoway_live(ui, &painter, &geom, node_id, &node_data,
                    (&pts_up, &biases_up), (&pts_dn, &biases_dn),
                    hyst_lane.then_some((pts_hy.as_slice(), biases_hy.as_slice())), "twtrail",
                    |ch| MULTI_COLORS[ch % MULTI_COLORS.len()]);
                if has_active { request_repaint_throttled(ui.ctx()); }
                if let Some(labels) = legend {
                    paint_curve_legend(&painter, rect, labels, |ch| MULTI_COLORS[ch % MULTI_COLORS.len()]);
                }

                // Right-click on empty graph → save/load/copy/paste/reset for
                // the *currently-selected* lane only (resolved via
                // `curve_param_keys` inside the header helpers). Loading a
                // curve into a two-way only replaces the active lane, so a
                // user editing the Down lane can paste/load a single-lane
                // curve into it without touching the Up lane.
                let lane_name = if hyst_lane { "Hyst" } else if lane_up { "Up" } else { "Down" };
                let mut menu_mutated = false;
                bg_resp.context_menu(|ui| {
                    // Graph-only: only points/biases for the active lane are
                    // touched; range / grid / scale / lane toggle stay as-is.
                    if curve_context_menu(ui, node_id, snarl, Some(lane_name)) {
                        menu_mutated = true;
                    }
                });
                if menu_mutated {
                    if let Some(node) = snarl.get_node(node_id) {
                        let (pk, bk) = curve_param_keys(node);
                        let new_pts: Vec<[f32; 2]> = node.params.get(pk).and_then(|v| v.as_array())
                            .map(|arr| arr.iter().filter_map(|p| {
                                let a = p.as_array()?;
                                Some([a.get(0)?.as_f64()? as f32, a.get(1)?.as_f64()? as f32])
                            }).collect())
                            .unwrap_or_else(|| vec![[0.0, 0.0], [1.0, 1.0]]);
                        let new_bss: Vec<f32> = node.params.get(bk).and_then(|v| v.as_array())
                            .map(|arr| arr.iter().filter_map(|b| b.as_f64().map(|f| f as f32)).collect())
                            .unwrap_or_default();
                        if hyst_lane {
                            (new_pts_hy, new_biases_hy) = twoway_hyst_points(node);
                            pts_hy_changed  = false;
                            bias_hy_changed = false;
                        } else if lane_up {
                            new_pts_up    = new_pts;
                            new_biases_up = new_bss;
                            pts_up_changed  = false;
                            bias_up_changed = false;
                        } else {
                            new_pts_dn    = new_pts;
                            new_biases_dn = new_bss;
                            pts_dn_changed  = false;
                            bias_dn_changed = false;
                        }
                    }
                }
            });

        // Write back curve changes
        if pts_up_changed || bias_up_changed {
            if let Some(node) = snarl.get_node_mut(node_id) {
                if pts_up_changed { new_biases_up.resize(new_pts_up.len().saturating_sub(1), 0.0); let j: Vec<Value> = new_pts_up.iter().map(|p| serde_json::json!([p[0],p[1]])).collect(); node.params.insert("points".into(), Value::Array(j)); }
                let bj: Vec<Value> = new_biases_up.iter().filter_map(|&b| Number::from_f64(b as f64).map(Value::Number)).collect(); node.params.insert("biases".into(), Value::Array(bj));
            }
        }
        if pts_dn_changed || bias_dn_changed {
            if let Some(node) = snarl.get_node_mut(node_id) {
                if pts_dn_changed { new_biases_dn.resize(new_pts_dn.len().saturating_sub(1), 0.0); let j: Vec<Value> = new_pts_dn.iter().map(|p| serde_json::json!([p[0],p[1]])).collect(); node.params.insert("points_dn".into(), Value::Array(j)); }
                let bj: Vec<Value> = new_biases_dn.iter().filter_map(|&b| Number::from_f64(b as f64).map(Value::Number)).collect(); node.params.insert("biases_dn".into(), Value::Array(bj));
            }
        }

        if pts_hy_changed || bias_hy_changed {
            if let Some(node) = snarl.get_node_mut(node_id) {
                twoway_write_hyst_points(node, &new_pts_hy, &new_biases_hy);
            }
            // A single dot IS the Hyst value.
            if new_pts_hy.len() == 1 { h_pct = new_pts_hy[0][1] * TWOWAY_HYST_MAX_PCT; }
        }
        let hyst_single = new_pts_hy.len() < 2;

        // Controls — each row wrapped so it can be registered as a pinnable element
        let mut changed = false;
        // Scale slider with center notch (double-click resets)
        let scale_resp = ui.horizontal(|ui| {
            ui.label(egui::RichText::new("Log").small().weak());
            let (sr, sresp) = ui.allocate_exact_size(egui::vec2(80.0, 14.0), egui::Sense::click_and_drag());
            if sresp.double_clicked() { sc_t = 0.0; changed = true; }
            else if sresp.dragged() { sc_t = (sc_t + sresp.drag_delta().x / sr.width() * 2.0).clamp(-1.0, 1.0); changed = true; }
            let slp = ui.painter_at(sr);
            slp.rect_filled(sr, 3.0, Color32::from_gray(35));
            slp.line_segment([egui::pos2(sr.center().x, sr.top()+2.0), egui::pos2(sr.center().x, sr.bottom()-2.0)], egui::Stroke::new(1.0, Color32::from_gray(70)));
            let kx = sr.left() + (sc_t+1.0)*0.5*sr.width();
            slp.circle_filled(egui::pos2(kx, sr.center().y), 5.0, if sresp.hovered() || sresp.dragged() { Color32::WHITE } else { Color32::from_gray(190) });
            ui.label(egui::RichText::new("Exp").small().weak());
            ui.separator();
            let ab = abs_on;
            ui.add_enabled_ui(!vm, |ui| { ui.checkbox(&mut abs_on, egui::RichText::new("Abs").small()).on_hover_text("Absolute mode: ignore sign of input"); });
            if abs_on != ab { changed = true; }
            let vmb = vm;
            // On an AutoMap curve the picked signal's type sets Vec mode, and the
            // pins are AutoMap, so there is nothing to retype.
            if !automap {
                ui.checkbox(&mut vm, egui::RichText::new("Vec").small()).on_hover_text("Vec2 mode: process magnitude. Forces Abs on.");
            }
            if vm != vmb {
                changed = true;
                let tgt = if vm { SignalType::Vec2 } else { SignalType::Float };
                let wrg = if vm { SignalType::Float } else { SignalType::Vec2 };
                let aw: Vec<(OutPinId, InPinId)> = snarl.wires().collect();
                for (oid, iid) in aw {
                    if iid.node == node_id && snarl.get_node(oid.node).and_then(|n| n.outputs.get(oid.output)).map(|p| p.signal_type) == Some(wrg) { snarl.disconnect(oid, iid); }
                    if oid.node == node_id && snarl.get_node(iid.node).and_then(|n| n.inputs.get(iid.input)).map(|p| p.signal_type) == Some(wrg) { snarl.disconnect(oid, iid); }
                }
                if let Some(node) = snarl.get_node_mut(node_id) {
                    for p in node.inputs.iter_mut()  { p.signal_type = tgt; }
                    for p in node.outputs.iter_mut() { p.signal_type = tgt; }
                    if vm { node.params.insert("absolute".into(), Value::Bool(true)); }
                }
            }
        });
        register_exposable_element(ui, node_id, "scale_row", scale_resp.response.rect);

        let range_resp = ui.horizontal(|ui| {
            ui.label(egui::RichText::new("In").small().weak()); let i1b = i1; ui.add(egui::DragValue::new(&mut i1).speed(0.01).range(0.001f32..=1000.0f32)); if (i1-i1b).abs()>1e-5{changed=true;}
            ui.label(egui::RichText::new("Out").small().weak()); let o1b = o1; ui.add(egui::DragValue::new(&mut o1).speed(0.01).range(0.001f32..=1000.0f32)); if (o1-o1b).abs()>1e-5{changed=true;}
            ui.label(egui::RichText::new("Grid").small().weak());
            let (gxb,gyb)=(gx_f,gy_f); ui.add(egui::DragValue::new(&mut gx_f).speed(0.1).range(1usize..=32usize)); ui.label(egui::RichText::new("×").small()); ui.add(egui::DragValue::new(&mut gy_f).speed(0.1).range(1usize..=32usize)); if gx_f!=gxb||gy_f!=gyb{changed=true;}
        });
        register_exposable_element(ui, node_id, "range_row", range_resp.response.rect);

        let grid_resp = ui.horizontal(|ui| {
            let snb=snap_on; ui.checkbox(&mut snap_on, egui::RichText::new("Snap").small()); if snap_on!=snb{changed=true;}
            ui.label(egui::RichText::new("Trail").small().weak()); let tmb=tm; ui.add(egui::DragValue::new(&mut tm).speed(5).range(0i64..=1000i64).suffix("ms")); if tm!=tmb{changed=true;}
        });
        register_exposable_element(ui, node_id, "grid_row", grid_resp.response.rect);

        let grid_opts_resp = ui.horizontal(|ui| {
            let ssgb=ssg; ui.checkbox(&mut ssg, egui::RichText::new("Scale grid").small()).on_hover_text("Adapt grid lines to Log/Exp scaling"); if ssg!=ssgb{changed=true;}
            let sglb=sgl; ui.checkbox(&mut sgl, egui::RichText::new("Labels").small()).on_hover_text("Show value labels on grid lines"); if sgl!=sglb{changed=true;}
        });
        register_exposable_element(ui, node_id, "grid_options_row", grid_opts_resp.response.rect);

        let hyst_resp = ui.horizontal(|ui| {
            ui.label(egui::RichText::new("Hyst").small().weak());
            let hpb=h_pct;
            ui.add_enabled(hyst_single, egui::DragValue::new(&mut h_pct).speed(0.01).range(0.0f32..=TWOWAY_HYST_MAX_PCT).suffix("%"))
                .on_hover_text(HYST_HINT).on_disabled_hover_text(HYST_GRAPH_HINT);
            if (h_pct-hpb).abs()>1e-5{changed=true;}
            if twoway_hyst_extras(ui, &mut h_dir, &mut h_start, true).0 { changed = true; }
        });
        register_exposable_element(ui, node_id, "hyst_row", hyst_resp.response.rect);

        let interp_resp = ui.horizontal(|ui| {
            ui.label(egui::RichText::new("Interp").small().weak()); let imb=i_ms; ui.add(egui::DragValue::new(&mut i_ms).speed(1.0).range(0.0f32..=500.0f32).suffix("ms")); if (i_ms-imb).abs()>1e-5{changed=true;}
            ui.label(egui::RichText::new("Peak hold").small().weak()).on_hover_text(PEAK_HOLD_HINT);
            let phb=ph_ms; ui.add(egui::DragValue::new(&mut ph_ms).speed(0.5).range(0.0f32..=200.0f32).suffix("ms")).on_hover_text(PEAK_HOLD_HINT); if (ph_ms-phb).abs()>1e-5{changed=true;}
            if twoway_dir_picker(ui, &mut ph_dir, PEAK_DIR_HINTS, true).0 { changed = true; }
        });
        register_exposable_element(ui, node_id, "interp_row", interp_resp.response.rect);

        if changed || params_changed {
            if let Some(node) = snarl.get_node_mut(node_id) {
                if let Some(n)=Number::from_f64(i1 as f64){node.params.insert("in_max".into(),Value::Number(n));}
                if let Some(n)=Number::from_f64(o1 as f64){node.params.insert("out_max".into(),Value::Number(n));}
                if let Some(n)=Number::from_f64(sc_t as f64){node.params.insert("scale_t".into(),Value::Number(n));}
                if let Some(n)=Number::from_f64(h_pct as f64){node.params.insert("hysteresis_pct".into(),Value::Number(n));}
                node.params.insert("hyst_dir".into(),Value::String(h_dir.clone()));
                node.params.insert("hyst_start_up".into(),Value::Bool(h_start));
                if let Some(n)=Number::from_f64(i_ms as f64){node.params.insert("interp_ms".into(),Value::Number(n));}
                if let Some(n)=Number::from_f64(ph_ms as f64){node.params.insert("peak_hold_ms".into(),Value::Number(n));}
                node.params.insert("peak_hold_dir".into(),Value::String(ph_dir.clone()));
                node.params.insert("grid_x".into(),serde_json::json!(gx_f as i64));
                node.params.insert("grid_y".into(),serde_json::json!(gy_f as i64));
                node.params.insert("snap".into(),Value::Bool(snap_on));
                node.params.insert("trail_ms".into(),serde_json::json!(tm));
                node.params.insert("absolute".into(),Value::Bool(abs_on));
                // An AutoMap curve's Vec mode is its picked signal's, not a param.
                if !automap { node.params.insert("vec_mode".into(),Value::Bool(vm)); }
                node.params.insert("active_lane".into(),Value::String(lane_sel.clone()));
                node.params.insert("show_scaled_grid".into(),Value::Bool(ssg));
                node.params.insert("show_grid_labels".into(),Value::Bool(sgl));
            }
        }

        if !automap { ui.horizontal(|ui| {
            ui.label(egui::RichText::new("Ch").small().weak());
            if ui.small_button("+").on_hover_text("Add channel").clicked() {
                if let Some(node) = snarl.get_node_mut(node_id) {
                    let next = node.inputs.len() + 1;
                    let sig = if vm { SignalType::Vec2 } else { SignalType::Float };
                    node.inputs.push(PinDescriptor::new(format!("In {}", next), sig));
                    node.outputs.push(PinDescriptor::new(format!("Out {}", next), sig));
                }
            }
            if n_channels > 1 && ui.small_button("−").on_hover_text("Remove last channel").clicked() {
                remove_input_pin(node_id, n_channels - 1, inputs, snarl);
                remove_output_pin(node_id, n_channels - 1, outputs, snarl);
            }
        }); }
    });

    if let Some(rect) = curve_graph_rect {
        register_exposable_element(ui, node_id, "curve", rect);
    }

    undo_requested || pts_up_changed || pts_dn_changed || pts_hy_changed
}

pub(crate) fn render_twoway_lane_toggle(
    inner_id: NodeId,
    ui: &mut egui::Ui,
    snarl: &mut Snarl<NodeData>,
    container: egui::Vec2,
) {
    let mut lane = snarl.get_node(inner_id)
        .and_then(|n| n.params.get("active_lane").and_then(|v| v.as_str()).map(|s| s.to_string()))
        .unwrap_or_else(|| "up".to_string());
    ui.set_max_width(container.x);
    apply_widget_scale(ui, container, egui::vec2(120.0, 22.0));
    let mut changed = false;
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new("Edit:").weak());
        let up = lane == "up";
        let dn = lane == "dn";
        if ui.selectable_label(up, egui::RichText::new("↑ Up")).clicked() && !up { lane = "up".into(); changed = true; }
        if ui.selectable_label(dn, egui::RichText::new("↓ Down")).clicked() && !dn { lane = "dn".into(); changed = true; }
        let hy = lane == "hy";
        if ui.selectable_label(hy, egui::RichText::new("Hyst")).on_hover_text(HYST_GRAPH_TAB_HINT).clicked() && !hy { lane = "hy".into(); changed = true; }
    });
    if changed {
        if let Some(node) = snarl.get_node_mut(inner_id) {
            node.params.insert("active_lane".into(), Value::String(lane));
        }
    }
}

pub(crate) fn render_twoway_curve_only(
    inner_id: NodeId,
    ui: &mut egui::Ui,
    snarl: &mut Snarl<NodeData>,
    container: egui::Vec2,
    graph_ov: Option<&crate::canvas::node::PinGraphOverride>,
) {
    // No vsync bypass — same rationale as show_response_curve_body.
    let avail = egui::vec2(container.x.max(20.0), container.y.max(20.0));
    let (rect, bg_resp) = ui.allocate_exact_size(avail, egui::Sense::click());
    let bg_for_menu = bg_resp.clone();
    paint_twoway_curve_graph(inner_id, ui, snarl, rect, bg_resp, graph_ov);
    // Right-click on empty graph → save/load/copy/paste/reset for the active
    // lane. The pinned widget doesn't expose the lane toggle, so users edit
    // whichever lane was last selected in the source module (or via the
    // "lane_toggle" pinned row if also pinned).
    let lane_name = snarl.get_node(inner_id)
        .and_then(|n| n.params.get("active_lane").and_then(|v| v.as_str()))
        .map(|s| match s { "dn" => "Down", "hy" => "Hyst", _ => "Up" })
        .unwrap_or("Up");
    bg_for_menu.context_menu(|ui| {
        curve_context_menu(ui, inner_id, snarl, Some(lane_name));
    });
}

/// Paints both up and down curves of a twoway response curve node into rect.
/// Also handles control-point interaction for the active lane.
pub(crate) fn paint_twoway_curve_graph(
    node_id: NodeId,
    ui: &mut egui::Ui,
    snarl: &mut Snarl<NodeData>,
    rect: egui::Rect,
    bg_resp: egui::Response,
    graph_ov: Option<&crate::canvas::node::PinGraphOverride>,
) {
    let node_data = match snarl.get_node(node_id).cloned() { Some(n) => n, None => return };

    let read_pts = |key: &str| -> Vec<[f32; 2]> {
        node_data.params.get(key).and_then(|v| v.as_array())
            .map(|arr| arr.iter().filter_map(|p| {
                let a = p.as_array()?;
                Some([a.get(0)?.as_f64()? as f32, a.get(1)?.as_f64()? as f32])
            }).collect())
            .unwrap_or_else(|| vec![[0.0, 0.0], [1.0, 1.0]])
    };
    let read_biases = |key: &str| -> Vec<f32> {
        node_data.params.get(key).and_then(|v| v.as_array())
            .map(|arr| arr.iter().filter_map(|b| b.as_f64().map(|f| f as f32)).collect())
            .unwrap_or_default()
    };

    let pts_up    = read_pts("points");
    let biases_up = read_biases("biases");
    let pts_dn    = read_pts("points_dn");
    let biases_dn = read_biases("biases_dn");

    let absolute   = node_data.params.get("absolute").and_then(|v| v.as_bool()).unwrap_or(true);
    let vec_mode   = twoway_vec_mode(&node_data);
    let in_max     = node_data.params.get("in_max").and_then(|v| v.as_f64()).unwrap_or(1.0) as f32;
    let in_min     = node_data.params.get("in_min").and_then(|v| v.as_f64()).unwrap_or(-1.0) as f32;
    let out_max    = node_data.params.get("out_max").and_then(|v| v.as_f64()).unwrap_or(1.0) as f32;
    let out_min    = node_data.params.get("out_min").and_then(|v| v.as_f64()).unwrap_or(-1.0) as f32;
    let scale_t    = node_data.params.get("scale_t").and_then(|v| v.as_f64()).map(|f| f as f32).unwrap_or(0.0);
    let grid_x     = node_data.params.get("grid_x").and_then(|v| v.as_i64()).unwrap_or(4).max(1) as usize;
    let grid_y     = node_data.params.get("grid_y").and_then(|v| v.as_i64()).unwrap_or(4).max(1) as usize;
    let snap       = node_data.params.get("snap").and_then(|v| v.as_bool()).unwrap_or(false);
    let active_lane = node_data.params.get("active_lane").and_then(|v| v.as_str()).unwrap_or("up").to_string();
    // TODO: scaled-grid overlay (`show_scaled_grid`) is not implemented for
    // this up/dn-lane renderer yet — the toggle exists and other curve
    // renderers honor it; read kept underscored until the overlay is ported.
    let _ssg       = node_data.params.get("show_scaled_grid").and_then(|v| v.as_bool()).unwrap_or(false);
    let sgl        = node_data.params.get("show_grid_labels").and_then(|v| v.as_bool()).unwrap_or(false);

    let absolute_eff = absolute || vec_mode;
    let (x_lo, x_hi): (f32, f32) = if absolute_eff { (0.0, 1.0) } else { (-1.0, 1.0) };
    // The Hyst graph's Y is a band width, 0..TWOWAY_HYST_MAX_PCT, in any mode.
    let hyst_lane = active_lane == "hy";
    let (y_lo, y_hi): (f32, f32) = if absolute_eff || hyst_lane { (0.0, 1.0) } else { (-1.0, 1.0) };
    let x_range = x_hi - x_lo;
    let y_range = y_hi - y_lo;
    let lane_up = active_lane == "up";
    let lane_ix: u8 = if lane_up { 0 } else if hyst_lane { 2 } else { 1 };

    let legend = curve_channel_labels(&node_data);

    let c2s = |x: f32, y: f32| egui::pos2(
        rect.left() + (x - x_lo) / x_range * rect.width(),
        rect.bottom() - (y - y_lo) / y_range * rect.height(),
    );
    let s2c = |pos: egui::Pos2| -> [f32; 2] {[
        x_lo + (pos.x - rect.left()) / rect.width() * x_range,
        y_lo + (rect.bottom() - pos.y) / rect.height() * y_range,
    ]};

    // Gamepad-nav: publish geometry (global/screen space) + read the selected
    // dot + bias-mode flag, same as the regular/vec curve graph.
    let pass = ui.ctx().cumulative_pass_nr();
    let to_global = ui.ctx().layer_transform_to_global(ui.layer_id())
        .unwrap_or(egui::emath::TSTransform::IDENTITY);
    ui.ctx().data_mut(|d| d.insert_temp(
        egui::Id::new(("gp_nav_curve_geom", node_id.0)),
        (pass, to_global * rect, x_lo, x_hi, y_lo, y_hi)));
    let nav_sel: Option<(u64, usize, bool)> = ui.ctx()
        .data(|d| d.get_temp(egui::Id::new(("gp_nav_curve_sel", node_id.0))));
    let nav_sel_dot: Option<usize> = nav_sel.filter(|(p,_,_)| crate::widgets::nav_pass_matches(ui.ctx(), *p)).map(|(_,i,_)| i);
    let nav_editing_dot: bool = nav_sel.map(|(_,_,e)| e).unwrap_or(false);
    let nav_bias = ui.ctx().data(|d|
        d.get_temp::<u64>(egui::Id::new(("gp_nav_curve_bias", node_id.0)))).map_or(false, |p| crate::widgets::nav_pass_matches(ui.ctx(), p));

    let painter = ui.painter_at(rect);
    let (graph_bg, graph_outline) = graph_chrome(graph_ov);
    painter.rect_filled(rect, 2.0, graph_bg);

    // Grid
    let abs_max_in  = in_max.abs().max(in_min.abs()).max(f32::EPSILON);
    let abs_max_out = out_max.abs().max(out_min.abs()).max(f32::EPSILON);
    let snap_nodes: Vec<f32> = (0..=grid_x).map(|i| i as f32 / grid_x as f32).collect();
    let snap_nodes_y: Vec<f32> = (0..=grid_y).map(|i| i as f32 / grid_y as f32).collect();
    let do_snap = |x: f32, y: f32| -> (f32, f32) {
        if !snap { return (x, y); }
        let u = ((x - x_lo) / x_range).clamp(0.0, 1.0);
        let v = ((y - y_lo) / y_range).clamp(0.0, 1.0);
        let su = snap_nodes.iter().copied().min_by(|a, b| (a-u).abs().partial_cmp(&(b-u).abs()).unwrap()).unwrap_or(u);
        let sv = snap_nodes_y.iter().copied().min_by(|a, b| (a-v).abs().partial_cmp(&(b-v).abs()).unwrap()).unwrap_or(v);
        (x_lo + su * x_range, y_lo + sv * y_range)
    };
    let gxp: Vec<f32> = (1..grid_x).map(|i| x_lo + i as f32 / grid_x as f32 * x_range).collect();
    let gyp: Vec<f32> = (1..grid_y).map(|i| y_lo + i as f32 / grid_y as f32 * y_range).collect();
    let (grid_faint, grid_axis) = graph_grid_colors(graph_ov);
    let gs = egui::Stroke::new(0.5, grid_faint);
    for &x in &gxp { painter.line_segment([c2s(x, y_lo), c2s(x, y_hi)], gs); }
    for &y in &gyp { painter.line_segment([c2s(x_lo, y), c2s(x_hi, y)], gs); }
    if !hyst_lane { painter.line_segment([c2s(x_lo, y_lo), c2s(x_hi, y_hi)], egui::Stroke::new(0.5, grid_axis)); }

    if sgl {
        let lc = Color32::from_rgba_unmultiplied(180, 180, 180, 160);
        let fnt = egui::FontId::proportional(9.0);
        let mut lsx = f32::NEG_INFINITY;
        for &x in &gxp {
            let sx = c2s(x, y_hi).x;
            if sx - lsx < 20.0 { continue; } lsx = sx;
            let u = (x - x_lo) / x_range;
            let val = if absolute_eff { curve_scale_inv(u, scale_t) * abs_max_in } else { let c = u*2.0-1.0; (if c<0.0{-1.0f32}else{1.0})*curve_scale_inv(c.abs(),scale_t)*abs_max_in };
            let lbl = if abs_max_in <= 1.01 { format!("{:.0}%", val*100.0) } else { format!("{:.2}", val) };
            painter.text(egui::pos2(sx+1.0, rect.top()+1.0), egui::Align2::LEFT_TOP, &lbl, fnt.clone(), lc);
        }
        let mut lsy = f32::INFINITY;
        for &y in &gyp {
            let sy = c2s(x_lo, y).y;
            if lsy - sy < 20.0 { continue; } lsy = sy;
            let v = (y - y_lo) / y_range;
            let val = if absolute_eff { curve_scale_inv(v, scale_t) * abs_max_out } else { let c = v*2.0-1.0; (if c<0.0{-1.0f32}else{1.0})*curve_scale_inv(c.abs(),scale_t)*abs_max_out };
            let lbl = if hyst_lane { format!("{:.1}%", v * TWOWAY_HYST_MAX_PCT) } else if abs_max_out <= 1.01 { format!("{:.0}%", val*100.0) } else { format!("{:.2}", val) };
            painter.text(egui::pos2(rect.left()+1.0, sy-9.0), egui::Align2::LEFT_TOP, &lbl, fnt.clone(), lc);
        }
    }

    // Inactive lane (dimmed)
    for (inact_pts, inact_bias) in twoway_dim_curves(lane_up, hyst_lane, absolute_eff, (&pts_up, &biases_up), (&pts_dn, &biases_dn)) {
    if inact_pts.len() >= 2 {
        let ic = Color32::from_rgba_unmultiplied(130, 130, 130, 70);
        let mut pp = c2s(x_lo, sample_curve(inact_pts, x_lo, inact_bias).clamp(y_lo, y_hi));
        for s in 1..=120usize { let t = s as f32/120.0; let ix = x_lo+t*x_range; let np = c2s(ix, sample_curve(inact_pts, ix, inact_bias).clamp(y_lo, y_hi)); painter.line_segment([pp, np], egui::Stroke::new(1.0, ic)); pp = np; }
    }
    }

    // Active lane — mutable for editing
    let (pts_hy, biases_hy) = twoway_hyst_points(&node_data);
    let (edit_pts, edit_biases) = if hyst_lane { (pts_hy.clone(), biases_hy.clone()) }
        else if lane_up { (pts_up.clone(), biases_up.clone()) } else { (pts_dn.clone(), biases_dn.clone()) };
    let mut new_edit_pts = edit_pts.clone();
    let mut new_edit_biases = edit_biases.clone();
    let mut pts_changed = false;
    let mut bias_changed = false;

    // A one-dot Hyst graph draws as its flat band.
    if !new_edit_pts.is_empty() {
        let cp: Vec<egui::Pos2> = (0..=120).map(|i| { let x = x_lo + x_range * i as f32 / 120.0; c2s(x, sample_curve(&new_edit_pts, x, &new_edit_biases).clamp(y_lo, y_hi)) }).collect();
        for w in cp.windows(2) { painter.line_segment([w[0], w[1]], egui::Stroke::new(1.5, Color32::from_gray(200))); }
    }

    // Alt-drag bias handles (mouse Alt OR gamepad bias mode).
    let alt_held = ui.input(|i| i.modifiers.alt) || nav_bias;
    if alt_held && new_edit_pts.len() >= 2 {
        while new_edit_biases.len() < new_edit_pts.len() - 1 { new_edit_biases.push(0.0); }
        for seg in 0..(new_edit_pts.len()-1) {
            let mid_x = (new_edit_pts[seg][0]+new_edit_pts[seg+1][0])*0.5;
            let mid_y = sample_curve(&new_edit_pts, mid_x, &new_edit_biases).clamp(y_lo, y_hi);
            let hpos = c2s(mid_x, mid_y);
            let hresp = ui.interact(egui::Rect::from_center_size(hpos, egui::Vec2::splat(14.0)), ui.id().with(("twbh_pin", node_id, lane_ix, seg as u32)), egui::Sense::click_and_drag());
            if hresp.double_clicked() { new_edit_biases[seg] = 0.0; bias_changed = true; }
            else if hresp.dragged() { let dy = -hresp.drag_delta().y / rect.height() * y_range; new_edit_biases[seg] = (new_edit_biases[seg] + dy).clamp(-2.0, 2.0); bias_changed = true; }
            let hcol = if hresp.hovered() || hresp.dragged() { Color32::from_rgb(255,220,50) } else { Color32::from_rgb(180,140,20) };
            painter.circle_filled(hpos, 4.0, hcol);
            painter.circle_stroke(hpos, 4.0, egui::Stroke::new(1.0, Color32::from_gray(100)));
        }
    }

    // Control point handles
    let mut remove_idx: Option<usize> = None;
    for i in 0..edit_pts.len() {
        let [px, py] = edit_pts[i];
        let screen = c2s(px, py);
        let pt_id  = ui.id().with(("twpt_pin", node_id, lane_ix, i as u32));
        let pt_resp = ui.interact(egui::Rect::from_center_size(screen, egui::Vec2::splat(12.0)), pt_id, egui::Sense::click_and_drag());
        let oid = ui.id().with(("twpt_orig_pin", node_id, lane_ix, i as u32));
        if pt_resp.drag_started() && !alt_held { ui.ctx().data_mut(|d| d.insert_temp(oid, [px, py, 0.0f32, 0.0f32])); }
        if pt_resp.dragged() && !alt_held {
            let prev = ui.ctx().data(|d| d.get_temp::<[f32;4]>(oid)).unwrap_or([px, py, 0.0, 0.0]);
            let dd = pt_resp.drag_delta();
            let (ax, ay) = (prev[2]+dd.x, prev[3]+dd.y);
            ui.ctx().data_mut(|d| d.insert_temp(oid, [prev[0], prev[1], ax, ay]));
            let nx = prev[0] + ax * x_range / rect.width();
            let ny = prev[1] - ay * y_range / rect.height();
            let lox = new_edit_pts.get(i.wrapping_sub(1)).map(|p| p[0]+0.001).unwrap_or(x_lo);
            let hix = new_edit_pts.get(i+1).map(|p| p[0]-0.001).unwrap_or(x_hi);
            let (sx, sy) = do_snap(nx, ny);
            new_edit_pts[i] = [sx.clamp(lox, hix), sy.clamp(y_lo, y_hi)];
            pts_changed = true;
        }
        if pt_resp.drag_stopped() { ui.ctx().data_mut(|d| d.remove_temp::<[f32;4]>(oid)); }
        if pt_resp.secondary_clicked() && edit_pts.len() > if hyst_lane { 1 } else { 2 } { remove_idx = Some(i); pts_changed = true; }
        // Gamepad-nav selected-dot highlight (active lane).
        if nav_sel_dot == Some(i) {
            let accent = ui.visuals().selection.stroke.color;
            let [r8, g8, b8, _] = accent.to_array();
            for k in 0..5 {
                let t = (k as f32 + 1.0) / 5.0;
                let rr = (if nav_editing_dot { 16.0 } else { 12.0 }) * t;
                let a = ((if nav_editing_dot { 170.0 } else { 120.0 }) * (1.0 - t)) as u8;
                if a == 0 { continue; }
                painter.circle_stroke(screen, rr,
                    egui::Stroke::new(2.0, Color32::from_rgba_unmultiplied(r8, g8, b8, a)));
            }
            painter.circle_filled(screen, if nav_editing_dot { 6.0 } else { 5.0 }, accent);
            painter.circle_stroke(screen, if nav_editing_dot { 6.0 } else { 5.0 },
                egui::Stroke::new(1.5, Color32::WHITE));
        }
        let nav_here = nav_sel_dot == Some(i);
        let col = if pt_resp.hovered() || pt_resp.dragged() || nav_here { Color32::WHITE } else { Color32::from_gray(190) };
        painter.circle_filled(screen, 5.0, col);
        painter.circle_stroke(screen, 5.0, egui::Stroke::new(1.0, Color32::from_gray(80)));
    }

    // Remove (right-click) / add (double-click) — biases stay on their segments.
    if let Some(idx) = remove_idx { curve_remove_point(&mut new_edit_pts, &mut new_edit_biases, idx); }
    if bg_resp.double_clicked() {
        if let Some(pos) = bg_resp.interact_pointer_pos() {
            let [gx_raw, gy_raw] = s2c(pos);
            let (gxs, gys) = do_snap(gx_raw, gy_raw);
            let gx = gxs.clamp(x_lo, x_hi); let gy = gys.clamp(y_lo, y_hi);
            let idx = new_edit_pts.partition_point(|p| p[0] < gx);
            curve_insert_point(&mut new_edit_pts, &mut new_edit_biases, idx, [gx, gy]);
            pts_changed = true;
        }
    }

    // Write back
    if (pts_changed || bias_changed) && hyst_lane {
        if let Some(node) = snarl.get_node_mut(node_id) {
            twoway_write_hyst_points(node, &new_edit_pts, &new_edit_biases);
        }
    } else if pts_changed || bias_changed {
        if let Some(node) = snarl.get_node_mut(node_id) {
            let pts_key   = if lane_up { "points" }    else { "points_dn" };
            let bias_key  = if lane_up { "biases" }    else { "biases_dn" };
            if pts_changed {
                new_edit_biases.resize(new_edit_pts.len().saturating_sub(1), 0.0);
                let j: Vec<Value> = new_edit_pts.iter().map(|p| serde_json::json!([p[0], p[1]])).collect();
                node.params.insert(pts_key.into(), Value::Array(j));
            }
            let bj: Vec<Value> = new_edit_biases.iter().filter_map(|&b| Number::from_f64(b as f64).map(Value::Number)).collect();
            node.params.insert(bias_key.into(), Value::Array(bj));
        }
    }

    // Live layer: band, raw tick, and the held point on the engine's lane.
    let geom = TwowayGeom::from_params(rect, &node_data);
    let has_active = paint_twoway_live(ui, &painter, &geom, node_id, &node_data,
        (&pts_up, &biases_up), (&pts_dn, &biases_dn),
        hyst_lane.then_some((pts_hy.as_slice(), biases_hy.as_slice())), "twtrail_pin",
        |ch| graph_channel_color(graph_ov, ch));
    if has_active { request_repaint_throttled(ui.ctx()); }
    if let Some(labels) = legend {
        paint_curve_legend(&painter, rect, labels, |ch| graph_channel_color(graph_ov, ch));
    }

    // Optional override frame, drawn last so it sits above the graph content.
    if let Some(stroke) = graph_outline {
        painter.rect_stroke(rect, 2.0, stroke, egui::StrokeKind::Inside);
    }
}

/// Hover text for the two-way curve's hysteresis band.
const HYST_HINT: &str = "Band the input must come back by before the lane flips (shaded on the graph).\n\
    Input noise smaller than this never moves the output, with no lag in the\n\
    direction of travel. Set it just above the input's peak-to-peak jitter —\n\
    watch the tick under the graph stay inside the shaded band.";

/// The Hyst row's direction picker and "Start ↑" toggle, shared by the body and
/// the pinned row. Returns whether either changed, and their two rects (for
/// gamepad nav, in that order).
fn twoway_hyst_extras(ui: &mut egui::Ui, dir: &mut String, start_up: &mut bool, small: bool) -> (bool, [egui::Rect; 2]) {
    let txt = |s: &str| if small { egui::RichText::new(s).small() } else { egui::RichText::new(s) };
    let (mut changed, group) = twoway_dir_picker(ui, dir, HYST_DIR_HINTS, small);
    let r = ui.checkbox(start_up, txt("Start ↑")).on_hover_text(
        "Start on Up: at rest the input sits on the Up curve, so a pull from rest\n\
         follows it from the first sample with no band to clear. The band still\n\
         applies once the input has moved off rest.");
    changed |= r.changed();
    (changed, [group, r.rect])
}

/// Hover texts for the Hyst row's ↕ ↑ ↓.
const HYST_DIR_HINTS: [&str; 3] = [
    "The band guards both flips: a press and a release each have to clear it",
    "The band guards only the press (Down → Up). A release flips to Down at once",
    "The band guards only the release (Up → Down). A press flips to Up at once",
];
/// Hover texts for Peak hold's ↕ ↑ ↓.
const PEAK_DIR_HINTS: [&str; 3] = [
    "Hold peaks on the press and the release",
    "Hold peaks on the press (Up curve) only — a release follows its curve at once",
    "Hold peaks on the release (Down curve) only",
];

/// A ↕ ↑ ↓ picker for a "both" / "up" / "down" param, with a hover text per
/// choice. Returns whether it changed, and its rect (for gamepad nav).
fn twoway_dir_picker(ui: &mut egui::Ui, dir: &mut String, hints: [&str; 3], small: bool) -> (bool, egui::Rect) {
    let mut changed = false;
    let rect = ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 2.0;
        for ((val, glyph), hint) in [("both", "↕"), ("up", "↑"), ("down", "↓")].into_iter().zip(hints) {
            let t = if small { egui::RichText::new(glyph).small() } else { egui::RichText::new(glyph) };
            if ui.selectable_label(dir == val, t).on_hover_text(hint).clicked() && dir != val {
                *dir = val.to_string();
                changed = true;
            }
        }
    }).response.rect;
    (changed, rect)
}

/// Hover text for Peak hold.
const PEAK_HOLD_HINT: &str = "Keep a curve peak up for at least this long (0 = off).\n\
    A pad reports every few ms, so a fast pull can step right over a narrow\n\
    peak, and one report on it is too short for a game to catch. With this on,\n\
    the highest the curve got across each step is put out and held. A flip\n\
    between curves drops it at once. ↕ ↑ ↓ picks the curve(s) it holds on.";

/// Hover text for the Hyst tab.
const HYST_GRAPH_TAB_HINT: &str = "Edit how wide the hysteresis band is along the input.\n\
    One dot is a flat band set by the Hyst value; add dots to vary it, e.g. no\n\
    band near rest and a wide one across a noisy resistance shelf.";
/// Hover text for the Hyst value while the Hyst graph has several dots.
const HYST_GRAPH_HINT: &str = "Set by the Hyst graph. Remove its dots down to one to edit it here.";

/// The Hyst graph's dots as the editor shows them: the stored graph at two
/// dots or more, else one dot at the Hyst value's height (Y 0..1 = 0..
/// `TWOWAY_HYST_MAX_PCT` %), at the X it was last left at.
pub(crate) fn twoway_hyst_points(n: &NodeData) -> (Vec<[f32; 2]>, Vec<f32>) {
    let pts: Vec<[f32; 2]> = n.params.get("hyst_points").and_then(|v| v.as_array())
        .map(|arr| arr.iter().filter_map(|p| {
            let a = p.as_array()?;
            Some([a.first()?.as_f64()? as f32, a.get(1)?.as_f64()? as f32])
        }).collect())
        .unwrap_or_default();
    if pts.len() >= 2 {
        let biases = n.params.get("hyst_biases").and_then(|v| v.as_array())
            .map(|arr| arr.iter().filter_map(|b| b.as_f64().map(|f| f as f32)).collect())
            .unwrap_or_default();
        return (pts, biases);
    }
    let pct = n.params.get("hysteresis_pct").and_then(|v| v.as_f64()).unwrap_or(0.5) as f32;
    let x = pts.first().map_or(0.0, |p| p[0]);
    (vec![[x, (pct / TWOWAY_HYST_MAX_PCT).clamp(0.0, 1.0)]], Vec::new())
}

/// Store an edited Hyst graph. A single dot is written as the Hyst value (its
/// height), so the value box and the dot stay one setting.
pub(crate) fn twoway_write_hyst_points(n: &mut NodeData, pts: &[[f32; 2]], biases: &[f32]) {
    let num = |f: f32| Number::from_f64(f as f64).map_or(Value::Null, Value::Number);
    if let [[x, y]] = pts {
        n.params.insert("hysteresis_pct".into(), num(y.clamp(0.0, 1.0) * TWOWAY_HYST_MAX_PCT));
        n.params.insert("hyst_points".into(), serde_json::json!([[x, y]]));
        n.params.insert("hyst_biases".into(), serde_json::json!([]));
    } else {
        let mut b = biases.to_vec();
        b.resize(pts.len().saturating_sub(1), 0.0);
        n.params.insert("hyst_points".into(), Value::Array(pts.iter().map(|p| serde_json::json!([p[0], p[1]])).collect()));
        n.params.insert("hyst_biases".into(), Value::Array(b.into_iter().map(num).collect()));
    }
}

/// The curves drawn dimmed behind the one being edited: the other lane, or
/// both behind the Hyst graph (to place the band against their features —
/// only in Abs / Vec, where they share its 0..1 Y).
fn twoway_dim_curves<'a>(
    lane_up: bool, hyst_lane: bool, absolute_eff: bool,
    up: (&'a [[f32; 2]], &'a [f32]), dn: (&'a [[f32; 2]], &'a [f32]),
) -> Vec<(&'a [[f32; 2]], &'a [f32])> {
    if hyst_lane { if absolute_eff { vec![up, dn] } else { Vec::new() } }
    else if lane_up { vec![dn] } else { vec![up] }
}

/// Where a two-way curve graph sits, for the live overlay.
pub(crate) struct TwowayGeom {
    pub rect: egui::Rect,
    pub x_lo: f32, pub x_hi: f32, pub y_lo: f32, pub y_hi: f32,
    pub absolute_eff: bool,
    pub in_min: f32, pub in_max: f32,
    pub scale_t: f32,
    /// Hysteresis as the engine applies it, in input units.
    pub hyst: TwowayHyst,
}

impl TwowayGeom {
    fn from_params(rect: egui::Rect, n: &NodeData) -> Self {
        let f = |k: &str, d: f64| n.params.get(k).and_then(|v| v.as_f64()).unwrap_or(d) as f32;
        let absolute_eff = n.params.get("absolute").and_then(|v| v.as_bool()).unwrap_or(true) || twoway_vec_mode(n);
        let (in_min, in_max) = (f("in_min", -1.0), f("in_max", 1.0));
        let lo = if absolute_eff { 0.0 } else { -1.0 };
        // The Hyst graph's Y is a band width: 0..1 in any mode.
        let y_lo = if n.params.get("active_lane").and_then(|v| v.as_str()) == Some("hy") { 0.0 } else { lo };
        TwowayGeom {
            rect, x_lo: lo, x_hi: 1.0, y_lo, y_hi: 1.0, absolute_eff, in_min, in_max,
            scale_t: f("scale_t", 0.0),
            hyst: TwowayHyst::from_params(&n.params, absolute_eff),
        }
    }
    /// Top of the tracked input range (a magnitude's top in Abs / Vec mode).
    fn in_hi(&self) -> f32 {
        if self.absolute_eff { self.in_max.abs().max(self.in_min.abs()) } else { self.in_max }
    }
    fn c2s(&self, x: f32, y: f32) -> egui::Pos2 {
        egui::pos2(
            self.rect.left() + (x - self.x_lo) / (self.x_hi - self.x_lo) * self.rect.width(),
            self.rect.bottom() - (y - self.y_lo) / (self.y_hi - self.y_lo) * self.rect.height(),
        )
    }
    /// An input value's graph X, through the range and the Log/Exp scale.
    fn gx(&self, v: f32) -> f32 {
        if self.absolute_eff {
            let abs_max = self.in_max.abs().max(self.in_min.abs()).max(f32::EPSILON);
            curve_scale((v.abs() / abs_max).clamp(0.0, 1.0), self.scale_t)
        } else {
            let inr = (self.in_max - self.in_min).abs().max(f32::EPSILON);
            let norm = ((v - self.in_min) / inr * 2.0 - 1.0).clamp(-1.0, 1.0);
            norm.signum() * curve_scale(norm.abs(), self.scale_t)
        }
    }
    /// The hysteresis band as a full-height strip from input `a` to `b`, with
    /// the edge at `edge` drawn as the line crossing it flips the lane.
    fn paint_band(&self, painter: &egui::Painter, a: f32, b: f32, edge: f32, col: Color32, glyph: &str) {
        let (xa, xb) = (self.c2s(self.gx(a), self.y_lo).x, self.c2s(self.gx(b), self.y_lo).x);
        let (l, r) = (xa.min(xb), xa.max(xb));
        // At least a sliver, so a small band still reads at small graph sizes.
        let (l, r) = if r - l < 2.0 { let c = (l + r) * 0.5; (c - 1.0, c + 1.0) } else { (l, r) };
        let [cr, cg, cb, _] = col.to_array();
        painter.rect_filled(
            egui::Rect::from_x_y_ranges(l..=r, self.rect.top()..=self.rect.bottom()),
            0.0, Color32::from_rgba_unmultiplied(cr, cg, cb, 34));
        let ex = self.c2s(self.gx(edge), self.y_lo).x;
        painter.line_segment([egui::pos2(ex, self.rect.top()), egui::pos2(ex, self.rect.bottom())],
            egui::Stroke::new(1.0, Color32::from_rgba_unmultiplied(cr, cg, cb, 140)));
        painter.text(egui::pos2(ex, self.rect.top() + 1.0), egui::Align2::CENTER_TOP, glyph,
            egui::FontId::proportional(9.0), Color32::from_rgba_unmultiplied(cr, cg, cb, 200));
    }
}

/// The live layer of a two-way curve graph. Per channel: the hysteresis band
/// behind the engine's held point (input wandering inside it changes nothing;
/// crossing its edge flips the lane), the raw input as a tick on the floor, and
/// the trail and arrow at the held point on the lane the engine is on. With no
/// live input, the band a pull from rest must clear before the Up lane engages
/// (none with "Start on Up").
/// Returns whether any channel was live.
#[allow(clippy::too_many_arguments)]
pub(crate) fn paint_twoway_live(
    ui: &egui::Ui,
    painter: &egui::Painter,
    g: &TwowayGeom,
    node_id: NodeId,
    node: &NodeData,
    up: (&[[f32; 2]], &[f32]),
    dn: (&[[f32; 2]], &[f32]),
    // Set while the Hyst graph is being edited: the live dot rides it instead.
    hyst_curve: Option<(&[[f32; 2]], &[f32])>,
    salt: &str,
    color: impl Fn(usize) -> Color32,
) -> bool {
    let n_channels = curve_channels(node);
    let trail_ms = node.params.get("trail_ms").and_then(|v| v.as_i64()).unwrap_or(300).clamp(0, 1000);
    let trail_dur = std::time::Duration::from_millis(trail_ms as u64);
    let now = std::time::Instant::now();
    let x_range = g.x_hi - g.x_lo;
    let mut has_active = false;
    for ch in 0..n_channels {
        let Some(raw) = node.extra.last_signals.get(ch).and_then(|s| s.as_ref()).map(sig_f32) else { continue };
        has_active = true;
        // The engine's tracker rides after the inputs as Vec2(held, lane).
        let (held, lane_up) = match node.extra.last_signals.get(n_channels + ch) {
            Some(Some(Signal::Vec2(v))) => (v.x, v.y >= 0.0),
            _ => (if g.absolute_eff { raw.abs() } else { raw }, true),
        };
        let (apts, abias) = if lane_up { up } else { dn };
        let ch_col = color(ch);

        // Band: behind the held point, against the lane's direction.
        // Clamped to the input range: at rest on the Up lane there is no band
        // below to draw.
        let band = g.hyst.band_for(lane_up, held);
        let edge = (if lane_up { held - band } else { held + band }).clamp(g.hyst.rest, g.in_hi());
        if (edge - held).abs() > f32::EPSILON {
            g.paint_band(painter, held, edge, edge, ch_col, if lane_up { "↓" } else { "↑" });
        }
        // Raw input tick on the floor — inside the band it moves, the dot doesn't.
        let rx = g.c2s(g.gx(raw), g.y_lo).x;
        painter.line_segment([egui::pos2(rx, g.rect.bottom()), egui::pos2(rx, g.rect.bottom() - 7.0)],
            egui::Stroke::new(2.0, Color32::from_rgba_unmultiplied(ch_col.r(), ch_col.g(), ch_col.b(), 220)));

        let graph_x = g.gx(held);
        if let Some((hp, hb)) = hyst_curve {
            // On the Hyst graph: the band in force, at the held point.
            let p = g.c2s(graph_x, sample_curve(hp, graph_x, hb).clamp(0.0, 1.0));
            painter.circle_filled(p, 4.0, Color32::from_rgba_unmultiplied(ch_col.r(), ch_col.g(), ch_col.b(), 230));
            continue;
        }
        let graph_y = sample_curve(apts, graph_x, abias).clamp(g.y_lo, g.y_hi);

        let lane_id: u8 = if lane_up { 0 } else { 1 };
        type Trail = std::collections::VecDeque<(f32, std::time::Instant)>;
        let tid  = ui.id().with((salt, node_id, ch as u32));
        let tlid = ui.id().with((salt, "lane", node_id, ch as u32));
        let prev_lane_id = ui.data(|d| d.get_temp::<u8>(tlid)).unwrap_or(lane_id);
        let mut tbuf: Trail = ui.data(|d| d.get_temp::<Trail>(tid).unwrap_or_default());
        if prev_lane_id != lane_id { tbuf.clear(); }
        if trail_ms > 0 {
            tbuf.push_back((graph_x, now));
            while tbuf.front().is_some_and(|&(_, t)| now.duration_since(t) > trail_dur) { tbuf.pop_front(); }
        } else { tbuf.clear(); }
        let tlist: Vec<(f32, std::time::Instant)> = tbuf.iter().cloned().collect();
        ui.data_mut(|d| { d.insert_temp(tid, tbuf); d.insert_temp(tlid, lane_id); });

        // Trail resamples the curve between x positions to follow its shape.
        for w in tlist.windows(2) {
            let (x0, _) = w[0]; let (x1, t1) = w[1];
            let age = now.duration_since(t1).as_secs_f32() / trail_dur.as_secs_f32().max(0.001);
            let alpha = ((1.0 - age.clamp(0.0, 1.0)) * 220.0) as u8;
            let tc = Color32::from_rgba_unmultiplied(ch_col.r(), ch_col.g(), ch_col.b(), alpha);
            let steps = (((x1 - x0).abs() / x_range * 80.0) as usize).max(1);
            let mut pp = g.c2s(x0, sample_curve(apts, x0, abias).clamp(g.y_lo, g.y_hi));
            for s in 1..=steps {
                let ix = x0 + (x1 - x0) * s as f32 / steps as f32;
                let np = g.c2s(ix, sample_curve(apts, ix, abias).clamp(g.y_lo, g.y_hi));
                painter.line_segment([pp, np], egui::Stroke::new(1.5, tc));
                pp = np;
            }
        }

        // Arrow along the curve, pointing the lane's way.
        let head = g.c2s(graph_x, graph_y);
        let eps = x_range * 0.015;
        let (x_a, x_b) = if lane_up {
            ((graph_x - eps).clamp(g.x_lo, g.x_hi), (graph_x + eps).clamp(g.x_lo, g.x_hi))
        } else {
            ((graph_x + eps).clamp(g.x_lo, g.x_hi), (graph_x - eps).clamp(g.x_lo, g.x_hi))
        };
        let p_a = g.c2s(x_a, sample_curve(apts, x_a, abias).clamp(g.y_lo, g.y_hi));
        let p_b = g.c2s(x_b, sample_curve(apts, x_b, abias).clamp(g.y_lo, g.y_hi));
        let tang = p_b - p_a;
        let fwd = tang / tang.length().max(0.001);
        let perp = egui::vec2(-fwd.y, fwd.x);
        let r = 6.0f32;
        let (tip, l, rp) = (head + fwd * r, head - fwd * (r * 0.5) + perp * (r * 0.7), head - fwd * (r * 0.5) - perp * (r * 0.7));
        painter.add(egui::Shape::convex_polygon(vec![tip, l, rp],
            Color32::from_rgba_unmultiplied(ch_col.r(), ch_col.g(), ch_col.b(), 230), egui::Stroke::NONE));
    }
    let rest_band = g.hyst.band_for(false, g.hyst.rest);
    if !has_active && !g.hyst.start_up && rest_band > 0.0 {
        // At rest: the band a pull has to clear before the Up lane engages.
        let (a, b) = (g.hyst.rest, (g.hyst.rest + rest_band).min(g.in_hi()));
        g.paint_band(painter, a, b, b, Color32::from_gray(170), "↑");
    }
    has_active
}

pub(crate) fn render_twoway_hyst_row(
    inner_id: NodeId,
    ui: &mut egui::Ui,
    snarl: &mut Snarl<NodeData>,
    container: egui::Vec2,
) {
    let (mut h_pct, mut h_dir, mut h_start) = snarl.get_node(inner_id).map(|n| (
        n.params.get("hysteresis_pct").and_then(|v| v.as_f64()).unwrap_or(0.5) as f32,
        n.params.get("hyst_dir").and_then(|v| v.as_str()).unwrap_or("both").to_string(),
        n.params.get("hyst_start_up").and_then(|v| v.as_bool()).unwrap_or(false),
    )).unwrap_or((0.5, "both".to_string(), false));
    let hyst_single = snarl.get_node(inner_id).is_none_or(|n| twoway_hyst_points(n).0.len() < 2);
    ui.set_max_width(container.x);
    apply_widget_scale(ui, container, egui::vec2(300.0, 22.0));
    let mut changed = false;
    let mut fr = [egui::Rect::NOTHING; 3];
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new("Hyst").weak());
        let r = ui.add_enabled(hyst_single, egui::DragValue::new(&mut h_pct).speed(0.01).range(0.0f32..=TWOWAY_HYST_MAX_PCT).suffix("%"))
            .on_hover_text(HYST_HINT).on_disabled_hover_text(HYST_GRAPH_HINT);
        fr[0] = r.rect; changed |= r.changed();
        let (c, rects) = twoway_hyst_extras(ui, &mut h_dir, &mut h_start, false);
        changed |= c;
        fr[1] = rects[0]; fr[2] = rects[1];
    });
    publish_nav_field_rects(ui, inner_id, &fr);
    if changed {
        if let Some(node) = snarl.get_node_mut(inner_id) {
            if let Some(n) = Number::from_f64(h_pct as f64) { node.params.insert("hysteresis_pct".into(), Value::Number(n)); }
            node.params.insert("hyst_dir".into(), Value::String(h_dir));
            node.params.insert("hyst_start_up".into(), Value::Bool(h_start));
        }
    }
}

pub(crate) fn render_twoway_interp_row(
    inner_id: NodeId,
    ui: &mut egui::Ui,
    snarl: &mut Snarl<NodeData>,
    container: egui::Vec2,
) {
    let (mut i_ms, mut ph_ms, mut ph_dir) = snarl.get_node(inner_id).map(|n| (
        n.params.get("interp_ms").and_then(|v| v.as_f64()).unwrap_or(50.0) as f32,
        n.params.get("peak_hold_ms").and_then(|v| v.as_f64()).unwrap_or(0.0) as f32,
        n.params.get("peak_hold_dir").and_then(|v| v.as_str()).unwrap_or("up").to_string(),
    )).unwrap_or((50.0, 0.0, "up".to_string()));
    ui.set_max_width(container.x);
    apply_widget_scale(ui, container, egui::vec2(360.0, 22.0));
    let mut changed = false;
    let mut fr = [egui::Rect::NOTHING; 3];
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new("Interp").weak());
        let r = ui.add(egui::DragValue::new(&mut i_ms).speed(1.0).range(0.0f32..=500.0f32).suffix("ms"));
        fr[0] = r.rect; changed |= r.changed();
        ui.label(egui::RichText::new("Peak hold").weak()).on_hover_text(PEAK_HOLD_HINT);
        let r = ui.add(egui::DragValue::new(&mut ph_ms).speed(0.5).range(0.0f32..=200.0f32).suffix("ms")).on_hover_text(PEAK_HOLD_HINT);
        fr[1] = r.rect; changed |= r.changed();
        let (c, r) = twoway_dir_picker(ui, &mut ph_dir, PEAK_DIR_HINTS, false);
        fr[2] = r; changed |= c;
    });
    publish_nav_field_rects(ui, inner_id, &fr);
    if changed {
        if let Some(node) = snarl.get_node_mut(inner_id) {
            if let Some(n) = Number::from_f64(i_ms as f64) { node.params.insert("interp_ms".into(), Value::Number(n)); }
            if let Some(n) = Number::from_f64(ph_ms as f64) { node.params.insert("peak_hold_ms".into(), Value::Number(n)); }
            node.params.insert("peak_hold_dir".into(), Value::String(ph_dir));
        }
    }
}
