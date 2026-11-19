//! Custom reusable widgets: egui/snarl stock widgets often aren't flexible
//! enough for FlexInput's UI, so hand-rolled replacements collect here.
//! Future home of style flavors / theming hooks.

pub(crate) mod tex_cache;

/// Drop-in replacement for the removed-in-spirit `egui::popup_below_widget`
/// (deprecated in 0.33): a memory-toggled popup anchored under the widget.
/// Mirrors egui's own `popup_above_or_below_widget` shim over `egui::Popup`.
pub(crate) fn popup_below_widget<R>(
    widget_response: &egui::Response,
    popup_id: egui::Id,
    close_behavior: egui::PopupCloseBehavior,
    add_contents: impl FnOnce(&mut egui::Ui) -> R,
) -> Option<R> {
    let response = egui::Popup::from_response(widget_response)
        .layout(egui::Layout::top_down_justified(egui::Align::LEFT))
        .open_memory(None)
        .close_behavior(close_behavior)
        .id(popup_id)
        .align(egui::RectAlign::BOTTOM_START)
        .width(widget_response.rect.width())
        .show(|ui| {
            ui.set_min_width(ui.available_width());
            add_contents(ui)
        })?;
    Some(response.inner)
}

/// True if the slider track (covered by `slider_resp`) was double-clicked
/// this frame. egui's Slider widget senses drag only, so `.double_clicked()`
/// on its Response is always false — we overlay a click-sense interact on
/// the same rect using a distinct id and read the double-click from there.
/// Call AFTER rendering the slider so the overlay sits on top.
pub(crate) fn slider_track_double_clicked(ui: &egui::Ui, slider_resp: &egui::Response) -> bool {
    let id = slider_resp.id.with("__dblclick_overlay");
    ui.interact(slider_resp.rect, id, egui::Sense::click()).double_clicked()
}

/// Fixed-width label cell used to align the leading column across the
/// Deadzone / Gyro × / Mouse × slider rows in a device-node header.
/// A slider with two handles, picking a range `lo..=hi` within `bounds`, `width`
/// wide. A drag moves whichever handle is nearer where it starts (the higher one
/// when they sit together and the drag starts to their right), and neither handle
/// passes the other. The span between them is drawn in the selection colour.
pub(crate) fn range_slider(
    ui: &mut egui::Ui,
    lo: &mut f32,
    hi: &mut f32,
    bounds: std::ops::RangeInclusive<f32>,
    width: f32,
) -> egui::Response {
    let h = ui.spacing().interact_size.y;
    let (rect, mut resp) = ui.allocate_exact_size(egui::vec2(width, h), egui::Sense::click_and_drag());
    let knob = h * 0.35;
    let rail = rect.shrink2(egui::vec2(knob, 0.0));
    let (b0, b1) = (*bounds.start(), *bounds.end());
    let x_of = |v: f32| egui::remap_clamp(v, b0..=b1, rail.left()..=rail.right());
    let v_of = |x: f32| egui::remap_clamp(x, rail.left()..=rail.right(), b0..=b1);

    let which = resp.id.with("range_handle");
    if resp.drag_started() || resp.clicked() {
        if let Some(p) = resp.interact_pointer_pos() {
            let (dl, dh) = ((p.x - x_of(*lo)).abs(), (p.x - x_of(*hi)).abs());
            let upper = dh < dl || (dh == dl && p.x > x_of(*hi));
            ui.ctx().data_mut(|d| d.insert_temp(which, upper));
        }
    }
    if resp.dragged() || resp.clicked() {
        if let Some(p) = resp.interact_pointer_pos() {
            let v = v_of(p.x);
            let upper = ui.ctx().data(|d| d.get_temp::<bool>(which)).unwrap_or(false);
            let before = (*lo, *hi);
            if upper {
                *hi = v.max(*lo);
            } else {
                *lo = v.min(*hi);
            }
            if (*lo, *hi) != before {
                resp.mark_changed();
            }
        }
    }

    let visuals = ui.style().interact(&resp);
    let painter = ui.painter();
    let y = rect.center().y;
    let rail_stroke = egui::Stroke::new(4.0_f32, ui.visuals().widgets.inactive.bg_fill);
    painter.line_segment([egui::pos2(rail.left(), y), egui::pos2(rail.right(), y)], rail_stroke);
    let span = egui::Stroke::new(4.0_f32, ui.visuals().selection.bg_fill);
    painter.line_segment([egui::pos2(x_of(*lo), y), egui::pos2(x_of(*hi), y)], span);
    for v in [*lo, *hi] {
        painter.circle(egui::pos2(x_of(v), y), knob, visuals.bg_fill, visuals.fg_stroke);
    }
    resp
}

pub(crate) fn slider_label(ui: &mut egui::Ui, label: &str, cell_w: f32) {
    let (cell, _) = ui.allocate_exact_size(
        egui::vec2(cell_w, 18.0),
        egui::Sense::hover(),
    );
    ui.painter().text(
        egui::pos2(cell.left(), cell.center().y),
        egui::Align2::LEFT_CENTER,
        label,
        egui::TextStyle::Small.resolve(ui.style()),
        ui.style().visuals.weak_text_color(),
    );
}

