//! Config overlay (M3) — a shortcut-summoned, transparent, always-on-top layer
//! for tweaking module parameters LIVE while a game runs. Unlike the info
//! overlay (display-only, click-through), the config overlay is INTERACTIVE over
//! its panel (so you can drag sliders) but click-through everywhere else (so the
//! game behind it stays reachable). Its defining behavior — suppress the inputs
//! used to navigate it, pass through the input the tweaked parameter affects —
//! lands in later phases (M3.3/M3.4) on the `__src_block__` machinery.
//!
//! M3.1: the shell — a toggle-summoned viewport with a placeholder panel. The
//! curated tweak-pin flow + parameter controls arrive in M3.2.
//!
//! Same transparency machinery as the info + menu overlays (unique title +
//! transparent + skip-taskbar triggers the vendored NOREDIRECTIONBITMAP patch;
//! passthrough commands latched per state change).

use std::time::Duration;

use crate::app::FlexInputApp;

const CONFIG_OVERLAY_TITLE: &str = "FlexInput Config Overlay";

/// Ctx temp-data slot: is the config overlay currently summoned?
pub const CONFIG_OVERLAY_VISIBLE_KEY: &str = "fxi_config_overlay_visible";
/// Ctx temp-data slot: is the config overlay in edit (arrange tweak-pins) mode?
pub const CONFIG_OVERLAY_EDIT_KEY: &str = "fxi_config_overlay_edit";

fn visible_id() -> egui::Id {
    egui::Id::new(CONFIG_OVERLAY_VISIBLE_KEY)
}
fn edit_id() -> egui::Id {
    egui::Id::new(CONFIG_OVERLAY_EDIT_KEY)
}
/// Panel bounds published each frame so the passthrough hit-test (which reads the
/// OS cursor — a click-through window gets no pointer events) knows where the
/// interactive region is.
fn panel_rect_id() -> egui::Id {
    egui::Id::new("fxi_config_panel_rect")
}

pub fn config_overlay_visible(ctx: &egui::Context) -> bool {
    ctx.data(|d| d.get_temp::<bool>(visible_id())).unwrap_or(false)
}

pub fn set_config_overlay_visible(ctx: &egui::Context, on: bool) {
    ctx.data_mut(|d| d.insert_temp(visible_id(), on));
    // Leaving the overlay drops edit mode too (mirrors set_overlay_visible).
    if !on {
        ctx.data_mut(|d| d.remove_temp::<bool>(edit_id()));
    }
}

/// Show the config overlay viewport (call once per frame from
/// `FlexInputApp::update`, right after the menu overlay). No-op while hidden.
pub fn show_config_overlay(app: &mut FlexInputApp, ctx: &egui::Context) {
    if !config_overlay_visible(ctx) {
        return;
    }
    let frame_interval = Duration::from_secs_f64(1.0 / app.overlay_fps() as f64);
    let monitor_size = ctx
        .input(|i| i.viewport().monitor_size)
        .filter(|s| s.x > 1.0 && s.y > 1.0)
        .unwrap_or(egui::vec2(1920.0, 1080.0));

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

        // Passthrough: click-through EXCEPT while the OS cursor is over the panel
        // (or a drag / popup is in flight), so the game stays reachable and the
        // panel stays usable. Reads the panel rect published last frame.
        let interactive = if egui::Popup::is_any_open(vctx)
            || vctx.input(|i| i.pointer.any_down())
            || vctx.is_using_pointer()
        {
            true
        } else {
            match crate::overlay::os_cursor_in_points(vctx.pixels_per_point()) {
                None => true, // can't read cursor → stay interactive (never worse)
                Some(c) => vctx
                    .data(|d| d.get_temp::<egui::Rect>(panel_rect_id()))
                    .map(|r| r.expand(6.0).contains(c))
                    .unwrap_or(false),
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

        if vctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            close = true;
        }

        // Placeholder panel (M3.1). The curated tweak-pin controls replace this
        // body in M3.2. A centred Area whose Frame paints the panel; its rect is
        // published for the passthrough hit-test above.
        let area = egui::Area::new(egui::Id::new("fxi_config_panel"))
            .order(egui::Order::Foreground)
            .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
            .show(vctx, |ui| {
                egui::Frame::default()
                    .fill(egui::Color32::from_rgba_unmultiplied(20, 20, 26, 235))
                    .stroke(egui::Stroke::new(1.5, egui::Color32::from_rgb(120, 140, 200)))
                    .corner_radius(10.0)
                    .inner_margin(egui::Margin::same(16))
                    .show(ui, |ui| {
                        ui.set_max_width(360.0);
                        ui.label(
                            egui::RichText::new("⚙ Config Overlay")
                                .size(18.0)
                                .strong()
                                .color(egui::Color32::WHITE),
                        );
                        ui.add_space(6.0);
                        ui.label(
                            egui::RichText::new(
                                "Shell (M3.1). Parameter tweak controls arrive next. \
                                 Press Esc or the shortcut, or click Done, to dismiss.",
                            )
                            .size(12.0)
                            .color(egui::Color32::from_gray(180)),
                        );
                        ui.add_space(12.0);
                        if ui
                            .button(egui::RichText::new("✔ Done").strong())
                            .clicked()
                        {
                            close = true;
                        }
                    });
            });
        vctx.data_mut(|d| d.insert_temp(panel_rect_id(), area.response.rect));
    });

    if close {
        set_config_overlay_visible(ctx, false);
    }
    // Pace the parent context (immediate viewports render with the parent).
    ctx.request_repaint_after(frame_interval);
}
