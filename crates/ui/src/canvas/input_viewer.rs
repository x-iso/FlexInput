//! Input Viewer module body — a live schematic controller board.
//!
//! Draws the wired device's buttons / sticks / triggers as glyph chips and
//! scopes lighting up from `live_signals`. Fixed-aspect board painted by
//! [`paint_viewer_board`] so the node body and the pinned (layout / overlay)
//! renderer share one code path; the pinned variant letterboxes the board
//! into its container (whole-container widget per the pinned-widget scaling
//! contract — no `apply_widget_scale`).
//!
//! Cluster visibility is presence-based: a cluster renders when the resolved
//! device currently exposes its pins in `live_signals` (touchpad strip, mute,
//! capture, paddles), so a DS4 shows its pad and an Xbox pad doesn't. With no
//! device wired, the base board renders dimmed with a hint.

use std::collections::HashMap;

use egui_snarl::{NodeId, Snarl};
use flexinput_core::Signal;

use super::remapper_icons::{self, Skin};
use super::NodeData;
use super::viewer::{
    register_exposable_element, remapper_resolve_skin, remapper_upstream_device_id,
    rasterize_svg_recolored, AutomapGlowParent,
};

type LiveSignals = HashMap<(String, String), Signal>;

/// Design-space board size (logical px at scale 1). Everything inside is laid
/// out in fractions of this rect, so any target rect with the same aspect
/// renders identically.
pub(crate) const BOARD_W: f32 = 420.0;
pub(crate) const BOARD_H: f32 = 260.0;

// ── Node body ─────────────────────────────────────────────────────────────────

pub(crate) fn show_input_viewer_body(
    node_id: NodeId,
    ui: &mut egui::Ui,
    snarl: &mut Snarl<NodeData>,
    live_signals: &LiveSignals,
    automap_parent: Option<&AutomapGlowParent<'_>>,
) {
    // Skin is picked in the node HEADER (shared selector with the Remapper).
    let skin_param = snarl
        .get_node(node_id)
        .and_then(|n| n.params.get("skin").and_then(|v| v.as_str()))
        .unwrap_or("auto")
        .to_string();
    let skin = remapper_resolve_skin(snarl, node_id, &skin_param, automap_parent);
    let dev_id = remapper_upstream_device_id(snarl, node_id, 0, automap_parent);

    let (rect, _) = ui.allocate_exact_size(egui::vec2(BOARD_W, BOARD_H), egui::Sense::hover());
    paint_viewer_board(ui, rect, node_id.0, dev_id.as_deref(), skin, live_signals);

    register_exposable_element(ui, node_id, "viewer", rect);
}

// ── Board painter (shared by body + pinned renderer) ──────────────────────────

