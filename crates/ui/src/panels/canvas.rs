use std::collections::HashSet;

use flexinput_core::ModuleDescriptor;

use crate::canvas::Canvas;

pub fn show(
    canvas: &mut Canvas,
    descriptors: &[ModuleDescriptor],
    live_device_ids: &HashSet<String>,
    ui: &mut egui::Ui,
) {
    canvas.show(descriptors, live_device_ids, ui);
}