/// The one colour swatch used everywhere in the app: an egui colour button
/// wired for correct straight-alpha editing plus a right-click copy/paste
/// menu (hex on the system clipboard too). `use_alpha` picks whether the
/// alpha channel is editable — when false the picker shows no alpha slider
/// and the stored alpha is forced opaque.
///
/// Correctness note: two egui gotchas, both handled here. (1) `Hsva::from_srgba_
/// unmultiplied` routes through PREMULTIPLIED `Color32`, which collapses toward
/// black at low alpha — so we build the picker's HSVA from an OPAQUE RGB
/// conversion and attach alpha separately. (2) egui re-round-trips `value`
/// through gamma space every frame the picker is open (`Hsva`⇄`HsvaGamma`);
/// caching that HSVA in memory let the tiny float error ACCUMULATE and the
/// colour crept dark, so we re-derive HSVA from the (u8-quantized) bytes each
/// frame instead — the quantization resets the drift and the colour settles.
///
/// CALLER CONTRACT: `col` must be the raw STRAIGHT-alpha bytes as stored —
/// never bytes recovered from a `Color32` (`from_rgba_unmultiplied(..).r()` …):
/// `Color32` is premultiplied internally, so that hands back `rgb × alpha`,
/// brightness clamps to the alpha value, and the colour re-darkens every frame
/// (see `menu_body::pcolor_bytes`, which parses colour params correctly).
pub(crate) fn fxi_color_swatch(
    ui: &mut egui::Ui,
    col: &mut [u8; 4],
    tooltip: &str,
    use_alpha: bool,
) -> bool {
    use egui::ecolor::Hsva;
    use egui::widgets::color_picker::{color_edit_button_hsva, Alpha};

    let clip_id = egui::Id::new("fxi_color_clipboard");
    let alpha = if use_alpha { Alpha::OnlyBlend } else { Alpha::Opaque };
    // Opaque path: the picker edits RGB only and the caller's stored alpha is
    // preserved (used by the theme swatches, whose alpha is a separate slider).
    let keep_a = col[3];

    // HSVA ⇄ sRGB bytes, straight-alpha. Crucial: `Hsva::from_srgba_unmultiplied`
    // round-trips through *premultiplied* `Color32`, which collapses to black at
    // low alpha — the "roll to dark" glitch. Build HSVA from an OPAQUE RGB
    // conversion (lossless) and attach alpha separately instead.
    let to_bytes = |h: &Hsva| -> [u8; 4] {
        if use_alpha {
            h.to_srgba_unmultiplied()
        } else {
            let [r, g, b] = h.to_srgb();
            [r, g, b, keep_a]
        }
    };
    let from_bytes = |c: &[u8; 4]| -> Hsva {
        let mut h = Hsva::from(egui::Color32::from_rgb(c[0], c[1], c[2]));
        if use_alpha {
            h.a = c[3] as f32 / 255.0;
        }
        h
    };

    // Re-derive the working HSVA from the (u8-quantized) bytes every frame — NOT
    // cached across frames. egui re-round-trips value through gamma space each
    // frame the picker is open; carrying that f32 in memory let the error
    // accumulate and the colour drifted dark. Re-quantizing here settles it.
    let mut hsva = from_bytes(col);

    let resp = color_edit_button_hsva(ui, &mut hsva, alpha)
        .on_hover_text(format!("{tooltip}\nRight-click: copy / paste colour"));

    let mut changed = false;
    let picked = to_bytes(&hsva);
    if picked != *col {
        *col = picked;
        changed = true;
    }
    // (No overpaint here: egui's colour button splits a non-opaque swatch into
    // "over checkers | opaque" — the proper transparency preview. It only ever
    // looked muddy because callers used to hand in premultiplied bytes.)
    // Right-click Copy/Paste menu under its OWN popup id — attaching a
    // `context_menu` to the response conflicted with the colour-picker popup
    // (same widget id) and closed it a frame after opening.
    let menu_id = resp.id.with("fxi_color_menu");
    if resp.secondary_clicked() {
        egui::Popup::toggle_id(ui.ctx(), menu_id);
    }
    popup_below_widget(
        &resp,
        menu_id,
        egui::PopupCloseBehavior::CloseOnClick,
        |ui| {
            ui.set_min_width(150.0);
            if ui.button("Copy colour").clicked() {
                ui.ctx().data_mut(|d| d.insert_temp(clip_id, *col));
                ui.ctx().copy_text(if col[3] == 255 {
                    format!("#{:02X}{:02X}{:02X}", col[0], col[1], col[2])
                } else {
                    format!("#{:02X}{:02X}{:02X}{:02X}", col[0], col[1], col[2], col[3])
                });
            }
            // Old clipboard entries may still be RGB triples — accept both.
            let clip: Option<[u8; 4]> = ui.ctx().data(|d| {
                d.get_temp::<[u8; 4]>(clip_id)
                    .or_else(|| d.get_temp::<[u8; 3]>(clip_id).map(|c| [c[0], c[1], c[2], 255]))
            });
            let label = match clip {
                Some(c) if c[3] == 255 => {
                    format!("Paste colour  #{:02X}{:02X}{:02X}", c[0], c[1], c[2])
                }
                Some(c) => format!(
                    "Paste colour  #{:02X}{:02X}{:02X}{:02X}",
                    c[0], c[1], c[2], c[3]
                ),
                None => "Paste colour".to_string(),
            };
            if ui
                .add_enabled(clip.is_some(), egui::Button::new(label))
                .clicked()
            {
                if let Some(c) = clip {
                    // Opaque swatches keep their own alpha (a separate slider).
                    *col = if use_alpha { c } else { [c[0], c[1], c[2], keep_a] };
                    changed = true;
                }
            }
        },
    );
    changed
}

