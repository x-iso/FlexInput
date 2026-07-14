//! Virtual Menu overlay — a second transparent always-on-top viewport that
//! exists only while a menu is OPEN (eval-side truth, read from the node's
//! `Open` output mirror) or being POSITIONED, fully independent of the info
//! overlay's visibility in both directions.
//!
//! * **Live** — click-through; every open menu on the active tab (top level +
//!   first-level sub-patches) draws at its `menu_rect` (monitor fractions)
//!   with the hovered zone highlighted from the `Hover` output mirror.
//! * **Edit** — entered from the node body's "Position on screen…" button
//!   (`request_menu_edit`); passthrough off, the target menu gets a dim
//!   backdrop, drag-to-move + corner drag-to-resize, and a Done chip.
//!   Esc or Done exits; `menu_rect` writes back through the node path.
//!
//! Same transparency machinery as the info overlay (unique title +
//! transparent + skip-taskbar triggers the vendored NOREDIRECTIONBITMAP
//! patch; passthrough commands latched per state change).

use std::time::Duration;

use egui_snarl::{NodeId, Snarl};
use flexinput_core::touchzones as tz;

use crate::app::FlexInputApp;
use crate::canvas::NodeData;

const MENU_OVERLAY_TITLE: &str = "FlexInput Menu Overlay";
/// Default `menu_rect` (monitor fractions) for a menu that was never placed.
const DEFAULT_RECT: [f32; 4] = [0.375, 0.35, 0.25, 0.30];

/// Ctx temp slot holding the edit target as `(outer subpatch uid, inner uid)`
/// (`None` outer = the node sits on the tab canvas).
fn edit_id() -> egui::Id { egui::Id::new("fxi_menu_edit_target") }

/// Enter position-edit mode for one menu node (from its body button).
pub fn request_menu_edit(ctx: &egui::Context, outer_uid: Option<usize>, inner_uid: usize) {
    ctx.data_mut(|d| d.insert_temp(edit_id(), (outer_uid, inner_uid)));
}

/// The menu currently being positioned, if any.
pub fn menu_edit_target(ctx: &egui::Context) -> Option<(Option<usize>, usize)> {
    ctx.data(|d| d.get_temp::<(Option<usize>, usize)>(edit_id()))
}

fn clear_menu_edit(ctx: &egui::Context) {
    ctx.data_mut(|d| d.remove_temp::<(Option<usize>, usize)>(edit_id()));
}

/// One menu instance found on the active tab.
struct MenuInst {
    outer: Option<NodeId>,
    inner: NodeId,
    open: bool,
    hover: i32,
    name: String,
    icon: String,
    icon_svg: String,
    rect: [f32; 4],
    tree: tz::ZoneNode,
    /// The node's mapping cards (`zone_maps`), for per-zone destination icons.
    zone_maps: Vec<serde_json::Value>,
}

fn read_menu(node: &NodeData, outer: Option<NodeId>, inner: NodeId) -> MenuInst {
    let pstr = |k: &str| node.params.get(k).and_then(|v| v.as_str()).unwrap_or("").to_string();
    let open = node.extra.last_out.get(1).and_then(|s| *s).map(|s| s.as_bool()).unwrap_or(false);
    let hover = node.extra.last_out.get(2).and_then(|s| *s).map(|s| s.as_float()).unwrap_or(-1.0) as i32;
    let rect = node.params.get("menu_rect").and_then(|v| v.as_array())
        .and_then(|a| {
            let f = |i: usize| a.get(i)?.as_f64().map(|x| x as f32);
            Some([f(0)?, f(1)?, f(2)?, f(3)?])
        })
        .unwrap_or(DEFAULT_RECT);
    let read_edges = |which: &str| -> Vec<f32> {
        node.params.get(which).and_then(|v| v.as_array())
            .map(|a| a.iter().filter_map(|x| x.as_f64().map(|f| f as f32)).collect())
            .unwrap_or_default()
    };
    let tree = node.params.get("zone_tree").and_then(tz::ZoneNode::from_value)
        .unwrap_or_else(|| tz::ZoneNode::from_grid(&read_edges("col_edges"), &read_edges("row_edges")));
    MenuInst {
        outer,
        inner,
        open,
        hover,
        name: pstr("menu_name"),
        icon: pstr("menu_icon"),
        icon_svg: pstr("menu_icon_svg"),
        rect,
        tree,
        zone_maps: node.params.get("zone_maps").and_then(|v| v.as_array()).cloned().unwrap_or_default(),
    }
}

