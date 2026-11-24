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
//! **In a window** (Settings → Pin & overlays): the same overlay in an ordinary
//! window of its own - in the taskbar, under whatever is in front, never
//! click-through - filled with a background colour, for a stream's capture (OBS)
//! to take and key out instead of the overlay covering the game. It shows the
//! layout made for the screen, scaled to fit the window (`fit_screen`), so the
//! layout is the one edited either way and lines up with the game when the
//! capture is stretched over it.
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

/// Title of the overlay shown in a window of its own: what a capture (OBS) finds it by.
/// Not [`OVERLAY_WINDOW_TITLE`], which [`with_overlay_not_topmost`] finds to put back on top.
const OVERLAY_OWN_WINDOW_TITLE: &str = "FlexInput Info Overlay";

/// The info overlay in a window of its own instead of over the screen (Settings → Pin &
/// overlays): what it takes from the settings.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct OverlayWindow {
    /// Its background: the colour a capture keys out.
    pub bg: egui::Color32,
    /// Where it was last and how big inside: `[x, y, w, h]` in 100% points.
    pub rect: Option<[f32; 4]>,
}

/// How the overlay window shows the screen, for Settings to say: the share of the screen's
/// pixels it draws it with (1.0 = as many) and the screen's size in pixels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct WindowFit {
    pub shown: f32,
    pub screen_px: egui::Vec2,
}

/// The size (and place, when it's on a monitor) the overlay window opened with, in 100%
/// points: kept while it's open, since a builder following the saved place would move and
/// resize the window under the user's own drag.
type WindowOpened = ([f32; 2], Option<[f32; 2]>);

fn in_window_id() -> egui::Id { egui::Id::new("fxi_overlay_in_window") }
fn passthrough_applied_id() -> egui::Id { egui::Id::new("fxi_overlay_passthrough_applied") }
fn window_opened_id() -> egui::Id { egui::Id::new("fxi_overlay_window_opened") }
fn window_fit_id() -> egui::Id { egui::Id::new("fxi_overlay_window_fit") }
fn window_actual_size_id() -> egui::Id { egui::Id::new("fxi_overlay_window_actual_size") }

/// How the open overlay window shows the screen; `None` while it isn't open.
pub(crate) fn overlay_window_fit(ctx: &egui::Context) -> Option<WindowFit> {
    ctx.data(|d| d.get_temp(window_fit_id()))
}

/// Size the open overlay window to the screen's pixels, so it shows it at 100%.
pub(crate) fn request_overlay_window_actual_size(ctx: &egui::Context) {
    ctx.data_mut(|d| d.insert_temp(window_actual_size_id(), true));
}

/// Where a screen of `screen` points goes in a window's inside `win`: as large as fits,
/// its shape kept, centred - the window's background fills the rest.
pub(crate) fn fit_screen(win: egui::Rect, screen: egui::Vec2) -> egui::emath::TSTransform {
    let scale = (win.width() / screen.x.max(1.0)).min(win.height() / screen.y.max(1.0)).max(0.01);
    let at = win.min + (win.size() - screen * scale) * 0.5;
    egui::emath::TSTransform::new(at.to_vec2(), scale)
}

/// The overlay window's builder: titled for a capture to find, opening where it was last
/// (when that is still on a monitor) at the size it had, half the screen the first time.
fn own_window_builder(ctx: &egui::Context, w: OverlayWindow, screen: egui::Vec2) -> egui::ViewportBuilder {
    let zoom = ctx.zoom_factor().max(0.1);
    let kept: Option<WindowOpened> = ctx.data(|d| d.get_temp(window_opened_id()));
    let (size, pos) = kept.unwrap_or_else(|| {
        let opened = match w.rect {
            Some([x, y, wd, ht]) => ([wd, ht], crate::monitors::on_a_monitor([x, y]).then_some([x, y])),
            None => ([screen.x * 0.5 * zoom, screen.y * 0.5 * zoom], None),
        };
        ctx.data_mut(|d| d.insert_temp(window_opened_id(), opened));
        opened
    });
    let builder = egui::ViewportBuilder::default()
        .with_app_id(crate::APP_ID)
        .with_title(OVERLAY_OWN_WINDOW_TITLE)
        .with_inner_size(egui::vec2(size[0], size[1]) / zoom)
        .with_min_inner_size([160.0, 90.0])
        .with_active(false);
    match pos {
        Some([x, y]) => builder.with_position(egui::pos2(x, y) / zoom),
        None => builder,
    }
}

/// Where the overlay window is and how big inside, `[x, y, w, h]` in 100% points - `None`
/// while minimized, mid-drag, or not yet sized.
fn own_window_rect(vctx: &egui::Context) -> Option<[f32; 4]> {
    let (outer, inner, minimized) = vctx.input(|i| {
        let v = i.viewport();
        (v.outer_rect, v.inner_rect, v.minimized)
    });
    if minimized == Some(true) || vctx.input(|i| i.pointer.any_down()) {
        return None;
    }
    let zoom = vctx.zoom_factor();
    let (outer, inner) = (outer?, inner?);
    let r = [outer.min.x * zoom, outer.min.y * zoom, inner.width() * zoom, inner.height() * zoom];
    (r[2] >= 100.0 && r[3] >= 60.0).then_some(r)
}

