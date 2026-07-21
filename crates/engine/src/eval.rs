use std::collections::{HashMap, HashSet, VecDeque};
use std::time::Instant;

use glam::Vec2;
use flexinput_core::{Signal, SignalType, automap};
use serde_json::Value;

use crate::graph::{NodeSnap, ProcessingGraph};
use crate::state::NodeState;

// The evaluator is split across `eval/`; this file keeps the graph tick itself
// (`eval_graph_tick` / `eval_subgraph`, which share a dispatch and must not be
// separated) plus the module-body evaluators the tick calls into.
//
// Children are re-exported with `pub use` rather than `pub(crate) use` because
// `eval::` is a public module: paths like `flexinput_engine::eval::sample_curve`
// are consumed outside this crate, and a crate-restricted glob would narrow
// them. Items the split promoted to `pub(crate)` simply stay crate-visible —
// a glob re-export never widens.
mod activation;
mod compute;
mod config;
mod curves;
mod device_cal;
mod modules;
#[cfg(test)]
mod tests;
mod publish;

pub use activation::*;
pub use compute::*;
pub use config::*;
pub use curves::*;
pub use device_cal::*;
pub use modules::*;
// Every publisher is crate-internal — nothing outside the engine publishes
// into the bus — so this one glob is narrowed rather than `pub`.
pub(crate) use publish::*;