/// Collect every menu node on the tab canvas + first-level sub-patches
/// (matching the overlay pick flow's reach).
fn collect_menus(tab_snarl: &Snarl<NodeData>) -> Vec<MenuInst> {
    let mut out = Vec::new();
    for (id, node_ref) in tab_snarl.nodes_ids_data() {
        let n = &node_ref.value;
        if n.module_id == "module.menu" {
            out.push(read_menu(n, None, id));
        }
        if let Some(sp) = n.subpatch.as_deref() {
            for (iid, inner_ref) in sp.snarl.nodes_ids_data() {
                if inner_ref.value.module_id == "module.menu" {
                    out.push(read_menu(&inner_ref.value, Some(id), iid));
                }
            }
        }
    }
    out
}

/// Write a menu's `menu_rect` back through its node path.
fn write_menu_rect(tab_snarl: &mut Snarl<NodeData>, outer: Option<NodeId>, inner: NodeId, rect: [f32; 4]) {
    let val = serde_json::json!([rect[0], rect[1], rect[2], rect[3]]);
    match outer {
        None => {
            if let Some(node) = tab_snarl.get_node_mut(inner) {
                node.params.insert("menu_rect".to_string(), val);
            }
        }
        Some(sp_id) => {
            if let Some(sp) = tab_snarl.get_node_mut(sp_id).and_then(|n| n.subpatch.as_deref_mut()) {
                if let Some(node) = sp.snarl.get_node_mut(inner) {
                    node.params.insert("menu_rect".to_string(), val);
                }
            }
        }
    }
}

/// Show the menu overlay viewport (call once per frame from
/// `FlexInputApp::update`, right after the info overlay). No-op while no menu
/// is open or being positioned.
pub fn show_menu_overlay(app: &mut FlexInputApp, ctx: &egui::Context) {
    let frame_interval = Duration::from_secs_f64(1.0 / app.overlay_fps() as f64);
    let (tab, _live_signals, _panic) = app.overlay_parts();
    let tab_snarl = &mut tab.canvas.snarl;

    let menus = collect_menus(tab_snarl);
    // Validate the edit target against the current node set (node deleted /
    // tab switched while editing → drop the edit).
    let mut edit = menu_edit_target(ctx);
    if let Some((outer, inner)) = edit {
        let found = menus.iter().any(|m| m.outer.map(|n| n.0) == outer && m.inner.0 == inner);
        if !found {
            clear_menu_edit(ctx);
            edit = None;
        }
    }

    let any_open = menus.iter().any(|m| m.open);
    if !any_open && edit.is_none() {
        return; // viewport not declared → eframe destroys the OS window
    }

    let monitor_size = ctx
        .input(|i| i.viewport().monitor_size)
        .filter(|s| s.x > 1.0 && s.y > 1.0)
        .unwrap_or(egui::vec2(1920.0, 1080.0));

    let viewport_id = egui::ViewportId::from_hash_of("fxi_menu_overlay");
    let builder = egui::ViewportBuilder::default()
        .with_title(MENU_OVERLAY_TITLE)
        .with_decorations(false)
        .with_transparent(true)
        .with_always_on_top()
        .with_taskbar(false)
        .with_mouse_passthrough(true)
        .with_active(false)
        .with_resizable(false)
        .with_has_shadow(false)
        .with_position(egui::pos2(0.0, 0.0))
        .with_inner_size(monitor_size);

    let mut exit_edit = false;
    let mut rect_write: Option<(Option<NodeId>, NodeId, [f32; 4])> = None;

    ctx.show_viewport_immediate(viewport_id, builder, |vctx, _class| {
        // Geometry self-correction from inside the viewport (see overlay.rs).
        let (inner_rect, child_monitor) = vctx.input(|i| {
            (i.viewport().inner_rect, i.viewport().monitor_size)
        });
        if let (Some(inner_rect), Some(want)) = (inner_rect, child_monitor) {
            if (inner_rect.size() - want).length() > 1.0 {
                vctx.send_viewport_cmd(egui::ViewportCommand::OuterPosition(egui::pos2(0.0, 0.0)));
                vctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(want));
            }
        }

        // Click-through while live; interactive while positioning.
        let pt_id = egui::Id::new("fxi_menu_passthrough_applied");
        let want_passthrough = edit.is_none();
        let applied: Option<bool> = vctx.data(|d| d.get_temp(pt_id));
        if applied != Some(want_passthrough) {
            vctx.send_viewport_cmd(egui::ViewportCommand::MousePassthrough(want_passthrough));
            if !want_passthrough {
                vctx.send_viewport_cmd(egui::ViewportCommand::Focus);
            }
            vctx.data_mut(|d| d.insert_temp(pt_id, want_passthrough));
        }

        if edit.is_some() && vctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            exit_edit = true;
        }

        egui::CentralPanel::default()
            .frame(egui::Frame::NONE)
            .show(vctx, |ui| {
                let screen = ui.max_rect();
                let to_px = |r: [f32; 4]| -> egui::Rect {
                    egui::Rect::from_min_size(
                        egui::pos2(screen.left() + r[0] * screen.width(),
                                   screen.top() + r[1] * screen.height()),
                        egui::vec2(r[2] * screen.width(), r[3] * screen.height()),
                    )
                };

                if let Some((outer_uid, inner_uid)) = edit {
                    // Dim backdrop so edit reads as a distinct state.
                    ui.painter().rect_filled(screen, 0.0, egui::Color32::from_black_alpha(96));
                    if let Some(m) = menus.iter().find(|m|
                        m.outer.map(|n| n.0) == outer_uid && m.inner.0 == inner_uid)
                    {
                        let mut r = m.rect;
                        let px = to_px(r);
                        paint_menu(ui, px, m, true);

                        // Drag the body to move.
                        let body = ui.interact(px, egui::Id::new(("fxi_menu_move", inner_uid)), egui::Sense::drag());
                        if body.dragged() {
                            let d = body.drag_delta();
                            r[0] += d.x / screen.width();
                            r[1] += d.y / screen.height();
                        }
                        // Drag the bottom-right corner to resize.
                        let handle = egui::Rect::from_center_size(px.right_bottom(), egui::vec2(22.0, 22.0));
                        let hresp = ui.interact(handle, egui::Id::new(("fxi_menu_size", inner_uid)), egui::Sense::drag());
                        if hresp.dragged() {
                            let d = hresp.drag_delta();
                            r[2] += d.x / screen.width();
                            r[3] += d.y / screen.height();
                        }
                        ui.painter().rect_filled(handle.shrink(4.0), 3.0, egui::Color32::from_rgb(255, 196, 90));
                        if hresp.hovered() || body.hovered() {
                            ui.output_mut(|o| o.cursor_icon = if hresp.hovered() {
                                egui::CursorIcon::ResizeNwSe
                            } else {
                                egui::CursorIcon::Move
                            });
                        }

                        // Clamp: keep a sane minimum and stay on screen.
                        r[2] = r[2].clamp(0.06, 1.0);
                        r[3] = r[3].clamp(0.06, 1.0);
                        r[0] = r[0].clamp(0.0, 1.0 - r[2]);
                        r[1] = r[1].clamp(0.0, 1.0 - r[3]);
                        if r != m.rect {
                            rect_write = Some((m.outer, m.inner, r));
                        }

                        menu_edit_chrome(ui, screen, &mut exit_edit);
                    }
                } else {
                    for m in menus.iter().filter(|m| m.open) {
                        paint_menu(ui, to_px(m.rect), m, false);
                    }
                }
            });
    });

    if let Some((outer, inner, r)) = rect_write {
        write_menu_rect(tab_snarl, outer, inner, r);
    }
    if exit_edit {
        clear_menu_edit(ctx);
    }

    // Pace the parent context (immediate viewports render with the parent).
    ctx.request_repaint_after(frame_interval);
}

