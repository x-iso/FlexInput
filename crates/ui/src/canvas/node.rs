use std::collections::{HashMap, VecDeque};

use flexinput_core::{ModuleDescriptor, PinDescriptor, Signal};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Runtime-only per-node state (not serialized).  Used by display modules.
#[derive(Debug, Clone, Default)]
pub struct NodeExtra {
    /// Rolling signal history for oscilloscope / vectorscope nodes.
    /// Each entry is [ch0, ch1, ch2, ch3]; None when the channel is unconnected.
    pub history: VecDeque<[Option<f32>; 4]>,
    /// Most recent evaluated signal per input (for readout / body display).
    pub last_signals: Vec<Option<Signal>>,
    /// Ring buffer of (timestamp, value) pairs for the delay module.
    pub delay_buf: VecDeque<(std::time::Instant, f32)>,
    /// Biquad direct-form-II state [x₋₁, x₋₂, y₋₁, y₋₂] for the lowpass module.
    pub filter_state: [f64; 4],
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeData {
    pub module_id: String,
    pub display_name: String,
    pub category: String,
    pub inputs: Vec<PinDescriptor>,
    pub outputs: Vec<PinDescriptor>,
    pub params: HashMap<String, Value>,
    #[serde(skip)]
    pub extra: NodeExtra,
}

impl From<&ModuleDescriptor> for NodeData {
    fn from(d: &ModuleDescriptor) -> Self {
        NodeData {
            module_id: d.id.to_string(),
            display_name: d.display_name.to_string(),
            category: d.category.to_string(),
            inputs: d.inputs.clone(),
            outputs: d.outputs.clone(),
            params: HashMap::new(),
            extra: NodeExtra::default(),
        }
    }
}
