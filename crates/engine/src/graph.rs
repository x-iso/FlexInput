use std::collections::HashMap;
use serde_json::Value;

/// Topologically-sorted, egui-free snapshot of the signal graph.
/// Rebuilt by the UI thread every frame so the processing thread always has current params.
#[derive(Clone, Default)]
pub struct ProcessingGraph {
    pub nodes: Vec<NodeSnap>,
}

/// Inner graph for a sub-patch node, produced by recursively building the inner snarl.
#[derive(Clone, Default)]
pub struct InlineSubgraph {
    pub graph: ProcessingGraph,
    /// For each outer output pin k: (inner flat node index, output pin 0) of the outlet node.
    pub outlet_locs: Vec<Option<(usize, usize)>>,
}

/// A virtual feedback source feeding a physical device's haptic inputs, plus the
/// per-device rumble shaping configured on that virtual device's sink node. The
/// shaping applies ONLY to AutoMap feedback (game/app rumble forwarded back to a
/// physical pad) — the user's own direct rumble wiring is never reshaped.
#[derive(Clone)]
pub struct FeedbackSource {
    /// The virtual device id whose output (rumble/lightbar) flows back.
    pub device_id: String,
    /// HD voice-coil amplitude floor: the min output for any non-zero feedback
    /// (lifts faint game rumble to a perceptible level). 0 = no floor.
    pub rumble_floor: f32,
    /// HD voice-coil amplitude ceiling: output is remapped into floor..max.
    pub rumble_max: f32,
    /// Low-end boost exponent for the floor..max remap (`v^exp`); <1 boosts.
    pub rumble_exp: f32,
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
    /// source_device_id may be "collector:{uid}" for AutoMap Collector nodes.
    pub automap_source: Option<(String, Vec<String>)>,
    /// When automap_source is a collector, this holds the upstream real device ID
    /// used as fallback for pins the collector does not override.
    pub automap_fallback_dev: Option<String>,
    /// Virtual sink devices that auto-map FROM this physical device, used to pull
    /// feedback signals (rumble, lightbar) backward along the AutoMap wire.
    /// When this device is itself a sink (has haptic inputs), the engine looks up
    /// matching feedback signals (per FEEDBACK_PAIRS) from these virtual devices.
    /// Each carries the source's per-device rumble shaping (floor/max/exp).
    pub feedback_sources: Vec<FeedbackSource>,
    /// True for device.source nodes whose feedback-input wires loop back to
    /// their own source-half (directly or through Splitter/Math chain). Eval
    /// defers the multi_sources read to a post-pass so the chain has produced
    /// values by the time we read them.
    pub is_self_sink: bool,
    /// When true, the AutoMap digital→analog trigger bridge is allowed for this
    /// sink: a pressed `btn_lt_dig`/`btn_rt_dig` may drive the virtual analog
    /// `left_trigger`/`right_trigger` as a lowest-priority fallback. Set from the
    /// upstream physical source's `digital_triggers` opt-in (always on for
    /// digital-only pads like Switch Pro). Default false.
    pub digital_trigger_bridge: bool,
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
    /// Populated for subpatch nodes; contains the recursively-built inner graph.
    pub inline_subgraph: Option<Box<InlineSubgraph>>,
}
