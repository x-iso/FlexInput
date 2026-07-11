//! Shared renderers for the per-device-source header controls
//! (Calibrate button + Hz, Deadzone slider, Gyro × slider) and the
//! per-keymouse-sink Mouse × slider.
//!
//! These widgets used to live exclusively in the snarl node header
//! (see [`crate::canvas::viewer`]); they're factored out so Easy mode
//! can reuse the same rendering and parameter-mutation behavior
//! without duplicating it. The Advanced-mode node header now calls
//! these helpers, so any future tweak only needs to happen in one
//! place.

use std::collections::HashMap;

use eframe::egui;
use egui_snarl::NodeId;
use serde_json::Value;

use crate::canvas::viewer::{device_source_caps, slider_label};

/// Render the Calibrate button + measured polling-rate display for a
/// `device.source` node. Sets `*calibrate_request = Some(node_id)`
/// when clicked. `device_rates_hz` is the map maintained by the app
/// (device_id → measured polling Hz).
pub fn render_calibrate_row(
    ui: &mut egui::Ui,
    node_id: NodeId,
    device_id: &str,
    device_rates_hz: &HashMap<String, u32>,
    calibrate_request: &mut Option<NodeId>,
) {
    let (has_dz, has_gy, has_st) = device_source_caps(device_id, true);
    if !(has_dz || has_gy || has_st) {
        return;
    }
    ui.horizontal(|ui| {
        if ui.small_button("Calibrate")
            .on_hover_text("Open the Device Calibration window")
            .clicked()
        {
            *calibrate_request = Some(node_id);
        }
        let hz = device_rates_hz.get(device_id).copied().unwrap_or(0);
        ui.label(egui::RichText::new(format!("{} Hz", hz))
            .color(egui::Color32::from_rgb(220, 160, 40))
            .small())
            .on_hover_text("Measured per-device polling rate (raw events/sec)");
    });
}

/// App-level defaults for game→physical rumble forwarding: neutral
/// pass-through (full 0..1 band, linear curve). The *user's* preferred
/// defaults live in Settings (`AppSettings::default_rumble_*`) — node widgets
/// fall back and double-click-reset to those; these constants back the
/// Settings fields' own serde defaults and the Settings row's reset.
pub const RUMBLE_DEF_FLOOR: f32 = 0.0;
pub const RUMBLE_DEF_MAX: f32 = 1.0;
pub const RUMBLE_DEF_EXP: f32 = 1.0;

/// The shaping that was hard-coded as the *implicit* fallback before rumble
/// became per-node/per-patch (floor 0.35, max 1.0, exp 0.6 — the old env-var
/// boost). A virtual pad sink saved before that change carries no rumble
/// params; `migrate_loaded_snarl` backfills these so the loaded patch keeps
/// the feel it had when it was saved, instead of silently jumping to the new
/// neutral default. (Legacy max == neutral max, so only floor/exp differ.)
pub const RUMBLE_LEGACY_FLOOR: f32 = 0.35;
pub const RUMBLE_LEGACY_MAX: f32 = 1.0;
pub const RUMBLE_LEGACY_EXP: f32 = 0.6;

