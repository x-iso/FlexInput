use flexinput_core::{Module, ModuleDescriptor, ModuleRegistration, PinDescriptor, Signal, SignalType};
use smallvec::SmallVec;

pub fn registrations() -> Vec<ModuleRegistration> {
    vec![
        reg::<ReadoutModule>(),
        reg::<OscilloscopeModule>(),
        reg::<VectorscopeModule>(),
        reg::<TriggerScopeModule>(),
        reg::<Controller3DDisplayModule>(),
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

// ── Trigger Scope ─────────────────────────────────────────────────────────────

#[derive(Default)]
pub struct TriggerScopeModule;

impl Module for TriggerScopeModule {
    fn descriptor() -> ModuleDescriptor {
        ModuleDescriptor {
            id: "display.trigscope",
            display_name: "Trigger Scope",
            category: "Display",
            inputs: vec![
                PinDescriptor::new("trig", SignalType::Float),
                PinDescriptor::new("ch1",  SignalType::Float),
                PinDescriptor::new("ch2",  SignalType::Float),
                PinDescriptor::new("ch3",  SignalType::Float),
                PinDescriptor::new("ch4",  SignalType::Float),
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
            inputs: vec![PinDescriptor::new("ch1", SignalType::Vec2)],
            outputs: vec![],
        }
    }
    fn process(&mut self, _: &[Option<Signal>]) -> SmallVec<[Signal; 4]> { SmallVec::new() }
}

// ── Controller 3D Display ───────────────────────────────────────────────────────

/// Display module that renders an interactive 3D controller model with gyro-driven orientation.
#[derive(Default)]
pub struct Controller3DDisplayModule;

impl Module for Controller3DDisplayModule {
    fn descriptor() -> ModuleDescriptor {
        ModuleDescriptor {
            id: "display.controller3d",
            display_name: "Controller 3D",
            category: "Display",
            inputs: vec![
                // AutoMap device — used to auto-detect which controller model to
                // show (and reserved for reading its gyro directly later).
                PinDescriptor::new("Device", SignalType::AutoMap).optional(),
                // Quaternion pose (x,y,z,w) from the Gyro 3DOF module's
                // "Orientation" output; rotates the rendered model.
                PinDescriptor::new("Orientation", SignalType::Vec4).optional(),
            ],
            outputs: vec![],
        }
    }
    fn process(&mut self, _: &[Option<Signal>]) -> SmallVec<[Signal; 4]> { SmallVec::new() }
}
