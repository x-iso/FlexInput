use flexinput_core::{Module, ModuleDescriptor, ModuleRegistration, PinDescriptor, Signal, SignalType};
use smallvec::SmallVec;

use crate::util::{get_bool, get_float};

pub fn registrations() -> Vec<ModuleRegistration> {
    vec![
        reg::<And>(), reg::<Or>(), reg::<Not>(), reg::<Xor>(),
        reg::<Equal>(), reg::<NotEqual>(), reg::<GreaterThan>(), reg::<LessThan>(),
        reg::<HasChanged>(), reg::<LogicDelay>(), reg::<Counter>(),
    ]
}

fn reg<M: Module + Default>() -> ModuleRegistration {
    ModuleRegistration { descriptor: M::descriptor(), factory: || Box::new(M::default()) }
}

fn out_b(v: bool) -> SmallVec<[Signal; 4]> {
    let mut s = SmallVec::new();
    s.push(Signal::Bool(v));
    s
}

// ── And ───────────────────────────────────────────────────────────────────────

#[derive(Default)]
pub struct And;
impl Module for And {
    fn descriptor() -> ModuleDescriptor {
        ModuleDescriptor {
            id: "logic.and", display_name: "AND", category: "Logic",
            inputs: vec![
                PinDescriptor::new("a", SignalType::Bool),
                PinDescriptor::new("b", SignalType::Bool),
            ],
            outputs: vec![PinDescriptor::new("out", SignalType::Bool)],
        }
    }
    fn process(&mut self, inputs: &[Option<Signal>]) -> SmallVec<[Signal; 4]> {
        out_b(get_bool(inputs, 0, false) && get_bool(inputs, 1, false))
    }
}

// ── Or ────────────────────────────────────────────────────────────────────────

#[derive(Default)]
pub struct Or;
impl Module for Or {
    fn descriptor() -> ModuleDescriptor {
        ModuleDescriptor {
            id: "logic.or", display_name: "OR", category: "Logic",
            inputs: vec![
                PinDescriptor::new("a", SignalType::Bool),
                PinDescriptor::new("b", SignalType::Bool),
            ],
            outputs: vec![PinDescriptor::new("out", SignalType::Bool)],
        }
    }
    fn process(&mut self, inputs: &[Option<Signal>]) -> SmallVec<[Signal; 4]> {
        out_b(get_bool(inputs, 0, false) || get_bool(inputs, 1, false))
    }
}

// ── Not ───────────────────────────────────────────────────────────────────────

#[derive(Default)]
pub struct Not;
impl Module for Not {
    fn descriptor() -> ModuleDescriptor {
        ModuleDescriptor {
            id: "logic.not", display_name: "NOT", category: "Logic",
            inputs: vec![PinDescriptor::new("in", SignalType::Bool)],
            outputs: vec![PinDescriptor::new("out", SignalType::Bool)],
        }
    }
    fn process(&mut self, inputs: &[Option<Signal>]) -> SmallVec<[Signal; 4]> {
        out_b(!get_bool(inputs, 0, false))
    }
}

// ── Xor ───────────────────────────────────────────────────────────────────────

#[derive(Default)]
pub struct Xor;
impl Module for Xor {
    fn descriptor() -> ModuleDescriptor {
        ModuleDescriptor {
            id: "logic.xor", display_name: "XOR", category: "Logic",
            inputs: vec![
                PinDescriptor::new("a", SignalType::Bool),
                PinDescriptor::new("b", SignalType::Bool),
            ],
            outputs: vec![PinDescriptor::new("out", SignalType::Bool)],
        }
    }
    fn process(&mut self, inputs: &[Option<Signal>]) -> SmallVec<[Signal; 4]> {
        out_b(get_bool(inputs, 0, false) ^ get_bool(inputs, 1, false))
    }
}

// ── Equal ─────────────────────────────────────────────────────────────────────

#[derive(Default)]
pub struct Equal;
impl Module for Equal {
    fn descriptor() -> ModuleDescriptor {
        ModuleDescriptor {
            id: "logic.equal", display_name: "Equal", category: "Logic",
            inputs: vec![
                PinDescriptor::new("a", SignalType::Float),
                PinDescriptor::new("b", SignalType::Float),
            ],
            outputs: vec![PinDescriptor::new("out", SignalType::Bool)],
        }
    }
    fn process(&mut self, inputs: &[Option<Signal>]) -> SmallVec<[Signal; 4]> {
        out_b(get_float(inputs, 0, 0.0) == get_float(inputs, 1, 0.0))
    }
}

// ── NotEqual ──────────────────────────────────────────────────────────────────

