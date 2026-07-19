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
