use std::collections::{HashMap, HashSet, VecDeque};
use std::time::Instant;

use glam::Vec2;
use flexinput_core::{Signal, SignalType, automap};
use serde_json::Value;

use crate::graph::{NodeSnap, ProcessingGraph};
use crate::state::NodeState;

// ── Public output type ────────────────────────────────────────────────────────

pub struct TickOutput {
    /// Latest output per (node_uid, output_pin). Excludes device.source (UI evaluates fresh).
    pub outputs: HashMap<(usize, usize), Option<Signal>>,
    /// Per display node: one scope sample for this tick (uid, per-channel values).
    pub scope_samples: Vec<(usize, Vec<Option<f32>>)>,
    /// Latest inputs per display/response_curve node for UI readout rendering.
    pub last_inputs: HashMap<usize, Vec<Option<Signal>>>,
    /// Latest signals destined for each (device_id, pin_id) sink slot.
    pub sink_outputs: HashMap<(String, String), Signal>,
}

fn apply_deadzone(sig: Signal, dz: f32) -> Signal {
    if dz <= 0.0 { return sig; }
    match sig {
        Signal::Float(v) => {
            let av = v.abs();
            if av < dz { Signal::Float(0.0) }
            else { Signal::Float(v.signum() * (av - dz) / (1.0 - dz).max(f32::EPSILON)) }
        }
        Signal::Vec2(v) => {
            let len = v.length();
            if len < dz { Signal::Vec2(Vec2::ZERO) }
            else { Signal::Vec2(v / len * (len - dz) / (1.0 - dz).max(f32::EPSILON)) }
        }
        other => other,
    }
}

fn combine_signals(a: Signal, b: Signal) -> Signal {
    match (a, b) {
        (Signal::Float(x), Signal::Float(y)) => Signal::Float(x + y),
        (Signal::Vec2(x),  Signal::Vec2(y))  => Signal::Vec2(x + y),
        (Signal::Bool(x),  Signal::Bool(y))  => Signal::Bool(x || y),
        (Signal::Int(x),   Signal::Int(y))   => Signal::Int(x + y),
        (_, b) => b,
    }
}

// ── Sub-patch inner evaluation ────────────────────────────────────────────────

/// Namespaces inner node UIDs under their containing subpatch's UID to avoid
/// collisions in the shared `state` map when multiple subpatches share inner node indices.
#[inline]
pub fn namespaced_uid(outer: usize, inner: usize) -> usize {
    outer.wrapping_shl(20).wrapping_add(inner.wrapping_add(1))
}

/// Evaluates the inner graph of a sub-patch node.
/// Returns the per-node computed signal vectors in inner flat-graph order.
/// `outer_uid` is the UID of the containing meta-module node, used for state namespacing.
/// Inner display nodes (oscilloscope, response_curve, etc.) push samples into
/// `scope_samples`/`last_inputs` keyed by `namespaced_uid` so the UI can render
/// live feedback on inner module bodies (and their pinned mirrors on the outer body).
/// AutoMap collectors inside the subpatch inject into `collector_sigs` using a
/// namespaced key so downstream sinks can pick them up via the same routing path.
fn eval_subgraph(
    graph: &ProcessingGraph,
    outer_inputs: &[Option<Signal>],
    state: &mut HashMap<usize, NodeState>,
    dev_sigs: &HashMap<(String, String), Signal>,
    collector_sigs: &mut HashMap<(String, String), Signal>,
    scope_samples: &mut Vec<(usize, Vec<Option<f32>>)>,
    last_inputs: &mut HashMap<usize, Vec<Option<Signal>>>,
    outer_uid: usize,
    dt: f32,
) -> Vec<Vec<Option<Signal>>> {
    let n = graph.nodes.len();
    let mut computed: Vec<Vec<Option<Signal>>> = vec![vec![]; n];

    for (idx, snap) in graph.nodes.iter().enumerate() {
        // Inlet: produce the corresponding outer input signal.
        if snap.module_id == "subpatch.inlet" {
            let pin_idx = snap.params.get("pin_index")
                .and_then(|v| v.as_u64()).unwrap_or(0) as usize;
            computed[idx] = vec![outer_inputs.get(pin_idx).copied().flatten()];
            continue;
        }

        // Nested subpatch within this subpatch.
        if let Some(ref sg) = snap.inline_subgraph {
            let inner_inputs: Vec<Option<Signal>> = snap.input_sources.iter()
                .map(|src| src.and_then(|(si, op)| {
                    computed.get(si).and_then(|v| v.get(op)).copied().flatten()
                }))
                .collect();
            let nested_uid = namespaced_uid(outer_uid, snap.node_uid);
            let inner_computed = eval_subgraph(
                &sg.graph, &inner_inputs, state, dev_sigs, collector_sigs,
                scope_samples, last_inputs, nested_uid, dt,
            );
            computed[idx] = sg.outlet_locs.iter()
                .map(|loc| loc.and_then(|(ni, np)| inner_computed.get(ni).and_then(|v| v.get(np)).copied().flatten()))
                .collect();
            continue;
        }

        // AutoMap collector inside a subpatch: inject signals into collector_sigs
        // using a namespaced key so it matches what find_automap_device produced.
        if snap.module_id == "module.automap_collect" {
            let inputs: Vec<Option<Signal>> = snap.input_sources.iter()
                .map(|src| src.and_then(|(si, op)| {
                    computed.get(si).and_then(|v| v.get(op)).copied().flatten()
                }))
                .collect();
            let collect_ids = snap.params.get("_collect_pin_ids")
                .and_then(|v| v.as_array())
                .map(|a| a.iter().map(|v| v.as_str().unwrap_or("").to_string()).collect::<Vec<_>>())
                .unwrap_or_default();
            let ns_uid = namespaced_uid(outer_uid, snap.node_uid);
            let uid_key = format!("collector:{}", ns_uid);
            for (i, pin_id) in collect_ids.iter().enumerate() {
                if let Some(sig) = inputs.get(i + 1).and_then(|s| *s) {
                    if !pin_id.is_empty() {
                        collector_sigs.insert((uid_key.clone(), pin_id.clone()), sig);
                    }
                }
            }
            computed[idx] = vec![None];
            continue;
        }

        let inputs: Vec<Option<Signal>> = snap.input_sources.iter()
            .map(|src| src.and_then(|(si, op)| {
                computed.get(si).and_then(|v| v.get(op)).copied().flatten()
            }))
            .collect();

        let ns_uid = namespaced_uid(outer_uid, snap.node_uid);
        let node_state = state.entry(ns_uid).or_insert_with(NodeState::default);
        if let Some(ref vals) = snap.aux_f32_override {
            node_state.aux_f32 = vals.clone();
        }
        let node_outputs = compute_node(snap, &inputs, node_state, dev_sigs, collector_sigs, dt);

        // Display state for inner nodes — keyed by namespaced UID so the UI walk
        // can find them when populating `node.extra.last_signals` / `history`.
        match snap.module_id.as_str() {
            "display.oscilloscope" | "display.readout" => {
                let sample = inputs.iter().map(|s| sig_to_f32(*s)).collect();
                scope_samples.push((ns_uid, sample));
                last_inputs.insert(ns_uid, inputs.clone());
            }
            "display.vectorscope" => {
                let sample = inputs.iter().flat_map(|sig| match sig {
                    Some(Signal::Vec2(v)) => [Some(v.x), Some(v.y)],
                    _ => [None, None],
                }).collect();
                scope_samples.push((ns_uid, sample));
                last_inputs.insert(ns_uid, inputs.clone());
            }
            "module.response_curve" | "module.vec_response_curve" | "module.twoway_response_curve" => {
                last_inputs.insert(ns_uid, inputs.clone());
            }
            "processing.gyro_3dof" => {
                last_inputs.insert(ns_uid, node_outputs.clone());
            }
            _ => {}
        }

        computed[idx] = node_outputs;
    }

    computed
}

// ── Main graph tick ───────────────────────────────────────────────────────────

