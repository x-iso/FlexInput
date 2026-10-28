//! The per-device HidHide eye button on Easy-mode device cards and Advanced-mode
//! device node headers.
//!
//! The masking state lives on `FlexInputApp` (settings + the reconcile cache),
//! which neither card has a route to, so state goes out through a published
//! snapshot and clicks come back through a queue the app drains — the same shape
//! as the gamepad-nav switch (`gamepad_nav::publish_nav_state`).

use std::collections::HashMap;

use eframe::egui;

/// What a device card needs to draw the eye button.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HideUiState {
    /// Hidden from other apps right now (or will be on the next reconcile).
    pub hidden: bool,
    /// The user set this with the eye button; `false` = following the
    /// "hide remapped originals" setting.
    pub manual: bool,
    /// Why the button is greyed out, if it is.
    pub blocked: Option<&'static str>,
}

/// A click on an eye button.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HideRequest {
    /// Left click: pin the device to this state.
    Set(bool),
    /// Right click: drop the manual choice and follow the setting again.
    Auto,
}

static SNAPSHOT: std::sync::Mutex<Option<HashMap<String, HideUiState>>> =
    std::sync::Mutex::new(None);
static REQUESTS: std::sync::Mutex<Vec<(String, HideRequest)>> = std::sync::Mutex::new(Vec::new());

/// Publish this frame's per-device state (called by the app before panels draw).
pub fn publish(states: HashMap<String, HideUiState>) {
    if let Ok(mut g) = SNAPSHOT.lock() {
        *g = Some(states);
    }
}

/// The eye button's state for a device, or `None` where there is no button: a
/// MIDI port, a device this frame has not heard of, or a build without HidHide.
pub fn state_of(device_id: &str) -> Option<HideUiState> {
    SNAPSHOT.lock().ok()?.as_ref()?.get(device_id).copied()
}

/// Queue an eye-button click.
pub fn request(device_id: &str, req: HideRequest) {
    if let Ok(mut q) = REQUESTS.lock() {
        q.push((device_id.to_string(), req));
    }
}

/// Drain queued eye-button clicks (called by the app each frame).
pub fn drain_requests() -> Vec<(String, HideRequest)> {
    REQUESTS.lock().map(|mut q| std::mem::take(&mut *q)).unwrap_or_default()
}

/// Width of the eye button (and of the LED swatch beside it).
pub const EYE_BUTTON_SIZE: f32 = 26.0;
/// The eye button's size — shared with the LED swatch so the two match.
pub const CARD_BUTTON_SIZE: egui::Vec2 = egui::vec2(EYE_BUTTON_SIZE, EYE_BUTTON_SIZE * 0.82);

/// The eye button for `device_id`. Accent-filled while the device is hidden; a
/// small dot in its corner marks a manual choice. Left click toggles (and makes
/// the choice manual), right click returns to automatic. Draws nothing and
/// returns `None` when the device has no button. Returns the button's rect.
pub fn eye_button(ui: &mut egui::Ui, device_id: &str) -> Option<egui::Rect> {
    let st = state_of(device_id)?;
    let size = CARD_BUTTON_SIZE;
    let sense = if st.blocked.is_some() { egui::Sense::hover() } else { egui::Sense::click() };
    let (rect, resp) = ui.allocate_exact_size(size, sense);
    if ui.is_rect_visible(rect) {
        let v = ui.visuals();
        let accent = v.selection.stroke.color;
        let (fill, stroke) = if st.blocked.is_some() {
            (egui::Color32::TRANSPARENT, egui::Stroke::new(1.0_f32, egui::Color32::from_gray(60)))
        } else if st.hidden {
            (v.selection.bg_fill, egui::Stroke::new(1.5_f32, accent))
        } else if resp.hovered() {
            (v.widgets.hovered.bg_fill, v.widgets.hovered.bg_stroke)
        } else {
            (egui::Color32::TRANSPARENT, egui::Stroke::new(1.0_f32, egui::Color32::from_gray(80)))
        };
        let painter = ui.painter();
        painter.rect(rect, 4.0, fill, stroke, egui::StrokeKind::Inside);
        let glyph = if st.blocked.is_some() {
            egui::Color32::from_gray(90)
        } else if st.hidden || resp.hovered() {
            egui::Color32::WHITE
        } else {
            egui::Color32::from_gray(170)
        };
        paint_eye(painter, rect.center(), rect.width() * 0.62, glyph);
        if st.manual && st.blocked.is_none() {
            let c = egui::pos2(rect.right() - 4.5, rect.top() + 4.5);
            painter.circle_filled(c, 2.5, egui::Color32::from_rgb(230, 170, 60));
        }
    }
    // Linux hides through FlexInput's own helper, not HidHide.
    let via = if cfg!(windows) { " (HidHide)" } else { "" };
    let tip = match st.blocked {
        Some(why) => format!("Hide from games{via} — unavailable.\n{why}"),
        None => {
            let state = if st.hidden { "Hidden from games" } else { "Visible to games" };
            let mode = if st.manual {
                "set by you — right-click to go back to automatic"
            } else {
                "automatic — hidden while remapped to a virtual output, when \
                 Settings → Hide original controllers is on"
            };
            format!("{state}{via}, {mode}.\nClick to {} it.",
                if st.hidden { "show" } else { "hide" })
        }
    };
    let resp = resp.on_hover_text(tip);
    if st.blocked.is_none() {
        if resp.clicked() {
            request(device_id, HideRequest::Set(!st.hidden));
        } else if resp.secondary_clicked() && st.manual {
            request(device_id, HideRequest::Auto);
        }
    }
    Some(rect)
}

/// An eye: two arcs meeting at the corners, and a pupil.
fn paint_eye(painter: &egui::Painter, c: egui::Pos2, w: f32, color: egui::Color32) {
    let h = w * 0.36;
    let n = 12;
    let arc = |sign: f32| -> Vec<egui::Pos2> {
        (0..=n)
            .map(|i| {
                let t = i as f32 / n as f32;
                let x = c.x - w * 0.5 + w * t;
                // Half-sine lid: zero at the corners, `h` at the middle.
                let y = c.y + sign * h * (std::f32::consts::PI * t).sin();
                egui::pos2(x, y)
            })
            .collect()
    };
    let stroke = egui::Stroke::new(1.6_f32, color);
    painter.add(egui::Shape::line(arc(-1.0), stroke));
    painter.add(egui::Shape::line(arc(1.0), stroke));
    painter.circle_filled(c, h * 0.62, color);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requests_drain_once() {
        let _ = drain_requests();
        request("sdl:ps5:0", HideRequest::Set(true));
        request("sdl:ps5:0", HideRequest::Auto);
        assert_eq!(
            drain_requests(),
            vec![
                ("sdl:ps5:0".to_string(), HideRequest::Set(true)),
                ("sdl:ps5:0".to_string(), HideRequest::Auto),
            ]
        );
        assert!(drain_requests().is_empty());
    }
}
