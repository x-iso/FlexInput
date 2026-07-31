//! RWS (Real-World Sensitivity) aim evaluator.
//!
//! Scales a rotation-rate Vec2 into per-tick MOUSE DISPLACEMENT so that a
//! physical rotation maps 1:1 to the in-game camera rotation once `scale`
//! (mouse counts per degree) is calibrated; `rws` multiplies that ground truth.
//! The output (out 0, Vec2) is wired to the KB/M `mouse_move` sink pin — applied
//! once per tick and NOT scaled by the device card's mouse_sensitivity — so the
//! calibrated `scale` is the sole knob and presets stay portable.
//!
//! Phase 1: rate → displacement (gyro / stick_rate modes) + the calibration
//! constant. Flick-stick (input 1) lands in a later phase.

use super::*;

/// Signal-graph gyro normalization: ±1.0 corresponds to ±this many deg/s.
/// Mirrors `flexinput_devices::gyro::GYRO_REF_DPS` (kept local to avoid leaning
/// on a cross-crate pub path for one constant; keep the two in sync).
pub(crate) const GYRO_REF_DPS: f32 = 2000.0;

/// Compute one tick of the RWS module.
/// Outputs: [Mouse Vec2 (per-tick displacement), X f32, Y f32].
pub(crate) fn compute_rws(
    inputs: &[Option<Signal>],
    _state: &mut NodeState,
    params: &HashMap<String, Value>,
    dt: f32,
) -> Vec<Option<Signal>> {
    let pf = |k: &str, d: f32| {
        params.get(k).and_then(|v| v.as_f64()).map(|v| v as f32).unwrap_or(d)
    };
    let ps = |k: &str, d: &'static str| {
        params.get(k).and_then(|v| v.as_str()).unwrap_or(d).to_string()
    };
    let pb = |k: &str, d: bool| params.get(k).and_then(|v| v.as_bool()).unwrap_or(d);

    let scale = pf("scale", 100.0);
    let rws = pf("rws", 1.0);
    let calibrating = pb("calibrating", false);
    let cal_speed = pf("cal_speed", 0.5);
    let input_mode = ps("input_mode", "gyro");
    let max_rate = pf("max_rate_dps", 360.0);

    // Rotation rate in deg/s (yaw = x, pitch = y). While calibrating we ignore
    // the inputs and spin at a KNOWN rate (cal_speed revolutions/second) so the
    // user can match the game to the on-screen reference; cal_speed 1.0 = 1 rev/s.
    let (yaw_dps, pitch_dps) = if calibrating {
        (cal_speed * 360.0, 0.0)
    } else {
        let rot = match inputs.first().and_then(|s| *s) {
            Some(Signal::Vec2(v)) => v,
            Some(Signal::Float(f)) => glam::Vec2::new(f, 0.0),
            _ => glam::Vec2::ZERO,
        };
        // Gyro axes are already ±1 == ±GYRO_REF_DPS deg/s; a stick is bounded
        // deflection, so treat it as a rate up to `max_rate_dps` at full tilt.
        let k = if input_mode == "stick_rate" { max_rate } else { GYRO_REF_DPS };
        (rot.x * k, rot.y * k)
    };

    // Per-tick displacement in mouse counts (scale = counts per degree).
    let dx = yaw_dps * dt * scale * rws;
    let dy = pitch_dps * dt * scale * rws;

    vec![
        Some(Signal::Vec2(glam::Vec2::new(dx, dy))),
        Some(Signal::Float(dx)),
        Some(Signal::Float(dy)),
    ]
}