/// A two-handle range slider for the rumble floor..max band, plus a compact
/// Curve value box on the side. Used by virtual gamepad sink nodes (everything
/// but `virtual.keymouse`).
///
/// The two handles set the *floor* (minimum strength for any non-zero game
/// rumble — lifts faint rumble so it's felt) and the *max* (ceiling on output
/// amplitude). The Curve exponent reshapes the response between them. These
/// affect ONLY the game/app rumble this virtual pad forwards to a physical pad
/// via Auto-Map; the user's own direct rumble wiring is sent at full scale.
///
/// Nodes whose params don't set the rumble keys follow (and double-click
/// reset to) the user's Settings defaults, passed in via `defaults`.
///
/// Returns true if any value changed this frame.
pub fn render_rumble_feedback_controls(
    ui: &mut egui::Ui,
    params: &mut HashMap<String, Value>,
    defaults: crate::canvas::DeviceParamDefaults,
) -> bool {
    let getp = |k: &str, d: f32| -> f32 {
        params.get(k).and_then(|v| v.as_f64()).map(|f| f as f32).unwrap_or(d)
    };
    let mut floor = getp("rumble_floor", defaults.rumble_floor).clamp(0.0, 1.0);
    let mut max = getp("rumble_max", defaults.rumble_max).clamp(0.0, 1.0);
    let mut exp = getp("rumble_exp", defaults.rumble_exp).clamp(0.2, 3.0);
    if max < floor { max = floor; }
    let (f0, m0, e0) = (floor, max, exp);

    ui.horizontal(|ui| {
        // Size the label cell to the actual text width so the slider sits right
        // after it (matches the tight label→control gap of the Mouse × row).
        // Short label ("Rumble") so the slider + curve box fit even on the
        // narrow Easy-mode output card; the hover text carries the full meaning.
        let cell_w = ui.painter()
            .layout_no_wrap(
                "Rumble".to_owned(),
                egui::TextStyle::Small.resolve(ui.style()),
                egui::Color32::WHITE,
            )
            .size().x + 2.0;
        slider_label(ui, "Rumble", cell_w);
        // Lay the rest out RIGHT-TO-LEFT so the curve box is allocated against
        // the row's right edge FIRST and can never overflow the card, then the
        // slider fills exactly the gap between the label and the curve box (so
        // they sit tight together, not with a wide gap). Compact-cap the slider
        // so it doesn't balloon on the wide canvas-node header.
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            // Small right inset so the curve box sits a few px inside the row's
            // right edge instead of butting against it (right-to-left, so this
            // space is consumed at the far right first).
            ui.add_space(5.0);
            // Compact Curve box: a mini graph whose line bends with the response
            // exponent. Drag to adjust; Ctrl+click to type; double-click resets.
            curve_box(ui, &mut exp, defaults.rumble_exp);
            ui.add_space(4.0);
            // `available_width()` here is the space left between the label and the
            // curve box; fill it (capped so the canvas-node slider stays compact).
            let slider_w = ui.available_width().clamp(40.0, 150.0);
            range_slider(ui, &mut floor, &mut max, defaults.rumble_floor, defaults.rumble_max, slider_w)
                .on_hover_text(
                    "Rumble a game/app sends to this virtual pad, when forwarded to a \
                     physical pad via Auto-Map. Left handle = floor (lifts faint rumble \
                     so it's felt); right handle = ceiling on output. Double-click to \
                     reset. Your own direct rumble wiring is sent at full scale.",
                );
        });
    });

    if max < floor { max = floor; }
    let mut changed = false;
    if (floor - f0).abs() > f32::EPSILON { changed = true; }
    if (max - m0).abs() > f32::EPSILON { changed = true; }
    if (exp - e0).abs() > f32::EPSILON { changed = true; }
    if changed {
        params.insert("rumble_floor".into(), Value::from(floor as f64));
        params.insert("rumble_max".into(), Value::from(max as f64));
        params.insert("rumble_exp".into(), Value::from(exp as f64));
    }
    changed
}

