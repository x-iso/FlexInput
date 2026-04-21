mod node;
mod viewer;

pub use node::NodeData;
pub use viewer::FlexViewer;

use std::collections::HashMap;

use egui_snarl::{ui::SnarlStyle, Snarl};
use flexinput_core::{PinDescriptor, ModuleDescriptor};
use flexinput_devices::PhysicalDevice;
use flexinput_virtual::{SinkPin, VirtualDevice};
use serde_json::Value;

pub struct Canvas {
    pub snarl: Snarl<NodeData>,
    style: SnarlStyle,
}

impl Canvas {
    pub fn new() -> Self {
        let mut style = SnarlStyle::default();
        style.collapsible = Some(true);
        Canvas { snarl: Snarl::new(), style }
    }

    pub fn show(&mut self, descriptors: &[ModuleDescriptor], ui: &mut egui::Ui) {
        let mut viewer = FlexViewer { descriptors };
        self.snarl.show(&mut viewer, &self.style, "flexinput_canvas", ui);
    }

    /// Add a physical device as a source node. No-op if already present.
    pub fn add_device_source(&mut self, device: &PhysicalDevice) {
        let already_present = self.snarl.nodes_ids_data().any(|(_, n)| {
            n.value.module_id == "device.source"
                && n.value.params.get("device_id").and_then(|v| v.as_str()) == Some(&device.id)
        });
        if already_present {
            return;
        }

        let outputs = device
            .outputs
            .iter()
            .map(|p| PinDescriptor::new(&p.display_name, p.signal_type))
            .collect();

        let inputs = device
            .inputs
            .iter()
            .map(|p| PinDescriptor::new(&p.display_name, p.signal_type))
            .collect();

        let mut params = HashMap::new();
        params.insert("device_id".to_string(), Value::String(device.id.clone()));
        params.insert("output_pin_ids".to_string(), Value::Array(
            device.outputs.iter().map(|p| Value::String(p.id.clone())).collect(),
        ));
        params.insert("input_pin_ids".to_string(), Value::Array(
            device.inputs.iter().map(|p| Value::String(p.id.clone())).collect(),
        ));

        let node = NodeData {
            module_id: "device.source".to_string(),
            display_name: device.display_name.clone(),
            category: "Device".to_string(),
            inputs,
            outputs,
            params,
        };

        self.snarl.insert_node(egui::pos2(80.0, 80.0), node);
    }

    /// Add a virtual device as a sink node. No-op if already present (keyed by device id).
    pub fn add_virtual_sink(&mut self, device: &dyn VirtualDevice) {
        let already_present = self.snarl.nodes_ids_data().any(|(_, n)| {
            n.value.module_id == "device.sink"
                && n.value.params.get("device_id").and_then(|v| v.as_str()) == Some(device.id())
        });
        if already_present {
            return;
        }

        let fixed_count = device.sink_pins().len();
        let inputs = device
            .sink_pins()
            .iter()
            .map(|p: &SinkPin| PinDescriptor::new(p.display_name, p.signal_type))
            .collect();

        let mut params = HashMap::new();
        params.insert("device_id".to_string(), Value::String(device.id().to_string()));
        params.insert("fixed_input_count".to_string(), Value::Number(fixed_count.into()));
        params.insert("input_pin_ids".to_string(), Value::Array(
            device.sink_pins().iter().map(|p| Value::String(p.id.to_string())).collect(),
        ));

        let node = NodeData {
            module_id: "device.sink".to_string(),
            display_name: device.display_name().to_string(),
            category: "Device".to_string(),
            inputs,
            outputs: vec![],
            params,
        };

        self.snarl.insert_node(egui::pos2(400.0, 80.0), node);
    }
}

impl Default for Canvas {
    fn default() -> Self {
        Self::new()
    }
}
