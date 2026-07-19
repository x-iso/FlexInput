//! Oscillator and Envelope node bodies + the envelope curve editor.

use super::*;

pub(crate) fn show_oscillator_body(node_id: NodeId, inputs: &[InPin], ui: &mut egui::Ui, snarl: &mut Snarl<NodeData>) {
    let (shape, freq_unit, freq_p, phase_p, bipolar) = snarl.get_node(node_id).map(|n| {
        let shape      = n.params.get("shape")     .and_then(|v| v.as_str()) .unwrap_or("sine").to_string();
        let freq_unit  = n.params.get("freq_unit") .and_then(|v| v.as_str()) .unwrap_or("hz").to_string();
        let freq_p     = n.params.get("freq_param") .and_then(|v| v.as_f64()).unwrap_or(1.0)  as f32;
        let phase_p    = n.params.get("phase_param").and_then(|v| v.as_f64()).unwrap_or(0.0)  as f32;
        let bipolar    = n.params.get("bipolar")   .and_then(|v| v.as_bool()).unwrap_or(true);
        (shape, freq_unit, freq_p, phase_p, bipolar)
    }).unwrap_or_default();

    let freq_wired  = inputs.get(0).map(|p| !p.remotes.is_empty()).unwrap_or(false);
    let phase_wired = inputs.get(1).map(|p| !p.remotes.is_empty()).unwrap_or(false);

    let mut shape     = shape;
    let mut freq_unit = freq_unit;
    let mut freq_p    = freq_p;
    let mut phase_p   = phase_p;
    let mut bipolar   = bipolar;
    let mut changed   = false;

    let mut shape_rect:   Option<egui::Rect> = None;
    let mut freq_rect:    Option<egui::Rect> = None;
    let mut phase_rect:   Option<egui::Rect> = None;
    let mut preview_rect: Option<egui::Rect> = None;
    ui.vertical(|ui| {
        // Row 1: shape selector
        let r = ui.horizontal(|ui| {
            changed |= ui.selectable_value(&mut shape, "sine".into(),     egui::RichText::new("Sine").small()).changed();
            changed |= ui.selectable_value(&mut shape, "triangle".into(), egui::RichText::new("Tri").small()).changed();
            changed |= ui.selectable_value(&mut shape, "saw".into(),      egui::RichText::new("Saw").small()).changed();
            changed |= ui.selectable_value(&mut shape, "square".into(),   egui::RichText::new("Sqr").small()).changed();
        });
        shape_rect = Some(r.response.rect);

        // Row 2: frequency unit toggle + value
        let r = ui.horizontal(|ui| {
            changed |= ui.selectable_value(&mut freq_unit, "hz".into(), egui::RichText::new("Hz").small()).changed();
            changed |= ui.selectable_value(&mut freq_unit, "ms".into(), egui::RichText::new("ms").small()).changed();
            let (lo, hi, spd) = if freq_unit == "hz" { (0.01, 200.0, 0.1) } else { (1.0, 60_000.0, 10.0) };
            changed |= ui.add_enabled(!freq_wired, egui::DragValue::new(&mut freq_p).speed(spd).range(lo..=hi)).changed();
        });
        freq_rect = Some(r.response.rect);

        // Row 3: phase offset
        let r = ui.horizontal(|ui| {
            ui.label(egui::RichText::new("Phase").small().weak());
            changed |= ui.add_enabled(!phase_wired, egui::DragValue::new(&mut phase_p).speed(0.01).range(0.0..=1.0)).changed();
            // Bi/Uni toggle
            ui.separator();
            changed |= ui.selectable_value(&mut bipolar, true,  egui::RichText::new("Bi").small()).changed();
            changed |= ui.selectable_value(&mut bipolar, false, egui::RichText::new("Uni").small()).changed();
        });
        phase_rect = Some(r.response.rect);

        // Row 4: waveform preview
        let preview_size = egui::vec2(ui.available_width().max(80.0), 36.0);
        let (rect, _) = ui.allocate_exact_size(preview_size, egui::Sense::hover());
        preview_rect = Some(rect);
        if ui.is_rect_visible(rect) {
            let painter = ui.painter_at(rect);
            painter.rect_filled(rect, 2.0, egui::Color32::from_gray(18));

            // Zero / baseline grid line
            let zero_y = if bipolar {
                rect.center().y
            } else {
                rect.bottom()
            };
            painter.line_segment(
                [egui::pos2(rect.left(), zero_y), egui::pos2(rect.right(), zero_y)],
                egui::Stroke::new(0.5, egui::Color32::from_gray(55)),
            );

            // Waveform
            let n = 128usize;
            let pts: Vec<egui::Pos2> = (0..=n).map(|i| {
                let t = i as f32 / n as f32;
                let phase = (t + phase_p).rem_euclid(1.0);
                let v = {
                    let raw = flexinput_engine::osc_sample(&shape, phase);
                    if bipolar { raw } else { (raw + 1.0) * 0.5 }
                };
                let x = rect.left() + t * rect.width();
                let y = if bipolar {
                    rect.center().y - v * rect.height() * 0.45
                } else {
                    rect.bottom() - v * rect.height() * 0.9
                };
                egui::pos2(x, y.clamp(rect.top(), rect.bottom()))
            }).collect();
            painter.add(egui::Shape::line(pts, egui::Stroke::new(1.5, egui::Color32::from_rgb(100, 180, 255))));
        }
    });

    if changed {
        if let Some(node) = snarl.get_node_mut(node_id) {
            node.params.insert("shape".into(),      Value::String(shape));
            node.params.insert("freq_unit".into(),  Value::String(freq_unit));
            node.params.insert("bipolar".into(),    Value::Bool(bipolar));
            if let Some(n) = Number::from_f64(freq_p  as f64) { node.params.insert("freq_param".into(),  Value::Number(n)); }
            if let Some(n) = Number::from_f64(phase_p as f64) { node.params.insert("phase_param".into(), Value::Number(n)); }
        }
    }
    if let Some(r) = shape_rect   { register_exposable_element(ui, node_id, "shape",   r); }
    if let Some(r) = freq_rect    { register_exposable_element(ui, node_id, "freq",    r); }
    if let Some(r) = phase_rect   { register_exposable_element(ui, node_id, "phase",   r); }
    if let Some(r) = preview_rect { register_exposable_element(ui, node_id, "preview", r); }
}