/// Whether the info overlay shows in a window of its own (as last shown).
pub fn overlay_in_window(ctx: &egui::Context) -> bool {
    ctx.data(|d| d.get_temp::<bool>(in_window_id())).unwrap_or(false)
}


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
///
/// The pinned main window covers the dialog the same way, so this also drops
/// that pin, for good rather than for the dialog's span ([`crate::main_pin`]).
#[cfg(windows)]
pub fn with_overlay_not_topmost<R>(f: impl FnOnce() -> R) -> R {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        FindWindowW, SetWindowPos, HWND_NOTOPMOST, HWND_TOPMOST,
        SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE,
    };
    crate::main_pin::release();
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
    crate::main_pin::release();
    f()
}

/// OS cursor position in an overlay's logical points: the desktop's pixels, less
/// where the overlay sits on the desktop, over its scale. (It took the overlay to
/// sit at the desktop's corner, so on any other monitor every hit-test - an edit
/// mode's handles and toolbar - landed on the primary's.) Read from the OS (not
/// egui) because a click-through window receives no pointer events — this is what
/// lets edit mode decide, each frame, whether the cursor sits over an interactive
/// region (toolbar / pinned item) and flip passthrough accordingly.
#[cfg(windows)]
pub(crate) fn os_cursor_in_points(vctx: &egui::Context) -> Option<egui::Pos2> {
    use windows_sys::Win32::Foundation::POINT;
    use windows_sys::Win32::UI::WindowsAndMessaging::GetCursorPos;
    let mut p = POINT { x: 0, y: 0 };
    // SAFETY: GetCursorPos writes into our POINT; returns 0 on failure.
    if unsafe { GetCursorPos(&mut p) } == 0 {
        return None;
    }
    let ppp = vctx.pixels_per_point().max(0.1);
    let origin = vctx.input(|i| i.viewport().inner_rect).map_or(egui::Pos2::ZERO, |r| r.min);
    Some(egui::pos2(p.x as f32 / ppp, p.y as f32 / ppp) - origin.to_vec2())
}

#[cfg(not(any(windows, target_os = "linux")))]
pub(crate) fn os_cursor_in_points(_vctx: &egui::Context) -> Option<egui::Pos2> {
    None
}

/// The cursor over an overlay, in its points, for hover hit-tests. Windows
/// reads the OS cursor (see above). Linux has no global cursor, so its
/// overlays take the pointer only inside an input region
/// ([`set_input_rects`]) and egui's own pointer is the answer: `None` while
/// the cursor is off every region rect.
pub(crate) fn overlay_cursor(vctx: &egui::Context) -> Option<egui::Pos2> {
    #[cfg(target_os = "linux")]
    return vctx.input(|i| i.pointer.hover_pos());
    #[cfg(not(target_os = "linux"))]
    os_cursor_in_points(vctx)
}

