//! The desktop's monitors, for choosing which one the overlays cover - and so which one a
//! calibration's screenshot is taken of.
//!
//! The choice lives here as well as in the settings, so every overlay (each builds its own
//! viewport from a free function) reads it without being handed the app; the header's
//! picker writes it and the app saves it ([`take_changed`]).
//!
//! Windows only for now. Elsewhere the list is empty: no picker, and the overlays keep to
//! the primary screen as before.

use std::sync::{Mutex, RwLock};
use std::time::{Duration, Instant};

/// One monitor, as Windows places it on the desktop.
#[derive(Clone, Debug, PartialEq)]
pub struct Monitor {
    /// Its device name, `DISPLAY2`: what the choice is kept as.
    pub id: String,
    /// Where it is on the desktop and how big, in physical pixels.
    pub origin: [i32; 2],
    pub size: [i32; 2],
    pub primary: bool,
}

impl Monitor {
    /// How the picker names it: its display number, as Windows numbers its devices.
    pub fn number(&self) -> String {
        self.id.trim_start_matches("DISPLAY").to_string()
    }

    pub fn describe(&self) -> String {
        format!(
            "Display {}: {}×{}{}",
            self.number(),
            self.size[0],
            self.size[1],
            if self.primary { ", primary" } else { "" }
        )
    }
}

/// The monitor chosen for the overlays, by id; `None` = the primary.
static CHOSEN: RwLock<Option<String>> = RwLock::new(None);
/// A choice made in the picker that the settings haven't taken yet.
static CHANGED: Mutex<Option<Option<String>>> = Mutex::new(None);
/// The list, read at most every second: the overlays ask every frame.
static CACHE: Mutex<Option<(Instant, Vec<Monitor>)>> = Mutex::new(None);

/// The monitors, left to right.
pub fn list() -> Vec<Monitor> {
    let mut cache = CACHE.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((at, list)) = cache.as_ref() {
        if at.elapsed() < Duration::from_secs(1) {
            return list.clone();
        }
    }
    let mut list = enumerate();
    list.sort_by_key(|m| (m.origin[0], m.origin[1]));
    *cache = Some((Instant::now(), list.clone()));
    list
}

/// Set the choice as the settings have it (at launch).
pub fn set_chosen(id: Option<String>) {
    *CHOSEN.write().unwrap_or_else(|e| e.into_inner()) = id;
}

pub fn chosen() -> Option<String> {
    CHOSEN.read().unwrap_or_else(|e| e.into_inner()).clone()
}

/// Choose from the picker: takes effect at once, and is handed to the settings.
pub fn choose(id: Option<String>) {
    set_chosen(id.clone());
    *CHANGED.lock().unwrap_or_else(|e| e.into_inner()) = Some(id);
}

/// A choice the picker made since last asked, for the settings to keep.
pub fn take_changed() -> Option<Option<String>> {
    CHANGED.lock().unwrap_or_else(|e| e.into_inner()).take()
}

/// The monitor the overlays cover: the chosen one while it is connected, else the primary.
/// `None` where monitors can't be listed.
pub fn overlay_monitor() -> Option<Monitor> {
    pick(&list(), chosen().as_deref()).cloned()
}

/// The chosen monitor in `list`, else the primary.
fn pick<'a>(list: &'a [Monitor], chosen: Option<&str>) -> Option<&'a Monitor> {
    list.iter().find(|m| chosen == Some(m.id.as_str())).or_else(|| list.iter().find(|m| m.primary))
}

/// Where a new overlay viewport goes and how big, in the main window's points: over the
/// overlay monitor, near enough - [`keep_overlay_on_monitor`] puts it exactly there once it
/// is open and knows its own scale. Without a list, the primary's corner and the main
/// window's monitor's size, as before.
pub fn overlay_geometry(ctx: &egui::Context) -> (egui::Pos2, egui::Vec2) {
    let fallback = ctx
        .input(|i| i.viewport().monitor_size)
        .filter(|s| s.x > 1.0 && s.y > 1.0)
        .unwrap_or(egui::vec2(1920.0, 1080.0));
    match overlay_monitor() {
        Some(m) => {
            let ppp = ctx.pixels_per_point().max(0.1);
            (
                egui::pos2(m.origin[0] as f32, m.origin[1] as f32) / ppp,
                egui::vec2(m.size[0] as f32, m.size[1] as f32) / ppp,
            )
        }
        None => (egui::Pos2::ZERO, fallback),
    }
}