// ── Gamepad-nav highlight subsystem ─────────────────────────────────────────
// One place for the selection/focus glow so (a) it looks identical everywhere,
// (b) a future restyle is a single edit, and (c) the viewport-pass gating that
// makes highlights show in the separate config-overlay window lives in one
// helper instead of scattered per-channel re-stamps.

/// Ctx-data slot holding the pass number the nav driver stamps its highlight
/// channels with each frame (see `nav_pass`). Written once per `run_gamepad_nav`.
pub(crate) const NAV_PASS_KEY: &str = "gp_nav_pass";

/// The pass all nav highlights are stamped with this frame. `egui`'s `data` map
/// is shared across viewports but `cumulative_pass_nr()` is PER-viewport, so a
/// pinned body rendered in the config-overlay viewport must gate its highlight
/// against THIS (the root nav pass) rather than its own viewport's counter —
/// otherwise the highlight silently mismatches over the game. Falls back to the
/// local pass when the nav driver hasn't run (e.g. no gamepad), which is fine
/// because no highlight channel is published on those frames either.
pub(crate) fn nav_pass(ctx: &egui::Context) -> u64 {
    ctx.data(|d| d.get_temp::<u64>(egui::Id::new(NAV_PASS_KEY)))
        .unwrap_or_else(|| ctx.cumulative_pass_nr())
}

/// True if a highlight channel stamped with `pass` is current this frame,
/// tolerant of a small lag (mirrors the existing `saturating_sub(pass) <= 2`
/// gates). Use at every SELECTION/focus channel read that a pinned body or glow
/// painter performs, in place of `pass == ui.ctx().cumulative_pass_nr()`.
pub(crate) fn nav_pass_matches(ctx: &egui::Context, pass: u64) -> bool {
    nav_pass(ctx).saturating_sub(pass) <= 2
}

/// Ctx-data slot a mapping body stamps (with the root nav pass) every frame a
/// Learn capture is live for the nav-driving pad. See `hold_nav_for_capture`.
const NAV_CAPTURE_HOLD_KEY: &str = "gp_nav_capture_hold";

/// Hold gamepad UI-nav inert this frame: a mapping body (Remapper / Map Action /
/// 3DOF-to-2D Lean / Touch Zones) has a Learn capture armed, so the buttons the
/// user demonstrates must reach that capture instead of navigating (LB/RB
/// flipping tabs, Select Alt-Tabbing, …). Re-publish EVERY frame the capture is
/// live — nav resumes on its own once the stamps stop, so an arm left behind by
/// a body that stopped rendering can never lock navigation out. Stamped with the
/// root nav pass so a body in the config-overlay viewport gates correctly.
pub(crate) fn hold_nav_for_capture(ctx: &egui::Context) {
    // Pass first: nesting the data lock inside `nav_pass`'s would deadlock.
    let pass = nav_pass(ctx);
    ctx.data_mut(|d| d.insert_temp(egui::Id::new(NAV_CAPTURE_HOLD_KEY), pass));
}

/// True while some body is holding nav for a live capture (`hold_nav_for_capture`).
pub(crate) fn nav_held_for_capture(ctx: &egui::Context) -> bool {
    ctx.data(|d| d.get_temp::<u64>(egui::Id::new(NAV_CAPTURE_HOLD_KEY)))
        .map_or(false, |p| nav_pass_matches(ctx, p))
}

/// Centralized appearance of every gamepad-nav highlight. Built from the active
/// egui visuals so it tracks the theme; the single seam a future custom theme
/// would override to restyle all highlights at once.
#[derive(Clone, Copy)]
pub(crate) struct NavHighlightStyle {
    /// Focused/selected ring colour.
    pub accent: egui::Color32,
    /// Ring colour while a divider/handle is grabbed (being dragged by nav).
    pub grabbed: egui::Color32,
}

impl NavHighlightStyle {
    pub(crate) fn from_visuals(v: &egui::Visuals) -> Self {
        Self {
            accent: v.selection.stroke.color,
            grabbed: egui::Color32::from_rgb(90, 220, 120),
        }
    }
    pub(crate) fn of(ctx: &egui::Context) -> Self {
        Self::from_visuals(&ctx.style().visuals)
    }
}

/// The standard "on top, clipped to its container" painter for a post-hoc glow:
/// a `Foreground`-layer painter cropped to `clip`, so a highlight on content
/// inside a `ScrollArea` (e.g. a Remapper card scrolled partly out of view) is
/// cropped at the visible edge instead of spilling over the hidden part. Inline
/// callers pass `ui.clip_rect()`; post-hoc callers pass their published
/// container rect (or the full rect when there is no scroll container).
///
/// `host` is the layer the highlighted widget lives on: whatever is stacked
/// over it is cut out of the glow (see [`nav_glow_layer`]).
pub(crate) fn nav_highlight_painter(
    ctx: &egui::Context,
    id: egui::Id,
    host: egui::LayerId,
    clip: egui::Rect,
) -> egui::Painter {
    ctx.layer_painter(nav_glow_layer(ctx, egui::Order::Foreground, id, host))
        .with_clip_rect(clip)
}

