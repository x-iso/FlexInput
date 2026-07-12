//! Info overlay — a transparent, click-through, always-on-top viewport that
//! sits over the whole screen (games included, as long as they run
//! borderless/windowed — exclusive fullscreen bypasses the compositor).
//!
//! Renders the active tab's [`OverlayLayout`] (pinned module elements +
//! decorations; see `canvas::overlay_body`). Three states:
//!
//! * **Hidden** — viewport not declared; eframe destroys the OS window.
//! * **Live** — mouse-passthrough on, never focused; pins render read-only
//!   with live signal glow. Repaint is paced by `OVERLAY_FRAME_INTERVAL`
//!   (becomes the `overlay_fps` setting) on the PARENT context — immediate
//!   viewports render with the parent, and egui keeps the earliest deadline,
//!   so the background throttle can't slow the overlay down.
//! * **Edit** — passthrough off, window focused; items get drag/resize/
//!   selection chrome plus a floating toolbar (Done, Add element, snap +
//!   decorations + inspector via the shared `layout_editing_controls_core`).
//!   Esc or Done returns to Live.
//!
//! Transparency notes (hard-won, see the machine-quirks memory):
//! * The window title + transparent + skip-taskbar combo triggers the
//!   vendored egui-winit patch that sets `WS_EX_NOREDIRECTIONBITMAP` at
//!   creation — without it the never-painted GDI redirection surface
//!   composites as an opaque white sheet under the DirectComposition
//!   swapchain on Win11 26H1 + AMD.
//! * eframe hardcodes a fully transparent clear for immediate viewports
//!   (`wgpu_integration::render_immediate_viewport`), so the app's opaque
//!   `clear_color` never paints over the overlay.
//!
//! Visible/edit state lives in ctx temp-data slots (same pattern as the
//! see-through eye toggle) so the title-bar buttons can flip them without
//! threading `&mut FlexInputApp` through the title-bar renderer.

use std::time::Duration;

use crate::app::FlexInputApp;

/// Ctx temp-data slot holding the overlay's visible flag.
pub const OVERLAY_VISIBLE_KEY: &str = "fxi_overlay_visible";
/// Ctx temp-data slot holding the overlay's edit-mode flag.
pub const OVERLAY_EDIT_KEY: &str = "fxi_overlay_edit";

/// Window title of the overlay viewport. Keep in sync with the vendored
/// egui-winit expectations documented above (the NOREDIRECTIONBITMAP patch
/// keys on transparent + skip-taskbar, not the title — the title is only
/// cosmetic, but stays unique for debuggability).
const OVERLAY_WINDOW_TITLE: &str = "FlexInput Overlay";

/// Repaint cadence while the overlay is visible. Hardcoded ~60 FPS for now;
/// becomes the `overlay_fps` setting (the low `bg_repaint_hz` cadence would
/// look awful animating on top of a game).
const OVERLAY_FRAME_INTERVAL: Duration = Duration::from_millis(16);

fn visible_id() -> egui::Id { egui::Id::new(OVERLAY_VISIBLE_KEY) }
fn edit_id() -> egui::Id { egui::Id::new(OVERLAY_EDIT_KEY) }

pub fn overlay_visible(ctx: &egui::Context) -> bool {
    ctx.data(|d| d.get_temp::<bool>(visible_id())).unwrap_or(false)
}

pub fn set_overlay_visible(ctx: &egui::Context, on: bool) {
    ctx.data_mut(|d| d.insert_temp(visible_id(), on));
    if !on {
        set_overlay_edit(ctx, false);
    }
}

pub fn overlay_edit(ctx: &egui::Context) -> bool {
    ctx.data(|d| d.get_temp::<bool>(edit_id())).unwrap_or(false)
}

pub fn set_overlay_edit(ctx: &egui::Context, on: bool) {
    ctx.data_mut(|d| d.insert_temp(edit_id(), on));
    if !on {
        // Leaving edit always tears down a pick in progress.
        crate::canvas::viewer::set_overlay_pick_active(ctx, false);
    }
}

/// Title-bar buttons: ▣ toggles the overlay, ✏ (shown while visible) enters
/// edit mode. Same selectable-button look as the see-through eye.
pub fn render_overlay_toggle(ui: &mut egui::Ui, bar_h: f32) {
    let on = overlay_visible(ui.ctx());
    let btn_size = egui::vec2(26.0, (bar_h - 6.0).max(18.0));

    let resp = ui.add_sized(
        btn_size,
        egui::Button::selectable(on, egui::RichText::new("▣").size(14.0)),
    );
    let hover = if on {
        "Overlay: ON — click to hide.\nA transparent, click-through layer over the whole screen.\nWorks over borderless/windowed games (not exclusive fullscreen)."
    } else {
        "Overlay: OFF — click to show the info overlay.\nA transparent, click-through layer over the whole screen.\nWorks over borderless/windowed games (not exclusive fullscreen)."
    };
    if resp.on_hover_text(hover).clicked() {
        set_overlay_visible(ui.ctx(), !on);
    }

    if on {
        let editing = overlay_edit(ui.ctx());
        let resp = ui.add_sized(
            btn_size,
            egui::Button::selectable(editing, egui::RichText::new("✏").size(13.0)),
        );
        if resp
            .on_hover_text("Edit the overlay: move/resize pinned elements, add decorations.\nEsc or Done exits back to click-through.")
            .clicked()
        {
            set_overlay_edit(ui.ctx(), !editing);
        }
    }
}

