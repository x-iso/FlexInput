//! Scope displays: Readout, Oscilloscope, Vectorscope, Trigger Scope
//! bodies + their pinned display/control renderers.

use super::*;

// ── Readout ───────────────────────────────────────────────────────────────────

pub(crate) fn render_readout_value(
    inner_id: NodeId,
    ui: &mut egui::Ui,
    snarl: &mut Snarl<NodeData>,
    container: egui::Vec2,
) {
    let sig = snarl.get_node(inner_id)
        .and_then(|n| n.extra.last_signals.first().copied().flatten());
    let text = match sig {
        Some(Signal::Float(f)) => format!("{f:.4}"),
        Some(Signal::Bool(b))  => if b { "true".into() } else { "false".into() },
        Some(Signal::Vec2(v))  => format!("({:.3}, {:.3})", v.x, v.y),
        Some(Signal::Vec4(v))  => format!("({:.3}, {:.3}, {:.3}, {:.3})", v.x, v.y, v.z, v.w),
        Some(Signal::Int(i))   => format!("{i}"),
        None                   => "—".into(),
    };
    let font = container.y.clamp(10.0, 64.0) * 0.55;
    ui.add_sized(
        [container.x, container.y.max(18.0)],
        egui::Label::new(egui::RichText::new(text).monospace().size(font)),
    );
}

// ── Oscilloscope / Vectorscope ────────────────────────────────────────────────

pub(crate) fn render_oscilloscope_display(
    inner_id: NodeId,
    ui: &mut egui::Ui,
    snarl: &mut Snarl<NodeData>,
    container: egui::Vec2,
    graph_ov: Option<&crate::canvas::node::PinGraphOverride>,
) {
    // Conditional vsync — see scope_should_request_repaint above.
    let _ = scope_should_request_repaint(inner_id, snarl, ui.ctx());
    let (history, n_channels, win_ms, osc_scale, osc_auto, osc_uni) = snarl.get_node(inner_id).map(|n| {
        let win = n.params.get("osc_win_ms").and_then(|v| v.as_f64()).unwrap_or(200.0).clamp(10.0, 10_000.0) as f32;
        let sc  = n.params.get("osc_scale") .and_then(|v| v.as_f64()).unwrap_or(1.0).max(0.001) as f32;
        let au  = n.params.get("osc_auto")  .and_then(|v| v.as_bool()).unwrap_or(false);
        let uni = n.params.get("osc_uni")   .and_then(|v| v.as_bool()).unwrap_or(false);
        (n.extra.history.clone(), n.inputs.len().max(1), win, sc, au, uni)
    }).unwrap_or_default();

    let osc_win = (win_ms / 1000.0 * current_sample_rate() as f32) as usize;
    let n_total = history.len();
    let start   = n_total.saturating_sub(osc_win);
    let visible: Vec<Vec<Option<f32>>> = history.iter().skip(start).cloned().collect();
    let n = visible.len();

    let eff_scale = if osc_auto {
        let max_v = visible.iter()
            .flat_map(|s| s.iter().filter_map(|v| *v))
            .map(|v: f32| v.abs())
            .fold(0.0f32, f32::max);
        if max_v > 0.0 { max_v } else { 1.0 }
    } else { osc_scale };

    let avail = egui::vec2(container.x.max(40.0), container.y.max(24.0));
    let (rect, _) = ui.allocate_exact_size(avail, egui::Sense::hover());
    let painter = ui.painter_at(rect);
    let (graph_bg, graph_outline) = graph_chrome(graph_ov);
    painter.rect_filled(rect, 2.0, graph_bg);
    let (grid_faint, grid_axis) = graph_grid_colors(graph_ov);

    for i in 1..4 {
        let y = if osc_uni {
            rect.bottom() - rect.height() * (i as f32 / 4.0)
        } else {
            rect.top() + rect.height() * (i as f32 / 4.0)
        };
        let is_zero = !osc_uni && i == 2;
        painter.line_segment(
            [egui::pos2(rect.left(), y), egui::pos2(rect.right(), y)],
            egui::Stroke::new(
                if is_zero { 1.0 } else { 0.5 },
                if is_zero { grid_axis } else { grid_faint },
            ),
        );
    }
    if osc_uni {
        painter.line_segment(
            [egui::pos2(rect.left(), rect.bottom()), egui::pos2(rect.right(), rect.bottom())],
            egui::Stroke::new(1.0, grid_axis),
        );
    }

    let pixel_budget = (rect.width().ceil() as usize).max(2);
    let n_ch_inner = if n > 0 { visible[0].len() } else { 0 };
    let display: Vec<Vec<Option<f32>>> = if n <= pixel_budget {
        visible.clone()
    } else {
        (0..pixel_budget).map(|i| {
            let lo = i * n / pixel_budget;
            let hi = ((i + 1) * n / pixel_budget).min(n);
            (0..n_ch_inner).map(|ch| {
                let vals: Vec<f32> = visible[lo..hi].iter()
                    .filter_map(|s| s.get(ch).copied().flatten())
                    .collect();
                if vals.is_empty() { None } else { Some(vals.iter().sum::<f32>() / vals.len() as f32) }
            }).collect()
        }).collect()
    };
    let nd = display.len();
    if nd >= 2 {
        for ch in 0..n_channels {
            let pts: Vec<egui::Pos2> = display.iter().enumerate().filter_map(|(i, s)| {
                s.get(ch).copied().flatten().map(|v| {
                    let x = rect.left() + (i as f32 / (nd - 1) as f32) * rect.width();
                    let norm = v / eff_scale;
                    let y = if osc_uni {
                        rect.bottom() - norm.clamp(0.0, 1.0) * rect.height() * 0.92
                    } else {
                        rect.center().y - norm.clamp(-1.0, 1.0) * rect.height() * 0.45
                    };
                    egui::pos2(x, y)
                })
            }).collect();
            let ch_col = graph_channel_color(graph_ov, ch);
            for w in pts.windows(2) {
                painter.line_segment([w[0], w[1]], egui::Stroke::new(1.5, ch_col));
            }
        }
    }
    if let Some(stroke) = graph_outline {
        painter.rect_stroke(rect, 2.0, stroke, egui::StrokeKind::Inside);
    }
    request_repaint_throttled(ui.ctx());
    let _ = inner_id;
}

