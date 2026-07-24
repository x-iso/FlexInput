//! Info overlay — a transparent, click-through, always-on-top viewport that
//! sits over the whole screen (games included, as long as they run
//! borderless/windowed — exclusive fullscreen bypasses the compositor).
//!
//! Renders the active tab's [`OverlayLayout`] (pinned module elements +
//! decorations; see `canvas::overlay_body`). Four states:
//!
//! * **Hidden** — viewport not declared; eframe destroys the OS window.
//! * **Live** — mouse-passthrough on, never focused; pins render read-only
//!   with live signal glow. Repaint is paced by the `overlay_fps` setting
//!   on the PARENT context — immediate viewports render with the parent,
//!   and egui keeps the earliest deadline, so the background throttle can't
//!   slow the overlay down.
//! * **Edit** — passthrough off, window focused; items get drag/resize/
//!   selection chrome plus a floating toolbar (Done, Add element, snap +
//!   decorations + inspector via the shared `layout_editing_controls_core`).
//!   Esc or Done returns to Live.
//! * **Pick** — entered from the toolbar's Add element: the overlay collapses
//!   to a pulsing amber border (click-through again) while exposable elements
//!   in the main window / first-level sub-patch editors arm amber; app.rs
//!   path-resolves the click and the overlay consumes it back into Edit with
//!   the new pin selected. Esc (in any FlexInput window) cancels.
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


fn visible_id() -> egui::Id { egui::Id::new(OVERLAY_VISIBLE_KEY) }
fn edit_id() -> egui::Id { egui::Id::new(OVERLAY_EDIT_KEY) }

/// Run `f` (typically a blocking native file dialog) with the overlay window
/// temporarily dropped out of always-on-top, then restore topmost.
///
/// **Every blocking `rfd` call on the UI thread must go through this.** Any that
/// doesn't is an overlay hang waiting to be reported: with the overlay up, the
/// dialog opens behind it and the frozen event loop leaves the overlay
/// unable to repaint or toggle passthrough, so it also stops responding until
/// the invisible dialog is dismissed. This was first fixed for the SVG picker
/// alone, and every un-wrapped dialog added since reproduced it exactly.
///
/// Dialogs spawned on a worker thread (`*_async`) don't cause the FREEZE — the
/// UI thread keeps running, so the overlay repaints and manages its own
/// passthrough. They can still open *behind* the topmost overlay, though, so if
/// one of those ever reads as "nothing happened", this is the reason; it wants
/// the topmost drop applied from the worker rather than a wrapper here.
///
/// The overlay is `always_on_top`; a native file dialog opens without an owner
/// window, so it lands BEHIND the topmost overlay and is unreachable. And
/// because `pick_file` blocks the event loop, the overlay can't repaint to
/// toggle its own passthrough while the dialog is up — the window style is
/// frozen at whatever it was when the toolbar button was clicked (interactive,
/// covering the dialog). Dropping the OS topmost bit directly, synchronously,
/// before the blocking call is the reliable fix; we restore it after. No-op
/// off Windows or when the overlay window isn't present.
#[cfg(windows)]
pub fn with_overlay_not_topmost<R>(f: impl FnOnce() -> R) -> R {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        FindWindowW, SetWindowPos, HWND_NOTOPMOST, HWND_TOPMOST,
        SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE,
    };
    let title: Vec<u16> = OVERLAY_WINDOW_TITLE
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();
    // SAFETY: FindWindowW with a null class + valid NUL-terminated title;
    // returns null when no such window exists (overlay hidden).
    let hwnd = unsafe { FindWindowW(std::ptr::null(), title.as_ptr()) };
    let flags = SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE;
    if !hwnd.is_null() {
        unsafe { SetWindowPos(hwnd, HWND_NOTOPMOST, 0, 0, 0, 0, flags) };
    }
    let r = f();
    if !hwnd.is_null() {
        unsafe { SetWindowPos(hwnd, HWND_TOPMOST, 0, 0, 0, 0, flags) };
    }
    r
}

#[cfg(not(windows))]
pub fn with_overlay_not_topmost<R>(f: impl FnOnce() -> R) -> R {
    f()
}

/// OS cursor position in the overlay's logical points. The overlay covers the
/// primary monitor anchored at screen origin, so physical screen pixels divided
/// by the overlay's scale give overlay-local points directly. Read from the OS
/// (not egui) because a click-through window receives no pointer events — this
/// is what lets edit mode decide, each frame, whether the cursor sits over an
/// interactive region (toolbar / pinned item) and flip passthrough accordingly.
#[cfg(windows)]
pub(crate) fn os_cursor_in_points(pixels_per_point: f32) -> Option<egui::Pos2> {
    use windows_sys::Win32::Foundation::POINT;
    use windows_sys::Win32::UI::WindowsAndMessaging::GetCursorPos;
    let mut p = POINT { x: 0, y: 0 };
    // SAFETY: GetCursorPos writes into our POINT; returns 0 on failure.
    if unsafe { GetCursorPos(&mut p) } == 0 {
        return None;
    }
    let ppp = pixels_per_point.max(0.1);
    Some(egui::pos2(p.x as f32 / ppp, p.y as f32 / ppp))
}

#[cfg(not(windows))]
pub(crate) fn os_cursor_in_points(_pixels_per_point: f32) -> Option<egui::Pos2> {
    None
}

