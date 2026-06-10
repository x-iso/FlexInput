use flexinput_core::{Module, ModuleDescriptor, ModuleRegistration, PinDescriptor, Signal, SignalType};
use smallvec::SmallVec;

pub fn registrations() -> Vec<ModuleRegistration> {
    vec![
        ModuleRegistration { descriptor: Oscillator::descriptor(),         factory: || Box::new(Oscillator) },
        ModuleRegistration { descriptor: EnvelopeGenerator::descriptor(),  factory: || Box::new(EnvelopeGenerator) },
    ]
}

pub struct Oscillator;
pub struct EnvelopeGenerator;

impl Module for EnvelopeGenerator {
    fn descriptor() -> ModuleDescriptor {
        ModuleDescriptor {
            id: "generator.envelope",
            display_name: "Envelope",
            category: "Generator",
            inputs: vec![
                PinDescriptor::new("Trigger", SignalType::Bool).optional(),
                PinDescriptor::new("Time",    SignalType::Float).optional(),
            ],
            outputs: vec![PinDescriptor::new("Out", SignalType::Float)],
        }
    }
    fn process(&mut self, _: &[Option<Signal>]) -> SmallVec<[Signal; 4]> { SmallVec::new() }
}

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