/// Keep an overlay viewport covering the overlay monitor: call from inside it, every
/// frame. It is moved there in that monitor's pixels, whatever the scales involved, and
/// once there sized to it. (The overlays sat at the desktop's corner - the primary monitor -
/// sized from whichever monitor the main window was on.) True when the window isn't there
/// yet - moved or resized this frame - so what it draws now is laid out for the wrong place.
pub fn keep_overlay_on_monitor(vctx: &egui::Context) -> bool {
    let (outer, inner, here) = vctx.input(|i| {
        let v = i.viewport();
        (v.outer_rect, v.inner_rect, v.monitor_size)
    });
    let ppp = vctx.pixels_per_point().max(0.1);
    match overlay_monitor() {
        Some(m) => {
            let want = egui::pos2(m.origin[0] as f32, m.origin[1] as f32);
            let Some(outer) = outer else { return false };
            let at = outer.min * ppp;
            if (at - want).length() > 1.5 {
                vctx.send_viewport_cmd(egui::ViewportCommand::OuterPosition(want / ppp));
                return true;
            }
            // There: as big as the monitor, in its own points.
            let size = egui::vec2(m.size[0] as f32, m.size[1] as f32) / ppp;
            if inner.is_some_and(|r| (r.size() - size).length() > 1.0) {
                vctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(size));
                return true;
            }
            false
        }
        None => {
            if let (Some(inner), Some(want)) = (inner, here) {
                if (inner.size() - want).length() > 1.0 {
                    vctx.send_viewport_cmd(egui::ViewportCommand::OuterPosition(egui::Pos2::ZERO));
                    vctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(want));
                    return true;
                }
            }
            false
        }
    }
}

/// How many frames the overlay `which` has been open, before this one: 0 on the frame it
/// opens. Call once a frame while it's shown; [`overlay_closed`] while it isn't.
pub fn overlay_frames_open(ctx: &egui::Context, which: &'static str) -> u32 {
    let id = egui::Id::new(("fxi_overlay_frames_open", which));
    let n: u32 = ctx.data(|d| d.get_temp(id)).unwrap_or(0);
    ctx.data_mut(|d| d.insert_temp(id, n.saturating_add(1)));
    n
}

/// The overlay `which` isn't shown: it opens afresh next time.
pub fn overlay_closed(ctx: &egui::Context, which: &'static str) {
    ctx.data_mut(|d| d.remove::<u32>(egui::Id::new(("fxi_overlay_frames_open", which))));
}

/// How an overlay draws a frame while it opens. Its window is made afresh each time, and what
/// it draws is laid out from what was measured the frame before - a row pinned scales to the
/// size it measured, a card list stands as tall as it was - so on the frame it opened its
/// widgets came out at guesses, and a window still being moved onto its monitor drew them
/// laid out for the wrong place: a glimpse of everything out of place before it settled.
/// A frame drawn while the window is still being moved into place draws nothing - for a few
/// frames at most, so a window that can't be moved still shows - and the first frame it does
/// draw is laid out [`SETTLE_PASSES`] times, all but the last pass discarded (egui's own way to
/// re-run a frame before showing it), so what shows is laid out from what they measured.
/// Returns whether to leave this frame empty. The overlay's window is shown inside
/// [`with_settle_passes`], which lets egui run that many.
pub fn settle_overlay_frame(vctx: &egui::Context, which: &'static str, frames_open: u32, moved: bool) -> bool {
    let empty = moved && frames_open < 8;
    let shown_id = egui::Id::new(("fxi_overlay_shown", which));
    let settling_id = egui::Id::new(("fxi_overlay_settling", which));
    // Decided on the frame's first pass, and kept for the passes that re-run it.
    let settling = if vctx.current_pass_index() == 0 {
        let shown_before = frames_open > 0 && vctx.data(|d| d.get_temp::<bool>(shown_id)).unwrap_or(false);
        let settling = !empty && !shown_before;
        vctx.data_mut(|d| {
            d.insert_temp(settling_id, settling);
            d.insert_temp(shown_id, !empty || shown_before);
        });
        settling
    } else {
        vctx.data(|d| d.get_temp::<bool>(settling_id)).unwrap_or(false)
    };
    if settling && vctx.current_pass_index() + 1 < SETTLE_PASSES {
        vctx.request_discard("an overlay opening: its widgets measured first");
    }
    empty
}

