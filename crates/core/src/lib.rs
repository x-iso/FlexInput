pub mod module;
pub mod patch;
pub mod signal;

pub use module::{Module, ModuleDescriptor, ModuleFactory, ModuleRegistration, PinDescriptor};
pub use patch::{NodeInstance, Patch, SubPatch, SubPatchPin, Wire, PATCH_VERSION};
pub use signal::{Signal, SignalType};
