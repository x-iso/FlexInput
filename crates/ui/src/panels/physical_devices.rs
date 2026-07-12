use eframe::egui::{self, Color32, RichText};
use flexinput_devices::{ControllerKind, PhysicalDevice};

use crate::canvas::{Canvas, DeviceParamDefaults};
use crate::canvas::remapper_icons;
use crate::panels::device_icon::render_device_icon;

const CARD_MIN_W: f32 = 240.0;
const ICON_H: f32 = 32.0;
const PILL_W: f32 = 110.0;
const CARD_H: f32 = 36.0;

pub fn show(
    ui: &mut egui::Ui,
    devices: &[PhysicalDevice],
    canvas: &mut Canvas,
    default_collapsed: bool,
    defaults: DeviceParamDefaults,
) {
    if devices.is_empty() {
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            ui.add_space(12.0);
            ui.label(RichText::new("No devices detected").weak());
        });
        return;
    }

    let mut name_counts: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
    for d in devices {
        *name_counts.entry(d.display_name.as_str()).or_insert(0) += 1;
    }
    let mut name_seen: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();

    let out = egui::ScrollArea::horizontal()
        .id_salt("physical_scroll")
        .auto_shrink([false, false])
        .show(ui, |ui| {
            ui.horizontal_top(|ui| {
                for device in devices {
                    let total = *name_counts.get(device.display_name.as_str()).unwrap_or(&1);
                    let idx   = *name_seen.get(device.display_name.as_str()).unwrap_or(&0);
                    *name_seen.entry(device.display_name.as_str()).or_insert(0) += 1;
                    let label = if total > 1 {
                        std::borrow::Cow::Owned(format!("{} #{}", device.display_name, idx + 1))
                    } else {
                        std::borrow::Cow::Borrowed(device.display_name.as_str())
                    };
                    device_card(ui, device, &label, canvas, default_collapsed, defaults);
                }
                // Trailing breathing room so the last card never sits buried
                // under the right-edge fade.
                ui.add_space(40.0);
            });
        });
    paint_scroll_edge_fades(ui.ctx(), &out, ui.clip_rect());
}