/// Where an overlay takes the pointer on Linux, in its points: everywhere for
/// `None`, nowhere (click-through) for no rects. Windows flips click-through
/// whenever the OS cursor crosses these same rects; Wayland has no global
/// cursor to watch, so the compositor gets them as the window's input region.
/// An empty region also takes back the keyboard focus a `Focus` gave.
#[cfg(target_os = "linux")]
pub(crate) fn set_input_rects(vctx: &egui::Context, rects: Option<Vec<egui::Rect>>) {
    egui_winit::set_input_region(vctx, vctx.viewport_id(), rects);
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
        egui::Button::selectable(on, egui::RichText::new("▣").size(crate::settings::ui_size(14.0))),
    );
    let in_window = overlay_in_window(ui.ctx());
    let hover = match (on, in_window) {
        (true, false) => "Overlay: ON — click to hide.\nA transparent, click-through layer over the whole screen.\nWorks over borderless/windowed games (not exclusive fullscreen).",
        (false, false) => "Overlay: OFF — click to show the info overlay.\nA transparent, click-through layer over the whole screen.\nWorks over borderless/windowed games (not exclusive fullscreen).",
        (true, true) => "Overlay window: OPEN — click to close it.\nThe info overlay in a window of its own, for a stream's capture to take.\nSettings → Pin & overlays.",
        (false, true) => "Overlay window: CLOSED — click to open the info overlay's window.\nThe info overlay in a window of its own, for a stream's capture to take.\nSettings → Pin & overlays.",
    };
    if resp.on_hover_text(hover).clicked() {
        set_overlay_visible(ui.ctx(), !on);
    }

    if on {
        let editing = overlay_edit(ui.ctx());
        let resp = ui.add_sized(
            btn_size,
            egui::Button::selectable(editing, egui::RichText::new("✏").size(crate::settings::ui_size(13.0))),
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
        if !in_window && eframe::egui_wgpu::winit::TRANSPARENCY_UNSUPPORTED
            .load(std::sync::atomic::Ordering::Relaxed)
        {
            ui.label(egui::RichText::new("⚠").size(crate::settings::ui_size(14.0)).color(egui::Color32::from_rgb(240, 180, 60)))
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
    // Over the screen, or in a window of its own (Settings). Each has its own viewport, so
    // a switch closes the one and opens the other afresh.
    let window = app.overlay_window();
    let visible = overlay_visible(ctx);
    let which = if window.is_some() { "info_window" } else { "info" };
    crate::monitors::overlay_closed(ctx, if window.is_some() { "info" } else { "info_window" });
    ctx.data_mut(|d| {
        d.insert_temp(in_window_id(), window.is_some());
        if window.is_none() || !visible {
            d.remove::<WindowOpened>(window_opened_id());
            d.remove::<WindowFit>(window_fit_id());
            d.remove::<bool>(window_actual_size_id());
        }
        // A screen overlay opens click-through (its builder's), whatever was applied to
        // the last one.
        if window.is_some() || !visible {
            d.remove::<bool>(passthrough_applied_id());
        }
    });
    if !visible {
        crate::monitors::overlay_closed(ctx, which);
        return;
    }
    let frames_open = crate::monitors::overlay_frames_open(ctx, which);

    // Cover the monitor picked in the header (`crate::monitors`), the primary
    // until one is; the viewport puts itself exactly there once open.
    let (overlay_pos, monitor_size) = crate::monitors::overlay_geometry(ctx);
    // The screen the layout is laid out for: the window shows that monitor's, in its own
    // points, scaled to fit.
    let screen = match window {
        Some(_) => crate::monitors::overlay_screen_points(ctx.zoom_factor()).unwrap_or(monitor_size),
        None => monitor_size,
    };

    let edit = overlay_edit(ctx);
    // Live repaint cadence — the user's `overlay_fps` setting, NOT the low
    // `bg_repaint_hz` cadence (which would look awful over a game).
    let frame_interval = Duration::from_secs_f64(1.0 / app.overlay_fps() as f64);
    // Pick mode is only meaningful while editing (it's entered from the edit
    // toolbar); anything else left it stale — clear it. A pick armed by the
    // CONFIG overlay shares the same armed state but is destined elsewhere, so
    // the info overlay ignores it here (never cancels or consumes it).
    let mut pick = crate::canvas::viewer::overlay_pick_active(ctx)
        && !crate::canvas::viewer::overlay_pick_dest_config(ctx);
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
                (screen.x - init_size[0]) * 0.5 + cascade,
                (screen.y - init_size[1]) * 0.5 + cascade,
            ];
            overlay_layout.items.push(crate::canvas::node::LayoutItem::Module(
                crate::canvas::node::ExposedModule {
                    tab: overlay_layout.new_item_tab(),
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
                    keep_aspect: false,
                    anchor: Default::default(),
                    pin_style: Some(2),
                    zoom: None,
                    hidden_sections: Vec::new(),
                    sections_open: false,
                    graph_labels: None,
                    text_max: None,
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

    let mut exit_edit = false;

    if let Some(w) = window {
        let mut close = false;
        let mut rect = None;
        let builder = own_window_builder(ctx, w, screen);
        let screen_px = crate::monitors::overlay_monitor().map(|m| egui::vec2(m.size[0] as f32, m.size[1] as f32));
        let viewport_id = egui::ViewportId::from_hash_of("fxi_overlay_window");
        crate::monitors::with_settle_passes(ctx, which, frames_open, || ctx.show_viewport_immediate(viewport_id, builder, |vctx, _class| {
            // Never moved into place, so every frame shows; the first is laid out until its
            // pinned rows settle.
            let _ = crate::monitors::settle_overlay_frame(vctx, which, frames_open, false);
            close = vctx.input(|i| i.viewport().close_requested());
            rect = own_window_rect(vctx);
            if edit && !pick && vctx.input(|i| i.key_pressed(egui::Key::Escape)) {
                exit_edit = true;
            }
            show_window_contents(
                vctx, w.bg, screen, screen_px, edit, pick, tab_snarl, overlay_layout, live_signals, &panic_shortcut, &mut exit_edit,
            );
        }));
        if close {
            set_overlay_visible(ctx, false);
        }
        if let Some(r) = rect {
            app.keep_overlay_window_rect(r);
        }
        if exit_edit {
            set_overlay_edit(ctx, false);
        }
        ctx.request_repaint_after(frame_interval);
        return;
    }

    let viewport_id = egui::ViewportId::from_hash_of("fxi_overlay");
    let builder = egui::ViewportBuilder::default()
        .with_app_id(crate::APP_ID)
        .with_title(OVERLAY_WINDOW_TITLE)
        .with_decorations(false)
        .with_transparent(true)
        .with_always_on_top()
        .with_taskbar(false)
        .with_mouse_passthrough(true)
        .with_active(false)
        .with_resizable(false)
        .with_has_shadow(false)
        .with_position(overlay_pos)
        .with_inner_size(monitor_size);

    crate::monitors::with_settle_passes(ctx, "info", frames_open, || ctx.show_viewport_immediate(viewport_id, builder, |vctx, _class| {
        // Self-correct geometry from INSIDE the viewport: the builder size was
        // computed from the parent's monitor before the child window existed
        // (wrong DPI/monitor possible). The child's own input has the truth.
        let moved = crate::monitors::keep_overlay_on_monitor(vctx);
        let hidden = crate::monitors::settle_overlay_frame(vctx, "info", frames_open, moved);

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
        let interactive_rects = || -> Vec<egui::Rect> {
            let toolbar = vctx.data(|d| d.get_temp::<egui::Rect>(toolbar_rect_id()));
            let items = overlay_layout.items.iter().map(|it| {
                let (p, s) = it.bbox();
                egui::Rect::from_min_size(
                    egui::pos2(p[0], p[1]),
                    egui::vec2(s[0].max(8.0), s[1].max(8.0)),
                )
                .expand(HIT_MARGIN)
            });
            toolbar.map(|r| r.expand(4.0)).into_iter().chain(items).collect()
        };
        // A right-click context menu (or a combo popup) extends BEYOND the item
        // box it was opened from, so hit-testing only the item rect makes the
        // menu click-through the instant the cursor leaves the item — you could
        // only reach "Unpin" on a pin big enough to contain the whole menu.
        // While any popup/menu is open, keep the whole overlay interactive so
        // its entries are reachable. (The popup can only have opened while the
        // cursor was over an interactive region, so this never latches on empty
        // space. Tooltips use a separate order and don't count.) Likewise keep
        // it interactive through an active drag even if the cursor briefly
        // leaves the item, so a move/resize can't be cut off mid-gesture.
        let whole = || {
            egui::Popup::is_any_open(vctx)
                || vctx.input(|i| i.pointer.any_down())
                || vctx.is_using_pointer()
        };
        #[cfg(not(target_os = "linux"))]
        let interactive = edit && !pick && (whole() || match os_cursor_in_points(vctx) {
            // Can't read the cursor → stay interactive (old whole-window
            // behavior); never worse than before.
            None => true,
            Some(c) => interactive_rects().iter().any(|r| r.contains(c)),
        });
        // Linux: the same rects become the window's input region.
        #[cfg(target_os = "linux")]
        let interactive = {
            let rects = if !edit || pick {
                Some(Vec::new())
            } else if whole() {
                None
            } else {
                Some(interactive_rects())
            };
            set_input_rects(vctx, rects);
            edit && !pick
        };

        let pt_id = passthrough_applied_id();
        let want_passthrough = if edit && !pick { !interactive } else { true };
        let applied: Option<bool> = vctx.data(|d| d.get_temp(pt_id));
        if applied != Some(want_passthrough) {
            #[cfg(not(target_os = "linux"))]
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
                // Still being moved onto its monitor: nothing laid out for the wrong place,
                // and an edited layout's items not re-anchored to it.
                if hidden {
                    return;
                }
                let rect = ui.max_rect();

                if pick {
                    // Pick state: the overlay collapses to a glowing border
                    // frame (pin-mode indicator) + a hint chip. Items stay
                    // hidden so the FlexInput window behind is unobstructed.
                    paint_pick_frame(ui, rect);
                    return;
                }

                overlay_contents(ui, rect, tab_snarl, overlay_layout, edit, live_signals, &panic_shortcut, true);

                if edit {
                    let below = crate::canvas::viewer::tab_row_h(overlay_layout.layout_tabs.len());
                    overlay_edit_toolbar(ui, below, tab_snarl, overlay_layout, &mut exit_edit);
                }
            });
    }));

    if exit_edit {
        set_overlay_edit(ctx, false);
    }

    // Live signal glow / animations need a real cadence; pace the PARENT
    // context (immediate viewports render with the parent; egui keeps the
    // earliest requested deadline, so the bg throttle can't override this).
    ctx.request_repaint_after(frame_interval);
}

/// The overlay window's inside: its background, and on it the screen's layout (`screen`, its
/// points; `screen_px` its pixels, where known) scaled to fit - or, picking, the pick frame -
/// with the edit toolbar over it.
#[allow(clippy::too_many_arguments)]
fn show_window_contents(
    vctx: &egui::Context,
    bg: egui::Color32,
    screen: egui::Vec2,
    screen_px: Option<egui::Vec2>,
    edit: bool,
    pick: bool,
    tab_snarl: &mut egui_snarl::Snarl<crate::canvas::NodeData>,
    overlay_layout: &mut crate::canvas::OverlayLayout,
    live_signals: &std::collections::HashMap<(String, String), flexinput_core::Signal>,
    panic_shortcut: &crate::app::PanicShortcut,
    exit_edit: &mut bool,
) {
    egui::CentralPanel::default()
        .frame(egui::Frame::NONE.fill(bg))
        .show(vctx, |ui| {
            let win = ui.max_rect();
            let to_window = fit_screen(win, screen);
            publish_window_fit(vctx, screen, screen_px, to_window.scaling);
            if pick {
                paint_pick_frame(ui, win);
                return;
            }
            // The screen's layout on a layer of its own, scaled onto the window - its input
            // too. An area rather than a sublayer, so a pin's own layer nests one deep, as
            // over the screen.
            let full = egui::Rect::from_min_size(egui::Pos2::ZERO, screen);
            let layer = egui::LayerId::new(egui::Order::Middle, egui::Id::new("fxi_overlay_window_body"));
            vctx.set_transform_layer(layer, to_window);
            egui::Area::new(layer.id)
                .order(layer.order)
                .fixed_pos(egui::Pos2::ZERO)
                .constrain(false)
                .movable(false)
                .interactable(true)
                .show(vctx, |ui| {
                    ui.expand_to_include_rect(full);
                    let mut body = ui.new_child(egui::UiBuilder::new().max_rect(full));
                    body.set_clip_rect(full);
                    overlay_contents(&mut body, full, tab_snarl, overlay_layout, edit, live_signals, panic_shortcut, false);
                });
            if edit {
                let row = crate::canvas::viewer::tab_row_h(overlay_layout.layout_tabs.len());
                let below = (to_window * egui::pos2(0.0, row)).y - win.min.y;
                overlay_edit_toolbar(ui, below, tab_snarl, overlay_layout, exit_edit);
            }
        });
}

/// What the overlay shows on its screen `rect`: the layout, live or edited - and edited while
/// empty, how to fill it. `dim` shades the screen while editing, so the mode reads apart
/// (not where the background is the colour a capture keys out).
#[allow(clippy::too_many_arguments)]
fn overlay_contents(
    ui: &mut egui::Ui,
    rect: egui::Rect,
    tab_snarl: &mut egui_snarl::Snarl<crate::canvas::NodeData>,
    overlay_layout: &mut crate::canvas::OverlayLayout,
    edit: bool,
    live_signals: &std::collections::HashMap<(String, String), flexinput_core::Signal>,
    panic_shortcut: &crate::app::PanicShortcut,
    dim: bool,
) {
    if edit {
        if dim {
            ui.painter().rect_filled(rect, 0.0, egui::Color32::from_black_alpha(48));
        }
        if overlay_layout.items.is_empty() {
            ui.painter().text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                "Overlay is empty — right-click for decorations,\nor use “Add element” to pin module UI here.",
                crate::settings::ui_font(15.0),
                egui::Color32::from_rgba_unmultiplied(220, 235, 255, 220),
            );
        }
    }
    crate::canvas::overlay_body::show_overlay_body(
        ui, rect, tab_snarl, overlay_layout, edit, live_signals, panic_shortcut,
    );
}

/// Tell Settings how the overlay window shows the screen (`scale`: its points onto the
/// window's), and take a request to show it at 100%: the window's inside resized to the
/// screen's pixels.
fn publish_window_fit(vctx: &egui::Context, screen: egui::Vec2, screen_px: Option<egui::Vec2>, scale: f32) {
    let ppp = vctx.pixels_per_point().max(0.1);
    let screen_px = screen_px.unwrap_or(screen * ppp);
    let shown = screen.x * scale * ppp / screen_px.x.max(1.0);
    vctx.data_mut(|d| d.insert_temp(window_fit_id(), WindowFit { shown, screen_px }));
    if vctx.data_mut(|d| d.remove_temp::<bool>(window_actual_size_id())).unwrap_or(false) {
        // The letterbox goes with it: the window takes the screen's shape.
        vctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(screen_px / ppp));
    }
}

/// Pick-state visuals: a pulsing amber border frame hugging the screen edge
/// (the "pin mode" indicator) plus a hint chip below the top edge. Painted on
/// a click-through window, so nothing here can intercept the pick click.
pub(crate) fn paint_pick_frame(ui: &mut egui::Ui, rect: egui::Rect) {
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
    let font = crate::settings::ui_font(14.0);
    let galley = p.layout_no_wrap(hint.to_string(), font, egui::Color32::from_rgb(255, 225, 170));
    let pad = egui::vec2(14.0, 8.0);
    let chip = egui::Rect::from_center_size(
        egui::pos2(rect.center().x, rect.top() + 46.0),
        galley.size() + pad * 2.0,
    );
    p.rect_filled(chip, 8.0, egui::Color32::from_rgba_unmultiplied(30, 22, 8, 230));
    p.rect_stroke(chip, 8.0, egui::Stroke::new(1.0_f32, base), egui::StrokeKind::Inside);
    p.galley(chip.min + pad, galley, egui::Color32::WHITE);

    // The pulse needs frames; the parent-paced overlay interval covers it.
}

/// Floating toolbar shown top-center in edit mode: Done, Add element, and the
/// shared layout tools (snap grid, decoration adders, inspector strip). `below`: how far
/// down the window the overlay's row of tabs ends (0 without one), the toolbar under it.
fn overlay_edit_toolbar(
    ui: &mut egui::Ui,
    below: f32,
    tab_snarl: &mut egui_snarl::Snarl<crate::canvas::NodeData>,
    overlay_layout: &mut crate::canvas::OverlayLayout,
    exit_edit: &mut bool,
) {
    let area = egui::Area::new(egui::Id::new("fxi_overlay_toolbar"))
        .order(egui::Order::Foreground)
        .anchor(egui::Align2::CENTER_TOP, egui::vec2(0.0, 12.0 + below))
        .interactable(true);
    let area_resp = area.show(ui.ctx(), |ui| {
        let bg = ui.visuals().window_fill();
        egui::Frame::default()
            .fill(egui::Color32::from_rgba_unmultiplied(bg.r(), bg.g(), bg.b(), 240))
            .stroke(egui::Stroke::new(1.0_f32, ui.visuals().widgets.noninteractive.bg_stroke.color))
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
                        if crate::canvas::viewer::layout_tool_word(ui, "Done", "Exit overlay editing (Esc)", true) {
                            *exit_edit = true;
                        }
                        ui.separator();
                        if crate::canvas::viewer::layout_tool_word(ui, "+ Add element",
                            "Pick a module UI element to pin: the overlay collapses to a\nglowing border and pinnable elements light up amber in the\nFlexInput window (and first-level sub-patch editors). Esc cancels.", false)
                        {
                            crate::canvas::viewer::set_overlay_pick_active(ui.ctx(), true);
                            crate::canvas::viewer::set_overlay_pick_dest_config(ui.ctx(), false);
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
                    crate::canvas::viewer::overlay_inspector_strip(ui, &mut state, sel_module);
                });
            });
    });
    // Record the toolbar's rect so next frame's passthrough hit-test keeps the
    // window interactive while the cursor is over it (its size isn't known
    // until laid out here).
    ui.ctx().data_mut(|d| d.insert_temp(toolbar_rect_id(), area_resp.response.rect));
}

