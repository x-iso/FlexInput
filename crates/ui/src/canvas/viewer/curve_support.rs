//! Shared response-curve machinery: the core graph painter, pinned row
//! renderers, .fxc save/load, curve clipboard + right-click menu, scale
//! helpers.

use super::*;

/// Renders the response curve graph as a bare widget filling `container`.
/// No surrounding sliders, buttons, channel +/-, etc. — just the graph,
/// fully interactive (drag points, alt-bias, dbl-click add, right-click remove).
/// Sized exactly to the user-allocated rect from the sub-patch layout.
pub(crate) fn render_response_curve_only(
    inner_id: NodeId,
    ui: &mut egui::Ui,
    inner_snarl: &mut Snarl<NodeData>,
    container: egui::Vec2,
    is_vec: bool,
    graph_ov: Option<&crate::canvas::node::PinGraphOverride>,
) {
    // No vsync bypass — same rationale as show_response_curve_body.
    let avail = egui::vec2(container.x.max(20.0), container.y.max(20.0));
    let (rect, bg_resp) = ui.allocate_exact_size(avail, egui::Sense::click());
    let bg_for_menu = bg_resp.clone();
    paint_response_curve_graph(inner_id, ui, inner_snarl, rect, bg_resp, is_vec, graph_ov);
    // Right-click context menu — same actions as the canvas-editor body so the
    // layout-pinned widget is fully usable on its own.
    let _ = is_vec; // graph-only menu doesn't distinguish; kept for signature compat.
    bg_for_menu.context_menu(|ui| {
        curve_context_menu(ui, inner_id, inner_snarl, None);
    });
}

/// Bare Log-Exp slider + Abs (only for non-vec) + Snap.
pub(crate) fn render_response_curve_scale_row(
    inner_id: NodeId,
    ui: &mut egui::Ui,
    snarl: &mut Snarl<NodeData>,
    container: egui::Vec2,
    is_vec: bool,
) {
    let (mut sc_t, mut absolute, mut snap_on) = snarl.get_node(inner_id).map(|n| {
        let s = n.params.get("scale_t").and_then(|v| v.as_f64()).unwrap_or(0.0) as f32;
        let a = if is_vec { true } else { n.params.get("absolute").and_then(|v| v.as_bool()).unwrap_or(true) };
        let sn = n.params.get("snap").and_then(|v| v.as_bool()).unwrap_or(false);
        (s, a, sn)
    }).unwrap_or((0.0, true, false));

    ui.set_max_width(container.x);
    let s = apply_widget_scale(ui, container, egui::vec2(220.0, 22.0));
    let mut changed = false;
    let mut fr: Vec<egui::Rect> = Vec::with_capacity(3);
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new("Log").weak());
        // ASTH row model: the slider is the row's flexible element — it takes
        // its minimum scaled width plus ALL surplus container width, so the
        // labels/checkboxes scale with the frame height while widening the
        // frame lengthens the slider.
        let slider_w = pin_flex_width(ui, container, 60.0);
        let slider_h = (16.0 * s).max(10.0);
        let (slider_rect, slider_resp) =
            ui.allocate_exact_size(egui::vec2(slider_w, slider_h), egui::Sense::click_and_drag());
        fr.push(slider_rect);
        if slider_resp.double_clicked() { sc_t = 0.0; changed = true; }
        else if slider_resp.dragged() {
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
            egui::pos2(knob_x, slider_rect.center().y),
            (slider_rect.height() * 0.35).max(3.0),
            if slider_resp.hovered() || slider_resp.dragged() { Color32::WHITE } else { Color32::from_gray(190) },
        );
        ui.label(egui::RichText::new("Exp").weak());
        ui.separator();
        if !is_vec {
            let was = absolute;
            let r = ui.checkbox(&mut absolute, egui::RichText::new("Abs"));
            fr.push(r.rect);
            changed |= absolute != was;
        }
        let was = snap_on;
        let r = ui.checkbox(&mut snap_on, egui::RichText::new("Snap"));
        fr.push(r.rect);
        changed |= snap_on != was;
    });
    publish_nav_field_rects(ui, inner_id, &fr);
    if changed {
        if let Some(node) = snarl.get_node_mut(inner_id) {
            if let Some(n) = Number::from_f64(sc_t as f64) { node.params.insert("scale_t".into(), Value::Number(n)); }
            if !is_vec { node.params.insert("absolute".into(), Value::Bool(absolute)); }
            node.params.insert("snap".into(), Value::Bool(snap_on));
        }
    }
}

