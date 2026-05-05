use glam::Vec2;
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

fn out_v(v: Vec2) -> SmallVec<[Signal; 4]> {
    let mut s = SmallVec::new();
    s.push(Signal::Vec2(v));
    s
}

fn has_vec2(inputs: &[Option<Signal>]) -> bool {
    inputs.iter().any(|s| matches!(s, Some(Signal::Vec2(_))))
}

/// Lift a signal slot to Vec2, splatting scalars. Returns `Vec2::splat(default)` for None.
fn to_vec2(sig: Option<Signal>, default: f32) -> Vec2 {
    match sig {
        Some(Signal::Vec2(v)) => v,
        Some(other) => Vec2::splat(other.as_float()),
        None => Vec2::splat(default),
    }
}

// ── Add ───────────────────────────────────────────────────────────────────────

#[derive(Default)]
pub struct Add;
impl Module for Add {
    fn descriptor() -> ModuleDescriptor {
        ModuleDescriptor {
            id: "math.add", display_name: "Add", category: "Math",
            inputs: vec![
                PinDescriptor::new("a", SignalType::Any),
                PinDescriptor::new("b", SignalType::Any),
            ],
            outputs: vec![PinDescriptor::new("out", SignalType::Any)],
        }
    }
    fn process(&mut self, inputs: &[Option<Signal>]) -> SmallVec<[Signal; 4]> {
        if has_vec2(inputs) {
            let sum = (0..inputs.len())
                .map(|i| to_vec2(inputs.get(i).and_then(|s| *s), 0.0))
                .fold(Vec2::ZERO, |acc, v| acc + v);
            out_v(sum)
        } else {
            out_f((0..inputs.len()).map(|i| get_float(inputs, i, 0.0)).sum())
        }
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
                PinDescriptor::new("a", SignalType::Any),
                PinDescriptor::new("b", SignalType::Any),
            ],
            outputs: vec![PinDescriptor::new("out", SignalType::Any)],
        }
    }
    fn process(&mut self, inputs: &[Option<Signal>]) -> SmallVec<[Signal; 4]> {
        if has_vec2(inputs) {
            let first = to_vec2(inputs.get(0).and_then(|s| *s), 0.0);
            let rest = (1..inputs.len())
                .map(|i| to_vec2(inputs.get(i).and_then(|s| *s), 0.0))
                .fold(Vec2::ZERO, |acc, v| acc + v);
            out_v(first - rest)
        } else {
            let first = get_float(inputs, 0, 0.0);
            let rest: f32 = (1..inputs.len()).map(|i| get_float(inputs, i, 0.0)).sum();
            out_f(first - rest)
        }
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
                PinDescriptor::new("a", SignalType::Any),
                PinDescriptor::new("b", SignalType::Any),
            ],
            outputs: vec![PinDescriptor::new("out", SignalType::Any)],
        }
    }
    fn process(&mut self, inputs: &[Option<Signal>]) -> SmallVec<[Signal; 4]> {
        if has_vec2(inputs) {
            let first = to_vec2(inputs.get(0).and_then(|s| *s), 0.0);
            let scale = (1..inputs.len())
                .map(|i| to_vec2(inputs.get(i).and_then(|s| *s), 1.0))
                .fold(Vec2::ONE, |acc, v| acc * v);
            out_v(first * scale)
        } else {
            let first = get_float(inputs, 0, 0.0);
            let rest: f32 = (1..inputs.len()).map(|i| get_float(inputs, i, 1.0)).product();
            out_f(first * rest)
        }
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
                PinDescriptor::new("a", SignalType::Any),
                PinDescriptor::new("b", SignalType::Any),
            ],
            outputs: vec![PinDescriptor::new("out", SignalType::Any)],
        }
    }
    fn process(&mut self, inputs: &[Option<Signal>]) -> SmallVec<[Signal; 4]> {
        if has_vec2(inputs) {
            let mut v = to_vec2(inputs.get(0).and_then(|s| *s), 0.0);
            for i in 1..inputs.len() {
                let d = to_vec2(inputs.get(i).and_then(|s| *s), 1.0);
                v = Vec2::new(
                    if d.x == 0.0 { 0.0 } else { v.x / d.x },
                    if d.y == 0.0 { 0.0 } else { v.y / d.y },
                );
            }
            out_v(v)
        } else {
            let mut v = get_float(inputs, 0, 0.0);
            for i in 1..inputs.len() {
                let d = get_float(inputs, i, 1.0);
                v = if d == 0.0 { 0.0 } else { v / d };
            }
            out_f(v)
        }
    }
}

