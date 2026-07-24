//! Config overlay (M3) — a shortcut-summoned, transparent, always-on-top layer
//! for tweaking module parameters LIVE while a game runs. Unlike the info
//! overlay (display-only, click-through), the config overlay pins the module's
//! INTERACTIVE elements (sliders, response curves, toggles, dropdowns, numeric
//! rows) and is interactive over them (and its toolbar) while click-through
//! everywhere else — so the game behind stays reachable but every pin can be
//! adjusted on the fly. Its defining behavior — suppress the inputs used to
//! navigate it, pass through the input the tweaked parameter affects — lands in
//! later phases (M3.3/M3.4) on the `__src_block__` machinery.
//!
//! M3.2: curated tweak-pins. The pick flow reuses the info overlay's armed-pick
//! machinery (amber highlights + the app.rs/subpatch.rs path resolution) via a
//! DESTINATION discriminator (`overlay_pick_dest_config`), and filters picks to
//! [`is_editable_element`] so only adjustable controls can be pinned. Rendering
//! + edit-mode arrange reuse [`show_overlay_body`] and the shared layout tools
//! verbatim, driving the per-tab `config` [`OverlayLayout`] instead of `overlay`.
//!
//! Same transparency machinery as the info + menu overlays (unique title +
//! transparent + skip-taskbar triggers the vendored NOREDIRECTIONBITMAP patch;
//! passthrough commands latched per state change).

use std::time::Duration;

use egui_snarl::Snarl;

use crate::app::FlexInputApp;
use crate::canvas::node::{ExposedModule, LayoutItem, OverlayLayout};
use crate::canvas::NodeData;

const CONFIG_OVERLAY_TITLE: &str = "FlexInput Config Overlay";

/// Ctx temp-data slot: is the config overlay currently summoned?
pub const CONFIG_OVERLAY_VISIBLE_KEY: &str = "fxi_config_overlay_visible";
/// Ctx temp-data slot: is the config overlay in edit (arrange tweak-pins) mode?
pub const CONFIG_OVERLAY_EDIT_KEY: &str = "fxi_config_overlay_edit";
/// Ctx temp-data slot: wall-clock time a non-editable pick was rejected, so the
/// "not tweakable" hint chip can fade after a couple seconds.
const CONFIG_REJECT_KEY: &str = "fxi_config_pick_rejected";
/// Ctx temp-data slot: the physical device id whose input should PASS THROUGH to
/// the game right now — the upstream device of the tweak-pin under the cursor
/// (M3.4). Empty string = nothing passing through. Written by the overlay each
/// frame; read by `FlexInputApp::update` when building the source-block set.
pub const CONFIG_PASSTHROUGH_DEV_KEY: &str = "fxi_config_passthrough_dev";

fn visible_id() -> egui::Id {
    egui::Id::new(CONFIG_OVERLAY_VISIBLE_KEY)
}
fn edit_id() -> egui::Id {
    egui::Id::new(CONFIG_OVERLAY_EDIT_KEY)
}
/// Toolbar bounds published each frame so the passthrough hit-test (which reads
/// the OS cursor — a click-through window gets no pointer events) keeps the
/// window interactive while the cursor is over the toolbar.
fn toolbar_rect_id() -> egui::Id {
    egui::Id::new("fxi_config_toolbar_rect")
}
fn reject_id() -> egui::Id {
    egui::Id::new(CONFIG_REJECT_KEY)
}
fn passthrough_dev_id() -> egui::Id {
    egui::Id::new(CONFIG_PASSTHROUGH_DEV_KEY)
}

/// The physical device the config overlay wants passed through to the game right
/// now (the tweak-pin under the cursor's upstream device), or `None`. Read by
/// `update()` to poke a hole in the source-block set.
pub fn config_passthrough_dev(ctx: &egui::Context) -> Option<String> {
    ctx.data(|d| d.get_temp::<String>(passthrough_dev_id()))
        .filter(|s| !s.is_empty())
}

