//! Live-selection-aware routing resolver for the config overlay.
//!
//! The config overlay's passthrough (M3.4) traces UPSTREAM from a tweak-pin to
//! its physical source device: perfect for "tweak a stick curve → that stick
//! passes." It breaks for SOURCE-like params (a Knob/Constant with no physical
//! upstream) whose *effect* is decided DOWNSTREAM by the patch — e.g. a "gyro↔
//! stick mix" Knob whose destination virtual stick is chosen at runtime by
//! `module.split` / `module.selector` gates driven by dropdowns/switches.
//!
//! This module answers, for such a source param, in the CURRENT live selection
//! state:
//!   1. which virtual output pins does it modulate? (downstream, gate-pruned)
//!   2. which physical inputs feed those outputs? (upstream from each, gate-
//!      pruned) — the set to pass through so the tweak is *felt* in-game.
//!   3. is each physical stick currently routed to its virtual stick? — the
//!      "free to be a tweak control" test.
//!
//! Live selection is read from `NodeData.extra.last_signals` (per-input latest
//! eval values, refreshed every frame by `apply_display_state` /
//! `sync_display_state_into` in `graph.rs`). The gate select semantics MUST
//! match the engine: `module.split` — input 0 = select, input 1 = data, active
//! output = `floor(clamp(sel,0,1)·n_outputs)`; `module.selector` — input 0 =
//! select, inputs 1.. = sources, active source = `floor(clamp(sel,0,1)·
//! n_sources)` (see `crates/engine/src/eval/compute.rs`).
//!
//! Pure functions over a `Snarl<NodeData>` — no UI/engine side effects. The
//! passthrough/nav wiring that consumes them lives in `graph.rs` / `nav/config.rs`.

use super::*;
use std::collections::{HashSet, VecDeque};

/// Which physical input the config overlay assigns to ADJUST a tweak-pin, so it
/// doesn't collide with the input(s) passing through to be felt. Derived from
/// the pin's passthrough set (see [`control_input_from_pins`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ControlInput {
    LeftStick,
    RightStick,
    Dpad,
}

/// Pick the control input from a tweak-pin's passthrough pin set. A physical
/// stick that's passing through (so its effect is felt) must NOT also drive the
/// editor; use the OTHER stick. When BOTH sticks pass through, neither is free →
/// the D-pad adjusts the param (and, being suppressed, never leaks to the game).
pub(crate) fn control_input_from_pins(pins: &[String]) -> ControlInput {
    let left = pins.iter().any(|p| p == "left_stick");
    let right = pins.iter().any(|p| p == "right_stick");
    match (left, right) {
        (true, true) => ControlInput::Dpad,
        (true, false) => ControlInput::RightStick, // left felt → right adjusts
        // right felt → left adjusts; also the default when no stick is felt.
        (false, _) => ControlInput::LeftStick,
    }
}

/// Physical bus pins to pass through for a SOURCE-like tweak param (`tweak`
/// names the node inside `snarl` — the tab snarl for a top-level pin, or the
/// sub-patch's inner snarl for a `[sp]` pin). Traces what the param modulates
/// DOWNSTREAM in the current live selection state, then the physical inputs
/// feeding those outputs. `snarl` must carry fresh `extra.last_signals`.
pub(crate) fn source_passthrough_pins(snarl: &Snarl<NodeData>, tweak: NodeId) -> Vec<String> {
    let affected = downstream_affected_outputs(snarl, tweak);
    let mut leaves: HashSet<String> = HashSet::new();
    for pin in &affected {
        leaves.extend(physical_leaves_of_output(snarl, pin));
    }
    let mut pins: Vec<String> = leaves.iter().flat_map(|p| expand_pin_group(p)).collect();
    pins.sort();
    pins.dedup();
    pins
}

// ── Downstream: which virtual outputs does the tweak param modulate? ──────────

