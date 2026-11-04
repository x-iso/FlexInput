//! Phase tuner: a slider over the game for display-synced output's **Phase in
//! frame** (see `flexinput_virtual::display_clock`), summoned by its own
//! gamepad shortcut.
//!
//! Tuning it means watching motion while moving it, so the overlay takes only
//! the left stick, the D-pad and East (which closes it) from the pads: the game
//! keeps getting everything else, gyro and right stick among them, and the
//! overlay never takes focus or the mouse. Whether it disturbs what it tunes
//! depends on the setup — a window over a borderless game can make the
//! compositor take the game over, and VRR with it — so it shows the clock's
//! live status: if opening it turns VRR off, it says so.

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use eframe::egui::{self, Align2, Color32, FontId, Pos2, Rect, Stroke};
use flexinput_core::Signal;

use crate::gamepad_nav::{merge_nav_input, read_nav_input, NavInput};
use crate::settings::{self, AppSettings};

type LiveSignals = HashMap<(String, String), Signal>;

const TITLE: &str = "FlexInput Phase Overlay";

/// What the overlay reads, withheld from the game while it's up.
pub(crate) const NAV_PINS: &[&str] = &[
    "left_stick", "left_stick_x", "left_stick_y",
    "left_stick_up", "left_stick_down", "left_stick_left", "left_stick_right",
    "dpad", "dpad_x", "dpad_y", "dpad_up", "dpad_down", "dpad_left", "dpad_right",
    "btn_east",
];

/// A D-pad press moves the phase by a percent; held, it repeats.
const STEP: f32 = 0.01;
const REPEAT_AFTER_S: f64 = 0.4;
const REPEAT_EVERY_S: f64 = 1.0 / 15.0;
/// The stick moves it continuously, this much a second at full deflection.
const STICK_RATE: f32 = 0.5;
const STICK_DEADZONE: f32 = 0.2;

fn visible_id() -> egui::Id { egui::Id::new("fxi_phase_overlay_visible") }
fn nav_id() -> egui::Id { egui::Id::new("fxi_phase_overlay_nav") }
fn passthrough_id() -> egui::Id { egui::Id::new("fxi_phase_overlay_passthrough_applied") }

pub(crate) fn visible(ctx: &egui::Context) -> bool {
    ctx.data(|d| d.get_temp::<bool>(visible_id())).unwrap_or(false)
}

pub(crate) fn set_visible(ctx: &egui::Context, on: bool) {
    ctx.data_mut(|d| {
        d.insert_temp(visible_id(), on);
        // A fresh open starts from the pads as they are: the shortcut's own
        // D-pad press mustn't move the phase.
        d.remove_temp::<Nav>(nav_id());
    });
}

/// The pads' state between frames.
#[derive(Clone, Default)]
struct Nav {
    /// Per pad, the buttons held last frame (for rising edges).
    prev: HashMap<String, HashSet<String>>,
    /// The D-pad direction held (+1 up, -1 down) and when its next repeat is.
    held: Option<(f32, f64)>,
    /// When the last frame was, for the stick's rate.
    last_t: Option<f64>,
    /// Moved since it was last saved: saved when the input lets go.
    unsaved: bool,
}

/// The pads' nav input this frame, merged into one controller.
fn read_pads(live: &LiveSignals, nav: &mut Nav, first: bool) -> NavInput {
    let pads: HashSet<&str> = live.keys()
        .map(|(dev, _)| dev.as_str())
        .filter(|dev| crate::app::graph::is_physical_pad_id(dev))
        .collect();
    let mut merged: Option<NavInput> = None;
    let mut prev = HashMap::new();
    for dev in pads {
        let mut input = read_nav_input(live, dev, nav.prev.get(dev).unwrap_or(&HashSet::new()));
        if first {
            input.rising.clear();
        }
        prev.insert(dev.to_string(), input.pressed.clone());
        match merged.as_mut() {
            Some(m) => merge_nav_input(m, &input),
            None => merged = Some(input),
        }
    }
    nav.prev = prev;
    merged.unwrap_or(NavInput {
        pressed: HashSet::new(),
        rising: HashSet::new(),
        lstick: egui::Vec2::ZERO,
        rstick: egui::Vec2::ZERO,
        lt: 0.0,
        rt: 0.0,
        gyro_pitch: 0.0,
        gyro_yaw: 0.0,
    })
}