/// Paint one menu at `px`: plate, zone cells, hovered-zone highlight, and the
/// name/icon chip above. In edit mode nothing is highlighted.
fn paint_menu(ui: &mut egui::Ui, px: egui::Rect, m: &MenuInst, editing: bool) {
    let p = ui.painter();
    let accent = egui::Color32::from_rgb(255, 196, 90);
    p.rect_filled(px, 8.0, egui::Color32::from_rgba_unmultiplied(16, 16, 20, 210));
    p.rect_stroke(px, 8.0, egui::Stroke::new(1.5, egui::Color32::from_gray(110)), egui::StrokeKind::Inside);

    for (zid, [x0, y0, x1, y1]) in m.tree.zones() {
        let zr = egui::Rect::from_min_max(
            egui::pos2(px.left() + x0 * px.width(), px.top() + y0 * px.height()),
            egui::pos2(px.left() + x1 * px.width(), px.top() + y1 * px.height()),
        ).shrink(2.0);
        let hovered = !editing && m.hover == zid as i32;
        if hovered {
            p.rect_filled(zr, 5.0, accent.gamma_multiply(0.30));
            p.rect_stroke(zr, 5.0, egui::Stroke::new(2.0, accent), egui::StrokeKind::Inside);
        } else {
            p.rect_stroke(zr, 5.0, egui::Stroke::new(1.0, egui::Color32::from_gray(80)), egui::StrokeKind::Inside);
        }

        // Destination icons: every out pin across the zone's mapping cards,
        // deduped in card order. KBM as the base skin — destinations are
        // typically keys/mouse/macro targets; a virtual-pad pin still renders
        // via the chip painter's any-skin fallback (dimmed).
        let mut pins: Vec<String> = Vec::new();
        for c in m.zone_maps.iter().filter(|c|
            c.get("z").and_then(|v| v.as_u64()).unwrap_or(0) == zid as u64)
        {
            for pin in c.get("out").and_then(|v| v.as_array()).into_iter().flatten()
                .filter_map(|v| v.as_str())
            {
                if !pins.iter().any(|x| x == pin) { pins.push(pin.to_string()); }
            }
        }
        if pins.is_empty() {
            // Unmapped zone: faint index so it's still identifiable as a target.
            p.text(
                zr.center(), egui::Align2::CENTER_CENTER, format!("{zid}"),
                egui::FontId::proportional((zr.height() * 0.28).clamp(11.0, 26.0)),
                if hovered { egui::Color32::WHITE } else { egui::Color32::from_gray(150) },
            );
        } else {
            let ic = (zr.height() * 0.42).clamp(14.0, 30.0);
            let gap = 4.0;
            // Show as many icons as fit; a trailing "…" marks the overflow.
            let fit = (((zr.width() - 8.0 + gap) / (ic + gap)).floor() as usize)
                .clamp(1, pins.len());
            let truncated = fit < pins.len();
            let total_w = fit as f32 * ic + (fit.saturating_sub(1)) as f32 * gap;
            let mut x = zr.center().x - total_w * 0.5;
            let y = zr.center().y - ic * 0.5;
            for pin in pins.iter().take(fit) {
                let w = crate::canvas::viewer::paint_chord_chip_to_rect(
                    p, ui.ctx(), egui::pos2(x, y), ic, pin,
                    crate::canvas::remapper_icons::Skin::Kbm,
                );
                x += w + gap;
            }
            if truncated {
                p.text(egui::pos2(x, zr.center().y), egui::Align2::LEFT_CENTER, "…",
                    egui::FontId::proportional(ic * 0.6), egui::Color32::from_gray(170));
            }
        }
    }

    // Name + icon chip above the plate.
    let label = if m.name.is_empty() { "Menu".to_string() } else { m.name.clone() };
    let font = egui::FontId::proportional(13.0);
    let galley = p.layout_no_wrap(label, font, egui::Color32::from_gray(230));
    let icon_tex = crate::macro_icons::macro_port_icon_texture(ui.ctx(), &m.icon, &m.icon_svg, 16.0);
    let icon_w = if icon_tex.is_some() { 20.0 } else { 0.0 };
    let pad = egui::vec2(10.0, 5.0);
    let chip = egui::Rect::from_min_size(
        egui::pos2(px.left(), px.top() - galley.size().y - pad.y * 2.0 - 6.0),
        egui::vec2(galley.size().x + icon_w + pad.x * 2.0, galley.size().y + pad.y * 2.0),
    );
    p.rect_filled(chip, 6.0, egui::Color32::from_rgba_unmultiplied(16, 16, 20, 210));
    if let Some(tex) = icon_tex {
        let ir = egui::Rect::from_center_size(
            egui::pos2(chip.left() + pad.x + 8.0, chip.center().y), egui::vec2(16.0, 16.0));
        p.image(tex.id(), ir,
            egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
            egui::Color32::WHITE);
    }
    p.galley(egui::pos2(chip.left() + pad.x + icon_w, chip.top() + pad.y), galley,
        egui::Color32::from_gray(230));
}

