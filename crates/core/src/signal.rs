use glam::{Vec2, Vec4};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", content = "value")]
pub enum Signal {
    Float(f32),
    Bool(bool),
    Vec2(Vec2),
    Vec4(Vec4),
    Int(i32),
}

impl Signal {
    pub fn signal_type(self) -> SignalType {
        match self {
            Signal::Float(_) => SignalType::Float,
            Signal::Bool(_) => SignalType::Bool,
            Signal::Vec2(_) => SignalType::Vec2,
            Signal::Vec4(_) => SignalType::Vec4,
            Signal::Int(_) => SignalType::Int,
        }
    }

    /// Convert to a compatible type, or None if incompatible.
    pub fn coerce_to(self, target: SignalType) -> Option<Signal> {
        match (self, target) {
            (s, t) if s.signal_type() == t => Some(s),
            (Signal::Bool(b), SignalType::Float) => Some(Signal::Float(if b { 1.0 } else { 0.0 })),
            (Signal::Float(f), SignalType::Bool) => Some(Signal::Bool(f >= 0.5)),
            (Signal::Int(i), SignalType::Float) => Some(Signal::Float(i as f32)),
            (Signal::Float(f), SignalType::Int) => Some(Signal::Int(f as i32)),
            (Signal::Vec4(v), SignalType::Float) => Some(Signal::Float(v.x)),
            _ => None,
        }
    }

    pub fn as_float(self) -> f32 {
        match self.coerce_to(SignalType::Float) {
            Some(Signal::Float(f)) => f,
            _ => 0.0,
        }
    }

    pub fn as_vec4(self) -> Vec4 {
        match self.coerce_to(SignalType::Vec4) {
            Some(Signal::Vec4(v)) => v,
            _ => Vec4::ZERO,
        }
    }

    pub fn as_bool(self) -> bool {
        match self.coerce_to(SignalType::Bool) {
            Some(Signal::Bool(b)) => b,
            _ => false,
        }
    }

    /// The rest/idle value of this signal's own type. Used to MUTE a pin
    /// without dropping it from a signal map — consumers that probe for a
    /// pin's presence (`contains_key`) keep seeing the device's real shape,
    /// they just read it as unpressed / centered.
    pub fn zeroed(self) -> Signal {
        match self {
            Signal::Float(_) => Signal::Float(0.0),
            Signal::Bool(_)  => Signal::Bool(false),
            Signal::Vec2(_)  => Signal::Vec2(Vec2::ZERO),
            Signal::Vec4(_)  => Signal::Vec4(Vec4::ZERO),
            Signal::Int(_)   => Signal::Int(0),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum SignalType {
    Float,
    Bool,
    Vec2,
    Int,
    /// Quaternion orientation (x,y,z,w) — used by gyro 3DOF module output.
    Vec4,
    /// Accepts any incoming type — used for pass-through, selector, and switch modules.
    Any,
    /// Auto-mapping bus port: connects two devices and relays all name-matched signals
    /// automatically. Direct wires to individual pins on the destination take priority.
    AutoMap,
}

impl SignalType {
    pub fn accepts(self, incoming: SignalType) -> bool {
        match (self, incoming) {
            (SignalType::AutoMap, SignalType::AutoMap) => true,
            (SignalType::AutoMap, _) | (_, SignalType::AutoMap) => false,
            (SignalType::Any, _) | (_, SignalType::Any) => true,
            (a, b) if a == b => true,
            (SignalType::Vec4, SignalType::Vec4) => true,
            (SignalType::Float, SignalType::Bool)
            | (SignalType::Bool, SignalType::Float)
            | (SignalType::Float, SignalType::Int)
            | (SignalType::Int, SignalType::Float) => true,
            _ => false,
        }
    }

    pub fn display_name(self) -> &'static str {
        match self {
            SignalType::Float   => "Float",
            SignalType::Bool    => "Bool",
            SignalType::Vec2    => "Vec2",
            SignalType::Int     => "Int",
            SignalType::Vec4    => "Vec4",
            SignalType::Any     => "Any",
            SignalType::AutoMap => "AutoMap",
        }
    }

    /// Suggested wire / pin color for the UI.
    pub fn color_rgb(self) -> [u8; 3] {
        match self {
            SignalType::Float   => [100, 180, 255],
            SignalType::Bool    => [255, 220, 60],  // yellow (was orange)
            SignalType::Vec2    => [120, 220, 140],
            SignalType::Int     => [200, 160, 255],
            SignalType::Vec4    => [230, 150, 100], // warm orange (quaternion)
            SignalType::Any     => [180, 180, 180],
            SignalType::AutoMap => [255, 140, 40],  // orange
        }
    }
}