/// How far a gamepad-nav auto-scroll moves a view (`view_top..view_bot`) to show
/// `top..bot`, as a change to the scroll offset (positive scrolls down).
///
/// Something that fits is brought fully into view with `margin` around it, the
/// margin shrinking to what the view has room for. Something TALLER than the view
/// can never be shown whole, and asking for both edges is what flickered: the
/// top rule scrolled up, the bottom rule scrolled back down, every frame. Such a
/// target only moves the view while the view sticks out past one of its ends: its
/// top comes to the view's top (or its bottom to the view's bottom), and once the
/// view lies inside it nothing moves. The answer for the view it leads to is
/// always 0, so repeating it can't oscillate.
pub(crate) fn nav_reveal_delta(top: f32, bot: f32, view_top: f32, view_bot: f32, margin: f32) -> f32 {
    let (h, view_h) = (bot - top, view_bot - view_top);
    if !(h.is_finite() && view_h.is_finite()) || view_h <= 0.0 {
        return 0.0;
    }
    if h <= view_h {
        let m = margin.min((view_h - h) * 0.5).max(0.0);
        if top - m < view_top {
            top - m - view_top
        } else if bot + m > view_bot {
            bot + m - view_bot
        } else {
            0.0
        }
    } else if top - margin > view_top {
        top - margin - view_top
    } else if bot + margin < view_bot {
        bot + margin - view_bot
    } else {
        0.0
    }
}

/// The layer a post-hoc nav highlight (a glow, a focus HUD, a tip) paints on,
/// for a widget that lives on `host`. It stays a free `order` layer, so it is
/// always above the widget and its own sublayers, but at the end of the pass
/// [`cut_nav_glows`] removes every part that falls under something stacked over
/// `host`: a Settings or Calibration window, a popup, a tooltip. A plain
/// `Foreground` glow painted over all of those.
///
/// Why not an egui sublayer of `host`: the Easy body nests three deep (body
/// area → scaled body → module pin → card-list scroll), and egui resolves
/// nested sublayers in hash-map order, which changes every pass. The glow would
/// flicker under the cards it rings.
///
/// Inert unless [`install_nav_glow_cut`] ran on the context.
pub(crate) fn nav_glow_layer(
    ctx: &egui::Context,
    order: egui::Order,
    id: egui::Id,
    host: egui::LayerId,
) -> egui::LayerId {
    let layer = egui::LayerId::new(order, id.with(host));
    // Both before taking the data lock: nesting ctx locks deadlocks.
    let key = nav_glow_cut_key(ctx.viewport_id());
    let pass = ctx.cumulative_pass_nr();
    ctx.data_mut(|d| {
        let cuts = d.get_temp_mut_or_default::<(u64, Vec<(egui::LayerId, egui::LayerId)>)>(key);
        if cuts.0 != pass {
            *cuts = (pass, Vec::new());
        }
        if !cuts.1.contains(&(layer, host)) {
            cuts.1.push((layer, host));
        }
    });
    layer
}

/// Register the end-of-pass cut behind [`nav_glow_layer`]. Once, at startup:
/// it runs at the end of every pass of every viewport (the config overlay and
/// the over-game picker included), after all the windows have drawn, so it sees
/// this frame's window rects.
pub(crate) fn install_nav_glow_cut(ctx: &egui::Context) {
    ctx.on_end_pass("nav_glow_cut", std::sync::Arc::new(cut_nav_glows));
}

/// Ctx-data slot of this pass's `(glow layer, host)` pairs. Per viewport: ctx
/// data is shared by all of them.
fn nav_glow_cut_key(viewport: egui::ViewportId) -> egui::Id {
    egui::Id::new(("gp_nav_glow_cut", viewport))
}

/// Cut every glow registered this pass out from under what covers its host.
/// Each shape is re-added once per uncovered piece of its clip rect; the pieces
/// are disjoint, so translucent rings never double up.
fn cut_nav_glows(ctx: &egui::Context) {
    let key = nav_glow_cut_key(ctx.viewport_id());
    let pass = ctx.cumulative_pass_nr();
    let Some((stamp, glows)) =
        ctx.data_mut(|d| d.remove_temp::<(u64, Vec<(egui::LayerId, egui::LayerId)>)>(key))
    else {
        return;
    };
    if stamp != pass {
        return;
    }
    for (layer, host) in glows {
        let covers = nav_covers(ctx, host);
        if covers.is_empty() {
            continue;
        }
        ctx.graphics_mut(|g| {
            let painted = std::mem::take(g.entry(layer));
            let list = g.entry(layer);
            for shape in painted.all_entries() {
                let mut pieces = vec![shape.clip_rect];
                for cover in &covers {
                    pieces = pieces.into_iter().flat_map(|p| rect_minus(p, *cover)).collect();
                }
                for piece in pieces {
                    list.add(piece, shape.shape.clone());
                }
            }
        });
    }
}