pub fn config_overlay_visible(ctx: &egui::Context) -> bool {
    ctx.data(|d| d.get_temp::<bool>(visible_id())).unwrap_or(false)
}

pub fn set_config_overlay_visible(ctx: &egui::Context, on: bool) {
    ctx.data_mut(|d| d.insert_temp(visible_id(), on));
    // Leaving the overlay drops edit mode too (mirrors set_overlay_visible),
    // and clears any lingering passthrough request so a closed overlay can't
    // keep a device unblocked.
    if !on {
        ctx.data_mut(|d| {
            d.remove_temp::<bool>(edit_id());
            d.remove_temp::<String>(passthrough_dev_id());
        });
    }
}

pub fn config_overlay_edit(ctx: &egui::Context) -> bool {
    ctx.data(|d| d.get_temp::<bool>(edit_id())).unwrap_or(false)
}

pub fn set_config_overlay_edit(ctx: &egui::Context, on: bool) {
    ctx.data_mut(|d| d.insert_temp(edit_id(), on));
}

/// Show the config overlay viewport (call once per frame from
/// `FlexInputApp::update`, right after the menu overlay). No-op while hidden.
pub fn show_config_overlay(app: &mut FlexInputApp, ctx: &egui::Context) {
    if !config_overlay_visible(ctx) {
        return;
    }
    let edit = config_overlay_edit(ctx);
    let frame_interval = Duration::from_secs_f64(1.0 / app.overlay_fps() as f64);

    // A pick is only ours if it was armed by the config overlay. It's only
    // meaningful while editing (entered from the toolbar) — clear a stray one.
    let mut pick = crate::canvas::viewer::overlay_pick_active(ctx)
        && crate::canvas::viewer::overlay_pick_dest_config(ctx);
    if pick && !edit {
        crate::canvas::viewer::set_overlay_pick_active(ctx, false);
        pick = false;
    }

    let monitor_size = ctx
        .input(|i| i.viewport().monitor_size)
        .filter(|s| s.x > 1.0 && s.y > 1.0)
        .unwrap_or(egui::vec2(1920.0, 1080.0));

    let (tab, live_signals, panic_shortcut) = app.overlay_parts();
    // Disjoint field borrows: the snarl renders the pins, the config layout is
    // edited (mirrors `show_overlay`'s split on `tab.overlay`).
    let tab_snarl = &mut tab.canvas.snarl;
    let config_layout = &mut tab.config;

    // A pick landed this frame (the main canvas + sub-patch editors ran before
    // us in `update`, so the path-resolved result is already stashed). Only pin
    // it if the picked element is an editable control; otherwise flash a hint.
    if pick {
        if let Some((source_path, inner_uid, eid, size)) =
            crate::canvas::viewer::take_overlay_pick_result(ctx)
        {
            let editable = crate::canvas::overlay_body::resolve_overlay_module(
                tab_snarl, &source_path, inner_uid,
            )
            .map(|n| crate::canvas::viewer::is_editable_element(&n.module_id, &eid))
            .unwrap_or(false);
            if editable {
                let init_size = if size[0] >= 1.0 && size[1] >= 1.0 { size } else { [220.0, 100.0] };
                let n = config_layout.items.len() as f32;
                let cascade = (n % 8.0) * 28.0;
                let pos = [
                    (monitor_size.x - init_size[0]) * 0.5 + cascade,
                    (monitor_size.y - init_size[1]) * 0.5 + cascade,
                ];
                config_layout.items.push(LayoutItem::Module(ExposedModule {
                    inner_node_id: inner_uid,
                    element_id: eid,
                    pos,
                    size: init_size,
                    text_override: None,
                    switch_override: None,
                    graph_override: None,
                    source_path,
                    iv_style_override: None,
                    menu_style_override: None,
                }));
                let idx = config_layout.items.len() - 1;
                config_layout.selected_item = Some(idx);
                config_layout.selected_items = vec![idx];
                config_layout.cycle_pos = None;
            } else {
                // Not an adjustable control (a viewer, scope, readout, label…).
                // Read the clock BEFORE taking the data lock — `ctx.input` and
                // `ctx.data_mut` both lock the same Context RwLock, so nesting
                // them self-deadlocks (epaint's 10s watchdog then panics).
                let now = ctx.input(|i| i.time);
                ctx.data_mut(|d| d.insert_temp(reject_id(), now));
            }
            crate::canvas::viewer::set_overlay_pick_active(ctx, false);
            pick = false;
        }
    }

    let viewport_id = egui::ViewportId::from_hash_of("fxi_config_overlay");
    let builder = egui::ViewportBuilder::default()
        .with_title(CONFIG_OVERLAY_TITLE)
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
    let mut enter_edit = false;
    let mut close = false;

    ctx.show_viewport_immediate(viewport_id, builder, |vctx, _class| {
        // Geometry self-correction from inside the viewport (see overlay.rs):
        // force the window to fill the monitor if the OS handed us a wrong size.
        let (inner_rect, child_monitor) = vctx.input(|i| {
            (i.viewport().inner_rect, i.viewport().monitor_size)
        });
        if let (Some(inner_rect), Some(want)) = (inner_rect, child_monitor) {
            if (inner_rect.size() - want).length() > 1.0 {
                vctx.send_viewport_cmd(egui::ViewportCommand::OuterPosition(egui::pos2(0.0, 0.0)));
                vctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(want));
            }
        }

        // Passthrough: click-through EXCEPT while the cursor is over the toolbar
        // or a pinned item (so the game stays reachable and the pins/toolbar stay
        // usable), or while a drag/popup is in flight. During a pick the window
        // goes fully click-through so the pin click lands on FlexInput behind it.
        const HIT_MARGIN: f32 = 14.0;
        let interactive = if pick {
            false
        } else if egui::Popup::is_any_open(vctx)
            || vctx.input(|i| i.pointer.any_down())
            || vctx.is_using_pointer()
        {
            true
        } else {
            match crate::overlay::os_cursor_in_points(vctx.pixels_per_point()) {
                None => true, // can't read cursor → stay interactive (never worse)
                Some(c) => {
                    let over_toolbar = vctx
                        .data(|d| d.get_temp::<egui::Rect>(toolbar_rect_id()))
                        .map(|r| r.expand(4.0).contains(c))
                        .unwrap_or(false);
                    // Topmost tweak-pin under the cursor (items paint bottom→top,
                    // so the LAST match is on top). In live tweak mode its
                    // upstream physical device passes through to the game (M3.4)
                    // — you feel the parameter's effect while adjusting and the
                    // pin's live graph dot keeps moving.
                    let active_item = config_layout.items.iter().rev().find(|it| {
                        let (p, s) = it.bbox();
                        egui::Rect::from_min_size(
                            egui::pos2(p[0], p[1]),
                            egui::vec2(s[0].max(8.0), s[1].max(8.0)),
                        )
                        .expand(HIT_MARGIN)
                        .contains(c)
                    });
                    let dev = if !edit {
                        active_item.and_then(|it| match it {
                            LayoutItem::Module(m) => crate::app::config_passthrough_device(
                                tab_snarl, &m.source_path, m.inner_node_id,
                            ),
                            _ => None,
                        })
                    } else {
                        None
                    };
                    vctx.data_mut(|d| d.insert_temp(passthrough_dev_id(), dev.unwrap_or_default()));
                    over_toolbar || active_item.is_some()
                }
            }
        };
        let pt_id = egui::Id::new("fxi_config_passthrough_applied");
        let want_passthrough = !interactive;
        let applied: Option<bool> = vctx.data(|d| d.get_temp(pt_id));
        if applied != Some(want_passthrough) {
            vctx.send_viewport_cmd(egui::ViewportCommand::MousePassthrough(want_passthrough));
            if !want_passthrough {
                vctx.send_viewport_cmd(egui::ViewportCommand::Focus);
            }
            vctx.data_mut(|d| d.insert_temp(pt_id, want_passthrough));
        }

        // Esc: in a pick the main window handles cancel; otherwise Esc exits
        // edit mode, or (in live mode) dismisses the overlay.
        if !pick && vctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            if edit { exit_edit = true; } else { close = true; }
        }

        egui::CentralPanel::default()
            .frame(egui::Frame::NONE)
            .show(vctx, |ui| {
                let rect = ui.max_rect();

                if pick {
                    // Pick state: collapse to the glowing pin-mode border (shared
                    // with the info overlay) so the FlexInput window behind is
                    // unobstructed while an element is chosen.
                    crate::overlay::paint_pick_frame(ui, rect);
                    return;
                }

                if edit {
                    // Faint dim so edit mode reads as a distinct state.
                    ui.painter().rect_filled(rect, 0.0, egui::Color32::from_black_alpha(48));
                    if config_layout.items.is_empty() {
                        ui.painter().text(
                            rect.center(),
                            egui::Align2::CENTER_CENTER,
                            "No tweak-pins yet — use “Add element” to pin an\nadjustable control (slider, curve, toggle, numeric row).",
                            egui::FontId::proportional(15.0),
                            egui::Color32::from_rgba_unmultiplied(220, 235, 255, 220),
                        );
                    }
                }

                crate::canvas::overlay_body::show_overlay_body(
                    ui, rect, tab_snarl, config_layout, edit,
                    live_signals, &panic_shortcut,
                );

                paint_reject_hint(ui, rect);

                config_toolbar(
                    ui, tab_snarl, config_layout, edit,
                    &mut exit_edit, &mut enter_edit, &mut close,
                );
            });
    });

    if enter_edit {
        set_config_overlay_edit(ctx, true);
    }
    if exit_edit {
        set_config_overlay_edit(ctx, false);
        crate::canvas::viewer::set_overlay_pick_active(ctx, false);
    }
    if close {
        set_config_overlay_visible(ctx, false);
    }
    // Pace the parent context (immediate viewports render with the parent).
    ctx.request_repaint_after(frame_interval);
}