/// Bare oscilloscope controls row: Win slider + Scale (with Auto fallback) +
/// Bi/Uni selector. Same controls as the editor body but as a free widget.
pub(crate) fn render_oscilloscope_controls(
    inner_id: NodeId,
    ui: &mut egui::Ui,
    snarl: &mut Snarl<NodeData>,
    container: egui::Vec2,
) {
    let (mut win_ms, mut sc, mut au, mut uni, eff_scale) = snarl.get_node(inner_id).map(|n| {
        let win = n.params.get("osc_win_ms").and_then(|v| v.as_f64()).unwrap_or(200.0).clamp(10.0, 10_000.0) as f32;
        let s   = n.params.get("osc_scale") .and_then(|v| v.as_f64()).unwrap_or(1.0).max(0.001) as f32;
        let a   = n.params.get("osc_auto")  .and_then(|v| v.as_bool()).unwrap_or(false);
        let u   = n.params.get("osc_uni")   .and_then(|v| v.as_bool()).unwrap_or(false);
        (win, s, a, u, s)
    }).unwrap_or((200.0, 1.0, false, false, 1.0));

    let mut changed = false;
    let mut fr = [egui::Rect::NOTHING; 4];
    ui.set_max_width(container.x);
    apply_widget_scale(ui, container, egui::vec2(360.0, 22.0));
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new("Win").weak());
        // Flexible element: the Win slider absorbs surplus container width.
        ui.spacing_mut().slider_width = pin_flex_width(ui, container, 70.0);
        let r = ui.add(egui::Slider::new(&mut win_ms, 10.0f32..=10_000.0)
            .logarithmic(true).show_value(false));
        fr[0] = r.rect; changed |= r.changed();
        let lbl = if win_ms >= 1000.0 { format!("{:.1}s", win_ms / 1000.0) } else { format!("{:.0}ms", win_ms) };
        ui.label(egui::RichText::new(lbl).weak());
        ui.separator();
        ui.label(egui::RichText::new("Scale").weak());
        if au {
            fr[1] = ui.label(egui::RichText::new(format!("{:.3}", eff_scale)).weak()).rect;
        } else {
            let r = ui.add(egui::DragValue::new(&mut sc).speed(0.01)
                .range(0.001f32..=100.0).max_decimals(3));
            fr[1] = r.rect; changed |= r.changed();
        }
        let was_au = au;
        fr[2] = ui.checkbox(&mut au, egui::RichText::new("Auto")).rect;
        changed |= au != was_au;
        ui.separator();
        let was_uni = uni;
        let rb = ui.selectable_value(&mut uni, false, egui::RichText::new("Bi"));
        let ru = ui.selectable_value(&mut uni, true,  egui::RichText::new("Uni"));
        fr[3] = rb.rect.union(ru.rect);
        changed |= uni != was_uni;
    });
    publish_nav_field_rects(ui, inner_id, &fr);
    if changed {
        if let Some(node) = snarl.get_node_mut(inner_id) {
            if let Some(n) = Number::from_f64(win_ms as f64) { node.params.insert("osc_win_ms".into(), Value::Number(n)); }
            if let Some(n) = Number::from_f64(sc as f64)     { node.params.insert("osc_scale".into(),  Value::Number(n)); }
            node.params.insert("osc_auto".into(), Value::Bool(au));
            node.params.insert("osc_uni".into(),  Value::Bool(uni));
        }
    }
}

pub(crate) fn render_vectorscope_display(
    inner_id: NodeId,
    ui: &mut egui::Ui,
    snarl: &mut Snarl<NodeData>,
    container: egui::Vec2,
    graph_ov: Option<&crate::canvas::node::PinGraphOverride>,
) {
    // Conditional vsync — see scope_should_request_repaint above.
    let _ = scope_should_request_repaint(inner_id, snarl, ui.ctx());
    // Visualization tail length — bounded so we don't pay for samples
    // that won't be drawn. History buffer itself can be much longer
    // (20k entries by default).
    const MAX_VS_TRAIL: usize = 600;
    // Pull only the tail we actually render plus channel/last-signal
    // metadata. Skipping the full `history.clone()` avoids cloning a
    // VecDeque of up to 20k Vec<Option<f32>> entries every frame.
    let (history_tail, n_channels, last_signals) = snarl.get_node(inner_id)
        .map(|n| {
            let hist = &n.extra.history;
            let skip = hist.len().saturating_sub(MAX_VS_TRAIL);
            let tail: Vec<Vec<Option<f32>>> = hist.iter().skip(skip).cloned().collect();
            (tail, n.inputs.len().max(1), n.extra.last_signals.clone())
        })
        .unwrap_or_default();

    let side = container.x.min(container.y).max(40.0);
    let (rect, _) = ui.allocate_exact_size(egui::Vec2::splat(side), egui::Sense::hover());
    let painter = ui.painter_at(rect);
    let (graph_bg, graph_outline) = graph_chrome(graph_ov);
    painter.rect_filled(rect, 2.0, graph_bg);
    let (grid_faint, grid_axis) = graph_grid_colors(graph_ov);
    painter.line_segment(
        [egui::pos2(rect.center().x, rect.top()), egui::pos2(rect.center().x, rect.bottom())],
        egui::Stroke::new(0.5, grid_axis),
    );
    painter.line_segment(
        [egui::pos2(rect.left(), rect.center().y), egui::pos2(rect.right(), rect.center().y)],
        egui::Stroke::new(0.5, grid_axis),
    );
    painter.circle_stroke(rect.center(), rect.width().min(rect.height()) * 0.45,
        egui::Stroke::new(0.5, grid_faint));

    // Trail rendering: instead of one circle per sample (was up to 2000
    // painter calls per channel per frame), we emit a small number of
    // contiguous polyline segments with constant alpha per segment. The
    // alpha steps from low (oldest) to high (newest) so the line looks
    // like a fading trail — and the polyline shows actual motion rather
    // than a static dot cloud. Cost drops from O(N) painter shapes to
    // O(SEGMENTS) regardless of trail length.
    //
    // 12 chunks looks smooth at 60 fps with a few hundred samples of
    // trail — perceivable fade gradient without visible banding.
    const FADE_CHUNKS: usize = 12;
    let nt = history_tail.len();
    let center = rect.center();
    let hx = rect.width() * 0.45;
    let hy = rect.height() * 0.45;
    for ch in 0..n_channels {
        let col = graph_channel_color(graph_ov, ch);
        let xi = ch * 2;
        let yi = ch * 2 + 1;

        // Pre-project the trail into screen space, dropping samples where
        // either x or y is missing. We need an indexed list so we know
        // each surviving point's "age" within the original trail (which
        // drives the per-chunk alpha).
        let mut pts: Vec<(usize, egui::Pos2)> = Vec::with_capacity(nt);
        for (idx, sample) in history_tail.iter().enumerate() {
            if let (Some(x), Some(y)) = (
                sample.get(xi).copied().flatten(),
                sample.get(yi).copied().flatten(),
            ) {
                let px = center.x + x.clamp(-1.0, 1.0) * hx;
                let py = center.y - y.clamp(-1.0, 1.0) * hy;
                pts.push((idx, egui::pos2(px, py)));
            }
        }

        // Slice the projected polyline into FADE_CHUNKS roughly equal
        // chunks, each rendered as one painter.line() call with a fixed
        // alpha derived from the chunk's age. Adjacent chunks share their
        // boundary point so the visual line is continuous.
        if pts.len() >= 2 {
            let per_chunk = (pts.len() / FADE_CHUNKS).max(1);
            for c in 0..FADE_CHUNKS {
                let lo = c * per_chunk;
                let hi = ((c + 1) * per_chunk + 1).min(pts.len()); // +1 to share boundary
                if hi <= lo + 1 { continue; }
                // Age 0.0 = oldest chunk, 1.0 = newest. Alpha curve
                // matches the previous dot-cloud's `(idx/nt)*200 + 35`
                // intensity ramp so the visual weight feels similar.
                let age = c as f32 / (FADE_CHUNKS - 1).max(1) as f32;
                let alpha = (age * 200.0) as u8 + 35;
                let stroke_color = Color32::from_rgba_unmultiplied(
                    col.r(), col.g(), col.b(), alpha,
                );
                let chunk_pts: Vec<egui::Pos2> = pts[lo..hi].iter().map(|(_, p)| *p).collect();
                painter.line(chunk_pts, egui::Stroke::new(1.25, stroke_color));
            }
        }

        // Current value head — a small filled+stroked circle so the user
        // can pinpoint the live sample even when the trail dims away.
        if let Some(Some(Signal::Vec2(v))) = last_signals.get(ch) {
            let px = center.x + v.x.clamp(-1.0, 1.0) * hx;
            let py = center.y - v.y.clamp(-1.0, 1.0) * hy;
            painter.circle_filled(egui::pos2(px, py), 4.0, col);
            painter.circle_stroke(egui::pos2(px, py), 4.0,
                egui::Stroke::new(1.0, Color32::from_gray(100)));
        }
    }
    // Only force a repaint while the trail still has live samples or
    // the current frame's signals contain a Vec2. Idle vectorscope
    // (no history, no live input) is static.
    let has_trail = nt > 0;
    let has_live = last_signals.iter().any(|s| matches!(s, Some(Signal::Vec2(_))));
    if has_trail || has_live { request_repaint_throttled(ui.ctx()); }
    if let Some(stroke) = graph_outline {
        painter.rect_stroke(rect, 2.0, stroke, egui::StrokeKind::Inside);
    }
    let _ = inner_id;
}