pub fn eval_graph_tick(
    graph: &ProcessingGraph,
    state: &mut HashMap<usize, NodeState>,
    dev_sigs: &HashMap<(String, String), Signal>,
    dt: f32,
) -> TickOutput {
    let n = graph.nodes.len();
    let mut computed: Vec<Vec<Option<Signal>>> = vec![vec![]; n];

    let mut outputs: HashMap<(usize, usize), Option<Signal>> = HashMap::new();
    let mut scope_samples: Vec<(usize, Vec<Option<f32>>)> = Vec::new();
    let mut last_inputs: HashMap<usize, Vec<Option<Signal>>> = HashMap::new();
    let mut sink_outputs: HashMap<(String, String), Signal> = HashMap::new();
    // Signals injected by AutoMap Collector nodes, keyed by ("collector:{uid}", pin_id).
    let mut collector_sigs: HashMap<(String, String), Signal> = HashMap::new();

    for (idx, snap) in graph.nodes.iter().enumerate() {
        // ── module.automap_collect: inject individual inputs into collector_sigs ──
        if snap.module_id == "module.automap_collect" {
            let inputs: Vec<Option<Signal>> = snap.input_sources.iter()
                .map(|src| src.and_then(|(si, op)| {
                    computed.get(si).and_then(|v| v.get(op)).copied().flatten()
                }))
                .collect();
            let collect_ids = snap.params.get("_collect_pin_ids")
                .and_then(|v| v.as_array())
                .map(|a| a.iter().map(|v| v.as_str().unwrap_or("").to_string()).collect::<Vec<_>>())
                .unwrap_or_default();
            let uid_key = format!("collector:{}", snap.node_uid);
            for (i, pin_id) in collect_ids.iter().enumerate() {
                if let Some(sig) = inputs.get(i + 1).and_then(|s| *s) {
                    if !pin_id.is_empty() {
                        collector_sigs.insert((uid_key.clone(), pin_id.clone()), sig);
                    }
                }
            }
            computed[idx] = vec![None]; // AutoMap passthrough: no signal value
            continue;
        }

        // ── module.automap_fork: gate AutoMap bus to selected output ─────────
        if snap.module_id == "module.automap_fork" {
            let dev_id = snap.params.get("_automap_device_id").and_then(|v| v.as_str()).unwrap_or("");
            let collector_id_upstream = snap.params.get("_automap_collector_id").and_then(|v| v.as_str()).unwrap_or("");
            // inputs[0] = AutoMap (ignored as value), inputs[1] = select
            let select = match snap.input_sources.get(1)
                .and_then(|src| src.and_then(|(si, op)| computed.get(si).and_then(|v| v.get(op)).copied().flatten()))
            {
                Some(Signal::Float(f)) => {
                    let n = snap.n_outputs.max(1);
                    ((f.clamp(0.0, 1.0) * (n as f32 - 1.0 + 0.5)).floor() as usize).min(n - 1)
                }
                Some(Signal::Bool(b)) => if b { 1 } else { 0 },
                _ => 0,
            };
            for out_idx in 0..snap.n_outputs {
                if out_idx != select { continue; }
                let key = format!("forksel:{}:{}", snap.node_uid, out_idx);
                for pin in flexinput_core::automap::ALL_PINS {
                    let sig = if !collector_id_upstream.is_empty() {
                        collector_sigs.get(&(collector_id_upstream.to_string(), pin.id.to_string())).copied()
                            .or_else(|| dev_sigs.get(&(dev_id.to_string(), pin.id.to_string())).copied())
                    } else {
                        dev_sigs.get(&(dev_id.to_string(), pin.id.to_string())).copied()
                    };
                    if let Some(sig) = sig {
                        collector_sigs.insert((key.clone(), pin.id.to_string()), sig);
                    }
                }
            }
            computed[idx] = vec![None; snap.n_outputs];
            continue;
        }

        // ── module.automap_selector: gate selected AutoMap input to output ────
        if snap.module_id == "module.automap_selector" {
            // inputs[0] = select, inputs[1..] = AutoMap buses
            let n_inputs = snap.input_sources.len().saturating_sub(1).max(1);
            let select = match snap.input_sources.get(0)
                .and_then(|src| src.and_then(|(si, op)| computed.get(si).and_then(|v| v.get(op)).copied().flatten()))
            {
                Some(Signal::Float(f)) => {
                    let n = n_inputs as f32;
                    ((f.clamp(0.0, 1.0) * n).floor() as usize).min(n_inputs - 1)
                }
                Some(Signal::Bool(b)) => if b { 1 } else { 0 },
                _ => 0,
            };
            let input_devs = snap.params.get("_automap_input_devs")
                .and_then(|v| v.as_array())
                .map(|a| a.iter().map(|v| v.as_str().unwrap_or("").to_string()).collect::<Vec<_>>())
                .unwrap_or_default();
            let selected_dev = input_devs.get(select).map(|s| s.as_str()).unwrap_or("");
            let key = format!("forksel:{}:0", snap.node_uid);
            if !selected_dev.is_empty() {
                for pin in flexinput_core::automap::ALL_PINS {
                    if let Some(sig) = dev_sigs.get(&(selected_dev.to_string(), pin.id.to_string())).copied() {
                        collector_sigs.insert((key.clone(), pin.id.to_string()), sig);
                    }
                }
            }
            computed[idx] = vec![None];
            continue;
        }

        // ── device.sink: collect combined inputs, populate sink_outputs ──────
        if let Some(ref st) = snap.sink_target {
            // Pins that have at least one actual direct wire (non-empty multi_sources).
            // These take priority over auto-mapped signals for the same pin.
            let directly_wired: HashSet<&str> = st.pin_ids.iter().enumerate()
                .filter(|(i, pid)| !pid.is_empty() && st.multi_sources.get(*i).map_or(false, |s| !s.is_empty()))
                .map(|(_, pid)| pid.as_str())
                .collect();

            // Direct-wire inputs (possibly multi-source per pin, combined additively).
            for (in_idx, pin_id) in st.pin_ids.iter().enumerate() {
                if pin_id.is_empty() { continue; }
                let mut combined: Option<Signal> = None;
                if let Some(sources) = st.multi_sources.get(in_idx) {
                    for &(src_idx, out_pin) in sources {
                        if let Some(Some(sig)) = computed.get(src_idx).and_then(|v| v.get(out_pin)) {
                            combined = Some(match combined {
                                None => *sig,
                                Some(prev) => combine_signals(prev, *sig),
                            });
                        }
                    }
                }
                if let Some(sig) = combined {
                    sink_outputs.insert((st.device_id.clone(), pin_id.clone()), sig);
                }
            }

            // AutoMap: semantic-map source device pins → sink device pins.
            // Uses resolve_mapping() for cross-family translation (e.g. btn_cross → btn_south).
            if let Some((ref src_dev, ref src_pins)) = st.automap_source {
                let dst_ids: Vec<&str> = st.pin_ids.iter()
                    .filter(|pid| !pid.is_empty())
                    .map(|pid| pid.as_str())
                    .collect();
                let src_ids: Vec<&str> = src_pins.iter()
                    .filter(|p| !p.is_empty() && p.as_str() != "automap_out")
                    .map(|p| p.as_str())
                    .collect();
                let is_collector = src_dev.starts_with("collector:") || src_dev.starts_with("forksel:");
                for (mapped_src, mapped_dst) in automap::resolve_mapping(&src_ids, &dst_ids) {
                    if directly_wired.contains(mapped_dst) { continue; }
                    // For collectors (including fork/selector gates): check collector_sigs first,
                    // then fall back to upstream device.
                    let sig_opt = if is_collector {
                        collector_sigs.get(&(src_dev.clone(), mapped_src.to_string())).copied()
                            .or_else(|| {
                                st.automap_fallback_dev.as_ref().and_then(|fb| {
                                    dev_sigs.get(&(fb.clone(), mapped_src.to_string())).copied()
                                })
                            })
                    } else {
                        dev_sigs.get(&(src_dev.clone(), mapped_src.to_string())).copied()
                    };
                    if let Some(sig) = sig_opt {
                        // Type coercion (Bool↔Float) is performed by the virtual device's
                        // send() via Signal::as_float / as_bool, so we just hand the raw
                        // signal off — semantic groups already routed it to the right pin.
                        sink_outputs
                            .entry((st.device_id.clone(), mapped_dst.to_string()))
                            .or_insert(sig);
                    }
                }
                // Wildcard pass-through for virtual keyboard/mouse sinks: forward EVERY
                // collector-injected signal to the sink as-is (using the source pin name
                // verbatim).  The sink's send() handles arbitrary key names through its
                // learned_keys fallback, so users can drive any custom key (F1, Space,
                // letters, …) by adding it to the Collector via the Learn-key UI.
                if is_collector && st.device_id.starts_with("virtual.keymouse") {
                    for ((dev, pin), &sig) in collector_sigs.iter() {
                        if dev != src_dev { continue; }
                        if directly_wired.contains(pin.as_str()) { continue; }
                        sink_outputs
                            .entry((st.device_id.clone(), pin.clone()))
                            .or_insert(sig);
                    }
                }
            }

            // Resolve Vec2 vs individual axis conflicts (they write the same hardware registers).
            // Priority: directly-wired axes beat auto-mapped Vec2; Vec2 wins in all other cases.
            const STICK_GROUPS: &[(&str, &[&str])] = &[
                ("left_stick",  &["left_stick_x", "left_stick_y"]),
                ("right_stick", &["right_stick_x", "right_stick_y"]),
                ("dpad",        &["dpad_x", "dpad_y"]),
            ];
            for &(vec2_pin, axis_pins) in STICK_GROUPS {
                let has_vec2     = sink_outputs.contains_key(&(st.device_id.clone(), vec2_pin.to_string()));
                let has_any_axis = axis_pins.iter().any(|p| sink_outputs.contains_key(&(st.device_id.clone(), p.to_string())));
                if !has_vec2 || !has_any_axis { continue; }
                let vec2_direct     = directly_wired.contains(vec2_pin);
                let any_axis_direct = axis_pins.iter().any(|p| directly_wired.contains(*p));
                if any_axis_direct && !vec2_direct {
                    sink_outputs.remove(&(st.device_id.clone(), vec2_pin.to_string()));
                } else {
                    for &axis_pin in axis_pins {
                        sink_outputs.remove(&(st.device_id.clone(), axis_pin.to_string()));
                    }
                }
            }

            computed[idx] = vec![];
            continue; // no further processing for sink nodes
        }

        // ── inline sub-patch: run inner graph and map outlet outputs ──────────
        if let Some(ref sg) = snap.inline_subgraph {
            let outer_inputs: Vec<Option<Signal>> = snap.input_sources.iter()
                .map(|src| src.and_then(|(si, op)| {
                    computed.get(si).and_then(|v| v.get(op)).copied().flatten()
                }))
                .collect();
            let inner_computed = eval_subgraph(
                &sg.graph, &outer_inputs, state, dev_sigs, &mut collector_sigs,
                &mut scope_samples, &mut last_inputs, snap.node_uid, dt,
            );
            computed[idx] = sg.outlet_locs.iter()
                .map(|loc| loc.and_then(|(ni, np)| inner_computed.get(ni).and_then(|v| v.get(np)).copied().flatten()))
                .collect();
            continue;
        }

        let inputs: Vec<Option<Signal>> = snap.input_sources.iter()
            .map(|src| src.and_then(|(src_idx, out_pin)| {
                computed.get(src_idx).and_then(|v| v.get(out_pin)).copied().flatten()
            }))
            .collect();

        let node_state = state.entry(snap.node_uid).or_insert_with(NodeState::default);

        // Apply any pending state override (e.g. counter reset from UI).
        if let Some(ref vals) = snap.aux_f32_override {
            node_state.aux_f32 = vals.clone();
        }

        let node_outputs = compute_node(snap, &inputs, node_state, dev_sigs, &collector_sigs, dt);

        match snap.module_id.as_str() {
            "display.oscilloscope" | "display.readout" => {
                let sample = inputs.iter().map(|s| sig_to_f32(*s)).collect();
                scope_samples.push((snap.node_uid, sample));
                last_inputs.insert(snap.node_uid, inputs.clone());
            }
            "display.vectorscope" => {
                let sample = inputs.iter().flat_map(|sig| match sig {
                    Some(Signal::Vec2(v)) => [Some(v.x), Some(v.y)],
                    _ => [None, None],
                }).collect();
                scope_samples.push((snap.node_uid, sample));
                last_inputs.insert(snap.node_uid, inputs.clone());
            }
            "module.response_curve" | "module.vec_response_curve" | "module.twoway_response_curve" => {
                last_inputs.insert(snap.node_uid, inputs.clone());
            }
            // Export outputs (not inputs) so the UI body can show a live readout.
            "processing.gyro_3dof" => {
                last_inputs.insert(snap.node_uid, node_outputs.clone());
            }
            _ => {}
        }

        // Exclude device.source from the exported outputs; UI evaluates those fresh.
        if snap.module_id != "device.source" {
            for (out_pin, sig) in node_outputs.iter().enumerate() {
                outputs.insert((snap.node_uid, out_pin), *sig);
            }
        }

        computed[idx] = node_outputs;
    }

    TickOutput { outputs, scope_samples, last_inputs, sink_outputs }
}

