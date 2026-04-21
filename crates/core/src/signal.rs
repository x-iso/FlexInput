use glam::Vec2;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", content = "value")]
pub enum Signal {
    Float(f32),
    Bool(bool),
    Vec2(Vec2),
    Int(i32),
}

impl Signal {
    pub fn signal_type(self) -> SignalType {
        match self {
            Signal::Float(_) => SignalType::Float,
            Signal::Bool(_) => SignalType::Bool,
            Signal::Vec2(_) => SignalType::Vec2,
            Signal::Int(_) => SignalType::Int,
        }
    }

    /// Convert to a compatible type, or None if incompatible.
    pub fn coerce_to(self, target: SignalType) -> Option<Signal> {
        match (self, target) {
            (s, t) if s.signal_type() == t => Some(s),
            (Signal::Bool(b), SignalType::Float) => Some(Signal::Float(if b { 1.0 } else { 0.0 })),
            (Signal::Float(f), SignalType::Bool) => Some(Signal::Bool(f != 0.0)),
            (Signal::Int(i), SignalType::Float) => Some(Signal::Float(i as f32)),
            (Signal::Float(f), SignalType::Int) => Some(Signal::Int(f as i32)),
            _ => None,
        }
    }

    pub fn as_float(self) -> f32 {
        match self.coerce_to(SignalType::Float) {
            Some(Signal::Float(f)) => f,
            _ => 0.0,
        }
    }

    pub fn as_bool(self) -> bool {
        match self.coerce_to(SignalType::Bool) {
            Some(Signal::Bool(b)) => b,
            _ => false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum SignalType {
    Float,
    Bool,
    Vec2,
    Int,
    /// Accepts any incoming type — used for pass-through, selector, and switch modules.
    Any,
}

impl SignalType {
    pub fn accepts(self, incoming: SignalType) -> bool {
        self == SignalType::Any
            || self == incoming
            || matches!(
                (self, incoming),
                (SignalType::Float, SignalType::Bool)
                    | (SignalType::Bool, SignalType::Float)
                    | (SignalType::Float, SignalType::Int)
                    | (SignalType::Int, SignalType::Float)
            )
    }

    pub fn display_name(self) -> &'static str {
        match self {
            SignalType::Float => "Float",
            SignalType::Bool  => "Bool",
            SignalType::Vec2  => "Vec2",
            SignalType::Int   => "Int",
            SignalType::Any   => "Any",
        }
    }

    /// Suggested wire color for the UI.
    pub fn color_rgb(self) -> [u8; 3] {
        match self {
            SignalType::Float => [100, 180, 255],
            SignalType::Bool  => [255, 120, 80],
            SignalType::Vec2  => [120, 220, 140],
            SignalType::Int   => [200, 160, 255],
            SignalType::Any   => [180, 180, 180],
        }
    }
}