/// Paint the schematic board into `rect` (any size; the caller is responsible
/// for keeping the BOARD_W:BOARD_H aspect — see [`letterbox`]).
pub(crate) fn paint_viewer_board(
    ui: &mut egui::Ui,
    rect: egui::Rect,
    node_uid: usize,
    dev_id: Option<&str>,
    skin: Skin,
    live_signals: &LiveSignals,
) {
    let ctx = ui.ctx().clone();
    let painter = ui.painter_at(rect);
    let s = rect.width() / BOARD_W; // uniform scale (aspect preserved)
    let at = |fx: f32, fy: f32| egui::pos2(rect.left() + fx * rect.width(), rect.top() + fy * rect.height());

    let dev = dev_id.unwrap_or("");
    let has = |pin: &str| !dev.is_empty() && live_signals.contains_key(&(dev.to_string(), pin.to_string()));
    let readf = |pin: &str| -> f32 {
        live_signals.get(&(dev.to_string(), pin.to_string())).map(|s| s.as_float()).unwrap_or(0.0)
    };
    let readb = |pin: &str| -> bool {
        live_signals.get(&(dev.to_string(), pin.to_string())).map(|s| s.as_bool()).unwrap_or(false)
    };
    // Smoothed 0..1 glow for a bool pin (up-fast / down-slow, mirrors the
    // canvas pin glow so button flashes read at a glance without strobing).
    let glow = |pin: &str| -> f32 {
        let target = if readb(pin) { 1.0 } else { 0.0 };
        glow_smoothed(&ctx, node_uid, pin, target)
    };

    // Board plate.
    let plate = ui.visuals().extreme_bg_color;
    painter.rect_filled(rect, 10.0 * s, plate.gamma_multiply(0.85));
    painter.rect_stroke(
        rect,
        10.0 * s,
        egui::Stroke::new(1.0, ui.visuals().widgets.noninteractive.bg_stroke.color),
        egui::StrokeKind::Inside,
    );

    let accent = egui::Color32::from_rgb(255, 196, 90);

    // ── Triggers: vertical fill bars in the top corners ──
    let lt = if has("left_trigger") { readf("left_trigger") } else if readb("btn_lt_dig") { 1.0 } else { 0.0 };
    let rt = if has("right_trigger") { readf("right_trigger") } else if readb("btn_rt_dig") { 1.0 } else { 0.0 };
    paint_trigger_bar(&painter, egui::Rect::from_min_max(at(0.045, 0.06), at(0.105, 0.30)), lt, accent, s);
    paint_trigger_bar(&painter, egui::Rect::from_min_max(at(0.895, 0.06), at(0.955, 0.30)), rt, accent, s);

    // ── Bumpers ──
    glyph_chip(ui, &painter, skin, "btn_lb", at(0.185, 0.12), 26.0 * s, glow("btn_lb"), accent);
    glyph_chip(ui, &painter, skin, "btn_rb", at(0.815, 0.12), 26.0 * s, glow("btn_rb"), accent);

    // ── Center column: touchpad strip (when present) or just menu chips ──
    let has_touch = has("touch1_x");
    if has_touch {
        let pad = egui::Rect::from_min_max(at(0.36, 0.055), at(0.64, 0.315));
        paint_touchpad_strip(&painter, pad, dev, live_signals, glow("btn_touchpad"), accent, s);
        glyph_chip(ui, &painter, skin, "btn_back",  at(0.305, 0.12), 20.0 * s, glow("btn_back"), accent);
        glyph_chip(ui, &painter, skin, "btn_start", at(0.695, 0.12), 20.0 * s, glow("btn_start"), accent);
    } else {
        glyph_chip(ui, &painter, skin, "btn_back",  at(0.42, 0.12), 20.0 * s, glow("btn_back"), accent);
        glyph_chip(ui, &painter, skin, "btn_start", at(0.58, 0.12), 20.0 * s, glow("btn_start"), accent);
    }

    // ── Guide cluster ──
    glyph_chip(ui, &painter, skin, "btn_guide", at(0.50, 0.44), 26.0 * s, glow("btn_guide"), accent);
    if has("btn_capture") {
        glyph_chip(ui, &painter, skin, "btn_capture", at(0.41, 0.46), 18.0 * s, glow("btn_capture"), accent);
    }
    if has("btn_mute") {
        glyph_chip(ui, &painter, skin, "btn_mute", at(0.59, 0.46), 18.0 * s, glow("btn_mute"), accent);
    }

    // ── D-pad: ONE glyph whose art switches to the pressed direction ──
    // (the icon set ships a neutral cross + one variant per direction; on a
    // diagonal the direction that rose most recently wins via glow ordering).
    {
        let dirs = [
            ("dpad_up", glow("dpad_up")), ("dpad_down", glow("dpad_down")),
            ("dpad_left", glow("dpad_left")), ("dpad_right", glow("dpad_right")),
        ];
        let (pin, g) = dirs.iter()
            .filter(|(p, _)| readb(p))
            .max_by(|a, b| a.1.total_cmp(&b.1))
            .map(|(p, g)| (*p, *g))
            .unwrap_or(("dpad", dirs.iter().map(|(_, g)| *g).fold(0.0f32, f32::max)));
        glyph_chip(ui, &painter, skin, pin, at(0.165, 0.46), 46.0 * s, g, accent);
    }

    // ── Face diamond (right) ──
    glyph_chip(ui, &painter, skin, "btn_north", at(0.835, 0.345), 24.0 * s, glow("btn_north"), accent);
    glyph_chip(ui, &painter, skin, "btn_south", at(0.835, 0.575), 24.0 * s, glow("btn_south"), accent);
    glyph_chip(ui, &painter, skin, "btn_west",  at(0.755, 0.46),  24.0 * s, glow("btn_west"), accent);
    glyph_chip(ui, &painter, skin, "btn_east",  at(0.915, 0.46),  24.0 * s, glow("btn_east"), accent);

    // ── Stick scopes ──
    let ls = stick_vec(live_signals, dev, "left_stick", "left_stick_x", "left_stick_y");
    let rs = stick_vec(live_signals, dev, "right_stick", "right_stick_x", "right_stick_y");
    paint_stick_scope(&painter, at(0.345, 0.735), 36.0 * s, ls, glow("btn_ls"), ui, accent);
    paint_stick_scope(&painter, at(0.655, 0.735), 36.0 * s, rs, glow("btn_rs"), ui, accent);

    // ── Rear paddles (when present) ──
    for (pin, fx, label) in [
        ("btn_paddle_l1", 0.055, "P1"), ("btn_paddle_l2", 0.135, "P2"),
        ("btn_paddle_r1", 0.945, "P1"), ("btn_paddle_r2", 0.865, "P2"),
    ] {
        if has(pin) {
            glyph_chip(ui, &painter, skin, pin, at(fx, 0.92), 18.0 * s, glow(pin), accent);
            painter.text(
                at(fx, 0.92), egui::Align2::CENTER_CENTER, label,
                egui::FontId::proportional(7.0 * s), egui::Color32::from_gray(210),
            );
        }
    }

    // ── No-device dim + hint ──
    if dev.is_empty() {
        painter.rect_filled(rect, 10.0 * s, egui::Color32::from_black_alpha(110));
        painter.text(
            rect.center(), egui::Align2::CENTER_CENTER,
            "No device — wire an AutoMap source",
            egui::FontId::proportional(12.0 * s),
            egui::Color32::from_gray(170),
        );
        return;
    }

    // Live animation cadence (throttled; suppressed while backgrounded, where
    // the bg repaint rate — or the overlay pacing — takes over).
    crate::app::request_repaint_throttled(&ctx);
}

