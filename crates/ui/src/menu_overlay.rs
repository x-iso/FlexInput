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
    /// Radial mode: the same zone tree projected as a sector ring.
    radial: bool,
    /// Pointer deadzone — doubles as the ring's dead-center radius fraction.
    deadzone: f32,
    /// Radial angular origin offset (fraction) — rotates the ring.
    origin: f32,
    /// Configurable main / highlight colours.
    colors: crate::canvas::menu_body::ZoneColors,
    /// Live pointer (unit-rect 0..1, y down) from the eval mirror — drives the
    /// cursor-deflection indicator while the menu is open.
    ptr: Option<egui::Vec2>,
    /// Per-zone icon + name overrides (`zone_meta` param).
    zone_meta: std::collections::HashMap<u32, crate::canvas::menu_body::ZoneMeta>,
    /// Last accepted selection `(zone, seq)` from the eval mirror — the seq
    /// increments per selection, driving the select-linger animation and the
    /// select glow.
    sel: Option<(u32, u32)>,
    /// How long the selected cell stays on screen after the menu hides
    /// before fading out (`select_linger` param, seconds; 0 = off).
    linger_s: f32,
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
        radial: node.params.get("menu_radial").and_then(|v| v.as_bool()).unwrap_or(false),
        deadzone: node.params.get("pointer_deadzone").and_then(|v| v.as_f64()).unwrap_or(0.25) as f32,
        origin: node.params.get("menu_radial_origin").and_then(|v| v.as_f64()).unwrap_or(0.0) as f32,
        colors: crate::canvas::menu_body::ZoneColors::read(node),
        ptr: crate::canvas::menu_body::menu_pointer(node),
        zone_meta: crate::canvas::menu_body::menu_zone_meta(node),
        sel: crate::canvas::menu_body::menu_sel_info(node),
        linger_s: node.params.get("select_linger").and_then(|v| v.as_f64()).unwrap_or(0.5) as f32,
    }
}

/// Per-menu select-linger/glow tracking state (ctx temp):
/// `(seen seq, zone of that seq, linger start time, open last frame,
/// seq when the current/last open session began, glow start time)`.
type LingerState = (u32, u32, f64, bool, u32, f64);

/// Seconds the lingering cell takes to fade out once its hold time is up.
const LINGER_FADE_S: f64 = 0.45;
/// Duration of the select-glow border flash (plays on EVERY selection,
/// open menu or lingering cell alike).
const GLOW_S: f64 = 0.35;

/// Advance one menu's linger/glow state machine. Returns
/// `(linger cell, glow)` — each as `(zone, alpha)`:
/// * linger — the cell of a now-hidden menu to keep painting while it fades.
///   A selection only replays when it happened during the open session that
///   just ended; reopening cancels the fade, and a stale seq from before the
///   overlay started tracking never animates.
/// * glow — the quick border flash marking the moment a zone was accepted.
fn linger_tick(
    ctx: &egui::Context,
    m: &MenuInst,
    now: f64,
) -> (Option<(u32, f32)>, Option<(u32, f32)>) {
    let key = egui::Id::new(("fxi_menu_linger", m.outer.map(|n| n.0), m.inner.0));
    let cur_seq = m.sel.map(|(_, s)| s).unwrap_or(0);
    let mut st: LingerState = ctx.data(|d| d.get_temp(key)).unwrap_or((
        cur_seq, 0, f64::NEG_INFINITY, m.open, cur_seq, f64::NEG_INFINITY,
    ));
    // Selection observed (order matters: before the close check, so a
    // release-select — close + seq bump on the same tick — lingers).
    if let Some((zone, seq)) = m.sel {
        if seq != st.0 {
            st.0 = seq;
            st.1 = zone;
            st.5 = now; // glow flash on every accepted selection
        }
    }
    if m.open && !st.3 {
        // Opened: remember the session's starting seq, cancel any fade.
        st.4 = st.0;
        st.2 = f64::NEG_INFINITY;
    }
    if !m.open && st.3 && st.0 != st.4 {
        // Closed after at least one accepted selection → start the linger.
        st.2 = now;
    }
    st.3 = m.open;
    ctx.data_mut(|d| d.insert_temp(key, st));

    let glow = if st.5.is_finite() && now - st.5 < GLOW_S {
        Some((st.1, (1.0 - ((now - st.5) / GLOW_S) as f32).clamp(0.0, 1.0)))
    } else {
        None
    };

    let linger = if m.open || m.linger_s <= 0.0 || !st.2.is_finite() {
        None
    } else {
        let t = now - st.2;
        let hold = m.linger_s as f64;
        if t >= hold + LINGER_FADE_S {
            None
        } else {
            let alpha = if t <= hold { 1.0 } else { 1.0 - ((t - hold) / LINGER_FADE_S) as f32 };
            Some((st.1, alpha.clamp(0.0, 1.0)))
        }
    };
    (linger, glow)
}

