//! Module bodies built from the body kit (`kit.rs`): the first modules moved
//! onto the shared layout. Each keeps the pinnable element ids (and rects) it
//! had, so pins made before keep working.

use super::kit::{self, Kit};
use super::*;

fn param_f32(snarl: &Snarl<NodeData>, node_id: NodeId, key: &str, default: f32) -> f32 {
    snarl
        .get_node(node_id)
        .and_then(|n| n.params.get(key).and_then(|v| v.as_f64()))
        .map_or(default, |v| v as f32)
}

fn param_str(snarl: &Snarl<NodeData>, node_id: NodeId, key: &str, default: &str) -> String {
    snarl
        .get_node(node_id)
        .and_then(|n| n.params.get(key).and_then(|v| v.as_str()))
        .unwrap_or(default)
        .to_string()
}

fn param_bool(snarl: &Snarl<NodeData>, node_id: NodeId, key: &str, default: bool) -> bool {
    snarl
        .get_node(node_id)
        .and_then(|n| n.params.get(key).and_then(|v| v.as_bool()))
        .unwrap_or(default)
}

fn set_f32(snarl: &mut Snarl<NodeData>, node_id: NodeId, key: &str, v: f32) {
    if let (Some(node), Some(n)) = (snarl.get_node_mut(node_id), Number::from_f64(v as f64)) {
        node.params.insert(key.into(), Value::Number(n));
    }
}

fn set_value(snarl: &mut Snarl<NodeData>, node_id: NodeId, key: &str, v: Value) {
    if let Some(node) = snarl.get_node_mut(node_id) {
        node.params.insert(key.into(), v);
    }
}

fn wired(inputs: &[InPin], idx: usize) -> bool {
    inputs.get(idx).is_some_and(|p| !p.remotes.is_empty())
}

/// The params key a kit node's width grip writes (for tests outside the viewer).
#[cfg(test)]
pub(crate) fn kit_width_param() -> &'static str {
    kit::WIDTH_PARAM
}

/// The narrowest each kit module's rows allow.
pub(crate) fn kit_min_width(module_id: &str) -> f32 {
    match module_id {
        "math.quantize" => 220.0,
        "logic.counter" => 240.0,
        _ => kit::MIN_W,
    }
}

/// The Channels row the per-channel filters share: − count + adds or drops an
/// In/Out pin pair at the end.
fn channels_row(
    ui: &mut egui::Ui,
    k: Kit,
    node_id: NodeId,
    inputs: &[InPin],
    outputs: &[OutPin],
    snarl: &mut Snarl<NodeData>,
) {
    let n = snarl.get_node(node_id).map_or(1, |n| n.inputs.len()).max(1);
    let (step, _) = k.stepper(ui, "Channels", n, 1..=64, ("Remove the last channel", "Add a channel"));
    match step {
        Some(1) => {
            if let Some(node) = snarl.get_node_mut(node_id) {
                let next = node.inputs.len() + 1;
                node.inputs.push(PinDescriptor::new(format!("In {next}"), SignalType::Float));
                node.outputs.push(PinDescriptor::new(format!("Out {next}"), SignalType::Float));
            }
        }
        Some(-1) if n > 1 => {
            remove_input_pin(node_id, n - 1, inputs, snarl);
            remove_output_pin(node_id, n - 1, outputs, snarl);
        }
        _ => {}
    }
}

/// Delay (`module.delay`): every channel delayed by the same time.
pub(crate) fn show_delay_body(
    node_id: NodeId,
    inputs: &[InPin],
    outputs: &[OutPin],
    ui: &mut egui::Ui,
    snarl: &mut Snarl<NodeData>,
    w: f32,
) {
    let mut ms = param_f32(snarl, node_id, "delay_ms", 100.0);
    kit::body(ui, w, |ui, k| {
        let (changed, rect) = k.slider(ui, "Delay", &mut ms, 0.0..=60_000.0, true, |v| {
            egui::DragValue::new(v).speed(1.0).range(0.0..=60_000.0).max_decimals(0).suffix(" ms")
        });
        register_exposable_element(ui, node_id, "ms", rect);
        if changed {
            set_f32(snarl, node_id, "delay_ms", ms);
        }
        channels_row(ui, k, node_id, inputs, outputs, snarl);
    });
}