// ── Per-node dispatch ─────────────────────────────────────────────────────────

fn compute_node(
    snap: &NodeSnap,
    inputs: &[Option<Signal>],
    state: &mut NodeState,
    dev_sigs: &HashMap<(String, String), Signal>,
    collector_sigs: &HashMap<(String, String), Signal>,
    dt: f32,
) -> Vec<Option<Signal>> {
    match snap.module_id.as_str() {
        "device.source" => {
            let dev_id = snap.device_id.as_deref().unwrap_or("");
            let dz = snap.params.get("deadzone").and_then(|v| v.as_f64()).unwrap_or(0.1) as f32;
            (0..snap.n_outputs).map(|i| {
                let pin_id = snap.output_pin_ids.get(i).map(|s| s.as_str()).unwrap_or("");
                if pin_id.is_empty() { return None; }
                let sig = dev_sigs.get(&(dev_id.to_string(), pin_id.to_string())).copied()?;
                Some(apply_deadzone(sig, dz))
            }).collect()
        }
        "module.automap_split" => {
            let dev_id = snap.params.get("_automap_device_id").and_then(|v| v.as_str()).unwrap_or("");
            // The collector_id (set by build_processing_graph) is the closest
            // upstream collector in the AutoMap wire chain. Splitter prefers its
            // injected/overridden signals over the raw device samples so the
            // probe reflects the most recent state along the chain.
            let collector_id = snap.params.get("_automap_collector_id").and_then(|v| v.as_str()).unwrap_or("");
            (0..snap.n_outputs).map(|i| {
                let pin_id = snap.output_pin_ids.get(i).map(|s| s.as_str()).unwrap_or("");
                // "automap_pass" or empty = the AutoMap passthrough slot — no signal value.
                if pin_id.is_empty() || pin_id == "automap_pass" { return None; }
                if !collector_id.is_empty() {
                    if let Some(&sig) = collector_sigs.get(&(collector_id.to_string(), pin_id.to_string())) {
                        return Some(sig);
                    }
                }
                dev_sigs.get(&(dev_id.to_string(), pin_id.to_string())).copied()
            }).collect()
        }
        "module.constant" | "module.knob" => {
            let v = snap.params.get("value").and_then(|v| v.as_f64()).unwrap_or(0.0) as f32;
            vec![Some(Signal::Float(v))]
        }
        "module.switch" => {
            let a = snap.params.get("active").and_then(|v| v.as_bool()).unwrap_or(false);
            vec![Some(Signal::Bool(a))]
        }
        "generator.oscillator" => {
            let out = compute_oscillator(inputs, state, &snap.params, dt);
            state.last_signals = out.clone();
            out
        }
        "module.delay" => {
            let out = compute_delay(inputs, state, &snap.params);
            state.last_signals = out.clone();
            out
        }
        "module.average" => {
            let out = compute_average(inputs, state, &snap.params);
            state.last_signals = out.clone();
            out
        }
        "module.dc_filter" => {
            let out = compute_dc_filter(inputs, state, &snap.params, dt);
            state.last_signals = out.clone();
            out
        }
        "module.twoway_response_curve" => {
            let out = compute_twoway_response_curve(inputs, state, &snap.params, dt);
            state.last_signals = out.clone();
            out
        }
        "logic.has_changed" => {
            let out = compute_has_changed(inputs, state);
            state.last_signals = out.clone();
            out
        }
        "logic.delay" => {
            let out = compute_logic_delay(inputs, state, &snap.params, dt);
            state.last_signals = out.clone();
            out
        }
        "logic.counter" => {
            let out = compute_counter(inputs, state, &snap.params);
            state.last_signals = out.clone();
            out
        }
        "processing.gyro_3dof" => {
            let out = compute_gyro_3dof(inputs, state, &snap.params, dev_sigs, collector_sigs, dt);
            state.last_signals = out.clone();
            out
        }
        "module.response_curve" | "module.vec_response_curve" => {
            state.last_signals = inputs.to_vec();
            (0..snap.n_outputs).map(|out_idx| {
                eval_pure(&snap.module_id, out_idx, inputs, &snap.params, snap.n_outputs)
            }).collect()
        }
        "display.oscilloscope" | "display.vectorscope" | "display.readout" | "device.sink" => vec![],
        "subpatch.inlet" => vec![],
        "subpatch.outlet" => vec![inputs.first().copied().flatten()],
        id => {
            (0..snap.n_outputs).map(|out_idx| {
                eval_pure(id, out_idx, inputs, &snap.params, snap.n_outputs)
            }).collect()
        }
    }
}

// ── Pure module evaluation ────────────────────────────────────────────────────

