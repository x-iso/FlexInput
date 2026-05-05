use std::collections::HashSet;

use flexinput_core::ModuleDescriptor;
use flexinput_devices::PhysicalDevice;

use crate::canvas::Canvas;

pub fn show(
    canvas: &mut Canvas,
    descriptors: &[ModuleDescriptor],
    live_device_ids: &HashSet<String>,
    physical_devices: &[PhysicalDevice],
    ui: &mut egui::Ui,
) {
    canvas.show(descriptors, live_device_ids, physical_devices, ui);
}