/// Floating label tab that hangs off a panel edge onto the canvas. Drawn via
/// a dedicated `egui::Area` so it overlays anything underneath.
///
/// `anchor` is the screen-space point on the panel's canvas-facing edge where
/// the tab anchors (the panel-edge corner of the tab). `direction` indicates
/// which way the tab extends from that anchor:
///   - `Down` → tab hangs below the anchor (use for top panel).
///   - `Up`   → tab sits above the anchor (use for bottom panel).
/// Returns the response of the tab's clickable surface so callers can toggle
/// state on click.
pub fn draw_floating_heading(
    ctx: &egui::Context,
    id_source: &str,
    text: &str,
    anchor: egui::Pos2,
    direction: TabDirection,
) -> egui::Response {
    const TAB_H: f32 = 28.0;
    const TAB_PAD_X: f32 = 14.0;

    let tl = match direction {
        TabDirection::Down => anchor,
        TabDirection::Up   => egui::pos2(anchor.x, anchor.y - TAB_H),
    };

    // Probe hover state from the previous frame so we can pick the fill color
    // before painting (egui::Frame doesn't expose a post-paint fill swap).
    let interact_id = egui::Id::new((id_source, "tab_interact"));
    let was_hovered = ctx.memory(|m| m.data.get_temp::<bool>(interact_id).unwrap_or(false));

    let area = egui::Area::new(egui::Id::new(id_source))
        .order(egui::Order::Middle)
        .fixed_pos(tl)
        .interactable(true)
        .show(ctx, |ui| {
            let panel_bg = ui.visuals().panel_fill;
            let base = darken_color(panel_bg, 0.35);
            let fill = if was_hovered { lighten_color(base, 0.20) } else { base };

            let cr = match direction {
                TabDirection::Down => egui::CornerRadius { nw: 0, ne: 0, sw: 6, se: 6 },
                TabDirection::Up   => egui::CornerRadius { nw: 6, ne: 6, sw: 0, se: 0 },
            };

            let frame_resp = egui::Frame::default()
                .inner_margin(egui::Margin { left: TAB_PAD_X as i8, right: TAB_PAD_X as i8, top: 4, bottom: 4 })
                .corner_radius(cr)
                .fill(fill)
                .show(ui, |ui| {
                    ui.label(RichText::new(text).strong().size(14.0));
                });

            // Paint a gray outline on the three canvas-facing sides only —
            // the side touching the panel edge stays open. Use a contiguous
            // open path with arc-traced rounded corners so there are no
            // visible gaps at the corner radius.
            let rect = frame_resp.response.rect;
            let stroke_color = ui.visuals().widgets.noninteractive.bg_stroke.color;
            let stroke = egui::Stroke::new(1.0, stroke_color);
            let painter = ui.painter();
            const R: f32 = 6.0;
            const ARC_SEGS: usize = 6;
            let arc = |center: egui::Pos2, a0: f32, a1: f32| -> Vec<egui::Pos2> {
                (0..=ARC_SEGS).map(|i| {
                    let t = i as f32 / ARC_SEGS as f32;
                    let a = a0 + (a1 - a0) * t;
                    egui::pos2(center.x + R * a.cos(), center.y + R * a.sin())
                }).collect()
            };
            let mut path: Vec<egui::Pos2> = Vec::new();
            match direction {
                TabDirection::Down => {
                    // Start at top-left, go down the left side, arc the bottom
                    // corners, finish at top-right.
                    path.push(egui::pos2(rect.left(), rect.top()));
                    path.push(egui::pos2(rect.left(), rect.bottom() - R));
                    path.extend(arc(
                        egui::pos2(rect.left() + R, rect.bottom() - R),
                        std::f32::consts::PI,
                        std::f32::consts::FRAC_PI_2,
                    ));
                    path.push(egui::pos2(rect.right() - R, rect.bottom()));
                    path.extend(arc(
                        egui::pos2(rect.right() - R, rect.bottom() - R),
                        std::f32::consts::FRAC_PI_2,
                        0.0,
                    ));
                    path.push(egui::pos2(rect.right(), rect.top()));
                }
                TabDirection::Up => {
                    // Start at bottom-left, go up the left side, arc the top
                    // corners (sweeping the short way through the corner),
                    // finish at bottom-right. With Y-down: angle π = left,
                    // 3π/2 = up. Sweep π → 3π/2 for top-left, 3π/2 → 2π for
                    // top-right.
                    use std::f32::consts::{PI, FRAC_PI_2, TAU};
                    path.push(egui::pos2(rect.left(), rect.bottom()));
                    path.push(egui::pos2(rect.left(), rect.top() + R));
                    path.extend(arc(
                        egui::pos2(rect.left() + R, rect.top() + R),
                        PI,
                        PI + FRAC_PI_2,
                    ));
                    path.push(egui::pos2(rect.right() - R, rect.top()));
                    path.extend(arc(
                        egui::pos2(rect.right() - R, rect.top() + R),
                        PI + FRAC_PI_2,
                        TAU,
                    ));
                    path.push(egui::pos2(rect.right(), rect.bottom()));
                }
            }
            painter.add(egui::Shape::line(path, stroke));

            ui.interact(rect, ui.id().with("tab_click"), egui::Sense::click())
        });

    let resp = area.inner.on_hover_cursor(egui::CursorIcon::PointingHand);
    // Stash hover state for next frame's color choice.
    ctx.memory_mut(|m| m.data.insert_temp(interact_id, resp.hovered()));
    resp
}

