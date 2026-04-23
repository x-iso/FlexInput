use eframe::egui::{self, Color32, RichText};
use flexinput_virtual::{available_device_kinds, create_device, VirtualDevice};

use crate::canvas::Canvas;

pub struct VirtualDevicePanel {
    pub active: Vec<Box<dyn VirtualDevice>>,
}

impl VirtualDevicePanel {
    pub fn new() -> Self {
        Self { active: vec![] }
    }

    pub fn show(&mut self, ui: &mut egui::Ui, canvas: &mut Canvas) {
        ui.horizontal(|ui| {
            ui.strong("Virtual Outputs");
            ui.separator();

            let mut to_remove: Option<usize> = None;
            for i in 0..self.active.len() {
                let dev = &self.active[i];
                let chip_label = chip_name(&self.active, i);

                let chip = egui::Frame::default()
                    .inner_margin(egui::Margin::symmetric(6, 2))
                    .corner_radius(12.0)
                    .fill(ui.visuals().widgets.inactive.bg_fill)
                    .stroke(egui::Stroke::new(1.0, ui.visuals().widgets.noninteractive.bg_stroke.color));

                chip.show(ui, |ui| {
                    ui.horizontal(|ui| {
                        let (dot, hover) = if dev.is_connected() {
                            (RichText::new("●").small().color(Color32::from_rgb(80, 200, 100)),
                             "Connected")
                        } else {
                            (RichText::new("●").small().color(Color32::from_rgb(220, 80, 60)),
                             "Not connected — driver unavailable (ViGEmBus / enigo)")
                        };
                        ui.label(dot).on_hover_text(hover);
                        ui.label(&chip_label);

                        // Re-add to canvas only when the sink node is absent.
                        let dev_id = dev.id().to_string();
                        let on_canvas = canvas.snarl.nodes_ids_data().any(|(_, n)| {
                            n.value.module_id == "device.sink"
                                && n.value.params.get("device_id").and_then(|v| v.as_str()) == Some(&dev_id)
                        });
                        if !on_canvas {
                            if ui.small_button("+canvas").on_hover_text("Add to canvas").clicked() {
                                let dev_ref: &dyn VirtualDevice = self.active[i].as_ref();
                                canvas.add_virtual_sink(dev_ref);
                                // Fix canvas node title to match current chip numbering.
                                let new_name = chip_name(&self.active, i);
                                let dev_id_str = self.active[i].id().to_string();
                                if let Some((nid, _)) = canvas.snarl.nodes_ids_data().find(|(_, n)| {
                                    n.value.module_id == "device.sink"
                                        && n.value.params.get("device_id").and_then(|v| v.as_str()) == Some(&dev_id_str)
                                }) {
                                    if let Some(node) = canvas.snarl.get_node_mut(nid) {
                                        node.display_name = new_name;
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
                let removed = self.active.remove(i);
                let removed_id = removed.id().to_string();

                // Find and remove the canvas sink node for the removed device.
                if let Some((nid, _)) = canvas.snarl.nodes_ids_data().find(|(_, n)| {
                    n.value.module_id == "device.sink"
                        && n.value.params.get("device_id").and_then(|v| v.as_str()) == Some(&removed_id)
                }) {
                    canvas.snarl.remove_node(nid);
                }

                // Update display_names of canvas nodes for same-kind devices so their
                // titles stay in sync with the chip labels.
                let kind_prefix = removed_id.split('.').take(2).collect::<Vec<_>>().join(".");
                for (j, dev) in self.active.iter().enumerate() {
                    if !dev.id().starts_with(&kind_prefix) { continue; }
                    let new_name = chip_name(&self.active, j);
                    let dev_id = dev.id().to_string();
                    if let Some((nid, _)) = canvas.snarl.nodes_ids_data().find(|(_, n)| {
                        n.value.module_id == "device.sink"
                            && n.value.params.get("device_id").and_then(|v| v.as_str()) == Some(&dev_id)
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
                        self.active.iter().any(|a| a.id().starts_with(kind.kind_id))
                    };

                    if already {
                        ui.add_enabled(false, egui::Button::new(kind.display_name));
                    } else if ui.button(kind.display_name).clicked() {
                        let instance = self.active.iter()
                            .filter(|d| d.id().starts_with(kind.kind_id))
                            .count();

                        let dev = create_device(kind.kind_id, instance);
                        canvas.add_virtual_sink(dev.as_ref());
                        self.active.push(dev);
                        // Update the canvas node title to reflect current numbering.
                        let j = self.active.len() - 1;
                        let new_name = chip_name(&self.active, j);
                        let dev_id = self.active[j].id().to_string();
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

/// Compute the human-readable label for chip at index `i`, based on how many
/// devices of the same kind are currently active.  Always current after add/remove.
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