/// Namespaces inner node UIDs under their containing subpatch's UID to avoid
/// collisions in the shared `state` map (and the `remap:`/`collector:` keys the
/// UI's AutoMap resolver derives) when multiple subpatches — including nested
/// ones — share inner node indices.
///
/// The previous `(outer << 20) + inner + 1` was NOT injective: a left-shift by
/// 20 discards high bits, so two different `(outer, inner)` pairs (e.g. a
/// top-level subpatch's Remapper and a differently-nested one) could alias to
/// the same UID. That made the two distinct nodes write the SAME collector key,
/// each clobbering the other's per-frame output (observed as a Remapper's
/// suppressed D-pad direction leaking through). This uses a splitmix64-style
/// finalizer over a 128→64 fold of the two operands, which is effectively
/// collision-free for the small integer node ids in play.
///
/// MUST stay identical between the engine eval and the UI's `find_automap_device`
/// walkers — both call this same function so their keys agree.
#[inline]
pub fn namespaced_uid(outer: usize, inner: usize) -> usize {
    // Reserve a marker bit so a namespaced uid never collides with a raw
    // top-level node uid (which are small snarl indices). +1 on inner keeps
    // inner==0 distinguishable.
    let mut z = (outer as u64)
        .wrapping_mul(0x9E37_79B9_7F4A_7C15)
        .wrapping_add((inner as u64).wrapping_add(1));
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^= z >> 31;
    // Force the high bit set so these never alias a plain (small) node uid.
    (z | 0x8000_0000_0000_0000) as usize
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
    last_outputs: &mut HashMap<usize, Vec<Option<Signal>>>,
    fb_routes: &mut HashMap<String, String>,
    outer_uid: usize,
    dt: f32,
) -> Vec<Vec<Option<Signal>>> {
    let n = graph.nodes.len();
    let mut computed: Vec<Vec<Option<Signal>>> = vec![vec![]; n];

    for (idx, snap) in graph.nodes.iter().enumerate() {
        // Compute a namespaced UID for this inner node early so inner-node
        // special cases can publish into `collector_sigs` using the same
        // keying scheme the UI's AutoMap resolver expects.
        let ns_uid = namespaced_uid(outer_uid, snap.node_uid);
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
                scope_samples, last_inputs, last_outputs, fb_routes, nested_uid, dt,
            );
            computed[idx] = sg.outlet_locs.iter()
                .map(|loc| loc.and_then(|(ni, np)| inner_computed.get(ni).and_then(|v| v.get(np)).copied().flatten()))
                .collect();
            continue;
        }

        // AutoMap collector inside a subpatch: inject signals into collector_sigs
        // using a namespaced key so it matches what find_automap_device produced.
        // Mirrors the top-level arm: pass-through upstream first, then apply
        // explicit collected-pin overrides.
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
            let uid_key = format!("collector:{}", ns_uid);

            // Phase 1: pass-through from upstream AutoMap source.
            // See top-level arm for the rationale on iterating actual
            // collector_sigs entries rather than `ALL_PINS`.
            let upstream_dev = snap.params.get("_automap_device_id")
                .and_then(|v| v.as_str()).unwrap_or("").to_string();
            let upstream_collector = snap.params.get("_automap_collector_id")
                .and_then(|v| v.as_str()).unwrap_or("").to_string();
            if !upstream_collector.is_empty() {
                let copies: Vec<(String, Signal)> = collector_sigs.iter()
                    .filter(|((dev, _), _)| dev == &upstream_collector)
                    .map(|((_, pin), sig)| (pin.clone(), *sig))
                    .collect();
                for (pin, sig) in copies {
                    collector_sigs.insert((uid_key.clone(), pin), sig);
                }
                if !upstream_dev.is_empty() {
                    for pin in flexinput_core::automap::ALL_PINS {
                        let key = (uid_key.clone(), pin.id.to_string());
                        if collector_sigs.contains_key(&key) { continue; }
                        if let Some(&sig) = dev_sigs.get(&(upstream_dev.clone(), pin.id.to_string())) {
                            collector_sigs.insert(key, sig);
                        }
                    }
                }
            } else if !upstream_dev.is_empty() {
                for pin in flexinput_core::automap::ALL_PINS {
                    if let Some(&sig) = dev_sigs.get(&(upstream_dev.clone(), pin.id.to_string())) {
                        collector_sigs.insert((uid_key.clone(), pin.id.to_string()), sig);
                    }
                }
            }

            // Phase 2: explicit collected-pin overrides.
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

        // Handle remapper inside a subpatch the same way the top-level loop
        // does, but publish under the namespaced remap key so downstream
        // sinks (or outer graph routing) can pick up the overrides.
        if snap.module_id == "module.remapper" {
            // Shared Remapper evaluation (identical at top level and in sub-patches).
            eval_remapper_node(snap, ns_uid, dev_sigs, collector_sigs, state, dt);

            computed[idx] = vec![None];
            continue;
        }

        // Touch Zones mapping mode nested in a sub-patch — publish under the
        // NAMESPACED uid so the touchmap key matches downstream lookups.
        if snap.module_id == "module.touch_zones"
            && snap.params.get("zone_mode").and_then(|v| v.as_str()) == Some("mapping")
        {
            eval_touch_zones_map_node(snap, ns_uid, dev_sigs, collector_sigs, state, dt);
            computed[idx] = vec![None];
            continue;
        }

        // Virtual Menu nested in a sub-patch — publish under the NAMESPACED
        // uid so the menumap key matches downstream lookups; mirror outputs
        // into last_outputs so the outer body's zone-live highlight works.
        if snap.module_id == "module.menu" {
            let inputs: Vec<Option<Signal>> = snap.input_sources.iter()
                .map(|src| src.and_then(|(si, op)| {
                    computed.get(si).and_then(|v| v.get(op)).copied().flatten()
                }))
                .collect();
            computed[idx] = eval_menu_node(
                snap, ns_uid, &inputs, dev_sigs, collector_sigs, state, dt);
            last_outputs.insert(ns_uid, computed[idx].clone());
            continue;
        }

        // module.map_action inside subpatch: mirror top-level behaviour but
        // write last_outputs keyed by the namespaced UID so UI/outer bodies
        // can observe inner output state.
        if snap.module_id == "module.map_action" {
            // Shared Map Action evaluation (identical at top level and sub-patch).
            computed[idx] = eval_map_action_node(snap, ns_uid, dev_sigs, collector_sigs, state, dt);
            last_outputs.insert(ns_uid, computed[idx].clone());
            continue;
        }

        // AutoMap fork / combiner / selector inside a sub-patch. Same helpers
        // as the top-level loop, but keyed under `ns_uid` so downstream sinks
        // (and the UI's `find_automap_device_rec` walker, which folds the outer
        // chain when it crosses the subpatch boundary) look up the right entry
        // in `collector_sigs`. Without these arms, the sub-patch falls through
        // to `compute_node` which doesn't touch `collector_sigs` at all —
        // upstream Remapper / Collector overrides get dropped on the floor.
        if snap.module_id == "module.automap_fork" {
            automap_fork_publish(snap, ns_uid, &computed, dev_sigs, collector_sigs);
            computed[idx] = vec![None; snap.n_outputs];
            last_outputs.insert(ns_uid, computed[idx].clone());
            continue;
        }
        if snap.module_id == "module.automap_combiner" {
            automap_combiner_publish(snap, ns_uid, dev_sigs, collector_sigs);
            computed[idx] = vec![None];
            last_outputs.insert(ns_uid, computed[idx].clone());
            continue;
        }
        if snap.module_id == "module.automap_selector" {
            automap_selector_publish(snap, ns_uid, &computed, dev_sigs, collector_sigs, fb_routes);
            computed[idx] = vec![None];
            last_outputs.insert(ns_uid, computed[idx].clone());
            continue;
        }
        // Feedback Control inside a sub-patch — the common case (device pins
        // aren't reachable there, which is the whole reason this node exists).
        // The injection key is the PHYSICAL device id (stamped at build time),
        // not the uid, so no namespacing is needed; outlets read dev_sigs by the
        // stamped virtual destination id. Identical to the top-level arm.
        if snap.module_id == "module.feedback_control" {
            let out = feedback_control_publish(snap, &computed, dev_sigs, collector_sigs);
            last_outputs.insert(ns_uid, out.clone());
            computed[idx] = out;
            continue;
        }
        // Audio Stream Haptics inside a sub-patch. Publish under the NAMESPACED uid
        // (ns_uid) so it matches both the capture manager's nested registration and
        // the downstream sink's collector lookup. Without this arm ASTH did nothing
        // when nested — the reported "doesn't work inside a sub-patch".
        if snap.module_id == AUDIO_STREAM_HAPTICS_ID {
            // output[0] = AutoMap passthrough; output[1..] = raw band EFs + freqs.
            let out = audio_stream_haptics_publish(snap, ns_uid, dev_sigs, collector_sigs);
            computed[idx] = out.clone();
            last_outputs.insert(ns_uid, out);
            continue;
        }
        // Network Send / Receive nested in a sub-patch. Publish under the
        // NAMESPACED uid so the socket, collector pass-through, and downstream
        // sink lookup all agree (mirrors ASTH's nested arm above).
        if snap.module_id == NET_SEND_ID {
            let out = net_send_publish(snap, ns_uid, dev_sigs, collector_sigs);
            computed[idx] = out.clone();
            last_outputs.insert(ns_uid, out);
            continue;
        }
        if snap.module_id == NET_RECV_ID {
            let out = net_recv_publish(snap, ns_uid, dev_sigs, collector_sigs);
            computed[idx] = out.clone();
            last_outputs.insert(ns_uid, out);
            continue;
        }

        let inputs: Vec<Option<Signal>> = snap.input_sources.iter()
            .map(|src| src.and_then(|(si, op)| {
                computed.get(si).and_then(|v| v.get(op)).copied().flatten()
            }))
            .collect();

        let node_state = state.entry(ns_uid).or_insert_with(NodeState::default);
        if let Some(ref vals) = snap.aux_f32_override {
            node_state.aux_f32 = vals.clone();
        }
        let node_outputs = compute_node(snap, &inputs, node_state, dev_sigs, collector_sigs, dt);

        // ── 3DOF lean dispatch (subgraph eval) ───────────────────────────
        //
        // Mirror of the top-level dispatch — the 3DOF module commonly sits
        // inside a sub-patch (gyro pre-processing wrapped behind a clean
        // interface). Without this block, lean mappings inside a subpatch
        // wouldn't fire even though their UI works the same way.
        //
        // Uses `ns_uid` (namespaced UID) so the collector key matches what
        // `find_automap_device_rec` in app.rs computes when something
        // downstream traces back through the subpatch boundary.
        if snap.module_id == "processing.gyro_3dof" {
            lean_dispatch_into_collector_sigs(
                snap, ns_uid, &node_outputs, node_state, collector_sigs, dt,
            );
        }

        // Display state for inner nodes — keyed by namespaced UID so the UI walk
        // can find them when populating `node.extra.last_signals` / `history`.
        match snap.module_id.as_str() {
            "display.oscilloscope" | "display.readout" => {
                let sample = inputs.iter().map(|s| sig_to_f32(*s)).collect();
                scope_samples.push((ns_uid, sample));
                last_inputs.insert(ns_uid, inputs.clone());
            }
            "display.trigscope" => {
                // inputs[0] is trigger; inputs[1..] are data channels.
                // Emit all inputs so the UI can do trigger-edge detection.
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
            // The 3D controller viewer reads its Orientation quaternion (input
            // pin 1, a Vec4) from the mirrored inputs.
            "display.controller3d" => {
                last_inputs.insert(ns_uid, inputs.clone());
            }
            "module.response_curve" | "module.vec_response_curve" | "module.vec_reshape" => {
                last_inputs.insert(ns_uid, inputs.clone());
            }
            "module.twoway_response_curve" => {
                last_inputs.insert(ns_uid, inputs.clone());
            }
            "processing.gyro_3dof" => {
                last_inputs.insert(ns_uid, node_outputs.clone());
            }
            "generator.envelope" => {
                last_inputs.insert(ns_uid, node_state.last_signals.clone());
            }
            _ => {}
        }
        // Populate last_outputs for every node — used by the UI to drive the
        // per-pin signal glow on downstream device-sink inputs.
        last_outputs.insert(ns_uid, node_outputs.clone());

        computed[idx] = node_outputs;
    }

    computed
}