/// Show the overlay viewport (call once per frame from `FlexInputApp::update`,
/// after the sub-patch editors). No-op while hidden.
pub fn show_overlay(app: &mut FlexInputApp, ctx: &egui::Context) {
    if !overlay_visible(ctx) {
        return;
    }

    // Cover the monitor the main window is on. egui only exposes the monitor
    // SIZE (points), not its desktop origin, so the overlay is anchored at
    // (0,0) — the primary monitor. A monitor picker comes later.
    let monitor_size = ctx
        .input(|i| i.viewport().monitor_size)
        .filter(|s| s.x > 1.0 && s.y > 1.0)
        .unwrap_or(egui::vec2(1920.0, 1080.0));

    let edit = overlay_edit(ctx);
    // Pick mode is only meaningful while editing (it's entered from the edit
    // toolbar); anything else left it stale — clear it.
    let mut pick = crate::canvas::viewer::overlay_pick_active(ctx);
    if pick && !edit {
        crate::canvas::viewer::set_overlay_pick_active(ctx, false);
        pick = false;
    }

    let (tab, live_signals, panic_shortcut) = app.overlay_parts();
    let tab_snarl = &mut tab.canvas.snarl;
    let overlay_layout = &mut tab.overlay;

    // A pick landed this frame (main canvas and sub-patch editors run before
    // us in `update`, so the path-resolved result is already stashed): add
    // the pin near the overlay center — cascaded so consecutive pins don't
    // stack exactly — select it, and drop back to plain edit mode.
    if pick {
        if let Some((source_path, inner_uid, eid, size)) =
            crate::canvas::viewer::take_overlay_pick_result(ctx)
        {
            let init_size = if size[0] >= 1.0 && size[1] >= 1.0 { size } else { [220.0, 100.0] };
            let n = overlay_layout.items.len() as f32;
            let cascade = (n % 8.0) * 28.0;
            let pos = [
                (monitor_size.x - init_size[0]) * 0.5 + cascade,
                (monitor_size.y - init_size[1]) * 0.5 + cascade,
            ];
            overlay_layout.items.push(crate::canvas::node::LayoutItem::Module(
                crate::canvas::node::ExposedModule {
                    inner_node_id: inner_uid,
                    element_id: eid,
                    pos,
                    size: init_size,
                    text_override: None,
                    switch_override: None,
                    graph_override: None,
                    source_path,
                },
            ));
            let idx = overlay_layout.items.len() - 1;
            overlay_layout.selected_item = Some(idx);
            overlay_layout.selected_items = vec![idx];
            overlay_layout.cycle_pos = None;
            crate::canvas::viewer::set_overlay_pick_active(ctx, false);
            pick = false;
        }
    }

    let viewport_id = egui::ViewportId::from_hash_of("fxi_overlay");
    let builder = egui::ViewportBuilder::default()
        .with_title(OVERLAY_WINDOW_TITLE)
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

    ctx.show_viewport_immediate(viewport_id, builder, |vctx, _class| {
        // Self-correct geometry from INSIDE the viewport: the builder size was
        // computed from the parent's monitor before the child window existed
        // (wrong DPI/monitor possible). The child's own input has the truth.
        let (inner, child_monitor) = vctx.input(|i| {
            (i.viewport().inner_rect, i.viewport().monitor_size)
        });
        if let (Some(inner), Some(want)) = (inner, child_monitor) {
            if (inner.size() - want).length() > 1.0 {
                vctx.send_viewport_cmd(egui::ViewportCommand::OuterPosition(egui::pos2(0.0, 0.0)));
                vctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(want));
            }
        }

        // Mouse passthrough follows the mode; only send the command when the
        // desired state changes (each send is a SetWindowLong round-trip).
        // During a pick the overlay goes back to click-through so the click
        // lands on the FlexInput window underneath/behind it.
        let pt_id = egui::Id::new("fxi_overlay_passthrough_applied");
        let want_passthrough = !edit || pick;
        let applied: Option<bool> = vctx.data(|d| d.get_temp(pt_id));
        if applied != Some(want_passthrough) {
            vctx.send_viewport_cmd(egui::ViewportCommand::MousePassthrough(want_passthrough));
            if !want_passthrough {
                vctx.send_viewport_cmd(egui::ViewportCommand::Focus);
            }
            vctx.data_mut(|d| d.insert_temp(pt_id, want_passthrough));
        }

        if edit && !pick && vctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            exit_edit = true;
        }

        egui::CentralPanel::default()
            .frame(egui::Frame::NONE)
            .show(vctx, |ui| {
                let rect = ui.max_rect();

                if pick {
                    // Pick state: the overlay collapses to a glowing border
                    // frame (pin-mode indicator) + a hint chip. Items stay
                    // hidden so the FlexInput window behind is unobstructed.
                    paint_pick_frame(ui, rect);
                    return;
                }

                if edit {
                    // Faint dim so edit mode reads as a distinct state.
                    ui.painter().rect_filled(rect, 0.0, egui::Color32::from_black_alpha(48));
                    if overlay_layout.items.is_empty() {
                        ui.painter().text(
                            rect.center(),
                            egui::Align2::CENTER_CENTER,
                            "Overlay is empty — right-click for decorations,\nor use “Add element” to pin module UI here.",
                            egui::FontId::proportional(15.0),
                            egui::Color32::from_rgba_unmultiplied(220, 235, 255, 220),
                        );
                    }
                }

                crate::canvas::overlay_body::show_overlay_body(
                    ui, rect, tab_snarl, overlay_layout, edit,
                    live_signals, &panic_shortcut,
                );

                if edit {
                    overlay_edit_toolbar(ui, rect, tab_snarl, overlay_layout, &mut exit_edit);
                }
            });
    });

    if exit_edit {
        set_overlay_edit(ctx, false);
    }

    // Live signal glow / animations need a real cadence; pace the PARENT
    // context (immediate viewports render with the parent; egui keeps the
    // earliest requested deadline, so the bg throttle can't override this).
    ctx.request_repaint_after(OVERLAY_FRAME_INTERVAL);
}