pub(crate) fn show_readout_body(node_id: NodeId, ui: &mut egui::Ui, snarl: &mut Snarl<NodeData>) {
    let sig = snarl
        .get_node(node_id)
        .and_then(|n| n.extra.last_signals.first().copied().flatten());

    use flexinput_core::Signal;
    let text = match sig {
        Some(Signal::Float(f)) => format!("{f:.4}"),
        Some(Signal::Bool(b))  => if b { "true".into() } else { "false".into() },
        Some(Signal::Vec2(v))  => format!("({:.3}, {:.3})", v.x, v.y),
        Some(Signal::Vec4(v))  => format!("({:.3}, {:.3}, {:.3}, {:.3})", v.x, v.y, v.z, v.w),
        Some(Signal::Int(i))   => format!("{i}"),
        None                   => "—".into(),
    };
    let resp = ui.add_sized(
        [120.0, 24.0],
        egui::Label::new(egui::RichText::new(text).monospace().size(14.0)),
    );
    register_exposable_element(ui, node_id, "value", resp.rect);
}

/// Decide whether a scope-like module should bypass the user's base
/// Repaint rate and force vsync this frame. Returns true when the input
/// signal looks like it changed since last frame, OR when we haven't
/// repainted in a while (so the scope's own decay/sweep animations
/// catch up even on an idle input). Hashes are stashed on the node's
/// `NodeExtra` so the next frame can compare.
///
/// The hash is FNV-1a over the f32 bit patterns of every channel's
/// current sample — cheap to compute and zero allocations.
///
/// `MAX_IDLE_FRAMES` is the longest stretch we'll skip repaints during
/// a steady signal; at 30 Hz that's 1 s, plenty fast for the human
/// to perceive an updated reading after they touch the input again.
pub(crate) fn scope_should_request_repaint(
    node_id: NodeId,
    snarl: &mut Snarl<NodeData>,
    ctx: &egui::Context,
) -> bool {
    const MAX_IDLE_FRAMES: u32 = 30;
    let Some(node) = snarl.get_node_mut(node_id) else { return false; };
    let mut h: u64 = 0xcbf29ce484222325;
    for sig in &node.extra.last_signals {
        let f = match sig {
            Some(Signal::Float(v)) => *v,
            Some(Signal::Bool(b))  => if *b { 1.0 } else { 0.0 },
            Some(Signal::Vec2(v))  => v.x + v.y * 1.3137,
            _ => 0.0,
        };
        h ^= f.to_bits() as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    let changed = h != node.extra.prev_input_hash;
    node.extra.prev_input_hash = h;
    if changed {
        node.extra.idle_frames_since_change = 0;
        request_repaint_throttled(ctx);
        true
    } else {
        node.extra.idle_frames_since_change =
            node.extra.idle_frames_since_change.saturating_add(1);
        if node.extra.idle_frames_since_change < MAX_IDLE_FRAMES {
            request_repaint_throttled(ctx);
            true
        } else {
            false
        }
    }
}

pub(crate) fn show_oscilloscope_body(node_id: NodeId, inputs: &[InPin], ui: &mut egui::Ui, snarl: &mut Snarl<NodeData>) {
    // Request vsync only while the input signal is animating (see
    // `scope_should_request_repaint` for the gate's logic). A stationary
    // scope with no input change settles to the user's base Repaint
    // rate after ~1 s; the moment a sample changes the gate re-arms.
    let _ = scope_should_request_repaint(node_id, snarl, ui.ctx());
    // ── Init params on first use ──────────────────────────────────────────────
    let needs_init = snarl.get_node(node_id).map(|n| !n.params.contains_key("osc_win_ms")).unwrap_or(false);
    if needs_init {
        if let Some(node) = snarl.get_node_mut(node_id) {
            node.params.insert("osc_win_ms".into(), serde_json::json!(200.0f64));
            node.params.insert("osc_scale".into(), serde_json::json!(1.0));
            node.params.insert("osc_auto".into(),  Value::Bool(false));
            node.params.insert("osc_uni".into(),   Value::Bool(false));
        }
    }

    // ── Read params ───────────────────────────────────────────────────────────
    let (win_ms, osc_scale, osc_auto, osc_uni) = snarl.get_node(node_id).map(|n| {
        let win = n.params.get("osc_win_ms").and_then(|v| v.as_f64()).unwrap_or(200.0).clamp(10.0, 10000.0) as f32;
        let sc  = n.params.get("osc_scale") .and_then(|v| v.as_f64()).unwrap_or(1.0).max(0.001) as f32;
        let au  = n.params.get("osc_auto")  .and_then(|v| v.as_bool()).unwrap_or(false);
        let uni = n.params.get("osc_uni")   .and_then(|v| v.as_bool()).unwrap_or(false);
        (win, sc, au, uni)
    }).unwrap_or((200.0, 1.0, false, false));
    let osc_win = (win_ms / 1000.0 * current_sample_rate() as f32) as usize;

    let history = snarl.get_node(node_id).map(|n| n.extra.history.clone()).unwrap_or_default();
    let n_channels = snarl.get_node(node_id).map(|n| n.inputs.len()).unwrap_or(1).max(1);
    let n_total = history.len();
    let start   = n_total.saturating_sub(osc_win);
    let visible: Vec<Vec<Option<f32>>> = history.iter().skip(start).cloned().collect();
    let n       = visible.len();

    // Auto-scale: max absolute value across all visible channels.
    let eff_scale = if osc_auto {
        let max_v = visible.iter()
            .flat_map(|s| s.iter().filter_map(|v| *v))
            .map(|v: f32| v.abs())
            .fold(0.0f32, f32::max);
        if max_v > 0.0 { max_v } else { 1.0 }
    } else {
        osc_scale
    };

    let mut display_rect: Option<egui::Rect> = None;
    ui.vertical(|ui| {
        egui::Resize::default()
            .id_salt(("osc", node_id))
            .default_size([240.0, 100.0])
            .min_size([60.0, 30.0])
            .show(ui, |ui| {
                let (rect, _) = ui.allocate_exact_size(ui.available_size(), egui::Sense::hover());
                display_rect = Some(rect);
                let painter = ui.painter_at(rect);
                painter.rect_filled(rect, 2.0, GRAPH_BG_DEFAULT);
                let (grid_faint, grid_axis) = graph_grid_colors(None);

                // Grid lines.
                for i in 1..4 {
                    let y = if osc_uni {
                        rect.bottom() - rect.height() * (i as f32 / 4.0)
                    } else {
                        rect.top() + rect.height() * (i as f32 / 4.0)
                    };
                    let is_zero = !osc_uni && i == 2;
                    painter.line_segment(
                        [egui::pos2(rect.left(), y), egui::pos2(rect.right(), y)],
                        egui::Stroke::new(
                            if is_zero { 1.0 } else { 0.5 },
                            if is_zero { grid_axis } else { grid_faint },
                        ),
                    );
                }
                // Baseline for uni mode.
                if osc_uni {
                    painter.line_segment(
                        [egui::pos2(rect.left(), rect.bottom()), egui::pos2(rect.right(), rect.bottom())],
                        egui::Stroke::new(1.0, grid_axis),
                    );
                }

                // Downsample to pixel budget so line count never exceeds display width.
                let pixel_budget = (rect.width().ceil() as usize).max(2);
                let n_ch_inner = if n > 0 { visible[0].len() } else { 0 };
                let display: Vec<Vec<Option<f32>>> = if n <= pixel_budget {
                    visible.clone()
                } else {
                    (0..pixel_budget).map(|i| {
                        let lo = i * n / pixel_budget;
                        let hi = ((i + 1) * n / pixel_budget).min(n);
                        (0..n_ch_inner).map(|ch| {
                            let vals: Vec<f32> = visible[lo..hi].iter()
                                .filter_map(|s| s.get(ch).copied().flatten())
                                .collect();
                            if vals.is_empty() { None } else { Some(vals.iter().sum::<f32>() / vals.len() as f32) }
                        }).collect()
                    }).collect()
                };
                let nd = display.len();

                // Signal lines.
                if nd >= 2 {
                    for ch in 0..n_channels {
                        let pts: Vec<egui::Pos2> = display.iter().enumerate().filter_map(|(i, s)| {
                            s.get(ch).copied().flatten().map(|v| {
                                let x = rect.left() + (i as f32 / (nd - 1) as f32) * rect.width();
                                let norm = v / eff_scale;
                                let y = if osc_uni {
                                    rect.bottom() - norm.clamp(0.0, 1.0) * rect.height() * 0.92
                                } else {
                                    rect.center().y - norm.clamp(-1.0, 1.0) * rect.height() * 0.45
                                };
                                egui::pos2(x, y)
                            })
                        }).collect();
                        for w in pts.windows(2) {
                            painter.line_segment([w[0], w[1]], egui::Stroke::new(1.5, MULTI_COLORS[ch % MULTI_COLORS.len()]));
                        }
                    }
                }
            });

        // ── Controls ─────────────────────────────────────────────────────────
        let mut win_ms_ctrl = win_ms;
        let mut sc      = osc_scale;
        let mut au      = osc_auto;
        let mut uni     = osc_uni;
        let mut changed = false;

        let controls_resp = ui.horizontal(|ui| {
            ui.label(egui::RichText::new("Win").small().weak());
            changed |= ui.add(egui::Slider::new(&mut win_ms_ctrl, 10.0f32..=10000.0)
                .logarithmic(true).show_value(false)).changed();
            let lbl = if win_ms_ctrl >= 1000.0 {
                format!("{:.1}s", win_ms_ctrl / 1000.0)
            } else {
                format!("{:.0}ms", win_ms_ctrl)
            };
            ui.label(egui::RichText::new(lbl).small().weak());
            ui.separator();
            ui.label(egui::RichText::new("Scale").small().weak());
            if au {
                ui.label(egui::RichText::new(format!("{:.3}", eff_scale)).small().weak());
            } else {
                changed |= ui.add(egui::DragValue::new(&mut sc).speed(0.01)
                    .range(0.001f32..=100.0).max_decimals(3)).changed();
            }
            let au_before = au;
            ui.checkbox(&mut au, egui::RichText::new("Auto").small());
            changed |= au != au_before;
            ui.separator();
            let uni_before = uni;
            ui.selectable_value(&mut uni, false, egui::RichText::new("Bi").small());
            ui.selectable_value(&mut uni, true,  egui::RichText::new("Uni").small());
            changed |= uni != uni_before;
        });
        register_exposable_element(ui, node_id, "controls", controls_resp.response.rect);

        if changed {
            if let Some(node) = snarl.get_node_mut(node_id) {
                if let Some(n) = Number::from_f64(win_ms_ctrl as f64) { node.params.insert("osc_win_ms".into(), Value::Number(n)); }
                node.params.insert("osc_auto".into(),  Value::Bool(au));
                node.params.insert("osc_uni".into(),   Value::Bool(uni));
                if let Some(n) = Number::from_f64(sc as f64) {
                    node.params.insert("osc_scale".into(), Value::Number(n));
                }
            }
        }

        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("Ch").small().weak());
            if ui.small_button("+").on_hover_text("Add channel").clicked() {
                if let Some(node) = snarl.get_node_mut(node_id) {
                    let next = node.inputs.len() + 1;
                    node.inputs.push(PinDescriptor::new(format!("ch{}", next), SignalType::Float));
                }
            }
            if n_channels > 1 && ui.small_button("−").on_hover_text("Remove channel").clicked() {
                remove_input_pin(node_id, n_channels - 1, inputs, snarl);
            }
        });
    });
    if let Some(r) = display_rect { register_exposable_element(ui, node_id, "display", r); }
}