/// The select-glow border stroke for one zone — a bright flash that decays
/// over [`GLOW_S`]. Drawn on top of the live cell (press/click selects) and
/// the lingering cell (release selects) so accepting is unmistakable.
fn paint_zone_glow(ui: &egui::Ui, px: egui::Rect, m: &MenuInst, zone: u32, alpha: f32) {
    let col = egui::Color32::from_rgb(255, 235, 170).gamma_multiply(alpha);
    let stroke = egui::Stroke::new(2.0 + 3.0 * alpha, col);
    let Some((_, band)) = m.tree.zones().into_iter().find(|(z, _)| *z == zone) else {
        return;
    };
    if m.radial {
        let geom = crate::canvas::menu_body::RingGeom::of(px, m.deadzone, m.origin);
        if let Some((r0, r1, a0, a1)) =
            crate::canvas::menu_body::radial_band_geom(&geom, band)
        {
            for s in crate::canvas::menu_body::radial_sector_shapes(
                geom.center, r0, r1, a0, a1, egui::Color32::TRANSPARENT, stroke,
            ) {
                ui.painter().add(s);
            }
        }
    } else {
        let [x0, y0, x1, y1] = band;
        let zr = egui::Rect::from_min_max(
            egui::pos2(px.left() + x0 * px.width(), px.top() + y0 * px.height()),
            egui::pos2(px.left() + x1 * px.width(), px.top() + y1 * px.height()),
        ).shrink(2.0);
        ui.painter().rect_stroke(zr, 5.0, stroke, egui::StrokeKind::Middle);
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
    // Collect the active tab's menus, then drop the borrow so the write-back at
    // the end can also reach any open sub-patch editor (see below).
    let menus = {
        let (tab, _live_signals, _panic) = app.overlay_parts();
        collect_menus(&tab.canvas.snarl)
    };
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

    // Select-linger + select-glow: after a menu hides, its chosen cell stays
    // for `select_linger` seconds and fades; every accepted selection also
    // flashes the cell border. Tracked BEFORE the early return — the close
    // transition is only observable on frames where every menu is already
    // closed. `(menu index, zone, alpha)` per entry.
    let now = ctx.input(|i| i.time);
    let mut lingers: Vec<(usize, u32, f32)> = Vec::new();
    let mut glows: Vec<(usize, u32, f32)> = Vec::new();
    for (i, m) in menus.iter().enumerate() {
        let (linger, glow) = linger_tick(ctx, m, now);
        if let Some((z, a)) = linger { lingers.push((i, z, a)); }
        if let Some((z, a)) = glow { glows.push((i, z, a)); }
    }

    let any_open = menus.iter().any(|m| m.open);
    if !any_open && edit.is_none() && lingers.is_empty() {
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
                    for (i, m) in menus.iter().enumerate() {
                        if !m.open { continue; }
                        paint_menu(ui, to_px(m.rect), m, false);
                        // Select glow on the live cell (press/click selects
                        // keep the menu open).
                        if let Some(&(_, z, a)) = glows.iter().find(|(gi, _, _)| *gi == i) {
                            paint_zone_glow(ui, to_px(m.rect), m, z, a);
                        }
                    }
                    // Lingering select cells of menus that just hid: only the
                    // chosen zone, fading out on its own (+ its glow flash).
                    for &(idx, zone, alpha) in &lingers {
                        let m = &menus[idx];
                        paint_menu_linger(ui, to_px(m.rect), m, zone, alpha);
                        if let Some(&(_, z, a)) = glows.iter().find(|(gi, _, _)| *gi == idx) {
                            paint_zone_glow(ui, to_px(m.rect), m, z, a);
                        }
                    }
                }
            });
    });

    if let Some((outer, inner, r)) = rect_write {
        {
            let (tab, _live_signals, _panic) = app.overlay_parts();
            write_menu_rect(&mut tab.canvas.snarl, outer, inner, r);
        }
        // A sub-patch editor renders (and writes back) its OWN clone of the inner
        // snarl; without this, the next time the user edits the menu the editor's
        // full-snarl write-back clobbers the menu_rect we just wrote to the
        // embedded copy — the placement would silently reset. Mirror the write
        // into the editor's clone so its write-back preserves it.
        app.write_menu_rect_to_editors(outer, inner, r);
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

    if m.radial {
        // Sector ring — the zones carry their own plate fill, no backdrop.
        crate::canvas::menu_body::paint_radial_ring(
            ui, px, &m.tree.zones(), m.deadzone, m.origin,
            if editing { -1 } else { m.hover },
            None, &m.zone_maps, &m.zone_meta, m.colors, None,
            if editing { None } else { m.ptr },
        );
        paint_menu_chip(ui, px, m);
        return;
    }

    p.rect_filled(px, 8.0, crate::canvas::menu_body::plate_fill(m.colors.main));
    p.rect_stroke(px, 8.0, egui::Stroke::new(1.5, m.colors.main), egui::StrokeKind::Inside);

    // Zone cells (plate/hover/icons/labels) via the shared painter — the
    // select-linger fade draws the same cell, so they must match exactly.
    for (zid, [x0, y0, x1, y1]) in m.tree.zones() {
        let zr = egui::Rect::from_min_max(
            egui::pos2(px.left() + x0 * px.width(), px.top() + y0 * px.height()),
            egui::pos2(px.left() + x1 * px.width(), px.top() + y1 * px.height()),
        ).shrink(2.0);
        let hovered = !editing && m.hover == zid as i32;
        crate::canvas::menu_body::paint_grid_zone_cell(
            ui, zr, zid, hovered, &m.zone_maps, &m.zone_meta, m.colors,
        );
    }

    // Cursor-deflection indicator: where the pointer currently is relative to
    // the field center (translucent — blends over icons it crosses).
    if !editing {
        if let Some(c) = m.ptr {
            let pos = egui::pos2(
                px.left() + c.x.clamp(0.0, 1.0) * px.width(),
                px.top() + c.y.clamp(0.0, 1.0) * px.height(),
            );
            crate::canvas::menu_body::paint_menu_cursor(p, px.center(), pos, m.colors.hi);
        }
    }

    paint_menu_chip(ui, px, m);
}

