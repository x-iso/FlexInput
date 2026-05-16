use std::collections::{HashMap, HashSet};

use flexinput_core::{ModuleDescriptor, Signal};
use flexinput_devices::PhysicalDevice;

use crate::canvas::Canvas;

pub fn show(
    canvas: &mut Canvas,
    descriptors: &[ModuleDescriptor],
    live_device_ids: &HashSet<String>,
    live_signals: &HashMap<(String, String), Signal>,
    panic_shortcut: &crate::app::PanicShortcut,
    physical_devices: &[PhysicalDevice],
    ui: &mut egui::Ui,
) {
    canvas.show(descriptors, live_device_ids, live_signals, panic_shortcut, physical_devices, ui);
}
