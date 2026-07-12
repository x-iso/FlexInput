//! Info overlay — a transparent, click-through, always-on-top viewport that
//! sits over the whole screen (games included, as long as they run
//! borderless/windowed — exclusive fullscreen bypasses the compositor).
//!
//! SPIKE STAGE: this renders demo content only. It exists to validate the
//! windowing primitives on real hardware before the pin/edit machinery is
//! built on top:
//!   * per-viewport transparency (vendored egui-wgpu picks a PreMultiplied
//!     surface alpha mode when the backend supports it),
//!   * click-through via `with_mouse_passthrough` (winit `set_cursor_hittest`
//!     → `WS_EX_TRANSPARENT` on Windows),
//!   * always-on-top + no taskbar entry + no focus steal,
//!   * smooth repaint while the main window is unfocused or minimized.
//!
//! Transparency note: immediate viewports are always cleared with
//! `[0, 0, 0, 0]` by eframe (`wgpu_integration::render_immediate_viewport`
//! hardcodes it), so `FlexInputApp::clear_color` — which returns an opaque
//! color when see-through is off — does NOT paint over the overlay. That
//! hardcoded transparent clear is load-bearing for this feature.
//!
//! State lives in a ctx temp-data slot (same pattern as the see-through
//! eye toggle, `SEE_THROUGH_DATA_KEY`) so the title-bar button can flip it
//! without threading `&mut FlexInputApp` through the title-bar renderer.

use std::time::Duration;

/// Ctx temp-data slot holding the overlay's visible flag.
pub const OVERLAY_VISIBLE_KEY: &str = "fxi_overlay_visible";

/// Window title of the overlay viewport. Combined with `with_transparent` +
/// `with_taskbar(false)`, the vendored egui-winit gives this window
/// `WS_EX_NOREDIRECTIONBITMAP` at creation — without it the never-painted GDI
/// redirection surface composites as an opaque white sheet under the
/// DirectComposition swapchain on Win11 26H1 + AMD (see the FLEXINPUT PATCH
/// in vendor/egui-winit/src/lib.rs).
const OVERLAY_WINDOW_TITLE: &str = "FlexInput Overlay";

/// Repaint cadence while the overlay is visible. Hardcoded for the spike;
/// becomes the `overlay_fps` setting later (the low `bg_repaint_hz` cadence
/// would look awful animating on top of a game).
const SPIKE_FRAME_INTERVAL: Duration = Duration::from_millis(16);

fn visible_id() -> egui::Id {
    egui::Id::new(OVERLAY_VISIBLE_KEY)
}

pub fn overlay_visible(ctx: &egui::Context) -> bool {
    ctx.data(|d| d.get_temp::<bool>(visible_id())).unwrap_or(false)
}

pub fn set_overlay_visible(ctx: &egui::Context, on: bool) {
    ctx.data_mut(|d| d.insert_temp(visible_id(), on));
}

/// Title-bar toggle button. Sits next to the see-through eye; same
/// selectable-button look (see `render_eye_toggle` in app.rs).
pub fn render_overlay_toggle(ui: &mut egui::Ui, bar_h: f32) {
    let on = overlay_visible(ui.ctx());
    let label = egui::RichText::new("▣").size(14.0);
    let btn = egui::Button::selectable(on, label);
    let resp = ui.add_sized(egui::vec2(26.0, (bar_h - 6.0).max(18.0)), btn);
    let hover = if on {
        "Overlay: ON — click to hide.\nSpike build: demo content, fully click-through."
    } else {
        "Overlay: OFF — click to show the info overlay.\nSpike build: demo content, fully click-through."
    };
    let resp = resp.on_hover_text(hover);
    if resp.clicked() {
        set_overlay_visible(ui.ctx(), !on);
    }
}

/// Show the overlay viewport (call once per frame from `FlexInputApp::update`,
/// after the sub-patch editors). No-op while the overlay is hidden — eframe
/// destroys the OS window on the first frame the viewport isn't declared.
pub fn show_overlay(ctx: &egui::Context) {
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

    let (pos, size) = (egui::pos2(0.0, 0.0), monitor_size);
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
        .with_position(pos)
        .with_inner_size(size);

    ctx.show_viewport_immediate(viewport_id, builder, |vctx, _class| {
        // Self-correct geometry from INSIDE the viewport: the builder size was
        // computed from the parent's monitor before the child window existed
        // (wrong DPI/monitor possible). The child's own input has the real
        // numbers once the window is up.
        let (inner, child_monitor) = vctx.input(|i| {
            (i.viewport().inner_rect, i.viewport().monitor_size)
        });
        if let (Some(inner), Some(want)) = (inner, child_monitor) {
            if (inner.size() - want).length() > 1.0 {
                vctx.send_viewport_cmd(egui::ViewportCommand::OuterPosition(egui::pos2(0.0, 0.0)));
                vctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(want));
            }
        }

        egui::CentralPanel::default()
            .frame(egui::Frame::NONE)
            .show(vctx, |ui| draw_spike_demo(ui));
    });

    // Immediate viewports only render when the parent updates, so pace the
    // PARENT context. egui keeps the earliest requested deadline, so the
    // longer bg-throttle request later in `update` can't override this one.
    ctx.request_repaint_after(SPIKE_FRAME_INTERVAL);
}