/// Bare In/Out range row. For non-vec curves: in_min, in_max, out_min, out_max.
/// For vec curves: in_max, out_max (vec curves are always [0,1]).
pub(crate) fn render_response_curve_range_row(
    inner_id: NodeId,
    ui: &mut egui::Ui,
    snarl: &mut Snarl<NodeData>,
    container: egui::Vec2,
    is_vec: bool,
) {
    ui.set_max_width(container.x);
    apply_widget_scale(ui, container, egui::vec2(220.0, 22.0));
    let mut changed = false;
    if is_vec {
        let (mut i_max, mut o_max) = snarl.get_node(inner_id).map(|n| {
            let i = n.params.get("in_max").and_then(|v| v.as_f64()).unwrap_or(1.0) as f32;
            let o = n.params.get("out_max").and_then(|v| v.as_f64()).unwrap_or(1.0) as f32;
            (i, o)
        }).unwrap_or((1.0, 1.0));
        let mut fr = [egui::Rect::NOTHING; 2];
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("In max").weak());
            let r = ui.add(egui::DragValue::new(&mut i_max).speed(0.01).max_decimals(2));
            fr[0] = r.rect; changed |= r.changed();
            ui.separator();
            ui.label(egui::RichText::new("Out max").weak());
            let r = ui.add(egui::DragValue::new(&mut o_max).speed(0.01).max_decimals(2));
            fr[1] = r.rect; changed |= r.changed();
        });
        publish_nav_field_rects(ui, inner_id, &fr);
        if changed {
            if let Some(node) = snarl.get_node_mut(inner_id) {
                if let Some(n) = Number::from_f64(i_max as f64) { node.params.insert("in_max".into(),  Value::Number(n)); }
                if let Some(n) = Number::from_f64(o_max as f64) { node.params.insert("out_max".into(), Value::Number(n)); }
            }
        }
    } else {
        let (mut i0, mut i1, mut o0, mut o1) = snarl.get_node(inner_id).map(|n| {
            let i0 = n.params.get("in_min").and_then(|v| v.as_f64()).unwrap_or(-1.0) as f32;
            let i1 = n.params.get("in_max").and_then(|v| v.as_f64()).unwrap_or( 1.0) as f32;
            let o0 = n.params.get("out_min").and_then(|v| v.as_f64()).unwrap_or(-1.0) as f32;
            let o1 = n.params.get("out_max").and_then(|v| v.as_f64()).unwrap_or( 1.0) as f32;
            (i0, i1, o0, o1)
        }).unwrap_or((-1.0, 1.0, -1.0, 1.0));
        let mut fr = [egui::Rect::NOTHING; 4];
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("In").weak());
            let r = ui.add(egui::DragValue::new(&mut i0).speed(0.01).prefix("↓").max_decimals(2));
            fr[0] = r.rect; changed |= r.changed();
            let r = ui.add(egui::DragValue::new(&mut i1).speed(0.01).prefix("↑").max_decimals(2));
            fr[1] = r.rect; changed |= r.changed();
            ui.separator();
            ui.label(egui::RichText::new("Out").weak());
            let r = ui.add(egui::DragValue::new(&mut o0).speed(0.01).prefix("↓").max_decimals(2));
            fr[2] = r.rect; changed |= r.changed();
            let r = ui.add(egui::DragValue::new(&mut o1).speed(0.01).prefix("↑").max_decimals(2));
            fr[3] = r.rect; changed |= r.changed();
        });
        publish_nav_field_rects(ui, inner_id, &fr);
        if changed {
            if let Some(node) = snarl.get_node_mut(inner_id) {
                for (k, v) in [
                    ("in_min", i0 as f64), ("in_max", i1 as f64),
                    ("out_min", o0 as f64), ("out_max", o1 as f64),
                ] {
                    if let Some(n) = Number::from_f64(v) { node.params.insert(k.into(), Value::Number(n)); }
                }
            }
        }
    }
}

/// Bare Grid + Trail row.
pub(crate) fn render_response_curve_grid_row(
    inner_id: NodeId,
    ui: &mut egui::Ui,
    snarl: &mut Snarl<NodeData>,
    container: egui::Vec2,
) {
    let (mut gx, mut gy, mut tm) = snarl.get_node(inner_id).map(|n| {
        let gx = n.params.get("grid_x").and_then(|v| v.as_i64()).unwrap_or(4) as f64;
        let gy = n.params.get("grid_y").and_then(|v| v.as_i64()).unwrap_or(4) as f64;
        let tm = n.params.get("trail_ms").and_then(|v| v.as_i64()).unwrap_or(300);
        (gx, gy, tm)
    }).unwrap_or((4.0, 4.0, 300));
    ui.set_max_width(container.x);
    apply_widget_scale(ui, container, egui::vec2(220.0, 22.0));
    let mut changed = false;
    let mut field_rects = [egui::Rect::NOTHING; 3];
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new("Grid").weak());
        let rh = ui.add(egui::DragValue::new(&mut gx).speed(0.25)
            .range(1.0..=20.0).max_decimals(0).prefix("H "));
        field_rects[0] = rh.rect; changed |= rh.changed();
        let rv = ui.add(egui::DragValue::new(&mut gy).speed(0.25)
            .range(1.0..=20.0).max_decimals(0).prefix("V "));
        field_rects[1] = rv.rect; changed |= rv.changed();
        ui.separator();
        ui.label(egui::RichText::new("Trail").weak());
        let rt = ui.add(egui::DragValue::new(&mut tm).speed(5.0)
            .range(0i64..=1000).suffix("ms"));
        field_rects[2] = rt.rect; changed |= rt.changed();
    });
    publish_nav_field_rects(ui, inner_id, &field_rects);
    if changed {
        if let Some(node) = snarl.get_node_mut(inner_id) {
            node.params.insert("grid_x".into(),   serde_json::json!(gx as i64));
            node.params.insert("grid_y".into(),   serde_json::json!(gy as i64));
            node.params.insert("trail_ms".into(), serde_json::json!(tm));
        }
    }
}

/// Bare Scale grid + Labels checkboxes row.
pub(crate) fn render_response_curve_grid_options_row(
    inner_id: NodeId,
    ui: &mut egui::Ui,
    snarl: &mut Snarl<NodeData>,
    container: egui::Vec2,
) {
    let (mut ssg, mut sgl) = snarl.get_node(inner_id).map(|n| {
        let ssg = n.params.get("show_scaled_grid").and_then(|v| v.as_bool()).unwrap_or(false);
        let sgl = n.params.get("show_grid_labels").and_then(|v| v.as_bool()).unwrap_or(false);
        (ssg, sgl)
    }).unwrap_or((false, false));
    ui.set_max_width(container.x);
    apply_widget_scale(ui, container, egui::vec2(220.0, 22.0));
    let mut changed = false;
    let mut fr = [egui::Rect::NOTHING; 2];
    ui.horizontal(|ui| {
        let was = ssg;
        fr[0] = ui.checkbox(&mut ssg, egui::RichText::new("Scale grid"))
            .on_hover_text("Adapt grid lines to the current Log/Exp scaling").rect;
        changed |= ssg != was;
        let was = sgl;
        fr[1] = ui.checkbox(&mut sgl, egui::RichText::new("Labels"))
            .on_hover_text("Show value labels on grid lines").rect;
        changed |= sgl != was;
    });
    publish_nav_field_rects(ui, inner_id, &fr);
    if changed {
        if let Some(node) = snarl.get_node_mut(inner_id) {
            node.params.insert("show_scaled_grid".into(), Value::Bool(ssg));
            node.params.insert("show_grid_labels".into(), Value::Bool(sgl));
        }
    }
}