/// Easy-mode "Devices" tab — a small horizontal label that hangs off the RIGHT
/// edge of the left panel (its LEFT edge anchored at `left_edge_x`, top at
/// `top_y`), extending rightward onto the preset bar. The attached (left) side
/// has sharp corners; the free (right) side is rounded. The caller anchors it to
/// the (animating) left-panel edge, so it rides the panel as it collapses and
/// stays pinned at the window's top-left as the re-open button. Returns the
/// clickable tab response and its rect (the latter feeds the preset-label clamp).
pub fn draw_devices_tab(
    ctx: &egui::Context,
    id_source: &str,
    text: &str,
    left_edge_x: f32,
    top_y: f32,
) -> (egui::Response, egui::Rect) {
    // Fixed height matches the preset-dropdown pill (30px) for a clean visual
    // pairing; text is centred vertically inside it.
    const TAB_H: f32 = 30.0;
    const TAB_PAD_X: f32 = 12.0;
    const R: f32 = 6.0;

    let interact_id = egui::Id::new((id_source, "tab_interact"));
    let was_hovered = ctx.memory(|m| m.data.get_temp::<bool>(interact_id).unwrap_or(false));

    let area = egui::Area::new(egui::Id::new(id_source))
        .order(egui::Order::Middle)
        .fixed_pos(egui::pos2(left_edge_x, top_y))
        .interactable(true)
        .show(ctx, |ui| {
            let font = egui::FontId::proportional(14.0);
            let text_w = ui.painter()
                .layout_no_wrap(text.to_string(), font.clone(), Color32::WHITE)
                .size().x;
            let tab_w = text_w + 2.0 * TAB_PAD_X;
            let (rect, resp) =
                ui.allocate_exact_size(egui::vec2(tab_w, TAB_H), egui::Sense::click());

            let panel_bg = ui.visuals().panel_fill;
            let base = darken_color(panel_bg, 0.35);
            let fill = if was_hovered || resp.hovered() {
                lighten_color(base, 0.20)
            } else {
                base
            };
            // Rounded on the free (right) side, sharp where it meets the panel.
            let cr = egui::CornerRadius { nw: 0, ne: R as u8, sw: 0, se: R as u8 };
            let painter = ui.painter();
            painter.rect_filled(rect, cr, fill);

            // Outline the three free sides (top, right, bottom); the left edge
            // stays open where it attaches to the panel border.
            let stroke_color = ui.visuals().widgets.noninteractive.bg_stroke.color;
            let stroke = egui::Stroke::new(1.0, stroke_color);
            const ARC_SEGS: usize = 6;
            use std::f32::consts::FRAC_PI_2;
            let arc = |center: egui::Pos2, a0: f32, a1: f32| -> Vec<egui::Pos2> {
                (0..=ARC_SEGS).map(|i| {
                    let t = i as f32 / ARC_SEGS as f32;
                    let a = a0 + (a1 - a0) * t;
                    egui::pos2(center.x + R * a.cos(), center.y + R * a.sin())
                }).collect()
            };
            let mut path: Vec<egui::Pos2> = Vec::new();
            // top-left (open) → along top → top-right arc → down right → bottom-
            // right arc → along bottom → bottom-left (open).
            path.push(egui::pos2(rect.left(), rect.top()));
            path.push(egui::pos2(rect.right() - R, rect.top()));
            path.extend(arc(egui::pos2(rect.right() - R, rect.top() + R), -FRAC_PI_2, 0.0));
            path.push(egui::pos2(rect.right(), rect.bottom() - R));
            path.extend(arc(egui::pos2(rect.right() - R, rect.bottom() - R), 0.0, FRAC_PI_2));
            path.push(egui::pos2(rect.left(), rect.bottom()));
            painter.add(egui::Shape::line(path, stroke));

            painter.text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                text,
                font,
                ui.visuals().strong_text_color(),
            );

            (resp, rect)
        });

    let (resp, rect) = area.inner;
    let resp = resp.on_hover_cursor(egui::CursorIcon::PointingHand);
    ctx.memory_mut(|m| m.data.insert_temp(interact_id, resp.hovered()));
    (resp, rect)
}

pub fn lighten_color(c: Color32, amount: f32) -> Color32 {
    let f = amount.clamp(0.0, 1.0);
    let lift = |v: u8| -> u8 {
        let lifted = v as f32 + (255.0 - v as f32) * f;
        lifted.clamp(0.0, 255.0) as u8
    };
    Color32::from_rgba_unmultiplied(lift(c.r()), lift(c.g()), lift(c.b()), c.a())
}

