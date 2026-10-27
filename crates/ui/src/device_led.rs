//! The per-device default lightbar colour swatch on Easy-mode device cards and
//! Advanced-mode device node headers.
//!
//! The colour lives in settings (`AppSettings::led_defaults`), which neither
//! card has a route to, so — like the HidHide eye beside it (`hidhide_toggle`) —
//! state goes out through a published snapshot and edits come back through a
//! queue the app drains. The I/O thread sends the colour to the pad whenever no
//! game or module drives its lightbar.

use std::collections::HashMap;

use eframe::egui;

/// An edit made with a swatch.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LedRequest {
    /// Use this colour while nothing else drives the lightbar.
    Set([u8; 3]),
    /// No default: leave the lightbar to the pad itself.
    Clear,
}

/// Device id → its default colour (`None` = unset), for devices that have a
/// lightbar. Devices without one aren't listed and get no swatch.
static SNAPSHOT: std::sync::Mutex<Option<HashMap<String, Option<[u8; 3]>>>> =
    std::sync::Mutex::new(None);
static REQUESTS: std::sync::Mutex<Vec<(String, LedRequest)>> = std::sync::Mutex::new(Vec::new());

/// Publish this frame's per-device state (called by the app before panels draw).
pub fn publish(states: HashMap<String, Option<[u8; 3]>>) {
    if let Ok(mut g) = SNAPSHOT.lock() {
        *g = Some(states);
    }
}

/// `Some(default colour or None)` for a device with a lightbar; `None` when it
/// has none (or this frame hasn't heard of it), so no swatch is drawn.
pub fn state_of(device_id: &str) -> Option<Option<[u8; 3]>> {
    SNAPSHOT.lock().ok()?.as_ref()?.get(device_id).copied()
}

/// Queue a swatch edit.
pub fn request(device_id: &str, req: LedRequest) {
    if let Ok(mut q) = REQUESTS.lock() {
        q.push((device_id.to_string(), req));
    }
}

/// Drain queued swatch edits (called by the app each frame).
pub fn drain_requests() -> Vec<(String, LedRequest)> {
    REQUESTS.lock().map(|mut q| std::mem::take(&mut *q)).unwrap_or_default()
}

/// The default-colour swatch for `device_id`, the HidHide eye's size. Filled
/// with the colour when one is set, empty with a slash when not. Click opens a
/// colour picker; right-click clears it. Draws nothing and returns `None` for a
/// device without a lightbar. Returns the swatch's rect.
pub fn swatch_button(ui: &mut egui::Ui, device_id: &str) -> Option<egui::Rect> {
    let current = state_of(device_id)?;
    let (rect, resp) = ui.allocate_exact_size(
        crate::hidhide_toggle::CARD_BUTTON_SIZE, egui::Sense::click());
    if ui.is_rect_visible(rect) {
        let painter = ui.painter();
        let stroke = if resp.hovered() {
            egui::Stroke::new(1.5_f32, egui::Color32::WHITE)
        } else {
            egui::Stroke::new(1.0_f32, egui::Color32::from_gray(80))
        };
        match current {
            Some([r, g, b]) => {
                painter.rect(rect, 4.0, egui::Color32::from_rgb(r, g, b), stroke,
                    egui::StrokeKind::Inside);
            }
            None => {
                painter.rect(rect, 4.0, egui::Color32::TRANSPARENT, stroke,
                    egui::StrokeKind::Inside);
                let inner = rect.shrink(5.0);
                painter.line_segment([inner.left_bottom(), inner.right_top()],
                    egui::Stroke::new(1.2_f32, egui::Color32::from_gray(110)));
            }
        }
    }
    let tip = match current {
        Some(_) => "Default light-bar colour — used whenever no game or module sets it.\n\
                    Click to change, right-click to clear (the pad keeps its own colour).",
        None => "No default light-bar colour — the pad keeps its own until a game or \
                 module sets one.\nClick to pick a default.",
    };
    let resp = resp.on_hover_cursor(egui::CursorIcon::PointingHand).on_hover_text(tip);

    let popup_id = ui.id().with(("led_default_picker", device_id));
    if resp.clicked() {
        egui::Popup::toggle_id(ui.ctx(), popup_id);
    }
    if resp.secondary_clicked() && current.is_some() {
        request(device_id, LedRequest::Clear);
    }
    crate::widgets::popup_below_widget(&resp, popup_id,
        egui::PopupCloseBehavior::CloseOnClickOutside, |ui| {
            ui.set_min_width(220.0);
            let [r, g, b] = current.unwrap_or([0, 0, 255]);
            let mut c = egui::Color32::from_rgb(r, g, b);
            if egui::widgets::color_picker::color_picker_color32(
                ui, &mut c, egui::widgets::color_picker::Alpha::Opaque,
            ) {
                request(device_id, LedRequest::Set([c.r(), c.g(), c.b()]));
            }
            if current.is_some() && ui.button("Clear default").clicked() {
                request(device_id, LedRequest::Clear);
                egui::Popup::close_id(ui.ctx(), popup_id);
            }
        });
    Some(rect)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requests_drain_once() {
        let _ = drain_requests();
        request("sdl:ps4:0", LedRequest::Set([1, 2, 3]));
        request("sdl:ps4:0", LedRequest::Clear);
        assert_eq!(
            drain_requests(),
            vec![
                ("sdl:ps4:0".to_string(), LedRequest::Set([1, 2, 3])),
                ("sdl:ps4:0".to_string(), LedRequest::Clear),
            ]
        );
        assert!(drain_requests().is_empty());
    }
}
