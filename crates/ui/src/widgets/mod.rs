//! Custom reusable widgets: egui/snarl stock widgets often aren't flexible
//! enough for FlexInput's UI, so hand-rolled replacements collect here.
//! Future home of style flavors / theming hooks.

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
pub(crate) fn nav_highlight_painter(ctx: &egui::Context, id: egui::Id, clip: egui::Rect) -> egui::Painter {
    ctx.layer_painter(egui::LayerId::new(egui::Order::Foreground, id))
        .with_clip_rect(clip)
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