// ── Envelope Generator ────────────────────────────────────────────────────────

pub(crate) fn envelope_init_params(node: &mut crate::canvas::NodeData) {
    if node.params.contains_key("points") { return; }
    node.params.insert("points".into(),          serde_json::json!([[0.0, 0.0], [0.3, 1.0], [1.0, 0.0]]));
    node.params.insert("biases".into(),          serde_json::json!([0.0, 0.0]));
    node.params.insert("grid_x".into(),           serde_json::json!(4i64));
    node.params.insert("grid_y".into(),           serde_json::json!(4i64));
    node.params.insert("snap".into(),             Value::Bool(false));
    node.params.insert("show_grid".into(),        Value::Bool(true));
    node.params.insert("show_grid_labels".into(), Value::Bool(false));
    node.params.insert("time_mul".into(),         serde_json::json!(500.0f64));
    node.params.insert("timebase".into(),         Value::String("ms".into()));
    node.params.insert("sustain".into(),          serde_json::json!(0.3f64));
    // Behavior flags (off = one-shot); combinable Hold / Bounce / Loop.
    node.params.insert("hold".into(),             Value::Bool(false));
    node.params.insert("bounce".into(),           Value::Bool(false));
    node.params.insert("loop".into(),             Value::Bool(false));
}

/// Core envelope graph painter — curve editing + sustain line + playhead + live trail.
pub(crate) fn paint_envelope_curve_graph(
    node_id: NodeId,
    ui: &mut egui::Ui,
    snarl: &mut Snarl<NodeData>,
    rect: egui::Rect,
    bg_resp: egui::Response,
    graph_ov: Option<&crate::canvas::node::PinGraphOverride>,
) {
    if let Some(node) = snarl.get_node_mut(node_id) { envelope_init_params(node); }

    let (points, biases, grid_x, grid_y, snap, sustain, show_grid, show_grid_labels, time_mul, timebase, mode) =
        snarl.get_node(node_id).map(|n| {
            let pts: Vec<[f32; 2]> = n.params.get("points")
                .and_then(|v| v.as_array())
                .map(|arr| arr.iter().filter_map(|p| {
                    let a = p.as_array()?;
                    Some([a.get(0)?.as_f64()? as f32, a.get(1)?.as_f64()? as f32])
                }).collect())
                .unwrap_or_else(|| vec![[0.0, 0.0], [1.0, 0.0]]);
            let bss: Vec<f32> = n.params.get("biases")
                .and_then(|v| v.as_array())
                .map(|arr| arr.iter().filter_map(|b| b.as_f64().map(|f| f as f32)).collect())
                .unwrap_or_default();
            let gx   = n.params.get("grid_x").and_then(|v| v.as_i64()).unwrap_or(4).max(1) as usize;
            let gy   = n.params.get("grid_y").and_then(|v| v.as_i64()).unwrap_or(4).max(1) as usize;
            let sn   = n.params.get("snap").and_then(|v| v.as_bool()).unwrap_or(false);
            let sus  = n.params.get("sustain").and_then(|v| v.as_f64()).unwrap_or(0.3) as f32;
            let sg   = n.params.get("show_grid").and_then(|v| v.as_bool()).unwrap_or(true);
            let sgl  = n.params.get("show_grid_labels").and_then(|v| v.as_bool()).unwrap_or(false);
            let tmul = n.params.get("time_mul").and_then(|v| v.as_f64()).unwrap_or(500.0) as f32;
            let tb   = n.params.get("timebase").and_then(|v| v.as_str()).unwrap_or("ms").to_string();
            let (hold, bounce, loopf) = flexinput_engine::envelope_flags(&n.params);
            (pts, bss, gx, gy, sn, sus, sg, sgl, tmul, tb, (hold, bounce, loopf))
        }).unwrap_or_else(|| (vec![[0.0, 0.0], [1.0, 0.0]], vec![], 4, 4, false, 0.3, true, false, 500.0, "ms".into(), (false, false, false)));
    let (hold, bounce, loopf) = mode;
    // The flat-value "buffer" visual applies only to Hold+Bounce without Loop.
    let buffer_mode = hold && bounce && !loopf;
    let _ = loopf;

    // Period (seconds) — used for X labels and trail duration
    let period_s: f32 = match timebase.as_str() {
        "s"  => time_mul.max(0.0001),
        "hz" => if time_mul > 0.0 { 1.0 / time_mul } else { 1.0 },
        _    => (time_mul / 1000.0).max(0.0001),
    };
    let period_ms = period_s * 1000.0;

    // Current phase from last_signals[1]; discontinuity epoch from [2].
    let phase: Option<f32> = snarl.get_node(node_id)
        .and_then(|n| n.extra.last_signals.get(1))
        .and_then(|s| if let Some(Signal::Float(f)) = s { Some(*f) } else { None });
    let epoch: f32 = snarl.get_node(node_id)
        .and_then(|n| n.extra.last_signals.get(2))
        .and_then(|s| if let Some(Signal::Float(f)) = s { Some(*f) } else { None })
        .unwrap_or(0.0);

    let mut new_points   = points.clone();
    let mut new_biases   = biases.clone();
    let mut pts_changed  = false;
    let mut bias_changed = false;

    // Sustain always sits on a control point: snap to the nearest point X (drives
    // the orange line and, in Hold+Bounce, the flat-hold region). Computed up
    // front from the current points so the curve drawing can dim the post-sustain
    // segment. During an active drag this lags one frame, same as the slider thumb.
    let sustain_snapped = if !new_points.is_empty() {
        new_points.iter().map(|p| p[0])
            .min_by(|a, b| (a - sustain).abs().partial_cmp(&(b - sustain).abs()).unwrap())
            .unwrap_or(sustain)
    } else { sustain };
    let sustain_changed = (sustain_snapped - sustain).abs() > 1e-6;
    // Sample the curve, applying the Hold+Bounce flat-hold past the sustain point.
    let sample_y = |pts: &[[f32; 2]], biases: &[f32], x: f32| -> f32 {
        let sx = if buffer_mode { x.min(sustain_snapped) } else { x };
        flexinput_engine::sample_curve(pts, sx, biases).clamp(0.0, 1.0)
    };

    let painter = ui.painter_at(rect);

    // [0..1] x [0..1] coordinate space
    let c2s = |x: f32, y: f32| egui::pos2(
        rect.left() + x * rect.width(),
        rect.bottom() - y * rect.height(),
    );
    let s2c = |pos: egui::Pos2| -> [f32; 2] {[
        (pos.x - rect.left()) / rect.width(),
        (rect.bottom() - pos.y) / rect.height(),
    ]};

    let snap_nodes_x: Vec<f32> = (0..=grid_x).map(|i| i as f32 / grid_x as f32).collect();
    let snap_nodes_y: Vec<f32> = (0..=grid_y).map(|i| i as f32 / grid_y as f32).collect();
    let grid_x_positions: Vec<f32> = (1..grid_x).map(|i| i as f32 / grid_x as f32).collect();
    let grid_y_positions: Vec<f32> = (1..grid_y).map(|i| i as f32 / grid_y as f32).collect();

    let do_snap = |x: f32, y: f32| -> (f32, f32) {
        if !snap { return (x, y); }
        let sx = snap_nodes_x.iter().copied()
            .min_by(|a, b| (a - x).abs().partial_cmp(&(b - x).abs()).unwrap()).unwrap_or(x);
        let sy = snap_nodes_y.iter().copied()
            .min_by(|a, b| (a - y).abs().partial_cmp(&(b - y).abs()).unwrap()).unwrap_or(y);
        (sx, sy)
    };

    // Background + optional outline
    let (graph_bg, graph_outline) = graph_chrome(graph_ov);
    painter.rect_filled(rect, 2.0, graph_bg);

    // Grid lines
    if show_grid {
        let (grid_faint, _grid_axis) = graph_grid_colors(graph_ov);
        let gs = egui::Stroke::new(0.5, grid_faint);
        for &x in &grid_x_positions { painter.line_segment([c2s(x, 0.0), c2s(x, 1.0)], gs); }
        for &y in &grid_y_positions  { painter.line_segment([c2s(0.0, y), c2s(1.0, y)], gs); }
    }

    // Grid labels: X = time position, Y = 0-100%
    if show_grid_labels {
        const MIN_LABEL_PX: f32 = 20.0;
        let label_col = Color32::from_rgba_unmultiplied(180, 180, 180, 160);
        let font = egui::FontId::proportional(9.0);
        let mut last_sx = f32::NEG_INFINITY;
        for &u in &grid_x_positions {
            let sx = c2s(u, 1.0).x;
            if sx - last_sx < MIN_LABEL_PX { continue; }
            last_sx = sx;
            let t_ms = u * period_ms;
            let label = if period_ms >= 1000.0 {
                format!("{:.2}s", t_ms / 1000.0)
            } else {
                format!("{:.0}ms", t_ms)
            };
            painter.text(egui::pos2(sx + 1.0, rect.top() + 1.0),
                egui::Align2::LEFT_TOP, &label, font.clone(), label_col);
        }
        let mut last_sy = f32::INFINITY;
        for &v in &grid_y_positions {
            let sy = c2s(0.0, v).y;
            if last_sy - sy < MIN_LABEL_PX { continue; }
            last_sy = sy;
            let label = format!("{:.0}%", v * 100.0);
            painter.text(egui::pos2(rect.left() + 1.0, sy - 9.0),
                egui::Align2::LEFT_TOP, &label, font.clone(), label_col);
        }
    }

    // Curve — mid-gray so the colored trail stands out. In Hold+Bounce the curve
    // after the sustain point is inactive (the value is held flat through the
    // buffer), so dim it and draw the held horizontal path in the active color.
    if new_points.len() >= 2 {
        let steps = 120usize;
        let main_col = Color32::from_gray(120);
        let dim_col  = Color32::from_gray(55);
        for i in 0..steps {
            let x0 = i as f32 / steps as f32;
            let x1 = (i + 1) as f32 / steps as f32;
            let y0 = flexinput_engine::sample_curve(&new_points, x0, &new_biases).clamp(0.0, 1.0);
            let y1 = flexinput_engine::sample_curve(&new_points, x1, &new_biases).clamp(0.0, 1.0);
            let col = if buffer_mode && x0 >= sustain_snapped { dim_col } else { main_col };
            painter.line_segment([c2s(x0, y0), c2s(x1, y1)], egui::Stroke::new(1.5, col));
        }
        // Hold+Bounce: held horizontal path at the sustain value across the buffer.
        if buffer_mode {
            let hold_y = flexinput_engine::sample_curve(&new_points, sustain_snapped, &new_biases).clamp(0.0, 1.0);
            painter.line_segment(
                [c2s(sustain_snapped, hold_y), c2s(1.0, hold_y)],
                egui::Stroke::new(1.5, main_col),
            );
        }
    }

    // Bias handles (Alt held)
    let nav_bias = ui.ctx().data(|d|
        d.get_temp::<u64>(egui::Id::new(("gp_nav_curve_bias", node_id.0))))
        == Some(ui.ctx().cumulative_pass_nr());
    let alt_held = ui.input(|i| i.modifiers.alt) || nav_bias;
    if alt_held && new_points.len() >= 2 {
        while new_biases.len() < new_points.len() - 1 { new_biases.push(0.0); }
        for seg in 0..(new_points.len() - 1) {
            let mid_x = (new_points[seg][0] + new_points[seg + 1][0]) * 0.5;
            let mid_y = flexinput_engine::sample_curve(&new_points, mid_x, &new_biases).clamp(0.0, 1.0);
            let hpos  = c2s(mid_x, mid_y);
            let hid   = ui.id().with(("env_bias_h", node_id, seg));
            let hresp = ui.interact(
                egui::Rect::from_center_size(hpos, egui::Vec2::splat(14.0)),
                hid, egui::Sense::click_and_drag());
            if hresp.double_clicked() { new_biases[seg] = 0.0; bias_changed = true; }
            else if hresp.dragged() {
                new_biases[seg] = (new_biases[seg] - hresp.drag_delta().y / rect.height()).clamp(-2.0, 2.0);
                bias_changed = true;
            }
            let hcol = if hresp.hovered() || hresp.dragged() {
                Color32::from_rgb(255, 220, 50)
            } else { Color32::from_rgb(180, 140, 20) };
            painter.circle_filled(hpos, 4.0, hcol);
            painter.circle_stroke(hpos, 4.0, egui::Stroke::new(1.0, Color32::from_gray(100)));
        }
    }

    // Control points
    let mut remove_idx: Option<usize> = None;
    for i in 0..new_points.len() {
        let [px, py] = new_points[i];
        let screen   = c2s(px, py);
        let pt_id    = ui.id().with(("env_cpt", node_id, i));
        let pt_resp  = ui.interact(
            egui::Rect::from_center_size(screen, egui::Vec2::splat(12.0)),
            pt_id, egui::Sense::click_and_drag());
        let origin_id = ui.id().with(("env_cpt_origin", node_id, i));
        if pt_resp.drag_started() && !alt_held {
            ui.ctx().data_mut(|d| d.insert_temp(origin_id, [px, py, 0.0f32, 0.0f32]));
        }
        if pt_resp.dragged() && !alt_held {
            let prev = ui.ctx().data(|d| d.get_temp::<[f32; 4]>(origin_id))
                .unwrap_or([px, py, 0.0, 0.0]);
            let acc_x = prev[2] + pt_resp.drag_delta().x;
            let acc_y = prev[3] + pt_resp.drag_delta().y;
            ui.ctx().data_mut(|d| d.insert_temp(origin_id, [prev[0], prev[1], acc_x, acc_y]));
            let nx_raw = prev[0] + acc_x / rect.width();
            let ny_raw = prev[1] - acc_y / rect.height();
            let lo_x = new_points.get(i.wrapping_sub(1)).map(|p| p[0] + 0.001).unwrap_or(0.0);
            let hi_x = new_points.get(i + 1).map(|p| p[0] - 0.001).unwrap_or(1.0);
            let (sx, sy) = do_snap(nx_raw, ny_raw);
            new_points[i] = [sx.clamp(lo_x, hi_x), sy.clamp(0.0, 1.0)];
            pts_changed = true;
        }
        if pt_resp.drag_stopped() {
            ui.ctx().data_mut(|d| d.remove_temp::<[f32; 4]>(origin_id));
        }
        if pt_resp.secondary_clicked() && new_points.len() > 2 {
            remove_idx = Some(i);
            pts_changed = true;
        }
        let col = if pt_resp.hovered() || pt_resp.dragged() { Color32::WHITE } else { Color32::from_gray(190) };
        painter.circle_filled(screen, 5.0, col);
        painter.circle_stroke(screen, 5.0, egui::Stroke::new(1.0, Color32::from_gray(80)));
    }

    // Double-click adds a point
    if bg_resp.double_clicked() {
        if let Some(pos) = bg_resp.interact_pointer_pos() {
            let [gx_raw, gy_raw] = s2c(pos);
            let (gx_sn, gy_sn) = do_snap(gx_raw, gy_raw);
            let gx = gx_sn.clamp(0.0, 1.0);
            let gy = gy_sn.clamp(0.0, 1.0);
            let idx = new_points.partition_point(|p| p[0] < gx);
            new_points.insert(idx, [gx, gy]);
            pts_changed = true;
        }
    }
    if let Some(idx) = remove_idx { new_points.remove(idx); }

    // Publish geometry for gamepad-nav (same ids as response_curve so the driver works here too)
    {
        let pass = ui.ctx().cumulative_pass_nr();
        let to_global = ui.ctx().layer_transform_to_global(ui.layer_id())
            .unwrap_or(egui::emath::TSTransform::IDENTITY);
        let screen_rect = to_global * rect;
        ui.ctx().data_mut(|d| d.insert_temp(
            egui::Id::new(("gp_nav_curve_geom", node_id.0)),
            (pass, screen_rect, 0.0f32, 1.0f32, 0.0f32, 1.0f32)));
    }

    // Sustain vertical line (orange). `sustain_snapped` was computed up front
    // (snaps to nearest control point so it follows the dots on edit/load).
    let sus_x = rect.left() + sustain_snapped.clamp(0.0, 1.0) * rect.width();
    painter.line_segment(
        [egui::pos2(sus_x, rect.top()), egui::pos2(sus_x, rect.bottom())],
        egui::Stroke::new(1.5, Color32::from_rgba_unmultiplied(255, 155, 30, 200)),
    );

    // Playhead line + live trail dot (phase position along the curve)
    if let Some(ph) = phase {
        let ph = ph.clamp(0.0, 1.0);

        // Trail color: user override for channel 0, else default cyan
        let dot_col = graph_ov
            .and_then(|o| o.channel_colors.first().copied().flatten())
            .map(rgba_to_color32)
            .unwrap_or(Color32::from_rgb(80, 200, 255));

        // Playhead line — subtle underlay
        let ph_x = rect.left() + ph * rect.width();
        painter.line_segment(
            [egui::pos2(ph_x, rect.top()), egui::pos2(ph_x, rect.bottom())],
            egui::Stroke::new(1.0, Color32::from_rgba_unmultiplied(255, 255, 255, 80)),
        );

        // Trail covers ~1/3 of the envelope period so it traces ~1/3 of the graph.
        // Each point carries the engine's discontinuity epoch so we can break the
        // line exactly where the dot teleported (retrigger, loop wrap/reset, hold
        // early-release jump) instead of guessing from phase direction.
        let trail_dur = std::time::Duration::from_secs_f32(period_s / 3.0);
        let now = std::time::Instant::now();
        type Trail = std::collections::VecDeque<(f32, f32, std::time::Instant)>;
        let trail_id = ui.id().with(("env_trail", node_id));
        let mut trail: Trail = ui.data(|d| d.get_temp::<Trail>(trail_id).clone().unwrap_or_default());
        trail.push_back((ph, epoch, now));
        while trail.front().map(|&(_, _, t)| now.duration_since(t) > trail_dur).unwrap_or(false) {
            trail.pop_front();
        }
        let trail_pts: Vec<(f32, f32, std::time::Instant)> = trail.iter().cloned().collect();
        ui.data_mut(|d| d.insert_temp(trail_id, trail));

        let trail_secs = trail_dur.as_secs_f32().max(0.001);
        for w in trail_pts.windows(2) {
            let (x0, e0, _)  = w[0];
            let (x1, e1, t1) = w[1];
            // Discontinuity: the dot teleported between these samples. Skip the
            // bridging segment so the old trail fades in place and the new trail
            // starts fresh at the landing spot.
            if e1 != e0 { continue; }
            let age   = now.duration_since(t1).as_secs_f32() / trail_secs;
            let alpha = ((1.0 - age.clamp(0.0, 1.0)) * 200.0) as u8;
            let col   = Color32::from_rgba_unmultiplied(dot_col.r(), dot_col.g(), dot_col.b(), alpha);
            let steps = (((x1 - x0).abs() * 80.0) as usize).max(1);
            let x0_y = sample_y(&new_points, &new_biases, x0);
            let mut prev = c2s(x0, x0_y);
            for s in 1..=steps {
                let t  = s as f32 / steps as f32;
                let ix = x0 + (x1 - x0) * t;
                let iy = sample_y(&new_points, &new_biases, ix);
                let next = c2s(ix, iy);
                painter.line_segment([prev, next], egui::Stroke::new(1.5, col));
                prev = next;
            }
        }

        // Live dot at current position (held flat past sustain in Hold+Bounce)
        let ph_y = sample_y(&new_points, &new_biases, ph);
        painter.circle_filled(c2s(ph, ph_y), 3.5,
            Color32::from_rgba_unmultiplied(dot_col.r(), dot_col.g(), dot_col.b(), 220));

        request_repaint_throttled(ui.ctx());
    }

    // Optional override outline frame
    if let Some(stroke) = graph_outline {
        painter.rect_stroke(rect, 2.0, stroke, egui::StrokeKind::Inside);
    }

    // Write back
    if pts_changed || bias_changed || sustain_changed {
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
            if sustain_changed {
                if let Some(n) = Number::from_f64(sustain_snapped as f64) {
                    node.params.insert("sustain".into(), Value::Number(n));
                }
            }
        }
    }
}

