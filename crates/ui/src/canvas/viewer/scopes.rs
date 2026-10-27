//! Scope displays: Readout and Vectorscope (the Oscilloscope and the Trigger
//! Scope share `waveform.rs`)
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
    let font = super::quantize_font_size(container.y.clamp(10.0, 64.0) * 0.55);
    ui.add_sized(
        [container.x, container.y.max(18.0)],
        egui::Label::new(egui::RichText::new(text).monospace().size(font)),
    );
}

// ── Vectorscope ───────────────────────────────────────────────────────────────
// (The Oscilloscope and the Trigger Scope live in `waveform.rs`.)

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
        egui::Stroke::new(0.5_f32, grid_axis),
    );
    painter.line_segment(
        [egui::pos2(rect.left(), rect.center().y), egui::pos2(rect.right(), rect.center().y)],
        egui::Stroke::new(0.5_f32, grid_axis),
    );
    painter.circle_stroke(rect.center(), rect.width().min(rect.height()) * 0.45,
        egui::Stroke::new(0.5_f32, grid_faint));

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
                painter.line(chunk_pts, egui::Stroke::new(1.25_f32, stroke_color));
            }
        }

        // Current value head — a small filled+stroked circle so the user
        // can pinpoint the live sample even when the trail dims away.
        if let Some(Some(Signal::Vec2(v))) = last_signals.get(ch) {
            let px = center.x + v.x.clamp(-1.0, 1.0) * hx;
            let py = center.y - v.y.clamp(-1.0, 1.0) * hy;
            painter.circle_filled(egui::pos2(px, py), 4.0, col);
            painter.circle_stroke(egui::pos2(px, py), 4.0,
                egui::Stroke::new(1.0_f32, Color32::from_gray(100)));
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
            egui::Stroke::new(0.5_f32, grid_axis),
        );
        painter.line_segment(
            [egui::pos2(rect.left(), rect.center().y), egui::pos2(rect.right(), rect.center().y)],
            egui::Stroke::new(0.5_f32, grid_axis),
        );
        painter.circle_stroke(rect.center(), rect.width().min(rect.height()) * 0.45,
            egui::Stroke::new(0.5_f32, grid_faint));

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
                        egui::Stroke::new(1.25_f32, stroke_color));
                }
            }

            // Current-position dot (filled+stroked) so the live sample
            // remains visible when the trail dims out.
            if let Some(Some(Signal::Vec2(v))) = last_signals.get(ch) {
                let px = center.x + v.x.clamp(-1.0, 1.0) * hx;
                let py = center.y - v.y.clamp(-1.0, 1.0) * hy;
                painter.circle_filled(egui::pos2(px, py), 4.0, col);
                painter.circle_stroke(egui::pos2(px, py), 4.0,
                    egui::Stroke::new(1.0_f32, Color32::from_gray(100)));
            }
        }

        // Paint a small diagonal-line resize grip in the bottom-right corner
        // (mirrors egui's internal `paint_resize_corner_with_style`).
        {
            let grip_color = ui.style().interact(&handle_resp).fg_stroke.color;
            let grip_stroke = egui::Stroke::new(1.0_f32, grip_color);
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

