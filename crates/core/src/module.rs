use serde::{Deserialize, Serialize};
use smallvec::SmallVec;

use crate::signal::{Signal, SignalType};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PinDescriptor {
    pub name: String,
    pub signal_type: SignalType,
    #[serde(default)]
    pub optional: bool,
}

impl PinDescriptor {
    pub fn new(name: impl Into<String>, signal_type: SignalType) -> Self {
        Self { name: name.into(), signal_type, optional: false }
    }

    pub fn optional(mut self) -> Self {
        self.optional = true;
        self
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModuleDescriptor {
    /// Stable dot-namespaced ID, e.g. "math.multiply". Never rename once shipped.
    pub id: &'static str,
    pub display_name: &'static str,
    pub category: &'static str,
    pub inputs: Vec<PinDescriptor>,
    pub outputs: Vec<PinDescriptor>,
}

pub trait Module: Send + 'static {
    fn descriptor() -> ModuleDescriptor
    where
        Self: Sized;

    fn process(&mut self, inputs: &[Option<Signal>]) -> SmallVec<[Signal; 4]>;

    /// True for modules that contribute a widget to the overlay window
    /// (oscilloscope, vectorscope, gamepad visualiser, etc.).
    /// Rendering happens in the UI crate; core stays egui-free.
    fn has_overlay_widget(&self) -> bool {
        false
    }
}

/// Type-erased factory so the registry can instantiate modules without generics.
pub type ModuleFactory = fn() -> Box<dyn Module>;

pub struct ModuleRegistration {
    pub descriptor: ModuleDescriptor,
    pub factory: ModuleFactory,
}