/// Demo content exercising exactly what the spike must prove: full-monitor
/// coverage + DPI alignment (corner brackets), alpha blending (translucent
/// shapes), smooth motion (orbiting dot), and text readability over
/// arbitrary backgrounds (shadowed text).
fn draw_spike_demo(ui: &mut egui::Ui) {
    let rect = ui.max_rect();
    let painter = ui.painter();
    let t = ui.input(|i| i.time) as f32;

    // Corner brackets — verify the viewport really reaches all four monitor
    // corners and lines are crisp at the monitor's DPI scale.
    let accent = egui::Color32::from_rgba_unmultiplied(120, 200, 255, 200);
    let arm = 28.0;
    let inset = 6.0;
    let s = egui::Stroke::new(2.0, accent);
    for (corner, dx, dy) in [
        (rect.left_top(), 1.0, 1.0),
        (rect.right_top(), -1.0, 1.0),
        (rect.right_bottom(), -1.0, -1.0),
        (rect.left_bottom(), 1.0, -1.0),
    ] {
        let c = corner + egui::vec2(dx * inset, dy * inset);
        painter.line_segment([c, c + egui::vec2(dx * arm, 0.0)], s);
        painter.line_segment([c, c + egui::vec2(0.0, dy * arm)], s);
    }

    // Orbiting dot with a fading trail around the screen center — the
    // smoothness litmus test over a running game.
    let center = rect.center();
    let radius = rect.height() * 0.18;
    for k in 0..12 {
        let age = k as f32 * 0.05;
        let a = t * 1.6 - age;
        let p = center + egui::vec2(a.cos(), a.sin()) * radius;
        let alpha = (200.0 * (1.0 - k as f32 / 12.0)) as u8;
        painter.circle_filled(
            p,
            7.0 * (1.0 - k as f32 / 14.0),
            egui::Color32::from_rgba_unmultiplied(255, 180, 60, alpha),
        );
    }
    // Faint orbit ring: verifies thin translucent strokes blend correctly.
    painter.circle_stroke(
        center,
        radius,
        egui::Stroke::new(1.0, egui::Color32::from_rgba_unmultiplied(255, 255, 255, 40)),
    );

    // Title chip (top center) — shadowed text so it reads on any background.
    let title = "FlexInput overlay — spike (click-through)";
    let font = egui::FontId::proportional(16.0);
    let chip_pos = egui::pos2(center.x, rect.top() + 28.0);
    let galley_size = painter
        .layout_no_wrap(title.into(), font.clone(), egui::Color32::WHITE)
        .size();
    let chip_rect = egui::Rect::from_center_size(chip_pos, galley_size + egui::vec2(24.0, 12.0));
    painter.rect_filled(chip_rect, 8.0, egui::Color32::from_black_alpha(130));
    painter.text(
        chip_pos + egui::vec2(1.0, 1.0),
        egui::Align2::CENTER_CENTER,
        title,
        font.clone(),
        egui::Color32::from_black_alpha(180),
    );
    painter.text(
        chip_pos,
        egui::Align2::CENTER_CENTER,
        title,
        font,
        egui::Color32::WHITE,
    );

    // Stats chip (bottom right): frame delta, to eyeball the repaint cadence.
    let dt_ms = ui.input(|i| i.stable_dt) * 1000.0;
    let stats = format!("{dt_ms:.1} ms/frame");
    let font = egui::FontId::monospace(12.0);
    let pos = rect.right_bottom() + egui::vec2(-16.0, -16.0);
    let size = painter
        .layout_no_wrap(stats.clone(), font.clone(), egui::Color32::WHITE)
        .size();
    let chip = egui::Rect::from_min_size(pos - size - egui::vec2(16.0, 8.0), size + egui::vec2(16.0, 8.0));
    painter.rect_filled(chip, 6.0, egui::Color32::from_black_alpha(130));
    painter.text(
        chip.center(),
        egui::Align2::CENTER_CENTER,
        stats,
        font,
        egui::Color32::from_rgba_unmultiplied(180, 230, 255, 255),
    );
}