pub(crate) fn show_envelope_body(node_id: NodeId, inputs: &[InPin], ui: &mut egui::Ui, snarl: &mut Snarl<NodeData>) {
    if let Some(node) = snarl.get_node_mut(node_id) { envelope_init_params(node); }

    let (flags, timebase, time_mul, sustain, grid_x, grid_y, snap, show_grid, show_grid_labels) =
        snarl.get_node(node_id).map(|n| {
            let fl  = flexinput_engine::envelope_flags(&n.params);
            let tb  = n.params.get("timebase").and_then(|v| v.as_str()).unwrap_or("ms").to_string();
            let tm  = n.params.get("time_mul").and_then(|v| v.as_f64()).unwrap_or(500.0) as f32;
            let su  = n.params.get("sustain") .and_then(|v| v.as_f64()).unwrap_or(0.3)   as f32;
            let gx  = n.params.get("grid_x").and_then(|v| v.as_i64()).unwrap_or(4).max(1);
            let gy  = n.params.get("grid_y").and_then(|v| v.as_i64()).unwrap_or(4).max(1);
            let sn  = n.params.get("snap").and_then(|v| v.as_bool()).unwrap_or(false);
            let sg  = n.params.get("show_grid").and_then(|v| v.as_bool()).unwrap_or(true);
            let sgl = n.params.get("show_grid_labels").and_then(|v| v.as_bool()).unwrap_or(false);
            (fl, tb, tm, su, gx, gy, sn, sg, sgl)
        }).unwrap_or_else(|| ((false, false, false), "ms".into(), 500.0, 0.3, 4, 4, false, true, false));

    // Detect Time-input wiring from the snarl directly (input pin 1) rather than
    // the passed `inputs` slice — the sub-patch inner body is rendered with an
    // empty slice, so this is the only way the grayed-out box works there too.
    let _ = inputs;
    let time_wired = !snarl.in_pin(InPinId { node: node_id, input: 1 }).remotes.is_empty();

    let (mut hold, mut bounce, mut loopf) = flags;
    let mut timebase = timebase;
    let mut time_mul = time_mul;
    let mut sustain  = sustain;
    let mut gx_f     = grid_x as f64;
    let mut gy_f     = grid_y as f64;
    let mut snap_on  = snap;
    let mut show_grid = show_grid;
    let mut show_labels = show_grid_labels;
    let mut changed  = false;
    let mut sustain_changed = false;

    let mut curve_rect:      Option<egui::Rect> = None;
    let mut time_rect:       Option<egui::Rect> = None;
    let mut mode_rect:       Option<egui::Rect> = None;
    let mut sustain_rect:    Option<egui::Rect> = None;
    let mut grid_rect:       Option<egui::Rect> = None;
    let mut grid_opts_rect:  Option<egui::Rect> = None;

    let pts_x: Vec<f32> = snarl.get_node(node_id)
        .and_then(|n| n.params.get("points").and_then(|v| v.as_array()).map(|arr| {
            arr.iter().filter_map(|p| {
                let a = p.as_array()?;
                Some(a.get(0)?.as_f64()? as f32)
            }).collect()
        }))
        .unwrap_or_default();

    ui.vertical(|ui| {
        // Curve graph (resizable)
        egui::Resize::default()
            .id_salt(("env_crv", node_id))
            .default_size([180.0, 150.0])
            .min_size([80.0, 60.0])
            .show(ui, |ui| {
                let (rect, bg_resp) = ui.allocate_exact_size(ui.available_size(), egui::Sense::click());
                curve_rect = Some(rect);
                paint_envelope_curve_graph(node_id, ui, snarl, rect, bg_resp.clone(), None);
                bg_resp.context_menu(|ui| {
                    curve_context_menu(ui, node_id, snarl, None);
                });
            });

        // The painter auto-snaps sustain to the nearest control point and may
        // have rewritten the param this frame; re-read so the slider thumb tracks
        // the orange line exactly.
        if let Some(s) = snarl.get_node(node_id)
            .and_then(|n| n.params.get("sustain").and_then(|v| v.as_f64()))
        { sustain = s as f32; }

        // Sustain row — directly under the graph. Snaps to the nearest control
        // point; relevant only when Hold is enabled.
        let r = ui.horizontal(|ui| {
            ui.label(egui::RichText::new("Sustain").small().weak());
            let sustain_active = hold;
            let slider = egui::Slider::new(&mut sustain, 0.0..=1.0)
                .show_value(false)
                .clamping(egui::SliderClamping::Always);
            let sr = ui.add_enabled(sustain_active, slider);
            if sr.changed() {
                if !pts_x.is_empty() {
                    sustain = pts_x.iter().copied()
                        .min_by(|a, b| (a - sustain).abs().partial_cmp(&(b - sustain).abs()).unwrap())
                        .unwrap_or(sustain);
                }
                sustain_changed = true;
            }
            ui.label(egui::RichText::new(format!("{:.0}%", sustain * 100.0)).small().weak());
        });
        sustain_rect = Some(r.response.rect);

        // Time row: timebase selector + time_mul drag; changing unit auto-converts value
        let r = ui.horizontal(|ui| {
            let old_tb = timebase.clone();
            let tb_r_ms = ui.selectable_value(&mut timebase, "ms".into(), egui::RichText::new("ms").small());
            let tb_r_s  = ui.selectable_value(&mut timebase, "s".into(),  egui::RichText::new("s").small());
            let tb_r_hz = ui.selectable_value(&mut timebase, "hz".into(), egui::RichText::new("Hz").small());
            if tb_r_ms.changed() || tb_r_s.changed() || tb_r_hz.changed() {
                let period_ms: f32 = match old_tb.as_str() {
                    "s"  => time_mul * 1000.0,
                    "hz" => if time_mul > 0.0 { 1000.0 / time_mul } else { 1000.0 },
                    _    => time_mul,
                };
                time_mul = match timebase.as_str() {
                    "s"  => (period_ms / 1000.0).clamp(0.001, 2000.0),
                    "hz" => (1000.0 / period_ms.max(0.001)).clamp(0.001, 2000.0),
                    _    => period_ms.clamp(0.001, 2000.0),
                };
                changed = true;
            }
            let (lo, hi, spd) = match timebase.as_str() {
                "s"  => (0.001f32, 2000.0, 0.001),
                "hz" => (0.001,    2000.0, 0.001),
                _    => (0.001,    2000.0, 1.0),
            };
            // When Time is wired the box is disabled and shows the live applied
            // value (last_signals[3]); otherwise it edits the manual time_mul.
            let live_time = snarl.get_node(node_id)
                .and_then(|n| n.extra.last_signals.get(3))
                .and_then(|s| if let Some(Signal::Float(f)) = s { Some(*f) } else { None });
            let mut shown = if time_wired { live_time.unwrap_or(time_mul) } else { time_mul };
            let resp = ui.add_enabled(!time_wired,
                egui::DragValue::new(&mut shown).speed(spd).range(lo..=hi).max_decimals(3));
            if !time_wired { time_mul = shown; changed |= resp.changed(); }
        });
        time_rect = Some(r.response.rect);

        // Behavior row: combinable Hold / Bounce / Loop checkboxes.
        // Off = one-shot.
        let r = ui.horizontal(|ui| {
            let b0 = hold;
            ui.checkbox(&mut hold, egui::RichText::new("Hold").small())
                .on_hover_text("Run to the sustain point and hold there while the trigger is held");
            changed |= hold != b0;
            let b1 = bounce;
            ui.checkbox(&mut bounce, egui::RichText::new("Bounce").small())
                .on_hover_text("Ping-pong: forward while held, reverse back to the start on release");
            changed |= bounce != b1;
            let b2 = loopf;
            ui.checkbox(&mut loopf, egui::RichText::new("Loop").small())
                .on_hover_text("Repeat the active segment while the trigger is held");
            changed |= loopf != b2;
        });
        mode_rect = Some(r.response.rect);

        // Grid row: H / V divisions + Snap
        let r = ui.horizontal(|ui| {
            ui.label(egui::RichText::new("Grid").small().weak());
            changed |= ui.add(egui::DragValue::new(&mut gx_f).speed(0.25)
                .range(1.0..=20.0).max_decimals(0).prefix("H ")).changed();
            changed |= ui.add(egui::DragValue::new(&mut gy_f).speed(0.25)
                .range(1.0..=20.0).max_decimals(0).prefix("V ")).changed();
            ui.separator();
            let snap_before = snap_on;
            ui.checkbox(&mut snap_on, egui::RichText::new("Snap").small())
                .on_hover_text("Snap dragged points to grid intersections");
            changed |= snap_on != snap_before;
        });
        grid_rect = Some(r.response.rect);

        // Grid options row: show grid + labels
        let r = ui.horizontal(|ui| {
            let sg_before = show_grid;
            ui.checkbox(&mut show_grid, egui::RichText::new("Grid").small())
                .on_hover_text("Show grid lines");
            changed |= show_grid != sg_before;
            let sl_before = show_labels;
            ui.checkbox(&mut show_labels, egui::RichText::new("Labels").small())
                .on_hover_text("Show time (X) and value (Y) labels on grid lines");
            changed |= show_labels != sl_before;
        });
        grid_opts_rect = Some(r.response.rect);
    });

    if changed || sustain_changed {
        if let Some(node) = snarl.get_node_mut(node_id) {
            node.params.insert("hold".into(),     Value::Bool(hold));
            node.params.insert("bounce".into(),   Value::Bool(bounce));
            node.params.insert("loop".into(),     Value::Bool(loopf));
            node.params.remove("mode"); // superseded by the flags above
            node.params.insert("timebase".into(), Value::String(timebase));
            if let Some(n) = Number::from_f64(time_mul as f64) {
                node.params.insert("time_mul".into(), Value::Number(n));
            }
            // Sustain is written only on an explicit slider change; otherwise the
            // painter owns it (auto-snap to nearest dot), so leave it untouched
            // here to avoid reverting that snap on unrelated edits.
            if sustain_changed {
                if let Some(n) = Number::from_f64(sustain as f64) {
                    node.params.insert("sustain".into(), Value::Number(n));
                }
            }
            node.params.insert("grid_x".into(), serde_json::json!(gx_f as i64));
            node.params.insert("grid_y".into(), serde_json::json!(gy_f as i64));
            node.params.insert("snap".into(),   Value::Bool(snap_on));
            node.params.insert("show_grid".into(),        Value::Bool(show_grid));
            node.params.insert("show_grid_labels".into(), Value::Bool(show_labels));
        }
    }

    if let Some(r) = curve_rect      { register_exposable_element(ui, node_id, "curve",            r); }
    if let Some(r) = time_rect       { register_exposable_element(ui, node_id, "time_row",         r); }
    if let Some(r) = mode_rect       { register_exposable_element(ui, node_id, "mode_row",         r); }
    if let Some(r) = sustain_rect    { register_exposable_element(ui, node_id, "sustain_row",      r); }
    if let Some(r) = grid_rect       { register_exposable_element(ui, node_id, "grid_row",         r); }
    if let Some(r) = grid_opts_rect  { register_exposable_element(ui, node_id, "grid_options_row", r); }
}