#[cfg(test)]
mod toolbar_shots {
    use super::*;

    /// Pictures of the overlay's edit toolbar with a gradient rectangle selected - the tools
    /// and the compact Inspector under them, in a line, in tabs and folded:
    /// `cargo test -p flexinput-ui --lib shoot_overlay_toolbar -- --ignored`, PNGs
    /// `target/module_shots/overlay_toolbar{,_tabs,_folded}.png`.
    #[test]
    #[ignore]
    fn shoot_overlay_toolbar() {
        use crate::canvas::node::{Gradient, GradientKind, LayoutDecoration, LayoutItem};
        let ctx = egui::Context::default();
        crate::app::fonts::setup_fonts(&ctx);
        let mut snarl: egui_snarl::Snarl<crate::canvas::NodeData> = egui_snarl::Snarl::new();
        let mut layout = crate::canvas::OverlayLayout::default();
        layout.items.push(LayoutItem::Deco(LayoutDecoration::Rect {
            tab: 0,
            pos: [200.0, 300.0],
            size: [300.0, 160.0],
            fill: [0; 4],
            fill_gradient: Some(Gradient::new(GradientKind::Linear, [40, 120, 255, 255], [255, 80, 160, 255])),
            stroke: [0; 4],
            stroke_px: 0.0,
            corner_radius: 0.0,
            anchor: Default::default(),
        }));
        layout.selected_item = Some(0);
        layout.selected_items = vec![0];
        let mut shot = crate::canvas::shot::Shot::default();
        // In a line where the screen holds it, in tabs where it doesn't, and folded away.
        for (width, folded, name) in [(1920.0, false, "overlay_toolbar"), (1280.0, false, "overlay_toolbar_tabs"), (1280.0, true, "overlay_toolbar_folded")] {
            ctx.data_mut(|d| d.insert_temp(egui::Id::new("layout_inspector_folded"), folded));
            let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(width, 120.0));
            let mut shapes = Vec::new();
            for t in 0..10 {
                let raw = egui::RawInput { screen_rect: Some(screen), time: Some(t as f64 / 30.0), ..Default::default() };
                let out = ctx.run(raw, |ctx| {
                    egui::CentralPanel::default().show(ctx, |ui| {
                        let mut exit = false;
                        overlay_edit_toolbar(ui, 0.0, &mut snarl, &mut layout, &mut exit);
                    });
                });
                shot.absorb(&out.textures_delta);
                shapes = out.shapes;
            }
            let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(format!("../../target/module_shots/{name}.png"));
            shot.save(&ctx, &shapes, screen, 1.0, &path).expect("the picture saves");
            println!("{}", path.display());
        }
    }

    /// A picture of an overlay with three tabs being edited: their row across the top, the
    /// toolbar under it, the layout beneath - a rectangle anchored to another, selected, so
    /// its anchor shows and the other's doesn't: `cargo test -p flexinput-ui --lib
    /// shoot_overlay_tabs -- --ignored`, PNG `target/module_shots/overlay_tabs.png`.
    #[test]
    #[ignore]
    fn shoot_overlay_tabs() {
        use crate::canvas::node::{Anchor, LayoutDecoration, LayoutItem};
        let ctx = egui::Context::default();
        crate::app::fonts::setup_fonts(&ctx);
        let mut snarl: egui_snarl::Snarl<crate::canvas::NodeData> = egui_snarl::Snarl::new();
        let mut layout = crate::canvas::OverlayLayout::default();
        let rect = |pos: [f32; 2], fill: [u8; 4], anchor: Anchor| LayoutItem::Deco(LayoutDecoration::Rect {
            tab: 0,
            pos,
            size: [220.0, 120.0],
            fill,
            fill_gradient: None,
            stroke: [0; 4],
            stroke_px: 0.0,
            corner_radius: 6.0,
            anchor,
        });
        let target = Anchor { id: 1, ..Default::default() };
        let follower = Anchor { to: Some(1), ..Default::default() };
        layout.items.push(rect([160.0, 160.0], [40, 120, 255, 255], target));
        layout.items.push(rect([520.0, 220.0], [255, 80, 160, 255], follower));
        layout.items.push(rect([860.0, 160.0], [60, 200, 120, 255], Default::default()));
        layout.layout_tabs = vec!["Aim".into(), "Move".into(), "Menu".into()];
        layout.selected_item = Some(1);
        layout.selected_items = vec![1];
        let mut shot = crate::canvas::shot::Shot::default();
        let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1280.0, 420.0));
        let mut shapes = Vec::new();
        for t in 0..10 {
            let raw = egui::RawInput { screen_rect: Some(screen), time: Some(t as f64 / 30.0), ..Default::default() };
            let out = ctx.run(raw, |ctx| {
                egui::CentralPanel::default().frame(egui::Frame::NONE.fill(egui::Color32::from_rgb(30, 34, 40))).show(ctx, |ui| {
                    let panic = crate::app::PanicShortcut::default();
                    crate::canvas::overlay_body::show_overlay_body(
                        ui, screen, &mut snarl, &mut layout, true, &Default::default(), &panic,
                    );
                    let mut exit = false;
                    let below = crate::canvas::viewer::tab_row_h(layout.layout_tabs.len());
                    overlay_edit_toolbar(ui, below, &mut snarl, &mut layout, &mut exit);
                });
            });
            shot.absorb(&out.textures_delta);
            shapes = out.shapes;
        }
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/module_shots/overlay_tabs.png");
        shot.save(&ctx, &shapes, screen, 1.0, &path).expect("the picture saves");
        println!("{}", path.display());
    }
}

