use std::collections::HashMap;

use flexinput_core::{Module, Patch, Signal};
use uuid::Uuid;

pub mod eval;
pub mod graph;
pub mod router;
pub mod state;
pub mod thread;

pub use eval::{
    apply_curve, biases_from_params, curve_points_from_params, curve_points_from_params_keyed,
    curve_scale, curve_scale_inv, envelope_flags, eval_graph_tick, eval_pure, get_b, get_f,
    crossover_hz_to_pos, multiband_collapse_band, multiband_collapse_carrier, namespaced_uid,
    osc_sample, pin_is_analog_input, read_scale_t, sample_curve, sig_to_f32,
    vec_reshape_apply, VEC_RESHAPE_BOUNDARY_DEFAULT, VEC_RESHAPE_GAIN_DEFAULT,
};
pub use graph::{FeedbackSource, InlineSubgraph, NodeSnap, ProcessingGraph};
pub use router::InputRouter;
pub use state::NodeState;
pub use thread::{spawn_processing_thread, current_sample_rate, current_io_rate, set_io_rate, new_device_rates, new_scope_taps, new_arc_graph, new_arc_signals, ArcGraph, ArcSignals, DeviceRates, ProcessingOutput, ScopeTaps, ScopeTapRing, SinkBus, DEFAULT_SAMPLE_RATE, SCOPE_TAP_PINS, SCOPE_TAP_RETAIN_MS, SCOPE_TAP_MAX_LEN};

pub struct Engine {
    modules: HashMap<Uuid, Box<dyn Module>>,
    patch: Patch,
    router: InputRouter,
    pub pass_through_outputs: bool,
    last_outputs: HashMap<(Uuid, String), Signal>,
}

impl Engine {
    pub fn new() -> Self {
        Self {
            modules: HashMap::new(),
            patch: Patch::default(),
            router: InputRouter::new(),
            pass_through_outputs: false,
            last_outputs: HashMap::new(),
        }
    }

    pub fn load_patch(&mut self, patch: Patch) {
        self.patch = patch;
        self.modules.clear();
    }

    pub fn patch(&self) -> &Patch { &self.patch }
    pub fn router_mut(&mut self) -> &mut InputRouter { &mut self.router }

    pub fn tick(&mut self) {}
}

impl Default for Engine {
    fn default() -> Self { Self::new() }
}
