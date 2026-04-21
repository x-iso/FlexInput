use flexinput_core::{Module, ModuleDescriptor, ModuleRegistration, PinDescriptor, Signal, SignalType};
use smallvec::SmallVec;

pub fn registrations() -> Vec<ModuleRegistration> {
    vec![
        reg::<ConstantModule>(),
        reg::<SwitchModule>(),
        reg::<KnobModule>(),
        reg::<SelectorModule>(),
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

/// Routes `in_0` or `in_1` to `out` depending on the Bool `select` input.
/// select=false → in_0, select=true → in_1.
#[derive(Default)]
pub struct SelectorModule;

impl Module for SelectorModule {
    fn descriptor() -> ModuleDescriptor {
        ModuleDescriptor {
            id: "module.selector",
            display_name: "Selector",
            category: "Utility",
            inputs: vec![
                PinDescriptor::new("select", SignalType::Bool),
                PinDescriptor::new("in_0",   SignalType::Float),
                PinDescriptor::new("in_1",   SignalType::Float),
            ],
            outputs: vec![PinDescriptor::new("out", SignalType::Float)],
        }
    }
    fn process(&mut self, inputs: &[Option<Signal>]) -> SmallVec<[Signal; 4]> {
        let sel = inputs.first().and_then(|s| *s)
            .and_then(|s| if let Signal::Bool(b) = s { Some(b) } else { None })
            .unwrap_or(false);
        let v = if sel { inputs.get(2) } else { inputs.get(1) };
        match v.and_then(|s| *s) {
            Some(sig) => { let mut r = SmallVec::new(); r.push(sig); r }
            None => SmallVec::new(),
        }
    }
}
