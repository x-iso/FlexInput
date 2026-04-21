use eframe::egui::{self, Color32, RichText};
use flexinput_core::SignalType;
use flexinput_devices::{ControllerKind, DevicePin, PhysicalDevice};

use crate::canvas::Canvas;

pub fn show(ui: &mut egui::Ui, devices: &[PhysicalDevice], canvas: &mut Canvas) {
    ui.horizontal(|ui| {
        ui.strong("Physical Inputs");
        if devices.is_empty() {
            ui.separator();
            ui.label(RichText::new("No devices detected").weak());
        }
    });

    if devices.is_empty() {
        return;
    }

    // Cap card height to whatever space is left after the header row.
    let card_max_h = ui.available_height();

    egui::ScrollArea::horizontal()
        .id_salt("physical_scroll")
        .min_scrolled_height(card_max_h)
        .max_height(card_max_h)
        .show(ui, |ui| {
            ui.horizontal_top(|ui| {
                for device in devices {
                    device_card(ui, device, canvas, card_max_h);
                }
            });
        });
}

fn device_card(ui: &mut egui::Ui, device: &PhysicalDevice, canvas: &mut Canvas, max_h: f32) {
    let already_on_canvas = canvas.snarl.nodes_ids_data().any(|(_, n)| {
        n.value.module_id == "device.source"
            && n.value.params.get("device_id").and_then(|v| v.as_str()) == Some(&device.id)
    });

    egui::Frame::default()
        .inner_margin(egui::Margin::same(8))
        .corner_radius(4.0)
        .stroke(egui::Stroke::new(1.0, ui.visuals().widgets.noninteractive.bg_stroke.color))
        .show(ui, |ui| {
            ui.set_width(220.0);

            // Header row: kind icon + name + canvas button
            ui.horizontal(|ui| {
                ui.label(controller_icon(device.kind));
                ui.label(RichText::new(&device.display_name).strong().small());
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if already_on_canvas {
                        ui.label(RichText::new("On canvas").weak().small());
                    } else if ui.small_button("Add to canvas").clicked() {
                        canvas.add_device_source(device);
                    }
                });
            });

            ui.add_space(4.0);

            // Pin type summary bar
            pin_type_bar(ui, &device.outputs);

            ui.add_space(4.0);

            // Collapsible sections inside a vertical scroll area so they don't
            // push the panel to expand.
            let scroll_h = (max_h - 72.0).max(40.0);
            egui::ScrollArea::vertical()
                .id_salt(format!("{}_vscroll", device.id))
                .max_height(scroll_h)
                .show(ui, |ui| {
                    egui::CollapsingHeader::new(
                        RichText::new(format!("{} outputs", device.outputs.len())).small(),
                    )
                    .id_salt(&device.id)
                    .default_open(false)
                    .show(ui, |ui| {
                        for pin in &device.outputs {
                            pin_row(ui, pin);
                        }
                    });

                    if !device.inputs.is_empty() {
                        egui::CollapsingHeader::new(
                            RichText::new(format!("{} inputs (haptic)", device.inputs.len())).small(),
                        )
                        .id_salt(format!("{}_inputs", device.id))
                        .default_open(false)
                        .show(ui, |ui| {
                            for pin in &device.inputs {
                                pin_row(ui, pin);
                            }
                        });
                    }
                });
        });
}

/// Compact colored dots showing output type distribution.
fn pin_type_bar(ui: &mut egui::Ui, pins: &[DevicePin]) {
    let floats = pins.iter().filter(|p| p.signal_type == SignalType::Float).count();
    let bools  = pins.iter().filter(|p| p.signal_type == SignalType::Bool).count();
    let vecs   = pins.iter().filter(|p| p.signal_type == SignalType::Vec2).count();

    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 4.0;
        if vecs > 0 {
            let [r, g, b] = SignalType::Vec2.color_rgb();
            ui.colored_label(Color32::from_rgb(r, g, b), "●");
            ui.label(RichText::new(format!("Vec2 ×{vecs}")).small());
        }
        if floats > 0 {
            let [r, g, b] = SignalType::Float.color_rgb();
            ui.colored_label(Color32::from_rgb(r, g, b), "●");
            ui.label(RichText::new(format!("Float ×{floats}")).small());
        }
        if bools > 0 {
            let [r, g, b] = SignalType::Bool.color_rgb();
            ui.colored_label(Color32::from_rgb(r, g, b), "●");
            ui.label(RichText::new(format!("Bool ×{bools}")).small());
        }
    });
}

fn pin_row(ui: &mut egui::Ui, pin: &DevicePin) {
    ui.horizontal(|ui| {
        let [r, g, b] = pin.signal_type.color_rgb();
        ui.colored_label(Color32::from_rgb(r, g, b), "●");
        ui.label(RichText::new(&pin.display_name).small());
    });
}

fn controller_icon(kind: ControllerKind) -> &'static str {
    match kind {
        ControllerKind::XInput     => "[Xbox]",
        ControllerKind::DualShock4 => "[DS4]",
        ControllerKind::DualSense  => "[DS5]",
        ControllerKind::SwitchPro  => "[NSW]",
        ControllerKind::Generic    => "[HID]",
    }
}
