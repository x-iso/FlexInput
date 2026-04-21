use flexinput_core::{Module, ModuleDescriptor, ModuleRegistration, PinDescriptor, Signal, SignalType};
use smallvec::SmallVec;

use crate::util::get_bool;

pub fn registrations() -> Vec<ModuleRegistration> {
    vec![reg::<And>(), reg::<Or>(), reg::<Not>(), reg::<Xor>()]
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