/// Letterbox helper: the largest BOARD-aspect rect centered in `container`.
pub(crate) fn letterbox(container: egui::Rect) -> egui::Rect {
    let aspect = BOARD_W / BOARD_H;
    let (w, h) = if container.width() / container.height() > aspect {
        (container.height() * aspect, container.height())
    } else {
        (container.width(), container.width() / aspect)
    };
    egui::Rect::from_center_size(container.center(), egui::vec2(w, h))
}

// ── Element painters ──────────────────────────────────────────────────────────

fn paint_trigger_bar(painter: &egui::Painter, r: egui::Rect, value: f32, accent: egui::Color32, s: f32) {
    let v = value.clamp(0.0, 1.0);
    painter.rect_filled(r, 3.0 * s, egui::Color32::from_gray(45));
    if v > 0.005 {
        let fill = egui::Rect::from_min_max(egui::pos2(r.left(), r.bottom() - r.height() * v), r.max);
        painter.rect_filled(fill, 3.0 * s, accent.gamma_multiply(0.35 + 0.65 * v));
    }
    painter.rect_stroke(r, 3.0 * s, egui::Stroke::new(1.0, egui::Color32::from_gray(90)), egui::StrokeKind::Inside);
}

fn paint_touchpad_strip(
    painter: &egui::Painter,
    r: egui::Rect,
    dev: &str,
    live_signals: &LiveSignals,
    click_glow: f32,
    accent: egui::Color32,
    s: f32,
) {
    painter.rect_filled(r, 5.0 * s, egui::Color32::from_gray(40));
    let stroke_col = if click_glow > 0.02 {
        accent.gamma_multiply(0.4 + 0.6 * click_glow)
    } else {
        egui::Color32::from_gray(90)
    };
    painter.rect_stroke(r, 5.0 * s, egui::Stroke::new(1.0 + click_glow * 1.5, stroke_col), egui::StrokeKind::Inside);

    let readf = |pin: &str| -> f32 {
        live_signals.get(&(dev.to_string(), pin.to_string())).map(|s| s.as_float()).unwrap_or(0.0)
    };
    let readb = |pin: &str| -> bool {
        live_signals.get(&(dev.to_string(), pin.to_string())).map(|s| s.as_bool()).unwrap_or(false)
    };
    for (px, py, pa) in [("touch1_x", "touch1_y", "touch1_active"), ("touch2_x", "touch2_y", "touch2_active")] {
        if !readb(pa) { continue; }
        let (ux, uy) = flexinput_core::touchzones::pad_point_to_unit(readf(px), readf(py));
        let dot = egui::pos2(r.left() + ux * r.width(), r.top() + uy * r.height());
        painter.circle_filled(dot, 3.5 * s, accent);
    }
}

