use eframe::egui::{self, Color32, RichText};
use flexinput_core::SignalType;
use flexinput_devices::{ControllerKind, DevicePin, HidHideClient, PhysicalDevice};

use crate::canvas::Canvas;

pub fn show(
    ui: &mut egui::Ui,
    devices: &[PhysicalDevice],
    canvas: &mut Canvas,
    hidhide: Option<&HidHideClient>,
) {
    ui.horizontal(|ui| {
        ui.strong("Physical Devices");
        if devices.is_empty() {
            ui.separator();
            ui.label(RichText::new("No devices detected").weak());
        }
    });

    if devices.is_empty() {
        return;
    }

    let blacklist: Vec<String> = hidhide.map_or_else(Vec::new, |hh| hh.blacklist());
    let card_max_h = ui.available_height();

    egui::ScrollArea::horizontal()
        .id_salt("physical_scroll")
        .max_height(card_max_h)
        .show(ui, |ui| {
            ui.horizontal_top(|ui| {
                for device in devices {
                    let is_hidden = device.instance_path.as_deref()
                        .map_or(false, |ip| {
                            let up = ip.to_uppercase();
                            blacklist.iter().any(|b| b.to_uppercase() == up)
                        });
                    device_card(ui, device, canvas, hidhide, is_hidden, card_max_h);
                }
            });
        });
}

// ── Device card ───────────────────────────────────────────────────────────────

fn device_card(
    ui: &mut egui::Ui,
    device: &PhysicalDevice,
    canvas: &mut Canvas,
    hidhide: Option<&HidHideClient>,
    is_hidden: bool,
    max_h: f32,
) {
    // MIDI OUT is a sink even though enumerate() returns it with empty inputs.
    let is_sink = match device.kind {
        ControllerKind::MidiOut => true,
        ControllerKind::MidiIn  => false,
        _ => device.outputs.is_empty() && !device.inputs.is_empty(),
    };
    let canvas_module = if is_sink { "device.sink" } else { "device.source" };
    let already_on_canvas = canvas.snarl.nodes_ids_data().any(|(_, n)| {
        n.value.module_id == canvas_module
            && n.value.params.get("device_id").and_then(|v| v.as_str()) == Some(&device.id)
    });

    egui::Frame::default()
        .inner_margin(egui::Margin::same(8))
        .corner_radius(4.0)
        .stroke(egui::Stroke::new(1.0, ui.visuals().widgets.noninteractive.bg_stroke.color))
        .show(ui, |ui| {
            ui.set_width(220.0);

            ui.horizontal(|ui| {
                ui.label(controller_icon(device.kind));
                ui.label(RichText::new(&device.display_name).strong().small());
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if already_on_canvas {
                        ui.label(RichText::new("On canvas").weak().small());
                    } else if ui.small_button("Add to canvas").clicked() {
                        if is_sink {
                            canvas.add_physical_sink(device);
                        } else {
                            canvas.add_device_source(device);
                        }
                    }

                    if let Some(hh) = hidhide {
                        if device.instance_path.is_some() {
                            let (icon, hover) = if is_hidden {
                                (RichText::new("👁").small().color(ui.visuals().weak_text_color()),
                                 "Hidden from system (click to show)")
                            } else {
                                (RichText::new("👁").small(),
                                 "Visible to system (click to hide from other apps)")
                            };
                            if ui.add(egui::Button::new(icon).frame(false))
                                .on_hover_text(hover)
                                .clicked()
                            {
                                if let Some(ip) = &device.instance_path {
                                    hh.set_hidden(ip, !is_hidden);
                                    if !is_hidden { hh.set_active(true); }
                                }
                            }
                        } else {
                            ui.add_enabled(false,
                                egui::Button::new(RichText::new("👁").small().weak()).frame(false),
                            ).on_hover_text("Device path unavailable — cannot control HidHide");
                        }
                    }
                });
            });

            ui.add_space(4.0);
            let summary_pins = if is_sink { &device.inputs } else { &device.outputs };
            pin_type_bar(ui, summary_pins);
            ui.add_space(4.0);

            let scroll_h = (max_h - 72.0).max(40.0);
            egui::ScrollArea::vertical()
                .id_salt(format!("{}_vscroll", device.id))
                .max_height(scroll_h)
                .show(ui, |ui| {
                    if !device.outputs.is_empty() {
                        egui::CollapsingHeader::new(
                            RichText::new(format!("{} outputs", device.outputs.len())).small(),
                        )
                        .id_salt(&device.id)
                        .default_open(false)
                        .show(ui, |ui| {
                            for pin in &device.outputs { pin_row(ui, pin); }
                        });
                    }
                    if !device.inputs.is_empty() {
                        let label = if is_sink {
                            format!("{} inputs", device.inputs.len())
                        } else {
                            format!("{} inputs (haptic)", device.inputs.len())
                        };
                        egui::CollapsingHeader::new(RichText::new(label).small())
                            .id_salt(format!("{}_inputs", device.id))
                            .default_open(false)
                            .show(ui, |ui| {
                                for pin in &device.inputs { pin_row(ui, pin); }
                            });
                    }
                });
        });
}

// ── Shared helpers ────────────────────────────────────────────────────────────

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
        ControllerKind::MidiIn     => "[MIDI IN]",
        ControllerKind::MidiOut    => "[MIDI OUT]",
    }
}
