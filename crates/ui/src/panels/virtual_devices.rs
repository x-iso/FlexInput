use std::sync::{Arc, Mutex};

use eframe::egui::{self, Color32, RichText};
use flexinput_virtual::{available_device_kinds, create_device, VirtualDevice};

use crate::canvas::Canvas;

pub struct VirtualDevicePanel {
    /// Devices for this tab. Shared with the I/O thread when this tab is active.
    pub active: Arc<Mutex<Vec<Box<dyn VirtualDevice>>>>,
}

impl VirtualDevicePanel {
    pub fn new() -> Self {
        Self { active: Arc::new(Mutex::new(vec![])) }
    }

    pub fn show(&mut self, ui: &mut egui::Ui, canvas: &mut Canvas) {
        // Snapshot device state briefly so we can render without holding the lock.
        let chips: Vec<(String, String, bool)> = {
            let devs = self.active.lock().unwrap();
            devs.iter().enumerate().map(|(i, d)| {
                (d.id().to_string(), chip_name(&devs, i), d.is_connected())
            }).collect()
        };

        ui.horizontal(|ui| {
            ui.strong("Virtual Outputs");
            ui.separator();

            let mut to_remove: Option<usize> = None;
            for (i, (dev_id, chip_label, connected)) in chips.iter().enumerate() {
                let chip = egui::Frame::default()
                    .inner_margin(egui::Margin::symmetric(6, 2))
                    .corner_radius(12.0)
                    .fill(ui.visuals().widgets.inactive.bg_fill)
                    .stroke(egui::Stroke::new(1.0, ui.visuals().widgets.noninteractive.bg_stroke.color));

                chip.show(ui, |ui| {
                    ui.horizontal(|ui| {
                        let (dot, hover) = if *connected {
                            (RichText::new("●").small().color(Color32::from_rgb(80, 200, 100)),
                             "Connected")
                        } else {
                            (RichText::new("●").small().color(Color32::from_rgb(220, 80, 60)),
                             "Not connected — driver unavailable (ViGEmBus / enigo)")
                        };
                        ui.label(dot).on_hover_text(hover);
                        ui.label(chip_label.as_str());

                        let on_canvas = canvas.snarl.nodes_ids_data().any(|(_, n)| {
                            n.value.module_id == "device.sink"
                                && n.value.params.get("device_id").and_then(|v| v.as_str()) == Some(dev_id.as_str())
                        });
                        if !on_canvas {
                            if ui.small_button("+canvas").on_hover_text("Add to canvas").clicked() {
                                // Re-lock briefly to get the device reference for canvas registration.
                                let devs = self.active.lock().unwrap();
                                if let Some(dev) = devs.get(i) {
                                    canvas.add_virtual_sink(dev.as_ref());
                                    let new_name = chip_name(&devs, i);
                                    let did = dev.id().to_string();
                                    drop(devs);
                                    if let Some((nid, _)) = canvas.snarl.nodes_ids_data().find(|(_, n)| {
                                        n.value.module_id == "device.sink"
                                            && n.value.params.get("device_id").and_then(|v| v.as_str()) == Some(&did)
                                    }) {
                                        if let Some(node) = canvas.snarl.get_node_mut(nid) {
                                            node.display_name = new_name;
                                        }
                                    }
                                }
                            }
                        }

                        if ui.small_button("x").on_hover_text("Remove").clicked() {
                            to_remove = Some(i);
                        }
                    });
                });
            }

            if let Some(i) = to_remove {
                let (removed_id, kind_prefix) = {
                    let mut devs = self.active.lock().unwrap();
                    let removed = devs.remove(i);
                    let id = removed.id().to_string();
                    let prefix = id.split('.').take(2).collect::<Vec<_>>().join(".");
                    (id, prefix)
                };

                // Remove the canvas sink node for the removed device.
                if let Some((nid, _)) = canvas.snarl.nodes_ids_data().find(|(_, n)| {
                    n.value.module_id == "device.sink"
                        && n.value.params.get("device_id").and_then(|v| v.as_str()) == Some(&removed_id)
                }) {
                    canvas.snarl.remove_node(nid);
                }

                // Re-sync canvas node display names for remaining same-kind devices.
                let renames: Vec<(String, String)> = {
                    let devs = self.active.lock().unwrap();
                    devs.iter().enumerate()
                        .filter(|(_, d)| d.id().starts_with(&kind_prefix))
                        .map(|(j, d)| (d.id().to_string(), chip_name(&devs, j)))
                        .collect()
                };
                for (did, new_name) in renames {
                    if let Some((nid, _)) = canvas.snarl.nodes_ids_data().find(|(_, n)| {
                        n.value.module_id == "device.sink"
                            && n.value.params.get("device_id").and_then(|v| v.as_str()) == Some(&did)
                    }) {
                        if let Some(node) = canvas.snarl.get_node_mut(nid) {
                            node.display_name = new_name;
                        }
                    }
                }
            }

            // Add button
            ui.menu_button("+", |ui| {
                ui.label(RichText::new("Add virtual output").strong());
                ui.separator();

                for kind in available_device_kinds() {
                    let already = if kind.allows_multiple {
                        false
                    } else {
                        let devs = self.active.lock().unwrap();
                        devs.iter().any(|a| a.id().starts_with(kind.kind_id))
                    };

                    if already {
                        ui.add_enabled(false, egui::Button::new(kind.display_name));
                    } else if ui.button(kind.display_name).clicked() {
                        let instance = {
                            let devs = self.active.lock().unwrap();
                            devs.iter().filter(|d| d.id().starts_with(kind.kind_id)).count()
                        };

                        let dev = create_device(kind.kind_id, instance);
                        canvas.add_virtual_sink(dev.as_ref());
                        let mut devs = self.active.lock().unwrap();
                        devs.push(dev);
                        let j = devs.len() - 1;
                        let new_name = chip_name(&devs, j);
                        let dev_id = devs[j].id().to_string();
                        drop(devs);
                        if let Some((nid, _)) = canvas.snarl.nodes_ids_data().find(|(_, n)| {
                            n.value.module_id == "device.sink"
                                && n.value.params.get("device_id").and_then(|v| v.as_str()) == Some(&dev_id)
                        }) {
                            if let Some(node) = canvas.snarl.get_node_mut(nid) {
                                node.display_name = new_name;
                            }
                        }
                        ui.close();
                    }
                }
            });
        });
    }
}

fn chip_name(active: &[Box<dyn VirtualDevice>], i: usize) -> String {
    let dev = &active[i];
    let kind_prefix = dev.id().split('.').take(2).collect::<Vec<_>>().join(".");
    let total = active.iter().filter(|d| d.id().starts_with(&kind_prefix)).count();
    let rank  = active[..i].iter().filter(|d| d.id().starts_with(&kind_prefix)).count();
    let base = kind_base_name(&kind_prefix);
    if total <= 1 { base.to_string() } else { format!("{} #{}", base, rank + 1) }
}

fn kind_base_name(kind_prefix: &str) -> &'static str {
    match kind_prefix {
        "virtual.xinput"   => "Virtual XInput",
        "virtual.ds4"      => "Virtual DualShock 4",
        "virtual.keymouse" => "Virtual Keyboard & Mouse",
        _                  => "Virtual Device",
    }
}

impl Default for VirtualDevicePanel {
    fn default() -> Self { Self::new() }
}