/// Body for the Controller 3D display node: renders the connected (or manually
/// chosen) controller model, rotated live by the gyro `Orientation` quaternion
/// (input 1, a Vec4). The model is auto-detected from the connected device
/// (input 0, AutoMap) unless the `model` param overrides it.

pub(crate) fn show_vectorscope_body(node_id: NodeId, inputs: &[InPin], ui: &mut egui::Ui, snarl: &mut Snarl<NodeData>) {
    // Conditional vsync — see scope_should_request_repaint above.
    let _ = scope_should_request_repaint(node_id, snarl, ui.ctx());
    // Bounded tail clone: we only render the last MAX_VS_TRAIL samples,
    // so cloning the full 20k-entry history every frame was pure waste.
    // See `render_vectorscope_display` for the equivalent change on the
    // bare/sub-patch render path.
    const MAX_VS_TRAIL: usize = 600;
    let (history_tail, n_channels, last_signals) = snarl
        .get_node(node_id)
        .map(|n| {
            let h = &n.extra.history;
            let skip = h.len().saturating_sub(MAX_VS_TRAIL);
            let tail: Vec<Vec<Option<f32>>> = h.iter().skip(skip).cloned().collect();
            (tail, n.inputs.len().max(1), n.extra.last_signals.clone())
        })
        .unwrap_or_default();

    let mut display_rect: Option<egui::Rect> = None;
    ui.vertical(|ui| {
        // Aspect-locked square resize. Stores `side` as persisted egui memory so
        // it survives app restarts (same id scheme as the prior egui::Resize).
        let size_id = egui::Id::new(("vs_side", node_id));
        let mut side = ui
            .ctx()
            .data_mut(|d| d.get_persisted::<f32>(size_id))
            .unwrap_or(140.0)
            .max(40.0);

        let (rect, _) = ui.allocate_exact_size(egui::Vec2::splat(side), egui::Sense::hover());
        display_rect = Some(rect);

        // Drag handle in the bottom-right corner. Drives both axes from a single
        // delta so the area stays square.
        let handle_sz = 12.0;
        let handle_rect = egui::Rect::from_min_size(
            egui::pos2(rect.right() - handle_sz, rect.bottom() - handle_sz),
            egui::Vec2::splat(handle_sz),
        );
        let handle_resp = ui.interact(
            handle_rect,
            size_id.with("handle"),
            egui::Sense::click_and_drag(),
        );
        if handle_resp.hovered() || handle_resp.dragged() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeNwSe);
        }
        if handle_resp.dragged() {
            let d = handle_resp.drag_delta();
            // Use the dominant axis so diagonal drags feel natural.
            let delta = if d.x.abs() >= d.y.abs() { d.x } else { d.y };
            side = (side + delta).max(40.0);
            ui.ctx().data_mut(|d| d.insert_persisted(size_id, side));
        }

        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, 2.0, GRAPH_BG_DEFAULT);
        let (grid_faint, grid_axis) = graph_grid_colors(None);
        painter.line_segment(
            [egui::pos2(rect.center().x, rect.top()), egui::pos2(rect.center().x, rect.bottom())],
            egui::Stroke::new(0.5, grid_axis),
        );
        painter.line_segment(
            [egui::pos2(rect.left(), rect.center().y), egui::pos2(rect.right(), rect.center().y)],
            egui::Stroke::new(0.5, grid_axis),
        );
        painter.circle_stroke(rect.center(), rect.width().min(rect.height()) * 0.45,
            egui::Stroke::new(0.5, grid_faint));

        // Fading polyline trail. Replaces the per-sample circle dot
        // cloud — 600 samples ⇒ 12 polyline shapes per channel rather
        // than 600 circles. See `render_vectorscope_display` for the
        // matching change on the bare/sub-patch path.
        const FADE_CHUNKS: usize = 12;
        let nt = history_tail.len();
        let center = rect.center();
        let hx = rect.width()  * 0.45;
        let hy = rect.height() * 0.45;
        for ch in 0..n_channels {
            let col = MULTI_COLORS[ch % MULTI_COLORS.len()];
            let xi = ch * 2;
            let yi = ch * 2 + 1;

            // Project surviving samples to screen coords once.
            let mut pts: Vec<egui::Pos2> = Vec::with_capacity(nt);
            for sample in history_tail.iter() {
                if let (Some(x), Some(y)) = (
                    sample.get(xi).copied().flatten(),
                    sample.get(yi).copied().flatten(),
                ) {
                    pts.push(egui::pos2(
                        center.x + x.clamp(-1.0, 1.0) * hx,
                        center.y - y.clamp(-1.0, 1.0) * hy,
                    ));
                }
            }

            if pts.len() >= 2 {
                let per_chunk = (pts.len() / FADE_CHUNKS).max(1);
                for c in 0..FADE_CHUNKS {
                    let lo = c * per_chunk;
                    let hi = ((c + 1) * per_chunk + 1).min(pts.len()); // share boundary
                    if hi <= lo + 1 { continue; }
                    let age = c as f32 / (FADE_CHUNKS - 1).max(1) as f32;
                    let alpha = (age * 200.0) as u8 + 35;
                    let stroke_color = Color32::from_rgba_unmultiplied(
                        col.r(), col.g(), col.b(), alpha,
                    );
                    painter.line(pts[lo..hi].to_vec(),
                        egui::Stroke::new(1.25, stroke_color));
                }
            }

            // Current-position dot (filled+stroked) so the live sample
            // remains visible when the trail dims out.
            if let Some(Some(Signal::Vec2(v))) = last_signals.get(ch) {
                let px = center.x + v.x.clamp(-1.0, 1.0) * hx;
                let py = center.y - v.y.clamp(-1.0, 1.0) * hy;
                painter.circle_filled(egui::pos2(px, py), 4.0, col);
                painter.circle_stroke(egui::pos2(px, py), 4.0,
                    egui::Stroke::new(1.0, Color32::from_gray(100)));
            }
        }

        // Paint a small diagonal-line resize grip in the bottom-right corner
        // (mirrors egui's internal `paint_resize_corner_with_style`).
        {
            let grip_color = ui.style().interact(&handle_resp).fg_stroke.color;
            let grip_stroke = egui::Stroke::new(1.0, grip_color);
            let cp = handle_rect.right_bottom();
            let mut w = 2.0;
            while w <= handle_rect.width() && w <= handle_rect.height() {
                painter.line_segment(
                    [egui::pos2(cp.x - w, cp.y), egui::pos2(cp.x, cp.y - w)],
                    grip_stroke,
                );
                w += 4.0;
            }
        }

        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("Ch").small().weak());
            if ui.small_button("+").on_hover_text("Add channel").clicked() {
                if let Some(node) = snarl.get_node_mut(node_id) {
                    let next = node.inputs.len() + 1;
                    node.inputs.push(PinDescriptor::new(format!("ch{}", next), SignalType::Vec2));
                }
            }
            if n_channels > 1 && ui.small_button("−").on_hover_text("Remove channel").clicked() {
                remove_input_pin(node_id, n_channels - 1, inputs, snarl);
            }
        });
    });
    if let Some(r) = display_rect { register_exposable_element(ui, node_id, "display", r); }
}