/// A horizontal slider with two rectangular "clamp" handles defining a
/// `[lo, hi]` band within `0.0..=1.0`. Styled to match the regular egui
/// sliders (same rail, same widget visuals); the two handles read like a clamp
/// — the floor handle is rounded on its left edge, the max handle rounded on
/// its right edge, their inner (facing) edges square. The handles cannot
/// cross. Double-clicking the track resets both to `(def_lo, def_hi)`. Returns
/// the combined `Response` for hover-text / change detection.
pub(crate) fn range_slider(
    ui: &mut egui::Ui,
    lo: &mut f32,
    hi: &mut f32,
    def_lo: f32,
    def_hi: f32,
    width: f32,
) -> egui::Response {
    // Match egui's slider metrics so this reads as a native slider. egui's
    // round handle has radius ≈ interact_size.y / 2.5; size our rectangular
    // clamp jaws to that diameter so they're no bigger than a normal handle.
    let spacing = ui.style().spacing.clone();
    let rail_h = spacing.slider_rail_height;
    // The row is the full interact height; the handle stands proud of the thin
    // rail (taller than the rail, like a native slider handle) but leaves a
    // small margin inside the row. Width is a slim clamp jaw.
    let row_h = spacing.interact_size.y;
    let handle_h = (row_h - 2.0).max(12.0);
    let handle_w = 7.0_f32;
    let rounding = 2.0_f32;

    // Caller-supplied width (it reserves room for the curve box + gap first, so
    // the slider fills exactly the space between the label and the curve box).
    let desired = egui::vec2(width.max(40.0), row_h);
    let (rect, mut resp) = ui.allocate_exact_size(desired, egui::Sense::click_and_drag());
    let visuals = ui.style().visuals.clone();

    // Inset the usable track by a FULL handle width on each side: each clamp
    // handle's body extends outward from its value position by `handle_w`, so a
    // value at 0.0 (or 1.0) would otherwise paint past the widget rect and clip.
    let inset = handle_w;
    let track_left = rect.left() + inset;
    let track_right = rect.right() - inset;
    let track_w = (track_right - track_left).max(1.0);
    let cy = rect.center().y;
    let x_of = |v: f32| track_left + v.clamp(0.0, 1.0) * track_w;
    let v_of = |x: f32| ((x - track_left) / track_w).clamp(0.0, 1.0);

    // Which handle to grab: on press, pick the nearer one and remember it for
    // the duration of the drag (so a handle dragged past the other doesn't
    // swap mid-gesture).
    let grab_id = resp.id.with("__grab");
    if resp.drag_started() || resp.clicked() {
        if let Some(pos) = resp.interact_pointer_pos() {
            let d_lo = (pos.x - x_of(*lo)).abs();
            let d_hi = (pos.x - x_of(*hi)).abs();
            let pick_hi = if (d_lo - d_hi).abs() < 0.5 {
                pos.x >= x_of(*hi) // coincident handles: pick by side
            } else {
                d_hi < d_lo
            };
            ui.memory_mut(|m| m.data.insert_temp(grab_id, pick_hi));
        }
    }
    if resp.dragged() {
        if let Some(pos) = resp.interact_pointer_pos() {
            let pick_hi = ui.memory(|m| m.data.get_temp::<bool>(grab_id).unwrap_or(false));
            let v = v_of(pos.x);
            if pick_hi { *hi = v.max(*lo); } else { *lo = v.min(*hi); }
            resp.mark_changed();
        }
    }
    if resp.double_clicked() {
        *lo = def_lo.clamp(0.0, 1.0);
        *hi = def_hi.clamp(0.0, 1.0);
        if *hi < *lo { *hi = *lo; }
        resp.mark_changed();
    }

    // Rail + filled band, matching egui's slider rail look: same fill and the
    // widget corner radius (small rounded corners — NOT a full pill/stadium).
    let painter = ui.painter_at(rect);
    let rail_cr = visuals.widgets.inactive.corner_radius;
    let rail = egui::Rect::from_min_max(
        egui::pos2(track_left, cy - rail_h * 0.5),
        egui::pos2(track_right, cy + rail_h * 0.5),
    );
    painter.rect_filled(rail, rail_cr, visuals.widgets.inactive.bg_fill);
    let band = egui::Rect::from_min_max(
        egui::pos2(x_of(*lo), cy - rail_h * 0.5),
        egui::pos2(x_of(*hi), cy + rail_h * 0.5),
    );
    painter.rect_filled(band, 0.0, visuals.selection.bg_fill);

    // Two clamp handles. `lo` is rounded on its LEFT (outer) edge, square on
    // the right; `hi` is the mirror. Each handle's body sits to the OUTER side
    // of its value position so their inner faces meet at the band edges.
    for (v, is_hi) in [(*lo, false), (*hi, true)] {
        let cx = x_of(v);
        let hr = if is_hi {
            egui::Rect::from_min_max(
                egui::pos2(cx, cy - handle_h * 0.5),
                egui::pos2(cx + handle_w, cy + handle_h * 0.5),
            )
        } else {
            egui::Rect::from_min_max(
                egui::pos2(cx - handle_w, cy - handle_h * 0.5),
                egui::pos2(cx, cy + handle_h * 0.5),
            )
        };
        let hovered = resp.hovered()
            && resp.hover_pos().map(|p| hr.expand(2.0).contains(p)).unwrap_or(false);
        let dragging = resp.dragged()
            && ui.memory(|m| m.data.get_temp::<bool>(grab_id).unwrap_or(false)) == is_hi;
        let wv = if dragging { visuals.widgets.active }
            else if hovered { visuals.widgets.hovered }
            else { visuals.widgets.inactive };
        // Round only the outer corners; inner (facing) corners square → clamp look.
        let cr = if is_hi {
            egui::CornerRadius { nw: 0, sw: 0, ne: rounding as u8, se: rounding as u8 }
        } else {
            egui::CornerRadius { nw: rounding as u8, sw: rounding as u8, ne: 0, se: 0 }
        };
        painter.rect(hr, cr, wv.bg_fill, wv.fg_stroke, egui::StrokeKind::Inside);
    }

    resp
}

