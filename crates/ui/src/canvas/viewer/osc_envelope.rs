//! Oscillator and Envelope node bodies + the envelope curve editor.

use super::*;

pub(crate) fn show_oscillator_body(node_id: NodeId, inputs: &[InPin], ui: &mut egui::Ui, snarl: &mut Snarl<NodeData>, view_salt: u64) {
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

    // The settings sit in a column of a constant width, centred on the module: the shape strip,
    // the wave it makes, then the frequency and the phase - each with the small toggle that says
    // how to read it - on rows of their own. The column is a constant rather than what the rows
    // measure, because that is what lets this be narrower than a label, a DragValue and a
    // separator per setting: it was 224 points of module to hold a shape and two numbers.
    const COL: f32 = 128.0;
    const ROW: f32 = 4.0;
    const PREVIEW_H: f32 = 26.0;
    const PILL_W: f32 = 62.0;
    use super::simple_bodies::{trace_rect, value_pill_number, PILL_H};
    let group_h = 3.0 * PILL_H + PREVIEW_H + 3.0 * ROW;
    ui.allocate_exact_size(egui::vec2(COL, group_h), egui::Sense::hover());
    // Folded, the column is still asked for - the frame keeps its blocks when a node folds - but
    // nothing in it is drawn. Snarl runs a collapsed node's body and shifts it up under the
    // header's clip, which is how the phase row's number came to paint itself across the word
    // "Oscillator".
    if !super::simple_bodies::node_open(snarl, node_id) {
        return;
    }
    let inner = match super::module_icons::module_inner(ui, view_salt, node_id) {
        Some(r) => r,
        None => return,
    };
    let column = ui.max_rect();
    trace_rect(ui, node_id, "body_column", column);
    let cx = inner.center().x;
    let left = cx - COL * 0.5;
    // Vertically on the body column, like the Counter's cells: this group nearly fills it, so
    // the module's middle and the column's are the same answer to within a few points.
    let top = kit::group_top(column, group_h);
    let row_at = |i: usize| -> f32 {
        match i {
            0 => top,
            1 => top + PILL_H + ROW,
            2 => top + PILL_H + ROW + PREVIEW_H + ROW,
            _ => top + 2.0 * (PILL_H + ROW) + PREVIEW_H + ROW,
        }
    };

    // The shape: four cells the way the Counter's mode is, so the words are the control.
    let shape_rect = egui::Rect::from_min_size(egui::pos2(left, row_at(0)), egui::vec2(COL, PILL_H));
    {
        let mut child = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(shape_rect)
                .layout(egui::Layout::left_to_right(egui::Align::Center))
                .id_salt("osc_shape"),
        );
        // A strip lays its cells out end to end and adds its own spacing between them; the row
        // it is drawn in already asked for a fixed width, so the style's inter-widget spacing is
        // extra and the last cell lands past the row - into the pin labels beside it.
        child.spacing_mut().item_spacing.x = 0.0;
        changed |= kit::strip(
            &mut child,
            COL,
            &mut shape,
            &[
                ("sine".to_string(), "Sine", "Smooth, no harmonics"),
                ("triangle".to_string(), "Tri", "Soft edges, odd harmonics"),
                ("saw".to_string(), "Saw", "Ramps up, drops at once"),
                ("square".to_string(), "Sqr", "Halves the signal, odd harmonics"),
            ],
        );
        drop(child);
    }
    trace_rect(ui, node_id, "osc_shape", shape_rect);

    // The wave itself: what the shape and the phase actually make, and how bi/unipolar changes
    // it. Painted across the column, so it costs nothing in width.
    let preview_rect = egui::Rect::from_min_size(egui::pos2(left, row_at(1)), egui::vec2(COL, PREVIEW_H));
    trace_rect(ui, node_id, "osc_preview", preview_rect);
    if ui.is_rect_visible(preview_rect) {
        let painter = ui.painter_at(preview_rect);
        painter.rect_filled(preview_rect, 4.0, egui::Color32::from_gray(18));
        let zero_y = if bipolar {
            preview_rect.center().y
        } else {
            preview_rect.bottom()
        };
        painter.line_segment(
            [egui::pos2(preview_rect.left(), zero_y), egui::pos2(preview_rect.right(), zero_y)],
            egui::Stroke::new(0.5_f32, egui::Color32::from_gray(55)),
        );
        let n = 128usize;
        let pts: Vec<egui::Pos2> = (0..=n)
            .map(|i| {
                let t = i as f32 / n as f32;
                let phase = (t + phase_p).rem_euclid(1.0);
                let raw = flexinput_engine::osc_sample(&shape, phase);
                let v = if bipolar { raw } else { (raw + 1.0) * 0.5 };
                let x = preview_rect.left() + t * preview_rect.width();
                let y = if bipolar {
                    preview_rect.center().y - v * preview_rect.height() * 0.42
                } else {
                    preview_rect.bottom() - v * preview_rect.height() * 0.84
                };
                egui::pos2(x, y.clamp(preview_rect.top(), preview_rect.bottom()))
            })
            .collect();
        painter.add(egui::Shape::line(pts, egui::Stroke::new(1.5_f32, egui::Color32::from_rgb(100, 180, 255))));
    }

    // The frequency: a pill you drag across or double-click to type into, with the unit beside
    // it - hertz or the period in milliseconds, which is how a rhythm rather than a pitch is
    // counted. A value coming down the `freq` pin wins, so the pill takes no drags.
    let (lo, hi, step_per_pt) = if freq_unit == "hz" {
        (0.01f64, 200.0f64, 0.5f32)
    } else {
        (1.0, 60_000.0, 25.0)
    };
    let freq_rect = egui::Rect::from_min_size(egui::pos2(left, row_at(2)), egui::vec2(PILL_W, PILL_H));
    trace_rect(ui, node_id, "osc_freq_pill", freq_rect);
    let (freq_resp, typed) = value_pill_number(
        ui,
        node_id,
        "freq_param",
        freq_rect,
        "",
        &if freq_p.abs() >= 10.0 {
            format!("{}", freq_p.round() as i64)
        } else {
            format!("{freq_p:.2}")
        },
        0.0,
        lo as f64..=hi as f64,
    );
    let freq_resp = freq_resp.on_hover_text(if freq_wired {
        "The frequency the wire is driving - unplug it to set this here."
    } else {
        "How fast the wave repeats, in the unit next to it.\nDrag across the pill to change it, double-click to type."
    });
    if freq_wired {
        // The wire's value is what the module shows, and a drag would be a lie.
    } else {
        if let Some(v) = typed {
            freq_p = (v as f32).clamp(lo as f32, hi as f32);
            changed = true;
        }
        if freq_resp.dragged() {
            freq_p = (freq_p + freq_resp.drag_delta().x * step_per_pt).clamp(lo as f32, hi as f32);
            changed = true;
        }
    }
    let unit_rect = egui::Rect::from_min_size(
        egui::pos2(left + PILL_W + ROW, row_at(2)),
        egui::vec2(COL - PILL_W - ROW, PILL_H),
    );
    {
        let mut child = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(unit_rect)
                .layout(egui::Layout::left_to_right(egui::Align::Center))
                .id_salt("osc_unit"),
        );
        // A strip lays its cells out end to end and adds its own spacing between them; the row
        // it is drawn in already asked for a fixed width, so the style's inter-widget spacing is
        // extra and the last cell lands past the row - into the pin labels beside it.
        child.spacing_mut().item_spacing.x = 0.0;
        changed |= kit::strip(
            &mut child,
            unit_rect.width(),
            &mut freq_unit,
            &[
                ("hz".to_string(), "Hz", "Cycles per second"),
                ("ms".to_string(), "ms", "One cycle's length in milliseconds"),
            ],
        );
        drop(child);
    }
    trace_rect(ui, node_id, "osc_unit", unit_rect);

    // The phase, and whether the wave swings -1..1 or 0..1.
    let phase_rect = egui::Rect::from_min_size(egui::pos2(left, row_at(3)), egui::vec2(PILL_W, PILL_H));
    trace_rect(ui, node_id, "osc_phase_pill", phase_rect);
    let (phase_resp, typed) = value_pill_number(
        ui,
        node_id,
        "phase_param",
        phase_rect,
        "",
        &format!("{phase_p:.2}"),
        0.0,
        0.0..=1.0,
    );
    let phase_resp = phase_resp.on_hover_text(if phase_wired {
        "The offset the wire is driving - unplug it to set this here."
    } else {
        "Where in the cycle the wave starts, as a fraction of a turn."
    });
    if !phase_wired {
        if let Some(v) = typed {
            phase_p = (v as f32).rem_euclid(1.0);
            changed = true;
        }
        if phase_resp.dragged() {
            phase_p = (phase_p + phase_resp.drag_delta().x * 0.01).rem_euclid(1.0);
            changed = true;
        }
    }
    let bi_rect = egui::Rect::from_min_size(
        egui::pos2(left + PILL_W + ROW, row_at(3)),
        egui::vec2(COL - PILL_W - ROW, PILL_H),
    );
    {
        let mut child = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(bi_rect)
                .layout(egui::Layout::left_to_right(egui::Align::Center))
                .id_salt("osc_bipolar"),
        );
        // A strip lays its cells out end to end and adds its own spacing between them; the row
        // it is drawn in already asked for a fixed width, so the style's inter-widget spacing is
        // extra and the last cell lands past the row - into the pin labels beside it.
        child.spacing_mut().item_spacing.x = 0.0;
        changed |= kit::strip(
            &mut child,
            bi_rect.width(),
            &mut bipolar,
            &[
                (true, "Bi", "-1..1, centred on 0"),
                (false, "Uni", "0..1, one-sided"),
            ],
        );
        drop(child);
    }
    trace_rect(ui, node_id, "osc_bipolar", bi_rect);
    let freq_area = freq_rect.union(unit_rect);
    let phase_area = phase_rect.union(bi_rect);


    if changed {
        if let Some(node) = snarl.get_node_mut(node_id) {
            node.params.insert("shape".into(),      Value::String(shape));
            node.params.insert("freq_unit".into(),  Value::String(freq_unit));
            node.params.insert("bipolar".into(),    Value::Bool(bipolar));
            if let Some(n) = Number::from_f64(freq_p  as f64) { node.params.insert("freq_param".into(),  Value::Number(n)); }
            if let Some(n) = Number::from_f64(phase_p as f64) { node.params.insert("phase_param".into(), Value::Number(n)); }
        }
    }
    register_exposable_element(ui, node_id, "shape", shape_rect);
    register_exposable_element(ui, node_id, "freq", freq_area);
    register_exposable_element(ui, node_id, "phase", phase_area);
    register_exposable_element(ui, node_id, "preview", preview_rect);
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
/// The envelope's length as it is written in the pill: no false precision in ms, two places
/// in seconds, and hertz counted the way a repeating envelope is thought about.
fn envelope_time_text(v: f32, timebase: &str) -> String {
    match timebase {
        "s" => {
            if v.abs() >= 10.0 {
                format!("{v:.1} s")
            } else {
                format!("{v:.3} s")
            }
        }
        "hz" => format!("{v:.1} Hz"),
        _ => {
            if v.abs() >= 10.0 {
                format!("{} ms", v.round() as i64)
            } else {
                format!("{v:.1} ms")
            }
        }
    }
}
/// What an envelope point is: its level, and where along the envelope's length it sits, in the
/// unit the module is timed in. The graph gives no numbers of its own at a size this small.
fn envelope_point_help(x: f32, y: f32, period_ms: f32) -> String {
    format!("{:.2} level at {}", y, envelope_time_at(x, period_ms))
}
/// How far into an envelope `period_ms` long the graph position `x` (0..1) is.
fn envelope_time_at(x: f32, period_ms: f32) -> String {
    let t = x * period_ms;
    if period_ms >= 1000.0 {
        format!("{:.2} s", t / 1000.0)
    } else {
        format!("{:.0} ms", t)
    }
}
/// The module's own snap setting, for the reason [`graph_shown`] gives.
fn snap_on_param(n: &crate::canvas::node::NodeData) -> bool {
    n.params.get("snap").and_then(|v| v.as_bool()).unwrap_or(false)
}

