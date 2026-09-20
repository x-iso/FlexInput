//! JSM Config — a JoyShockMapper config text applied to the AutoMap bus.
//!
//! The node is a plain inline AutoMap pass-through as far as the graph is
//! concerned: the config text lives in its params (one entry per tab) and the
//! engine (`eval/modules/jsm`) republishes the bus with the config applied.
//! Optional module, like Audio Stream Haptics: with the `jsm` feature off the
//! descriptor isn't registered and the palette doesn't offer it.

use flexinput_core::{Module, ModuleDescriptor, ModuleRegistration, PinDescriptor, Signal, SignalType};
use smallvec::SmallVec;

pub fn registrations() -> Vec<ModuleRegistration> {
    #[allow(unused_mut)]
    let mut v: Vec<ModuleRegistration> = Vec::new();
    #[cfg(feature = "jsm")]
    v.push(ModuleRegistration {
        descriptor: JsmModule::descriptor(),
        factory: || Box::new(JsmModule),
    });
    v
}

#[cfg(feature = "jsm")]
#[derive(Default)]
pub struct JsmModule;

#[cfg(feature = "jsm")]
impl Module for JsmModule {
    fn descriptor() -> ModuleDescriptor {
        ModuleDescriptor {
            id: "module.jsm",
            display_name: "JSM Config",
            category: "AutoMap",
            inputs: vec![PinDescriptor::new("Device", SignalType::AutoMap)],
            outputs: vec![PinDescriptor::new("AutoMap", SignalType::AutoMap)],
        }
    }
    /// Nothing to compute here: the engine's JSM evaluator publishes the bus.
    fn process(&mut self, _: &[Option<Signal>]) -> SmallVec<[Signal; 4]> { SmallVec::new() }
}

#[cfg(test)]
mod tests {
    // The module is present exactly when its feature is on.
    #[test]
    fn registration_follows_the_feature() {
        let has = super::registrations().iter().any(|r| r.descriptor.id == "module.jsm");
        assert_eq!(has, cfg!(feature = "jsm"));
    }
}