/// `phase` moved by this frame's input: a step per D-pad press (repeating
/// while held), continuously by the stick, clamped to the allowed range.
/// On the frame the overlay opens a held direction is the shortcut's own and
/// counts only once let go.
fn adjust(mut phase: f32, input: &NavInput, nav: &mut Nav, now: f64, dt: f32, first: bool) -> f32 {
    // A Home or Capture held is a shortcut in the making, not tuning.
    if input.pressed.contains("btn_guide") || input.pressed.contains("btn_capture") {
        return phase;
    }
    let dir = if input.pressed.contains("dpad_up") {
        1.0
    } else if input.pressed.contains("dpad_down") {
        -1.0
    } else {
        0.0
    };
    if dir == 0.0 {
        nav.held = None;
    } else if first {
        nav.held = Some((dir, f64::INFINITY));
    } else {
        let fresh = input.is_rising("dpad_up") || input.is_rising("dpad_down");
        match nav.held {
            Some((d, next)) if d == dir && !fresh => {
                if now >= next {
                    phase += dir * STEP;
                    nav.held = Some((dir, now + REPEAT_EVERY_S));
                }
            }
            _ => {
                phase += dir * STEP;
                nav.held = Some((dir, now + REPEAT_AFTER_S));
            }
        }
    }
    let y = input.lstick.y;
    if y.abs() > STICK_DEADZONE {
        let push = (y.abs() - STICK_DEADZONE) / (1.0 - STICK_DEADZONE);
        phase += y.signum() * push * push * STICK_RATE * dt;
    }
    phase.clamp(0.0, settings::OUTPUT_SYNC_PHASE_MAX)
}

/// Read the pads, move the phase, draw the slider. Once per frame from
/// `FlexInputApp::update`; no-op while the overlay is closed.
pub(crate) fn show(
    ctx: &egui::Context,
    settings: &mut AppSettings,
    settings_dirty: &mut bool,
    live: &LiveSignals,
    overlay_fps: u32,
) {
    if !visible(ctx) {
        // Viewport gone: a new one must get its passthrough again.
        ctx.data_mut(|d| d.remove_temp::<bool>(passthrough_id()));
        return;
    }
    let now = ctx.input(|i| i.time);
    let stored = ctx.data_mut(|d| d.remove_temp::<Nav>(nav_id()));
    let first = stored.is_none();
    let mut nav = stored.unwrap_or_default();
    let input = read_pads(live, &mut nav, first);
    let dt = nav.last_t.map_or(0.0, |t| (now - t).clamp(0.0, 0.1)) as f32;
    nav.last_t = Some(now);

    if input.is_rising("btn_east") {
        if nav.unsaved {
            *settings_dirty = true;
        }
        set_visible(ctx, false);
        ctx.data_mut(|d| d.remove_temp::<bool>(passthrough_id()));
        return;
    }

    let phase = adjust(settings.output_sync_phase, &input, &mut nav, now, dt, first);
    if phase != settings.output_sync_phase {
        settings.output_sync_phase = phase;
        nav.unsaved = true;
    }
    let adjusting = nav.held.is_some() || input.lstick.y.abs() > STICK_DEADZONE;
    if nav.unsaved && !adjusting {
        *settings_dirty = true;
        nav.unsaved = false;
    }
    ctx.data_mut(|d| d.insert_temp(nav_id(), nav));

    let monitor_size = ctx
        .input(|i| i.viewport().monitor_size)
        .filter(|m| m.x > 1.0 && m.y > 1.0)
        .unwrap_or(egui::vec2(1920.0, 1080.0));
    let viewport_id = egui::ViewportId::from_hash_of("fxi_phase_overlay");
    let builder = egui::ViewportBuilder::default()
        .with_app_id(crate::APP_ID)
        .with_title(TITLE)
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

    let status = sync_status(settings);
    let synced = settings.output_sync().is_some();
    let per_frame = settings::snap_output_sync_per_frame(settings.output_sync_per_frame);
    ctx.show_viewport_immediate(viewport_id, builder, |vctx, _class| {
        // Geometry self-correction from inside the viewport (see overlay.rs).
        let (inner_rect, child_monitor) = vctx.input(|i| (i.viewport().inner_rect, i.viewport().monitor_size));
        if let (Some(inner_rect), Some(want)) = (inner_rect, child_monitor) {
            if (inner_rect.size() - want).length() > 1.0 {
                vctx.send_viewport_cmd(egui::ViewportCommand::OuterPosition(egui::pos2(0.0, 0.0)));
                vctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(want));
            }
        }
        if vctx.data(|d| d.get_temp::<bool>(passthrough_id())).is_none() {
            vctx.send_viewport_cmd(egui::ViewportCommand::MousePassthrough(true));
            vctx.data_mut(|d| d.insert_temp(passthrough_id(), true));
        }
        egui::CentralPanel::default()
            .frame(egui::Frame::NONE)
            .show(vctx, |ui| paint(ui, ui.max_rect(), phase, per_frame, synced, &status));
    });

    // Immediate viewports render with the parent: pace it.
    ctx.request_repaint_after(Duration::from_secs_f64(1.0 / overlay_fps.max(1) as f64));
}