#[derive(Default)]
pub struct NotEqual;
impl Module for NotEqual {
    fn descriptor() -> ModuleDescriptor {
        ModuleDescriptor {
            id: "logic.not_equal", display_name: "Not Equal", category: "Logic",
            inputs: vec![
                PinDescriptor::new("a", SignalType::Float),
                PinDescriptor::new("b", SignalType::Float),
            ],
            outputs: vec![PinDescriptor::new("out", SignalType::Bool)],
        }
    }
    fn process(&mut self, inputs: &[Option<Signal>]) -> SmallVec<[Signal; 4]> {
        out_b(get_float(inputs, 0, 0.0) != get_float(inputs, 1, 0.0))
    }
}

// ── GreaterThan ───────────────────────────────────────────────────────────────

#[derive(Default)]
pub struct GreaterThan;
impl Module for GreaterThan {
    fn descriptor() -> ModuleDescriptor {
        ModuleDescriptor {
            id: "logic.greater_than", display_name: "Greater Than", category: "Logic",
            inputs: vec![
                PinDescriptor::new("a", SignalType::Float),
                PinDescriptor::new("b", SignalType::Float),
            ],
            outputs: vec![PinDescriptor::new("out", SignalType::Bool)],
        }
    }
    fn process(&mut self, inputs: &[Option<Signal>]) -> SmallVec<[Signal; 4]> {
        out_b(get_float(inputs, 0, 0.0) > get_float(inputs, 1, 0.0))
    }
}

// ── LessThan ──────────────────────────────────────────────────────────────────

#[derive(Default)]
pub struct LessThan;
impl Module for LessThan {
    fn descriptor() -> ModuleDescriptor {
        ModuleDescriptor {
            id: "logic.less_than", display_name: "Less Than", category: "Logic",
            inputs: vec![
                PinDescriptor::new("a", SignalType::Float),
                PinDescriptor::new("b", SignalType::Float),
            ],
            outputs: vec![PinDescriptor::new("out", SignalType::Bool)],
        }
    }
    fn process(&mut self, inputs: &[Option<Signal>]) -> SmallVec<[Signal; 4]> {
        out_b(get_float(inputs, 0, 0.0) < get_float(inputs, 1, 0.0))
    }
}

// ── LogicDelay ────────────────────────────────────────────────────────────────

/// Delays a Bool signal edge by a configurable time.
/// "delay_true"  — only output true after input has been true for the full duration (debounce).
/// "delay_false" — hold true for the full duration after input goes false (extend).
#[derive(Default)]
pub struct LogicDelay;
impl Module for LogicDelay {
    fn descriptor() -> ModuleDescriptor {
        ModuleDescriptor {
            id: "logic.delay", display_name: "Logic Delay", category: "Logic",
            inputs:  vec![PinDescriptor::new("in",  SignalType::Bool)],
            outputs: vec![PinDescriptor::new("out", SignalType::Bool)],
        }
    }
    fn process(&mut self, _: &[Option<Signal>]) -> SmallVec<[Signal; 4]> { SmallVec::new() }
}

// ── HasChanged ────────────────────────────────────────────────────────────────

#[derive(Default)]
pub struct HasChanged;
impl Module for HasChanged {
    fn descriptor() -> ModuleDescriptor {
        ModuleDescriptor {
            id: "logic.has_changed", display_name: "Has Changed", category: "Logic",
            inputs: vec![PinDescriptor::new("in", SignalType::Any)],
            outputs: vec![
                PinDescriptor::new("changed",   SignalType::Bool),
                PinDescriptor::new("increased", SignalType::Bool),
                PinDescriptor::new("decreased", SignalType::Bool),
            ],
        }
    }
    // Stateful — real output computed by update_stateful_nodes in the UI crate.
    fn process(&mut self, _: &[Option<Signal>]) -> SmallVec<[Signal; 4]> { SmallVec::new() }
}

// ── Counter ───────────────────────────────────────────────────────────────────

#[derive(Default)]
pub struct Counter;
impl Module for Counter {
    fn descriptor() -> ModuleDescriptor {
        ModuleDescriptor {
            id: "logic.counter", display_name: "Counter", category: "Logic",
            inputs: vec![
                PinDescriptor::new("increase", SignalType::Bool).optional(),
                PinDescriptor::new("decrease", SignalType::Bool).optional(),
                PinDescriptor::new("reset",    SignalType::Bool).optional(),
                PinDescriptor::new("step",     SignalType::Float).optional(),
                PinDescriptor::new("min",      SignalType::Float).optional(),
                PinDescriptor::new("max",      SignalType::Float).optional(),
            ],
            outputs: vec![PinDescriptor::new("out", SignalType::Float)],
        }
    }
    fn process(&mut self, _: &[Option<Signal>]) -> SmallVec<[Signal; 4]> { SmallVec::new() }
}