/// A compact "curve" value box for a response exponent in `0.2..=3.0`.
///
/// Instead of a labelled number, it draws a small graph of `y = x^exp` over the
/// unit square so the *shape* communicates the effect (a line that bows up =
/// boosts the low/felt end; straight = linear; bows down = softens the low end).
///
/// Interaction:
///   - **Drag** (vertical): bend the curve. Dragging up lowers the exponent
///     (more low-end boost); dragging down raises it.
///   - **Ctrl+click**: switch to an inline numeric edit (a `DragValue`) for
///     precise entry; it reverts to the graph when it loses focus.
///   - **Double-click**: reset to `def`.
pub(crate) fn curve_box(ui: &mut egui::Ui, exp: &mut f32, def: f32) {
    const LO: f32 = 0.2;
    const HI: f32 = 3.0;
    *exp = exp.clamp(LO, HI);

    let size = egui::vec2(34.0, ui.style().spacing.interact_size.y.min(18.0));
    let edit_id = ui.id().with("__curve_edit");
    let editing = ui.memory(|m| m.data.get_temp::<bool>(edit_id).unwrap_or(false));

    if editing {
        // Inline numeric edit. Stays until the DragValue loses focus.
        let r = ui.add_sized(size, egui::DragValue::new(exp)
            .speed(0.02)
            .range(LO..=HI)
            .fixed_decimals(2));
        *exp = exp.clamp(LO, HI);
        if r.lost_focus() || (!r.has_focus() && !r.hovered() && ui.input(|i| i.pointer.any_click())) {
            ui.memory_mut(|m| m.data.insert_temp(edit_id, false));
        }
        return;
    }

    let (rect, mut resp) = ui.allocate_exact_size(size, egui::Sense::click_and_drag());
    let visuals = ui.style().visuals.clone();

    // Ctrl+click → numeric edit mode.
    if resp.clicked() && ui.input(|i| i.modifiers.command || i.modifiers.ctrl) {
        ui.memory_mut(|m| m.data.insert_temp(edit_id, true));
        resp.request_focus();
    }
    // Drag to bend. Map vertical drag to log-exponent so the feel is symmetric
    // around 1.0 (equal travel to halve vs. double the exponent).
    if resp.dragged() {
        let dy = resp.drag_delta().y;
        if dy != 0.0 {
            // ~120 px of travel spans the full LO..HI range. Up (negative dy)
            // lowers the exponent toward LO (more boost).
            let ln = exp.ln() + (dy / 120.0) * (HI.ln() - LO.ln());
            *exp = ln.exp().clamp(LO, HI);
            resp.mark_changed();
        }
    }
    if resp.double_clicked() {
        *exp = def.clamp(LO, HI);
        resp.mark_changed();
    }

    // Paint: framed box + the curve y = x^exp (y up).
    let painter = ui.painter_at(rect);
    let bg = if resp.hovered() { visuals.widgets.hovered.bg_fill } else { visuals.widgets.inactive.bg_fill };
    let frame_stroke = if resp.hovered() { visuals.widgets.hovered.fg_stroke } else { visuals.widgets.inactive.bg_stroke };
    painter.rect(rect, 3.0, bg, frame_stroke, egui::StrokeKind::Inside);
    let pad = 3.0_f32;
    let plot = rect.shrink(pad);
    let n = 16usize;
    let mut pts = Vec::with_capacity(n + 1);
    for i in 0..=n {
        let x = i as f32 / n as f32;
        let y = x.powf(*exp);
        pts.push(egui::pos2(
            plot.left() + x * plot.width(),
            plot.bottom() - y * plot.height(),
        ));
    }
    painter.add(egui::Shape::line(pts, egui::Stroke::new(1.4, visuals.selection.bg_fill)));

    resp.on_hover_text(
        "Response curve for forwarded rumble (exponent). Drag to bend: up = boost \
         the low/felt end, down = soften it; straight = linear. Ctrl+click to type \
         a value; double-click to reset.",
    );
}
