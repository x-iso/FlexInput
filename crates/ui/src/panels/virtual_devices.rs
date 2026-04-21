use std::collections::HashMap;

use eframe::egui::{self, RichText};
use flexinput_virtual::{available_device_kinds, create_device, VirtualDevice};

use crate::canvas::Canvas;

pub struct VirtualDevicePanel {
    pub active: Vec<Box<dyn VirtualDevice>>,
    /// How many instances of each kind have been created (ever, monotonically increasing).
    instance_counts: HashMap<String, usize>,
}

impl VirtualDevicePanel {
    pub fn new() -> Self {
        Self { active: vec![], instance_counts: HashMap::new() }
    }

    pub fn show(&mut self, ui: &mut egui::Ui, canvas: &mut Canvas) {
        ui.horizontal(|ui| {
            ui.strong("Virtual Outputs");
            ui.separator();

            // Active device chips
            let mut to_remove: Option<usize> = None;
            for (i, dev) in self.active.iter().enumerate() {
                let chip = egui::Frame::default()
                    .inner_margin(egui::Margin::symmetric(6, 2))
                    .corner_radius(12.0)
                    .fill(ui.visuals().widgets.inactive.bg_fill)
                    .stroke(egui::Stroke::new(1.0, ui.visuals().widgets.noninteractive.bg_stroke.color));

                chip.show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.label(dev.display_name());
                        if ui.small_button("x").clicked() {
                            to_remove = Some(i);
                        }
                    });
                });
            }
            if let Some(i) = to_remove {
                self.active.remove(i);
            }

            // Add button — reads static metadata only, no connections
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
                        let instance = *self.instance_counts
                            .entry(kind.kind_id.to_string())
                            .or_insert(0);
                        self.instance_counts.insert(kind.kind_id.to_string(), instance + 1);

                        let dev = create_device(kind.kind_id, instance);
                        canvas.add_virtual_sink(dev.as_ref());
                        self.active.push(dev);
                        ui.close();
                    }
                }
            });
        });
    }
}

impl Default for VirtualDevicePanel {
    fn default() -> Self {
        Self::new()
    }
}