/// Paint just the selected zone cell of a hidden menu at `alpha` — the
/// select-linger: the menu vanished on accept, its chosen cell stays and
/// fades. Geometry matches the live painters exactly (same tree fractions,
/// same shared cell painters).
fn paint_menu_linger(ui: &mut egui::Ui, px: egui::Rect, m: &MenuInst, zone: u32, alpha: f32) {
    let Some((_, band)) = m.tree.zones().into_iter().find(|(z, _)| *z == zone) else {
        return; // zone got restructured away since the selection
    };
    ui.scope(|ui| {
        ui.set_opacity(alpha);
        if m.radial {
            let geom = crate::canvas::menu_body::RingGeom::of(px, m.deadzone, m.origin);
            crate::canvas::menu_body::paint_radial_zone(
                ui, &geom, zone, band, true, false,
                &m.zone_maps, &m.zone_meta, m.colors,
            );
        } else {
            let [x0, y0, x1, y1] = band;
            let zr = egui::Rect::from_min_max(
                egui::pos2(px.left() + x0 * px.width(), px.top() + y0 * px.height()),
                egui::pos2(px.left() + x1 * px.width(), px.top() + y1 * px.height()),
            ).shrink(2.0);
            // Its own little plate — the menu's backdrop is gone, and a bare
            // outlined cell would float unreadably over the game.
            ui.painter().rect_filled(zr, 5.0, crate::canvas::menu_body::plate_fill(m.colors.main));
            crate::canvas::menu_body::paint_grid_zone_cell(
                ui, zr, zone, true, &m.zone_maps, &m.zone_meta, m.colors,
            );
        }
    });
}

/// Name + icon chip above the plate.
fn paint_menu_chip(ui: &mut egui::Ui, px: egui::Rect, m: &MenuInst) {
    let p = ui.painter();
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