pub fn eval_pure(
    id: &str,
    out_idx: usize,
    inputs: &[Option<Signal>],
    params: &HashMap<String, Value>,
    n_outputs: usize,
) -> Option<Signal> {
    let param_f = |name: &str, default: f32| -> f32 {
        params.get(name).and_then(|v| v.as_f64()).map(|f| f as f32).unwrap_or(default)
    };

    match id {
        "math.add" => {
            if inputs.iter().any(|s| matches!(s, Some(Signal::Vec2(_)))) {
                let sum = (0..inputs.len())
                    .map(|i| get_v2(inputs, i, 0.0))
                    .fold(Vec2::ZERO, |acc, v| acc + v);
                Some(Signal::Vec2(sum))
            } else {
                Some(Signal::Float((0..inputs.len()).map(|i| get_f(inputs, i, 0.0)).sum()))
            }
        }
        "math.subtract" => {
            if inputs.iter().any(|s| matches!(s, Some(Signal::Vec2(_)))) {
                let first = get_v2(inputs, 0, 0.0);
                let rest = (1..inputs.len()).map(|i| get_v2(inputs, i, 0.0)).fold(Vec2::ZERO, |acc, v| acc + v);
                Some(Signal::Vec2(first - rest))
            } else {
                let first = get_f(inputs, 0, 0.0);
                let rest: f32 = (1..inputs.len()).map(|i| get_f(inputs, i, 0.0)).sum();
                Some(Signal::Float(first - rest))
            }
        }
        "math.multiply" => {
            if inputs.iter().any(|s| matches!(s, Some(Signal::Vec2(_)))) {
                let first = get_v2(inputs, 0, 0.0);
                let scale = (1..inputs.len()).map(|i| get_v2(inputs, i, 1.0)).fold(Vec2::ONE, |acc, v| acc * v);
                Some(Signal::Vec2(first * scale))
            } else {
                let first = get_f(inputs, 0, 0.0);
                let rest: f32 = (1..inputs.len()).map(|i| get_f(inputs, i, 1.0)).product();
                Some(Signal::Float(first * rest))
            }
        }
        "math.divide" => {
            if inputs.iter().any(|s| matches!(s, Some(Signal::Vec2(_)))) {
                let mut v = get_v2(inputs, 0, 0.0);
                for i in 1..inputs.len() {
                    let d = get_v2(inputs, i, 1.0);
                    v = Vec2::new(
                        if d.x == 0.0 { 0.0 } else { v.x / d.x },
                        if d.y == 0.0 { 0.0 } else { v.y / d.y },
                    );
                }
                Some(Signal::Vec2(v))
            } else {
                let mut v = get_f(inputs, 0, 0.0);
                for i in 1..inputs.len() {
                    let d = get_f(inputs, i, 1.0);
                    v = if d == 0.0 { 0.0 } else { v / d };
                }
                Some(Signal::Float(v))
            }
        }
        "math.abs" => match inputs.get(0).and_then(|s| *s) {
            Some(Signal::Vec2(v)) => Some(Signal::Vec2(v.abs())),
            other => Some(Signal::Float(other.map(|s| s.as_float()).unwrap_or(0.0).abs())),
        },
        "math.negate" => match inputs.get(0).and_then(|s| *s) {
            Some(Signal::Vec2(v)) => Some(Signal::Vec2(-v)),
            other => Some(Signal::Float(-other.map(|s| s.as_float()).unwrap_or(0.0))),
        },
        "math.clamp"  => {
            let min = if inputs.get(1).and_then(|s| *s).is_some() { get_f(inputs, 1, -1.0) } else { param_f("min", -1.0) };
            let max = if inputs.get(2).and_then(|s| *s).is_some() { get_f(inputs, 2,  1.0) } else { param_f("max",  1.0) };
            match inputs.get(0).and_then(|s| *s) {
                Some(Signal::Vec2(v)) => Some(Signal::Vec2(v.clamp(Vec2::splat(min), Vec2::splat(max)))),
                other => Some(Signal::Float(other.map(|s| s.as_float()).unwrap_or(0.0).clamp(min, max))),
            }
        }
        "math.map_range" => {
            let in_min  = if inputs.get(1).and_then(|s| *s).is_some() { get_f(inputs, 1, -1.0) } else { param_f("in_min",  -1.0) };
            let in_max  = if inputs.get(2).and_then(|s| *s).is_some() { get_f(inputs, 2,  1.0) } else { param_f("in_max",   1.0) };
            let out_min = if inputs.get(3).and_then(|s| *s).is_some() { get_f(inputs, 3, -1.0) } else { param_f("out_min", -1.0) };
            let out_max = if inputs.get(4).and_then(|s| *s).is_some() { get_f(inputs, 4,  1.0) } else { param_f("out_max",  1.0) };
            let map = |v: f32| -> f32 {
                let t = if (in_max - in_min).abs() < f32::EPSILON { 0.0 }
                        else { (v - in_min) / (in_max - in_min) };
                out_min + t * (out_max - out_min)
            };
            match inputs.get(0).and_then(|s| *s) {
                Some(Signal::Vec2(v)) => Some(Signal::Vec2(Vec2::new(map(v.x), map(v.y)))),
                other => Some(Signal::Float(map(other.map(|s| s.as_float()).unwrap_or(0.0)))),
            }
        }
        "logic.and"       => Some(Signal::Bool(get_b(inputs, 0, false) && get_b(inputs, 1, false))),
        "logic.or"        => Some(Signal::Bool(get_b(inputs, 0, false) || get_b(inputs, 1, false))),
        "logic.not"       => Some(Signal::Bool(!get_b(inputs, 0, false))),
        "logic.xor"       => Some(Signal::Bool(get_b(inputs, 0, false) ^ get_b(inputs, 1, false))),
        "logic.equal"     => Some(Signal::Bool(get_f(inputs, 0, 0.0) == get_f(inputs, 1, 0.0))),
        "logic.not_equal" => Some(Signal::Bool(get_f(inputs, 0, 0.0) != get_f(inputs, 1, 0.0))),
        "logic.greater_than" => {
            let (a, b) = (get_f(inputs, 0, 0.0), get_f(inputs, 1, 0.0));
            let or_eq = params.get("or_equal").and_then(|v| v.as_bool()).unwrap_or(false);
            Some(Signal::Bool(if or_eq { a >= b } else { a > b }))
        }
        "logic.less_than" => {
            let (a, b) = (get_f(inputs, 0, 0.0), get_f(inputs, 1, 0.0));
            let or_eq = params.get("or_equal").and_then(|v| v.as_bool()).unwrap_or(false);
            Some(Signal::Bool(if or_eq { a <= b } else { a < b }))
        }
        "module.selector" => {
            if out_idx != 0 { return None; }
            let n_inputs = inputs.len().saturating_sub(1);
            let sel = get_f(inputs, 0, 0.0);
            let interp = params.get("interpolate").and_then(|v| v.as_bool()).unwrap_or(false);
            if interp && n_inputs >= 2 {
                let pos = sel.clamp(0.0, 1.0) * (n_inputs - 1) as f32;
                let lo = pos.floor() as usize;
                let hi = (lo + 1).min(n_inputs - 1);
                let t = pos.fract();
                let lo_v = inputs.get(lo + 1).and_then(|s| *s).map(|s| s.as_float()).unwrap_or(0.0);
                let hi_v = inputs.get(hi + 1).and_then(|s| *s).map(|s| s.as_float()).unwrap_or(0.0);
                Some(Signal::Float(lo_v * (1.0 - t) + hi_v * t))
            } else {
                let n = n_inputs as f32;
                let idx = (sel.clamp(0.0, 1.0) * n).floor() as usize;
                let idx = idx.min(n_inputs.saturating_sub(1));
                inputs.get(idx + 1).and_then(|s| *s)
            }
        }
        "module.split" => {
            let sel = get_f(inputs, 0, 0.0);
            let raw = inputs.get(1).and_then(|s| *s);
            let n   = n_outputs;
            let interp = params.get("interpolate").and_then(|v| v.as_bool()).unwrap_or(false);
            let zero_like = |sig: Option<Signal>| -> Signal {
                match sig {
                    Some(Signal::Vec2(_)) => Signal::Vec2(glam::Vec2::ZERO),
                    Some(Signal::Bool(_)) => Signal::Bool(false),
                    Some(Signal::Int(_))  => Signal::Int(0),
                    _                     => Signal::Float(0.0),
                }
            };
            if interp && n >= 2 {
                let pos = sel.clamp(0.0, 1.0) * (n - 1) as f32;
                let lo  = pos.floor() as usize;
                let hi  = (lo + 1).min(n - 1);
                let t   = pos.fract();
                match raw {
                    Some(Signal::Vec2(v)) => {
                        if out_idx == lo && lo == hi { Some(Signal::Vec2(v)) }
                        else if out_idx == lo        { Some(Signal::Vec2(v * (1.0 - t))) }
                        else if out_idx == hi        { Some(Signal::Vec2(v * t)) }
                        else                         { Some(Signal::Vec2(glam::Vec2::ZERO)) }
                    }
                    _ => {
                        let val = raw.map(|s| s.as_float()).unwrap_or(0.0);
                        if out_idx == lo && lo == hi { Some(Signal::Float(val)) }
                        else if out_idx == lo        { Some(Signal::Float(val * (1.0 - t))) }
                        else if out_idx == hi        { Some(Signal::Float(val * t)) }
                        else                         { Some(Signal::Float(0.0)) }
                    }
                }
            } else {
                let idx = (sel.clamp(0.0, 1.0) * n as f32).floor() as usize;
                let idx = idx.min(n.saturating_sub(1));
                if out_idx == idx { Some(raw.unwrap_or(Signal::Float(0.0))) } else { Some(zero_like(raw)) }
            }
        }
        "module.response_curve" => {
            if out_idx >= n_outputs { return None; }
            let x       = get_f(inputs, out_idx, 0.0);
            let pts     = curve_points_from_params(params);
            let biases  = biases_from_params(params);
            let abs     = params.get("absolute").and_then(|v| v.as_bool()).unwrap_or(true);
            let in_max  = params.get("in_max") .and_then(|v| v.as_f64()).unwrap_or(1.0)  as f32;
            let in_min  = params.get("in_min") .and_then(|v| v.as_f64()).unwrap_or(-1.0) as f32;
            let out_max = params.get("out_max").and_then(|v| v.as_f64()).unwrap_or(1.0)  as f32;
            let out_min = params.get("out_min").and_then(|v| v.as_f64()).unwrap_or(-1.0) as f32;
            Some(Signal::Float(apply_curve(x, &pts, &biases, abs, in_min, in_max, out_min, out_max, read_scale_t(params))))
        }
        "module.vec_response_curve" => {
            if out_idx >= n_outputs { return None; }
            let vec = match inputs.get(out_idx).and_then(|s| *s) {
                Some(Signal::Vec2(v)) => v,
                _ => return Some(Signal::Vec2(glam::Vec2::ZERO)),
            };
            let mag = vec.length();
            if mag < f32::EPSILON { return Some(Signal::Vec2(glam::Vec2::ZERO)); }
            let pts     = curve_points_from_params(params);
            let biases  = biases_from_params(params);
            let in_max  = params.get("in_max") .and_then(|v| v.as_f64()).unwrap_or(1.0) as f32;
            let out_max = params.get("out_max").and_then(|v| v.as_f64()).unwrap_or(1.0) as f32;
            let out_mag = apply_curve(mag, &pts, &biases, true, 0.0, in_max, 0.0, out_max, read_scale_t(params));
            Some(Signal::Vec2(vec / mag * out_mag))
        }
        "module.vec_to_axis" => {
            let vec = match inputs.first().and_then(|s| *s) {
                Some(Signal::Vec2(v)) => v,
                _ => glam::Vec2::ZERO,
            };
            match out_idx { 0 => Some(Signal::Float(vec.x)), 1 => Some(Signal::Float(vec.y)), _ => None }
        }
        "module.axis_to_vec" => {
            if out_idx != 0 { return None; }
            let x = match inputs.first().and_then(|s| *s) { Some(Signal::Float(f)) => f, _ => 0.0 };
            let y = match inputs.get(1).and_then(|s| *s)  { Some(Signal::Float(f)) => f, _ => 0.0 };
            Some(Signal::Vec2(glam::Vec2::new(x, y)))
        }
        _ => None,
    }
}