fn paint_stick_scope(
    painter: &egui::Painter,
    center: egui::Pos2,
    radius: f32,
    v: egui::Vec2,
    click_glow: f32,
    ui: &egui::Ui,
    accent: egui::Color32,
) {
    let faint = egui::Stroke::new(0.5, egui::Color32::from_gray(70));
    painter.circle_filled(center, radius, egui::Color32::from_gray(38));
    painter.line_segment([center - egui::vec2(radius, 0.0), center + egui::vec2(radius, 0.0)], faint);
    painter.line_segment([center - egui::vec2(0.0, radius), center + egui::vec2(0.0, radius)], faint);
    let ring_col = if click_glow > 0.02 {
        accent.gamma_multiply(0.4 + 0.6 * click_glow)
    } else {
        ui.visuals().widgets.noninteractive.bg_stroke.color
    };
    painter.circle_stroke(center, radius, egui::Stroke::new(1.0 + click_glow * 2.0, ring_col));

    // Screen-space dot: +y on the stick is UP, screen y grows down.
    let mag = v.length().min(1.0);
    let dot = center + egui::vec2(v.x, -v.y) * (radius - 5.0);
    let dot_col = if mag > 0.02 { accent } else { egui::Color32::from_gray(140) };
    painter.circle_filled(dot, 4.0, dot_col);
}

fn stick_vec(live_signals: &LiveSignals, dev: &str, vec_pin: &str, x_pin: &str, y_pin: &str) -> egui::Vec2 {
    if let Some(Signal::Vec2(v)) = live_signals.get(&(dev.to_string(), vec_pin.to_string())) {
        return egui::vec2(v.x, v.y);
    }
    let rf = |pin: &str| live_signals.get(&(dev.to_string(), pin.to_string())).map(|s| s.as_float()).unwrap_or(0.0);
    egui::vec2(rf(x_pin), rf(y_pin))
}

/// Glyph chip: cached rasterized SVG for the pin, dimmed at rest, with an
/// accent halo scaling with glow.
fn glyph_chip(
    ui: &egui::Ui,
    painter: &egui::Painter,
    skin: Skin,
    pin: &str,
    center: egui::Pos2,
    size: f32,
    glow: f32,
    accent: egui::Color32,
) {
    if glow > 0.02 {
        painter.circle_filled(center, size * 0.72, accent.gamma_multiply(0.30 * glow));
        painter.circle_stroke(center, size * 0.72, egui::Stroke::new(1.0, accent.gamma_multiply(0.7 * glow)));
    }
    let Some(tex) = glyph_texture(ui, skin, pin, size) else {
        // No glyph mapped: text pill fallback.
        painter.text(
            center, egui::Align2::CENTER_CENTER, pin,
            egui::FontId::proportional(size * 0.35), egui::Color32::from_gray(160),
        );
        return;
    };
    let r = egui::Rect::from_center_size(center, egui::vec2(size, size));
    // Dim at rest, full brightness under glow (tint multiplies the texture).
    let b = (140.0 + 115.0 * glow) as u8;
    painter.image(
        tex.id(), r,
        egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
        egui::Color32::from_gray(b),
    );
}

/// Rasterize + cache a pin glyph at the given logical size (keyed by physical
/// px so DPI changes re-rasterize). Preserves the SVG's own colors.
fn glyph_texture(ui: &egui::Ui, skin: Skin, pin: &str, logical_size: f32) -> Option<egui::TextureHandle> {
    let px = (logical_size * ui.ctx().pixels_per_point()).round().max(4.0) as u32;
    let key = egui::Id::new(("iv_glyph", skin.as_str(), pin, px));
    if let Some(tex) = ui.ctx().data(|d| d.get_temp::<egui::TextureHandle>(key)) {
        return Some(tex);
    }
    let bytes = remapper_icons::pin_svg(skin, pin)?;
    let text = std::str::from_utf8(bytes).ok()?;
    let img = rasterize_svg_recolored(text, px, px, "override", egui::Color32::TRANSPARENT)?;
    let tex = ui.ctx().load_texture(format!("iv_glyph_{pin}"), img, egui::TextureOptions::LINEAR);
    ui.ctx().data_mut(|d| d.insert_temp(key, tex.clone()));
    Some(tex)
}

/// Up-fast / down-slow smoothing, mirroring the canvas `pin_glow_smoothed`
/// but keyed by pin id string (the board has no pin indices).
fn glow_smoothed(ctx: &egui::Context, node_uid: usize, pin: &str, target: f32) -> f32 {
    let key = egui::Id::new(("iv_glow", node_uid, pin));
    let prev = ctx.data(|d| d.get_temp::<f32>(key)).unwrap_or(0.0);
    let dt = ctx.input(|i| i.stable_dt).clamp(0.0, 0.1);
    let rate = if target > prev { 30.0 } else { 6.0 };
    let smoothed = prev + (target - prev) * (1.0 - (-rate * dt).exp());
    ctx.data_mut(|d| d.insert_temp(key, smoothed));
    smoothed
}