/// Screen rects of the visible areas stacked over `host`: later in egui's layer
/// order (last pass's, plus areas first shown this pass, which go on top), or in
/// a higher `Order`. Only areas have rects, so `host`'s own sublayers never
/// count. A host egui doesn't order (the background) sits under everything.
fn nav_covers(ctx: &egui::Context, host: egui::LayerId) -> Vec<egui::Rect> {
    let above: Vec<(egui::LayerId, egui::Rect)> = ctx.memory(|m| {
        let order: Vec<egui::LayerId> = m.layer_ids().collect();
        let host_at = order.iter().position(|l| *l == host).map_or(-1, |i| i as i64);
        order
            .iter()
            .enumerate()
            .filter(|&(i, l)| (l.order, i as i64) > (host.order, host_at) && *l != host)
            .filter(|(_, l)| m.areas().is_visible(l))
            .filter_map(|(_, l)| Some((*l, m.area_rect(l.id)?)))
            .collect()
    });
    above
        .into_iter()
        .map(|(l, r)| ctx.layer_transform_to_global(l).map_or(r, |t| t * r))
        .filter(|r| r.is_positive())
        .collect()
}

/// `r` minus `hole`, as up to four disjoint rects: full-width bands above and
/// below the hole, then the parts beside it.
fn rect_minus(r: egui::Rect, hole: egui::Rect) -> Vec<egui::Rect> {
    let h = r.intersect(hole);
    if !h.is_positive() {
        return vec![r];
    }
    let bands = [
        egui::Rect::from_min_max(r.min, egui::pos2(r.max.x, h.min.y)),
        egui::Rect::from_min_max(egui::pos2(r.min.x, h.max.y), r.max),
        egui::Rect::from_min_max(egui::pos2(r.min.x, h.min.y), egui::pos2(h.min.x, h.max.y)),
        egui::Rect::from_min_max(egui::pos2(h.max.x, h.min.y), egui::pos2(r.max.x, h.max.y)),
    ];
    bands.into_iter().filter(|b| b.is_positive()).collect()
}