// ── Abs ───────────────────────────────────────────────────────────────────────

#[derive(Default)]
pub struct Abs;
impl Module for Abs {
    fn descriptor() -> ModuleDescriptor {
        ModuleDescriptor {
            id: "math.abs", display_name: "Abs", category: "Math",
            inputs: vec![PinDescriptor::new("in", SignalType::Any)],
            outputs: vec![PinDescriptor::new("out", SignalType::Any)],
        }
    }
    fn process(&mut self, inputs: &[Option<Signal>]) -> SmallVec<[Signal; 4]> {
        match inputs.get(0).and_then(|s| *s) {
            Some(Signal::Vec2(v)) => out_v(v.abs()),
            other => out_f(other.map(|s| s.as_float()).unwrap_or(0.0).abs()),
        }
    }
}

// ── Negate ───────────────────────────────────────────────────────────────────

#[derive(Default)]
pub struct Negate;
impl Module for Negate {
    fn descriptor() -> ModuleDescriptor {
        ModuleDescriptor {
            id: "math.negate", display_name: "Negate", category: "Math",
            inputs: vec![PinDescriptor::new("in", SignalType::Any)],
            outputs: vec![PinDescriptor::new("out", SignalType::Any)],
        }
    }
    fn process(&mut self, inputs: &[Option<Signal>]) -> SmallVec<[Signal; 4]> {
        match inputs.get(0).and_then(|s| *s) {
            Some(Signal::Vec2(v)) => out_v(-v),
            other => out_f(-other.map(|s| s.as_float()).unwrap_or(0.0)),
        }
    }
}

// ── Clamp ────────────────────────────────────────────────────────────────────

pub struct Clamp { pub min: f32, pub max: f32 }
impl Module for Clamp {
    fn descriptor() -> ModuleDescriptor {
        ModuleDescriptor {
            id: "math.clamp", display_name: "Clamp", category: "Math",
            inputs: vec![
                PinDescriptor::new("in",  SignalType::Any),
                PinDescriptor::new("min", SignalType::Float).optional(),
                PinDescriptor::new("max", SignalType::Float).optional(),
            ],
            outputs: vec![PinDescriptor::new("out", SignalType::Any)],
        }
    }
    fn process(&mut self, inputs: &[Option<Signal>]) -> SmallVec<[Signal; 4]> {
        let min = get_float(inputs, 1, self.min);
        let max = get_float(inputs, 2, self.max);
        match inputs.get(0).and_then(|s| *s) {
            Some(Signal::Vec2(v)) => out_v(v.clamp(Vec2::splat(min), Vec2::splat(max))),
            other => out_f(other.map(|s| s.as_float()).unwrap_or(0.0).clamp(min, max)),
        }
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
                PinDescriptor::new("in",      SignalType::Any),
                PinDescriptor::new("in_min",  SignalType::Float).optional(),
                PinDescriptor::new("in_max",  SignalType::Float).optional(),
                PinDescriptor::new("out_min", SignalType::Float).optional(),
                PinDescriptor::new("out_max", SignalType::Float).optional(),
            ],
            outputs: vec![PinDescriptor::new("out", SignalType::Any)],
        }
    }
    fn process(&mut self, inputs: &[Option<Signal>]) -> SmallVec<[Signal; 4]> {
        let in_min  = get_float(inputs, 1, -1.0);
        let in_max  = get_float(inputs, 2,  1.0);
        let out_min = get_float(inputs, 3, -1.0);
        let out_max = get_float(inputs, 4,  1.0);

        let map = |v: f32| -> f32 {
            let t = if (in_max - in_min).abs() < f32::EPSILON {
                0.0
            } else {
                (v - in_min) / (in_max - in_min)
            };
            out_min + t * (out_max - out_min)
        };

        match inputs.get(0).and_then(|s| *s) {
            Some(Signal::Vec2(v)) => out_v(Vec2::new(map(v.x), map(v.y))),
            other => out_f(map(other.map(|s| s.as_float()).unwrap_or(0.0))),
        }
    }
}
