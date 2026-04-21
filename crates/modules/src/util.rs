use flexinput_core::{Signal, SignalType};

pub fn get_float(inputs: &[Option<Signal>], idx: usize, default: f32) -> f32 {
    inputs
        .get(idx)
        .and_then(|s| *s)
        .and_then(|s| s.coerce_to(SignalType::Float))
        .map(|s| if let Signal::Float(f) = s { f } else { default })
        .unwrap_or(default)
}

pub fn get_bool(inputs: &[Option<Signal>], idx: usize, default: bool) -> bool {
    inputs
        .get(idx)
        .and_then(|s| *s)
        .and_then(|s| s.coerce_to(SignalType::Bool))
        .map(|s| if let Signal::Bool(b) = s { b } else { default })
        .unwrap_or(default)
}

pub fn get_int(inputs: &[Option<Signal>], idx: usize, default: i32) -> i32 {
    inputs
        .get(idx)
        .and_then(|s| *s)
        .and_then(|s| s.coerce_to(SignalType::Int))
        .map(|s| if let Signal::Int(i) = s { i } else { default })
        .unwrap_or(default)
}