/// Outward-bloom highlight on `rect`: `rings` expanding strokes fading from
/// `peak` alpha (the outermost grows `max_grow` px), then a crisp inner ring.
/// All geometry is explicit so each existing highlight reproduces exactly;
/// `color` is normally `style.accent` (or `style.grabbed`). Draws on `painter`,
/// which the caller has already set to the right layer + clip.
#[allow(clippy::too_many_arguments)]
pub(crate) fn paint_nav_bloom(
    painter: &egui::Painter,
    rect: egui::Rect,
    color: egui::Color32,
    round: f32,
    peak: f32,
    max_grow: f32,
    rings: usize,
    ring_stroke: f32,
    inner_expand: f32,
    inner_stroke: f32,
) {
    if !rect.is_finite() || rect.width() < 1.0 {
        return;
    }
    let [r, g, b, _] = color.to_array();
    let n = rings.max(1);
    for i in 0..n {
        let t = (i as f32 + 1.0) / n as f32; // 0..1 outward
        let grow = t * max_grow;
        let a = (peak * (1.0 - t)).round() as u8;
        if a == 0 {
            continue;
        }
        painter.rect_stroke(
            rect.expand(grow),
            round + grow,
            egui::Stroke::new(ring_stroke, egui::Color32::from_rgba_unmultiplied(r, g, b, a)),
            egui::StrokeKind::Outside,
        );
    }
    painter.rect_stroke(
        rect.expand(inner_expand),
        round,
        egui::Stroke::new(inner_stroke, color),
        egui::StrokeKind::Outside,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    const MARK: egui::Color32 = egui::Color32::from_rgb(1, 2, 3);

    #[test]
    fn rect_minus_leaves_disjoint_pieces_covering_the_rest() {
        let r = egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(100.0, 80.0));
        let far = egui::Rect::from_min_max(egui::pos2(200.0, 0.0), egui::pos2(300.0, 80.0));
        assert_eq!(rect_minus(r, far), vec![r]);
        assert!(rect_minus(r, r.expand(5.0)).is_empty());

        let hole = egui::Rect::from_min_max(egui::pos2(30.0, 20.0), egui::pos2(60.0, 50.0));
        let pieces = rect_minus(r, hole);
        assert_eq!(pieces.len(), 4);
        let area: f32 = pieces.iter().map(|p| p.area()).sum();
        assert_eq!(area, r.area() - hole.area());
        for (i, a) in pieces.iter().enumerate() {
            assert!(!a.intersect(hole).is_positive(), "piece {i} overlaps the hole");
            for b in &pieces[i + 1..] {
                assert!(!a.intersect(*b).is_positive(), "pieces overlap: {a:?} {b:?}");
            }
        }
    }

    /// Run one pass: a fixed window, and a glow ring crossing its edge painted
    /// BEFORE it (as nav does) for a widget on `host`. Returns the window rect and
    /// the glow's painted shapes.
    fn glow_under_window(ctx: &egui::Context, host: impl Fn() -> egui::LayerId) -> (egui::Rect, Vec<egui::epaint::ClippedShape>) {
        let out = ctx.run(egui::RawInput::default(), |ctx| {
            let p = nav_highlight_painter(ctx, egui::Id::new("test_glow"), host(), ctx.content_rect());
            p.rect_stroke(
                egui::Rect::from_min_size(egui::pos2(50.0, 50.0), egui::vec2(400.0, 300.0)),
                0.0, egui::Stroke::new(2.0_f32, MARK), egui::StrokeKind::Outside);
            egui::Window::new("cover")
                .fixed_pos(egui::pos2(100.0, 100.0))
                .fixed_size(egui::vec2(200.0, 150.0))
                .show(ctx, |ui| ui.label("covering"));
        });
        let win = ctx.memory(|m| m.area_rect(egui::Id::new("cover"))).expect("window rect");
        let glow = out.shapes.into_iter()
            .filter(|s| matches!(&s.shape, egui::Shape::Rect(r) if r.stroke.color == MARK))
            .collect();
        (win, glow)
    }

    /// The reported bug: the Easy glow showed on top of the Calibration and
    /// Settings windows. Whatever is stacked over the glow's host is cut out of
    /// it, and only that.
    #[test]
    fn a_window_over_the_host_covers_the_glow() {
        let ctx = egui::Context::default();
        install_nav_glow_cut(&ctx);
        for _ in 0..2 {
            let (win, glow) = glow_under_window(&ctx, egui::LayerId::background);
            assert!(!glow.is_empty(), "the uncovered part still paints");
            for s in &glow {
                assert!(!s.clip_rect.intersect(win).is_positive(),
                    "glow piece {:?} paints over the window {win:?}", s.clip_rect);
            }
        }
    }

    #[test]
    fn a_glow_on_the_window_itself_is_left_whole() {
        let ctx = egui::Context::default();
        install_nav_glow_cut(&ctx);
        for _ in 0..2 {
            let (_, glow) = glow_under_window(&ctx,
                || egui::LayerId::new(egui::Order::Middle, egui::Id::new("cover")));
            assert_eq!(glow.len(), 1);
            assert_eq!(glow[0].clip_rect, ctx.content_rect());
        }
    }

    /// Apply the auto-scroll the way a view does, frame after frame: the offset
    /// it settles on, and whether it ever moved back the way it came.
    fn settle(top: f32, bot: f32, view_h: f32, mut offset: f32) -> (f32, bool) {
        let mut last_sign = 0.0_f32;
        for _ in 0..20 {
            let d = nav_reveal_delta(top, bot, offset, offset + view_h, 24.0);
            if d == 0.0 {
                return (offset, false);
            }
            if last_sign != 0.0 && d.signum() != last_sign {
                return (offset, true);
            }
            last_sign = d.signum();
            offset += d;
        }
        (offset, true)
    }

    /// The reported bug: a widget taller than the Easy body flickered up and
    /// down, the top rule and the bottom rule undoing each other every frame.
    #[test]
    fn a_target_taller_than_the_view_settles_without_flicker() {
        // 500 tall in a 300 view, from above it, inside it and below it.
        for start in [0.0, 150.0, 250.0, 400.0, 700.0] {
            let (offset, flicker) = settle(200.0, 700.0, 300.0, start);
            assert!(!flicker, "from {start}: moved back and forth");
            let view = (offset, offset + 300.0);
            // The view ends up inside the widget (with the margin at an edge).
            assert!(view.0 >= 200.0 - 24.0 && view.1 <= 700.0 + 24.0, "from {start}: {view:?}");
        }
        // A view already inside it stays where it is: only content going past
        // the edge moves it.
        assert_eq!(settle(200.0, 700.0, 300.0, 300.0), (300.0, false));
        // Coming down to it shows its top; coming up to it, its bottom.
        assert_eq!(settle(200.0, 700.0, 300.0, 0.0).0, 176.0);
        assert_eq!(settle(200.0, 700.0, 300.0, 700.0).0, 424.0);
    }

    #[test]
    fn a_target_that_fits_is_shown_whole() {
        // Below the view, above it, already in it.
        assert_eq!(settle(400.0, 450.0, 300.0, 0.0).0, 450.0 + 24.0 - 300.0);
        assert_eq!(settle(10.0, 60.0, 300.0, 100.0).0, 10.0 - 24.0);
        assert_eq!(settle(100.0, 150.0, 300.0, 50.0), (50.0, false));
        // Too tall for the margin but not for the view: the margin gives way,
        // never the target.
        let (offset, flicker) = settle(100.0, 390.0, 300.0, 0.0);
        assert!(!flicker);
        assert!(offset <= 100.0 && offset + 300.0 >= 390.0, "{offset}");
    }
}

/// How much finer than the screen to rasterize a picture (an SVG) drawn in `layer`: how much
/// the layer is scaled up - a zoomed-in canvas, Easy mode's layout drawn bigger to fill its
/// panel - in the steps text is rasterized in (`egui::layers::text_raster_scale`), 1 where it
/// isn't scaled up. A picture rasterized for its size in points was stretched there, and came
/// out blurry (worst on a screen at 100%). The steps keep a zoom in progress from making a new
/// texture every frame.
pub(crate) fn raster_scale(ctx: &egui::Context, layer: egui::LayerId) -> f32 {
    let scaling = ctx.layer_transform_to_global(layer).map_or(1.0, |t| t.scaling);
    egui::layers::text_raster_scale(scaling).unwrap_or(1.0)
}

