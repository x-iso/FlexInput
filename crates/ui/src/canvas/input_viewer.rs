//! Input Viewer module body — a live schematic controller board.
//!
//! Draws the wired device's buttons / sticks / triggers as glyph chips and
//! scopes lighting up from `live_signals`. Fixed-aspect board painted by
//! [`paint_viewer_board`] so the node body and the pinned (layout / overlay)
//! renderer share one code path; the pinned variant letterboxes the board
//! into its container (whole-container widget per the pinned-widget scaling
//! contract — no `apply_widget_scale`).
//!
//! Board geometry is per skin family, baked from the user-editable layout
//! SVGs `app/assets/input_viewer_{ps,x,sw}_layout.svg` (420×201 canvas,
//! element ids = pin ids). Edit those files, then re-derive the [`Layout`]
//! tables (each shape's group transform applied to its geometry).
//!
//! Cluster visibility is presence-based on top of the layout: a cluster
//! renders when the layout has a slot for it AND the resolved device
//! currently exposes its pins in `live_signals` (touchpad strip, mute,
//! capture, paddles). With no device wired, the base board renders dimmed
//! with a hint.

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
/// out in design px of this rect, so any target rect with the same aspect
/// renders identically.
pub(crate) const BOARD_W: f32 = 420.0;
pub(crate) const BOARD_H: f32 = 201.0;

// ── Per-skin layouts (design px, from the layout SVGs) ────────────────────────

/// Skin-specific slot geometry. Chips are `(cx, cy, glyph_size)`; the touchpad
/// slot is `(x, y, w, h)`. Shared geometry (triggers, bumpers, face diamond,
/// right stick, paddles) is identical across the three source SVGs and lives
/// inline in [`paint_viewer_board`].
struct Layout {
    touchpad: Option<(f32, f32, f32, f32)>,
    back: (f32, f32, f32),
    start: (f32, f32, f32),
    guide: (f32, f32, f32),
    capture: (f32, f32, f32),
    mute: Option<(f32, f32, f32)>,
    dpad: (f32, f32, f32),
    left_stick: (f32, f32),
}

/// PlayStation: touchpad strip top-center, symmetric sticks, d-pad top-left,
/// create/PS/mute stacked in the middle.
const PS_LAYOUT: Layout = Layout {
    touchpad: Some((151.2, 10.8, 117.6, 67.6)),
    back: (128.1, 20.8, 20.0),
    start: (291.9, 20.8, 20.0),
    guide: (210.0, 145.0, 26.0),
    capture: (210.0, 105.0, 18.0),
    mute: Some((210.0, 177.0, 18.0)),
    dpad: (92.7, 70.9, 72.0),
    left_stick: (144.9, 150.0),
};

/// Xbox: no touchpad, asymmetric sticks (left stick top-left, d-pad low),
/// big guide up top with share/mute below it.
const X_LAYOUT: Layout = Layout {
    touchpad: None,
    back: (154.9, 44.9, 20.0),
    start: (264.9, 44.9, 20.0),
    guide: (210.0, 47.9, 35.6),
    capture: (210.0, 105.0, 18.0),
    mute: Some((210.0, 177.0, 18.0)),
    dpad: (144.9, 150.0, 72.0),
    left_stick: (92.7, 70.9),
};

/// Switch Pro: Xbox stick arrangement, capture/home as a symmetric pair, no
/// mute.
const SW_LAYOUT: Layout = Layout {
    touchpad: None,
    back: (154.9, 44.9, 20.0),
    start: (264.9, 44.9, 20.0),
    guide: (242.6, 78.3, 24.5),
    capture: (177.4, 78.3, 24.5),
    mute: None,
    dpad: (144.9, 150.0, 72.0),
    left_stick: (92.7, 70.9),
};

// ── Style ─────────────────────────────────────────────────────────────────────

