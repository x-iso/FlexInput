use std::collections::HashMap;

use flexinput_core::{Module, Patch, Signal};
use uuid::Uuid;

pub mod router;

pub use router::InputRouter;

pub struct Engine {
    modules: HashMap<Uuid, Box<dyn Module>>,
    patch: Patch,
    router: InputRouter,
    /// When true, virtual output nodes replay their last value while the graph
    /// is being edited live (overlay tweak mode).
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
        // TODO: instantiate modules via the module registry
    }

    pub fn patch(&self) -> &Patch {
        &self.patch
    }

    pub fn router_mut(&mut self) -> &mut InputRouter {
        &mut self.router
    }

    /// Execute one tick of the signal graph in topological order.
    pub fn tick(&mut self) {
        // TODO: topological sort + propagate signals
    }
}

impl Default for Engine {
    fn default() -> Self {
        Self::new()
    }
}