// ── Envelope layout-pinned widget renderers ───────────────────────────────────

pub(crate) fn render_envelope_curve_only(
    inner_id: NodeId,
    ui: &mut egui::Ui,
    inner_snarl: &mut Snarl<NodeData>,
    container: egui::Vec2,
    graph_ov: Option<&crate::canvas::node::PinGraphOverride>,
) {
    let avail = egui::vec2(container.x.max(20.0), container.y.max(20.0));
    let (rect, bg_resp) = ui.allocate_exact_size(avail, egui::Sense::click());
    let bg_for_menu = bg_resp.clone();
    paint_envelope_curve_graph(inner_id, ui, inner_snarl, rect, bg_resp, graph_ov);
    bg_for_menu.context_menu(|ui| {
        curve_context_menu(ui, inner_id, inner_snarl, None);
    });
}

pub(crate) fn render_envelope_time_row(
    inner_id: NodeId,
    ui: &mut egui::Ui,
    snarl: &mut Snarl<NodeData>,
    container: egui::Vec2,
) {
    ui.set_max_width(container.x);
    apply_widget_scale(ui, container, egui::vec2(180.0, 22.0));

    let (timebase, time_mul) = snarl.get_node(inner_id).map(|n| {
        let tb = n.params.get("timebase").and_then(|v| v.as_str()).unwrap_or("ms").to_string();
        let tm = n.params.get("time_mul").and_then(|v| v.as_f64()).unwrap_or(500.0) as f32;
        (tb, tm)
    }).unwrap_or_else(|| ("ms".into(), 500.0));

    // Time input wired? Then the box is disabled and shows the live value.
    let time_wired = !snarl.in_pin(InPinId { node: inner_id, input: 1 }).remotes.is_empty();
    let live_time = snarl.get_node(inner_id)
        .and_then(|n| n.extra.last_signals.get(3))
        .and_then(|s| if let Some(Signal::Float(f)) = s { Some(*f) } else { None });

    let mut timebase = timebase;
    let mut time_mul = time_mul;
    let mut changed  = false;

    let mut fr: Vec<egui::Rect> = Vec::with_capacity(4);
    ui.horizontal(|ui| {
        let old_tb = timebase.clone();
        let r_ms = ui.selectable_value(&mut timebase, "ms".into(), egui::RichText::new("ms").small());
        fr.push(r_ms.rect);
        let r_s  = ui.selectable_value(&mut timebase, "s".into(),  egui::RichText::new("s").small());
        fr.push(r_s.rect);
        let r_hz = ui.selectable_value(&mut timebase, "hz".into(), egui::RichText::new("Hz").small());
        fr.push(r_hz.rect);
        if r_ms.changed() || r_s.changed() || r_hz.changed() {
            let period_ms: f32 = match old_tb.as_str() {
                "s"  => time_mul * 1000.0,
                "hz" => if time_mul > 0.0 { 1000.0 / time_mul } else { 1000.0 },
                _    => time_mul,
            };
            time_mul = match timebase.as_str() {
                "s"  => (period_ms / 1000.0).clamp(0.001, 2000.0),
                "hz" => (1000.0 / period_ms.max(0.001)).clamp(0.001, 2000.0),
                _    => period_ms.clamp(0.001, 2000.0),
            };
            changed = true;
        }
        let (lo, hi, spd) = match timebase.as_str() {
            "s"  => (0.001f32, 2000.0, 0.001),
            "hz" => (0.001,    2000.0, 0.001),
            _    => (0.001,    2000.0, 1.0),
        };
        let mut shown = if time_wired { live_time.unwrap_or(time_mul) } else { time_mul };
        let r = ui.add_enabled(!time_wired,
            egui::DragValue::new(&mut shown).speed(spd).range(lo..=hi).max_decimals(3));
        fr.push(r.rect);
        if !time_wired { time_mul = shown; changed |= r.changed(); }
    });
    publish_nav_field_rects(ui, inner_id, &fr);
    if changed {
        if let Some(node) = snarl.get_node_mut(inner_id) {
            node.params.insert("timebase".into(), Value::String(timebase));
            if let Some(n) = Number::from_f64(time_mul as f64) {
                node.params.insert("time_mul".into(), Value::Number(n));
            }
        }
    }
}

