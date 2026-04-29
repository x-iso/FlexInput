use flexinput_core::{Module, ModuleDescriptor, ModuleRegistration, PinDescriptor, Signal, SignalType};
use smallvec::SmallVec;

pub fn registrations() -> Vec<ModuleRegistration> {
    vec![ModuleRegistration { descriptor: Oscillator::descriptor(), factory: || Box::new(Oscillator) }]
}

pub struct Oscillator;

impl Module for Oscillator {
    fn descriptor() -> ModuleDescriptor {
        ModuleDescriptor {
            id: "generator.oscillator",
            display_name: "Oscillator",
            category: "Generator",
            inputs: vec![
                PinDescriptor::new("freq",   SignalType::Float).optional(),
                PinDescriptor::new("phase",  SignalType::Float).optional(),
                PinDescriptor::new("retrig", SignalType::Bool).optional(),
            ],
            outputs: vec![PinDescriptor::new("out", SignalType::Float)],
        }
    }
    fn process(&mut self, _: &[Option<Signal>]) -> SmallVec<[Signal; 4]> { SmallVec::new() }
}
