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
/// sized from whichever monitor the main window was on.)
pub fn keep_overlay_on_monitor(vctx: &egui::Context) {
    let (outer, inner, here) = vctx.input(|i| {
        let v = i.viewport();
        (v.outer_rect, v.inner_rect, v.monitor_size)
    });
    let ppp = vctx.pixels_per_point().max(0.1);
    match overlay_monitor() {
        Some(m) => {
            let want = egui::pos2(m.origin[0] as f32, m.origin[1] as f32);
            let Some(outer) = outer else { return };
            let at = outer.min * ppp;
            if (at - want).length() > 1.5 {
                vctx.send_viewport_cmd(egui::ViewportCommand::OuterPosition(want / ppp));
                return;
            }
            // There: as big as the monitor, in its own points.
            let size = egui::vec2(m.size[0] as f32, m.size[1] as f32) / ppp;
            if inner.is_some_and(|r| (r.size() - size).length() > 1.0) {
                vctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(size));
            }
        }
        None => {
            if let (Some(inner), Some(want)) = (inner, here) {
                if (inner.size() - want).length() > 1.0 {
                    vctx.send_viewport_cmd(egui::ViewportCommand::OuterPosition(egui::Pos2::ZERO));
                    vctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(want));
                }
            }
        }
    }
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