/// Average (`module.average`): a moving average over a time window, with
/// outliers dropped past a spike threshold.
pub(crate) fn show_average_body(
    node_id: NodeId,
    inputs: &[InPin],
    outputs: &[OutPin],
    ui: &mut egui::Ui,
    snarl: &mut Snarl<NodeData>,
    w: f32,
) {
    let mut ms = snarl
        .get_node(node_id)
        .map_or(5.0, |n| flexinput_engine::eval::average_window_ms(&n.params) as f32);
    let mut mad = param_f32(snarl, node_id, "spike_mad", 0.0);
    kit::body(ui, w, |ui, k| {
        let (c1, r1) = k.slider(ui, "Window", &mut ms, 0.5..=5_000.0, true, |v| {
            egui::DragValue::new(v).speed(0.5).range(0.5..=5_000.0).max_decimals(1).suffix(" ms")
        });
        let (c2, r2) = k.slider(ui, "Spikes", &mut mad, 0.0..=20.0, false, |v| {
            egui::DragValue::new(v).speed(0.1).range(0.0..=20.0).max_decimals(1)
        });
        ui.interact(r2, ui.id().with(("avg_mad_hint", node_id)), egui::Sense::hover()).on_hover_text(
            "Outlier threshold in median absolute deviations. 0 = off. Try 3.0 to start.",
        );
        register_exposable_element(ui, node_id, "samples", r1);
        register_exposable_element(ui, node_id, "spike_mad", r2);
        if c1 || c2 {
            set_f32(snarl, node_id, "window_ms", ms);
            set_f32(snarl, node_id, "spike_mad", mad);
        }
        channels_row(ui, k, node_id, inputs, outputs, snarl);
    });
}

/// DC Filter (`module.dc_filter`): removes a slowly moving offset.
pub(crate) fn show_dc_filter_body(
    node_id: NodeId,
    inputs: &[InPin],
    outputs: &[OutPin],
    ui: &mut egui::Ui,
    snarl: &mut Snarl<NodeData>,
    w: f32,
) {
    let mut window = param_f32(snarl, node_id, "window_ms", 500.0);
    let mut decay = param_f32(snarl, node_id, "decay_ms", 200.0);
    kit::body(ui, w, |ui, k| {
        fn drag(v: &mut f32) -> egui::DragValue<'_> {
            egui::DragValue::new(v).speed(10.0).range(10.0..=60_000.0).max_decimals(0).suffix(" ms")
        }
        let (c1, r1) = k.slider(ui, "Window", &mut window, 10.0..=60_000.0, true, drag);
        let (c2, r2) = k.slider(ui, "Decay", &mut decay, 10.0..=60_000.0, true, drag);
        register_exposable_element(ui, node_id, "window_ms", r1);
        register_exposable_element(ui, node_id, "decay_ms", r2);
        if c1 || c2 {
            set_f32(snarl, node_id, "window_ms", window);
            set_f32(snarl, node_id, "decay_ms", decay);
        }
        channels_row(ui, k, node_id, inputs, outputs, snarl);
    });
}

/// Inverse (`math.negate`): flips the sign, or mirrors inside `0..max`.
pub(crate) fn show_inverse_body(node_id: NodeId, ui: &mut egui::Ui, snarl: &mut Snarl<NodeData>, w: f32) {
    let mut unipolar = param_bool(snarl, node_id, "unipolar", false);
    let mut max = param_f32(snarl, node_id, "unipolar_max", 1.0);
    kit::body(ui, w, |ui, k| {
        let (c1, r1) = k.choice(ui, Some("Flip"), &mut unipolar, &[
            (false, "Sign", "Output −v"),
            (true, "In 0..max", "Mirror the signal inside 0..max instead of flipping its sign"),
        ]);
        let (c2, r2) = ui
            .add_enabled_ui(unipolar, |ui| {
                k.value(ui, "Max", &mut max, |v| egui::DragValue::new(v).speed(0.01).range(0.0..=1000.0))
            })
            .inner;
        register_exposable_element(ui, node_id, "unipolar", r1.union(r2));
        if c1 || c2 {
            set_value(snarl, node_id, "unipolar", Value::Bool(unipolar));
            set_f32(snarl, node_id, "unipolar_max", max);
        }
    });
}

