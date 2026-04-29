use std::collections::HashMap;
use serde_json::Value;

/// Topologically-sorted, egui-free snapshot of the signal graph.
/// Rebuilt by the UI thread every frame so the processing thread always has current params.
#[derive(Clone, Default)]
pub struct ProcessingGraph {
    pub nodes: Vec<NodeSnap>,
}

/// Routing metadata for device.sink nodes: how to dispatch computed signals to a device.
#[derive(Clone)]
pub struct SinkTarget {
    /// The virtual or physical device_id this sink drives.
    pub device_id: String,
    /// For each input slot: the destination pin_id on the device.
    pub pin_ids: Vec<String>,
    /// For each input slot: ALL upstream sources (multi-source; combined additively).
    pub multi_sources: Vec<Vec<(usize, usize)>>,
    /// If an AutoMap pin is wired: (source_device_id, source_output_pin_ids).
    pub automap_source: Option<(String, Vec<String>)>,
}

#[derive(Clone)]
pub struct NodeSnap {
    /// egui_snarl::NodeId.0 — key into shared state maps.
    pub node_uid: usize,
    pub module_id: String,
    pub params: HashMap<String, Value>,
    pub n_outputs: usize,
    /// For each input pin: (index into `nodes` vec of source node, which output pin).
    /// Empty for device.sink nodes (they use sink_target.multi_sources instead).
    pub input_sources: Vec<Option<(usize, usize)>>,
    /// device.source: device_id and ordered output pin IDs.
    pub device_id: Option<String>,
    pub output_pin_ids: Vec<String>,
    /// Counter reset: if Some, the processing thread overwrites its aux_f32 state.
    pub aux_f32_override: Option<Vec<f32>>,
    /// Populated only for device.sink nodes; None for all others.
    pub sink_target: Option<SinkTarget>,
}