#[cfg(test)]
mod first_frame_tests {
    /// Where a row pinned on an overlay (3DOF to 2D's steering options, in their first look)
    /// draws its "re-center" on the overlay's first frame and once settled - its frames
    /// settled as an overlay's are (`monitors::settle_overlay_frame`) or not.
    fn first_and_settled(as_overlay: bool) -> (egui::Rect, egui::Rect) {
        use crate::canvas::node::{LayoutItem, NodeData};
        let ctx = egui::Context::default();
        crate::app::fonts::setup_fonts(&ctx);
        let mut snarl: egui_snarl::Snarl<NodeData> = egui_snarl::Snarl::new();
        let desc = flexinput_modules::all_modules().into_iter().map(|r| r.descriptor)
            .find(|d| d.id == "processing.gyro_3dof").expect("3DOF");
        let node = snarl.insert_node(egui::pos2(0.0, 0.0), NodeData::from(&desc));
        let mut layout = crate::canvas::OverlayLayout::default();
        layout.items.push(LayoutItem::Module(serde_json::from_value(serde_json::json!({
            "inner_node_id": node.0, "element_id": "steering_opts", "pos": [40.0, 40.0], "size": [520.0, 70.0],
            "pin_style": 1,
        })).expect("a pin")));
        let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1280.0, 720.0));
        let panic = crate::app::PanicShortcut::default();
        let mut at = Vec::new();
        for t in 0..7u32 {
            let raw = egui::RawInput { screen_rect: Some(screen), time: Some(t as f64 / 30.0), ..Default::default() };
            let run = || ctx.run(raw, |ctx| {
                if as_overlay {
                    crate::monitors::settle_overlay_frame(ctx, "first_frame_test", t, false);
                }
                egui::CentralPanel::default().frame(egui::Frame::NONE).show(ctx, |ui| {
                    crate::canvas::overlay_body::show_overlay_body(
                        ui, screen, &mut snarl, &mut layout, false, &Default::default(), &panic,
                    );
                });
            });
            let out = if as_overlay { crate::monitors::with_settle_passes(&ctx, "first_frame_test", t, run) } else { run() };
            fn walk(s: &egui::Shape, out: &mut Option<egui::Rect>) {
                match s {
                    egui::Shape::Text(t) if t.galley.text() == "re-center" => *out = Some(t.galley.rect.translate(t.pos.to_vec2())),
                    egui::Shape::Vec(v) => v.iter().for_each(|s| walk(s, out)),
                    _ => {}
                }
            }
            let mut found = None;
            out.shapes.iter().for_each(|c| walk(&c.shape, &mut found));
            at.push(found.expect("the row's words"));
        }
        (at[0], at[6])
    }

    // An overlay's window is made afresh each time it opens, and a pinned row in its first look
    // scales to the size it measured the frame before - so on the frame it opened it came out
    // at a guess, and jumped into place a frame later. That frame is laid out twice now, and
    // the one shown is where it stays.
    #[test]
    fn an_overlays_first_frame_has_its_widgets_in_place() {
        let (first, settled) = first_and_settled(false);
        assert!((first.min - settled.min).length() > 1.0 || (first.size() - settled.size()).length() > 1.0,
            "a first frame already in place shows nothing: {first:?} {settled:?}");
        let (first, settled) = first_and_settled(true);
        assert!((first.min - settled.min).length() < 0.5 && (first.size() - settled.size()).length() < 0.5,
            "the first frame's row at {first:?}, settled at {settled:?}");
    }
}