/// Quantize (`math.quantize`): rounds to steps of 1/factor.
pub(crate) fn show_quantize_body(
    node_id: NodeId,
    inputs: &[InPin],
    ui: &mut egui::Ui,
    snarl: &mut Snarl<NodeData>,
    w: f32,
) {
    let mut factor = param_f32(snarl, node_id, "factor", 1.0);
    let mut mode = param_str(snarl, node_id, "mode", "round");
    let factor_wired = wired(inputs, 1);
    kit::body(ui, w, |ui, k| {
        let (c1, r1) = ui
            .add_enabled_ui(!factor_wired, |ui| {
                k.value(ui, if factor_wired { "Factor (wired)" } else { "Factor" }, &mut factor, |v| {
                    egui::DragValue::new(v).speed(0.05).range(0.0..=10_000.0)
                })
            })
            .inner;
        ui.interact(r1, ui.id().with(("quantize_factor_hint", node_id)), egui::Sense::hover())
            .on_hover_text("Steps per unit: 1 = integers, 2 = halves, 4 = quarters");
        let (c2, r2) = k.choice(ui, Some("Rounding"), &mut mode, &[
            ("round".to_string(), "Round", "To the nearest step"),
            ("floor".to_string(), "Floor", "Down to the step below"),
            ("ceil".to_string(), "Ceil", "Up to the step above"),
            ("trunc".to_string(), "Trunc", "Toward zero"),
        ]);
        register_exposable_element(ui, node_id, "factor", r1);
        register_exposable_element(ui, node_id, "mode", r2);
        if c1 || c2 {
            set_value(snarl, node_id, "mode", Value::String(mode.clone()));
            set_f32(snarl, node_id, "factor", factor);
        }
    });
}

/// Logic Delay (`logic.delay`): holds back a change of state by a time.
pub(crate) fn show_logic_delay_body(node_id: NodeId, ui: &mut egui::Ui, snarl: &mut Snarl<NodeData>, w: f32) {
    let mut mode = param_str(snarl, node_id, "mode", "delay_false");
    let mut time = param_f32(snarl, node_id, "time", 100.0);
    let mut unit = param_str(snarl, node_id, "unit", "ms");
    kit::body(ui, w, |ui, k| {
        let (c1, r1) = k.choice(ui, Some("Delay"), &mut mode, &[
            ("delay_true".to_string(), "On", "Turning on waits the delay; turning off is immediate"),
            ("delay_false".to_string(), "Off", "Turning off waits the delay; turning on is immediate"),
        ]);
        let limit = if unit == "ms" { 60_000.0 } else { 10_000.0 };
        let (c2, r2) = k.value(ui, "Time", &mut time, |v| {
            egui::DragValue::new(v).speed(1.0).range(0.0..=limit).max_decimals(0)
        });
        let (c3, r3) = k.choice(ui, Some("Unit"), &mut unit, &[
            ("ms".to_string(), "ms", "Milliseconds"),
            ("samples".to_string(), "frames", "Engine ticks"),
        ]);
        register_exposable_element(ui, node_id, "mode", r1);
        register_exposable_element(ui, node_id, "time", r2.union(r3));
        if c1 || c2 || c3 {
            set_value(snarl, node_id, "mode", Value::String(mode.clone()));
            set_value(snarl, node_id, "unit", Value::String(unit.clone()));
            set_f32(snarl, node_id, "time", time);
        }
    });
}