// ── Trigger Scope ─────────────────────────────────────────────────────────────

pub(crate) fn show_trigscope_body(node_id: NodeId, inputs: &[InPin], ui: &mut egui::Ui, snarl: &mut Snarl<NodeData>) {
    // Conditional vsync — see scope_should_request_repaint above.
    let _ = scope_should_request_repaint(node_id, snarl, ui.ctx());
    let needs_init = snarl.get_node(node_id).map(|n| !n.params.contains_key("ts_win_ms")).unwrap_or(false);
    if needs_init {
        if let Some(node) = snarl.get_node_mut(node_id) {
            node.params.insert("ts_win_ms".into(), serde_json::json!(200.0f64));
            node.params.insert("ts_scale".into(),  serde_json::json!(1.0));
            node.params.insert("ts_auto".into(),   Value::Bool(false));
            node.params.insert("ts_uni".into(),    Value::Bool(false));
        }
    }

    let (win_ms, ts_scale, ts_auto, ts_uni) = snarl.get_node(node_id).map(|n| {
        let win = n.params.get("ts_win_ms").and_then(|v| v.as_f64()).unwrap_or(200.0).clamp(10.0, 10_000.0) as f32;
        let sc  = n.params.get("ts_scale") .and_then(|v| v.as_f64()).unwrap_or(1.0).max(0.001) as f32;
        let au  = n.params.get("ts_auto")  .and_then(|v| v.as_bool()).unwrap_or(false);
        let uni = n.params.get("ts_uni")   .and_then(|v| v.as_bool()).unwrap_or(false);
        (win, sc, au, uni)
    }).unwrap_or((200.0, 1.0, false, false));

    // Show in-progress accumulation while armed, frozen capture otherwise.
    // data channels are indices 1.. (index 0 is trigger).
    let (display_data, n_channels, is_live) = snarl.get_node(node_id).map(|n| {
        let n_data = n.inputs.len().saturating_sub(1).max(1);
        if n.extra.trig_armed {
            (n.extra.trig_acc.clone(), n_data, true)
        } else if let Some(cap) = &n.extra.trig_capture {
            (cap.clone(), n_data, false)
        } else {
            (Vec::new(), n_data, false)
        }
    }).unwrap_or((Vec::new(), 1, false));

    let eff_scale = if ts_auto {
        let max_v = display_data.iter()
            .flat_map(|s| s.iter().skip(1).filter_map(|v| *v))
            .map(|v: f32| v.abs())
            .fold(0.0f32, f32::max);
        if max_v > 0.0 { max_v } else { 1.0 }
    } else { ts_scale };

    let mut display_rect: Option<egui::Rect> = None;
    ui.vertical(|ui| {
        egui::Resize::default()
            .id_salt(("ts", node_id))
            .default_size([240.0, 100.0])
            .min_size([60.0, 30.0])
            .show(ui, |ui| {
                let (rect, _) = ui.allocate_exact_size(ui.available_size(), egui::Sense::hover());
                display_rect = Some(rect);
                let painter = ui.painter_at(rect);
                painter.rect_filled(rect, 2.0, GRAPH_BG_DEFAULT);
                let (grid_faint, grid_axis) = graph_grid_colors(None);

                for i in 1..4 {
                    let y = if ts_uni {
                        rect.bottom() - rect.height() * (i as f32 / 4.0)
                    } else {
                        rect.top() + rect.height() * (i as f32 / 4.0)
                    };
                    let is_zero = !ts_uni && i == 2;
                    painter.line_segment(
                        [egui::pos2(rect.left(), y), egui::pos2(rect.right(), y)],
                        egui::Stroke::new(
                            if is_zero { 1.0 } else { 0.5 },
                            if is_zero { grid_axis } else { grid_faint },
                        ),
                    );
                }
                if ts_uni {
                    painter.line_segment(
                        [egui::pos2(rect.left(), rect.bottom()), egui::pos2(rect.right(), rect.bottom())],
                        egui::Stroke::new(1.0, grid_axis),
                    );
                }

                if !display_data.is_empty() {
                    let n = display_data.len();
                    // While live, pin the right edge at the current fill fraction.
                    let win_samples = (win_ms / 1000.0 * current_sample_rate() as f32) as usize;
                    let fill_frac = if is_live && win_samples > 0 {
                        (n as f32 / win_samples as f32).min(1.0)
                    } else { 1.0 };
                    let draw_right = rect.left() + rect.width() * fill_frac;

                    let pixel_budget = ((draw_right - rect.left()).ceil() as usize).max(2);
                    let display: Vec<Vec<Option<f32>>> = if n <= pixel_budget {
                        display_data.clone()
                    } else {
                        (0..pixel_budget).map(|i| {
                            let lo = i * n / pixel_budget;
                            let hi = ((i + 1) * n / pixel_budget).min(n);
                            (0..=n_channels).map(|col| {
                                let vals: Vec<f32> = display_data[lo..hi].iter()
                                    .filter_map(|s| s.get(col).copied().flatten())
                                    .collect();
                                if vals.is_empty() { None } else { Some(vals.iter().sum::<f32>() / vals.len() as f32) }
                            }).collect()
                        }).collect()
                    };
                    let nd = display.len();
                    if nd >= 2 {
                        for ch in 0..n_channels {
                            let col_idx = ch + 1; // skip trig pin
                            let pts: Vec<egui::Pos2> = display.iter().enumerate().filter_map(|(i, s)| {
                                s.get(col_idx).copied().flatten().map(|v| {
                                    let x = rect.left() + (i as f32 / (nd - 1) as f32) * (draw_right - rect.left());
                                    let norm = v / eff_scale;
                                    let y = if ts_uni {
                                        rect.bottom() - norm.clamp(0.0, 1.0) * rect.height() * 0.92
                                    } else {
                                        rect.center().y - norm.clamp(-1.0, 1.0) * rect.height() * 0.45
                                    };
                                    egui::pos2(x, y)
                                })
                            }).collect();
                            for w in pts.windows(2) {
                                painter.line_segment([w[0], w[1]], egui::Stroke::new(1.5, MULTI_COLORS[ch % MULTI_COLORS.len()]));
                            }
                        }
                    }
                } else {
                    // No capture yet — dim placeholder text.
                    let text_color = Color32::from_gray(80);
                    painter.text(rect.center(), egui::Align2::CENTER_CENTER,
                        "Waiting for trigger...", egui::FontId::proportional(11.0), text_color);
                }
            });

        // ── Controls ──────────────────────────────────────────────────────────
        let mut win_ms_ctrl = win_ms;
        let mut sc      = ts_scale;
        let mut au      = ts_auto;
        let mut uni     = ts_uni;
        let mut changed = false;

        let controls_resp = ui.horizontal(|ui| {
            ui.label(egui::RichText::new("Win").small().weak());
            changed |= ui.add(egui::Slider::new(&mut win_ms_ctrl, 10.0f32..=10000.0)
                .logarithmic(true).show_value(false)).changed();
            let lbl = if win_ms_ctrl >= 1000.0 {
                format!("{:.1}s", win_ms_ctrl / 1000.0)
            } else {
                format!("{:.0}ms", win_ms_ctrl)
            };
            ui.label(egui::RichText::new(lbl).small().weak());
            ui.separator();
            ui.label(egui::RichText::new("Scale").small().weak());
            if au {
                ui.label(egui::RichText::new(format!("{:.3}", eff_scale)).small().weak());
            } else {
                changed |= ui.add(egui::DragValue::new(&mut sc).speed(0.01)
                    .range(0.001f32..=100.0).max_decimals(3)).changed();
            }
            let au_before = au;
            ui.checkbox(&mut au, egui::RichText::new("Auto").small());
            changed |= au != au_before;
            ui.separator();
            let uni_before = uni;
            ui.selectable_value(&mut uni, false, egui::RichText::new("Bi").small());
            ui.selectable_value(&mut uni, true,  egui::RichText::new("Uni").small());
            changed |= uni != uni_before;
        });
        register_exposable_element(ui, node_id, "controls", controls_resp.response.rect);

        if changed {
            if let Some(node) = snarl.get_node_mut(node_id) {
                if let Some(n) = Number::from_f64(win_ms_ctrl as f64) { node.params.insert("ts_win_ms".into(), Value::Number(n)); }
                node.params.insert("ts_auto".into(), Value::Bool(au));
                node.params.insert("ts_uni".into(),  Value::Bool(uni));
                if let Some(n) = Number::from_f64(sc as f64) {
                    node.params.insert("ts_scale".into(), Value::Number(n));
                }
            }
        }

        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("Ch").small().weak());
            if ui.small_button("+").on_hover_text("Add channel").clicked() {
                if let Some(node) = snarl.get_node_mut(node_id) {
                    let next = node.inputs.len(); // 0=trig, 1..=chN
                    node.inputs.push(PinDescriptor::new(format!("ch{}", next), SignalType::Float));
                }
            }
            // Minimum 2 inputs: trig + ch1.
            let n_all = snarl.get_node(node_id).map(|n| n.inputs.len()).unwrap_or(2);
            if n_all > 2 && ui.small_button("−").on_hover_text("Remove channel").clicked() {
                remove_input_pin(node_id, n_all - 1, inputs, snarl);
            }
        });
    });
    if let Some(r) = display_rect { register_exposable_element(ui, node_id, "display", r); }
}