/// The display clock's live state, for this overlay and the Settings readout.
pub(crate) fn sync_status(settings: &AppSettings) -> String {
    use flexinput_virtual::display_clock;
    let per_frame = settings::snap_output_sync_per_frame(settings.output_sync_per_frame);
    let Some(st) = display_clock::status() else {
        return if settings.output_display_sync {
            "Starting…".to_string()
        } else {
            "Sync to the display is off (Settings → Performance).".to_string()
        };
    };
    if st.display.is_empty() {
        return format!("No display to follow: reporting at {} Hz.", display_clock::output_hz(per_frame));
    }
    if !st.live {
        return format!("{}: waiting for its frames, reporting at {} Hz meanwhile.",
            st.display, display_clock::output_hz(per_frame));
    }
    let rate = match (st.vrr, settings.output_sync_follow_vrr) {
        (false, _) => format!("{:.0} Hz", st.frame_hz),
        (true, true) => format!("variable refresh, frames at {:.0} Hz", st.frame_hz),
        (true, false) => format!("variable refresh, not followed: as if {} Hz", st.nominal_hz),
    };
    format!("Now: {}, {rate} · {per_frame} reports a frame", st.display)
}

/// The slider at the right edge: the phase as a share of the frame, a strip
/// showing where in the frame the reports land, the clock's status.
fn paint(ui: &egui::Ui, screen: Rect, phase: f32, per_frame: u32, synced: bool, status: &str) {
    let p = ui.painter();
    let accent = ui.visuals().selection.stroke.color;
    let text = Color32::from_gray(230);
    let dim = Color32::from_gray(160);

    let h = screen.height();
    let track_top = screen.top() + h * 0.25;
    let track_bottom = screen.top() + h * 0.70;
    let track_x = screen.right() - 70.0;
    let panel = Rect::from_min_max(
        Pos2::new(screen.right() - 360.0, track_top - 70.0),
        Pos2::new(screen.right() - 30.0, track_bottom + 150.0),
    );
    p.rect_filled(panel, 12.0, Color32::from_rgba_unmultiplied(14, 14, 18, 215));
    p.text(Pos2::new(panel.left() + 20.0, track_top - 45.0), Align2::LEFT_CENTER,
        "Phase in frame", FontId::proportional(18.0), text);

    // 0 at the bottom, the latest allowed at the top.
    let y_of = |v: f32| track_bottom - (v / settings::OUTPUT_SYNC_PHASE_MAX) * (track_bottom - track_top);
    p.line_segment([Pos2::new(track_x, track_top), Pos2::new(track_x, track_bottom)],
        Stroke::new(6.0_f32, Color32::from_gray(70)));
    let hy = y_of(phase);
    p.line_segment([Pos2::new(track_x, hy), Pos2::new(track_x, track_bottom)], Stroke::new(6.0_f32, accent));
    for pct in (0..=90).step_by(10) {
        let y = y_of(pct as f32 / 100.0);
        p.line_segment([Pos2::new(track_x + 10.0, y), Pos2::new(track_x + 18.0, y)], Stroke::new(1.5_f32, dim));
    }
    p.circle_filled(Pos2::new(track_x, hy), 11.0, accent);
    p.circle_stroke(Pos2::new(track_x, hy), 11.0, Stroke::new(2.0_f32, Color32::WHITE));
    p.text(Pos2::new(track_x - 24.0, hy), Align2::RIGHT_CENTER,
        format!("{:.0}%", phase * 100.0), FontId::proportional(28.0), text);

    // One frame, vblank to vblank, and where its reports land in it.
    let strip = Rect::from_min_size(Pos2::new(panel.left() + 20.0, track_bottom + 30.0),
        egui::vec2(panel.width() - 40.0, 14.0));
    p.rect_filled(strip, 4.0, Color32::from_gray(45));
    p.line_segment([strip.left_top(), strip.left_bottom()], Stroke::new(2.0_f32, Color32::WHITE));
    for k in 0..per_frame {
        let at = (phase + k as f32 / per_frame as f32).fract();
        let x = strip.left() + at * strip.width();
        p.line_segment([Pos2::new(x, strip.top() - 3.0), Pos2::new(x, strip.bottom() + 3.0)], Stroke::new(3.0_f32, accent));
    }
    p.text(Pos2::new(strip.left(), strip.bottom() + 4.0), Align2::LEFT_TOP,
        "refresh", FontId::proportional(12.0), dim);

    let status_color = if synced { dim } else { Color32::from_rgb(230, 170, 80) };
    let status = p.layout(status.to_string(), FontId::proportional(13.0), status_color, strip.width());
    p.galley(Pos2::new(strip.left(), strip.bottom() + 26.0), status, status_color);
    p.text(Pos2::new(panel.left() + 20.0, panel.bottom() - 16.0), Align2::LEFT_CENTER,
        "Left stick / D-pad ↕ adjust   ·   East: close", FontId::proportional(13.0), dim);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input(pressed: &[&str], rising: &[&str], stick_y: f32) -> NavInput {
        NavInput {
            pressed: pressed.iter().map(|s| s.to_string()).collect(),
            rising: rising.iter().map(|s| s.to_string()).collect(),
            lstick: egui::vec2(0.0, stick_y),
            rstick: egui::Vec2::ZERO,
            lt: 0.0,
            rt: 0.0,
            gyro_pitch: 0.0,
            gyro_yaw: 0.0,
        }
    }

    #[test]
    fn a_dpad_press_steps_once_then_repeats_while_held() {
        let mut nav = Nav::default();
        let p = adjust(0.5, &input(&["dpad_up"], &["dpad_up"], 0.0), &mut nav, 0.0, 0.016, false);
        assert!((p - 0.51).abs() < 1e-6);
        // Held, inside the repeat delay: no more.
        let p = adjust(p, &input(&["dpad_up"], &[], 0.0), &mut nav, 0.2, 0.016, false);
        assert!((p - 0.51).abs() < 1e-6);
        // Past it: repeats.
        let p = adjust(p, &input(&["dpad_up"], &[], 0.0), &mut nav, 0.45, 0.016, false);
        assert!((p - 0.52).abs() < 1e-6);
        // Down steps down; released, nothing.
        let p = adjust(p, &input(&["dpad_down"], &["dpad_down"], 0.0), &mut nav, 0.5, 0.016, false);
        assert!((p - 0.51).abs() < 1e-6);
        let p = adjust(p, &input(&[], &[], 0.0), &mut nav, 0.6, 0.016, false);
        assert!((p - 0.51).abs() < 1e-6);
        assert!(nav.held.is_none());
    }

    #[test]
    fn the_shortcuts_own_dpad_press_does_not_step() {
        let mut nav = Nav::default();
        let p = adjust(0.5, &input(&["dpad_up"], &[], 0.0), &mut nav, 0.0, 0.0, true);
        assert_eq!(p, 0.5);
        // Still held long after: still the shortcut's.
        let p = adjust(p, &input(&["dpad_up"], &[], 0.0), &mut nav, 5.0, 0.016, false);
        assert_eq!(p, 0.5);
        // Let go and pressed again: a step.
        let p = adjust(p, &input(&[], &[], 0.0), &mut nav, 5.1, 0.016, false);
        let p = adjust(p, &input(&["dpad_up"], &["dpad_up"], 0.0), &mut nav, 5.2, 0.016, false);
        assert!((p - 0.51).abs() < 1e-6);
    }

    #[test]
    fn the_stick_moves_it_by_deflection_and_time_within_range() {
        let mut nav = Nav::default();
        // Inside the deadzone: nothing.
        assert_eq!(adjust(0.5, &input(&[], &[], 0.15), &mut nav, 0.0, 0.1, false), 0.5);
        // Full up for a second, in steps: the full rate.
        let mut p = 0.2;
        for i in 0..10 {
            p = adjust(p, &input(&[], &[], 1.0), &mut nav, i as f64 * 0.1, 0.1, false);
        }
        assert!((p - (0.2 + STICK_RATE)).abs() < 1e-4);
        // Pinned at the ends.
        assert_eq!(adjust(0.94, &input(&[], &[], 1.0), &mut nav, 0.0, 0.1, false), settings::OUTPUT_SYNC_PHASE_MAX);
        assert_eq!(adjust(0.01, &input(&[], &[], -1.0), &mut nav, 0.0, 0.1, false), 0.0);
    }

    #[test]
    fn a_held_home_is_a_shortcut_not_tuning() {
        let mut nav = Nav::default();
        let p = adjust(0.5, &input(&["btn_guide", "dpad_up"], &["dpad_up"], 1.0), &mut nav, 0.0, 0.1, false);
        assert_eq!(p, 0.5);
    }
}