/// Pixels per point to rasterize a picture drawn in `layer` at: the screen's, times
/// [`raster_scale`].
pub(crate) fn raster_ppp(ctx: &egui::Context, layer: egui::LayerId) -> f32 {
    ctx.pixels_per_point() * raster_scale(ctx, layer)
}

#[cfg(test)]
mod drag_value_fill_tests {
    /// Every filled rect painted in the frame a `DragValue` is being dragged, and the box's own
    /// rect. `range` none: egui's default, which has no ends.
    fn dragged(range: Option<std::ops::RangeInclusive<f64>>, value: f64) -> (egui::Rect, Vec<(egui::Rect, egui::Color32)>) {
        let ctx = egui::Context::default();
        let mut v = value;
        let mut rect = egui::Rect::NOTHING;
        let run = |events: Vec<egui::Event>, rect: &mut egui::Rect, v: &mut f64| {
            let raw = egui::RawInput {
                events,
                screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(400.0, 300.0))),
                ..Default::default()
            };
            ctx.run(raw, |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    // No speed: the value stays where it is while the pointer moves.
                    let dv = egui::DragValue::new(v).speed(0.0);
                    let dv = match &range {
                        Some(r) => dv.range(r.clone()),
                        None => dv,
                    };
                    *rect = ui.add_sized([120.0, 24.0], dv).rect;
                });
            })
        };
        run(vec![], &mut rect, &mut v);
        let at = rect.center();
        let button = |pos, pressed| egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: Default::default(),
        };
        run(vec![egui::Event::PointerMoved(at)], &mut rect, &mut v);
        run(vec![button(at, true)], &mut rect, &mut v);
        let mut out = None;
        for step in 1..=6 {
            out = Some(run(vec![egui::Event::PointerMoved(at + egui::vec2(step as f32 * 4.0, 0.0))], &mut rect, &mut v));
        }
        fn walk(s: &egui::Shape, out: &mut Vec<(egui::Rect, egui::Color32)>) {
            match s {
                egui::Shape::Rect(r) => out.push((r.rect, r.fill)),
                egui::Shape::Vec(v) => v.iter().for_each(|s| walk(s, out)),
                _ => {}
            }
        }
        let mut fills = Vec::new();
        out.expect("ran").shapes.iter().for_each(|cs| walk(&cs.shape, &mut fills));
        (rect, fills)
    }

    // A value box being dragged shows where the value is along its range - a darker shade of
    // the accent over a dark field, half of it at the middle of the range, from the middle
    // for a range either side of zero - and with no ends to its range, all of it in that
    // shade. It used to fill with the active colour whatever the value.
    #[test]
    fn a_dragged_value_box_fills_to_its_place_in_the_range() {
        let visuals = egui::Visuals::dark();
        let (field, accent) = egui::widgets::drag_fill_colors(&visuals);
        let width_of = |fills: &[(egui::Rect, egui::Color32)], c: egui::Color32| {
            fills.iter().filter(|(_, f)| *f == c).map(|(r, _)| r.width()).fold(0.0f32, f32::max)
        };
        let (rect, fills) = dragged(Some(0.0..=10.0), 5.0);
        let (bar, whole) = (width_of(&fills, accent), width_of(&fills, field));
        assert!(whole >= rect.width() - 1.0, "no dark field under the bar: {fills:?}");
        assert!((bar - whole * 0.5).abs() < 2.0, "half way along, half full: {bar} of {whole}");

        let (_, fills) = dragged(Some(-1.0..=1.0), 0.5);
        let bar = fills.iter().find(|(_, f)| *f == accent).map(|(r, _)| *r).expect("a bar");
        let field_rect = fills.iter().find(|(_, f)| *f == field).map(|(r, _)| *r).expect("a field");
        assert!((bar.left() - field_rect.center().x).abs() < 1.0, "a bipolar value fills from the middle: {bar:?} in {field_rect:?}");

        let (rect, fills) = dragged(None, 3.0);
        assert!(width_of(&fills, accent) >= rect.width() - 1.0, "no range: the whole box in the accent: {fills:?}");
    }

    // Where a value is along its range: straight along an ordinary one; on a log scale along
    // a positive one across orders of magnitude, so a 100 ms delay of a minute is plainly
    // there and not a sliver; and nowhere along one a million wide, which is no range.
    #[test]
    fn a_value_is_placed_along_its_range_as_it_reads() {
        use egui::widgets::value_span;
        let at = |v: f64, r: std::ops::RangeInclusive<f64>| value_span(v, &r).map(|(at, _)| at);
        assert_eq!(at(5.0, 0.0..=20.0), Some(0.25));
        let delay = at(100.0, 0.0..=60_000.0).expect("a range");
        assert!(delay > 0.3 && delay < 0.6, "100 ms of a minute: {delay}");
        assert!(at(1000.0, 0.0..=60_000.0) > Some(delay), "and a second further along");
        assert_eq!(at(60_000.0, 0.0..=60_000.0), Some(1.0));
        assert_eq!(at(0.0, -1e6..=1e6), None, "a Counter's limits are no range");
        assert_eq!(value_span(0.5, &(-1.0..=1.0)), Some((0.75, 0.5)), "bipolar: from the middle");
    }
}