/// The always-present top-center toolbar: title, edit toggle, Done. In edit
/// mode it expands with "Add element" + the shared layout tools (snap grid,
/// decoration adders) on row 1 and the selected item's inspector strip on row 2.
fn config_toolbar(
    ui: &mut egui::Ui,
    tab_snarl: &Snarl<NodeData>,
    config_layout: &mut OverlayLayout,
    edit: bool,
    exit_edit: &mut bool,
    enter_edit: &mut bool,
    close: &mut bool,
) {
    let area = egui::Area::new(egui::Id::new("fxi_config_toolbar"))
        .order(egui::Order::Foreground)
        .anchor(egui::Align2::CENTER_TOP, egui::vec2(0.0, 12.0))
        .interactable(true);
    let area_resp = area.show(ui.ctx(), |ui| {
        let bg = ui.visuals().window_fill();
        egui::Frame::default()
            .fill(egui::Color32::from_rgba_unmultiplied(bg.r(), bg.g(), bg.b(), 240))
            .stroke(egui::Stroke::new(1.0, egui::Color32::from_rgb(120, 140, 200)))
            .corner_radius(8.0)
            .inner_margin(egui::Margin::symmetric(10, 6))
            .show(ui, |ui| {
                // Selected-item info computed before the mutable layout borrow.
                let sel_module =
                    crate::canvas::overlay_body::overlay_selected_module_info(tab_snarl, config_layout);
                let mut state = crate::canvas::viewer::LayoutStateMut::of_overlay(config_layout);
                ui.vertical(|ui| {
                    ui.horizontal(|ui| {
                        ui.label(
                            egui::RichText::new("⚙ Config")
                                .strong()
                                .color(egui::Color32::from_rgb(200, 215, 255)),
                        );
                        ui.separator();
                        if ui
                            .add(egui::Button::selectable(edit, egui::RichText::new("✏ Edit")))
                            .on_hover_text("Arrange tweak-pins: add / move / resize / remove.\nExit back to live tweaking with Esc or Done.")
                            .clicked()
                        {
                            if edit { *exit_edit = true; } else { *enter_edit = true; }
                        }
                        if ui
                            .button(egui::RichText::new("✔ Done").strong())
                            .on_hover_text("Close the config overlay (or press the shortcut).")
                            .clicked()
                        {
                            *close = true;
                        }
                        if edit {
                            ui.separator();
                            if ui.button("➕ Add element")
                                .on_hover_text("Pick an adjustable control to pin: the overlay collapses to a\nglowing border and pinnable elements light up amber in the\nFlexInput window. Non-adjustable elements are ignored. Esc cancels.")
                                .clicked()
                            {
                                crate::canvas::viewer::set_overlay_pick_active(ui.ctx(), true);
                                crate::canvas::viewer::set_overlay_pick_dest_config(ui.ctx(), true);
                                // Bring the main window forward so the highlighted
                                // elements are visible/clickable.
                                ui.ctx().send_viewport_cmd_to(
                                    egui::ViewportId::ROOT,
                                    egui::ViewportCommand::Minimized(false),
                                );
                                ui.ctx().send_viewport_cmd_to(
                                    egui::ViewportId::ROOT,
                                    egui::ViewportCommand::Focus,
                                );
                            }
                            ui.separator();
                            crate::canvas::viewer::layout_toolbar_controls_core(ui, &mut state);
                        }
                    });
                    if edit {
                        crate::canvas::viewer::layout_inspector_strip_core(ui, &mut state, sel_module);
                    }
                });
            });
    });
    ui.ctx().data_mut(|d| d.insert_temp(toolbar_rect_id(), area_resp.response.rect));
}

