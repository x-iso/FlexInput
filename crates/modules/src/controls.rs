use flexinput_core::{Module, ModuleDescriptor, ModuleRegistration, PinDescriptor, Signal, SignalType};
use smallvec::SmallVec;

use crate::util::get_float;

pub fn registrations() -> Vec<ModuleRegistration> {
    vec![
        reg::<ConstantModule>(),
        reg::<SwitchModule>(),
        reg::<KnobModule>(),
        reg::<SelectorModule>(),
        ModuleRegistration {
            descriptor: SplitModule::descriptor(),
            factory: || Box::new(SplitModule::default()),
        },
    ]
}

fn reg<M: Module + Default + 'static>() -> ModuleRegistration {
    ModuleRegistration { descriptor: M::descriptor(), factory: || Box::new(M::default()) }
}

// ── Constant ─────────────────────────────────────────────────────────────────

/// Outputs a fixed Float value set via the body UI.
#[derive(Default)]
pub struct ConstantModule;

impl Module for ConstantModule {
    fn descriptor() -> ModuleDescriptor {
        ModuleDescriptor {
            id: "module.constant",
            display_name: "Constant",
            category: "Utility",
            inputs: vec![],
            outputs: vec![PinDescriptor::new("out", SignalType::Float)],
        }
    }
    fn process(&mut self, _inputs: &[Option<Signal>]) -> SmallVec<[Signal; 4]> {
        // Value resolved from params by the router; this path only runs in the engine.
        SmallVec::new()
    }
}

// ── Switch ────────────────────────────────────────────────────────────────────

/// Toggle that outputs a Bool (true/false) set via a checkbox in the body UI.
#[derive(Default)]
pub struct SwitchModule;

impl Module for SwitchModule {
    fn descriptor() -> ModuleDescriptor {
        ModuleDescriptor {
            id: "module.switch",
            display_name: "Switch",
            category: "Utility",
            inputs: vec![],
            outputs: vec![PinDescriptor::new("out", SignalType::Bool)],
        }
    }
    fn process(&mut self, _inputs: &[Option<Signal>]) -> SmallVec<[Signal; 4]> {
        SmallVec::new()
    }
}

// ── Knob ──────────────────────────────────────────────────────────────────────

/// Outputs a Float in [0, 1] set via a slider in the body UI.
#[derive(Default)]
pub struct KnobModule;

impl Module for KnobModule {
    fn descriptor() -> ModuleDescriptor {
        ModuleDescriptor {
            id: "module.knob",
            display_name: "Knob",
            category: "Utility",
            inputs: vec![],
            outputs: vec![PinDescriptor::new("out", SignalType::Float)],
        }
    }
    fn process(&mut self, _inputs: &[Option<Signal>]) -> SmallVec<[Signal; 4]> {
        SmallVec::new()
    }
}

// ── Selector ──────────────────────────────────────────────────────────────────

/// Routes one of N value inputs to `out` based on `select` (Float 0..1, quantized to N slots).
#[derive(Default)]
pub struct SelectorModule;

impl Module for SelectorModule {
    fn descriptor() -> ModuleDescriptor {
        ModuleDescriptor {
            id: "module.selector",
            display_name: "Selector",
            category: "Utility",
            inputs: vec![
                PinDescriptor::new("select", SignalType::Float),
                PinDescriptor::new("in_0",   SignalType::Float),
                PinDescriptor::new("in_1",   SignalType::Float),
            ],
            outputs: vec![PinDescriptor::new("out", SignalType::Float)],
        }
    }
    fn process(&mut self, inputs: &[Option<Signal>]) -> SmallVec<[Signal; 4]> {
        if inputs.len() < 2 { return SmallVec::new(); }
        let n = (inputs.len() - 1) as f32;
        let sel = get_float(inputs, 0, 0.0);
        let idx = (sel.clamp(0.0, 1.0) * n).floor() as usize;
        let idx = idx.min(inputs.len() - 2);
        match inputs.get(idx + 1).and_then(|s| *s) {
            Some(sig) => { let mut r = SmallVec::new(); r.push(sig); r }
            None => SmallVec::new(),
        }
    }
}

// ── Split ─────────────────────────────────────────────────────────────────────

/// Routes `in` to one of N outputs based on `select` (Float 0..1); unselected outputs emit 0.0.
pub struct SplitModule {
    pub n_outputs: usize,
}

impl Default for SplitModule {
    fn default() -> Self { Self { n_outputs: 2 } }
}

impl Module for SplitModule {
    fn descriptor() -> ModuleDescriptor {
        ModuleDescriptor {
            id: "module.split",
            display_name: "Split",
            category: "Utility",
            inputs: vec![
                PinDescriptor::new("select", SignalType::Float),
                PinDescriptor::new("in",     SignalType::Float),
            ],
            outputs: vec![
                PinDescriptor::new("out_0", SignalType::Float),
                PinDescriptor::new("out_1", SignalType::Float),
            ],
        }
    }
    fn process(&mut self, inputs: &[Option<Signal>]) -> SmallVec<[Signal; 4]> {
        let n = self.n_outputs.max(1);
        let sel = get_float(inputs, 0, 0.0);
        let val = get_float(inputs, 1, 0.0);
        let idx = (sel.clamp(0.0, 1.0) * n as f32).floor() as usize;
        let idx = idx.min(n - 1);
        let mut r = SmallVec::new();
        for i in 0..n {
            r.push(Signal::Float(if i == idx { val } else { 0.0 }));
        }
        r
    }
}