pub(crate) fn render_envelope_mode_row(
    inner_id: NodeId,
    ui: &mut egui::Ui,
    snarl: &mut Snarl<NodeData>,
    container: egui::Vec2,
) {
    ui.set_max_width(container.x);
    apply_widget_scale(ui, container, egui::vec2(180.0, 22.0));

    let (mut hold, mut bounce, mut loopf) = snarl.get_node(inner_id)
        .map(|n| flexinput_engine::envelope_flags(&n.params))
        .unwrap_or((false, false, false));

    let mut changed = false;
    let mut fr: Vec<egui::Rect> = Vec::with_capacity(3);
    ui.horizontal(|ui| {
        let b0 = hold;
        let r = ui.checkbox(&mut hold, egui::RichText::new("Hold").small());
        fr.push(r.rect); changed |= hold != b0;
        let b1 = bounce;
        let r = ui.checkbox(&mut bounce, egui::RichText::new("Bounce").small());
        fr.push(r.rect); changed |= bounce != b1;
        let b2 = loopf;
        let r = ui.checkbox(&mut loopf, egui::RichText::new("Loop").small());
        fr.push(r.rect); changed |= loopf != b2;
    });
    publish_nav_field_rects(ui, inner_id, &fr);
    if changed {
        if let Some(node) = snarl.get_node_mut(inner_id) {
            node.params.insert("hold".into(),   Value::Bool(hold));
            node.params.insert("bounce".into(), Value::Bool(bounce));
            node.params.insert("loop".into(),   Value::Bool(loopf));
            node.params.remove("mode");
        }
    }
}

