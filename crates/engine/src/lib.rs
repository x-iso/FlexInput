use std::collections::HashMap;

use flexinput_core::{Module, Patch, Signal};
use uuid::Uuid;

pub mod eval;
pub mod graph;
pub mod router;
pub mod state;
pub mod thread;

pub use eval::{
    apply_curve, biases_from_params, curve_points_from_params, curve_scale, curve_scale_inv,
    eval_graph_tick, eval_pure, get_b, get_f, osc_sample, read_scale_t, sample_curve, sig_to_f32,
};
pub use graph::{NodeSnap, ProcessingGraph};
pub use router::InputRouter;
pub use state::NodeState;
pub use thread::{spawn_processing_thread, ProcessingOutput, SinkBus, SAMPLE_RATE};

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