/// Bare trigger-scope display for sub-patch pinned layouts.
pub(crate) fn render_trigscope_display(
    inner_id: NodeId,
    ui: &mut egui::Ui,
    snarl: &mut Snarl<NodeData>,
    container: egui::Vec2,
    graph_ov: Option<&crate::canvas::node::PinGraphOverride>,
) {
    // Conditional vsync — see scope_should_request_repaint above.
    let _ = scope_should_request_repaint(inner_id, snarl, ui.ctx());
    let (display_data, n_channels, ts_scale, ts_auto, ts_uni, is_live, win_ms) = snarl.get_node(inner_id).map(|n| {
        let sc  = n.params.get("ts_scale") .and_then(|v| v.as_f64()).unwrap_or(1.0).max(0.001) as f32;
        let au  = n.params.get("ts_auto")  .and_then(|v| v.as_bool()).unwrap_or(false);
        let uni = n.params.get("ts_uni")   .and_then(|v| v.as_bool()).unwrap_or(false);
        let win = n.params.get("ts_win_ms").and_then(|v| v.as_f64()).unwrap_or(200.0).clamp(10.0, 10_000.0) as f32;
        let n_data = n.inputs.len().saturating_sub(1).max(1);
        if n.extra.trig_armed {
            (n.extra.trig_acc.clone(), n_data, sc, au, uni, true, win)
        } else if let Some(cap) = &n.extra.trig_capture {
            (cap.clone(), n_data, sc, au, uni, false, win)
        } else {
            (Vec::new(), n_data, sc, au, uni, false, win)
        }
    }).unwrap_or((Vec::new(), 1, 1.0, false, false, false, 200.0));

    let eff_scale = if ts_auto {
        let max_v = display_data.iter()
            .flat_map(|s| s.iter().skip(1).filter_map(|v| *v))
            .map(|v: f32| v.abs())
            .fold(0.0f32, f32::max);
        if max_v > 0.0 { max_v } else { 1.0 }
    } else { ts_scale };

    let avail = egui::vec2(container.x.max(40.0), container.y.max(24.0));
    let (rect, _) = ui.allocate_exact_size(avail, egui::Sense::hover());
    let painter = ui.painter_at(rect);
    let (graph_bg, graph_outline) = graph_chrome(graph_ov);
    painter.rect_filled(rect, 2.0, graph_bg);
    let (grid_faint, grid_axis) = graph_grid_colors(graph_ov);

    for i in 1..4 {
        let y = if ts_uni {
            rect.bottom() - rect.height() * (i as f32 / 4.0)
        } else {
            rect.top() + rect.height() * (i as f32 / 4.0)
        };
        let is_zero = !ts_uni && i == 2;
        painter.line_segment(
            [egui::pos2(rect.left(), y), egui::pos2(rect.right(), y)],
            egui::Stroke::new(
                if is_zero { 1.0 } else { 0.5 },
                if is_zero { grid_axis } else { grid_faint },
            ),
        );
    }
    if ts_uni {
        painter.line_segment(
            [egui::pos2(rect.left(), rect.bottom()), egui::pos2(rect.right(), rect.bottom())],
            egui::Stroke::new(1.0, grid_axis),
        );
    }

    if !display_data.is_empty() {
        let n = display_data.len();
        let win_samples = (win_ms / 1000.0 * current_sample_rate() as f32) as usize;
        let fill_frac = if is_live && win_samples > 0 {
            (n as f32 / win_samples as f32).min(1.0)
        } else { 1.0 };
        let draw_right = rect.left() + rect.width() * fill_frac;
        let pixel_budget = ((draw_right - rect.left()).ceil() as usize).max(2);
        let display: Vec<Vec<Option<f32>>> = if n <= pixel_budget {
            display_data.clone()
        } else {
            (0..pixel_budget).map(|i| {
                let lo = i * n / pixel_budget;
                let hi = ((i + 1) * n / pixel_budget).min(n);
                (0..=n_channels).map(|col| {
                    let vals: Vec<f32> = display_data[lo..hi].iter()
                        .filter_map(|s| s.get(col).copied().flatten())
                        .collect();
                    if vals.is_empty() { None } else { Some(vals.iter().sum::<f32>() / vals.len() as f32) }
                }).collect()
            }).collect()
        };
        let nd = display.len();
        if nd >= 2 {
            for ch in 0..n_channels {
                let col_idx = ch + 1;
                let pts: Vec<egui::Pos2> = display.iter().enumerate().filter_map(|(i, s)| {
                    s.get(col_idx).copied().flatten().map(|v| {
                        let x = rect.left() + (i as f32 / (nd - 1) as f32) * (draw_right - rect.left());
                        let norm = v / eff_scale;
                        let y = if ts_uni {
                            rect.bottom() - norm.clamp(0.0, 1.0) * rect.height() * 0.92
                        } else {
                            rect.center().y - norm.clamp(-1.0, 1.0) * rect.height() * 0.45
                        };
                        egui::pos2(x, y)
                    })
                }).collect();
                let ch_col = graph_channel_color(graph_ov, ch);
                for w in pts.windows(2) {
                    painter.line_segment([w[0], w[1]], egui::Stroke::new(1.5, ch_col));
                }
            }
        }
    } else {
        let text_color = Color32::from_gray(80);
        painter.text(rect.center(), egui::Align2::CENTER_CENTER,
            "Waiting for trigger...", egui::FontId::proportional(11.0), text_color);
    }

    if let Some(stroke) = graph_outline {
        painter.rect_stroke(rect, 2.0, stroke, egui::StrokeKind::Inside);
    }
    let _ = inner_id;
}