// ── Stateful compute functions ────────────────────────────────────────────────

fn compute_oscillator(
    inputs: &[Option<Signal>],
    state: &mut NodeState,
    params: &HashMap<String, Value>,
    dt: f32,
) -> Vec<Option<Signal>> {
    let shape     = params.get("shape")     .and_then(|v| v.as_str()) .unwrap_or("sine");
    let freq_unit = params.get("freq_unit") .and_then(|v| v.as_str()) .unwrap_or("hz");
    let bipolar   = params.get("bipolar")   .and_then(|v| v.as_bool()).unwrap_or(true);

    let freq_wired  = inputs.get(0).and_then(|s| *s).is_some();
    let phase_wired = inputs.get(1).and_then(|s| *s).is_some();

    let freq_val  = if freq_wired  { get_f(inputs, 0, 1.0) } else { params.get("freq_param") .and_then(|v| v.as_f64()).unwrap_or(1.0) as f32 };
    let phase_off = if phase_wired { get_f(inputs, 1, 0.0) } else { params.get("phase_param").and_then(|v| v.as_f64()).unwrap_or(0.0) as f32 };
    let retrig    = get_b(inputs, 2, false);

    let period_s = match freq_unit {
        "hz" => if freq_val > 0.0 { 1.0 / freq_val } else { 1.0 },
        _    => (freq_val / 1000.0).max(0.0001),
    }.max(0.0001);

    while state.aux_f32.len() < 2 { state.aux_f32.push(0.0); }

    let retrig_edge = retrig && state.aux_f32[1] < 0.5;
    state.aux_f32[1] = if retrig { 1.0 } else { 0.0 };
    if retrig_edge { state.aux_f32[0] = 0.0; }

    state.aux_f32[0] = (state.aux_f32[0] + dt / period_s) % 1.0;
    let phase  = (state.aux_f32[0] + phase_off).rem_euclid(1.0);
    let val    = osc_sample(shape, phase);
    let output = if bipolar { val } else { (val + 1.0) * 0.5 };
    vec![Some(Signal::Float(output))]
}

pub fn osc_sample(shape: &str, phase: f32) -> f32 {
    match shape {
        "sine"     => (phase * std::f32::consts::TAU).sin(),
        "triangle" => if phase < 0.5 { 4.0 * phase - 1.0 } else { 3.0 - 4.0 * phase },
        "saw"      => 2.0 * phase - 1.0,
        "square"   => if phase < 0.5 { 1.0 } else { -1.0 },
        _          => 0.0,
    }
}

fn compute_delay(
    inputs: &[Option<Signal>],
    state: &mut NodeState,
    params: &HashMap<String, Value>,
) -> Vec<Option<Signal>> {
    let delay_secs = params.get("delay_ms").and_then(|v| v.as_f64()).unwrap_or(100.0)
        .clamp(0.0, 60_000.0) as f32 / 1000.0;
    let now = Instant::now();

    while state.delay_bufs.len() < inputs.len() {
        state.delay_bufs.push(VecDeque::new());
    }

    let mut results = Vec::with_capacity(inputs.len());
    for (ch, inp) in inputs.iter().enumerate() {
        let Some(v) = sig_to_f32(*inp) else { results.push(None); continue; };
        let buf = &mut state.delay_bufs[ch];
        buf.push_back((now, v));

        let mut output = buf.front().map(|(_, v)| *v);
        for (ts, val) in buf.iter() {
            if now.duration_since(*ts).as_secs_f32() >= delay_secs { output = Some(*val); }
            else { break; }
        }

        let max_age = delay_secs + 1.0;
        while buf.len() > 2 {
            let oldest_age = now.duration_since(buf.front().unwrap().0).as_secs_f32();
            if oldest_age > max_age { buf.pop_front(); } else { break; }
        }

        results.push(output.map(Signal::Float));
    }
    results
}

fn compute_average(
    inputs: &[Option<Signal>],
    state: &mut NodeState,
    params: &HashMap<String, Value>,
) -> Vec<Option<Signal>> {
    let buf_size = params.get("buf_size").and_then(|v| v.as_f64())
        .map(|f| f as u64).unwrap_or(10).clamp(1, 10_000) as usize;
    let spike_mad = params.get("spike_mad").and_then(|v| v.as_f64()).unwrap_or(0.0).max(0.0);

    while state.avg_bufs.len()    < inputs.len() { state.avg_bufs.push(VecDeque::new()); }
    while state.avg_bufs_v2.len() < inputs.len() { state.avg_bufs_v2.push(VecDeque::new()); }

    let mut results = Vec::with_capacity(inputs.len());
    for (ch, inp) in inputs.iter().enumerate() {
        match inp {
            Some(Signal::Vec2(v)) => {
                let buf = &mut state.avg_bufs_v2[ch];
                buf.push_back(*v);
                while buf.len() > buf_size { buf.pop_front(); }

                let avg = if spike_mad > 0.0 && buf.len() >= 3 {
                    Vec2::new(
                        mad_average(buf.iter().map(|v| v.x), spike_mad as f32),
                        mad_average(buf.iter().map(|v| v.y), spike_mad as f32),
                    )
                } else {
                    buf.iter().copied().sum::<Vec2>() / buf.len() as f32
                };
                results.push(Some(Signal::Vec2(avg)));
            }
            inp => {
                let Some(v) = sig_to_f32(*inp) else { results.push(None); continue; };
                let buf = &mut state.avg_bufs[ch];
                buf.push_back(v);
                while buf.len() > buf_size { buf.pop_front(); }

                let avg = if spike_mad > 0.0 && buf.len() >= 3 {
                    mad_average(buf.iter().copied(), spike_mad as f32)
                } else {
                    buf.iter().sum::<f32>() / buf.len() as f32
                };
                results.push(Some(Signal::Float(avg)));
            }
        }
    }
    results
}

fn mad_average(values: impl Iterator<Item = f32> + Clone, spike_mad: f32) -> f32 {
    let mut sorted: Vec<f32> = values.collect();
    sorted.sort_by(|a, b| a.total_cmp(b));
    let median = sorted_median(&sorted);
    let mut devs: Vec<f32> = sorted.iter().map(|&x| (x - median).abs()).collect();
    devs.sort_by(|a, b| a.total_cmp(b));
    let mad = sorted_median(&devs);
    if mad < 1e-9 {
        sorted.iter().sum::<f32>() / sorted.len() as f32
    } else {
        let thresh = spike_mad * mad;
        let kept: Vec<f32> = sorted.iter().cloned().filter(|&x| (x - median).abs() <= thresh).collect();
        if kept.is_empty() { sorted.iter().sum::<f32>() / sorted.len() as f32 }
        else { kept.iter().sum::<f32>() / kept.len() as f32 }
    }
}

fn sorted_median(sorted: &[f32]) -> f32 {
    let n = sorted.len();
    if n == 0 { return 0.0; }
    if n % 2 == 0 { (sorted[n / 2 - 1] + sorted[n / 2]) / 2.0 } else { sorted[n / 2] }
}

const DC_THRESHOLD: f64    = 0.005;
const DC_STABILITY: f64    = 0.02;
const DC_FAST_TC_SECS: f64 = 0.05;