/// Flash a "not tweakable" chip for a couple seconds after a rejected pick.
fn paint_reject_hint(ui: &mut egui::Ui, rect: egui::Rect) {
    let Some(t0) = ui.ctx().data(|d| d.get_temp::<f64>(reject_id())) else { return; };
    let age = ui.input(|i| i.time) - t0;
    if age > 2.5 {
        ui.ctx().data_mut(|d| d.remove_temp::<f64>(reject_id()));
        return;
    }
    let fade = (1.0 - (age / 2.5)).clamp(0.0, 1.0) as f32;
    let msg = "That element isn't adjustable — pick a slider, curve, toggle, or numeric row.";
    let font = egui::FontId::proportional(14.0);
    let p = ui.painter();
    let galley = p.layout_no_wrap(msg.to_string(), font, egui::Color32::from_rgb(255, 210, 160));
    let pad = egui::vec2(14.0, 8.0);
    let chip = egui::Rect::from_center_size(
        egui::pos2(rect.center().x, rect.top() + 90.0),
        galley.size() + pad * 2.0,
    );
    let a = |base: u8| (base as f32 * fade) as u8;
    p.rect_filled(chip, 8.0, egui::Color32::from_rgba_unmultiplied(40, 26, 10, a(235)));
    p.rect_stroke(
        chip, 8.0,
        egui::Stroke::new(1.0, egui::Color32::from_rgba_unmultiplied(230, 160, 60, a(255))),
        egui::StrokeKind::Inside,
    );
    p.galley(chip.min + pad, galley, egui::Color32::from_rgba_unmultiplied(255, 235, 200, a(255)));
    // Fade needs frames; the parent-paced overlay interval covers it.
}
