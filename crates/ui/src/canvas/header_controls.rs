//! Shared renderers for the per-device-source header controls
//! (Calibrate button + Hz, Deadzone slider, Gyro × slider) and the
//! per-keymouse-sink Mouse × slider.
//!
//! These widgets used to live exclusively in the snarl node header
//! (see [`crate::canvas::viewer`]); they're factored out so Easy mode
//! can reuse the same rendering and parameter-mutation behavior
//! without duplicating it. The Advanced-mode node header now calls
//! these helpers, so any future tweak only needs to happen in one
//! place.

use std::collections::HashMap;

use eframe::egui;
use egui_snarl::NodeId;
use serde_json::Value;

use crate::canvas::DeviceParamDefaults;
use crate::canvas::viewer::{device_source_caps, slider_label, slider_track_double_clicked};

const LABEL_CELL_W: f32 = 60.0;

/// Render the Calibrate button + measured polling-rate display for a
/// `device.source` node. Sets `*calibrate_request = Some(node_id)`
/// when clicked. `device_rates_hz` is the map maintained by the app
/// (device_id → measured polling Hz).
pub fn render_calibrate_row(
    ui: &mut egui::Ui,
    node_id: NodeId,
    device_id: &str,
    device_rates_hz: &HashMap<String, u32>,
    calibrate_request: &mut Option<NodeId>,
) {
    let (has_dz, has_gy, has_st) = device_source_caps(device_id, true);
    if !(has_dz || has_gy || has_st) {
        return;
    }
    ui.horizontal(|ui| {
        if ui.small_button("Calibrate")
            .on_hover_text("Open the Device Calibration window")
            .clicked()
        {
            *calibrate_request = Some(node_id);
        }
        let hz = device_rates_hz.get(device_id).copied().unwrap_or(0);
        ui.label(egui::RichText::new(format!("{} Hz", hz))
            .color(egui::Color32::from_rgb(220, 160, 40))
            .small())
            .on_hover_text("Measured per-device polling rate (raw events/sec)");
    });
}

/// Render the Deadzone and Gyro × sliders for a `device.source` node.
/// Mutates `params` in place when the user moves a slider. Returns
/// `true` if any param value changed this frame (caller can use it to
/// flag the canvas dirty / push undo).
pub fn render_deadzone_gyro_sliders(
    ui: &mut egui::Ui,
    params: &mut HashMap<String, Value>,
    device_id: &str,
    defaults: DeviceParamDefaults,
) -> bool {
    let (has_dz, has_gy, _has_st) = device_source_caps(device_id, true);
    if !(has_dz || has_gy) {
        return false;
    }
    let mut dz = params.get("deadzone").and_then(|v| v.as_f64()).unwrap_or(0.1) as f32;
    let mut gm = params.get("gyro_multiplier").and_then(|v| v.as_f64()).unwrap_or(1.0) as f32;
    let dz_initial = dz;
    let gm_initial = gm;

    if has_dz {
        ui.horizontal(|ui| {
            slider_label(ui, "Deadzone", LABEL_CELL_W);
            let resp = ui.add(egui::Slider::new(&mut dz, 0.0_f32..=0.5)
                .show_value(false)
                .clamping(egui::SliderClamping::Always));
            if slider_track_double_clicked(ui, &resp) { dz = defaults.stick_deadzone; }
            ui.add(egui::DragValue::new(&mut dz)
                .speed(0.005)
                .range(0.0_f32..=0.5)
                .fixed_decimals(2));
        });
    }
    if has_gy {
        ui.horizontal(|ui| {
            slider_label(ui, "Gyro ×", LABEL_CELL_W);
            let resp = ui.add(egui::Slider::new(&mut gm, 0.1_f32..=50.0)
                .logarithmic(true)
                .show_value(false)
                .clamping(egui::SliderClamping::Always));
            if slider_track_double_clicked(ui, &resp) { gm = defaults.gyro_mult; }
            ui.add(egui::DragValue::new(&mut gm)
                .speed(0.05)
                .range(0.1_f32..=50.0)
                .fixed_decimals(2));
        });
    }

    let mut changed = false;
    if (dz - dz_initial).abs() > f32::EPSILON {
        params.insert("deadzone".into(), Value::from(dz as f64));
        changed = true;
    }
    if (gm - gm_initial).abs() > f32::EPSILON {
        params.insert("gyro_multiplier".into(), Value::from(gm as f64));
        changed = true;
    }
    changed
}

/// Render the Mouse × (Mouse Sensitivity) slider for a
/// `virtual.keymouse` sink node. Returns true if the value changed.
pub fn render_mouse_sens_slider(
    ui: &mut egui::Ui,
    params: &mut HashMap<String, Value>,
    defaults: DeviceParamDefaults,
) -> bool {
    let mut ms = params.get("mouse_sensitivity").and_then(|v| v.as_f64()).unwrap_or(1.0) as f32;
    let ms_initial = ms;
    ui.horizontal(|ui| {
        slider_label(ui, "Mouse ×", LABEL_CELL_W);
        let resp = ui.add(egui::Slider::new(&mut ms, 0.0_f32..=3000.0)
            .logarithmic(true)
            .show_value(false)
            .clamping(egui::SliderClamping::Always));
        if slider_track_double_clicked(ui, &resp) { ms = defaults.mouse_sensitivity; }
        ui.add(egui::DragValue::new(&mut ms)
            .speed(0.5)
            .range(0.0_f32..=3000.0)
            .fixed_decimals(2));
    });
    if (ms - ms_initial).abs() > f32::EPSILON {
        params.insert("mouse_sensitivity".into(), Value::from(ms as f64));
        true
    } else {
        false
    }
}