/// BFS forward from `tweak`'s outputs, pruning inactive branches at
/// `module.split` / `module.selector` gates by their live select value, and
/// terminating at `module.automap_collect` inputs (the injection points onto the
/// virtual bus). Returns the collected virtual pin ids.
fn downstream_affected_outputs(snarl: &Snarl<NodeData>, tweak: NodeId) -> HashSet<String> {
    let mut collected: HashSet<String> = HashSet::new();
    let mut visited: HashSet<(usize, usize)> = HashSet::new();
    let mut q: VecDeque<InPinId> = VecDeque::new();

    if let Some(node) = snarl.get_node(tweak) {
        for o in 0..node.outputs.len() {
            for &dst in &snarl.out_pin(OutPinId { node: tweak, output: o }).remotes {
                q.push_back(dst);
            }
        }
    }

    while let Some(inp) = q.pop_front() {
        if !visited.insert((inp.node.0, inp.input)) {
            continue;
        }
        let Some(node) = snarl.get_node(inp.node) else { continue };
        // Terminal: the collector injects the arriving value onto a named bus pin.
        if node.module_id == "module.automap_collect" {
            if let Some(pin) = collect_pin_id(node, inp.input) {
                collected.insert(pin);
            }
            continue;
        }
        for o in downstream_active_outputs(snarl, inp.node, inp.input) {
            for &dst in &snarl.out_pin(OutPinId { node: inp.node, output: o }).remotes {
                q.push_back(dst);
            }
        }
    }
    collected
}

/// Given `node` reached via input `in_idx`, which of its outputs currently carry
/// that value onward (live selection honored).
fn downstream_active_outputs(snarl: &Snarl<NodeData>, node_id: NodeId, in_idx: usize) -> Vec<usize> {
    let Some(node) = snarl.get_node(node_id) else { return vec![] };
    match node.module_id.as_str() {
        // Demux: only the data input (1) flows, and only to the active output.
        "module.split" => {
            if in_idx != 1 {
                return vec![];
            }
            split_active_outs(snarl, node_id)
        }
        // Mux: a source input flows to output 0 only when it is the selected one.
        "module.selector" => {
            if in_idx == 0 {
                return vec![];
            }
            if selector_active_ins(snarl, node_id).contains(&(in_idx - 1)) {
                vec![0]
            } else {
                vec![]
            }
        }
        // Sources / gate-controls / display sinks never carry the tweak value on.
        "module.switch" | "module.dropdown" | "module.constant" | "module.knob"
        | "display.vectorscope" | "display.oscilloscope" | "display.readout"
        | "display.trigscope" | "subpatch.outlet" => vec![],
        // Transparent (math, response curves, vec_to_axis, add/mul/…): all outputs.
        _ => (0..node.outputs.len()).collect(),
    }
}

// ── Upstream: which physical inputs feed a given virtual output pin? ──────────

/// The physical bus pin ids currently feeding virtual output `pin_id`, following
/// only live-active branches. Empty when `pin_id` isn't collected anywhere.
fn physical_leaves_of_output(snarl: &Snarl<NodeData>, pin_id: &str) -> HashSet<String> {
    let mut leaves: HashSet<String> = HashSet::new();
    for (nid, node) in snarl.nodes_ids_data() {
        if node.value.module_id != "module.automap_collect" {
            continue;
        }
        let Some(ids) = node
            .value
            .params
            .get("collect_input_pin_ids")
            .and_then(|v| v.as_array())
        else {
            continue;
        };
        for (k, idv) in ids.iter().enumerate() {
            if idv.as_str() != Some(pin_id) {
                continue;
            }
            // Collector input 0 is the Device (AutoMap) pass-through; the k-th
            // named pin is input k+1.
            if let Some(src) = wired_source(snarl, nid, k + 1) {
                let mut visited: HashSet<(usize, usize)> = HashSet::new();
                upstream_physical_leaves(snarl, src, &mut leaves, &mut visited);
            }
        }
    }
    leaves
}