#[cfg(test)]
mod scaled_text_tests {
    /// On-screen pixels per atlas texel, for the tallest glyph of text painted in a layer
    /// scaled by `scale`.
    fn pixels_per_texel(scale: f32) -> (f32, egui::Rect) {
        let ctx = egui::Context::default();
        let layer = egui::LayerId::new(egui::Order::Middle, egui::Id::new("scaled_text"));
        let mut out = None;
        for _ in 0..3 {
            out = Some(ctx.run(egui::RawInput::default(), |ctx| {
                ctx.set_transform_layer(layer, egui::emath::TSTransform::from_scaling(scale));
                ctx.layer_painter(layer).text(
                    egui::pos2(10.0, 10.0),
                    egui::Align2::LEFT_TOP,
                    "Hamburgefonts",
                    egui::FontId::proportional(14.0),
                    egui::Color32::WHITE,
                );
            }));
        }
        let out = out.expect("ran");
        let ppp = out.pixels_per_point;
        let atlas = ctx.fonts(|f| f.font_image_size());
        let mut most = 0.0f32;
        let mut bounds = egui::Rect::NOTHING;
        for prim in ctx.tessellate(out.shapes, ppp) {
            let egui::epaint::Primitive::Mesh(mesh) = prim.primitive else { continue };
            if mesh.texture_id != egui::TextureId::default() {
                continue;
            }
            for quad in mesh.indices.chunks(6) {
                let vs: Vec<_> = quad.iter().map(|&i| mesh.vertices[i as usize]).collect();
                let span = |f: &dyn Fn(&egui::epaint::Vertex) -> f32| {
                    let (lo, hi) = vs.iter().map(f).fold((f32::MAX, f32::MIN), |(lo, hi), v| (lo.min(v), hi.max(v)));
                    hi - lo
                };
                let pixels = span(&|v| v.pos.y) * ppp;
                let texels = span(&|v| v.uv.y) * atlas[1] as f32;
                if texels > 4.0 {
                    most = most.max(pixels / texels);
                    vs.iter().for_each(|v| bounds.extend_with(v.pos));
                }
            }
        }
        (most, bounds)
    }

    // Text in a layer drawn bigger - a zoomed-in canvas, Easy mode's layout filling its panel -
    // is rasterized for the size it's drawn at: a texel to a pixel, where the glyphs used to be
    // the screen's, stretched (two pixels a texel at twice the size: blurry).
    #[test]
    fn text_in_a_scaled_layer_is_rasterized_at_its_drawn_size() {
        let (_, at_one) = pixels_per_texel(1.0);
        for scale in [1.0f32, 1.5, 2.0, 3.0] {
            let (r, bounds) = pixels_per_texel(scale);
            assert!(r > 0.7 && r < 1.2, "scale {scale}: {r} pixels a texel");
            // Where it was and as big, in points: only the rasterizing changed.
            let back = egui::Rect::from_min_max((bounds.min.to_vec2() / scale).to_pos2(), (bounds.max.to_vec2() / scale).to_pos2());
            for (a, b) in [(back.min, at_one.min), (back.max, at_one.max)] {
                assert!((a - b).length() < 1.5, "scale {scale}: the text moved, {back:?} vs {at_one:?}");
            }
        }
    }
}

#[cfg(test)]
mod big_tooltip_tests {
    // A tooltip too big to fit beside its widget is pushed back over it, and then the widget
    // isn't hovered any more: egui keeps such a tooltip open while the pointer is on the
    // widget. On the canvas the widget is in a zoomed, panned layer, and egui used to compare
    // the screen-space pointer with the widget's rect in its layer's space: the tooltip closed
    // and opened every other frame (the vendored egui's FLEXINPUT PATCH). Once shown, it stays.
    #[test]
    fn a_big_tooltip_over_its_widget_on_a_zoomed_layer_stays_open() {
        let ctx = egui::Context::default();
        let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(320.0, 180.0));
        let long: String = (0..40).map(|i| format!("line {i} of a long tooltip that wraps a bit
")).collect();
        let mut shown = Vec::new();
        for frame in 0..80 {
            let mut input = egui::RawInput { screen_rect: Some(screen), time: Some(frame as f64 / 60.0), ..Default::default() };
            if frame == 0 {
                input.events.push(egui::Event::PointerMoved(egui::pos2(30.0, 126.0)));
            }
            let _ = ctx.run(input, |ctx| {
                let layer = egui::LayerId::new(egui::Order::Middle, egui::Id::new("zoomed"));
                ctx.set_transform_layer(layer, egui::emath::TSTransform::new(egui::vec2(10.0, 20.0), 1.5));
                egui::Area::new(egui::Id::new("zoomed")).order(egui::Order::Middle).fixed_pos(egui::pos2(0.0, 0.0)).show(ctx, |ui| {
                    ui.add_space(60.0);
                    ui.button("hover me").on_hover_text(long.as_str());
                });
            });
            shown.push(ctx.memory(|m| m.areas().visible_layer_ids().iter().any(|l| l.order == egui::Order::Tooltip)));
        }
        let first = shown.iter().position(|s| *s).expect("the tooltip shows once the pointer rests");
        assert!(shown[first..].iter().all(|s| *s), "it blinked: {shown:?}");
    }
}