/// Counter (`logic.counter`): counts steps between limits.
pub(crate) fn show_counter_body(node_id: NodeId, inputs: &[InPin], ui: &mut egui::Ui, snarl: &mut Snarl<NodeData>, w: f32) {
    let mut mode = param_str(snarl, node_id, "mode", "loop");
    let mut normalized = param_bool(snarl, node_id, "normalized", false);
    let mut step = param_f32(snarl, node_id, "step_param", 1.0);
    let mut min = param_f32(snarl, node_id, "min_param", 0.0);
    let mut max = param_f32(snarl, node_id, "max_param", 10.0);
    let (step_wired, min_wired, max_wired) = (wired(inputs, 3), wired(inputs, 4), wired(inputs, 5));
    kit::body(ui, w, |ui, k| {
        let (c1, r1) = k.choice(ui, None, &mut mode, &[
            ("loop".to_string(), "Loop", "Past the max, start again at the min"),
            ("limit".to_string(), "Limit", "Stop at the min and max"),
            ("bounce".to_string(), "Bounce", "Turn around at the min and max"),
            ("unlimited".to_string(), "Unlimited", "Count on past the max"),
        ]);
        let mut reset = false;
        let (c2, r2) = k.row(ui, "Output", |ui, rest| {
            let h = ui.spacing().interact_size.y;
            let changed = kit::strip(ui, rest - kit::GAP - h, &mut normalized, &[
                (false, "Raw", "The count itself"),
                (true, "0..1", "The count across min..max as 0..1"),
            ]);
            ui.add_space(kit::GAP);
            reset = ui
                .add(egui::Button::new("↺").min_size(egui::vec2(h, h)))
                .on_hover_text("Reset the count")
                .clicked();
            changed
        });
        let (c3, r3) = ui
            .add_enabled_ui(!step_wired, |ui| {
                k.value(ui, "Step", &mut step, |v| egui::DragValue::new(v).speed(0.1).range(0.001..=10_000.0))
            })
            .inner;
        let (c4, r4) = k.pair(
            ui,
            "Range",
            ("Min", &mut min, !min_wired),
            ("Max", &mut max, !max_wired && mode != "unlimited"),
            |v| egui::DragValue::new(v).speed(0.1),
        );
        register_exposable_element(ui, node_id, "mode", r1);
        register_exposable_element(ui, node_id, "range_mode", r2);
        register_exposable_element(ui, node_id, "step", r3);
        register_exposable_element(ui, node_id, "min_max", r4);

        if reset {
            if let Some(node) = snarl.get_node_mut(node_id) {
                while node.extra.aux_f32.len() < 2 {
                    node.extra.aux_f32.push(0.0);
                }
                node.extra.aux_f32[0] = 0.0;
                node.extra.aux_f32[1] = 1.0;
                node.extra.aux_f32_dirty = true;
            }
        }
        if c1 || c2 || c3 || c4 {
            set_value(snarl, node_id, "mode", Value::String(mode.clone()));
            set_value(snarl, node_id, "normalized", Value::Bool(normalized));
            set_f32(snarl, node_id, "step_param", step);
            set_f32(snarl, node_id, "min_param", min);
            set_f32(snarl, node_id, "max_param", max);
        }
    });
}

/// Has Changed (`logic.has_changed`): pulses when the input moves.
pub(crate) fn show_has_changed_body(node_id: NodeId, ui: &mut egui::Ui, snarl: &mut Snarl<NodeData>, w: f32) {
    let Some((field, mut shown)) = has_changed_hysteresis(node_id, snarl) else { return };
    kit::body(ui, w, |ui, k| {
        let (changed, rect) = k.value(ui, "Hysteresis", &mut shown, |v| field.drag(v));
        ui.interact(rect, ui.id().with(("has_changed_hint", node_id)), egui::Sense::hover()).on_hover_text(
            "Ignores movement smaller than this, like noise or a shaky hand. It reacts once \
             the input has moved this far from where it last reacted. 0 = every change counts.",
        );
        register_exposable_element(ui, node_id, "hysteresis", rect);
        if changed {
            set_has_changed_hysteresis(node_id, snarl, shown, &field);
        }
    });
}

/// Vec to Deflection (`module.vec_to_deflection`): the Angle output's unit.
pub(crate) fn show_vec_to_deflection_body(node_id: NodeId, ui: &mut egui::Ui, snarl: &mut Snarl<NodeData>, w: f32) {
    let mut degrees = param_bool(snarl, node_id, "degrees", false);
    kit::body(ui, w, |ui, k| {
        let (changed, rect) = k.choice(ui, Some("Angle"), &mut degrees, &[
            (false, "0..1", "Angle as a fraction of a full turn, wrapping to 0 at 1"),
            (true, "0..360°", "Angle in degrees, wrapping to 0 at 360"),
        ]);
        register_exposable_element(ui, node_id, "angle_unit", rect);
        if changed {
            set_value(snarl, node_id, "degrees", Value::Bool(degrees));
        }
    });
}

/// Greater / Less Than (`logic.greater_than`, `logic.less_than`).
pub(crate) fn show_or_equal_body(node_id: NodeId, ui: &mut egui::Ui, snarl: &mut Snarl<NodeData>, w: f32) {
    let mut or_equal = param_bool(snarl, node_id, "or_equal", false);
    kit::body(ui, w, |ui, k| {
        let (resp, _) = k.toggle(ui, "Equal", &mut or_equal, "counts too");
        if resp.on_hover_text("Also true when the two are equal").changed() {
            set_value(snarl, node_id, "or_equal", Value::Bool(or_equal));
        }
    });
}