/// Recurse upstream from output pin `start`, collecting physical bus pin ids at
/// the leaves (`module.automap_split` named outputs, `processing.gyro_3dof` IMU),
/// pruning dead branches at `module.split` / `module.selector` gates.
fn upstream_physical_leaves(
    snarl: &Snarl<NodeData>,
    start: OutPinId,
    out: &mut HashSet<String>,
    visited: &mut HashSet<(usize, usize)>,
) {
    if !visited.insert((start.node.0, start.output)) {
        return;
    }
    let Some(node) = snarl.get_node(start.node) else { return };
    match node.module_id.as_str() {
        "module.automap_split" => match split_output_pin_id(node, start.output) {
            Some(pin) if pin != "automap_pass" && !pin.is_empty() => {
                out.insert(pin);
            }
            // Whole-bus pass-through output: recurse into the Device input.
            _ => {
                if let Some(src) = wired_source(snarl, start.node, 0) {
                    upstream_physical_leaves(snarl, src, out, visited);
                }
            }
        },
        "processing.gyro_3dof" => {
            for p in GYRO_IMU_PINS {
                out.insert(p.to_string());
            }
        }
        // Demux: this output is live only when selected; then trace the data input.
        "module.split" => {
            if split_active_outs(snarl, start.node).contains(&start.output) {
                if let Some(src) = wired_source(snarl, start.node, 1) {
                    upstream_physical_leaves(snarl, src, out, visited);
                }
            }
        }
        // Mux: trace only the currently-selected source input(s).
        "module.selector" => {
            for si in selector_active_ins(snarl, start.node) {
                if let Some(src) = wired_source(snarl, start.node, si + 1) {
                    upstream_physical_leaves(snarl, src, out, visited);
                }
            }
        }
        // Non-physical leaves.
        "module.constant" | "module.knob" | "module.dropdown" | "module.switch"
        | "subpatch.inlet" => {}
        // Transparent: union of all connected inputs.
        _ => {
            for i in 0..node.inputs.len() {
                if let Some(src) = wired_source(snarl, start.node, i) {
                    upstream_physical_leaves(snarl, src, out, visited);
                }
            }
        }
    }
}

// ── Gate select semantics (mirror the engine) ────────────────────────────────

/// The live value driving `node`'s input pin `idx`, coerced to f32; 0.0 when
/// unwired/absent. Read from the SOURCE node's last computed output
/// (`extra.last_out`, populated for EVERY node each tick), NOT the gate's own
/// `extra.last_signals` — the engine only records `last_signals` for
/// display/curve nodes, so split/selector gates never have it. Reading the
/// wire's source output is what lets the resolver follow the CURRENT selection.
fn live_input_f32(snarl: &Snarl<NodeData>, node: NodeId, idx: usize) -> f32 {
    let Some(src) = wired_source(snarl, node, idx) else { return 0.0 };
    snarl
        .get_node(src.node)
        .and_then(|n| n.extra.last_out.get(src.output).copied().flatten())
        .map(|s| s.as_float())
        .unwrap_or(0.0)
}

fn interpolates(node: &NodeData) -> bool {
    node.params
        .get("interpolate")
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
}

/// Active output index(es) of a `module.split` demux. Non-interp: one; interp:
/// the two blended neighbours.
fn split_active_outs(snarl: &Snarl<NodeData>, node_id: NodeId) -> Vec<usize> {
    let Some(node) = snarl.get_node(node_id) else { return vec![] };
    let n = node.outputs.len();
    if n == 0 {
        return vec![];
    }
    let sel = live_input_f32(snarl, node_id, 0).clamp(0.0, 1.0);
    if interpolates(node) && n >= 2 {
        let pos = sel * (n as f32 - 1.0);
        let lo = pos.floor() as usize;
        let hi = (lo + 1).min(n - 1);
        if lo == hi { vec![lo] } else { vec![lo, hi] }
    } else {
        vec![((sel * n as f32).floor() as usize).min(n - 1)]
    }
}

/// Active source index(es) of a `module.selector` mux, as 0-based offsets into
/// the source inputs (actual input pin = offset + 1).
pub(crate) fn selector_active_ins(snarl: &Snarl<NodeData>, node_id: NodeId) -> Vec<usize> {
    let Some(node) = snarl.get_node(node_id) else { return vec![] };
    let n = node.inputs.len().saturating_sub(1); // minus the select input
    if n == 0 {
        return vec![];
    }
    let sel = live_input_f32(snarl, node_id, 0).clamp(0.0, 1.0);
    if interpolates(node) && n >= 2 {
        let pos = sel * (n as f32 - 1.0);
        let lo = pos.floor() as usize;
        let hi = (lo + 1).min(n - 1);
        if lo == hi { vec![lo] } else { vec![lo, hi] }
    } else {
        vec![((sel * n as f32).floor() as usize).min(n - 1)]
    }
}

// ── Small snarl/param helpers ─────────────────────────────────────────────────

