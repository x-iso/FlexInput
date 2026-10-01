//! AutoMap Response Curve / AutoMap Two-way Curve: the curve modules, run on
//! one signal of an AutoMap bus instead of on a wire.
//!
//! The node hands its bus on under `collector:{uid}`, as the JSM and ASTH nodes
//! do, then reshapes the one pin its header picked (`am_curve_pin`) in place,
//! with the params and the math of the plain curve it wraps. Every other pin
//! passes through untouched.

use super::*;

/// Evaluate one AutoMap curve node. Returns the picked signal's values before
/// and after the curve, one per channel — what the body's live dots are drawn
/// from. A single pin is one channel; a sensor group (`automap_curve_group`) is
/// three, each axis curved on its own as a multi-channel Response Curve does.
pub(crate) fn eval_automap_curve_node(
    snap: &NodeSnap,
    uid: usize,
    dev_sigs: &HashMap<(String, String), Signal>,
    collector_sigs: &mut HashMap<(String, String), Signal>,
    state: &mut HashMap<usize, NodeState>,
    dt: f32,
) -> (Vec<Option<Signal>>, Vec<Option<Signal>>) {
    republish_bus_as_collector(snap, uid, dev_sigs, collector_sigs);
    let pick = snap.params.get(AUTOMAP_CURVE_PIN_PARAM).and_then(|v| v.as_str()).unwrap_or("");
    if pick.is_empty() {
        return (vec![None], vec![None]);
    }
    let pins: Vec<&str> = match automap_curve_group(pick) {
        Some(axes) => axes.to_vec(),
        None => vec![pick],
    };
    let key = format!("collector:{uid}");
    let inputs: Vec<Option<Signal>> = pins.iter()
        .map(|p| collector_sigs.get(&(key.clone(), p.to_string())).copied())
        .collect();
    // The picked signal decides the curve's mode — a stick is shaped by its
    // length, so always absolute — and never the stored params: switching the
    // pick between a stick and a trigger must leave the curve as drawn.
    let outputs: Vec<Option<Signal>> = if snap.module_id == AUTOMAP_TWOWAY_CURVE_ID {
        let is_vec = matches!(inputs.as_slice(), [Some(Signal::Vec2(_))]);
        let abs = is_vec || snap.params.get("absolute").and_then(|v| v.as_bool()).unwrap_or(true);
        let ns = state.entry(uid).or_default();
        compute_twoway_response_curve_as(&inputs, ns, &snap.params, dt, abs, is_vec)
    } else {
        // A stick is shaped by its length, as the Vec Response Curve does; an
        // axis or a trigger by its value.
        inputs.iter().map(|input| {
            let base = match input {
                Some(Signal::Vec2(_)) => "module.vec_response_curve",
                Some(Signal::Float(_)) => "module.response_curve",
                _ => return None,
            };
            eval_pure(base, 0, &[*input], &snap.params, 1)
        }).collect()
    };
    for (pin, output) in pins.iter().zip(&outputs) {
        if let Some(output) = output {
            write_curved_pin(&key, pin, *output, collector_sigs);
            mark_produced(&key, pin, collector_sigs);
        }
    }
    (inputs, outputs)
}

/// Write the curved value back over `pin`, keeping a stick's two views of itself
/// in step: the bus carries each stick as a Vec2 AND as its two axes, and a
/// reader may take either, so curving one view alone would leave the other raw.
fn write_curved_pin(
    key: &str,
    pin: &str,
    output: Signal,
    collector_sigs: &mut HashMap<(String, String), Signal>,
) {
    collector_sigs.insert((key.to_string(), pin.to_string()), output);
    let mut set_if_carried = |p: &str, sig: Signal| {
        if let Some(slot) = collector_sigs.get_mut(&(key.to_string(), p.to_string())) {
            *slot = sig;
        }
    };
    match output {
        Signal::Vec2(v) => {
            if let Some((x, y)) = axis_pins_for_vec2(pin) {
                set_if_carried(x, Signal::Float(v.x));
                set_if_carried(y, Signal::Float(v.y));
            }
        }
        Signal::Float(f) => {
            if let Some(vec_pin) = vec2_pin_for_axis(pin) {
                let vec_key = (key.to_string(), vec_pin.to_string());
                if let Some(Signal::Vec2(v)) = collector_sigs.get(&vec_key).copied() {
                    let v = if pin.ends_with("_x") { Vec2::new(f, v.y) } else { Vec2::new(v.x, f) };
                    collector_sigs.insert(vec_key, Signal::Vec2(v));
                }
            }
        }
        _ => {}
    }
}

/// The two axis pins a Vec2 pin is carried as alongside itself.
fn axis_pins_for_vec2(pin: &str) -> Option<(&'static str, &'static str)> {
    match pin {
        "left_stick"  => Some(("left_stick_x", "left_stick_y")),
        "right_stick" => Some(("right_stick_x", "right_stick_y")),
        "dpad"        => Some(("dpad_x", "dpad_y")),
        _ => None,
    }
}