/// User-tunable board style, persisted under the `iv_style` param (only when
/// changed — absent = defaults). All colors carry alpha, so the plate can go
/// fully transparent for overlay use.
#[derive(Clone, Copy, PartialEq)]
pub(crate) struct IvStyle {
    /// Board plate fill (incl. transparency).
    pub bg: egui::Color32,
    /// Highlight for pressed elements (halos, trigger fill, dots, rings).
    pub accent: egui::Color32,
    /// Element tint: multiplies the glyph art; brightness still ramps with glow.
    pub tint: egui::Color32,
    /// Board outline stroke color.
    pub outline: egui::Color32,
    /// Board outline stroke width (0 = no outline).
    pub outline_w: f32,
}

impl Default for IvStyle {
    fn default() -> Self {
        Self {
            bg: egui::Color32::from_rgba_unmultiplied(20, 20, 23, 235),
            accent: egui::Color32::from_rgb(255, 196, 90),
            tint: egui::Color32::WHITE,
            outline: egui::Color32::from_rgba_unmultiplied(70, 70, 75, 255),
            outline_w: 1.0,
        }
    }
}

fn color_to_json(c: egui::Color32) -> serde_json::Value {
    serde_json::json!([c.r(), c.g(), c.b(), c.a()])
}

fn color_from_json(v: Option<&serde_json::Value>) -> Option<egui::Color32> {
    let a = v?.as_array()?;
    let ch = |i: usize| a.get(i).and_then(|x| x.as_u64()).map(|x| x.min(255) as u8);
    Some(egui::Color32::from_rgba_unmultiplied(ch(0)?, ch(1)?, ch(2)?, ch(3)?))
}

/// Resolve the style from a node's `iv_style` param (defaults when absent).
pub(crate) fn iv_style_of(node: Option<&NodeData>) -> IvStyle {
    let mut s = IvStyle::default();
    let Some(obj) = node.and_then(|n| n.params.get("iv_style")) else { return s };
    if let Some(c) = color_from_json(obj.get("bg")) { s.bg = c; }
    if let Some(c) = color_from_json(obj.get("accent")) { s.accent = c; }
    if let Some(c) = color_from_json(obj.get("tint")) { s.tint = c; }
    if let Some(c) = color_from_json(obj.get("outline")) { s.outline = c; }
    if let Some(w) = obj.get("outline_w").and_then(|v| v.as_f64()) { s.outline_w = w as f32; }
    s
}

fn iv_style_to_json(s: &IvStyle) -> serde_json::Value {
    serde_json::json!({
        "bg": color_to_json(s.bg),
        "accent": color_to_json(s.accent),
        "tint": color_to_json(s.tint),
        "outline": color_to_json(s.outline),
        "outline_w": s.outline_w,
    })
}

/// 🎨 style popup: background (with transparency), highlight, element tint,
/// and outline color/width, plus Reset. Edited on the node body; the pinned
/// board reads the same param.
fn show_style_menu(node_id: NodeId, ui: &mut egui::Ui, snarl: &mut Snarl<NodeData>) {
    let mut style = iv_style_of(snarl.get_node(node_id));
    let before = style;
    let mut reset = false;
    let btn = egui::Button::new(egui::RichText::new("🎨").size(13.0));
    egui::containers::menu::MenuButton::from_button(btn).ui(ui, |ui: &mut egui::Ui| {
        ui.set_min_width(190.0);
        egui::Grid::new((node_id, "iv_style_grid")).num_columns(2).spacing([8.0, 6.0]).show(ui, |ui| {
            ui.label("Background");
            ui.color_edit_button_srgba(&mut style.bg)
                .on_hover_text("Board plate color — lower the alpha for a see-through board on the overlay.");
            ui.end_row();
            ui.label("Highlight");
            ui.color_edit_button_srgba(&mut style.accent)
                .on_hover_text("Pressed-element color: halos, trigger fill, stick/touch dots.");
            ui.end_row();
            ui.label("Element tint");
            ui.color_edit_button_srgba(&mut style.tint)
                .on_hover_text("Multiplies the button art; brightness still rises on press.");
            ui.end_row();
            ui.label("Outline");
            ui.horizontal(|ui| {
                ui.color_edit_button_srgba(&mut style.outline);
                ui.add(egui::DragValue::new(&mut style.outline_w).range(0.0..=6.0).speed(0.05))
                    .on_hover_text("Board outline width (0 = none).");
            });
            ui.end_row();
        });
        ui.separator();
        if ui.button("Reset to defaults").clicked() {
            reset = true;
            ui.close();
        }
    });
    if reset {
        if let Some(node) = snarl.get_node_mut(node_id) {
            node.params.remove("iv_style");
        }
    } else if style != before {
        if let Some(node) = snarl.get_node_mut(node_id) {
            node.params.insert("iv_style".to_string(), iv_style_to_json(&style));
        }
    }
}