fn wired_source(snarl: &Snarl<NodeData>, node: NodeId, input: usize) -> Option<OutPinId> {
    snarl
        .in_pin(InPinId { node, input })
        .remotes
        .first()
        .copied()
}

/// The bus pin id a `module.automap_collect` input carries (input 0 = Device →
/// `None`; the k-th named pin is input k+1 → `collect_input_pin_ids[k]`).
fn collect_pin_id(node: &NodeData, in_idx: usize) -> Option<String> {
    if in_idx == 0 {
        return None;
    }
    node.params
        .get("collect_input_pin_ids")
        .and_then(|v| v.as_array())
        .and_then(|a| a.get(in_idx - 1))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
}

/// The bus pin id a `module.automap_split` output carries.
fn split_output_pin_id(node: &NodeData, out_idx: usize) -> Option<String> {
    node.params
        .get("output_pin_ids")
        .and_then(|v| v.as_array())
        .and_then(|a| a.get(out_idx))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use flexinput_core::{PinDescriptor, Signal, SignalType};
    use serde_json::json;

    fn node(module_id: &str, n_in: usize, n_out: usize) -> NodeData {
        NodeData {
            module_id: module_id.to_string(),
            display_name: module_id.to_string(),
            category: "test".to_string(),
            inputs: (0..n_in)
                .map(|i| PinDescriptor::new(format!("in{i}"), SignalType::Any))
                .collect(),
            outputs: (0..n_out)
                .map(|i| PinDescriptor::new(format!("out{i}"), SignalType::Any))
                .collect(),
            params: std::collections::HashMap::new(),
            subpatch: None,
            extra: crate::canvas::node::NodeExtra::default(),
        }
    }

    fn connect(s: &mut Snarl<NodeData>, from: NodeId, o: usize, to: NodeId, i: usize) {
        s.connect(
            OutPinId { node: from, output: o },
            InPinId { node: to, input: i },
        );
    }

    /// Build the gyro↔stick-mix skeleton from the user's real patch:
    ///   knob → mult(×gyro) → split "Where Gyro goes" → addRS/addLS → collect
    /// with the physical sticks (automap_split) always feeding addRS/addLS too.
    /// `sel` is the split's live select value; returns the tweak's passthrough.
    fn mix_route(sel: f32) -> Vec<String> {
        let mut s: Snarl<NodeData> = Snarl::new();
        let p = egui::Pos2::ZERO;
        let knob = s.insert_node(p, node("module.knob", 0, 1));
        let gyro = s.insert_node(p, node("processing.gyro_3dof", 1, 1));
        let mult = s.insert_node(p, node("math.multiply", 2, 1));
        // split: input 0 = select, input 1 = data; outputs 0 = RS, 1 = LS.
        let split = s.insert_node(p, node("module.split", 2, 2));
        let asplit = {
            let mut n = node("module.automap_split", 1, 3);
            n.params.insert(
                "output_pin_ids".into(),
                json!(["automap_pass", "right_stick", "left_stick"]),
            );
            s.insert_node(p, n)
        };
        let add_rs = s.insert_node(p, node("math.add", 2, 1));
        let add_ls = s.insert_node(p, node("math.add", 2, 1));
        let collect = {
            let mut n = node("module.automap_collect", 3, 1); // Device, RS, LS
            n.params.insert(
                "collect_input_pin_ids".into(),
                json!(["right_stick", "left_stick"]),
            );
            s.insert_node(p, n)
        };

        connect(&mut s, knob, 0, mult, 0);
        connect(&mut s, gyro, 0, mult, 1);
        connect(&mut s, mult, 0, split, 1); // data
        connect(&mut s, split, 0, add_rs, 1); // gyro→RS branch
        connect(&mut s, split, 1, add_ls, 1); // gyro→LS branch
        connect(&mut s, asplit, 1, add_rs, 0); // physical RS → virtual RS
        connect(&mut s, asplit, 2, add_ls, 0); // physical LS → virtual LS
        connect(&mut s, add_rs, 0, collect, 1); // → right_stick
        connect(&mut s, add_ls, 0, collect, 2); // → left_stick

        // A select source (dropdown) drives the split's `select` input; the
        // resolver reads the live value from the SOURCE node's `last_out`.
        let sel_src = s.insert_node(p, node("module.dropdown", 0, 1));
        connect(&mut s, sel_src, 0, split, 0);
        if let Some(n) = s.get_node_mut(sel_src) {
            n.extra.last_out = vec![Some(Signal::Float(sel))];
        }
        source_passthrough_pins(&s, knob)
    }

    #[test]
    fn mix_routes_gyro_to_right_stick() {
        // sel = 0.0 → split output 0 (RS branch) active.
        let pins = mix_route(0.0);
        // Feels: physical right stick + gyro IMU pass through.
        assert!(pins.iter().any(|p| p == "right_stick"));
        assert!(pins.iter().any(|p| p == "gyro_x"));
        // The left stick's physical input is NOT dragged in.
        assert!(!pins.iter().any(|p| p == "left_stick"));
        // → left stick is free to adjust the knob.
        assert_eq!(control_input_from_pins(&pins), ControlInput::LeftStick);
    }

    #[test]
    fn mix_routes_gyro_to_left_stick() {
        // sel = 1.0 → split output 1 (LS branch) active.
        let pins = mix_route(1.0);
        assert!(pins.iter().any(|p| p == "left_stick"));
        assert!(pins.iter().any(|p| p == "gyro_x"));
        assert!(!pins.iter().any(|p| p == "right_stick"));
        // → right stick is free to adjust the knob.
        assert_eq!(control_input_from_pins(&pins), ControlInput::RightStick);
    }

    #[test]
    fn control_input_policy() {
        let ls = |v: &[&str]| control_input_from_pins(&v.iter().map(|s| s.to_string()).collect::<Vec<_>>());
        // Right stick felt → left adjusts.
        assert_eq!(ls(&["right_stick", "gyro_x"]), ControlInput::LeftStick);
        // Left stick felt → right adjusts.
        assert_eq!(ls(&["left_stick", "gyro_x"]), ControlInput::RightStick);
        // Both felt → neither free → d-pad adjusts.
        assert_eq!(ls(&["left_stick", "right_stick"]), ControlInput::Dpad);
        // No stick felt → default to left.
        assert_eq!(ls(&["gyro_x"]), ControlInput::LeftStick);
    }

    #[test]
    fn selector_gates_the_upstream_branch() {
        // A selector picking between physical RS (source 0) and gyro (source 1)
        // for the virtual right stick: only the selected source is a leaf.
        let mut s: Snarl<NodeData> = Snarl::new();
        let p = egui::Pos2::ZERO;
        let asplit = {
            let mut n = node("module.automap_split", 1, 2);
            n.params
                .insert("output_pin_ids".into(), json!(["automap_pass", "right_stick"]));
            s.insert_node(p, n)
        };
        let gyro = s.insert_node(p, node("processing.gyro_3dof", 1, 1));
        // selector: input 0 = select, in1 = physical RS, in2 = gyro.
        let sel = s.insert_node(p, node("module.selector", 3, 1));
        let collect = {
            let mut n = node("module.automap_collect", 2, 1);
            n.params
                .insert("collect_input_pin_ids".into(), json!(["right_stick"]));
            s.insert_node(p, n)
        };
        let sel_src = s.insert_node(p, node("module.dropdown", 0, 1));
        connect(&mut s, asplit, 1, sel, 1);
        connect(&mut s, gyro, 0, sel, 2);
        connect(&mut s, sel_src, 0, sel, 0); // select source
        connect(&mut s, sel, 0, collect, 1);

        // select ≈ 0 → source 0 (physical RS).
        if let Some(n) = s.get_node_mut(sel_src) {
            n.extra.last_out = vec![Some(Signal::Float(0.0))];
        }
        let leaves = physical_leaves_of_output(&s, "right_stick");
        assert!(leaves.contains("right_stick"));
        assert!(!leaves.contains("gyro_x"), "gyro branch not selected");

        // select ≈ 1 → source 1 (gyro).
        if let Some(n) = s.get_node_mut(sel_src) {
            n.extra.last_out = vec![Some(Signal::Float(1.0))];
        }
        let leaves = physical_leaves_of_output(&s, "right_stick");
        assert!(leaves.contains("gyro_x"));
        assert!(!leaves.contains("right_stick"), "physical RS not selected");
    }
}