fn compute_dc_filter(
    inputs: &[Option<Signal>],
    state: &mut NodeState,
    params: &HashMap<String, Value>,
    dt: f32,
) -> Vec<Option<Signal>> {
    let window_secs = params.get("window_ms").and_then(|v| v.as_f64()).unwrap_or(500.0)
        .clamp(10.0, 60_000.0) as f32 / 1000.0;
    let decay_secs = params.get("decay_ms").and_then(|v| v.as_f64()).unwrap_or(200.0)
        .clamp(10.0, 60_000.0) / 1000.0;

    let dt64       = dt as f64;
    let alpha_fast = 1.0 - (-dt64 / DC_FAST_TC_SECS).exp();
    let alpha_est  = 1.0 - (-dt64 / window_secs as f64).exp();
    let alpha_corr = 1.0 - (-dt64 / decay_secs).exp();
    let blend_step = dt as f64 / decay_secs;

    while state.dc_fast.len()        < inputs.len() { state.dc_fast.push(0.0); }
    while state.dc_estimates.len()   < inputs.len() { state.dc_estimates.push(0.0); }
    while state.dc_corrections.len() < inputs.len() { state.dc_corrections.push(0.0); }
    while state.dc_timers.len()      < inputs.len() { state.dc_timers.push(0.0); }
    while state.dc_frozen.len()      < inputs.len() { state.dc_frozen.push(0.0); }
    while state.dc_blend.len()       < inputs.len() { state.dc_blend.push(0.0); }

    let mut results = Vec::with_capacity(inputs.len());
    for (ch, inp) in inputs.iter().enumerate() {
        let Some(v) = sig_to_f32(*inp) else { results.push(None); continue; };
        let v64 = v as f64;

        state.dc_fast[ch]      += alpha_fast * (v64 - state.dc_fast[ch]);
        state.dc_estimates[ch] += alpha_est  * (v64 - state.dc_estimates[ch]);

        let is_stable  = (state.dc_fast[ch] - state.dc_estimates[ch]).abs() < DC_STABILITY;
        let is_nonzero = state.dc_estimates[ch].abs() > DC_THRESHOLD;

        if is_stable && is_nonzero { state.dc_timers[ch] = (state.dc_timers[ch] + dt).min(window_secs + 1.0); }
        else                       { state.dc_timers[ch] = 0.0; }

        let output = if is_stable {
            if state.dc_timers[ch] >= window_secs {
                state.dc_corrections[ch] += alpha_corr * (state.dc_estimates[ch] - state.dc_corrections[ch]);
            } else {
                state.dc_corrections[ch] += alpha_corr * (0.0 - state.dc_corrections[ch]);
            }
            let out = v64 - state.dc_corrections[ch];
            state.dc_frozen[ch] = out;
            state.dc_blend[ch]  = 0.0;
            out
        } else {
            state.dc_blend[ch] = (state.dc_blend[ch] + blend_step).min(1.0);
            let b   = state.dc_blend[ch];
            let out = state.dc_frozen[ch] * (1.0 - b) + v64 * b;
            state.dc_corrections[ch] = v64 - out;
            out
        };
        results.push(Some(Signal::Float(output as f32)));
    }
    results
}

fn compute_twoway_response_curve(
    inputs: &[Option<Signal>],
    state: &mut NodeState,
    params: &HashMap<String, Value>,
    dt: f32,
) -> Vec<Option<Signal>> {
    let n_ch = inputs.len();

    // Grow per-channel state vectors lazily.
    while state.twoway_lane.len()       < n_ch { state.twoway_lane.push(1); }
    while state.twoway_dir_buf.len()    < n_ch { state.twoway_dir_buf.push(VecDeque::new()); }
    while state.twoway_blend.len()      < n_ch { state.twoway_blend.push(1.0); }
    while state.twoway_prev_input.len() < n_ch { state.twoway_prev_input.push(0.0); }
    while state.twoway_old_output.len() < n_ch { state.twoway_old_output.push(0.0); }

    // Shared params (applied to both curves).
    let abs     = params.get("absolute").and_then(|v| v.as_bool()).unwrap_or(true);
    let in_max  = params.get("in_max") .and_then(|v| v.as_f64()).unwrap_or(1.0)  as f32;
    let in_min  = params.get("in_min") .and_then(|v| v.as_f64()).unwrap_or(-1.0) as f32;
    let out_max = params.get("out_max").and_then(|v| v.as_f64()).unwrap_or(1.0)  as f32;
    let out_min = params.get("out_min").and_then(|v| v.as_f64()).unwrap_or(-1.0) as f32;
    let scale_t = read_scale_t(params);

    let vec_mode = params.get("vec_mode").and_then(|v| v.as_bool()).unwrap_or(false);

    // Up-lane (rising) curve params.
    let pts_up   = curve_points_from_params(params);
    let biases_up = biases_from_params(params);

    // Down-lane (falling) curve uses "_dn"-suffixed params, falling back to up-lane.
    let pts_dn: Vec<[f32; 2]> = params.get("points_dn")
        .and_then(|v| v.as_array())
        .map(|arr| arr.iter().filter_map(|p| {
            let a = p.as_array()?;
            Some([a.get(0)?.as_f64()? as f32, a.get(1)?.as_f64()? as f32])
        }).collect())
        .unwrap_or_else(|| pts_up.clone());
    let biases_dn: Vec<f32> = params.get("biases_dn")
        .and_then(|v| v.as_array())
        .map(|arr| arr.iter().filter_map(|b| b.as_f64().map(|f| f as f32)).collect())
        .unwrap_or_else(|| biases_up.clone());

    // Hysteresis params.
    let hyst_pct  = params.get("hysteresis_pct").and_then(|v| v.as_f64()).unwrap_or(0.5) as f32;
    let hyst_ms   = params.get("hysteresis_ms") .and_then(|v| v.as_f64()).unwrap_or(20.0) as f32;
    let interp_ms = params.get("interp_ms")     .and_then(|v| v.as_f64()).unwrap_or(50.0) as f32;

    let hyst_ticks = ((hyst_ms / 1000.0) / dt).ceil() as usize;
    let hyst_ticks = hyst_ticks.max(1);

    let abs_max   = in_max.abs().max(in_min.abs()).max(f32::EPSILON);
    let threshold = hyst_pct / 100.0 * abs_max;

    let interp_step = if interp_ms > 0.0 { dt / (interp_ms / 1000.0) } else { 1.0 };

    let mut results = Vec::with_capacity(n_ch);

    for ch in 0..n_ch {
        let raw_input = if vec_mode {
            match inputs.get(ch).and_then(|s| *s) {
                Some(Signal::Vec2(v)) => v.length(),
                Some(Signal::Float(f)) => f,
                _ => { results.push(None); continue; }
            }
        } else {
            match inputs.get(ch).and_then(|s| *s) {
                Some(Signal::Float(f)) => f,
                _ => { results.push(None); continue; }
            }
        };

        // Hysteresis: track displacement from a reference point.
        // twoway_prev_input = reference position (reset when direction reverses or lane commits).
        // twoway_dir_buf    = consistent-direction tick counter (push one item per tick; clear on reversal).
        //
        // A lane switch fires when:
        //   1. displacement from reference exceeds threshold, AND
        //   2. that displacement has been sustained for hyst_ticks consecutive ticks.
        //
        // This correctly handles slow movements (displacement accumulates across many ticks)
        // and filters jitter (jitter reverses direction and resets the tick counter).
        let reference = state.twoway_prev_input[ch];
        let displacement = raw_input - reference;
        let dir_buf = &mut state.twoway_dir_buf[ch];

        if displacement > threshold {
            dir_buf.push_back(1.0);
            // Direction reversed: start counting from now, update reference if needed
            if dir_buf.front().copied().unwrap_or(1.0) < 0.0 {
                dir_buf.clear();
                dir_buf.push_back(1.0);
                state.twoway_prev_input[ch] = raw_input;
            }
        } else if displacement < -threshold {
            dir_buf.push_back(-1.0);
            if dir_buf.front().copied().unwrap_or(-1.0) > 0.0 {
                dir_buf.clear();
                dir_buf.push_back(-1.0);
                state.twoway_prev_input[ch] = raw_input;
            }
        } else {
            // Within deadband: reset counter, update reference to current position
            dir_buf.clear();
            state.twoway_prev_input[ch] = raw_input;
        }
        // Cap counter length to hyst_ticks
        while dir_buf.len() > hyst_ticks { dir_buf.pop_front(); }

        let all_up   = dir_buf.len() >= hyst_ticks && dir_buf.iter().all(|&d| d > 0.0);
        let all_down = dir_buf.len() >= hyst_ticks && dir_buf.iter().all(|&d| d < 0.0);

        let prev_lane = state.twoway_lane[ch];
        if all_up   && prev_lane != 1  {
            state.twoway_old_output[ch] = state.twoway_blend[ch]
                .mul_add(apply_curve(raw_input, &pts_up, &biases_up, abs, in_min, in_max, out_min, out_max, scale_t),
                    (1.0 - state.twoway_blend[ch]) * state.twoway_old_output[ch]);
            state.twoway_lane[ch]  =  1;
            state.twoway_blend[ch] = 0.0;
            // Reset reference so hysteresis measures from the new committed position
            state.twoway_dir_buf[ch].clear();
            state.twoway_prev_input[ch] = raw_input;
        } else if all_down && prev_lane != -1 {
            state.twoway_old_output[ch] = state.twoway_blend[ch]
                .mul_add(apply_curve(raw_input, &pts_dn, &biases_dn, abs, in_min, in_max, out_min, out_max, scale_t),
                    (1.0 - state.twoway_blend[ch]) * state.twoway_old_output[ch]);
            state.twoway_lane[ch]  = -1;
            state.twoway_blend[ch] = 0.0;
            state.twoway_dir_buf[ch].clear();
            state.twoway_prev_input[ch] = raw_input;
        }

        // Advance blend.
        state.twoway_blend[ch] = (state.twoway_blend[ch] + interp_step).min(1.0);
        let blend = state.twoway_blend[ch];

        // Evaluate active-lane curve.
        let new_output = if state.twoway_lane[ch] >= 0 {
            apply_curve(raw_input, &pts_up, &biases_up, abs, in_min, in_max, out_min, out_max, scale_t)
        } else {
            apply_curve(raw_input, &pts_dn, &biases_dn, abs, in_min, in_max, out_min, out_max, scale_t)
        };

        let output = blend * new_output + (1.0 - blend) * state.twoway_old_output[ch];

        let sig = if vec_mode {
            match inputs.get(ch).and_then(|s| *s) {
                Some(Signal::Vec2(v)) => {
                    let mag = v.length();
                    if mag < f32::EPSILON { Signal::Vec2(glam::Vec2::ZERO) }
                    else { Signal::Vec2(v / mag * output) }
                }
                _ => Signal::Float(output),
            }
        } else {
            Signal::Float(output)
        };

        results.push(Some(sig));
    }

    results
}