// ── Main graph tick ───────────────────────────────────────────────────────────

/// Evaluate one tick into `out`. The caller owns `out` and is expected to
/// reuse the same `TickOutput` across ticks — we `.clear()` at the top so
/// the HashMaps keep their allocated capacity between calls instead of
/// being dropped and reallocated. At the default 2 kHz rate this was a
/// non-trivial source of allocator pressure even on empty graphs.
pub fn eval_graph_tick(
    graph: &ProcessingGraph,
    state: &mut HashMap<usize, NodeState>,
    dev_sigs: &HashMap<(String, String), Signal>,
    dt: f32,
    out: &mut TickOutput,
) {
    puffin::profile_function!();
    out.clear();
    let n = graph.nodes.len();
    let mut computed: Vec<Vec<Option<Signal>>> = vec![vec![]; n];

    // Apply per-device source-side post-processing (stick deadzone + gyro
    // multiplier) ONCE up front so every downstream consumer — direct wires,
    // AutoMap split/collector, sink AutoMap, remapper — sees the processed
    // values. Avoids the prior leak where AutoMap pulled raw dev_sigs and
    // bypassed the source node's params.
    let mut dev_sigs_owned: HashMap<(String, String), Signal> = {
        puffin::profile_scope!("preprocess_dev_sigs");
        preprocess_dev_sigs(graph, dev_sigs)
    };
    // Apply the Virtual Menu SOURCE-BLOCK (one tick stale): zero every pointer
    // pin an open menu asked to block last tick, so those analog inputs reach
    // ONLY the menu's navigation — not a mouse mapping, another module, or a
    // sink. Snapshot their pre-block values first so the menu itself still reads
    // them (it's the reason they're blocked for everyone else). See
    // `NodeState::source_block` / `unblocked_src`.
    {
        let req: Vec<(String, String)> = state.get(&MACRO_CARRY_UID)
            .map(|s| s.source_block.iter().cloned().collect())
            .unwrap_or_default();
        let mut snap: HashMap<(String, String), Signal> = HashMap::new();
        for key in &req {
            if let Some(&v) = dev_sigs_owned.get(key) {
                snap.insert(key.clone(), v);
            }
            dev_sigs_owned.insert(key.clone(), pointer_block_off(&key.1));
        }
        let e = state.entry(MACRO_CARRY_UID).or_default();
        e.unblocked_src = snap;
    }
    let dev_sigs = &dev_sigs_owned;

    // Destructure with `ref mut` so the rest of the function can keep
    // using bare names (outputs, scope_samples, …) as mutable references.
    // Borrows live until the end of the function; final
    // `TickOutput { … }` packing is no longer needed.
    let TickOutput {
        ref mut outputs,
        ref mut scope_samples,
        ref mut last_inputs,
        ref mut last_outputs,
        ref mut sink_outputs,
    } = *out;
    // Signals injected by AutoMap Collector nodes, keyed by ("collector:{uid}", pin_id).
    let mut collector_sigs: HashMap<(String, String), Signal> = HashMap::new();
    // Reverse feedback routes: a synthetic AutoMap node's OUTPUT id (e.g.
    // "forksel:5:0" from a Selector) → the SOURCE id it currently gates from
    // (e.g. "collector:3" for a network recv, or "gilrs:…" for a pad). Populated
    // by the Selector/Fork eval below; consumed by the reverse-feedback post-pass
    // so an ASTH / Feedback Control node placed AFTER a Selector still reaches the
    // pad or network back-channel (feedback flows backward along the gate).
    let mut fb_routes: HashMap<String, String> = HashMap::new();

    {
    puffin::profile_scope!("main_node_loop");
    for (idx, snap) in graph.nodes.iter().enumerate() {
        // ── module.map_action: AutoMap in → Bool out based on stored mappings ──
        if snap.module_id == "module.map_action" {
            // Shared Map Action evaluation (identical at top level and sub-patch).
            computed[idx] = eval_map_action_node(snap, snap.node_uid, dev_sigs, &collector_sigs, state, dt);
            last_outputs.insert(snap.node_uid, computed[idx].clone());
            continue;
        }

        // ── module.remapper: pass-through + per-mapping override + consume ────
        if snap.module_id == "module.remapper" {
            // Shared Remapper evaluation (identical at top level and in sub-patches).
            eval_remapper_node(snap, snap.node_uid, dev_sigs, &mut collector_sigs, state, dt);

            computed[idx] = vec![None];
            continue;
        }

        // ── module.touch_zones (mapping mode): inject per-zone behaviours ─────
        // Ports mode falls through to compute_node (typed zone outputs); mapping
        // mode publishes bus overrides under `touchmap:{uid}` like the Remapper.
        if snap.module_id == "module.touch_zones"
            && snap.params.get("zone_mode").and_then(|v| v.as_str()) == Some("mapping")
        {
            eval_touch_zones_map_node(snap, snap.node_uid, dev_sigs, &mut collector_sigs, state, dt);
            computed[idx] = vec![None];
            continue;
        }

        // ── module.menu: open/hover/select state machine + `menumap:{uid}`
        // injector (cards, suppression). Runs in BOTH zone modes — ports mode
        // still needs the state machine for its typed outputs and suppression.
        if snap.module_id == "module.menu" {
            let inputs: Vec<Option<Signal>> = snap.input_sources.iter()
                .map(|src| src.and_then(|(si, op)| {
                    computed.get(si).and_then(|v| v.get(op)).copied().flatten()
                }))
                .collect();
            computed[idx] = eval_menu_node(
                snap, snap.node_uid, &inputs, dev_sigs, &mut collector_sigs, state, dt);
            last_outputs.insert(snap.node_uid, computed[idx].clone());
            continue;
        }

        // ── module.automap_collect: inject individual inputs into collector_sigs ──
        //
        // Two-phase write into collector_sigs[("collector:{uid}", pin)]:
        //   1. Pass-through pins from the upstream AutoMap bus — pulled
        //      either from upstream `collector_sigs` (if upstream is a
        //      Remapper/Collector/Fork/Selector/Combiner/Lean) or from
        //      raw `dev_sigs` (if upstream is a physical device). This
        //      ensures mapped output pins from an upstream Remapper still
        //      reach downstream sinks even though the user didn't add
        //      those pins via the Collector's "+" dropdown.
        //   2. Explicit collected-pin overrides — values wired by the user
        //      to the collector's individual input ports. These win over
        //      pass-through for the same pin id.
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

            // Phase 1: pass-through from upstream AutoMap source.
            // Iterating upstream's actual collector_sigs entries (not just
            // `ALL_PINS`) is required so off-spec pin names — Remapper's
            // mapped keyboard keys like `key_f`, custom mouse buttons, etc.
            // — also flow through. `ALL_PINS` only covers canonical pin ids.
            let upstream_dev = snap.params.get("_automap_device_id")
                .and_then(|v| v.as_str()).unwrap_or("").to_string();
            let upstream_collector = snap.params.get("_automap_collector_id")
                .and_then(|v| v.as_str()).unwrap_or("").to_string();
            if !upstream_collector.is_empty() {
                // Copy EVERY entry from the upstream collector key.
                let copies: Vec<(String, Signal)> = collector_sigs.iter()
                    .filter(|((dev, _), _)| dev == &upstream_collector)
                    .map(|((_, pin), sig)| (pin.clone(), *sig))
                    .collect();
                for (pin, sig) in copies {
                    collector_sigs.insert((uid_key.clone(), pin), sig);
                }
                // For canonical pins not present on the upstream collector
                // (e.g. when upstream is a Remapper that only writes mapped
                // pins, not pass-through), fall back to raw device samples.
                if !upstream_dev.is_empty() {
                    for pin in flexinput_core::automap::ALL_PINS {
                        let key = (uid_key.clone(), pin.id.to_string());
                        if collector_sigs.contains_key(&key) { continue; }
                        if let Some(&sig) = dev_sigs.get(&(upstream_dev.clone(), pin.id.to_string())) {
                            collector_sigs.insert(key, sig);
                        }
                    }
                }
            } else if !upstream_dev.is_empty() {
                // No upstream collector — pure raw device pass-through.
                for pin in flexinput_core::automap::ALL_PINS {
                    if let Some(&sig) = dev_sigs.get(&(upstream_dev.clone(), pin.id.to_string())) {
                        collector_sigs.insert((uid_key.clone(), pin.id.to_string()), sig);
                    }
                }
            }

            // Phase 2: explicit collected-pin overrides (win over pass-through).
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
            automap_fork_publish(snap, snap.node_uid, &computed, dev_sigs, &mut collector_sigs);
            computed[idx] = vec![None; snap.n_outputs];
            continue;
        }

        // ── module.automap_combiner: merge N AutoMap inputs per per-pin policy ──
        // Default policy SORT: walk inputs top-down (lowest port = highest priority);
        // first asserted value wins. Per-pin overrides in `combiner_pin_policy`:
        //   - OR  : Bool = logical OR;  Float = max(|x|) preserving sign of max
        //   - AND : Bool = logical AND; Float = min(|x|) preserving sign of min
        //   - XOR : Bool = parity;      Float = |a - b| (folded across all inputs)
        //   - ADD : sum, clamped per pin (triggers [0,1], sticks/axes [-1,1])
        //   - MULT: product, clamped per pin
        // Writes into collector_sigs under "combiner:{uid}".
        if snap.module_id == "module.automap_combiner" {
            automap_combiner_publish(snap, snap.node_uid, dev_sigs, &mut collector_sigs);
            computed[idx] = vec![None];
            continue;
        }
        // ── module.automap_selector: gate selected AutoMap input to output ────
        if snap.module_id == "module.automap_selector" {
            automap_selector_publish(snap, snap.node_uid, &computed, dev_sigs, &mut collector_sigs, &mut fb_routes);
            computed[idx] = vec![None];
            continue;
        }
        // ── module.feedback_control: inject inlets into the physical pad's
        //    feedback channel; tap outlets from the virtual destination. ──────
        if snap.module_id == "module.feedback_control" {
            let out = feedback_control_publish(snap, &computed, dev_sigs, &mut collector_sigs);
            last_outputs.insert(snap.node_uid, out.clone());
            computed[idx] = out;
            continue;
        }
        // ── module.audio_stream_haptics: pass the AutoMap bus through, then
        //    inject audio-derived HD rumble into the target pad's feedback. ────
        if snap.module_id == AUDIO_STREAM_HAPTICS_ID {
            // output[0] = AutoMap passthrough (no scalar); output[1..] = raw band
            // EFs + band carrier freqs (Hz), see audio_stream_haptics_publish.
            let out = audio_stream_haptics_publish(snap, snap.node_uid, dev_sigs, &mut collector_sigs);
            last_outputs.insert(snap.node_uid, out.clone());
            computed[idx] = out;
            continue;
        }
        // ── module.network_send: pass the bus through locally + transmit it;
        //    inject peer feedback into the upstream pad. ────────────────────────
        if snap.module_id == NET_SEND_ID {
            let out = net_send_publish(snap, snap.node_uid, dev_sigs, &mut collector_sigs);
            last_outputs.insert(snap.node_uid, out.clone());
            computed[idx] = out;
            continue;
        }
        // ── module.network_recv: publish the peer's bus into collector:{uid};
        //    gather downstream feedback to ship back. ──────────────────────────
        if snap.module_id == NET_RECV_ID {
            let out = net_recv_publish(snap, snap.node_uid, dev_sigs, &mut collector_sigs);
            last_outputs.insert(snap.node_uid, out.clone());
            computed[idx] = out;
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

            // Per-sink scaling params. Currently: mouse sensitivity on
            // virtual.keymouse — applied to mouse_x / mouse_y / mouse pins.
            let mouse_sens = snap.params.get("mouse_sensitivity")
                .and_then(|v| v.as_f64()).unwrap_or(1.0) as f32;
            let scale_for_sink = |pin_id: &str, sig: Signal| -> Signal {
                if st.device_id.starts_with("virtual.keymouse") && is_mouse_pin(pin_id)
                    && (mouse_sens - 1.0).abs() > f32::EPSILON
                {
                    match sig {
                        Signal::Float(v) => Signal::Float(v * mouse_sens),
                        Signal::Vec2(v)  => Signal::Vec2(v * mouse_sens),
                        other => other,
                    }
                } else { sig }
            };

            // Direct-wire inputs (possibly multi-source per pin, combined additively).
            //
            // Self-sink nodes (device.source whose feedback inputs loop back to
            // their own outputs, directly or via a Splitter/Math chain) are
            // deferred to a post-pass below: their upstream chain only fills
            // `computed[]` after this iteration runs, so we wait until the main
            // loop completes before reading.
            if !st.is_self_sink {
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
                        sink_outputs.insert((st.device_id.clone(), pin_id.clone()), scale_for_sink(pin_id, sig));
                    }
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
                let is_collector = is_namespaced_source(src_dev);
                // Digital→analog trigger bridges (`btn_lt_dig`→`left_trigger`,
                // `btn_rt_dig`→`right_trigger`) are a LOWEST-PRIORITY fallback:
                // they only fill the analog trigger when no primary source — the
                // real analog `left_trigger`/`right_trigger`, a manually-injected
                // AutoMap value, or a Remapper analog mapping — already drove it.
                // Deferred to a second pass so primaries (processed first) win.
                let mut deferred_digital_triggers: Vec<(&str, &str)> = Vec::new();
                let resolve_sig = |mapped_src: &str| -> Option<Signal> {
                    if is_collector {
                        collector_sigs.get(&(src_dev.clone(), mapped_src.to_string())).copied()
                            .or_else(|| {
                                st.automap_fallback_dev.as_ref().and_then(|fb| {
                                    dev_sigs.get(&(fb.clone(), mapped_src.to_string())).copied()
                                })
                            })
                    } else {
                        dev_sigs.get(&(src_dev.clone(), mapped_src.to_string())).copied()
                    }
                };
                for (mapped_src, mapped_dst) in automap::resolve_mapping(&src_ids, &dst_ids) {
                    if directly_wired.contains(mapped_dst) { continue; }
                    let is_digital_trigger_bridge =
                        matches!((mapped_src, mapped_dst),
                            ("btn_lt_dig", "left_trigger") | ("btn_rt_dig", "right_trigger"));
                    if is_digital_trigger_bridge {
                        // Only honour the bridge when the upstream source opted in
                        // (or is a digital-only pad). Otherwise a pad with real
                        // analog triggers would have its digital button leak into
                        // the analog trigger.
                        if st.digital_trigger_bridge {
                            deferred_digital_triggers.push((mapped_src, mapped_dst));
                        }
                        continue;
                    }
                    if let Some(sig) = resolve_sig(mapped_src) {
                        // Type coercion (Bool↔Float) is performed by the virtual device's
                        // send() via Signal::as_float / as_bool, so we just hand the raw
                        // signal off — semantic groups already routed it to the right pin.
                        sink_outputs
                            .entry((st.device_id.clone(), mapped_dst.to_string()))
                            .or_insert(scale_for_sink(mapped_dst, sig));
                    }
                }
                // Second pass: digital-trigger fallback. Writes the analog trigger
                // ONLY when a primary source didn't (real analog, manual injection,
                // or Remapper analog). The digital button drives the FULL value —
                // pressed → 1.0, released → 0.0. We must write the 0.0 on release
                // too, otherwise the trigger latches at its last pressed value and
                // never lets go. On a mixed pad the real analog trigger always
                // writes a primary (even 0.0), so `contains_key` skips this and the
                // real analog wins as intended.
                for (mapped_src, mapped_dst) in deferred_digital_triggers {
                    let key = (st.device_id.clone(), mapped_dst.to_string());
                    if sink_outputs.contains_key(&key) { continue; }
                    if let Some(sig) = resolve_sig(mapped_src) {
                        let v = if sig.as_bool() { 1.0 } else { 0.0 };
                        sink_outputs.insert(key, Signal::Float(v));
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
                            .or_insert(scale_for_sink(pin, sig));
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

            // AutoMap feedback channel: signals flow BACKWARD along AutoMap wires
            // from virtual sinks to physical haptic inputs. Each virtual sink that
            // auto-maps FROM this device contributes its rumble/lightbar outputs
            // to matching haptic input pins on this device, silently and without
            // explicit user wiring. Direct wires (in `directly_wired`) take priority.
            if !st.feedback_sources.is_empty() {
                let dst_pins: Vec<&str> = st.pin_ids.iter()
                    .filter(|p| !p.is_empty())
                    .map(|p| p.as_str())
                    .collect();
                for fb in &st.feedback_sources {
                    for (virt_out_pin, _) in flexinput_core::automap::FEEDBACK_PAIRS.iter() {
                        let Some(&sig) = dev_sigs.get(&(fb.device_id.clone(), virt_out_pin.to_string())) else {
                            continue;
                        };
                        let Some(dst_pin) = flexinput_core::automap::resolve_feedback_pin(
                            virt_out_pin, &dst_pins
                        ) else { continue; };
                        if directly_wired.contains(dst_pin) { continue; }
                        // Perceptual shaping for HD voice-coil amplitude pins only.
                        // A game's classic rumble is often weak (0.1–0.3); mapped
                        // straight onto a Switch Pro / DualSense HD coil — which is
                        // then run through a power-law amp curve in the encoder —
                        // it's below the perceptible threshold and can't be felt.
                        // Shape ONLY the feedback path to the HD amp pins (direct
                        // knob wiring and ERM `rumble_strong`/lightbar are
                        // untouched), using the source virtual device's per-device
                        // floor/max/exp.
                        let routed = if matches!(dst_pin, "hd_l_amp" | "hd_r_amp") {
                            shape_hd_feedback(sig, fb.rumble_floor, fb.rumble_max, fb.rumble_exp)
                        } else {
                            sig
                        };
                        // COMBINE, don't first-wins. Multiple virtual sinks can
                        // auto-map FROM the same physical device (e.g. a virtual
                        // DS4 AND a virtual DualSense both fed by one Switch Pro);
                        // each contributes feedback to the same physical haptic
                        // pin. A plain `or_insert` kept only whichever source the
                        // `feedback_sources` iteration hit first — so only ONE
                        // virtual's rumble/ping reached the physical, and which one
                        // flipped across restarts as graph/enumeration order
                        // changed (the "only one passes ping" flakiness). Take the
                        // max so any active source drives the pad (haptics are
                        // level-triggered; loudest wins, matching rumble peak).
                        sink_outputs
                            .entry((st.device_id.clone(), dst_pin.to_string()))
                            .and_modify(|cur| *cur = combine_feedback_max(*cur, routed))
                            .or_insert(routed);
                    }
                }
            }

            // (Feedback Control injection is drained in a post-pass after the
            //  main loop — see below — so every injector node has run first.)

            // device.source nodes with haptic inputs, and device.sink nodes with
            // feedback output pins, still need output computation — don't skip them.
            if snap.module_id != "device.source" && snap.n_outputs == 0 {
                computed[idx] = vec![];
                continue;
            }
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
                scope_samples, last_inputs, last_outputs, &mut fb_routes, snap.node_uid, dt,
            );
            let out: Vec<Option<Signal>> = sg.outlet_locs.iter()
                .map(|loc| loc.and_then(|(ni, np)| inner_computed.get(ni).and_then(|v| v.get(np)).copied().flatten()))
                .collect();
            last_outputs.insert(snap.node_uid, out.clone());
            computed[idx] = out;
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

        // ── 3DOF lean dispatch: emit per-pin signals via the Map output ──
        //
        // The lean_left / lean_right sections each own an array of mappings
        // (Map-Action-shaped: `{ out, mode, window_ms, sustain, turbo }`).
        // A mapping's raw-held bool is whether the lean magnitude has
        // crossed `lean_threshold` on the corresponding side. The held
        // value flows through the standard press-mode pipeline (down /
        // short / long / double / on_press / on_release / turbo) the same
        // way Map Action does. Asserted output pins are written into
        // `collector_sigs` under "lean:{uid}" so a downstream AutoMap
        // collector (or subpatch outlet) routes them to gamepad/KB sinks.
        //
        // Analog mode is special: instead of treating the side as a
        // raw_held bool, it drives the destination through the shared
        // `analog_digital_pulse` modulator — Hold → PWM (duty = |lean|),
        // Turbo → ×2 max frequency, plain → tap train whose frequency
        // tracks |lean|. Released mappings and below-threshold leans
        // produce no pulses. Press-state slot [0] tracks the phase seconds.
        if snap.module_id == "processing.gyro_3dof" {
            lean_dispatch_into_collector_sigs(
                snap, snap.node_uid, &node_outputs, node_state,
                &mut collector_sigs, dt,
            );
        }

        match snap.module_id.as_str() {
            "display.oscilloscope" | "display.readout" => {
                let sample = inputs.iter().map(|s| sig_to_f32(*s)).collect();
                scope_samples.push((snap.node_uid, sample));
                last_inputs.insert(snap.node_uid, inputs.clone());
            }
            "display.trigscope" => {
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
            // The 3D controller viewer reads its Orientation quaternion (input
            // pin 1, a Vec4) from the mirrored inputs.
            "display.controller3d" => {
                last_inputs.insert(snap.node_uid, inputs.clone());
            }
            "module.response_curve" | "module.vec_response_curve" | "module.vec_reshape" => {
                last_inputs.insert(snap.node_uid, inputs.clone());
            }
            "module.twoway_response_curve" => {
                last_inputs.insert(snap.node_uid, inputs.clone());
            }
            "generator.envelope" => {
                // last_signals = [output, phase]; UI reads phase from index 1 for playhead
                last_inputs.insert(snap.node_uid, node_state.last_signals.clone());
            }
            // Export outputs (not inputs) so the UI body can show a live readout.
            "processing.gyro_3dof" => {
                last_inputs.insert(snap.node_uid, node_outputs.clone());
            }
            _ => {}
        }
        // Populate last_outputs for every node — used by the UI to drive the
        // per-pin signal glow on downstream device-sink inputs.
        last_outputs.insert(snap.node_uid, node_outputs.clone());

        // Exclude device.source from the exported outputs; UI evaluates those fresh.
        if snap.module_id != "device.source" {
            for (out_pin, sig) in node_outputs.iter().enumerate() {
                outputs.insert((snap.node_uid, out_pin), *sig);
            }
        }

        computed[idx] = node_outputs;
    }
    } // end main_node_loop

    // Post-pass: device.source self-sinks (feedback inputs wired back to their
    // own outputs, possibly through Splitter/Math chains). Their multi_sources
    // can only be read after the main loop has filled `computed[]` for the
    // whole graph — by then any chain that loops through this node has its
    // value, and we can route it into sink_outputs like any other sink.
    puffin::profile_scope!("self_sink_post_pass");
    for (idx, snap) in graph.nodes.iter().enumerate() {
        let Some(ref st) = snap.sink_target else { continue; };
        if !st.is_self_sink { continue; }
        for (in_idx, pin_id) in st.pin_ids.iter().enumerate() {
            if pin_id.is_empty() { continue; }
            let mut combined: Option<Signal> = None;
            if let Some(sources) = st.multi_sources.get(in_idx) {
                for &(src_idx, out_pin) in sources {
                    let sig_opt: Option<Signal> = computed.get(src_idx)
                        .and_then(|v| v.get(out_pin))
                        .copied()
                        .flatten()
                        .or_else(|| {
                            // Direct self-wire: source's own `computed[idx]` is
                            // the result of this same tick's compute_node, which
                            // for device.source mirrors dev_sigs anyway.
                            if src_idx != idx { return None; }
                            let src_pin = snap.output_pin_ids.get(out_pin)?;
                            if src_pin.is_empty() { return None; }
                            dev_sigs.get(&(st.device_id.clone(), src_pin.clone())).copied()
                        });
                    if let Some(sig) = sig_opt {
                        combined = Some(match combined {
                            None => sig,
                            Some(prev) => combine_signals(prev, sig),
                        });
                    }
                }
            }
            if let Some(sig) = combined {
                sink_outputs.insert((st.device_id.clone(), pin_id.clone()), sig);
            }
        }
    }

    // Post-pass: reverse-feedback routing through AutoMap Selectors. An ASTH /
    // Feedback Control node placed AFTER a Selector injects into the selector's
    // OUTPUT id (`feedback_inject:forksel:{uid}:{out}`). Copy those injections to
    // the source the selector is currently gating from — following the route
    // chain — so they land under the physical pad id (drained by the injection
    // post-pass below) or the network recv's `collector:{uid}` (drained by the
    // recv feedback post-pass). Runs BEFORE the injection drain so a pad terminal
    // is delivered this tick. Only fires when a Selector recorded a route.
    if !fb_routes.is_empty() {
        for from_id in fb_routes.keys().cloned().collect::<Vec<_>>() {
            // Resolve the terminal source through the (short) route chain.
            let mut terminal = from_id.clone();
            for _ in 0..8 {
                match fb_routes.get(&terminal) {
                    Some(next) => terminal = next.clone(),
                    None => break,
                }
            }
            if terminal == from_id { continue; }
            let from_key = format!("feedback_inject:{from_id}");
            let to_key = format!("feedback_inject:{terminal}");
            let entries: Vec<(String, Signal)> = collector_sigs.iter()
                .filter(|((d, _), _)| d == &from_key)
                .map(|((_, p), s)| (p.clone(), *s))
                .collect();
            for (pin, sig) in entries {
                use std::collections::hash_map::Entry;
                match collector_sigs.entry((to_key.clone(), pin)) {
                    Entry::Occupied(mut o) => { *o.get_mut() = combine_signals(*o.get(), sig); }
                    Entry::Vacant(v) => { v.insert(sig); }
                }
            }
        }
    }

    // Post-pass: Feedback Control injection drain. Runs AFTER the main loop so
    // every `module.feedback_control` node — at the top level or nested in any
    // sub-patch — has already written its inlet values into `collector_sigs`
    // under `feedback_inject:{physical_dev_id}`. For each physical sink, route
    // those values to the device's haptic inputs (direct pin-id match first,
    // then `resolve_feedback_pin` rumble/lightbar aliasing). Direct wires and
    // the auto-feedback in the main loop both win via `or_insert`.
    //
    // Cheap early-out: skip entirely unless at least one injector wrote this
    // tick (the common case is no Feedback Control nodes at all).
    let has_injection = collector_sigs.keys()
        .any(|(dev, _)| dev.starts_with("feedback_inject:"));
    if has_injection {
        puffin::profile_scope!("feedback_inject_post_pass");
        for snap in graph.nodes.iter() {
            let Some(ref st) = snap.sink_target else { continue; };
            if st.device_id.starts_with("virtual.") { continue; }
            let inject_key = format!("feedback_inject:{}", st.device_id);
            let dst_pins: Vec<&str> = st.pin_ids.iter()
                .filter(|p| !p.is_empty())
                .map(|p| p.as_str())
                .collect();
            // Pins with at least one real direct wire keep priority.
            let directly_wired: std::collections::HashSet<&str> = st.pin_ids.iter().enumerate()
                .filter(|(i, pid)| !pid.is_empty() && st.multi_sources.get(*i).map_or(false, |s| !s.is_empty()))
                .map(|(_, pid)| pid.as_str())
                .collect();
            for pin in flexinput_core::automap::FEEDBACK_INLET_PINS {
                let Some(&sig) = collector_sigs.get(&(inject_key.clone(), pin.id.to_string()))
                else { continue; };
                let dst_pin = if dst_pins.iter().any(|&p| p == pin.id) {
                    Some(pin.id)
                } else {
                    flexinput_core::automap::resolve_feedback_pin(pin.id, &dst_pins)
                };
                let Some(dst_pin) = dst_pin else { continue; };
                if directly_wired.contains(dst_pin) { continue; }
                // Perceptual HD shaping for a CLASSIC rumble that remapped onto an
                // HD voice-coil amp pin (e.g. a networked Switch Pro: rumble_strong
                // → hd_l_amp, since the pad exposes no rumble_strong inlet). Mirror
                // the main-loop auto-feedback pass (`shape_hd_feedback`) so a weak
                // game rumble (0.1–0.3) run through the encoder's power-law curve is
                // still perceptible. Only when the pin actually REMAPPED (pin.id !=
                // dst_pin): a direct hd_l_amp injection (ASTH / Feedback Control)
                // already carries an intended amplitude and must NOT be reshaped.
                // Uses the standard default floor/max/exp — the networked source's
                // per-device shaping isn't available on this end.
                let sig = if pin.id != dst_pin && matches!(dst_pin, "hd_l_amp" | "hd_r_amp") {
                    shape_hd_feedback(sig, 0.35, 1.0, 0.6)
                } else {
                    sig
                };
                // Precedence: direct wire > injection > auto-feedback. The
                // main-loop auto-feedback pass may have already `or_insert`-ed a
                // value for this pin — typically `0.0` (the virtual sink's idle
                // rumble when no game is driving it). A plain `or_insert` here
                // would let that idle `0.0` mask the user's explicit injection,
                // producing only a brief buzz on the rising edge. Instead COMBINE
                // additively (clamped) so injection adds on top of any real game
                // rumble and overrides idle silence.
                use std::collections::hash_map::Entry;
                match sink_outputs.entry((st.device_id.clone(), dst_pin.to_string())) {
                    Entry::Occupied(mut o) => {
                        let merged = combine_signals(*o.get(), sig);
                        *o.get_mut() = clamp_feedback_signal(dst_pin, merged);
                    }
                    Entry::Vacant(v) => { v.insert(sig); }
                }
            }
        }
    }

    // Post-pass: network Receive feedback aggregation. Runs AFTER the
    // feedback_inject post-pass so ASTH / Feedback Control nodes on the RECEIVER
    // (which target a recv node's synthetic `collector:{uid}` id) have already
    // written `feedback_inject:collector:{uid}`. Recurses into sub-patches, and
    // uses a whole-graph source→sinks index so a recv node reaches its downstream
    // virtual sinks even when they sit on a different sub-patch level.
    //
    // Cheap early-out: only build the index + walk if a network_recv node exists.
    if graph_has_net_recv(&graph.nodes) {
        let mut sink_sources: HashMap<String, Vec<String>> = HashMap::new();
        collect_sink_sources(&graph.nodes, &mut sink_sources);
        publish_recv_feedback_frames(&graph.nodes, 0, false, dev_sigs, &collector_sigs, &sink_sources);
    }

    // Snapshot this tick's macro-namespace values onto the reserved carry-over
    // entry so next tick a macro READER that runs before its producer (a menu
    // upstream of the Remapper targeting its Select/Show — a feedback cycle)
    // still observes the value, one tick stale. Rebuilt from empty each tick so a
    // released macro clears after one tick. See `NodeState::macro_prev`.
    {
        use flexinput_core::macros::{SIGS_NS, SIGS_NS_VEC2};
        let carry = state.entry(MACRO_CARRY_UID).or_default();
        carry.macro_prev.clear();
        carry.source_block.clear();
        for ((k, pin), sig) in collector_sigs.iter() {
            if k == SIGS_NS || k == SIGS_NS_VEC2 {
                carry.macro_prev.insert((k.clone(), pin.clone()), *sig);
            } else if let Some(dev) = k.strip_prefix(SRC_BLOCK_PREFIX) {
                carry.source_block.insert((dev.to_string(), pin.clone()));
            }
        }
    }
}

/// True if any network_recv node exists anywhere in the graph (recurses into
/// sub-patches). Gates the recv feedback post-pass so patches without networking
/// pay nothing.
fn graph_has_net_recv(nodes: &[NodeSnap]) -> bool {
    nodes.iter().any(|n| {
        n.module_id == NET_RECV_ID
            || n.inline_subgraph.as_ref().is_some_and(|sg| graph_has_net_recv(&sg.graph.nodes))
    })
}

/// Clamp a combined feedback value to the valid range for its haptic pin so
/// additive merging (game rumble + injected effect) can't overflow. Amplitudes
/// and most haptic pins are 0–1; everything falls back to 0–1 which is correct
/// for the rumble/lightbar/amp pins the Feedback Control node injects.
fn clamp_feedback_signal(_pin: &str, sig: Signal) -> Signal {
    match sig {
        Signal::Float(f) => Signal::Float(f.clamp(0.0, 1.0)),
        other => other,
    }
}

/// Typed OFF value for a canonical pin the sink forces to zero because an open
/// Virtual Menu is blocking it at the game boundary.
fn pointer_block_off(pin_id: &str) -> Signal {
    match automap::ALL_PINS.iter().find(|ap| ap.id == pin_id).map(|ap| ap.signal_type) {
        Some(SignalType::Vec2) => Signal::Vec2(Vec2::ZERO),
        Some(SignalType::Bool) => Signal::Bool(false),
        Some(SignalType::Int)  => Signal::Int(0),
        _ => Signal::Float(0.0),
    }
}