pub(crate) fn render_envelope_sustain_row(
    inner_id: NodeId,
    ui: &mut egui::Ui,
    snarl: &mut Snarl<NodeData>,
    container: egui::Vec2,
) {
    ui.set_max_width(container.x);
    apply_widget_scale(ui, container, egui::vec2(180.0, 22.0));

    let (hold, sustain) = snarl.get_node(inner_id).map(|n| {
        let (h, _, _) = flexinput_engine::envelope_flags(&n.params);
        let su = n.params.get("sustain").and_then(|v| v.as_f64()).unwrap_or(0.3) as f32;
        (h, su)
    }).unwrap_or((false, 0.3));

    let pts_x: Vec<f32> = snarl.get_node(inner_id)
        .and_then(|n| n.params.get("points").and_then(|v| v.as_array()).map(|arr| {
            arr.iter().filter_map(|p| {
                let a = p.as_array()?;
                Some(a.get(0)?.as_f64()? as f32)
            }).collect()
        }))
        .unwrap_or_default();

    let mut sustain = sustain;
    let mut changed = false;
    let sustain_active = hold;

    let mut fr: Vec<egui::Rect> = Vec::with_capacity(2);
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new("Sustain").small().weak());
        // Flexible element: the slider absorbs surplus container width.
        ui.spacing_mut().slider_width = pin_flex_width(ui, container, 70.0);
        let slider = egui::Slider::new(&mut sustain, 0.0..=1.0)
            .show_value(false)
            .clamping(egui::SliderClamping::Always);
        let r = ui.add_enabled(sustain_active, slider);
        fr.push(r.rect);
        if r.changed() {
            if !pts_x.is_empty() {
                sustain = pts_x.iter().copied()
                    .min_by(|a, b| (a - sustain).abs().partial_cmp(&(b - sustain).abs()).unwrap())
                    .unwrap_or(sustain);
            }
            changed = true;
        }
        let pct_lbl = format!("{:.0}%", sustain * 100.0);
        let r = ui.label(egui::RichText::new(pct_lbl).small().weak());
        fr.push(r.rect);
    });
    publish_nav_field_rects(ui, inner_id, &fr);
    if changed {
        if let Some(node) = snarl.get_node_mut(inner_id) {
            if let Some(n) = Number::from_f64(sustain as f64) {
                node.params.insert("sustain".into(), Value::Number(n));
            }
        }
    }
}