/// Bare trigger-scope controls row for sub-patch pinned layouts.
pub(crate) fn render_trigscope_controls(
    inner_id: NodeId,
    ui: &mut egui::Ui,
    snarl: &mut Snarl<NodeData>,
    container: egui::Vec2,
) {
    let (mut win_ms, mut sc, mut au, mut uni) = snarl.get_node(inner_id).map(|n| {
        let win = n.params.get("ts_win_ms").and_then(|v| v.as_f64()).unwrap_or(200.0).clamp(10.0, 10_000.0) as f32;
        let s   = n.params.get("ts_scale") .and_then(|v| v.as_f64()).unwrap_or(1.0).max(0.001) as f32;
        let a   = n.params.get("ts_auto")  .and_then(|v| v.as_bool()).unwrap_or(false);
        let u   = n.params.get("ts_uni")   .and_then(|v| v.as_bool()).unwrap_or(false);
        (win, s, a, u)
    }).unwrap_or((200.0, 1.0, false, false));

    let mut changed = false;
    let mut fr = [egui::Rect::NOTHING; 4];
    ui.set_max_width(container.x);
    apply_widget_scale(ui, container, egui::vec2(360.0, 22.0));
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new("Win").weak());
        // Flexible element: the Win slider absorbs surplus container width.
        ui.spacing_mut().slider_width = pin_flex_width(ui, container, 70.0);
        let r = ui.add(egui::Slider::new(&mut win_ms, 10.0f32..=10_000.0)
            .logarithmic(true).show_value(false));
        fr[0] = r.rect; changed |= r.changed();
        let lbl = if win_ms >= 1000.0 { format!("{:.1}s", win_ms / 1000.0) } else { format!("{:.0}ms", win_ms) };
        ui.label(egui::RichText::new(lbl).weak());
        ui.separator();
        ui.label(egui::RichText::new("Scale").weak());
        if au {
            fr[1] = ui.label(egui::RichText::new(format!("{:.3}", sc)).weak()).rect;
        } else {
            let r = ui.add(egui::DragValue::new(&mut sc).speed(0.01)
                .range(0.001f32..=100.0).max_decimals(3));
            fr[1] = r.rect; changed |= r.changed();
        }
        let was_au = au;
        fr[2] = ui.checkbox(&mut au, egui::RichText::new("Auto")).rect;
        changed |= au != was_au;
        ui.separator();
        let was_uni = uni;
        let rb = ui.selectable_value(&mut uni, false, egui::RichText::new("Bi"));
        let ru = ui.selectable_value(&mut uni, true,  egui::RichText::new("Uni"));
        fr[3] = rb.rect.union(ru.rect);
        changed |= uni != was_uni;
    });
    publish_nav_field_rects(ui, inner_id, &fr);
    if changed {
        if let Some(node) = snarl.get_node_mut(inner_id) {
            if let Some(n) = Number::from_f64(win_ms as f64) { node.params.insert("ts_win_ms".into(), Value::Number(n)); }
            if let Some(n) = Number::from_f64(sc as f64)     { node.params.insert("ts_scale".into(),  Value::Number(n)); }
            node.params.insert("ts_auto".into(), Value::Bool(au));
            node.params.insert("ts_uni".into(),  Value::Bool(uni));
        }
    }
}