#[cfg(test)]
mod window_tests {
    use crate::canvas::node::{LayoutDecoration, LayoutItem};

    const BG: egui::Color32 = egui::Color32::from_rgb(0, 255, 0);
    const FILL: [u8; 4] = [40, 120, 255, 255];

    /// A 1920 × 1080 screen's layout holding one rectangle at (600, 500), 300 × 160.
    fn layout() -> crate::canvas::OverlayLayout {
        let mut layout = crate::canvas::OverlayLayout::default();
        layout.items.push(LayoutItem::Deco(LayoutDecoration::Rect {
            tab: 0,
            pos: [600.0, 500.0],
            size: [300.0, 160.0],
            fill: FILL,
            fill_gradient: None,
            stroke: [0; 4],
            stroke_px: 0.0,
            corner_radius: 0.0,
            anchor: Default::default(),
        }));
        layout
    }

    /// Run the overlay window's inside - 960 × 600, showing a 1920 × 1080 screen - over
    /// `frames`, each frame's events in turn; the last frame's shapes.
    fn run(
        ctx: &egui::Context,
        layout: &mut crate::canvas::OverlayLayout,
        edit: bool,
        frames: Vec<Vec<egui::Event>>,
    ) -> Vec<egui::epaint::ClippedShape> {
        let mut snarl: egui_snarl::Snarl<crate::canvas::NodeData> = egui_snarl::Snarl::new();
        let win = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(960.0, 600.0));
        let panic = crate::app::PanicShortcut::default();
        let mut shapes = Vec::new();
        for (t, events) in frames.into_iter().enumerate() {
            let raw = egui::RawInput { screen_rect: Some(win), time: Some(t as f64 / 30.0), events, ..Default::default() };
            let out = ctx.run(raw, |ctx| {
                let mut exit = false;
                super::show_window_contents(
                    ctx, BG, egui::vec2(1920.0, 1080.0), None, edit, false,
                    &mut snarl, layout, &Default::default(), &panic, &mut exit,
                );
            });
            shapes = out.shapes;
        }
        shapes
    }

    /// Where the rectangles filled as `pick` says lie together, in the window.
    fn filled(shapes: &[egui::epaint::ClippedShape], pick: impl Fn(egui::Color32) -> bool + Copy) -> Option<egui::Rect> {
        fn walk(s: &egui::Shape, pick: impl Fn(egui::Color32) -> bool + Copy, out: &mut Option<egui::Rect>) {
            match s {
                egui::Shape::Rect(r) if pick(r.fill) => *out = Some(out.map_or(r.rect, |o| o.union(r.rect))),
                egui::Shape::Vec(v) => v.iter().for_each(|s| walk(s, pick, out)),
                _ => {}
            }
        }
        let mut out = None;
        shapes.iter().for_each(|c| walk(&c.shape, pick, &mut out));
        out
    }

    fn close(a: egui::Rect, b: egui::Rect) -> bool {
        (a.min - b.min).length() < 1.0 && (a.max - b.max).length() < 1.0
    }

    fn press(at: egui::Pos2, pressed: bool) -> egui::Event {
        egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed, modifiers: Default::default() }
    }

    // A screen as large as fits, its shape kept, centred: the rest is the background.
    #[test]
    fn a_screen_fits_a_window_whole_and_centred() {
        let win = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(960.0, 600.0));
        let t = super::fit_screen(win, egui::vec2(1920.0, 1080.0));
        assert!((t.scaling - 0.5).abs() < 1e-6, "{t:?}");
        assert_eq!(t * egui::pos2(0.0, 0.0), egui::pos2(0.0, 30.0));
        assert_eq!(t * egui::pos2(1920.0, 1080.0), egui::pos2(960.0, 570.0));
        let tall = super::fit_screen(egui::Rect::from_min_size(egui::pos2(10.0, 0.0), egui::vec2(400.0, 1000.0)), egui::vec2(1920.0, 1080.0));
        assert_eq!(tall * egui::pos2(0.0, 0.0), egui::pos2(10.0, 387.5), "a tall window letterboxed above and below");
    }

    // The window is filled with its colour, and the screen's layout lands in it where the
    // screen's scaled down puts it - a capture stretched over the game lines it up.
    #[test]
    fn the_overlay_window_shows_the_screens_layout_scaled_on_its_colour() {
        let ctx = egui::Context::default();
        crate::app::fonts::setup_fonts(&ctx);
        let mut layout = layout();
        let shapes = run(&ctx, &mut layout, false, vec![vec![]; 3]);
        let bg = filled(&shapes, |c| c == BG).expect("the background");
        assert!(close(bg, egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(960.0, 600.0))), "{bg:?}");
        // (Its colour as a live overlay draws it, a little see-through.)
        let item = filled(&shapes, |c| c != BG && c.a() > 0 && c.b() > c.r()).expect("the rectangle");
        let want = egui::Rect::from_min_size(egui::pos2(300.0, 280.0), egui::vec2(150.0, 80.0));
        assert!(close(item, want), "drawn at {item:?}, want {want:?}");
        let fit = super::overlay_window_fit(&ctx).expect("published");
        assert!((fit.shown - 0.5).abs() < 1e-3 && fit.screen_px == egui::vec2(1920.0, 1080.0), "{fit:?}");
    }

    // Edited in the window, the pointer is brought into the screen's layout: a click on the
    // rectangle where the window shows it selects it, and a marquee dragged in the window
    // catches it where the window shows it (not where the window's own points would be).
    #[test]
    fn the_overlay_window_is_edited_where_it_shows_the_layout() {
        let ctx = egui::Context::default();
        crate::app::fonts::setup_fonts(&ctx);
        let mut layout = layout();
        let on = egui::pos2(375.0, 320.0);
        run(&ctx, &mut layout, true, vec![
            vec![], vec![egui::Event::PointerMoved(on)], vec![press(on, true)], vec![press(on, false)], vec![],
        ]);
        assert_eq!(layout.selected_items, vec![0], "a click on it in the window");

        let ctx = egui::Context::default();
        crate::app::fonts::setup_fonts(&ctx);
        let mut layout = self::layout();
        let (from, to) = (egui::pos2(150.0, 180.0), egui::pos2(305.0, 285.0));
        let mid = from + (to - from) * 0.5;
        run(&ctx, &mut layout, true, vec![
            vec![], vec![egui::Event::PointerMoved(from)], vec![press(from, true)],
            vec![egui::Event::PointerMoved(mid)], vec![egui::Event::PointerMoved(to)],
            vec![press(to, false)], vec![],
        ]);
        assert_eq!(layout.selected_items, vec![0], "a marquee ending on it in the window");
    }
}