pub(crate) fn render_envelope_grid_row(
    inner_id: NodeId,
    ui: &mut egui::Ui,
    snarl: &mut Snarl<NodeData>,
    container: egui::Vec2,
) {
    ui.set_max_width(container.x);
    apply_widget_scale(ui, container, egui::vec2(180.0, 22.0));

    let (grid_x, grid_y, snap) = snarl.get_node(inner_id).map(|n| {
        let gx = n.params.get("grid_x").and_then(|v| v.as_i64()).unwrap_or(4).max(1);
        let gy = n.params.get("grid_y").and_then(|v| v.as_i64()).unwrap_or(4).max(1);
        let sn = n.params.get("snap").and_then(|v| v.as_bool()).unwrap_or(false);
        (gx, gy, sn)
    }).unwrap_or((4, 4, false));

    let mut gx_f    = grid_x as f64;
    let mut gy_f    = grid_y as f64;
    let mut snap_on = snap;
    let mut changed = false;

    let mut fr: Vec<egui::Rect> = Vec::with_capacity(3);
    ui.horizontal(|ui| {
        let r = ui.label(egui::RichText::new("Grid").small().weak());
        fr.push(r.rect);
        let r = ui.add(egui::DragValue::new(&mut gx_f).speed(0.25)
            .range(1.0..=20.0).max_decimals(0).prefix("H "));
        fr.push(r.rect); changed |= r.changed();
        let r = ui.add(egui::DragValue::new(&mut gy_f).speed(0.25)
            .range(1.0..=20.0).max_decimals(0).prefix("V "));
        fr.push(r.rect); changed |= r.changed();
        let snap_before = snap_on;
        let r = ui.checkbox(&mut snap_on, egui::RichText::new("Snap").small());
        fr.push(r.rect);
        changed |= snap_on != snap_before;
    });
    publish_nav_field_rects(ui, inner_id, &fr);
    if changed {
        if let Some(node) = snarl.get_node_mut(inner_id) {
            node.params.insert("grid_x".into(), serde_json::json!(gx_f as i64));
            node.params.insert("grid_y".into(), serde_json::json!(gy_f as i64));
            node.params.insert("snap".into(),   Value::Bool(snap_on));
        }
    }
}

