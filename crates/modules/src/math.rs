use flexinput_core::{Module, ModuleDescriptor, ModuleRegistration, PinDescriptor, Signal, SignalType};
use smallvec::SmallVec;

use crate::util::get_float;

pub fn registrations() -> Vec<ModuleRegistration> {
    vec![
        reg::<Add>(),
        reg::<Subtract>(),
        reg::<Multiply>(),
        reg::<Divide>(),
        reg::<Abs>(),
        reg::<Negate>(),
        reg::<MapRange>(),
        ModuleRegistration {
            descriptor: Clamp::descriptor(),
            factory: || Box::new(Clamp { min: -1.0, max: 1.0 }),
        },
    ]
}

fn reg<M: Module + Default + 'static>() -> ModuleRegistration {
    ModuleRegistration { descriptor: M::descriptor(), factory: || Box::new(M::default()) }
}

fn out_f(v: f32) -> SmallVec<[Signal; 4]> {
    let mut s = SmallVec::new();
    s.push(Signal::Float(v));
    s
}

// ── Add ───────────────────────────────────────────────────────────────────────

#[derive(Default)]
pub struct Add;
impl Module for Add {
    fn descriptor() -> ModuleDescriptor {
        ModuleDescriptor {
            id: "math.add", display_name: "Add", category: "Math",
            inputs: vec![
                PinDescriptor::new("a", SignalType::Float),
                PinDescriptor::new("b", SignalType::Float),
            ],
            outputs: vec![PinDescriptor::new("out", SignalType::Float)],
        }
    }
    fn process(&mut self, inputs: &[Option<Signal>]) -> SmallVec<[Signal; 4]> {
        out_f(get_float(inputs, 0, 0.0) + get_float(inputs, 1, 0.0))
    }
}

// ── Subtract ─────────────────────────────────────────────────────────────────

#[derive(Default)]
pub struct Subtract;
impl Module for Subtract {
    fn descriptor() -> ModuleDescriptor {
        ModuleDescriptor {
            id: "math.subtract", display_name: "Subtract", category: "Math",
            inputs: vec![
                PinDescriptor::new("a", SignalType::Float),
                PinDescriptor::new("b", SignalType::Float),
            ],
            outputs: vec![PinDescriptor::new("out", SignalType::Float)],
        }
    }
    fn process(&mut self, inputs: &[Option<Signal>]) -> SmallVec<[Signal; 4]> {
        out_f(get_float(inputs, 0, 0.0) - get_float(inputs, 1, 0.0))
    }
}

// ── Multiply ─────────────────────────────────────────────────────────────────

#[derive(Default)]
pub struct Multiply;
impl Module for Multiply {
    fn descriptor() -> ModuleDescriptor {
        ModuleDescriptor {
            id: "math.multiply", display_name: "Multiply", category: "Math",
            inputs: vec![
                PinDescriptor::new("a", SignalType::Float),
                PinDescriptor::new("b", SignalType::Float),
            ],
            outputs: vec![PinDescriptor::new("out", SignalType::Float)],
        }
    }
    fn process(&mut self, inputs: &[Option<Signal>]) -> SmallVec<[Signal; 4]> {
        out_f(get_float(inputs, 0, 0.0) * get_float(inputs, 1, 1.0))
    }
}

// ── Divide ───────────────────────────────────────────────────────────────────

#[derive(Default)]
pub struct Divide;
impl Module for Divide {
    fn descriptor() -> ModuleDescriptor {
        ModuleDescriptor {
            id: "math.divide", display_name: "Divide", category: "Math",
            inputs: vec![
                PinDescriptor::new("a", SignalType::Float),
                PinDescriptor::new("b", SignalType::Float),
            ],
            outputs: vec![PinDescriptor::new("out", SignalType::Float)],
        }
    }
    fn process(&mut self, inputs: &[Option<Signal>]) -> SmallVec<[Signal; 4]> {
        let b = get_float(inputs, 1, 1.0);
        out_f(if b == 0.0 { 0.0 } else { get_float(inputs, 0, 0.0) / b })
    }
}

// ── Abs ───────────────────────────────────────────────────────────────────────

#[derive(Default)]
pub struct Abs;
impl Module for Abs {
    fn descriptor() -> ModuleDescriptor {
        ModuleDescriptor {
            id: "math.abs", display_name: "Abs", category: "Math",
            inputs: vec![PinDescriptor::new("in", SignalType::Float)],
            outputs: vec![PinDescriptor::new("out", SignalType::Float)],
        }
    }
    fn process(&mut self, inputs: &[Option<Signal>]) -> SmallVec<[Signal; 4]> {
        out_f(get_float(inputs, 0, 0.0).abs())
    }
}

// ── Negate ───────────────────────────────────────────────────────────────────

#[derive(Default)]
pub struct Negate;
impl Module for Negate {
    fn descriptor() -> ModuleDescriptor {
        ModuleDescriptor {
            id: "math.negate", display_name: "Negate", category: "Math",
            inputs: vec![PinDescriptor::new("in", SignalType::Float)],
            outputs: vec![PinDescriptor::new("out", SignalType::Float)],
        }
    }
    fn process(&mut self, inputs: &[Option<Signal>]) -> SmallVec<[Signal; 4]> {
        out_f(-get_float(inputs, 0, 0.0))
    }
}

// ── Clamp ────────────────────────────────────────────────────────────────────

pub struct Clamp { pub min: f32, pub max: f32 }
impl Module for Clamp {
    fn descriptor() -> ModuleDescriptor {
        ModuleDescriptor {
            id: "math.clamp", display_name: "Clamp", category: "Math",
            inputs: vec![
                PinDescriptor::new("in",  SignalType::Float),
                PinDescriptor::new("min", SignalType::Float).optional(),
                PinDescriptor::new("max", SignalType::Float).optional(),
            ],
            outputs: vec![PinDescriptor::new("out", SignalType::Float)],
        }
    }
    fn process(&mut self, inputs: &[Option<Signal>]) -> SmallVec<[Signal; 4]> {
        let v   = get_float(inputs, 0, 0.0);
        let min = get_float(inputs, 1, self.min);
        let max = get_float(inputs, 2, self.max);
        out_f(v.clamp(min, max))
    }
}

// ── MapRange ─────────────────────────────────────────────────────────────────

#[derive(Default)]
pub struct MapRange;
impl Module for MapRange {
    fn descriptor() -> ModuleDescriptor {
        ModuleDescriptor {
            id: "math.map_range", display_name: "Map Range", category: "Math",
            inputs: vec![
                PinDescriptor::new("in",      SignalType::Float),
                PinDescriptor::new("in_min",  SignalType::Float).optional(),
                PinDescriptor::new("in_max",  SignalType::Float).optional(),
                PinDescriptor::new("out_min", SignalType::Float).optional(),
                PinDescriptor::new("out_max", SignalType::Float).optional(),
            ],
            outputs: vec![PinDescriptor::new("out", SignalType::Float)],
        }
    }
    fn process(&mut self, inputs: &[Option<Signal>]) -> SmallVec<[Signal; 4]> {
        let v       = get_float(inputs, 0, 0.0);
        let in_min  = get_float(inputs, 1, -1.0);
        let in_max  = get_float(inputs, 2,  1.0);
        let out_min = get_float(inputs, 3, -1.0);
        let out_max = get_float(inputs, 4,  1.0);
        let t = if (in_max - in_min).abs() < f32::EPSILON {
            0.0
        } else {
            (v - in_min) / (in_max - in_min)
        };
        out_f(out_min + t * (out_max - out_min))
    }
}