#[derive(Copy, Clone)]
pub enum TabDirection { Down, Up }

pub fn darken_color(c: Color32, amount: f32) -> Color32 {
    let f = (1.0 - amount).clamp(0.0, 1.0);
    Color32::from_rgba_unmultiplied(
        ((c.r() as f32) * f) as u8,
        ((c.g() as f32) * f) as u8,
        ((c.b() as f32) * f) as u8,
        c.a(),
    )
}

/// Paint horizontal alpha-gradient fades on the left/right edges of a
/// ScrollArea, conditional on whether there is hidden content in that
/// direction. Painted on the parent panel (over the panel's own background),
/// so the gradient extends fully to the panel's outer edge — overflowing
/// content disappears smoothly into the panel fill instead of clipping
/// abruptly against the ScrollArea's inner rect.
pub fn paint_scroll_edge_fades<R>(
    ctx: &egui::Context,
    out: &egui::scroll_area::ScrollAreaOutput<R>,
    panel_outer: egui::Rect,
) {
    const FADE_W: f32 = 40.0;
    let inner = out.inner_rect;
    let offset_x = out.state.offset.x;
    let max_scroll = (out.content_size.x - inner.width()).max(0.0);
    let can_scroll_left  = offset_x > 0.5;
    let can_scroll_right = offset_x < max_scroll - 0.5;

    // Render the fade on Background so floating windows (which default to
    // Order::Middle) sit on top of it instead of being overlaid by it.
    let layer = egui::LayerId::new(
        egui::Order::Background,
        egui::Id::new(("edge_fade", out.id)),
    );
    let mut painter = ctx.layer_painter(layer);
    // Clip to the panel's outer rect — the gradient can extend all the way
    // to the panel edge without bleeding onto the canvas.
    painter.set_clip_rect(panel_outer);

    let panel_bg = ctx.style().visuals.panel_fill;
    let solid    = darken_color(panel_bg, 0.35);
    // For the transparent end of the gradient, we MUST use a fully-zeroed
    // color — egui mesh vertex colors are premultiplied, so interpolating
    // from (R,G,B,255) → (R,G,B,0) yields midpoint (R/2,G/2,B/2,128) which
    // overlays as a noticeable lighter band. (0,0,0,0) interpolates cleanly.
    let transparent = Color32::TRANSPARENT;

    let top = panel_outer.top();
    let bot = panel_outer.bottom();

    if can_scroll_right {
        // Right side: gradient starts FADE_W before the panel edge, ends fully
        // opaque at the panel edge so any overflowing card is fully covered.
        let x_fade_start = panel_outer.right() - FADE_W;
        let x_fade_end   = panel_outer.right();
        paint_gradient_quad(
            &painter,
            egui::pos2(x_fade_start, top),
            egui::pos2(x_fade_end,   bot),
            transparent, solid, solid, transparent,
        );
    }
    if can_scroll_left {
        let x_fade_start = panel_outer.left();
        let x_fade_end   = panel_outer.left() + FADE_W;
        paint_gradient_quad(
            &painter,
            egui::pos2(x_fade_start, top),
            egui::pos2(x_fade_end,   bot),
            solid, transparent, transparent, solid,
        );
    }
}

fn paint_gradient_quad(
    painter: &egui::Painter,
    tl: egui::Pos2,
    br: egui::Pos2,
    c_tl: Color32, c_tr: Color32, c_br: Color32, c_bl: Color32,
) {
    use egui::epaint::{Mesh, Vertex};
    let mut mesh = Mesh::default();
    let uv = egui::epaint::WHITE_UV;
    let tr = egui::pos2(br.x, tl.y);
    let bl = egui::pos2(tl.x, br.y);
    let i = mesh.vertices.len() as u32;
    mesh.vertices.push(Vertex { pos: tl, uv, color: c_tl });
    mesh.vertices.push(Vertex { pos: tr, uv, color: c_tr });
    mesh.vertices.push(Vertex { pos: br, uv, color: c_br });
    mesh.vertices.push(Vertex { pos: bl, uv, color: c_bl });
    mesh.indices.extend_from_slice(&[i, i+1, i+2, i, i+2, i+3]);
    painter.add(egui::Shape::mesh(mesh));
}