fn compute_has_changed(
    inputs: &[Option<Signal>],
    state: &mut NodeState,
) -> Vec<Option<Signal>> {
    let cur = inputs.first().copied().flatten();
    while state.prev_signals.len() < 1 { state.prev_signals.push(None); }
    let prev = state.prev_signals[0];
    state.prev_signals[0] = cur;

    let (changed, increased, decreased) = match (prev, cur) {
        (Some(p), Some(c)) => {
            let ch = p != c;
            let (ps, cs) = (sig_scalar(p), sig_scalar(c));
            (ch, cs > ps, cs < ps)
        }
        (None, Some(_)) => (true, false, false),
        _ => (false, false, false),
    };
    vec![Some(Signal::Bool(changed)), Some(Signal::Bool(increased)), Some(Signal::Bool(decreased))]
}

fn compute_logic_delay(
    inputs: &[Option<Signal>],
    state: &mut NodeState,
    params: &HashMap<String, Value>,
    dt: f32,
) -> Vec<Option<Signal>> {
    let mode      = params.get("mode").and_then(|v| v.as_str()).unwrap_or("delay_false");
    let time      = params.get("time").and_then(|v| v.as_f64()).unwrap_or(100.0) as f32;
    let use_ms    = params.get("unit").and_then(|v| v.as_str()).unwrap_or("ms") == "ms";
    let threshold = if use_ms { time / 1000.0 } else { time };
    let tick      = if use_ms { dt } else { 1.0 };

    while state.aux_f32.len() < 2 { state.aux_f32.push(0.0); }
    let mode_code = if mode == "delay_true" { 0.0f32 } else { 1.0f32 };
    if state.aux_f32[1] != mode_code {
        state.aux_f32[0] = if mode == "delay_true" { 0.0 } else { threshold };
        state.aux_f32[1] = mode_code;
    }

    let input = inputs.first().copied().flatten()
        .and_then(|s| s.coerce_to(SignalType::Bool))
        .map(|s| matches!(s, Signal::Bool(true)))
        .unwrap_or(false);

    let timer  = &mut state.aux_f32[0];
    let output = match mode {
        "delay_true" => { if input { *timer += tick; *timer >= threshold } else { *timer = 0.0; false } }
        _            => { if input { *timer = 0.0; true } else { *timer += tick; *timer < threshold } }
    };
    vec![Some(Signal::Bool(output))]
}

fn compute_counter(
    inputs: &[Option<Signal>],
    state: &mut NodeState,
    params: &HashMap<String, Value>,
) -> Vec<Option<Signal>> {
    let mode       = params.get("mode")      .and_then(|v| v.as_str()) .unwrap_or("loop");
    let normalized = params.get("normalized").and_then(|v| v.as_bool()).unwrap_or(false);

    let step_wired = inputs.get(3).and_then(|s| *s).is_some();
    let min_wired  = inputs.get(4).and_then(|s| *s).is_some();
    let max_wired  = inputs.get(5).and_then(|s| *s).is_some();

    let step = (if step_wired { get_f(inputs, 3, 1.0)  } else { params.get("step_param").and_then(|v| v.as_f64()).unwrap_or(1.0)  as f32 }).max(f32::EPSILON);
    let min  =  if min_wired  { get_f(inputs, 4, 0.0)  } else { params.get("min_param") .and_then(|v| v.as_f64()).unwrap_or(0.0)  as f32 };
    let max  =  if max_wired  { get_f(inputs, 5, 10.0) } else { params.get("max_param") .and_then(|v| v.as_f64()).unwrap_or(10.0) as f32 };

    let max_steps = ((max - min) / step).round().max(0.0) as i32;

    while state.aux_f32.len() < 5 { state.aux_f32.push(0.0); }
    if state.aux_f32[1] == 0.0 { state.aux_f32[1] = 1.0; }

    let inc   = get_b(inputs, 0, false);
    let dec   = get_b(inputs, 1, false);
    let reset = get_b(inputs, 2, false);

    let inc_edge   = inc   && state.aux_f32[2] < 0.5;
    let dec_edge   = dec   && state.aux_f32[3] < 0.5;
    let reset_edge = reset && state.aux_f32[4] < 0.5;

    state.aux_f32[2] = if inc   { 1.0 } else { 0.0 };
    state.aux_f32[3] = if dec   { 1.0 } else { 0.0 };
    state.aux_f32[4] = if reset { 1.0 } else { 0.0 };

    let mut count = state.aux_f32[0] as i32;
    let mut dir   = state.aux_f32[1];

    if reset_edge {
        count = 0; dir = 1.0;
    } else {
        match mode {
            "loop" => {
                if inc_edge { count = (count + 1).rem_euclid(max_steps + 1); }
                if dec_edge { count = (count - 1).rem_euclid(max_steps + 1); }
            }
            "limit" => {
                if inc_edge { count = (count + 1).min(max_steps); }
                if dec_edge { count = (count - 1).max(0); }
            }
            "bounce" => {
                if max_steps > 0 {
                    if inc_edge { count += 1; }
                    if dec_edge { count -= 1; }
                    if count > max_steps { count = 2 * max_steps - count; }
                    if count < 0         { count = -count; }
                }
            }
            _ => {
                if inc_edge { count += 1; }
                if dec_edge { count = (count - 1).max(0); }
            }
        }
    }

    if mode != "unlimited" { count = count.clamp(0, max_steps); }
    state.aux_f32[0] = count as f32;
    state.aux_f32[1] = dir;

    let output = if normalized {
        if max_steps > 0 { count as f32 / max_steps as f32 } else { 0.0 }
    } else {
        min + count as f32 * step
    };
    vec![Some(Signal::Float(output))]
}

