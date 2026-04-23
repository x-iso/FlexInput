use flexinput_core::{Module, ModuleDescriptor, ModuleRegistration, PinDescriptor, Signal, SignalType};
use smallvec::SmallVec;

pub fn registrations() -> Vec<ModuleRegistration> {
    vec![
        reg::<ReadoutModule>(),
        reg::<OscilloscopeModule>(),
        reg::<VectorscopeModule>(),
    ]
}

fn reg<M: Module + Default + 'static>() -> ModuleRegistration {
    ModuleRegistration { descriptor: M::descriptor(), factory: || Box::new(M::default()) }
}

// ── Value Readout ─────────────────────────────────────────────────────────────

#[derive(Default)]
pub struct ReadoutModule;

impl Module for ReadoutModule {
    fn descriptor() -> ModuleDescriptor {
        ModuleDescriptor {
            id: "display.readout",
            display_name: "Readout",
            category: "Display",
            inputs: vec![PinDescriptor::new("in", SignalType::Float)],
            outputs: vec![],
        }
    }
    fn process(&mut self, _: &[Option<Signal>]) -> SmallVec<[Signal; 4]> { SmallVec::new() }
}

// ── Oscilloscope ──────────────────────────────────────────────────────────────

#[derive(Default)]
pub struct OscilloscopeModule;

impl Module for OscilloscopeModule {
    fn descriptor() -> ModuleDescriptor {
        ModuleDescriptor {
            id: "display.oscilloscope",
            display_name: "Oscilloscope",
            category: "Display",
            inputs: vec![
                PinDescriptor::new("ch1", SignalType::Float),
                PinDescriptor::new("ch2", SignalType::Float),
                PinDescriptor::new("ch3", SignalType::Float),
                PinDescriptor::new("ch4", SignalType::Float),
            ],
            outputs: vec![],
        }
    }
    fn process(&mut self, _: &[Option<Signal>]) -> SmallVec<[Signal; 4]> { SmallVec::new() }
}

// ── Vectorscope ───────────────────────────────────────────────────────────────

#[derive(Default)]
pub struct VectorscopeModule;

impl Module for VectorscopeModule {
    fn descriptor() -> ModuleDescriptor {
        ModuleDescriptor {
            id: "display.vectorscope",
            display_name: "Vectorscope",
            category: "Display",
            inputs: vec![
                PinDescriptor::new("x", SignalType::Float),
                PinDescriptor::new("y", SignalType::Float),
            ],
            outputs: vec![],
        }
    }
    fn process(&mut self, _: &[Option<Signal>]) -> SmallVec<[Signal; 4]> { SmallVec::new() }
}