/// Paints just the response-curve graph (background + grid + curve + control
/// points + bias handles + live-input trails) into `rect`, and writes back any
/// param changes made via interaction. Shared between the in-editor body
/// renderer and the bare layout-pinned renderer on the sub-patch face.
pub(crate) fn paint_response_curve_graph(
    node_id: NodeId,
    ui: &mut egui::Ui,
    snarl: &mut Snarl<NodeData>,
    rect: egui::Rect,
    bg_resp: egui::Response,
    is_vec: bool,
    graph_ov: Option<&crate::canvas::node::PinGraphOverride>,
) {
    // Initialise params on first use (kept consistent with the body renderers).
    let needs_init = snarl.get_node(node_id).map(|n| !n.params.contains_key("points")).unwrap_or(false);
    if needs_init {
        if let Some(node) = snarl.get_node_mut(node_id) {
            node.params.insert("points".into(), serde_json::json!([[0.0, 0.0], [1.0, 1.0]]));
            node.params.insert("biases".into(), serde_json::json!([0.0]));
            if !is_vec {
                node.params.insert("absolute".into(), Value::Bool(true));
                node.params.insert("in_min".into(),   serde_json::json!(-1.0));
                node.params.insert("out_min".into(),  serde_json::json!(-1.0));
            }
            node.params.insert("in_max".into(),   serde_json::json!(1.0f64));
            node.params.insert("out_max".into(),  serde_json::json!(1.0f64));
            node.params.insert("grid_x".into(),   serde_json::json!(4i64));
            node.params.insert("grid_y".into(),   serde_json::json!(4i64));
            node.params.insert("snap".into(),     Value::Bool(false));
            node.params.insert("scale_t".into(),  serde_json::json!(0.0f64));
            node.params.insert("trail_ms".into(), serde_json::json!(300i64));
        }
    }

    // Read params.
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
            let abs  = if is_vec { true } else { n.params.get("absolute").and_then(|v| v.as_bool()).unwrap_or(true) };
            let i0   = if is_vec { 0.0 } else { n.params.get("in_min") .and_then(|v| v.as_f64()).unwrap_or(-1.0) as f32 };
            let i1   = n.params.get("in_max").and_then(|v| v.as_f64()).unwrap_or(1.0) as f32;
            let o0   = if is_vec { 0.0 } else { n.params.get("out_min").and_then(|v| v.as_f64()).unwrap_or(-1.0) as f32 };
            let o1   = n.params.get("out_max").and_then(|v| v.as_f64()).unwrap_or(1.0) as f32;
            let gx   = n.params.get("grid_x").and_then(|v| v.as_i64()).unwrap_or(4).max(1) as usize;
            let gy   = n.params.get("grid_y").and_then(|v| v.as_i64()).unwrap_or(4).max(1) as usize;
            let sn   = n.params.get("snap").and_then(|v| v.as_bool()).unwrap_or(false);
            let sc   = n.params.get("scale_t").and_then(|v| v.as_f64()).map(|f| f as f32).unwrap_or(0.0);
            let tm   = n.params.get("trail_ms").and_then(|v| v.as_i64()).unwrap_or(300).clamp(0, 1000);
            let ssg  = n.params.get("show_scaled_grid").and_then(|v| v.as_bool()).unwrap_or(false);
            let sgl  = n.params.get("show_grid_labels").and_then(|v| v.as_bool()).unwrap_or(false);
            (pts, bss, abs, i0, i1, o0, o1, gx, gy, sn, sc, tm, ssg, sgl)
        })
        .unwrap_or_else(|| (vec![[0.0, 0.0], [1.0, 1.0]], vec![], true, -1.0, 1.0, -1.0, 1.0, 4, 4, false, 0.0f32, 300, false, false));

    let n_channels = snarl.get_node(node_id)
        .map(|n| n.inputs.len().min(n.outputs.len()))
        .unwrap_or(1).max(1);
    let live_inputs: Vec<Option<f32>> = (0..n_channels)
        .map(|ch| snarl.get_node(node_id)
            .and_then(|n| n.extra.last_signals.get(ch)?.as_ref())
            .map(sig_f32))
        .collect();

    let (x_lo, x_hi): (f32, f32) = if absolute { (0.0, 1.0) } else { (-1.0, 1.0) };
    let (y_lo, y_hi): (f32, f32) = if absolute { (0.0, 1.0) } else { (-1.0, 1.0) };
    let x_range = x_hi - x_lo;
    let y_range = y_hi - y_lo;

    let mut new_points  = points.clone();
    let mut new_biases  = biases.clone();
    let mut pts_changed  = false;
    let mut bias_changed = false;

    let painter = ui.painter_at(rect);

    let c2s = |x: f32, y: f32| egui::pos2(
        rect.left() + (x - x_lo) / x_range * rect.width(),
        rect.bottom() - (y - y_lo) / y_range * rect.height(),
    );
    let s2c = |pos: egui::Pos2| -> [f32; 2] {[
        x_lo + (pos.x - rect.left()) / rect.width() * x_range,
        y_lo + (rect.bottom() - pos.y) / rect.height() * y_range,
    ]};
    // Shared grid-position builder — same logic as the body renderers.
    let redist = |mut nodes: Vec<f32>, n: usize| -> Vec<f32> {
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
            nodes.insert(li+1, (nodes[li]+nodes[li+1])*0.5);
        }
        nodes.sort_by(|a, b| a.partial_cmp(b).unwrap());
        nodes
    };
    let build_grid_nodes = |n: usize| -> Vec<f32> {
        if n == 0 { return vec![0.0, 1.0]; }
        if !show_scaled_grid {
            return (0..=n).map(|i| i as f32 / n as f32).collect();
        }
        if absolute || is_vec {
            let nodes = (0..=n).map(|i| {
                let t = i as f32 / n as f32;
                1.0 - curve_scale_inv(1.0 - t, scale_t)
            }).collect();
            redist(nodes, n)
        } else {
            // Bidirectional: both halves expand outward from centre.
            // t=0 is centre, t=1 is edge; same scale formula as abs case.
            let half_lo = n / 2;
            let half_hi = n - half_lo;
            let lo: Vec<f32> = (0..=half_lo).map(|i| {
                let t = i as f32 / half_lo as f32;
                let s = 1.0 - curve_scale_inv(1.0 - t, scale_t);
                0.5 - s * 0.5
            }).collect();
            let hi: Vec<f32> = (0..=half_hi).map(|i| {
                let t = i as f32 / half_hi as f32;
                let s = 1.0 - curve_scale_inv(1.0 - t, scale_t);
                0.5 + s * 0.5
            }).collect();
            let mut merged = redist(lo, half_lo);
            for v in redist(hi, half_hi).iter().skip(1) { merged.push(*v); }
            merged.sort_by(|a, b| a.partial_cmp(b).unwrap());
            merged
        }
    };
    let snap_nodes_x = build_grid_nodes(grid_x);
    let snap_nodes_y = build_grid_nodes(grid_y);
    let grid_x_positions: Vec<f32> = (1..grid_x).map(|i| x_lo + snap_nodes_x[i] * x_range).collect();
    let grid_y_positions: Vec<f32> = (1..grid_y).map(|i| y_lo + snap_nodes_y[i] * y_range).collect();

    let do_snap = |x: f32, y: f32| -> (f32, f32) {
        if !snap { return (x, y); }
        let u = ((x - x_lo) / x_range).clamp(0.0, 1.0);
        let v = ((y - y_lo) / y_range).clamp(0.0, 1.0);
        let su = snap_nodes_x.iter().copied()
            .min_by(|a, b| (a-u).abs().partial_cmp(&(b-u).abs()).unwrap()).unwrap_or(u);
        let sv = snap_nodes_y.iter().copied()
            .min_by(|a, b| (a-v).abs().partial_cmp(&(b-v).abs()).unwrap()).unwrap_or(v);
        (x_lo + su * x_range, y_lo + sv * y_range)
    };

    let (graph_bg, graph_outline) = graph_chrome(graph_ov);
    painter.rect_filled(rect, 2.0, graph_bg);

    let (grid_faint, grid_axis) = graph_grid_colors(graph_ov);
    let gs = egui::Stroke::new(0.5, grid_faint);
    for &x in &grid_x_positions { painter.line_segment([c2s(x, y_lo), c2s(x, y_hi)], gs); }
    for &y in &grid_y_positions { painter.line_segment([c2s(x_lo, y), c2s(x_hi, y)], gs); }
    painter.line_segment([c2s(x_lo, y_lo), c2s(x_hi, y_hi)],
        egui::Stroke::new(0.5, grid_axis));

    if show_grid_labels {
        const MIN_LABEL_PX: f32 = 20.0;
        let label_col = Color32::from_rgba_unmultiplied(180, 180, 180, 160);
        let font = egui::FontId::proportional(9.0);
        let abs_max_in  = in_max.abs().max(in_min.abs());
        let abs_max_out = out_max.abs().max(out_min.abs());
        // real_in(u): u∈[0,1] graph pos → actual input value.
        // Graph x = curve_scale(|real|/abs_max), so real = curve_scale_inv(u)*abs_max.
        // Bipolar: centre u=0.5 is value 0; each half scales outward.
        let real_in = |u: f32| -> f32 {
            if absolute || is_vec {
                curve_scale_inv(u, scale_t) * abs_max_in
            } else {
                let c = u * 2.0 - 1.0; // [-1,1], 0 = centre
                let sign = if c < 0.0 { -1.0f32 } else { 1.0 };
                sign * curve_scale_inv(c.abs(), scale_t) * abs_max_in
            }
        };
        let real_out = |v: f32| -> f32 {
            if absolute || is_vec {
                curve_scale_inv(v, scale_t) * abs_max_out
            } else {
                let c = v * 2.0 - 1.0;
                let sign = if c < 0.0 { -1.0f32 } else { 1.0 };
                sign * curve_scale_inv(c.abs(), scale_t) * abs_max_out
            }
        };
        let mut last_sx = f32::NEG_INFINITY;
        for &x in &grid_x_positions {
            let sx = c2s(x, y_hi).x;
            if sx - last_sx < MIN_LABEL_PX { continue; }
            last_sx = sx;
            let u = (x - x_lo) / x_range;
            let val = real_in(u);
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
            let val = real_out(v);
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

    let bias_id_tag = if is_vec { "vbias_h_only" } else { "bias_h_only" };
    let pt_id_tag   = if is_vec { "vcpt_only" }    else { "cpt_only" };
    let trail_id_tag = if is_vec { "vtrail_only" } else { "trail_only" };

    // Bias handles show on mouse Alt OR gamepad bias mode (hold-North).
    let nav_bias = ui.ctx().data(|d|
        d.get_temp::<u64>(egui::Id::new(("gp_nav_curve_bias", node_id.0))))
        == Some(ui.ctx().cumulative_pass_nr());
    let alt_held = ui.input(|i| i.modifiers.alt) || nav_bias;
    if alt_held && new_points.len() >= 2 {
        while new_biases.len() < new_points.len() - 1 { new_biases.push(0.0); }
        for seg in 0..(new_points.len() - 1) {
            let mid_x = (new_points[seg][0] + new_points[seg + 1][0]) * 0.5;
            let mid_y = sample_curve(&new_points, mid_x, &new_biases).clamp(y_lo, y_hi);
            let hpos  = c2s(mid_x, mid_y);
            let hid   = ui.id().with((bias_id_tag, node_id, seg));
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
            painter.circle_stroke(hpos, 4.0, egui::Stroke::new(1.0, Color32::from_gray(100)));
        }
    }

    let mut remove_idx: Option<usize> = None;
    for i in 0..new_points.len() {
        let [px, py] = new_points[i];
        let screen   = c2s(px, py);
        let pt_id    = ui.id().with((pt_id_tag, node_id, i));
        let pt_resp  = ui.interact(
            egui::Rect::from_center_size(screen, egui::Vec2::splat(12.0)),
            pt_id, egui::Sense::click_and_drag());

        // Origin-anchored drag: stash the point's [x, y] at drag start and the
        // running pixel offset; each frame, target = origin + accumulated_px
        // mapped to curve coords, then snapped once. Without this, snapping
        // accumulates per-frame rounding and feels "drunk".
        let origin_id = ui.id().with(("crv_pt_origin", pt_id_tag, node_id, i));
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

        // Gamepad-nav: highlight the dot the driver selected. Driver publishes
        // (pass, selected_idx, editing_dot) under ("gp_nav_curve_sel", node).
        let sel: Option<(u64, usize, bool)> = ui.ctx().data(|d|
            d.get_temp(egui::Id::new(("gp_nav_curve_sel", node_id.0))));
        if let Some((pass, sel_i, editing_dot)) = sel {
            if pass == ui.ctx().cumulative_pass_nr() && sel_i == i {
                let accent = ui.visuals().selection.stroke.color;
                let [r8, g8, b8, _] = accent.to_array();
                for k in 0..5 {
                    let t = (k as f32 + 1.0) / 5.0;
                    let rr = (if editing_dot { 16.0 } else { 12.0 }) * t;
                    let a = ((if editing_dot { 170.0 } else { 120.0 }) * (1.0 - t)) as u8;
                    if a == 0 { continue; }
                    painter.circle_stroke(screen, rr,
                        egui::Stroke::new(2.0, Color32::from_rgba_unmultiplied(r8, g8, b8, a)));
                }
                painter.circle_filled(screen, if editing_dot { 6.0 } else { 5.0 }, accent);
                painter.circle_stroke(screen, if editing_dot { 6.0 } else { 5.0 },
                    egui::Stroke::new(1.5, Color32::WHITE));
            }
        }
    }

    // Gamepad-nav: publish curve geometry (graph rect + axis bounds) so the
    // driver can map graph↔screen for dot stepping, cursor hit-test, and moves.
    // Transform the rect to GLOBAL (screen) space — in Easy mode this body
    // renders on a scaled/scrolled sub-layer, so the raw rect is body-local and
    // would never match the screen-space gamepad cursor.
    {
        let pass = ui.ctx().cumulative_pass_nr();
        let to_global = ui.ctx().layer_transform_to_global(ui.layer_id())
            .unwrap_or(egui::emath::TSTransform::IDENTITY);
        let screen_rect = to_global * rect;
        ui.ctx().data_mut(|d| d.insert_temp(
            egui::Id::new(("gp_nav_curve_geom", node_id.0)),
            (pass, screen_rect, x_lo, x_hi, y_lo, y_hi)));
    }

    if bg_resp.double_clicked() {
        if let Some(pos) = bg_resp.interact_pointer_pos() {
            let [gx_raw, gy_raw] = s2c(pos);
            let (gx_sn, gy_sn)   = do_snap(gx_raw, gy_raw);
            let gx = gx_sn.clamp(x_lo, x_hi);
            let gy = gy_sn.clamp(y_lo, y_hi);
            let idx = new_points.partition_point(|p| p[0] < gx);
            new_points.insert(idx, [gx, gy]);
            pts_changed = true;
        }
    }
    if let Some(idx) = remove_idx { new_points.remove(idx); }

    // Live-position trails.
    let abs_max   = if is_vec { in_max.abs().max(f32::EPSILON) } else {
        in_max.abs().max(in_min.abs()).max(f32::EPSILON)
    };
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
        type Trail = std::collections::VecDeque<(f32, std::time::Instant)>;
        let trail_id = ui.id().with((trail_id_tag, node_id, ch as u32));
        let mut trail: Trail = ui.data(|d| d.get_temp::<Trail>(trail_id).clone().unwrap_or_default());
        if trail_ms > 0 {
            trail.push_back((graph_x, now));
            while trail.front().map(|&(_, t)| now.duration_since(t) > trail_dur).unwrap_or(false) {
                trail.pop_front();
            }
        } else { trail.clear(); }
        let trail_pts: Vec<(f32, std::time::Instant)> = trail.iter().cloned().collect();
        ui.data_mut(|d| d.insert_temp(trail_id, trail));
        let ch_col = graph_channel_color(graph_ov, ch);
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

    // Optional override frame, painted last so it sits above the graph content.
    if let Some(stroke) = graph_outline {
        painter.rect_stroke(rect, 2.0, stroke, egui::StrokeKind::Inside);
    }

    // Write back curve points / biases.
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
}


// ── Curve save/load/reset file format ────────────────────────────────────────
// .fxc is a JSON object. Float and Vec curves share the same fields; the Vec
// variant simply never stores "absolute", "in_min", "out_min".  Loading into
// either type ignores fields it doesn't use, so files are cross-compatible.
#[derive(serde::Serialize, serde::Deserialize)]
pub(crate) struct CurveFile {
    #[serde(default)]
    pub(crate) points:          Vec<[f64; 2]>,
    #[serde(default)]
    pub(crate) biases:          Vec<f64>,
    #[serde(default = "default_true")]
    pub(crate) absolute:        bool,
    #[serde(default = "default_neg1")]
    pub(crate) in_min:          f64,
    #[serde(default = "default_1")]
    pub(crate) in_max:          f64,
    #[serde(default = "default_neg1")]
    pub(crate) out_min:         f64,
    #[serde(default = "default_1")]
    pub(crate) out_max:         f64,
    #[serde(default = "default_4")]
    pub(crate) grid_x:          i64,
    #[serde(default = "default_4")]
    pub(crate) grid_y:          i64,
    #[serde(default)]
    pub(crate) snap:            bool,
    #[serde(default)]
    pub(crate) scale_t:         f64,
    #[serde(default = "default_300")]
    pub(crate) trail_ms:        i64,
    #[serde(default)]
    pub(crate) show_scaled_grid: bool,
    #[serde(default)]
    pub(crate) show_grid_labels: bool,
}
pub(crate) fn default_true()  -> bool { true  }
pub(crate) fn default_neg1()  -> f64  { -1.0  }
pub(crate) fn default_1()     -> f64  {  1.0  }
pub(crate) fn default_4()     -> i64  {  4    }
pub(crate) fn default_300()   -> i64  { 300   }

/// Resolves the (points, biases) param keys to operate on for a given node.
/// For two-way curves this respects `active_lane` so the user only touches
/// the lane they're currently editing — keeps the file format identical to
/// regular curves and avoids needing a two-lane file variant.
pub(crate) fn curve_param_keys(node: &NodeData) -> (&'static str, &'static str) {
    if node.module_id == "module.twoway_response_curve" {
        let lane = node.params.get("active_lane").and_then(|v| v.as_str()).unwrap_or("up");
        if lane == "dn" { ("points_dn", "biases_dn") } else { ("points", "biases") }
    } else {
        ("points", "biases")
    }
}

pub(crate) fn curve_header_save(node_id: NodeId, snarl: &Snarl<NodeData>) {
    let Some(n) = snarl.get_node(node_id) else { return };
    let (pts_key, bias_key) = curve_param_keys(n);
    let pts: Vec<[f64; 2]> = n.params.get(pts_key)
        .and_then(|v| v.as_array())
        .map(|arr| arr.iter().filter_map(|p| {
            let a = p.as_array()?;
            Some([a.get(0)?.as_f64()?, a.get(1)?.as_f64()?])
        }).collect())
        .unwrap_or_default();
    let bss: Vec<f64> = n.params.get(bias_key)
        .and_then(|v| v.as_array())
        .map(|arr| arr.iter().filter_map(|b| b.as_f64()).collect())
        .unwrap_or_default();
    let cf = CurveFile {
        points:           pts,
        biases:           bss,
        absolute:         n.params.get("absolute").and_then(|v| v.as_bool()).unwrap_or(true),
        in_min:           n.params.get("in_min") .and_then(|v| v.as_f64()).unwrap_or(-1.0),
        in_max:           n.params.get("in_max") .and_then(|v| v.as_f64()).unwrap_or( 1.0),
        out_min:          n.params.get("out_min").and_then(|v| v.as_f64()).unwrap_or(-1.0),
        out_max:          n.params.get("out_max").and_then(|v| v.as_f64()).unwrap_or( 1.0),
        grid_x:           n.params.get("grid_x") .and_then(|v| v.as_i64()).unwrap_or(4),
        grid_y:           n.params.get("grid_y") .and_then(|v| v.as_i64()).unwrap_or(4),
        snap:             n.params.get("snap")   .and_then(|v| v.as_bool()).unwrap_or(false),
        scale_t:          n.params.get("scale_t").and_then(|v| v.as_f64()).unwrap_or(0.0),
        trail_ms:         n.params.get("trail_ms").and_then(|v| v.as_i64()).unwrap_or(300),
        show_scaled_grid: n.params.get("show_scaled_grid").and_then(|v| v.as_bool()).unwrap_or(false),
        show_grid_labels: n.params.get("show_grid_labels").and_then(|v| v.as_bool()).unwrap_or(false),
    };
    if let Some(path) = crate::overlay::with_overlay_not_topmost(|| {
        rfd::FileDialog::new()
            .add_filter("FlexInput Curve", &["fxc"])
            .set_file_name("curve.fxc")
            .save_file()
    }) {
        if let Ok(json) = serde_json::to_string_pretty(&cf) {
            let _ = std::fs::write(path, json);
        }
    }
}

pub(crate) fn curve_header_load(node_id: NodeId, is_float: bool, snarl: &mut Snarl<NodeData>) {
    let Some(path) = crate::overlay::with_overlay_not_topmost(|| {
        rfd::FileDialog::new()
            .add_filter("FlexInput Curve", &["fxc"])
            .pick_file()
    }) else { return };
    let Ok(json) = std::fs::read_to_string(path) else { return };
    let Ok(cf)   = serde_json::from_str::<CurveFile>(&json) else { return };
    let Some(node) = snarl.get_node_mut(node_id) else { return };
    let (pts_key, bias_key) = curve_param_keys(node);
    let pts_json: Vec<Value> = cf.points.iter()
        .map(|p| serde_json::json!([p[0], p[1]]))
        .collect();
    let bss_json: Vec<Value> = cf.biases.iter()
        .filter_map(|&b| Number::from_f64(b).map(Value::Number))
        .collect();
    node.params.insert(pts_key.into(),  Value::Array(pts_json));
    node.params.insert(bias_key.into(), Value::Array(bss_json));
    node.params.insert("grid_x".into(),  serde_json::json!(cf.grid_x));
    node.params.insert("grid_y".into(),  serde_json::json!(cf.grid_y));
    node.params.insert("snap".into(),    Value::Bool(cf.snap));
    if let Some(n) = Number::from_f64(cf.scale_t) {
        node.params.insert("scale_t".into(), Value::Number(n));
    }
    node.params.insert("trail_ms".into(), serde_json::json!(cf.trail_ms));
    node.params.insert("show_scaled_grid".into(), Value::Bool(cf.show_scaled_grid));
    node.params.insert("show_grid_labels".into(), Value::Bool(cf.show_grid_labels));
    if is_float {
        node.params.insert("absolute".into(), Value::Bool(cf.absolute));
        if let Some(n) = Number::from_f64(cf.in_min)  { node.params.insert("in_min".into(),  Value::Number(n)); }
        if let Some(n) = Number::from_f64(cf.in_max)  { node.params.insert("in_max".into(),  Value::Number(n)); }
        if let Some(n) = Number::from_f64(cf.out_min) { node.params.insert("out_min".into(), Value::Number(n)); }
        if let Some(n) = Number::from_f64(cf.out_max) { node.params.insert("out_max".into(), Value::Number(n)); }
    } else {
        if let Some(n) = Number::from_f64(cf.in_max)  { node.params.insert("in_max".into(),  Value::Number(n)); }
        if let Some(n) = Number::from_f64(cf.out_max) { node.params.insert("out_max".into(), Value::Number(n)); }
    }
}

/// Graph-only load: replaces just the active lane's `points` + `biases`
/// from the chosen `.fxc` file. Range / grid / scale / trail / labels are
/// left untouched so the right-click menu (which is also available from
/// sub-patch layouts where module settings may not be visible) never
/// surprises the user by changing hidden state.
pub(crate) fn curve_graph_load(node_id: NodeId, snarl: &mut Snarl<NodeData>) {
    let Some(path) = crate::overlay::with_overlay_not_topmost(|| {
        rfd::FileDialog::new()
            .add_filter("FlexInput Curve", &["fxc"])
            .pick_file()
    }) else { return };
    let Ok(json) = std::fs::read_to_string(path) else { return };
    let Ok(cf)   = serde_json::from_str::<CurveFile>(&json) else { return };
    let Some(node) = snarl.get_node_mut(node_id) else { return };
    let (pts_key, bias_key) = curve_param_keys(node);
    let pts_json: Vec<Value> = cf.points.iter()
        .map(|p| serde_json::json!([p[0], p[1]]))
        .collect();
    let bss_json: Vec<Value> = cf.biases.iter()
        .filter_map(|&b| Number::from_f64(b).map(Value::Number))
        .collect();
    node.params.insert(pts_key.into(),  Value::Array(pts_json));
    node.params.insert(bias_key.into(), Value::Array(bss_json));
}

/// Graph-only reset: snaps just the active lane back to the default
/// `[(0,0), (1,1)]` identity curve. Like `curve_graph_load`, leaves other
/// module settings (range, grid, scale, etc.) untouched.
pub(crate) fn curve_graph_reset(node_id: NodeId, snarl: &mut Snarl<NodeData>) {
    let Some(node) = snarl.get_node_mut(node_id) else { return };
    let (pts_key, bias_key) = curve_param_keys(node);
    node.params.insert(pts_key.into(),  serde_json::json!([[0.0, 0.0], [1.0, 1.0]]));
    node.params.insert(bias_key.into(), serde_json::json!([0.0]));
}

pub(crate) fn curve_header_reset(node_id: NodeId, is_float: bool, snarl: &mut Snarl<NodeData>) {
    let Some(node) = snarl.get_node_mut(node_id) else { return };
    // Two-way: reset only the currently-selected lane's points/biases. Other
    // settings (grid, scale, range, etc.) are shared across both lanes and
    // still reset together. For regular/vec curves `curve_param_keys` returns
    // `("points", "biases")` so the behaviour is unchanged.
    let (pts_key, bias_key) = curve_param_keys(node);
    node.params.insert(pts_key.into(),             serde_json::json!([[0.0, 0.0], [1.0, 1.0]]));
    node.params.insert(bias_key.into(),            serde_json::json!([0.0]));
    node.params.insert("grid_x".into(),            serde_json::json!(4i64));
    node.params.insert("grid_y".into(),            serde_json::json!(4i64));
    node.params.insert("snap".into(),              Value::Bool(false));
    node.params.insert("scale_t".into(),           serde_json::json!(0.0f64));
    node.params.insert("trail_ms".into(),          serde_json::json!(300i64));
    node.params.insert("show_scaled_grid".into(),  Value::Bool(false));
    node.params.insert("show_grid_labels".into(),  Value::Bool(false));
    if is_float {
        node.params.insert("absolute".into(),  Value::Bool(true));
        node.params.insert("in_min".into(),    serde_json::json!(-1.0f64));
        node.params.insert("in_max".into(),    serde_json::json!( 1.0f64));
        node.params.insert("out_min".into(),   serde_json::json!(-1.0f64));
        node.params.insert("out_max".into(),   serde_json::json!( 1.0f64));
    } else {
        node.params.insert("in_max".into(),    serde_json::json!(1.0f64));
        node.params.insert("out_max".into(),   serde_json::json!(1.0f64));
    }
}

/// Maps x ∈ [0,1] → [0,1] continuously. t=0 → linear; t<0 → log-like; t>0 → exp-like.
/// Power law p = 2^(t*3): at t=±1, p=8 or 1/8 — far more extreme than the old log/exp modes.
pub(crate) fn curve_scale(x: f32, t: f32) -> f32 {
    if t.abs() < 1e-4 { return x; }
    x.clamp(0.0, 1.0).powf(2.0f32.powf(t * 3.0))
}

/// Inverse of curve_scale: given a scaled output y ∈ [0,1], find x such that curve_scale(x,t)=y.
/// Used to place grid lines at perceptually even intervals under the current scaling.
pub(crate) fn curve_scale_inv(y: f32, t: f32) -> f32 {
    if t.abs() < 1e-4 { return y; }
    let p = 2.0f32.powf(t * 3.0);
    y.clamp(0.0, 1.0).powf(1.0 / p)
}

// ── Response curve right-click menu ──────────────────────────────────────────
//
// Save/Load/Reset call into the existing `curve_header_*` helpers, which use
// the canonical `.fxc` file format (`CurveFile` struct) and — via
// `curve_param_keys` — operate on the currently-selected lane for two-way
// curves. Copy/Paste live in egui memory only (no file format involved).

pub(crate) const CURVE_CLIP_KEY: &str = "fxi_curve_clipboard";

#[derive(Clone, Debug)]
pub(crate) struct CurveClip {
    pub(crate) points: Vec<[f32; 2]>,
    pub(crate) biases: Vec<f32>,
}

/// Snapshot the active (points, biases) pair from a node, using the same
/// per-lane resolution as save/load/reset (so two-way Copy grabs the lane
/// the user is currently editing).
pub(crate) fn curve_clipboard_copy_from(node: &NodeData) -> CurveClip {
    let (pts_key, bias_key) = curve_param_keys(node);
    let points: Vec<[f32; 2]> = node.params.get(pts_key)
        .and_then(|v| v.as_array())
        .map(|arr| arr.iter().filter_map(|p| {
            let a = p.as_array()?;
            Some([a.get(0)?.as_f64()? as f32, a.get(1)?.as_f64()? as f32])
        }).collect())
        .unwrap_or_else(|| vec![[0.0, 0.0], [1.0, 1.0]]);
    let biases: Vec<f32> = node.params.get(bias_key)
        .and_then(|v| v.as_array())
        .map(|arr| arr.iter().filter_map(|b| b.as_f64().map(|f| f as f32)).collect())
        .unwrap_or_default();
    CurveClip { points, biases }
}

/// Write a clipboard payload into the node's active lane (or the only lane
/// for regular/vec curves). Biases are resized to `points.len() - 1` so the
/// sampler invariant is maintained.
pub(crate) fn curve_clipboard_paste_into(node: &mut NodeData, mut clip: CurveClip) {
    let (pts_key, bias_key) = curve_param_keys(node);
    let need = clip.points.len().saturating_sub(1);
    clip.biases.resize(need, 0.0);
    let pts: Vec<Value> = clip.points.iter()
        .map(|p| serde_json::json!([p[0], p[1]])).collect();
    let bss: Vec<Value> = clip.biases.iter()
        .filter_map(|&b| Number::from_f64(b as f64).map(Value::Number))
        .collect();
    node.params.insert(pts_key.into(), Value::Array(pts));
    node.params.insert(bias_key.into(), Value::Array(bss));
}

pub(crate) fn curve_clipboard_get(ctx: &egui::Context) -> Option<CurveClip> {
    ctx.data(|d| d.get_temp::<CurveClip>(egui::Id::new(CURVE_CLIP_KEY)))
}

pub(crate) fn curve_clipboard_set(ctx: &egui::Context, data: CurveClip) {
    ctx.data_mut(|d| d.insert_temp(egui::Id::new(CURVE_CLIP_KEY), data));
}

/// Shared right-click menu emitted by every response-curve widget (body and
/// pinned variants). Reset and Load only touch the curve's points/biases —
/// not range, grid, scale, or other module settings — so invoking the menu
/// from a sub-patch layout (where module sliders may be hidden) never alters
/// surrounding state the patch author tuned. Save still writes the full
/// `.fxc` so files remain interchangeable with the header Save button.
/// `lane_label` is "Up" or "Down" for two-way curves; `None` for regular/vec.
pub(crate) fn curve_context_menu(
    ui: &mut egui::Ui,
    node_id: NodeId,
    snarl: &mut Snarl<NodeData>,
    lane_label: Option<&str>,
) -> bool {
    let mut mutated = false;
    let prefix = lane_label.map(|p| format!("{p} curve: ")).unwrap_or_default();

    if ui.button(format!("{prefix}Reset")).on_hover_text("Reset only the curve (range / grid / scale stay as-is)").clicked() {
        curve_graph_reset(node_id, snarl);
        mutated = true;
        ui.close();
    }
    ui.separator();
    if ui.button(format!("{prefix}Copy")).clicked() {
        if let Some(node) = snarl.get_node(node_id) {
            let clip = curve_clipboard_copy_from(node);
            curve_clipboard_set(ui.ctx(), clip);
        }
        ui.close();
    }
    let has_clip = curve_clipboard_get(ui.ctx()).is_some();
    if ui.add_enabled(has_clip, egui::Button::new(format!("{prefix}Paste"))).clicked() {
        if let Some(clip) = curve_clipboard_get(ui.ctx()) {
            if let Some(node) = snarl.get_node_mut(node_id) {
                curve_clipboard_paste_into(node, clip);
                mutated = true;
            }
        }
        ui.close();
    }
    ui.separator();
    if ui.button(format!("{prefix}Save…")).clicked() {
        curve_header_save(node_id, snarl);
        ui.close();
    }
    if ui.button(format!("{prefix}Load…")).on_hover_text("Load only the curve from .fxc (range / grid / scale stay as-is)").clicked() {
        curve_graph_load(node_id, snarl);
        mutated = true;
        ui.close();
    }
    mutated
}

// ── Signal helpers ────────────────────────────────────────────────────────────

pub(crate) fn sig_f32(s: &Signal) -> f32 {
    match s {
        Signal::Float(f) => *f,
        Signal::Bool(b)  => if *b { 1.0 } else { 0.0 },
        Signal::Int(i)   => *i as f32,
        Signal::Vec2(v)  => v.length(),
        Signal::Vec4(v)  => v.length(),
    }
}