/// How many passes the first frame an overlay shows is laid out in. A row pinned in its first
/// look scales to what it measured the pass before, and measured at a new scale it comes out a
/// little different again - from a guess, it is in place by the third measurement.
pub const SETTLE_PASSES: usize = 4;

/// Show the overlay `which`'s window (`show`, the viewport call) with egui allowed the passes its
/// first frame settles in ([`settle_overlay_frame`]) while it hasn't shown one yet - two
/// otherwise, egui's own, put back as soon as the window is drawn. (egui reads the limit as a
/// frame starts, so the main window's frame, already under way, keeps its own.)
pub fn with_settle_passes<R>(ctx: &egui::Context, which: &'static str, frames_open: u32, show: impl FnOnce() -> R) -> R {
    let shown = frames_open > 0
        && ctx.data(|d| d.get_temp::<bool>(egui::Id::new(("fxi_overlay_shown", which)))).unwrap_or(false);
    if shown {
        return show();
    }
    let was = ctx.options(|o| o.max_passes);
    let want = std::num::NonZeroUsize::new(SETTLE_PASSES).expect("passes").max(was);
    ctx.options_mut(|o| o.max_passes = want);
    let r = show();
    ctx.options_mut(|o| o.max_passes = was);
    r
}

/// The header's picker, beside the overlay buttons: which monitor the overlays cover.
/// Shown only with more than one.
pub fn render_picker(ui: &mut egui::Ui, bar_h: f32) {
    let list = list();
    if list.len() < 2 {
        return;
    }
    let current = overlay_monitor();
    let shown = current.as_ref().map_or_else(|| "?".to_string(), |m| m.number());
    let size = egui::vec2(30.0, (bar_h - 6.0).max(18.0));
    // A button with a monitor drawn on it - screen, neck and foot - and the display's
    // number on the screen. (A font glyph for one came out as a bare thin box.)
    let resp = ui.add_sized(size, egui::Button::new(""));
    {
        let col = ui.style().interact(&resp).fg_stroke.color;
        let r = resp.rect;
        let h = (r.height() * 0.62).clamp(11.0, 18.0);
        let screen = egui::Rect::from_center_size(egui::pos2(r.center().x, r.center().y - h * 0.12), egui::vec2(h * 1.45, h * 0.86));
        let painter = ui.painter();
        painter.rect_stroke(screen, 2.0, egui::Stroke::new(1.3_f32, col), egui::StrokeKind::Inside);
        let neck_top = screen.bottom();
        let foot_y = neck_top + h * 0.26;
        painter.line_segment([egui::pos2(screen.center().x, neck_top), egui::pos2(screen.center().x, foot_y)], egui::Stroke::new(1.3_f32, col));
        painter.line_segment(
            [egui::pos2(screen.center().x - h * 0.32, foot_y), egui::pos2(screen.center().x + h * 0.32, foot_y)],
            egui::Stroke::new(1.3_f32, col),
        );
        painter.text(screen.center(), egui::Align2::CENTER_CENTER, &shown, crate::settings::ui_font(h * 0.62), col);
    }
    let resp = resp
        .on_hover_text(format!(
            "Overlays show on {}.\nClick to pick the monitor they cover - and the one a\ncalibration's screenshot is taken of.",
            current.as_ref().map_or_else(|| "the primary monitor".to_string(), Monitor::describe)
        ));
    egui::Popup::menu(&resp).show(|ui| {
        ui.set_min_width(180.0);
        for m in &list {
            let on = current.as_ref().is_some_and(|c| c.id == m.id);
            if ui.selectable_label(on, m.describe()).clicked() {
                // The primary is kept as no choice, so a monitor rearrangement that
                // makes another one primary is followed.
                choose((!m.primary).then(|| m.id.clone()));
            }
        }
    });
}

