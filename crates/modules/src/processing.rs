use flexinput_core::{Module, ModuleDescriptor, ModuleRegistration, PinDescriptor, Signal, SignalType};
use smallvec::SmallVec;

pub fn registrations() -> Vec<ModuleRegistration> {
    vec![reg::<DelayModule>(), reg::<LowpassModule>(), reg::<ResponseCurveModule>()]
}

fn reg<M: Module + Default + 'static>() -> ModuleRegistration {
    ModuleRegistration { descriptor: M::descriptor(), factory: || Box::new(M::default()) }
}

// ── Delay ─────────────────────────────────────────────────────────────────────

#[derive(Default)]
pub struct DelayModule;

impl Module for DelayModule {
    fn descriptor() -> ModuleDescriptor {
        ModuleDescriptor {
            id: "module.delay",
            display_name: "Delay",
            category: "Processing",
            inputs: vec![PinDescriptor::new("In", SignalType::Float)],
            outputs: vec![PinDescriptor::new("Out", SignalType::Float)],
        }
    }
    fn process(&mut self, _: &[Option<Signal>]) -> SmallVec<[Signal; 4]> { SmallVec::new() }
}

// ── Low-pass Filter ───────────────────────────────────────────────────────────

#[derive(Default)]
pub struct LowpassModule;

impl Module for LowpassModule {
    fn descriptor() -> ModuleDescriptor {
        ModuleDescriptor {
            id: "module.lowpass",
            display_name: "Low-pass",
            category: "Processing",
            inputs: vec![PinDescriptor::new("In", SignalType::Float)],
            outputs: vec![PinDescriptor::new("Out", SignalType::Float)],
        }
    }
    fn process(&mut self, _: &[Option<Signal>]) -> SmallVec<[Signal; 4]> { SmallVec::new() }
}

// ── Response Curve ────────────────────────────────────────────────────────────

#[derive(Default)]
pub struct ResponseCurveModule;

impl Module for ResponseCurveModule {
    fn descriptor() -> ModuleDescriptor {
        ModuleDescriptor {
            id: "module.response_curve",
            display_name: "Response Curve",
            category: "Processing",
            inputs: vec![
                PinDescriptor::new("In 1", SignalType::Float),
                PinDescriptor::new("In 2", SignalType::Float),
                PinDescriptor::new("In 3", SignalType::Float),
            ],
            outputs: vec![
                PinDescriptor::new("Out 1", SignalType::Float),
                PinDescriptor::new("Out 2", SignalType::Float),
                PinDescriptor::new("Out 3", SignalType::Float),
            ],
        }
    }
    fn process(&mut self, _: &[Option<Signal>]) -> SmallVec<[Signal; 4]> { SmallVec::new() }
}