/// Ctx temp slot holding last frame's overlay edit-toolbar rect (points), used
/// by the passthrough hit-test (the toolbar's size isn't known until it's laid
/// out, so we hit-test against the previous frame's rect — imperceptible lag).
fn toolbar_rect_id() -> egui::Id { egui::Id::new("fxi_overlay_toolbar_rect") }

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

        // The active GPU backend couldn't give any window a transparent
        // surface (latched by the vendored egui-wgpu when surface creation
        // falls back to an opaque alpha mode) — the overlay composites as an
        // opaque sheet. Warn right next to the toggle.
        if eframe::egui_wgpu::winit::TRANSPARENCY_UNSUPPORTED
            .load(std::sync::atomic::Ordering::Relaxed)
        {
            ui.label(egui::RichText::new("⚠").size(14.0).color(egui::Color32::from_rgb(240, 180, 60)))
                .on_hover_text(
                    "The active GPU backend doesn't support transparent windows,\n\
                     so the overlay renders as an opaque sheet instead of floating\n\
                     over your game. Try switching the Renderer in Settings\n\
                     (DirectX 12 on AMD) and restarting FlexInput.",
                );
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
    // Live repaint cadence — the user's `overlay_fps` setting, NOT the low
    // `bg_repaint_hz` cadence (which would look awful over a game).
    let frame_interval = Duration::from_secs_f64(1.0 / app.overlay_fps() as f64);
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
                    iv_style_override: None,
                    menu_style_override: None,
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
        //
        // In EDIT mode the window is click-through on EMPTY space but
        // interactive over the toolbar or any pinned item — so the user can
        // reach the game / windows behind the overlay while still dragging
        // pins and using the toolbar. The cursor is read from the OS (a
        // click-through window gets no pointer events of its own), hit-tested
        // against the toolbar rect (previous frame) and the item boxes.
        const HIT_MARGIN: f32 = 18.0; // room for selection / resize handles
        let interactive = if edit && !pick {
            // A right-click context menu (or a combo popup) extends BEYOND the
            // item box it was opened from, so hit-testing only the item rect
            // makes the menu click-through the instant the cursor leaves the
            // item — you could only reach "Unpin" on a pin big enough to
            // contain the whole menu. While any popup/menu is open, keep the
            // whole overlay interactive so its entries are reachable. (The
            // popup can only have opened while the cursor was over an
            // interactive region, so this never latches on empty space.
            // Tooltips use a separate order and don't count.)
            if egui::Popup::is_any_open(vctx) {
                true
            } else {
                match os_cursor_in_points(vctx.pixels_per_point()) {
                    // Can't read the cursor → stay interactive (old whole-window
                    // behavior); never worse than before.
                    None => true,
                    Some(c) => {
                        let over_toolbar = vctx
                            .data(|d| d.get_temp::<egui::Rect>(toolbar_rect_id()))
                            .map(|r| r.expand(4.0).contains(c))
                            .unwrap_or(false);
                        let over_item = overlay_layout.items.iter().any(|it| {
                            let (p, s) = it.bbox();
                            egui::Rect::from_min_size(
                                egui::pos2(p[0], p[1]),
                                egui::vec2(s[0].max(8.0), s[1].max(8.0)),
                            )
                            .expand(HIT_MARGIN)
                            .contains(c)
                        });
                        // Keep interactive through an active drag even if the
                        // cursor briefly leaves the item, so a move/resize can't
                        // be cut off by a passthrough flip mid-gesture.
                        let dragging = vctx.input(|i| i.pointer.any_down())
                            || vctx.is_using_pointer();
                        over_toolbar || over_item || dragging
                    }
                }
            }
        } else {
            false
        };

        let pt_id = egui::Id::new("fxi_overlay_passthrough_applied");
        let want_passthrough = if edit && !pick { !interactive } else { true };
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
    ctx.request_repaint_after(frame_interval);
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
    let area_resp = area.show(ui.ctx(), |ui| {
        let bg = ui.visuals().window_fill();
        egui::Frame::default()
            .fill(egui::Color32::from_rgba_unmultiplied(bg.r(), bg.g(), bg.b(), 240))
            .stroke(egui::Stroke::new(1.0, ui.visuals().widgets.noninteractive.bg_stroke.color))
            .corner_radius(8.0)
            .inner_margin(egui::Margin::symmetric(10, 6))
            .show(ui, |ui| {
                // Main controls on row 1; the selected item's properties drop
                // to a full-width row 2 below (so they spread in line instead
                // of stacking vertically in the narrow right margin).
                let sel_module = crate::canvas::overlay_body::overlay_selected_module_info(
                    tab_snarl, overlay_layout,
                );
                let mut state = crate::canvas::viewer::LayoutStateMut::of_overlay(overlay_layout);
                ui.vertical(|ui| {
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
                        crate::canvas::viewer::layout_toolbar_controls_core(ui, &mut state);
                    });
                    // Second row: properties of the selected item, if any.
                    crate::canvas::viewer::layout_inspector_strip_core(ui, &mut state, sel_module);
                });
            });
    });
    // Record the toolbar's rect so next frame's passthrough hit-test keeps the
    // window interactive while the cursor is over it (its size isn't known
    // until laid out here).
    ui.ctx().data_mut(|d| d.insert_temp(toolbar_rect_id(), area_resp.response.rect));
    let _ = rect;
}