// ── Node body ─────────────────────────────────────────────────────────────────

pub(crate) fn show_input_viewer_body(
    node_id: NodeId,
    ui: &mut egui::Ui,
    snarl: &mut Snarl<NodeData>,
    live_signals: &LiveSignals,
    automap_parent: Option<&AutomapGlowParent<'_>>,
) {
    // Skin is picked in the node HEADER (shared selector with the Remapper);
    // the 🎨 popup holds the style controls.
    let skin_param = snarl
        .get_node(node_id)
        .and_then(|n| n.params.get("skin").and_then(|v| v.as_str()))
        .unwrap_or("auto")
        .to_string();
    let skin = remapper_resolve_skin(snarl, node_id, &skin_param, automap_parent);
    let dev_id = remapper_upstream_device_id(snarl, node_id, 0, automap_parent);

    ui.allocate_ui_with_layout(
        egui::vec2(BOARD_W, 20.0),
        egui::Layout::right_to_left(egui::Align::Center),
        |ui| show_style_menu(node_id, ui, snarl),
    );

    let style = iv_style_of(snarl.get_node(node_id));
    let (rect, _) = ui.allocate_exact_size(egui::vec2(BOARD_W, BOARD_H), egui::Sense::hover());
    paint_viewer_board(ui, rect, node_id.0, dev_id.as_deref(), skin, &style, live_signals);

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
    style: &IvStyle,
    live_signals: &LiveSignals,
) {
    let ctx = ui.ctx().clone();
    let painter = ui.painter_at(rect);
    let s = rect.width() / BOARD_W; // uniform scale (aspect preserved)
    // Design px → screen.
    let at = |x: f32, y: f32| {
        egui::pos2(rect.left() + x / BOARD_W * rect.width(), rect.top() + y / BOARD_H * rect.height())
    };

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

    // The SKIN decides the layout, full stop — an Xbox skin on a touch-bearing
    // pad gets the Xbox arrangement (the touchpad simply has no slot there;
    // pick the PlayStation skin to see it).
    let has_touch = has("touch1_x");
    let layout = match skin {
        Skin::Playstation => &PS_LAYOUT,
        Skin::SwitchPro => &SW_LAYOUT,
        _ => &X_LAYOUT,
    };

    // Board plate (style-controlled fill + outline; alpha = transparency).
    painter.rect_filled(rect, 10.0 * s, style.bg);
    if style.outline_w > 0.05 {
        painter.rect_stroke(
            rect,
            10.0 * s,
            egui::Stroke::new(style.outline_w, style.outline),
            egui::StrokeKind::Inside,
        );
    }

    let accent = style.accent;
    let tint = style.tint;

    // ── Triggers: vertical fill bars below the bumpers ──
    let lt = if has("left_trigger") { readf("left_trigger") } else if readb("btn_lt_dig") { 1.0 } else { 0.0 };
    let rt = if has("right_trigger") { readf("right_trigger") } else if readb("btn_rt_dig") { 1.0 } else { 0.0 };
    paint_trigger_bar(&painter, egui::Rect::from_min_max(at(11.8, 48.1), at(37.0, 110.5)), lt, accent, s);
    paint_trigger_bar(&painter, egui::Rect::from_min_max(at(383.0, 48.1), at(408.2, 110.5)), rt, accent, s);

    // ── Bumpers (top corners) ──
    glyph_chip(ui, &painter, skin, tint, "btn_lb", at(24.4, 24.8), 36.5 * s, glow("btn_lb"), accent);
    glyph_chip(ui, &painter, skin, tint, "btn_rb", at(395.6, 24.8), 36.5 * s, glow("btn_rb"), accent);

    // ── Touchpad strip (layout slot + device presence) ──
    if has_touch {
        if let Some((tx, ty, tw, th)) = layout.touchpad {
            let pad = egui::Rect::from_min_max(at(tx, ty), at(tx + tw, ty + th));
            paint_touchpad_strip(&painter, pad, dev, live_signals, glow("btn_touchpad"), accent, s);
        }
    }

    // ── Menu chips ──
    let (bx, by, bs) = layout.back;
    glyph_chip(ui, &painter, skin, tint, "btn_back", at(bx, by), bs * s, glow("btn_back"), accent);
    let (sx, sy, ss) = layout.start;
    glyph_chip(ui, &painter, skin, tint, "btn_start", at(sx, sy), ss * s, glow("btn_start"), accent);

    // ── Guide cluster ──
    let (gx, gy, gs) = layout.guide;
    glyph_chip(ui, &painter, skin, tint, "btn_guide", at(gx, gy), gs * s, glow("btn_guide"), accent);
    if has("btn_capture") {
        let (cx, cy, cs) = layout.capture;
        glyph_chip(ui, &painter, skin, tint, "btn_capture", at(cx, cy), cs * s, glow("btn_capture"), accent);
    }
    if let Some((mx, my, ms)) = layout.mute {
        if has("btn_mute") {
            glyph_chip(ui, &painter, skin, tint, "btn_mute", at(mx, my), ms * s, glow("btn_mute"), accent);
        }
    }

    // ── D-pad: neutral base + per-direction highlight layers (the direction
    // art ships as base-white paths + one colored path per pressed arm, so
    // compositing the colored layers covers diagonals with no extra SVGs) ──
    {
        let (dx, dy, ds) = layout.dpad;
        paint_dpad(ui, &painter, skin, tint, at(dx, dy), ds * s, accent, &glow);
    }

    // ── Face diamond (right) ──
    glyph_chip(ui, &painter, skin, tint, "btn_north", at(327.5, 41.0), 24.0 * s, glow("btn_north"), accent);
    glyph_chip(ui, &painter, skin, tint, "btn_south", at(327.5, 100.8), 24.0 * s, glow("btn_south"), accent);
    glyph_chip(ui, &painter, skin, tint, "btn_west",  at(293.9, 70.9),  24.0 * s, glow("btn_west"), accent);
    glyph_chip(ui, &painter, skin, tint, "btn_east",  at(361.1, 70.9),  24.0 * s, glow("btn_east"), accent);

    // ── Stick scopes ──
    let ls = stick_vec(live_signals, dev, "left_stick", "left_stick_x", "left_stick_y");
    let rs = stick_vec(live_signals, dev, "right_stick", "right_stick_x", "right_stick_y");
    let (lx, ly) = layout.left_stick;
    paint_stick_scope(&painter, at(lx, ly), 36.0 * s, ls, glow("btn_ls"), ui, accent);
    paint_stick_scope(&painter, at(275.1, 150.0), 36.0 * s, rs, glow("btn_rs"), ui, accent);

    // ── Rear paddles (when present): vertical pairs on the side edges ──
    for (pin, px, py, label) in [
        ("btn_paddle_l1", 34.7, 143.9, "P1"), ("btn_paddle_l2", 34.7, 182.5, "P2"),
        ("btn_paddle_r1", 385.3, 143.9, "P1"), ("btn_paddle_r2", 385.3, 182.5, "P2"),
    ] {
        if has(pin) {
            glyph_chip(ui, &painter, skin, tint, pin, at(px, py), 20.6 * s, glow(pin), accent);
            painter.text(
                at(px, py), egui::Align2::CENTER_CENTER, label,
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

/// Composite d-pad: the neutral cross as base, plus the colored layer of each
/// pressed direction's SVG faded in by its glow (diagonals light two arms).
#[allow(clippy::too_many_arguments)]
fn paint_dpad(
    ui: &egui::Ui,
    painter: &egui::Painter,
    skin: Skin,
    tint: egui::Color32,
    center: egui::Pos2,
    size: f32,
    accent: egui::Color32,
    glow: &dyn Fn(&str) -> f32,
) {
    let dirs = [
        ("dpad_up", glow("dpad_up")), ("dpad_down", glow("dpad_down")),
        ("dpad_left", glow("dpad_left")), ("dpad_right", glow("dpad_right")),
    ];
    let gmax = dirs.iter().map(|(_, g)| *g).fold(0.0f32, f32::max);
    if gmax > 0.02 {
        painter.circle_filled(center, size * 0.62, accent.gamma_multiply(0.30 * gmax));
        painter.circle_stroke(center, size * 0.62, egui::Stroke::new(1.0, accent.gamma_multiply(0.7 * gmax)));
    }
    let r = egui::Rect::from_center_size(center, egui::vec2(size, size));
    let uv = egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0));
    if let Some(tex) = glyph_texture(ui, skin, "dpad", size) {
        painter.image(tex.id(), r, uv, tint.gamma_multiply(0.55 + 0.45 * gmax));
    }
    for (pin, g) in dirs {
        if g <= 0.02 { continue; }
        if let Some(tex) = dpad_highlight_texture(ui, skin, pin, size) {
            painter.image(tex.id(), r, uv, tint.gamma_multiply(g));
        }
    }
}

/// Glyph chip: cached rasterized SVG for the pin, dimmed at rest, with an
/// accent halo scaling with glow. `tint` multiplies the art (style option);
/// the glow still ramps brightness dim→full.
#[allow(clippy::too_many_arguments)]
fn glyph_chip(
    ui: &egui::Ui,
    painter: &egui::Painter,
    skin: Skin,
    tint: egui::Color32,
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
    painter.image(
        tex.id(), r,
        egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
        tint.gamma_multiply(0.55 + 0.45 * glow),
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

/// The colored ("pressed") layer of a directional d-pad SVG: same canvas, the
/// base-white paths stripped so only the highlighted arm remains. Cached like
/// [`glyph_texture`].
fn dpad_highlight_texture(ui: &egui::Ui, skin: Skin, pin: &str, logical_size: f32) -> Option<egui::TextureHandle> {
    let px = (logical_size * ui.ctx().pixels_per_point()).round().max(4.0) as u32;
    let key = egui::Id::new(("iv_dpad_hl", skin.as_str(), pin, px));
    if let Some(tex) = ui.ctx().data(|d| d.get_temp::<egui::TextureHandle>(key)) {
        return Some(tex);
    }
    let bytes = remapper_icons::pin_svg(skin, pin)?;
    let text = std::str::from_utf8(bytes).ok()?;
    let highlight_only = strip_white_paths(text);
    let img = rasterize_svg_recolored(&highlight_only, px, px, "override", egui::Color32::TRANSPARENT)?;
    let tex = ui.ctx().load_texture(format!("iv_dpad_hl_{pin}"), img, egui::TextureOptions::LINEAR);
    ui.ctx().data_mut(|d| d.insert_temp(key, tex.clone()));
    Some(tex)
}

/// Remove `<path … fill="#FFFFFF" …/>` elements from an SVG, leaving only the
/// colored paths (the icon set draws the pressed arm as its own colored path
/// on top of white base paths).
fn strip_white_paths(svg: &str) -> String {
    let mut out = String::with_capacity(svg.len());
    let mut rest = svg;
    while let Some(i) = rest.find("<path") {
        let (head, tail) = rest.split_at(i);
        out.push_str(head);
        let end = tail.find("/>").map(|e| e + 2).unwrap_or(tail.len());
        let (elem, after) = tail.split_at(end);
        let lower = elem.to_ascii_lowercase();
        let white = lower.contains("fill=\"#ffffff\"")
            || lower.contains("fill=\"#fff\"")
            || lower.contains("fill=\"white\"");
        if !white {
            out.push_str(elem);
        }
        rest = after;
    }
    out.push_str(rest);
    out
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