pub(crate) fn render_envelope_grid_options_row(
    inner_id: NodeId,
    ui: &mut egui::Ui,
    snarl: &mut Snarl<NodeData>,
    container: egui::Vec2,
) {
    ui.set_max_width(container.x);
    apply_widget_scale(ui, container, egui::vec2(180.0, 22.0));

    let (show_grid, show_labels) = snarl.get_node(inner_id).map(|n| {
        let sg  = n.params.get("show_grid").and_then(|v| v.as_bool()).unwrap_or(true);
        let sgl = n.params.get("show_grid_labels").and_then(|v| v.as_bool()).unwrap_or(false);
        (sg, sgl)
    }).unwrap_or((true, false));

    let mut show_grid   = show_grid;
    let mut show_labels = show_labels;
    let mut changed     = false;

    let mut fr: Vec<egui::Rect> = Vec::with_capacity(2);
    ui.horizontal(|ui| {
        let sg_before = show_grid;
        let r = ui.checkbox(&mut show_grid, egui::RichText::new("Grid").small());
        fr.push(r.rect);
        changed |= show_grid != sg_before;
        let sl_before = show_labels;
        let r = ui.checkbox(&mut show_labels, egui::RichText::new("Labels").small());
        fr.push(r.rect);
        changed |= show_labels != sl_before;
    });
    publish_nav_field_rects(ui, inner_id, &fr);
    if changed {
        if let Some(node) = snarl.get_node_mut(inner_id) {
            node.params.insert("show_grid".into(),        Value::Bool(show_grid));
            node.params.insert("show_grid_labels".into(), Value::Bool(show_labels));
        }
    }
}