/// Edit-mode chrome: a Done chip + hint, top-center.
fn menu_edit_chrome(ui: &mut egui::Ui, screen: egui::Rect, exit_edit: &mut bool) {
    let area = egui::Area::new(egui::Id::new("fxi_menu_edit_toolbar"))
        .order(egui::Order::Foreground)
        .anchor(egui::Align2::CENTER_TOP, egui::vec2(0.0, 12.0))
        .interactable(true);
    area.show(ui.ctx(), |ui| {
        let bg = ui.visuals().window_fill();
        egui::Frame::default()
            .fill(egui::Color32::from_rgba_unmultiplied(bg.r(), bg.g(), bg.b(), 240))
            .stroke(egui::Stroke::new(1.0, ui.visuals().widgets.noninteractive.bg_stroke.color))
            .corner_radius(8.0)
            .inner_margin(egui::Margin::symmetric(10, 6))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    if ui.button(egui::RichText::new("✔ Done").strong())
                        .on_hover_text("Save the menu's screen placement (Esc)")
                        .clicked()
                    {
                        *exit_edit = true;
                    }
                    ui.separator();
                    ui.label(egui::RichText::new(
                        "Drag the menu to move it · drag the corner square to resize")
                        .small().weak());
                });
            });
    });
    let _ = screen;
}