#[cfg(windows)]
fn enumerate() -> Vec<Monitor> {
    use windows_sys::core::BOOL;
    use windows_sys::Win32::Foundation::{LPARAM, RECT};
    use windows_sys::Win32::Graphics::Gdi::{EnumDisplayMonitors, GetMonitorInfoW, HDC, HMONITOR, MONITORINFO, MONITORINFOEXW};

    const MONITORINFOF_PRIMARY: u32 = 1;

    unsafe extern "system" fn each(monitor: HMONITOR, _: HDC, _: *mut RECT, data: LPARAM) -> BOOL {
        // SAFETY: `data` is the `Vec` handed to EnumDisplayMonitors below, alive for the
        // call; MONITORINFOEXW starts with the MONITORINFO GetMonitorInfoW takes, its
        // cbSize telling it which.
        unsafe {
            let out = &mut *(data as *mut Vec<Monitor>);
            let mut mi: MONITORINFOEXW = core::mem::zeroed();
            mi.monitorInfo.cbSize = size_of::<MONITORINFOEXW>() as u32;
            if GetMonitorInfoW(monitor, (&mut mi as *mut MONITORINFOEXW).cast::<MONITORINFO>()) != 0 {
                let r = mi.monitorInfo.rcMonitor;
                let n = mi.szDevice.iter().position(|&c| c == 0).unwrap_or(mi.szDevice.len());
                let id = String::from_utf16_lossy(&mi.szDevice[..n]).trim_start_matches(r"\\.\").to_string();
                out.push(Monitor {
                    id,
                    origin: [r.left, r.top],
                    size: [r.right - r.left, r.bottom - r.top],
                    primary: mi.monitorInfo.dwFlags & MONITORINFOF_PRIMARY != 0,
                });
            }
        }
        1
    }

    let mut out: Vec<Monitor> = Vec::new();
    // SAFETY: plain FFI; the callback only runs during the call, while `out` lives.
    unsafe {
        EnumDisplayMonitors(core::ptr::null_mut(), core::ptr::null(), Some(each), &mut out as *mut Vec<Monitor> as LPARAM);
    }
    out
}

#[cfg(not(windows))]
fn enumerate() -> Vec<Monitor> {
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    // The primary is the fallback, and a chosen monitor that has gone away falls back too.
    #[test]
    fn the_overlays_cover_the_chosen_monitor_while_it_is_there() {
        let list = [
            Monitor { id: "DISPLAY1".into(), origin: [0, 0], size: [1920, 1080], primary: true },
            Monitor { id: "DISPLAY2".into(), origin: [1920, 0], size: [2560, 1440], primary: false },
        ];
        let id = |chosen: Option<&str>| pick(&list, chosen).map(|m| m.id.as_str());
        assert_eq!(id(Some("DISPLAY2")), Some("DISPLAY2"));
        assert_eq!(id(None), Some("DISPLAY1"));
        assert_eq!(id(Some("DISPLAY3")), Some("DISPLAY1"), "a monitor gone away falls back");
        assert_eq!(list[1].describe(), "Display 2: 2560×1440");
        assert_eq!(list[0].describe(), "Display 1: 1920×1080, primary");
    }
}

#[cfg(test)]
mod settle_tests {
    /// The passes egui ran for an overlay's frame `frames_open` (`moved`: its window still
    /// being moved into place), and whether it was left empty.
    fn frame(ctx: &egui::Context, frames_open: u32, moved: bool) -> (usize, bool) {
        let mut passes = 0;
        let mut empty = false;
        super::with_settle_passes(ctx, "test", frames_open, || {
            let _ = ctx.run(egui::RawInput::default(), |ctx| {
                passes += 1;
                empty = super::settle_overlay_frame(ctx, "test", frames_open, moved);
            });
        });
        (passes, empty)
    }

    // The first frame an overlay shows is laid out in several passes (all but the last
    // measure, the last is shown), and only that one; a frame while its window is still being
    // moved shows nothing - for a few frames at most. The limit on passes is egui's own again
    // after each.
    #[test]
    fn an_overlay_lays_out_its_first_shown_frame_until_it_settles() {
        let n = super::SETTLE_PASSES;
        let ctx = egui::Context::default();
        let stock = ctx.options(|o| o.max_passes);
        assert_eq!(frame(&ctx, 0, false), (n, false), "the frame it opens on");
        assert_eq!(ctx.options(|o| o.max_passes), stock, "egui's limit put back");
        assert_eq!(frame(&ctx, 1, false), (1, false), "and none after");
        let ctx = egui::Context::default();
        assert_eq!(frame(&ctx, 0, true), (1, true), "moved: nothing shown, nothing to measure for");
        assert_eq!(frame(&ctx, 1, true), (1, true));
        assert_eq!(frame(&ctx, 2, false), (n, false), "the first frame shown, once in place");
        assert_eq!(frame(&ctx, 3, false), (1, false));
        // Reopened: afresh.
        assert_eq!(frame(&ctx, 0, false), (n, false));
        // A window that never gets into place still shows, after a few frames.
        let ctx = egui::Context::default();
        let shown = (0..20).find(|&n| !frame(&ctx, n, true).1).expect("it shows");
        assert!(shown <= 8, "empty for {shown} frames");
    }
}
