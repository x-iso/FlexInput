use std::collections::{HashMap, HashSet};

use egui_snarl::NodeId;
use flexinput_core::{ModuleDescriptor, Signal};
use flexinput_devices::PhysicalDevice;

use crate::canvas::{Canvas, DeviceParamDefaults};

pub fn show(
    canvas: &mut Canvas,
    descriptors: &[ModuleDescriptor],
    live_device_ids: &HashSet<String>,
    live_signals: &HashMap<(String, String), Signal>,
    panic_shortcut: &crate::app::PanicShortcut,
    physical_devices: &[PhysicalDevice],
    device_rates: &HashMap<String, u32>,
    param_defaults: DeviceParamDefaults,
    ui: &mut egui::Ui,
) -> Option<NodeId> {
    canvas.show(
        descriptors, live_device_ids, live_signals,
        panic_shortcut, physical_devices, device_rates,
        param_defaults, ui,
    )
}