fn compute_gyro_3dof(
    inputs: &[Option<Signal>],
    state: &mut NodeState,
    params: &HashMap<String, Value>,
    dev_sigs: &HashMap<(String, String), Signal>,
    collector_sigs: &HashMap<(String, String), Signal>,
    dt: f32,
) -> Vec<Option<Signal>> {
    let mode = params.get("mode").and_then(|v| v.as_str()).unwrap_or("local");

    let inv = |name: &str| -> f32 {
        if params.get(name).and_then(|v| v.as_bool()).unwrap_or(false) { -1.0 } else { 1.0 }
    };

    // Auto-map path: read all six axes from the connected device.
    // If upstream is a fork/selector/collector, read from collector_sigs first.
    let (gx_am, gy_am, gz_am, ax_am, ay_am, az_am) =
        if let Some(dev_id) = params.get("_automap_device_id").and_then(|v| v.as_str()) {
            let collector_id = params.get("_automap_collector_id").and_then(|v| v.as_str()).unwrap_or("");
            let get = |pin: &str| -> f32 {
                if !collector_id.is_empty() {
                    if let Some(Signal::Float(f)) = collector_sigs.get(&(collector_id.to_string(), pin.to_string())) {
                        return *f;
                    }
                }
                match dev_sigs.get(&(dev_id.to_string(), pin.to_string())) {
                    Some(Signal::Float(f)) => *f,
                    _ => 0.0,
                }
            };
            let az_raw = {
                let pin = "accel_z";
                if !collector_id.is_empty() {
                    if let Some(Signal::Float(f)) = collector_sigs.get(&(collector_id.to_string(), pin.to_string())) {
                        *f
                    } else {
                        match dev_sigs.get(&(dev_id.to_string(), pin.to_string())) {
                            Some(Signal::Float(f)) => *f,
                            _ => 1.0,
                        }
                    }
                } else {
                    match dev_sigs.get(&(dev_id.to_string(), pin.to_string())) {
                        Some(Signal::Float(f)) => *f,
                        _ => 1.0,
                    }
                }
            };
            (get("gyro_x"), get("gyro_y"), get("gyro_z"), get("accel_x"), get("accel_y"), az_raw)
        } else {
            (0.0, 0.0, 0.0, 0.0, 0.0, 1.0)
        };

    // Direct pin overrides (inputs 2–7: Gyro X/Y/Z, Accel X/Y/Z).
    // A wired pin supersedes the auto-map value for that axis only.
    let pin_or = |idx: usize, fallback: f32| -> f32 {
        if inputs.get(idx).and_then(|s| *s).is_some() { get_f(inputs, idx, fallback) } else { fallback }
    };
    let gx = pin_or(2, gx_am) * inv("inv_roll");
    let gy = pin_or(3, gy_am);   // pitch inversion applied to out_y below
    let gz = pin_or(4, gz_am);   // yaw   inversion applied to out_x below
    let ax = pin_or(5, ax_am) * inv("inv_accel_x");
    let ay = pin_or(6, ay_am) * inv("inv_accel_y");
    let az = pin_or(7, az_am) * inv("inv_accel_z");

    // aux_f32: [0]=laser_x, [1]=laser_y, [2]=smooth_gvx, [3]=smooth_gvy, [4]=smooth_gvz, [5]=prev_reset
    while state.aux_f32.len() < 6 { state.aux_f32.push(0.0); }

    // Laser reset: inputs[1] is the optional Reset bool pin.
    let reset_now = get_b(inputs, 1, false);
    let reset_edge = reset_now && state.aux_f32[5] < 0.5;
    state.aux_f32[5] = if reset_now { 1.0 } else { 0.0 };
    if reset_edge { state.aux_f32[0] = 0.0; state.aux_f32[1] = 0.0; }

    let (out_x_raw, out_y_raw) = match mode {
        "player" | "world" => {
            let gyro  = glam::Vec3::new(gx, gy, gz);
            let accel = glam::Vec3::new(ax, ay, az);

            // Low-pass filter the gravity estimate so that fast pitch oscillations do not
            // alias into g_hat, which would cause world_yaw = dot(gyro, g_hat) to oscillate
            // at 2× the pitch frequency — the "figure-8" Lissajous artifact.
            // Player: 1 s time constant tracks slow resting-orientation changes.
            // World:  3 s time constant gives a very stable reference frame.
            let tau = if mode == "world" { 3.0_f32 } else { 1.0_f32 };
            let alpha = 1.0 - (-dt / tau).exp();
            let acc_mag = accel.length();
            if acc_mag > 0.01 {
                let norm = accel / acc_mag;
                state.aux_f32[2] += alpha * (norm.x - state.aux_f32[2]);
                state.aux_f32[3] += alpha * (norm.y - state.aux_f32[3]);
                state.aux_f32[4] += alpha * (norm.z - state.aux_f32[4]);
            }
            let sg = glam::Vec3::new(state.aux_f32[2], state.aux_f32[3], state.aux_f32[4]);
            let sg_len = sg.length();
            let g_hat = if sg_len > 0.01 { sg / sg_len } else { glam::Vec3::new(0.0, 0.0, 1.0) };

            let world_yaw   = gyro.dot(g_hat);
            let gyro_no_yaw = gyro - world_yaw * g_hat;
            (world_yaw, gyro_no_yaw.y)
        }
        "laser" => {
            state.aux_f32[0] += gz * dt;
            state.aux_f32[1] += gy * dt;
            (state.aux_f32[0], state.aux_f32[1])
        }
        _ => (gz, gy), // "local": gz=yaw→X, gy=pitch→Y
    };

    // Apply yaw/pitch inversions to final output only (not inside the dot-product math).
    let out_x = out_x_raw * inv("inv_yaw");
    let out_y = out_y_raw * inv("inv_pitch");

    let out_vec = glam::Vec2::new(out_x, out_y);
    vec![
        Some(Signal::Vec2(out_vec)),
        Some(Signal::Float(out_x)),
        Some(Signal::Float(out_y)),
    ]
}

// ── Curve helpers ─────────────────────────────────────────────────────────────

pub fn sample_curve(pts: &[[f32; 2]], x: f32, biases: &[f32]) -> f32 {
    match pts.len() {
        0 => x,
        1 => pts[0][1],
        _ => {
            if x <= pts[0][0] { return pts[0][1]; }
            let last = pts.len() - 1;
            if x >= pts[last][0] { return pts[last][1]; }
            let seg = pts.windows(2).position(|w| x <= w[1][0]).unwrap_or(last - 1);
            let p1 = pts[seg]; let p2 = pts[seg + 1];
            let t    = (x - p1[0]) / (p2[0] - p1[0]);
            let bias = biases.get(seg).copied().unwrap_or(0.0);
            let base = p1[1] + (p2[1] - p1[1]) * t;
            base + bias * 4.0 * t * (1.0 - t)
        }
    }
}

pub fn apply_curve(
    x: f32, pts: &[[f32; 2]], biases: &[f32],
    absolute: bool, in_min: f32, in_max: f32, out_min: f32, out_max: f32, scale_t: f32,
) -> f32 {
    if absolute {
        let sign     = if x < 0.0 { -1.0f32 } else { 1.0 };
        let abs_max  = in_max.abs().max(in_min.abs()).max(f32::EPSILON);
        let abs_norm = (x.abs() / abs_max).clamp(0.0, 1.0);
        let scaled   = curve_scale(abs_norm, scale_t);
        let curve_y  = sample_curve(pts, scaled, biases).clamp(0.0, 1.0);
        let out_y    = curve_scale_inv(curve_y, scale_t);
        sign * out_y * out_max.abs().max(out_min.abs())
    } else {
        let in_range  = (in_max - in_min).abs().max(f32::EPSILON);
        let out_range = out_max - out_min;
        let norm      = ((x - in_min) / in_range * 2.0 - 1.0).clamp(-1.0, 1.0);
        let sign      = if norm < 0.0 { -1.0f32 } else { 1.0 };
        let scaled    = sign * curve_scale(norm.abs(), scale_t);
        let curve_y   = sample_curve(pts, scaled, biases);
        let sign_out  = if curve_y < 0.0 { -1.0f32 } else { 1.0 };
        let out_y     = sign_out * curve_scale_inv(curve_y.abs(), scale_t);
        out_min + (out_y.clamp(-1.0, 1.0) + 1.0) * 0.5 * out_range
    }
}

pub fn curve_scale(x: f32, t: f32) -> f32 {
    if t.abs() < 1e-4 { return x; }
    x.clamp(0.0, 1.0).powf(2.0f32.powf(t * 3.0))
}

pub fn curve_scale_inv(y: f32, t: f32) -> f32 {
    if t.abs() < 1e-4 { return y; }
    y.clamp(0.0, 1.0).powf(1.0 / 2.0f32.powf(t * 3.0))
}

pub fn curve_points_from_params(params: &HashMap<String, Value>) -> Vec<[f32; 2]> {
    let absolute = params.get("absolute").and_then(|v| v.as_bool()).unwrap_or(true);
    params.get("points").and_then(|v| v.as_array()).map(|arr| {
        arr.iter().filter_map(|pt| {
            let a = pt.as_array()?;
            Some([a.get(0)?.as_f64()? as f32, a.get(1)?.as_f64()? as f32])
        }).collect()
    }).unwrap_or_else(|| {
        if absolute { vec![[0.0, 0.0], [1.0, 1.0]] } else { vec![[-1.0, -1.0], [1.0, 1.0]] }
    })
}

pub fn biases_from_params(params: &HashMap<String, Value>) -> Vec<f32> {
    params.get("biases").and_then(|v| v.as_array()).map(|arr| {
        arr.iter().map(|v| v.as_f64().unwrap_or(0.0) as f32).collect()
    }).unwrap_or_default()
}

pub fn read_scale_t(params: &HashMap<String, Value>) -> f32 {
    params.get("scale_t").and_then(|v| v.as_f64()).map(|f| f as f32)
        .unwrap_or_else(|| match params.get("in_scale").and_then(|v| v.as_i64()).unwrap_or(0) {
            1 => -0.5,
            2 =>  0.5,
            _ =>  0.0,
        })
}

// ── Helpers ───────────────────────────────────────────────────────────────────

pub fn sig_to_f32(s: Option<Signal>) -> Option<f32> {
    match s {
        Some(Signal::Float(f)) => Some(f),
        Some(Signal::Bool(b))  => Some(if b { 1.0 } else { 0.0 }),
        Some(Signal::Vec2(v))  => Some(v.length()),
        Some(Signal::Int(i))   => Some(i as f32),
        None => None,
    }
}

pub fn get_f(inputs: &[Option<Signal>], i: usize, default: f32) -> f32 {
    inputs.get(i).and_then(|s| *s).map(|s| s.as_float()).unwrap_or(default)
}

pub fn get_b(inputs: &[Option<Signal>], i: usize, default: bool) -> bool {
    inputs.get(i).and_then(|s| *s).map(|s| s.as_bool()).unwrap_or(default)
}

/// Lift input slot to Vec2: Vec2 passes through, scalars are splatted, None → splat(default).
fn get_v2(inputs: &[Option<Signal>], i: usize, default: f32) -> Vec2 {
    match inputs.get(i).and_then(|s| *s) {
        Some(Signal::Vec2(v)) => v,
        Some(other) => Vec2::splat(other.as_float()),
        None => Vec2::splat(default),
    }
}

fn sig_scalar(s: Signal) -> f32 {
    match s {
        Signal::Float(f) => f,
        Signal::Int(i)   => i as f32,
        Signal::Bool(b)  => if b { 1.0 } else { 0.0 },
        Signal::Vec2(v)  => v.length(),
    }
}