/// What one of the graph's three switches means for a particular copy of it: the pinned
/// copy's own choice when it has one, otherwise whatever the module on the canvas is set to.
/// The default is the module, so a copy drawn before these overrides existed - or one nobody
/// touched - looks exactly as it did.
fn graph_shown(ov: Option<bool>, on_module: bool) -> bool {
    ov.unwrap_or(on_module)
}

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
            // A pinned copy of this curve can be told to draw its grid differently from the
            // module on the canvas; with no override on a field the copy follows the module.
            let sn = graph_shown(graph_ov.and_then(|o| o.snap), snap_on_param(n));
            let sus  = n.params.get("sustain").and_then(|v| v.as_f64()).unwrap_or(0.3) as f32;
            let sg = graph_shown(
                graph_ov.and_then(|o| o.show_grid),
                n.params.get("show_grid").and_then(|v| v.as_bool()).unwrap_or(true),
            );
            let sgl = graph_shown(
                graph_ov.and_then(|o| o.show_labels),
                n.params
                    .get("show_grid_labels")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false),
            );
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
        let gs = egui::Stroke::new(0.5_f32, grid_faint);
        for &x in &grid_x_positions { painter.line_segment([c2s(x, 0.0), c2s(x, 1.0)], gs); }
        for &y in &grid_y_positions  { painter.line_segment([c2s(0.0, y), c2s(1.0, y)], gs); }
    }

    // Grid labels: X = time position, Y = 0-100%. Time runs along the bottom edge: an
    // envelope peaks at the top, and a point's own value there was written over the times.
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
            painter.text(egui::pos2(sx + 2.0, rect.bottom() - 1.0),
                egui::Align2::LEFT_BOTTOM, &label, font.clone(), label_col);
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
    //
    // One joined line per part, sampled at every control point as well as evenly: separate
    // segments left a notch at each joint, and even steps alone cut the tip off a peak that
    // fell between two of them.
    if new_points.len() >= 2 {
        let main_col = Color32::from_gray(120);
        let dim_col  = Color32::from_gray(55);
        let split = if buffer_mode { sustain_snapped.clamp(0.0, 1.0) } else { 1.0 };
        let mut xs: Vec<f32> = (0..=120).map(|i| i as f32 / 120.0).collect();
        xs.extend(new_points.iter().map(|p| p[0].clamp(0.0, 1.0)));
        xs.push(split);
        xs.sort_by(f32::total_cmp);
        xs.dedup();
        let at = |x: f32| c2s(x, flexinput_engine::sample_curve(&new_points, x, &new_biases).clamp(0.0, 1.0));
        let rest: Vec<egui::Pos2> = xs.iter().filter(|&&x| x >= split).map(|&x| at(x)).collect();
        let live: Vec<egui::Pos2> = xs.iter().filter(|&&x| x <= split).map(|&x| at(x)).collect();
        if rest.len() >= 2 {
            painter.add(egui::Shape::line(rest, egui::Stroke::new(1.5_f32, dim_col)));
        }
        if live.len() >= 2 {
            painter.add(egui::Shape::line(live, egui::Stroke::new(1.5_f32, main_col)));
        }
        // Hold+Bounce: held horizontal path at the sustain value across the buffer.
        if buffer_mode {
            let hold_y = flexinput_engine::sample_curve(&new_points, sustain_snapped, &new_biases).clamp(0.0, 1.0);
            painter.line_segment(
                [c2s(sustain_snapped, hold_y), c2s(1.0, hold_y)],
                egui::Stroke::new(1.5_f32, main_col),
            );
        }
    }

    // Bias handles (Alt held)
    let nav_bias = ui.ctx().data(|d|
        d.get_temp::<u64>(egui::Id::new(("gp_nav_curve_bias", node_id.0))))
        .map_or(false, |p| crate::widgets::nav_pass_matches(ui.ctx(), p));
    let alt_held = ui.input(|i| i.modifiers.alt) || nav_bias;
    let mut bends = crate::touch::BendFingers::begin(ui, ui.id().with(("bends", node_id)));
    if (alt_held || bends.exposed()) && new_points.len() >= 2 {
        while new_biases.len() < new_points.len() - 1 { new_biases.push(0.0); }
        for seg in 0..(new_points.len() - 1) {
            let mid_x = (new_points[seg][0] + new_points[seg + 1][0]) * 0.5;
            let mid_y = flexinput_engine::sample_curve(&new_points, mid_x, &new_biases).clamp(0.0, 1.0);
            let hpos  = c2s(mid_x, mid_y);
            let hid   = ui.id().with(("env_bias_h", node_id, seg));
            let hresp = ui.interact(
                egui::Rect::from_center_size(hpos, egui::Vec2::splat(14.0)),
                hid, egui::Sense::click_and_drag());
            let finger = bends.finger(ui, hresp.id, hpos);
            if hresp.double_clicked() { new_biases[seg] = 0.0; bias_changed = true; }
            else if hresp.dragged() || finger.is_some() {
                new_biases[seg] = (new_biases[seg] - (hresp.drag_delta().y + finger.unwrap_or(0.0)) / rect.height()).clamp(-2.0, 2.0);
                bias_changed = true;
            }
            let hcol = if hresp.hovered() || hresp.dragged() || finger.is_some() {
                Color32::from_rgb(255, 220, 50)
            } else { Color32::from_rgb(180, 140, 20) };
            painter.circle_filled(hpos, bends.dot_radius(4.0), hcol);
            painter.circle_stroke(hpos, bends.dot_radius(4.0), egui::Stroke::new(1.0_f32, Color32::from_gray(100)));
        }
    }

    // Control points
    // Gamepad-nav: is the SUSTAIN line focused (dpad up/down toggle)? When so,
    // the dot highlight is suppressed and the sustain line is emphasised instead.
    let sustain_focused = ui.ctx().data(|d| {
        d.get_temp::<(u64, bool)>(egui::Id::new(("gp_nav_curve_sustain", node_id.0)))
            .map(|(_, f)| f)
            .unwrap_or(false)
    });
    let mut remove_idx: Option<usize> = None;
    let mut point_down = false;
    for i in 0..new_points.len() {
        let [px, py] = new_points[i];
        let screen   = c2s(px, py);
        let pt_id    = ui.id().with(("env_cpt", node_id, i));
        let pt_resp  = ui.interact(
            egui::Rect::from_center_size(screen, egui::Vec2::splat(12.0)),
            pt_id, egui::Sense::click_and_drag());
        point_down |= pt_resp.is_pointer_button_down_on() || pt_resp.dragged();
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
        if new_points.len() > 2 { bends.removal_ring(ui, &pt_resp, screen); }
        if pt_resp.secondary_clicked() && !bends.keeps_point(ui, &pt_resp) && new_points.len() > 2 {
            remove_idx = Some(i);
            pts_changed = true;
        }
        let col = if pt_resp.hovered() || pt_resp.dragged() { Color32::WHITE } else { Color32::from_gray(190) };
        painter.circle_filled(screen, 5.0, col);
        painter.circle_stroke(screen, 5.0, egui::Stroke::new(1.0_f32, Color32::from_gray(80)));
        // The point's level beside the dot, and on hover where in the envelope's length it
        // falls - the reading the Response Curve's dots give. The number goes with the graph's
        // own labels, or whenever the dot is under the pointer; the time is always available,
        // because the graph is the only place any of it is written.
        if show_grid_labels || pt_resp.hovered() || pt_resp.dragged() {
            super::curve_support::paint_point_value(&painter, rect, screen, format!("{:.2}", py));
        }
        pt_resp.on_hover_text(envelope_point_help(px, py, period_ms));

        // Gamepad-nav dot highlight — mirror the response curve painter so the
        // user sees which ADSR point is selected (works in sub-patch AND config
        // overlay, which re-stamps `gp_nav_curve_sel` with its own viewport pass).
        let sel: Option<(u64, usize, bool)> = ui.ctx().data(|d|
            d.get_temp(egui::Id::new(("gp_nav_curve_sel", node_id.0))));
        if let Some((pass, sel_i, editing_dot)) = sel {
            if crate::widgets::nav_pass_matches(ui.ctx(), pass) && sel_i == i && !sustain_focused {
                let accent = ui.visuals().selection.stroke.color;
                let [r8, g8, b8, _] = accent.to_array();
                for k in 0..5 {
                    let t = (k as f32 + 1.0) / 5.0;
                    let rr = (if editing_dot { 16.0 } else { 12.0 }) * t;
                    let a = ((if editing_dot { 170.0 } else { 120.0 }) * (1.0 - t)) as u8;
                    if a == 0 { continue; }
                    painter.circle_stroke(screen, rr,
                        egui::Stroke::new(2.0_f32, Color32::from_rgba_unmultiplied(r8, g8, b8, a)));
                }
                painter.circle_filled(screen, if editing_dot { 6.0 } else { 5.0 }, accent);
                painter.circle_stroke(screen, if editing_dot { 6.0 } else { 5.0 },
                    egui::Stroke::new(1.5_f32, Color32::WHITE));
            }
        }
    }

    // Right-click removes / double-click adds a point; biases stay on their segments.
    bends.finish(ui, point_down);
    if let Some(idx) = remove_idx { curve_remove_point(&mut new_points, &mut new_biases, idx); }
    if bg_resp.double_clicked() {
        if let Some(pos) = bg_resp.interact_pointer_pos() {
            let [gx_raw, gy_raw] = s2c(pos);
            let (gx_sn, gy_sn) = do_snap(gx_raw, gy_raw);
            let gx = gx_sn.clamp(0.0, 1.0);
            let gy = gy_sn.clamp(0.0, 1.0);
            let idx = new_points.partition_point(|p| p[0] < gx);
            curve_insert_point(&mut new_points, &mut new_biases, idx, [gx, gy]);
            pts_changed = true;
        }
    }

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
    // While the gamepad nav has the sustain line FOCUSED (dpad up/down toggle),
    // draw it thicker/brighter so the user sees left/right will move it
    // (`sustain_focused` was read up front, before the dot loop).
    let sus_x = rect.left() + sustain_snapped.clamp(0.0, 1.0) * rect.width();
    let (sus_w, sus_col) = if sustain_focused {
        (3.0, Color32::from_rgb(255, 190, 60))
    } else {
        (1.5_f32, Color32::from_rgba_unmultiplied(255, 155, 30, 200))
    };
    if sustain_focused {
        // Soft glow behind the line.
        painter.line_segment(
            [egui::pos2(sus_x, rect.top()), egui::pos2(sus_x, rect.bottom())],
            egui::Stroke::new(7.0_f32, Color32::from_rgba_unmultiplied(255, 155, 30, 60)),
        );
    }
    painter.line_segment(
        [egui::pos2(sus_x, rect.top()), egui::pos2(sus_x, rect.bottom())],
        egui::Stroke::new(sus_w, sus_col),
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
            egui::Stroke::new(1.0_f32, Color32::from_rgba_unmultiplied(255, 255, 255, 80)),
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
                painter.line_segment([prev, next], egui::Stroke::new(1.5_f32, col));
                prev = next;
            }
        }

        // Live dot at current position (held flat past sustain in Hold+Bounce)
        let ph_y = sample_y(&new_points, &new_biases, ph);
        painter.circle_filled(c2s(ph, ph_y), 3.5,
            Color32::from_rgba_unmultiplied(dot_col.r(), dot_col.g(), dot_col.b(), 220));

        request_repaint_throttled(ui.ctx());
    }

    // Live reading at the mouse or gamepad cursor, with the labels, as on the Response
    // Curve: when, and the level that comes out then - which past the sustain point of a
    // Hold+Bounce is the held level, not the dimmed curve.
    if show_grid_labels && !ui.input(|i| i.pointer.any_down()) {
        if let Some(at) = super::curve_support::graph_reading_pos(ui, rect, bg_resp.hover_pos()) {
            super::curve_support::paint_cursor_reading(&painter, rect, at, (0.0, 1.0, 0.0, 1.0),
                |x| sample_y(&new_points, &new_biases, x),
                |gx, gy| format!("{}  →  {:.2}", envelope_time_at(gx, period_ms), gy));
        }
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

pub(crate) fn show_envelope_body(node_id: NodeId, inputs: &[InPin], ui: &mut egui::Ui, snarl: &mut Snarl<NodeData>, body_w: f32, body_h: f32) {
    if let Some(node) = snarl.get_node_mut(node_id) { envelope_init_params(node); }

    let (flags, timebase, time_mul, sustain, grid_x, grid_y, snap) =
        snarl.get_node(node_id).map(|n| {
            let fl  = flexinput_engine::envelope_flags(&n.params);
            let tb  = n.params.get("timebase").and_then(|v| v.as_str()).unwrap_or("ms").to_string();
            let tm  = n.params.get("time_mul").and_then(|v| v.as_f64()).unwrap_or(500.0) as f32;
            let su  = n.params.get("sustain") .and_then(|v| v.as_f64()).unwrap_or(0.3)   as f32;
            let gx  = n.params.get("grid_x").and_then(|v| v.as_i64()).unwrap_or(4).max(1);
            let gy  = n.params.get("grid_y").and_then(|v| v.as_i64()).unwrap_or(4).max(1);
            let sn  = n.params.get("snap").and_then(|v| v.as_bool()).unwrap_or(false);
            // What the graph *shows* - its lines and their numbers - is not read here. Those
            // two switches live in the header, which writes the params itself; a body that read
            // them and wrote them back would be one frame of ordering away from undoing a click
            // on them.
            (fl, tb, tm, su, gx, gy, sn)
        }).unwrap_or_else(|| ((false, false, false), "ms".into(), 500.0, 0.3, 4, 4, false));

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
    let mut changed  = false;
    let mut sustain_changed = false;

    // The parts of this body the Properties shelf can pin, gathered as they are laid out.
    // The ids are the ones the pinned renderers look up, the same as when each of these
    // had a row of its own.
    let mut exposable: Vec<(&'static str, egui::Rect)> = Vec::new();

    let pts_x: Vec<f32> = snarl.get_node(node_id)
        .and_then(|n| n.params.get("points").and_then(|v| v.as_array()).map(|arr| {
            arr.iter().filter_map(|p| {
                let a = p.as_array()?;
                Some(a.get(0)?.as_f64()? as f32)
            }).collect()
        }))
        .unwrap_or_default();

    use super::simple_bodies::trace_rect;
    // One surface that follows the frame. The graph takes whatever height the rows don't need
    // and the whole width; the sustain slider is a row of the same width under it; and the four
    // groups of settings - the unit and the time it counts, the behaviour toggles, the grid
    // divisions, and what the grid shows - fill the width evenly, side by side in one band when
    // the frame is wide enough for them and wrapped into more bands as it narrows. It used to be
    // a `Resize` box with its own saved size on top of six stacked rows: the module didn't
    // follow its frame, and the rows cost height whether the frame had it or not.
    const GAP: f32 = 8.0;
    const SEP_W: f32 = 9.0;
    const CELL_GAP: f32 = 4.0;
    const MIN_GRAPH_H: f32 = 56.0;
    const MIN_GRAPH_W: f32 = 140.0;
    // A row is exactly one block of the layout grid, and the controls inside it are a little
    // shorter than the row. That is what makes resizing mean something: every block the grip
    // gives the module goes to the graph, and the rows land on block lines at any height.
    let block = crate::canvas::canvas_grid(ui.ctx()).map_or(28.0, |g| g.block);
    let row_h = block;
    let band_h = block;
    let ctrl_h = block - 4.0;
    // What each group needs to show its controls without cutting a label - and the band rule
    // that follows from it: a group that cannot get this inside a shared band takes a band to
    // itself, so the module grows taller rather than ending up with a length squeezed into a
    // short box. The first is the three unit words plus the room a seven-digit length needs;
    // the second is three squares and the air between them; the third is two number pills, that
    // air, and a square for the snap. The two graph switches are no longer here: they are in
    // the header.
    let group_min = [
        148.0f32,
        3.0 * ctrl_h + 2.0 * CELL_GAP,
        2.0 * 46.0 + 2.0 * CELL_GAP + ctrl_h,
    ];
    // Only the length group is written to want much: it holds the number the module is read
    // for, so a wider frame gives it away to nothing else. The other two ask for the size
    // their controls are, and a row that ends up wider than that spends it on air rather than
    // on a fatter button - see the mode squares and the grid pills.

    let column_width = body_w.max(MIN_GRAPH_W);

    // How many bands the three groups need: fill them in order and grow the count until every
    // band fits the column.
    let mut bands = 1usize;
    let band_widths = |bands: usize| -> Vec<f32> {
        let per = (group_min.len() + bands - 1) / bands;
        let mut out = vec![0.0f32; bands];
        let mut b = 0usize;
        let mut used = 0usize;
        for g in group_min.iter() {
            if used == per {
                b += 1;
                used = 0;
            }
            out[b] += g + if used > 0 { SEP_W + GAP } else { 0.0 };
            used += 1;
        }
        out
    };
    while bands < group_min.len() && band_widths(bands).iter().any(|w| *w > column_width + 0.5) {
        bands += 1;
    }
    let per_band = (group_min.len() + bands - 1) / bands;

    // Rows from the bottom up, so the graph gets what's left: it's the thing being edited, and
    // a frame that runs out of height loses graph, not controls.
    let rows_h = (bands as f32 + 1.0) * row_h;
    // The body asks for at least the rows plus a graph worth editing, whatever the grip set:
    // narrow frames wrap the groups into more bands, and the module gets taller for them
    // rather than losing the graph.
    let want = egui::vec2(column_width, body_h.max(rows_h + MIN_GRAPH_H));
    let (flow, _) = ui.allocate_exact_size(want, egui::Sense::hover());
    // Folded, the frame is still asked for - a module keeps its blocks when it folds - and
    // nothing inside it is drawn: snarl runs a collapsed node's body and shifts it up under the
    // header's clip, where the graph and its point handles were laid out and still answered the
    // pointer.
    if !super::simple_bodies::node_open(snarl, node_id) {
        return;
    }
    let column = flow;
    trace_rect(ui, node_id, "body_column", column);
    let graph_bottom = column.bottom() - rows_h;
    let graph_h = (graph_bottom - column.top()).max(MIN_GRAPH_H.min(column.height()));
    let graph = egui::Rect::from_min_size(column.min, egui::vec2(column.width(), graph_h));
    let sustain_top = graph.bottom();
    let sustain_row = egui::Rect::from_min_size(egui::pos2(column.left(), sustain_top), egui::vec2(column.width(), row_h));

    // ── The graph ──
    let bg_resp = ui.allocate_rect(graph, egui::Sense::click());
    paint_envelope_curve_graph(node_id, ui, snarl, graph, bg_resp.clone(), None);
    crate::touch::context_menu(&bg_resp, |ui| {
        curve_context_menu(ui, node_id, snarl, None);
    });
    exposable.push(("curve", graph));
    exposable.push(("sustain_row", sustain_row));
    trace_rect(ui, node_id, "env_curve", graph);

    // ── Sustain: the level the curve holds at, across the graph's width ──
    {
        let mut child = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(sustain_row)
                .layout(egui::Layout::left_to_right(egui::Align::Center))
                .id_salt("env_sustain"),
        );
        child.spacing_mut().item_spacing.x = 4.0;
        let label = child.label(egui::RichText::new("Sustain").small().weak());
        // Snaps to the nearest control point: the level is one of the curve's points, not a
        // free value, and the painter may have rewritten it this frame.
        //
        // The rail runs up to its own number. Its width used to be worked out from a guessed
        // 62 points of label and a guessed 58 of reading: "Sustain" is 28 points in the font it
        // is painted in and the reading is 18, so the track stopped 79 points short of the `30%`
        // at the row's right edge - a strip of bare frame in the middle of the one control the
        // width was bought for. Measured off what is actually on the row now.
        const SUSTAIN_VALUE_W: f32 = 34.0;
        let slider_w = (sustain_row.width()
            - label.rect.width()
            - 3.0 * child.spacing().item_spacing.x
            - SUSTAIN_VALUE_W)
            .max(40.0);
        let (slider_rect, _) = child.allocate_exact_size(egui::vec2(
            slider_w,
            row_h,
        ), egui::Sense::hover());
        trace_rect(ui, node_id, "env_sustain_slider", slider_rect);
        let sustain_active = hold;
        let slider = egui::Slider::new(&mut sustain, 0.0..=1.0)
            .show_value(false)
            .clamping(egui::SliderClamping::Always);
        let sr = {
            let mut sc = ui.new_child(
                egui::UiBuilder::new()
                    .max_rect(slider_rect)
                    .layout(egui::Layout::left_to_right(egui::Align::Center))
                    .id_salt("env_sustain_slider"),
            );
            sc.spacing_mut().slider_width = slider_w;
            sc.add_enabled(sustain_active, slider)
        };
        if sr.changed() {
            if !pts_x.is_empty() {
                sustain = pts_x.iter().copied()
                    .min_by(|a, b| (a - sustain).abs().partial_cmp(&(b - sustain).abs()).unwrap())
                    .unwrap_or(sustain);
            }
            sustain_changed = true;
        }
        let value_rect = egui::Rect::from_min_size(
            egui::pos2(sustain_row.right() - SUSTAIN_VALUE_W, sustain_row.top()),
            egui::vec2(SUSTAIN_VALUE_W, row_h),
        );
        ui.new_child(
            egui::UiBuilder::new()
                .max_rect(value_rect)
                .layout(egui::Layout::right_to_left(egui::Align::Center))
                .id_salt("env_sustain_value"),
        )
        .label(egui::RichText::new(format!("{:.0}%", sustain * 100.0)).small().weak());
        trace_rect(ui, node_id, "env_sustain_value", value_rect);
        let _ = sr.on_hover_text(if sustain_active {
            "Where the curve holds while the trigger is held. It sits on one of the curve's points."
        } else {
            "Hold is off, so the curve runs through this point instead of stopping at it."
        });
    }
    trace_rect(ui, node_id, "env_sustain", sustain_row);

    // ── The setting bands ──
    let painter = ui.painter_at(column);
    let visuals = ui.visuals().clone();
    let sep_colour = visuals.widgets.inactive.bg_fill;
    for b in 0..bands {
        let top = sustain_row.bottom() + b as f32 * band_h;
        let band = egui::Rect::from_min_size(egui::pos2(column.left(), top), egui::vec2(column.width(), band_h));
        let first = b * per_band;
        let last = (first + per_band).min(group_min.len());
        let n = last - first;
        let sep_total = if n > 1 { (n as f32 - 1.0) * (SEP_W + GAP) } else { 0.0 };
        // Shared in proportion to what each row asks for, and a row always spans the band: a
        // group alone in one gets the whole width, because the graph, the sustain rail and the
        // length all span the module and a row that stops short reads as broken. What a group
        // spends the width on is its own business: the length pill and the two grid numbers
        // take it, the squares between them do not.
        let wants: Vec<f32> = (first..last).map(|gi| group_min[gi]).collect();
        let want_sum: f32 = wants.iter().sum::<f32>().max(1.0);
        let avail = (band.width() - sep_total).max(0.0);
        let mut x = band.left();
        for (slot, gi) in (first..last).enumerate() {
            let each = (avail * wants[slot] / want_sum).max(48.0);
            let cell = egui::Rect::from_min_size(egui::pos2(x, band.top()), egui::vec2(each, band_h));
            match gi {
                // The time the envelope takes, and the unit it's counted in: a length in ms or
                // seconds, or how often it repeats in hertz. Switching unit keeps the length the
                // same rather than the number.
                0 => {
                    // Three two-letter units. At 30 a cell the strip was the wider half of
                    // the row, counting a length of up to seven digits into the other half.
                    let strip_w = 3.0 * 24.0 + 2.0 * 4.0;
                    let strip = egui::Rect::from_min_size(cell.min, egui::vec2(strip_w, band_h));
                    let old_tb = timebase.clone();
                    {
                        let mut child = ui.new_child(
                            egui::UiBuilder::new()
                                .max_rect(strip)
                                .layout(egui::Layout::left_to_right(egui::Align::Center))
                                .id_salt("env_timebase"),
                        );
                        child.spacing_mut().item_spacing.x = 0.0;
                        changed |= kit::strip(
                            &mut child,
                            strip_w,
                            &mut timebase,
                            &[
                                ("ms".to_string(), "ms", "The length of one pass, in milliseconds"),
                                ("s".to_string(), "s", "In seconds"),
                                ("hz".to_string(), "Hz", "How often it repeats, in hertz"),
                            ],
                        );
                    }
                    if timebase != old_tb {
                        let period_ms: f32 = match old_tb.as_str() {
                            "s" => time_mul * 1000.0,
                            "hz" => if time_mul > 0.0 { 1000.0 / time_mul } else { 1000.0 },
                            _ => time_mul,
                        };
                        time_mul = match timebase.as_str() {
                            "s" => (period_ms / 1000.0).clamp(0.001, 2000.0),
                            "hz" => (1000.0 / period_ms.max(0.001)).clamp(0.001, 2000.0),
                            _ => period_ms.clamp(0.001, 2000.0),
                        };
                        changed = true;
                    }
                    let value = egui::Rect::from_min_max(
                        egui::pos2(strip.right() + 4.0, cell.top()),
                        cell.max,
                    );
                    let mut child = ui.new_child(
                        egui::UiBuilder::new()
                            .max_rect(value)
                            .layout(egui::Layout::right_to_left(egui::Align::Center))
                            .id_salt("env_time_value"),
                    );
                    // When the Time pin is wired the box is disabled and shows the value the
                    // wire is driving: a drag here would be a lie.
                    let live_time = snarl
                        .get_node(node_id)
                        .and_then(|n| n.extra.last_signals.get(3))
                        .and_then(|s| match s {
                            Some(Signal::Float(f)) => Some(*f),
                            _ => None,
                        });
                    // The number, not the unit strip, is what a widened Envelope should spend
                    // its width on: it is the value being read, and at the old fixed box a
                    // five-second envelope and a five-millisecond one showed the same number of
                    // digits in the same space. It is a pill like every other setting, so it
                    // drags and takes typing.
                    let shown = envelope_time_text(time_mul, &timebase);
                    let pill_band = egui::Rect::from_center_size(cell.center(), egui::vec2(ctrl_h, ctrl_h));
                    let pill = egui::Rect::from_min_max(
                        egui::pos2(strip.right() + 4.0, pill_band.top()),
                        egui::pos2(cell.right() - 4.0, pill_band.bottom()),
                    );
                    trace_rect(ui, node_id, "env_time_pill", pill);
                    if time_wired {
                        if let Some(f) = live_time {
                            time_mul = f;
                        }
                        super::simple_bodies::setting_pill(&mut child, "env_time", pill, "", &envelope_time_text(time_mul, &timebase), 0.0);
                    } else {
                        // The step a drag takes, and the clamp: milliseconds count whole, the
                        // other two bases are small numbers either side of 1.
                        let (lo, hi, spd) = match timebase.as_str() {
                            "s" => (0.001f32, 2000.0, 0.001),
                            "hz" => (0.001, 2000.0, 0.001),
                            _ => (0.001, 2000.0, 1.0),
                        };
                        let (resp, typed) = super::simple_bodies::value_pill_number(
                            &mut child,
                            node_id,
                            "env_time",
                            pill,
                            "",
                            &shown,
                            0.0,
                            lo as f64..=hi as f64,
                        );
                        if let Some(v) = typed {
                            time_mul = v as f32;
                            changed = true;
                        } else if resp.dragged() {
                            time_mul = (time_mul + resp.drag_delta().x * spd).clamp(lo, hi);
                            changed = true;
                        }
                        resp.on_hover_text("How long the pass takes, in the unit chosen beside it. Drag to change, double-click to type.");
                    }
                    exposable.push(("time_row", cell));
                    trace_rect(ui, node_id, "env_time", cell);
                }
                // The three behaviours, which combine: run to the sustain point and wait,
                // ping-pong, or repeat the segment. None of them checked is a one-shot.
                1 => {
                    // Drawn rather than spelled: at one block per row the words are what holds
                    // this group wide, and each shape is the thing the switch does. The word is
                    // in the tooltip, with the sentence that explains it.
                    let labels = [
                        (
                            super::glyphs::Glyph::Hold,
                            &mut hold as &mut bool,
                            "Hold: run to the sustain point and hold there while the trigger is held",
                        ),
                        (
                            super::glyphs::Glyph::Bounce,
                            &mut bounce,
                            "Bounce: forward while held, back to the start on release",
                        ),
                        (super::glyphs::Glyph::Loop, &mut loopf, "Loop: repeat the active segment while the trigger is held"),
                    ];
                    // Three squares, not three planks. The cell used to be divided by three, so
                    // in a default-width Envelope each button came out 69 points across with a
                    // 16-point glyph lost in the middle of it. The squares stay the height of
                    // the row; what a wider row buys them is air, capped so the three still read
                    // as one control, and the group sits in the middle of its cell.
                    //
                    // Each wears its plate switched off as well: a word with no plate still
                    // reads as something to press, a drawing with none reads as decoration.
                    let gap = ((cell.width() - 3.0 * ctrl_h) / 2.0).clamp(CELL_GAP, 12.0);
                    let group_w = (3.0 * ctrl_h + 2.0 * gap).min(cell.width());
                    let box_rect = egui::Rect::from_min_size(
                        egui::pos2(cell.center().x - group_w * 0.5, cell.top()),
                        egui::vec2(group_w, band_h),
                    );
                    let mut child = ui.new_child(
                        egui::UiBuilder::new()
                            .max_rect(box_rect)
                            .layout(egui::Layout::left_to_right(egui::Align::Center))
                            .id_salt("env_modes"),
                    );
                    child.spacing_mut().item_spacing.x = 0.0;
                    for (i, (glyph, flag, hover)) in labels.into_iter().enumerate() {
                        if i > 0 {
                            child.add_space(gap);
                        }
                        let before = *flag;
                        let on = *flag;
                        let resp = child.add_sized(
                            [ctrl_h, ctrl_h],
                            egui::Button::selectable(on, "").frame_when_inactive(true),
                        );
                        trace_rect(ui, node_id, &format!("env_mode_{i}"), resp.rect);
                        super::glyphs::paint(
                            child.painter(),
                            resp.rect,
                            glyph,
                            if on {
                                ui.visuals().strong_text_color()
                            } else {
                                ui.visuals().text_color().linear_multiply(0.75)
                            },
                        );
                        let clicked = resp.on_hover_text(hover).clicked();
                        if clicked {
                            *flag = !before;
                            changed = true;
                        }
                    }
                exposable.push(("mode_row", cell));
                trace_rect(ui, node_id, "env_mode", cell);
                }
                // The grid the points snap to: divisions across time and across value, as two
                // pills - the letter on the left, the count on the right, dragged or typed -
                // with the snap's magnet as a square at the row's right edge. These were bare
                // `DragValue`s with " H" and " V" stuck on the end: two widgets in a style
                // nothing else on the module wears, hugging the left of a row that was often
                // twice as wide as they were, and a snap button handed whatever was left over -
                // 111 points of plank at the widest frame measured here.
                _ => {
                    let mut child = ui.new_child(
                        egui::UiBuilder::new()
                            .max_rect(cell)
                            .layout(egui::Layout::left_to_right(egui::Align::Center))
                            .id_salt("env_grid"),
                    );
                    let pill_w = ((cell.width() - ctrl_h - 2.0 * CELL_GAP) / 2.0).max(46.0);
                    let top = cell.center().y - ctrl_h * 0.5;
                    let rect_x = egui::Rect::from_min_size(
                        egui::pos2(cell.left(), top),
                        egui::vec2(pill_w, ctrl_h),
                    );
                    let rect_v = egui::Rect::from_min_size(
                        egui::pos2(cell.left() + pill_w + CELL_GAP, top),
                        egui::vec2(pill_w, ctrl_h),
                    );
                    let mag = egui::Rect::from_min_size(
                        egui::pos2(cell.right() - ctrl_h, top),
                        egui::vec2(ctrl_h, ctrl_h),
                    );
                    let rows = [
                        (
                            &mut gx_f as &mut f64,
                            rect_x,
                            "env_grid_x",
                            "H",
                            "Grid divisions across the envelope's time. A dragged point settles on one of these when Snap is on.",
                        ),
                        (
                            &mut gy_f,
                            rect_v,
                            "env_grid_y",
                            "V",
                            "Grid divisions across the envelope's value, from silence to full.",
                        ),
                    ];
                    for (value, rect, salt, letter, hover) in rows {
                        let shown = format!("{}", value.round() as i64);
                        let (resp, typed) = super::simple_bodies::value_pill_number(
                            &mut child,
                            node_id,
                            salt,
                            rect,
                            letter,
                            &shown,
                            16.0,
                            1.0..=20.0,
                        );
                        if let Some(v) = typed {
                            *value = v.round().clamp(1.0, 20.0);
                            changed = true;
                        } else if resp.dragged() {
                            *value = (*value + f64::from(resp.drag_delta().x) * 0.2).round().clamp(1.0, 20.0);
                            changed = true;
                        }
                        resp.on_hover_text(hover);
                    }
                    let before = snap_on;
                    let snap_resp = child.put(
                        mag,
                        egui::Button::selectable(snap_on, "")
                            .frame_when_inactive(true)
                            .min_size(egui::vec2(ctrl_h, ctrl_h)),
                    );
                    super::glyphs::paint(
                        child.painter(),
                        snap_resp.rect,
                        super::glyphs::Glyph::Magnet,
                        if snap_on {
                            ui.visuals().strong_text_color()
                        } else {
                            ui.visuals().text_color().linear_multiply(0.75)
                        },
                    );
                    let clicked = snap_resp
                        .on_hover_text("Snap: dragged points settle on the nearest grid intersection")
                        .clicked();
                    if clicked {
                        snap_on = !before;
                        changed = true;
                    }
                exposable.push(("grid_row", cell));
                trace_rect(ui, node_id, "env_grid", cell);
                // The two number pills and the snap square, traced for the rig: whether a row
                // of settings spends its width on its controls or on air between them is
                // invisible to a size test, and these rects are what make it an assertion.
                trace_rect(ui, node_id, "env_grid_x", rect_x);
                trace_rect(ui, node_id, "env_grid_y", rect_v);
                trace_rect(ui, node_id, "env_snap", mag);
                }
            }
            x = cell.right() + SEP_W + GAP;
            if slot + 1 < n {
                let sx = x - (SEP_W + GAP) * 0.5;
                painter.line_segment(
                    [egui::pos2(sx, band.top() + 4.0), egui::pos2(sx, band.bottom() - 4.0)],
                    egui::Stroke::new(1.0_f32, sep_colour),
                );
            }
        }
    }

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
        }
    }

    for (id, r) in exposable {
        register_exposable_element(ui, node_id, id, r);
    }
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
    crate::touch::context_menu(&bg_for_menu, |ui| {
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

    // Two nav fields: the whole timebase segment (one Enum) then the value box.
    // The three unit buttons publish as ONE rect (their union) so the gamepad
    // field-index (0=unit, 1=value) lines up 1:1 with `nav_element_fields`.
    let mut unit_rect = egui::Rect::NOTHING;
    let mut value_rect = egui::Rect::NOTHING;
    ui.horizontal(|ui| {
        let old_tb = timebase.clone();
        let r_ms = ui.selectable_value(&mut timebase, "ms".into(), egui::RichText::new("ms").small());
        unit_rect = unit_rect.union(r_ms.rect);
        let r_s  = ui.selectable_value(&mut timebase, "s".into(),  egui::RichText::new("s").small());
        unit_rect = unit_rect.union(r_s.rect);
        let r_hz = ui.selectable_value(&mut timebase, "hz".into(), egui::RichText::new("Hz").small());
        unit_rect = unit_rect.union(r_hz.rect);
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
        value_rect = r.rect;
        if !time_wired { time_mul = shown; changed |= r.changed(); }
    });
    publish_nav_field_rects(ui, inner_id, &[unit_rect, value_rect]);
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
        // Read-only %: NOT a nav field, so its rect is not published (field rects
        // must be 1:1 with `nav_element_fields`, editable controls only).
        ui.label(egui::RichText::new(pct_lbl).small().weak());
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
        // "Grid" caption is NOT a nav field — don't publish its rect (field rects
        // must be 1:1 with `nav_element_fields`: grid_x, grid_y, snap).
        ui.label(egui::RichText::new("Grid").small().weak());
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
#[cfg(test)]
mod a_pinned_graph_copy_follows_the_module_it_came_from {
    use super::graph_shown;

    // The three display switches of a pinned graph are per copy and optional. Unset, the copy
    // looks exactly like the module on the canvas - which is what a copy saved before these
    // switches existed, or one nobody touched, has to keep doing. Set, it wins both ways.
    #[test]
    fn unset_follows_the_module() {
        assert!(graph_shown(None, true));
        assert!(!graph_shown(None, false));
    }

    #[test]
    fn a_copy_can_force_it_either_way() {
        assert!(graph_shown(Some(true), false));
        assert!(!graph_shown(Some(false), true));
    }
}