fn device_card(
    ui: &mut egui::Ui,
    device: &PhysicalDevice,
    label: &str,
    canvas: &mut Canvas,
    default_collapsed: bool,
    defaults: DeviceParamDefaults,
) {
    // MIDI OUT is a pure sink; all other gamepad devices are sources.
    let is_sink = device.kind == ControllerKind::MidiOut;
    let canvas_module = if is_sink { "device.sink" } else { "device.source" };
    let already_on_canvas = canvas.snarl.nodes_ids_data().any(|(_, n)| {
        n.value.module_id == canvas_module
            && n.value.params.get("device_id").and_then(|v| v.as_str()) == Some(&device.id)
    });

    // Measure the label so we can pick a card width that fits everything
    // (icon + label + pill) without truncation or overlap.
    let label_w = {
        let font_id = egui::TextStyle::Body.resolve(ui.style());
        ui.painter().layout_no_wrap(label.to_string(), font_id, Color32::WHITE).size().x
    };
    let item_spacing = ui.spacing().item_spacing.x;
    let h_padding = 20.0; // frame inner_margin left + right
    let content_w = ICON_H + item_spacing + label_w + item_spacing + PILL_W;
    let card_w = (content_w + h_padding).max(CARD_MIN_W);

    egui::Frame::default()
        .inner_margin(egui::Margin { left: 10, right: 10, top: 0, bottom: 0 })
        .corner_radius(12.0)
        .stroke(egui::Stroke::new(1.0, ui.visuals().widgets.noninteractive.bg_stroke.color))
        .show(ui, |ui| {
            ui.set_width(card_w);
            ui.set_height(CARD_H);

            ui.with_layout(
                egui::Layout::left_to_right(egui::Align::Center),
                |ui| {
                    render_device_icon(
                        ui,
                        remapper_icons::device_card_svg(device.kind),
                        ICON_H,
                    );
                    ui.label(RichText::new(label).strong());
                    ui.with_layout(
                        egui::Layout::right_to_left(egui::Align::Center),
                        |ui| {
                            canvas_status_button(ui, already_on_canvas, || {
                                if is_sink {
                                    canvas.add_physical_sink(device, default_collapsed, defaults);
                                } else {
                                    canvas.add_device_source(device, default_collapsed, defaults);
                                }
                            });
                        },
                    );
                },
            );
        });
}

/// Fixed-size rounded canvas-status control. Both states share identical
/// geometry so cards line up visually no matter their neighbours. Pass `None`
/// for `width` to size by content; pass `Some(w)` to lock a uniform width.
pub(crate) fn canvas_status_button(
    ui: &mut egui::Ui,
    on_canvas: bool,
    mut on_click: impl FnMut(),
) {
    canvas_status_button_sized(ui, on_canvas, 110.0, &mut on_click);
}

pub(crate) fn canvas_status_button_sized(
    ui: &mut egui::Ui,
    on_canvas: bool,
    width: f32,
    on_click: &mut dyn FnMut(),
) {
    let size = egui::vec2(width, 26.0);

    if on_canvas {
        let stroke = egui::Stroke::new(
            1.0,
            ui.visuals().widgets.noninteractive.bg_stroke.color,
        );
        let btn = egui::Button::new(RichText::new("On canvas").weak())
            .corner_radius(13.0)
            .min_size(size)
            .fill(Color32::TRANSPARENT)
            .stroke(stroke)
            .sense(egui::Sense::hover());
        ui.add(btn);
    } else {
        let btn = egui::Button::new(RichText::new("Add to canvas").strong())
            .corner_radius(13.0)
            .min_size(size)
            .fill(ui.visuals().selection.bg_fill);
        if ui.add(btn).clicked() {
            on_click();
        }
    }
}