/// Pick-state visuals: a pulsing amber border frame hugging the screen edge
/// (the "pin mode" indicator) plus a hint chip below the top edge. Painted on
/// a click-through window, so nothing here can intercept the pick click.
fn paint_pick_frame(ui: &mut egui::Ui, rect: egui::Rect) {
    let t = ui.input(|i| i.time);
    // Slow pulse 0..1 — enough to read as "armed", not enough to distract.
    let pulse = (0.5 + 0.5 * (t * 2.2).sin()) as f32;
    let base = egui::Color32::from_rgb(255, 180, 60);
    let p = ui.painter();

    // Layered inset strokes fading inward fake an outer-glow the painter
    // can't do natively.
    for (inset, width, alpha) in [
        (2.0_f32, 4.0_f32, 200.0_f32),
        (7.0, 6.0, 90.0),
        (14.0, 8.0, 40.0),
    ] {
        let a = (alpha * (0.55 + 0.45 * pulse)) as u8;
        p.rect_stroke(
            rect.shrink(inset),
            10.0,
            egui::Stroke::new(width, egui::Color32::from_rgba_unmultiplied(base.r(), base.g(), base.b(), a)),
            egui::StrokeKind::Inside,
        );
    }

    // Hint chip.
    let hint = "PIN MODE — click an amber-highlighted element in FlexInput to pin it here (Esc cancels)";
    let font = egui::FontId::proportional(14.0);
    let galley = p.layout_no_wrap(hint.to_string(), font, egui::Color32::from_rgb(255, 225, 170));
    let pad = egui::vec2(14.0, 8.0);
    let chip = egui::Rect::from_center_size(
        egui::pos2(rect.center().x, rect.top() + 46.0),
        galley.size() + pad * 2.0,
    );
    p.rect_filled(chip, 8.0, egui::Color32::from_rgba_unmultiplied(30, 22, 8, 230));
    p.rect_stroke(chip, 8.0, egui::Stroke::new(1.0, base), egui::StrokeKind::Inside);
    p.galley(chip.min + pad, galley, egui::Color32::WHITE);

    // The pulse needs frames; the parent-paced overlay interval covers it.
}

/// Floating toolbar shown top-center in edit mode: Done, Add element, and the
/// shared layout tools (snap grid, decoration adders, inspector strip).
fn overlay_edit_toolbar(
    ui: &mut egui::Ui,
    rect: egui::Rect,
    tab_snarl: &mut egui_snarl::Snarl<crate::canvas::NodeData>,
    overlay_layout: &mut crate::canvas::OverlayLayout,
    exit_edit: &mut bool,
) {
    let area = egui::Area::new(egui::Id::new("fxi_overlay_toolbar"))
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
                        .on_hover_text("Exit overlay editing (Esc)")
                        .clicked()
                    {
                        *exit_edit = true;
                    }
                    ui.separator();
                    if ui.button("➕ Add element")
                        .on_hover_text("Pick a module UI element to pin: the overlay collapses to a\nglowing border and pinnable elements light up amber in the\nFlexInput window (and first-level sub-patch editors). Esc cancels.")
                        .clicked()
                    {
                        crate::canvas::viewer::set_overlay_pick_active(ui.ctx(), true);
                        // Bring the main window forward so the highlighted
                        // elements are actually visible/clickable.
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
                    let sel_module = crate::canvas::overlay_body::overlay_selected_module_info(
                        tab_snarl, overlay_layout,
                    );
                    crate::canvas::viewer::layout_editing_controls_core(
                        ui,
                        &mut crate::canvas::viewer::LayoutStateMut::of_overlay(overlay_layout),
                        sel_module,
                    );
                });
            });
    });
    let _ = rect;
}
