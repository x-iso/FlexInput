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
                let is_collector = src_dev.starts_with("collector:")
                    || src_dev.starts_with("forksel:")
                    || src_dev.starts_with("remap:")
                    || src_dev.starts_with("combiner:")
                    || src_dev.starts_with("lean:")
                    || src_dev.starts_with("touchmap:")
                    || src_dev.starts_with("menumap:");
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

// ── Per-node dispatch ─────────────────────────────────────────────────────────

#[cfg(test)]
mod trigger_tests {
    use super::*;
    use crate::graph::SinkTarget;

    // Two virtual sinks feeding back to one physical pad must COMBINE (max), not
    // first-wins — the "only one virtual passes ping after restart" bug.
    #[test]
    fn combine_feedback_takes_max_not_first() {
        // Loud + quiet → loud, regardless of order.
        assert_eq!(
            combine_feedback_max(Signal::Float(0.2), Signal::Float(0.9)),
            Signal::Float(0.9)
        );
        assert_eq!(
            combine_feedback_max(Signal::Float(0.9), Signal::Float(0.2)),
            Signal::Float(0.9)
        );
        // One source idle (0.0), the other active → active wins (the exact bug:
        // an idle virtual must not mask an active one).
        assert_eq!(
            combine_feedback_max(Signal::Float(0.0), Signal::Float(0.7)),
            Signal::Float(0.7)
        );
        // Bool OR.
        assert_eq!(
            combine_feedback_max(Signal::Bool(false), Signal::Bool(true)),
            Signal::Bool(true)
        );
        // Float vs Bool coercion.
        assert_eq!(
            combine_feedback_max(Signal::Float(0.3), Signal::Bool(true)),
            Signal::Float(1.0)
        );
    }

    fn canonical_pins() -> Vec<String> {
        automap::ALL_PINS.iter().map(|p| p.id.to_string()).collect()
    }

    fn empty_node(uid: usize, module_id: &str) -> NodeSnap {
        NodeSnap {
            node_uid: uid,
            module_id: module_id.to_string(),
            params: HashMap::new(),
            n_outputs: 0,
            input_sources: Vec::new(),
            device_id: None,
            output_pin_ids: Vec::new(),
            aux_f32_override: None,
            sink_target: None,
            inline_subgraph: None,
        }
    }

    fn sink_node(uid: usize, device_id: &str, src_dev: &str, bridge: bool) -> NodeSnap {
        let mut n = empty_node(uid, "device.sink");
        n.sink_target = Some(SinkTarget {
            device_id: device_id.to_string(),
            // All canonical pins are valid sink destinations.
            pin_ids: canonical_pins(),
            multi_sources: vec![Vec::new(); canonical_pins().len()],
            automap_source: Some((src_dev.to_string(), canonical_pins())),
            automap_fallback_dev: Some("gilrs:switch_pro:0".to_string()),
            feedback_sources: Vec::new(),
            is_self_sink: false,
            digital_trigger_bridge: bridge,
        });
        n
    }

    // ── Macro Output routing ──────────────────────────────────────────────────

    /// Macro node snap with `ports` as (id, type_str) pairs.
    fn macro_node(uid: usize, ports: &[(&str, &str)]) -> NodeSnap {
        let mut n = empty_node(uid, "module.macro");
        n.n_outputs = ports.len();
        n.output_pin_ids = ports.iter().map(|(id, _)| format!("macro:{id}")).collect();
        n.params.insert("macro_ports".into(), Value::Array(ports.iter().map(|(id, ty)|
            serde_json::json!({ "id": id, "name": id, "icon": "", "type": ty })
        ).collect()));
        n
    }

    // A digital Remapper mapping targeting a macro pin drives the macro node's
    // Bool port (same tick — the macro node evaluates after the remapper), the
    // unmapped port emits its typed off value, and the macro pin never leaks
    // onto the AutoMap bus toward the sink.
    #[test]
    fn remapper_digital_mapping_drives_macro_port() {
        let dev = "gilrs:switch_pro:0";
        let src = source_node(1, dev, 0.0);
        let mut remap = empty_node(2, "module.remapper");
        remap.params.insert("_automap_device_id".into(), Value::String(dev.into()));
        remap.params.insert("mappings".into(), serde_json::json!([
            { "in": ["btn_south"], "out": ["macro:aa11bb22"] }
        ]));
        remap.input_sources = vec![Some((0, 0))];
        remap.n_outputs = 1;
        let mac = macro_node(3, &[("aa11bb22", "bool"), ("cc33dd44", "float")]);
        let sink = sink_node(4, "virtual.xinput:0", "remap:2", true);
        let graph = ProcessingGraph { nodes: vec![src, remap, mac, sink] };
        let mut state = HashMap::new();
        let mut out = TickOutput::default();
        let press = |on: bool| {
            let mut m = HashMap::new();
            m.insert((dev.to_string(), "btn_south".to_string()), Signal::Bool(on));
            m
        };

        eval_graph_tick(&graph, &mut state, &press(true), 0.016, &mut out);
        assert_eq!(out.outputs.get(&(3, 0)).copied().flatten(), Some(Signal::Bool(true)),
            "mapped macro Bool port must assert while the chord is held");
        assert_eq!(out.outputs.get(&(3, 1)).copied().flatten(), Some(Signal::Float(0.0)),
            "unmapped Float port emits its typed off value");
        assert!(out.sink_outputs.keys().all(|(_, p)| !p.starts_with("macro:")),
            "macro pins must never reach a sink");

        eval_graph_tick(&graph, &mut state, &press(false), 0.016, &mut out);
        assert_eq!(out.outputs.get(&(3, 0)).copied().flatten(), Some(Signal::Bool(false)),
            "released mapping must drop the port back to false");
    }

    // A Virtual Menu placed UPSTREAM of the Remapper that maps a button to its
    // Select target is a feedback cycle: the Remapper is forced to evaluate
    // AFTER the menu, so this tick's `collector_sigs` never carries the Select
    // value when the menu reads it. The cross-tick macro carry-over
    // (`NodeState::macro_prev`) delivers it one tick later, so `select_on =
    // "press"` fires. Also exercises the Show target opening the menu the same
    // way. Node order [src, menu, remap, sink] reproduces the cyclic fallback
    // (menu before its producer).
    #[test]
    fn menu_select_from_downstream_remapper_via_carryover() {
        let dev = "gilrs:switch_pro:0";
        let src = source_node(1, dev, 0.0);

        let mut menu = empty_node(2, "module.menu");
        menu.params.insert("_automap_device_id".into(), Value::String(dev.into()));
        menu.params.insert("menu_id".into(), Value::String("abcd1234".into()));
        menu.params.insert("select_on".into(), Value::String("press".into()));
        menu.params.insert("col_edges".into(), serde_json::json!([0.5]));
        menu.params.insert("row_edges".into(), serde_json::json!([0.5]));
        menu.params.insert("zone_mode".into(), Value::String("mapping".into()));
        menu.params.insert("zone_maps".into(), serde_json::json!([
            { "f": 0, "z": 0, "in": ["menu_sel"], "out": ["btn_north"] }
        ]));
        menu.n_outputs = 3;
        menu.output_pin_ids = vec![
            "automap_pass".into(), "menu_open".into(), "menu_hover".into(),
        ];

        let mut remap = empty_node(3, "module.remapper");
        remap.params.insert("_automap_device_id".into(), Value::String(dev.into()));
        remap.params.insert("mappings".into(), serde_json::json!([
            { "in": ["btn_start"], "out": ["menu:abcd1234_show"] },
            { "in": ["btn_south"], "out": ["menu:abcd1234_sel"] },
        ]));
        remap.input_sources = vec![Some((0, 0))];
        remap.n_outputs = 1;

        // Sink pulls the menu's published bus (where the zone card writes btn_north).
        let sink = sink_node(4, "virtual.xinput:0", "menumap:2", false);

        let graph = ProcessingGraph { nodes: vec![src, menu, remap, sink] };
        let mut state = HashMap::new();
        let mut out = TickOutput::default();

        // btn_start held (Show), stick points top-left (zone 0); btn_south varies.
        let sigs = |south: bool| {
            let mut m = HashMap::new();
            m.insert((dev.to_string(), "btn_start".to_string()), Signal::Bool(true));
            m.insert((dev.to_string(), "btn_south".to_string()), Signal::Bool(south));
            m.insert((dev.to_string(), "left_stick".to_string()), Signal::Vec2(Vec2::new(-0.8, 0.8)));
            m
        };
        let north = |o: &TickOutput| o.sink_outputs
            .get(&("virtual.xinput:0".to_string(), "btn_north".to_string()))
            .map(|s| s.as_bool()).unwrap_or(false);
        let menu_open = |o: &TickOutput| o.last_outputs.get(&2)
            .and_then(|v| v.get(1)).copied().flatten().map(|s| s.as_bool()).unwrap_or(false);

        // Warm up: the Show macro opens the menu one tick after it's published.
        for _ in 0..4 {
            eval_graph_tick(&graph, &mut state, &sigs(false), 0.016, &mut out);
        }
        assert!(menu_open(&out), "macro Show target must open the menu via the carry-over");
        assert!(!north(&out), "no selection before Select is pressed");
        let hover = out.last_outputs.get(&2).and_then(|v| v.get(2)).copied().flatten();
        assert_eq!(hover, Some(Signal::Float(0.0)), "stick must hover zone 0 after warm-up");

        // Press Select: the menu sees a STALE (false) value this tick — the
        // Remapper only publishes it now, one node later …
        eval_graph_tick(&graph, &mut state, &sigs(true), 0.016, &mut out);
        assert!(!north(&out), "Select is one node downstream — not visible the same tick");
        // … and reads it via the carry-over on the next tick, firing the card
        // whose btn_north reaches the sink through the menu's published bus.
        eval_graph_tick(&graph, &mut state, &sigs(true), 0.016, &mut out);
        assert!(north(&out),
            "press-mode Select from a downstream Remapper must fire the zone card via the carry-over");
    }

    // The Virtual Menu's SOURCE-BLOCK must suppress a navigation input even when a
    // PARALLEL Combiner port carries a RAW copy that bypasses the menu — the exact
    // leak the user hit (SORT picks the raw port over the menu's zero). Blocking at
    // the source (dev_sigs) zeroes the raw port too, so nothing reaches the sink.
    #[test]
    fn menu_blocks_navigation_input_at_sink_despite_parallel_raw_port() {
        let dev = "gilrs:switch_pro:0";
        let src = source_node(1, dev, 0.0);

        // Menu opened by a Remapper's Show macro; suppresses the left stick.
        let mut menu = empty_node(2, "module.menu");
        menu.params.insert("_automap_device_id".into(), Value::String(dev.into()));
        menu.params.insert("menu_id".into(), Value::String("abcd1234".into()));
        menu.n_outputs = 3;
        menu.output_pin_ids = vec![
            "automap_pass".into(), "menu_open".into(), "menu_hover".into(),
        ];

        let mut remap = empty_node(3, "module.remapper");
        remap.params.insert("_automap_device_id".into(), Value::String(dev.into()));
        remap.params.insert("mappings".into(), serde_json::json!([
            { "in": ["btn_start"], "out": ["menu:abcd1234_show"] },
        ]));
        remap.input_sources = vec![Some((0, 0))];
        remap.n_outputs = 1;

        // Combiner: port 0 = menu bus (suppressed), port 1 = RAW device (leak path).
        let mut comb = empty_node(4, "module.automap_combiner");
        comb.params.insert("_automap_input_devs".into(), Value::Array(vec![
            Value::String(String::new()), Value::String(dev.into()),
        ]));
        comb.params.insert("_automap_input_collectors".into(), Value::Array(vec![
            Value::String("menumap:2".into()), Value::String(String::new()),
        ]));
        comb.input_sources = vec![Some((0, 0)), Some((1, 0))];

        // sink_node sets automap_fallback_dev = the switch_pro pad — the physical
        // source the menu keys its block by.
        let sink = sink_node(5, "virtual.xinput:0", "combiner:4", false);

        let graph = ProcessingGraph { nodes: vec![src, menu, remap, comb, sink] };
        let mut state = HashMap::new();
        let mut out = TickOutput::default();

        // btn_start opens the menu; the left stick is fully deflected.
        let sigs = || {
            let mut m = HashMap::new();
            m.insert((dev.to_string(), "btn_start".to_string()), Signal::Bool(true));
            m.insert((dev.to_string(), "left_stick".to_string()), Signal::Vec2(Vec2::new(0.9, 0.0)));
            m.insert((dev.to_string(), "left_stick_x".to_string()), Signal::Float(0.9));
            m
        };
        for _ in 0..4 { eval_graph_tick(&graph, &mut state, &sigs(), 0.016, &mut out); }

        assert!(out.last_outputs.get(&2).and_then(|v| v.get(1)).copied().flatten()
            .map(|s| s.as_bool()).unwrap_or(false), "menu should be open after warm-up");

        // Combiner SORT lets the raw port's 0.9 win, but the sink block zeroes it.
        let lx = out.sink_outputs.get(&("virtual.xinput:0".to_string(), "left_stick_x".to_string()))
            .map(|s| s.as_float());
        let lv = out.sink_outputs.get(&("virtual.xinput:0".to_string(), "left_stick".to_string()))
            .and_then(|s| if let Signal::Vec2(v) = s { Some(v.x) } else { None });
        let v = lx.or(lv).unwrap_or(0.0);
        assert!(v.abs() < 1e-4,
            "menu must suppress the navigation stick at the game boundary even via a parallel raw port, got {v}");
    }

    // An analog-mode mapping targeting a Float macro port passes the live
    // stick magnitude through — continuous, not a binary gate — and a Bool
    // port fed by the same analog write thresholds at 0.5.
    #[test]
    fn remapper_analog_mapping_drives_float_macro() {
        let dev = "gilrs:switch_pro:0";
        let src = source_node(1, dev, 0.0);
        let mut remap = empty_node(2, "module.remapper");
        remap.params.insert("_automap_device_id".into(), Value::String(dev.into()));
        remap.params.insert("mappings".into(), serde_json::json!([
            { "in": ["left_stick_up"], "out": ["macro:f1f1f1f1"], "mode": "analog" }
        ]));
        remap.input_sources = vec![Some((0, 0))];
        remap.n_outputs = 1;
        let mac = macro_node(3, &[("f1f1f1f1", "float")]);
        let graph = ProcessingGraph { nodes: vec![src, remap, mac] };
        let mut state = HashMap::new();
        let mut out = TickOutput::default();
        let push = |y: f32| {
            let mut m = HashMap::new();
            m.insert((dev.to_string(), "left_stick_y".to_string()), Signal::Float(y));
            m
        };

        eval_graph_tick(&graph, &mut state, &push(0.5), 0.016, &mut out);
        let v = out.outputs.get(&(3, 0)).copied().flatten().map(|s| s.as_float()).unwrap_or(-1.0);
        assert!((v - 0.5).abs() < 0.05, "half push should give ~0.5 on the Float port, got {v}");

        eval_graph_tick(&graph, &mut state, &push(0.0), 0.016, &mut out);
        let v = out.outputs.get(&(3, 0)).copied().flatten().map(|s| s.as_float()).unwrap_or(-1.0);
        assert!(v.abs() < 0.01, "neutral stick should release the port to 0, got {v}");
    }

    // A Touch Zones card targeting a macro pin publishes BOTH aspects: the
    // shaped gate (Bool) and the zone-local deflection (Vec2). The macro node
    // then coerces per port type: Vec2 passes through, Float takes the
    // magnitude, Bool follows the gate.
    #[test]
    fn touch_zones_card_drives_macro_aspects() {
        let mut tz = empty_node(1, "module.touch_zones");
        tz.params.insert("zone_mode".into(), Value::String("mapping".into()));
        tz.params.insert("_automap_device_id".into(), Value::String("pad".into()));
        tz.params.insert("col_edges".into(), serde_json::json!([]));
        tz.params.insert("row_edges".into(), serde_json::json!([]));
        tz.params.insert("zone_maps".into(), serde_json::json!([
            {"f":0,"z":0,"in":["tz_touch"],"out":["macro:abcd0123"]},
        ]));
        // Finger at pad center then pushed right: unit x 0.5→0.75 within the
        // single full-pad zone → deflection x ≈ +0.5 from the zone center.
        let finger = |px: f32| {
            let mut m: HashMap<(String, String), Signal> = HashMap::new();
            m.insert(("pad".into(), "touch1_active".into()), Signal::Bool(true));
            m.insert(("pad".into(), "touch1_x".into()), Signal::Float(px));
            m.insert(("pad".into(), "touch1_y".into()), Signal::Float(0.0));
            m
        };
        let mut state = HashMap::new();
        let mut c: HashMap<(String, String), Signal> = HashMap::new();
        // Land at center (adaptive center latches there), then move right.
        eval_touch_zones_map_node(&tz, 1, &finger(0.0), &mut c, &mut state, 0.016);
        c.clear();
        eval_touch_zones_map_node(&tz, 1, &finger(0.5), &mut c, &mut state, 0.016);

        assert_eq!(c.get(&("macro".to_string(), "macro:abcd0123".to_string())).copied(),
            Some(Signal::Bool(true)), "gate aspect must assert while touched");
        let v2 = c.get(&("macro#v2".to_string(), "macro:abcd0123".to_string())).copied();
        let Some(Signal::Vec2(v)) = v2 else { panic!("deflection aspect missing: {v2:?}") };
        assert!(v.x > 0.4 && v.y.abs() < 0.05, "rightward deflection expected, got {v:?}");
        assert!(c.iter().all(|((k, p), _)| k != "touchmap:1" || !p.starts_with("macro:")),
            "macro pins must not be published on the touchmap bus key");

        // Coercion: read the same namespace back through each port type.
        let mut ns = NodeState::default();
        let dev_sigs = HashMap::new();
        let mac = macro_node(2, &[("abcd0123", "vec2")]);
        let out = compute_node(&mac, &[], &mut ns, &dev_sigs, &c, 0.016);
        assert!(matches!(out[0], Some(Signal::Vec2(v)) if v.x > 0.4),
            "Vec2 port passes the deflection through, got {:?}", out[0]);
        let mac = macro_node(2, &[("abcd0123", "float")]);
        let out = compute_node(&mac, &[], &mut ns, &dev_sigs, &c, 0.016);
        assert!(matches!(out[0], Some(Signal::Float(f)) if (f - 0.5).abs() < 0.05),
            "Float port prefers the deflection magnitude over the binary gate, got {:?}", out[0]);
        let mac = macro_node(2, &[("abcd0123", "bool")]);
        let out = compute_node(&mac, &[], &mut ns, &dev_sigs, &c, 0.016);
        assert_eq!(out[0], Some(Signal::Bool(true)), "Bool port follows the gate");
    }

    // 3DOF-Lean mappings targeting macro pins: analog mode passes the live
    // lean magnitude; digital (down) mode asserts while the side is active.
    #[test]
    fn lean_mapping_drives_macro_port() {
        let mk = |mode: &str| {
            let mut n = empty_node(1, "processing.gyro_3dof");
            n.params.insert("lean_left".into(), serde_json::json!([
                { "out": ["macro:11aa22bb"], "mode": mode }
            ]));
            n
        };
        let outs = |lean: f32| vec![None, None, None, Some(Signal::Float(lean))];
        let get = |c: &HashMap<(String, String), Signal>|
            c.get(&("macro".to_string(), "macro:11aa22bb".to_string())).copied();

        // Analog: leaning left at 0.8 → Float(0.8) on the macro namespace.
        let snap = mk("analog");
        let mut ns = NodeState::default();
        let mut c = HashMap::new();
        lean_dispatch_into_collector_sigs(&snap, 1, &outs(-0.8), &mut ns, &mut c, 0.016);
        let v = get(&c).map(|s| s.as_float()).unwrap_or(-1.0);
        assert!((v - 0.8).abs() < 1e-4, "analog lean should pass magnitude, got {v}");
        // Below threshold → no write (port reads as released).
        c.clear();
        lean_dispatch_into_collector_sigs(&snap, 1, &outs(-0.1), &mut ns, &mut c, 0.016);
        assert_eq!(get(&c), None, "below-threshold lean must not assert the port");

        // Down mode: asserts Bool while the side is active.
        let snap = mk("down");
        let mut ns = NodeState::default();
        let mut c = HashMap::new();
        lean_dispatch_into_collector_sigs(&snap, 1, &outs(-0.8), &mut ns, &mut c, 0.016);
        assert_eq!(get(&c), Some(Signal::Bool(true)));
    }

    // ── Per-card response curve + manual activation threshold ────────────────

    fn curve_remap_graph(mapping: serde_json::Value) -> ProcessingGraph {
        let dev = "gilrs:switch_pro:0";
        let src = source_node(1, dev, 0.0);
        let mut remap = empty_node(2, "module.remapper");
        remap.params.insert("_automap_device_id".into(), Value::String(dev.into()));
        remap.params.insert("mappings".into(), serde_json::json!([mapping]));
        remap.input_sources = vec![Some((0, 0))];
        remap.n_outputs = 1;
        let sink = sink_node(3, "virtual.xinput:0", "remap:2", true);
        ProcessingGraph { nodes: vec![src, remap, sink] }
    }

    fn stick_y(y: f32) -> HashMap<(String, String), Signal> {
        let mut m = HashMap::new();
        m.insert(("gilrs:switch_pro:0".to_string(), "left_stick_y".to_string()), Signal::Float(y));
        m
    }

    fn sinkv(out: &TickOutput, pin: &str) -> Option<Signal> {
        out.sink_outputs.get(&("virtual.xinput:0".to_string(), pin.to_string())).copied()
    }

    // An analog mapping's per-card curve reshapes the emitted magnitude —
    // a halving curve turns a full stick push into ~0.5 trigger travel.
    #[test]
    fn remapper_analog_curve_shapes_output() {
        let graph = curve_remap_graph(serde_json::json!({
            "in": ["left_stick_up"], "out": ["right_trigger"], "mode": "analog",
            "curve": [[0.0, 0.0], [1.0, 0.5]],
        }));
        let mut state = HashMap::new();
        let mut out = TickOutput::default();
        eval_graph_tick(&graph, &mut state, &stick_y(1.0), 0.016, &mut out);
        let v = sinkv(&out, "right_trigger").map(|s| s.as_float()).unwrap_or(-1.0);
        assert!((v - 0.5).abs() < 0.05, "halving curve should give ~0.5 at full push, got {v}");
    }

    // Manual threshold on an analog→digital mapping: PLAIN HOLD above the
    // line (steady across ticks — no tap train), release the moment the
    // shaped value dips below.
    #[test]
    fn remapper_analog_threshold_holds_digital() {
        let graph = curve_remap_graph(serde_json::json!({
            "in": ["left_stick_up"], "out": ["btn_east"], "mode": "analog",
            "threshold": 0.6,
        }));
        let mut state = HashMap::new();
        let mut out = TickOutput::default();
        let east = |out: &TickOutput| sinkv(out, "btn_east").map(|s| s.as_bool()).unwrap_or(false);

        eval_graph_tick(&graph, &mut state, &stick_y(0.4), 0.016, &mut out);
        assert!(!east(&out), "below threshold must stay released");
        // Above threshold: held EVERY tick — the legacy pulse train would
        // toggle off within this window.
        for tick in 0..20 {
            eval_graph_tick(&graph, &mut state, &stick_y(0.8), 0.016, &mut out);
            assert!(east(&out), "threshold hold must be steady (tick {tick})");
        }
        eval_graph_tick(&graph, &mut state, &stick_y(0.4), 0.016, &mut out);
        assert!(!east(&out), "dipping below the line must release");
    }

    // Manual threshold on a DIGITAL-mode mapping with a cardinal input
    // overrides the built-in cardinal derivation (~0.5): the mapping only
    // fires past the card's own line.
    #[test]
    fn remapper_digital_threshold_overrides_cardinal() {
        let graph = curve_remap_graph(serde_json::json!({
            "in": ["left_stick_up"], "out": ["btn_east"],
            "threshold": 0.8,
        }));
        let mut state = HashMap::new();
        let mut out = TickOutput::default();
        let east = |out: &TickOutput| sinkv(out, "btn_east").map(|s| s.as_bool()).unwrap_or(false);

        eval_graph_tick(&graph, &mut state, &stick_y(0.6), 0.016, &mut out);
        assert!(!east(&out), "0.6 push is past the built-in derivation but below the card threshold");
        eval_graph_tick(&graph, &mut state, &stick_y(0.9), 0.016, &mut out);
        assert!(east(&out), "0.9 push crosses the card threshold");
        eval_graph_tick(&graph, &mut state, &stick_y(0.6), 0.016, &mut out);
        assert!(!east(&out), "falling back below the threshold releases");
    }

    // Lean cards: a per-card threshold replaces the node lean_threshold for
    // that card, and a curve reshapes the analog magnitude the card emits.
    #[test]
    fn lean_card_threshold_and_curve() {
        // Threshold 0.7 on a down-mode card: node threshold (0.3) alone
        // would fire at 0.5 lean — the card must not.
        let mut n = empty_node(1, "processing.gyro_3dof");
        n.params.insert("lean_left".into(), serde_json::json!([
            { "out": ["btn_south"], "mode": "down", "threshold": 0.7 }
        ]));
        let outs = |lean: f32| vec![None, None, None, Some(Signal::Float(lean))];
        let get = |c: &HashMap<(String, String), Signal>, pin: &str|
            c.get(&("lean:1".to_string(), pin.to_string())).copied();
        let mut ns = NodeState::default();
        let mut c = HashMap::new();
        lean_dispatch_into_collector_sigs(&n, 1, &outs(-0.5), &mut ns, &mut c, 0.016);
        assert_eq!(get(&c, "btn_south"), Some(Signal::Bool(false)),
            "below the card threshold the mapping must not fire");
        c.clear();
        lean_dispatch_into_collector_sigs(&n, 1, &outs(-0.8), &mut ns, &mut c, 0.016);
        assert_eq!(get(&c, "btn_south"), Some(Signal::Bool(true)));

        // Halving curve on an analog card: full lean → ~0.5 on the Float out.
        let mut n = empty_node(1, "processing.gyro_3dof");
        n.params.insert("lean_right".into(), serde_json::json!([
            { "out": ["right_trigger"], "mode": "analog",
              "curve": [[0.0, 0.0], [1.0, 0.5]] }
        ]));
        let mut ns = NodeState::default();
        let mut c = HashMap::new();
        lean_dispatch_into_collector_sigs(&n, 1, &outs(1.0), &mut ns, &mut c, 0.016);
        let v = get(&c, "right_trigger").map(|s| s.as_float()).unwrap_or(-1.0);
        assert!((v - 0.5).abs() < 0.01, "curve must shape the analog lean magnitude, got {v}");
    }

    // Multiple writers to one macro port merge by larger magnitude, in either
    // arrival order — an asserted mapping beats an idle/weaker one.
    #[test]
    fn macro_merge_larger_magnitude_wins() {
        let mut c: HashMap<(String, String), Signal> = HashMap::new();
        merge_macro_scalar(&mut c, "macro:x", Signal::Float(0.3));
        merge_macro_scalar(&mut c, "macro:x", Signal::Bool(true)); // mag 1.0
        merge_macro_scalar(&mut c, "macro:x", Signal::Float(0.6));
        assert_eq!(c.get(&("macro".to_string(), "macro:x".to_string())).copied(),
            Some(Signal::Bool(true)), "largest-magnitude write must win");
    }

    // Remapper in analog mode mapping a stick cardinal → right_trigger should
    // produce a CONTINUOUS value tracking how far the stick is pushed, not a
    // binary 0/1. Regression guard for the "stick→trigger outputs binary" bug.
    #[test]
    fn remapper_analog_stick_to_trigger_is_continuous() {
        let remap_uid = 1usize;
        let mut remap = empty_node(remap_uid, "module.remapper");
        remap.params.insert("_automap_device_id".into(), Value::String("gilrs:switch_pro:0".into()));
        remap.params.insert("mappings".into(), serde_json::json!([
            { "in": ["left_stick_up"], "out": ["right_trigger"], "mode": "analog" }
        ]));

        // Zero-deadzone source so this test measures continuity, not the
        // deadzone curve (deadzone is covered by the dedicated tests above).
        let src = source_node(3, "gilrs:switch_pro:0", 0.0);
        let sink = sink_node(2, "virtual.xinput:0", &format!("remap:{remap_uid}"), true);
        let graph = ProcessingGraph { nodes: vec![src, remap, sink] };

        let mut state = HashMap::new();
        let mut out = TickOutput::default();

        // Stick pushed halfway up (y = +0.5).
        let mut dev = HashMap::new();
        dev.insert(("gilrs:switch_pro:0".to_string(), "left_stick_y".to_string()), Signal::Float(0.5));
        eval_graph_tick(&graph, &mut state, &dev, 0.016, &mut out);
        let v = out.sink_outputs.get(&("virtual.xinput:0".to_string(), "right_trigger".to_string()))
            .map(|s| s.as_float()).unwrap_or(-1.0);
        assert!((v - 0.5).abs() < 0.05, "half stick push should give ~0.5 trigger, got {v}");

        // Full push → full trigger.
        dev.insert(("gilrs:switch_pro:0".to_string(), "left_stick_y".to_string()), Signal::Float(1.0));
        eval_graph_tick(&graph, &mut state, &dev, 0.016, &mut out);
        let v = out.sink_outputs.get(&("virtual.xinput:0".to_string(), "right_trigger".to_string()))
            .map(|s| s.as_float()).unwrap_or(-1.0);
        assert!((v - 1.0).abs() < 0.05, "full stick push should give ~1.0 trigger, got {v}");

        // Neutral stick → trigger releases to 0.
        dev.insert(("gilrs:switch_pro:0".to_string(), "left_stick_y".to_string()), Signal::Float(0.0));
        eval_graph_tick(&graph, &mut state, &dev, 0.016, &mut out);
        let v = out.sink_outputs.get(&("virtual.xinput:0".to_string(), "right_trigger".to_string()))
            .map(|s| s.as_float()).unwrap_or(0.0);
        assert!(v.abs() < 0.05, "neutral stick should release trigger to 0, got {v}");
    }

    // A Remapper captures its output by chord-learning, so on a Switch Pro the
    // user maps to the DIGITAL ZR button (`btn_rt_dig`), not `right_trigger`.
    // In analog mode that digital-trigger target must still produce continuous
    // analog travel on the virtual pad — not a binary press.
    #[test]
    fn remapper_analog_to_digital_trigger_button_is_continuous() {
        let remap_uid = 1usize;
        let mut remap = empty_node(remap_uid, "module.remapper");
        remap.params.insert("_automap_device_id".into(), Value::String("gilrs:switch_pro:0".into()));
        remap.params.insert("mappings".into(), serde_json::json!([
            { "in": ["left_stick_up"], "out": ["btn_rt_dig"], "mode": "analog" }
        ]));
        let src = source_node(3, "gilrs:switch_pro:0", 0.0);
        let sink = sink_node(2, "virtual.xinput:0", &format!("remap:{remap_uid}"), true);
        let graph = ProcessingGraph { nodes: vec![src, remap, sink] };
        let mut state = HashMap::new();
        let mut out = TickOutput::default();

        let mut dev = HashMap::new();
        dev.insert(("gilrs:switch_pro:0".to_string(), "left_stick_y".to_string()), Signal::Float(0.5));
        eval_graph_tick(&graph, &mut state, &dev, 0.016, &mut out);
        let v = out.sink_outputs.get(&("virtual.xinput:0".to_string(), "right_trigger".to_string()))
            .map(|s| s.as_float()).unwrap_or(-1.0);
        assert!((v - 0.5).abs() < 0.05, "analog map to digital ZR should give ~0.5 analog RT, got {v}");
    }

    // Two analog mappings sharing an input but with different outputs must BOTH
    // fire: left_stick_up→right_trigger AND left_stick_up→left_stick_up should
    // drive the trigger AND keep the stick output (not replace one another).
    #[test]
    fn analog_same_input_different_outputs_both_fire() {
        let remap_uid = 1usize;
        let mut remap = empty_node(remap_uid, "module.remapper");
        remap.params.insert("_automap_device_id".into(), Value::String("gilrs:switch_pro:0".into()));
        remap.params.insert("mappings".into(), serde_json::json!([
            { "in": ["left_stick_up"], "out": ["right_trigger"],  "mode": "analog" },
            { "in": ["left_stick_up"], "out": ["left_stick_up"],  "mode": "analog" }
        ]));
        let sink = sink_node(2, "virtual.xinput:0", &format!("remap:{remap_uid}"), true);
        let graph = ProcessingGraph { nodes: vec![remap, sink] };
        let mut state = HashMap::new();
        let mut out = TickOutput::default();

        let mut dev = HashMap::new();
        dev.insert(("gilrs:switch_pro:0".to_string(), "left_stick_y".to_string()), Signal::Float(1.0));
        eval_graph_tick(&graph, &mut state, &dev, 0.016, &mut out);

        let rt = out.sink_outputs.get(&("virtual.xinput:0".to_string(), "right_trigger".to_string()))
            .map(|s| s.as_float()).unwrap_or(-1.0);
        assert!((rt - 1.0).abs() < 0.05, "trigger mapping should still fire, got RT={rt}");
        // The stick output must be preserved (left_stick_y stays at +1).
        let ly = out.sink_outputs.get(&("virtual.xinput:0".to_string(), "left_stick_y".to_string()))
            .map(|s| s.as_float());
        let lstick = out.sink_outputs.get(&("virtual.xinput:0".to_string(), "left_stick".to_string()))
            .and_then(|s| if let Signal::Vec2(v) = s { Some(v.y) } else { None });
        let y = ly.or(lstick).unwrap_or(-1.0);
        assert!((y - 1.0).abs() < 0.05, "stick output should be preserved, got left_stick_y={y}");
    }

    // A Remapper's mapped OUTPUT pin must survive a downstream Combiner whose
    // higher-priority port carries the raw device bus. Regression for the
    // "General purpose preset" button→button bug: a real controller reports
    // every button each tick (false when up), so the raw-bus Collector on port 0
    // explicitly carries `btn_rb = false`. With the old SORT (`first port wins`)
    // that false value clobbered the Remapper's `btn_rb = true` on port 1 — so a
    // single mapped button produced nothing, yet pressing both swapped buttons
    // lit both (those pins ARE consumed and take the hierarchy branch).
    //
    // Topology:  device → Collector (port 0) ┐
    //            device → Remapper  (port 1) ├→ Combiner → sink
    #[test]
    fn remapped_output_survives_combiner_raw_bus_priority() {
        let dev = "gilrs:switch_pro:0";
        let src = source_node(1, dev, 0.0);

        let mut collect = empty_node(2, "module.automap_collect");
        collect.params.insert("_automap_device_id".into(), Value::String(dev.into()));
        collect.input_sources = vec![Some((0, 0))];
        collect.n_outputs = 1;

        let mut remap = empty_node(3, "module.remapper");
        remap.params.insert("_automap_device_id".into(), Value::String(dev.into()));
        remap.params.insert("mappings".into(), serde_json::json!([
            { "in": ["btn_south"], "out": ["btn_east"] },
            { "in": ["btn_east"],  "out": ["btn_rb"]   },
            { "in": ["btn_rb"],    "out": ["btn_east"] }
        ]));
        remap.input_sources = vec![Some((0, 0))];
        remap.n_outputs = 1;

        let mut combiner = empty_node(4, "module.automap_combiner");
        combiner.params.insert("_automap_input_devs".into(), Value::Array(vec![
            Value::String(String::new()), Value::String(String::new()),
        ]));
        combiner.params.insert("_automap_input_collectors".into(), Value::Array(vec![
            Value::String("collector:2".into()), Value::String("remap:3".into()),
        ]));
        combiner.input_sources = vec![Some((2, 0)), Some((3, 0))];
        combiner.n_outputs = 1;

        let sink = sink_node(20, "virtual.xinput:0", "combiner:4", true);
        let graph = ProcessingGraph { nodes: vec![src, collect, remap, combiner, sink] };
        let mut state = HashMap::new();
        let mut out = TickOutput::default();
        let getb = |out: &TickOutput, pin: &str| out.sink_outputs
            .get(&("virtual.xinput:0".to_string(), pin.to_string())).map(|s| s.as_bool()).unwrap_or(false);
        // A real controller reports EVERY button each tick (false when up).
        let press = |pins: &[&str]| {
            let mut m = HashMap::new();
            for p in ["btn_south", "btn_east", "btn_rb", "btn_west", "btn_north", "btn_lb"] {
                m.insert((dev.to_string(), p.to_string()), Signal::Bool(pins.contains(&p)));
            }
            m
        };
        let tick = |graph: &ProcessingGraph, state: &mut HashMap<usize, NodeState>,
                    out: &mut TickOutput, pins: &[&str]| {
            // Settle (release) between presses so press-mode edges are clean.
            eval_graph_tick(graph, state, &press(&[]), 0.016, out);
            eval_graph_tick(graph, state, &press(pins), 0.016, out);
        };

        // south → east
        tick(&graph, &mut state, &mut out, &["btn_south"]);
        assert!(getb(&out, "btn_east"), "south→east must fire btn_east");
        assert!(!getb(&out, "btn_south"), "consumed btn_south must be suppressed");

        // east → rb
        tick(&graph, &mut state, &mut out, &["btn_east"]);
        assert!(getb(&out, "btn_rb"), "east→rb must fire btn_rb");
        assert!(!getb(&out, "btn_east"), "consumed btn_east must be suppressed");

        // rb → east
        tick(&graph, &mut state, &mut out, &["btn_rb"]);
        assert!(getb(&out, "btn_east"), "rb→east must fire btn_east");
        assert!(!getb(&out, "btn_rb"), "consumed btn_rb must be suppressed");

        // Pressing the swapped pair leaves both asserted (east↔rb swap).
        tick(&graph, &mut state, &mut out, &["btn_east", "btn_rb"]);
        assert!(getb(&out, "btn_east") && getb(&out, "btn_rb"),
            "east+rb swap should leave both asserted");
    }

    // Touchpad zone outputs synthesize finger touch points on the virtual pad,
    // and two simultaneous zone mappings stack onto the 2 hardware touch points.
    #[test]
    fn remapper_touch_zones_synthesize_and_stack() {
        let dev = "gilrs:switch_pro:0";
        let remap_uid = 1usize;
        let mut remap = empty_node(remap_uid, "module.remapper");
        remap.params.insert("_automap_device_id".into(), Value::String(dev.into()));
        remap.params.insert("mappings".into(), serde_json::json!([
            { "in": ["btn_south"], "out": ["touch_left"]  },
            { "in": ["btn_east"],  "out": ["touch_right"] }
        ]));
        let sink = sink_node(2, "virtual.xinput:0", &format!("remap:{remap_uid}"), false);
        let graph = ProcessingGraph { nodes: vec![remap, sink] };
        let mut state = HashMap::new();
        let mut out = TickOutput::default();
        let getf = |out: &TickOutput, pin: &str| out.sink_outputs
            .get(&("virtual.xinput:0".to_string(), pin.to_string())).map(|s| s.as_float());
        let getb = |out: &TickOutput, pin: &str| out.sink_outputs
            .get(&("virtual.xinput:0".to_string(), pin.to_string())).map(|s| s.as_bool()).unwrap_or(false);

        // South only → one finger at the left zone.
        let mut dev_sigs = HashMap::new();
        dev_sigs.insert((dev.to_string(), "btn_south".to_string()), Signal::Bool(true));
        eval_graph_tick(&graph, &mut state, &dev_sigs, 0.016, &mut out);
        assert!(getb(&out, "touch1_active"), "south→touch_left must activate a touch point");
        assert!((getf(&out, "touch1_x").unwrap_or(0.0) - (-0.66)).abs() < 0.05,
            "left zone x≈-0.66, got {:?}", getf(&out, "touch1_x"));
        assert!(!getb(&out, "touch2_active"), "only one finger for a single zone mapping");

        // South + East → two stacked fingers (left + right).
        dev_sigs.insert((dev.to_string(), "btn_east".to_string()), Signal::Bool(true));
        eval_graph_tick(&graph, &mut state, &dev_sigs, 0.016, &mut out);
        assert!(getb(&out, "touch1_active") && getb(&out, "touch2_active"),
            "two zone mappings must stack onto 2 touch points");
        assert!((getf(&out, "touch2_x").unwrap_or(0.0) - 0.66).abs() < 0.05,
            "right zone x≈+0.66, got {:?}", getf(&out, "touch2_x"));

        // Release → both points report inactive (no latch).
        eval_graph_tick(&graph, &mut state, &HashMap::new(), 0.016, &mut out);
        assert!(!getb(&out, "touch1_active") && !getb(&out, "touch2_active"),
            "released zone mappings must release the touch points");
    }

    // "Hold zone": a tz_touch gesture that STARTS in a hold zone keeps firing that
    // zone's mapping even after the finger slides into a neighbour, and the
    // neighbour must NOT fire. Without the flag, crossing switches zones.
    #[test]
    fn touch_zones_hold_keeps_origin_zone_on_crossing() {
        let mk = |hold: bool| {
            let mut n = empty_node(1, "module.touch_zones");
            n.params.insert("zone_mode".into(), Value::String("mapping".into()));
            n.params.insert("_automap_device_id".into(), Value::String("pad".into()));
            n.params.insert("col_edges".into(), serde_json::json!([0.5])); // 2 columns
            n.params.insert("row_edges".into(), serde_json::json!([]));
            n.params.insert("zone_maps".into(), serde_json::json!([
                {"f":0,"z":0,"in":["tz_touch"],"out":["btn_south"]},
                {"f":0,"z":1,"in":["tz_touch"],"out":["btn_east"]},
            ]));
            if hold { n.params.insert("hold_zones".into(), serde_json::json!([[0,0]])); }
            n
        };
        // px in [-1,1] → unit x in [0,1]: -0.5→0.25 (zone 0), +0.5→0.75 (zone 1).
        let finger = |px: f32| {
            let mut m: HashMap<(String, String), Signal> = HashMap::new();
            m.insert(("pad".into(), "touch1_active".into()), Signal::Bool(true));
            m.insert(("pad".into(), "touch1_x".into()), Signal::Float(px));
            m.insert(("pad".into(), "touch1_y".into()), Signal::Float(0.0));
            m
        };
        let getb = |c: &HashMap<(String, String), Signal>, pin: &str|
            c.get(&("touchmap:1".to_string(), pin.to_string())).map(|s| s.as_bool()).unwrap_or(false);

        // WITH hold on zone 0.
        {
            let snap = mk(true);
            let mut state = HashMap::new();
            let mut c = HashMap::new();
            eval_touch_zones_map_node(&snap, 1, &finger(-0.5), &mut c, &mut state, 0.016);
            c.clear();
            eval_touch_zones_map_node(&snap, 1, &finger(-0.5), &mut c, &mut state, 0.016);
            assert!(getb(&c, "btn_south") && !getb(&c, "btn_east"), "zone0 fires btn_south");
            c.clear();
            eval_touch_zones_map_node(&snap, 1, &finger(0.5), &mut c, &mut state, 0.016);
            assert!(getb(&c, "btn_south"), "HOLD: origin zone still fires after crossing");
            assert!(!getb(&c, "btn_east"), "HOLD: crossed-into zone must NOT fire");
        }
        // WITHOUT hold — crossing switches zones.
        {
            let snap = mk(false);
            let mut state = HashMap::new();
            let mut c = HashMap::new();
            eval_touch_zones_map_node(&snap, 1, &finger(-0.5), &mut c, &mut state, 0.016);
            c.clear();
            eval_touch_zones_map_node(&snap, 1, &finger(-0.5), &mut c, &mut state, 0.016);
            c.clear();
            eval_touch_zones_map_node(&snap, 1, &finger(0.5), &mut c, &mut state, 0.016);
            assert!(!getb(&c, "btn_south") && getb(&c, "btn_east"),
                "no hold: crossing switches to the new zone");
        }
    }

    // Hold with an ANALOG origin zone: the analog output holds AND a button
    // mapped in the crossed-into zone must NOT fire (the held finger belongs
    // wholly to its origin; other zones ignore it).
    #[test]
    fn touch_zones_hold_analog_origin_suppresses_crossed_button() {
        let mut n = empty_node(1, "module.touch_zones");
        n.params.insert("zone_mode".into(), Value::String("mapping".into()));
        n.params.insert("_automap_device_id".into(), Value::String("pad".into()));
        n.params.insert("col_edges".into(), serde_json::json!([0.5]));
        n.params.insert("row_edges".into(), serde_json::json!([]));
        n.params.insert("zone_maps".into(), serde_json::json!([
            {"f":0,"z":0,"in":["tz_touch"],"out":["left_stick"]},
            {"f":0,"z":1,"in":["tz_touch"],"out":["btn_east"]},
        ]));
        n.params.insert("hold_zones".into(), serde_json::json!([[0,0]]));
        let finger = |px: f32| {
            let mut m: HashMap<(String, String), Signal> = HashMap::new();
            m.insert(("pad".into(), "touch1_active".into()), Signal::Bool(true));
            m.insert(("pad".into(), "touch1_x".into()), Signal::Float(px));
            m.insert(("pad".into(), "touch1_y".into()), Signal::Float(0.0));
            m
        };
        let mut state = HashMap::new();
        let mut c = HashMap::new();
        // Land in zone 0 (analog origin), establish the start zone.
        eval_touch_zones_map_node(&n, 1, &finger(-0.5), &mut c, &mut state, 0.016);
        c.clear();
        eval_touch_zones_map_node(&n, 1, &finger(-0.5), &mut c, &mut state, 0.016);
        // Cross into zone 1 (button). left_stick keeps outputting; btn_east silent.
        c.clear();
        eval_touch_zones_map_node(&n, 1, &finger(0.5), &mut c, &mut state, 0.016);
        assert!(c.contains_key(&("touchmap:1".to_string(), "left_stick".to_string())),
            "HOLD: analog origin keeps driving left_stick after crossing");
        let btn_east = c.get(&("touchmap:1".to_string(), "btn_east".to_string()))
            .map(|s| s.as_bool()).unwrap_or(false);
        assert!(!btn_east, "HOLD: crossed-into button zone must NOT fire");
    }

    // A zone mapped to the analog scroll pins publishes a Float rate that tracks
    // the finger's deflection (+Y up, +X right) — the variable-speed scroll dest.
    #[test]
    fn touch_zones_analog_scroll_rate_tracks_deflection() {
        let mut n = empty_node(1, "module.touch_zones");
        n.params.insert("zone_mode".into(), Value::String("mapping".into()));
        n.params.insert("_automap_device_id".into(), Value::String("pad".into()));
        n.params.insert("col_edges".into(), serde_json::json!([])); // single zone
        n.params.insert("row_edges".into(), serde_json::json!([]));
        n.params.insert("zone_maps".into(), serde_json::json!([
            {"f":0,"z":0,"in":["tz_touch"],"out":["scroll_y","scroll_x"],"mode":"analog"},
        ]));
        let finger = |px: f32, py: f32| {
            let mut m: HashMap<(String, String), Signal> = HashMap::new();
            m.insert(("pad".into(), "touch1_active".into()), Signal::Bool(true));
            m.insert(("pad".into(), "touch1_x".into()), Signal::Float(px));
            m.insert(("pad".into(), "touch1_y".into()), Signal::Float(py));
            m
        };
        let getf = |c: &HashMap<(String, String), Signal>, pin: &str|
            c.get(&("touchmap:1".to_string(), pin.to_string())).map(|s| s.as_float()).unwrap_or(0.0);
        let mut state = HashMap::new();
        let mut c = HashMap::new();
        // Land at centre to establish the adaptive centre, then deflect up-right
        // (raw pad +Y is up; pad_point_to_unit flips it into y-down unit space).
        eval_touch_zones_map_node(&n, 1, &finger(0.0, 0.0), &mut c, &mut state, 0.016);
        c.clear();
        eval_touch_zones_map_node(&n, 1, &finger(0.8, 0.8), &mut c, &mut state, 0.016);
        assert!(getf(&c, "scroll_y") > 0.0, "upward deflection → scroll up (scroll_y > 0)");
        assert!(getf(&c, "scroll_x") > 0.0, "rightward deflection → scroll right (scroll_x > 0)");
    }

    // A zone can carry BOTH an analog (tz_touch) card and a click (tz_click) card;
    // clicking must still fire the click mapping while the analog output runs.
    #[test]
    fn touch_zones_analog_zone_click_still_fires() {
        let mut n = empty_node(1, "module.touch_zones");
        n.params.insert("zone_mode".into(), Value::String("mapping".into()));
        n.params.insert("_automap_device_id".into(), Value::String("pad".into()));
        n.params.insert("col_edges".into(), serde_json::json!([])); // single zone
        n.params.insert("row_edges".into(), serde_json::json!([]));
        n.params.insert("zone_maps".into(), serde_json::json!([
            {"f":0,"z":0,"in":["tz_touch"],"out":["mouse"],"mode":"analog"},
            {"f":0,"z":0,"in":["tz_click"],"out":["btn_east"],"mode":"down"},
        ]));
        let input = |click: bool| {
            let mut m: HashMap<(String, String), Signal> = HashMap::new();
            m.insert(("pad".into(), "touch1_active".into()), Signal::Bool(true));
            m.insert(("pad".into(), "touch1_x".into()), Signal::Float(0.3));
            m.insert(("pad".into(), "touch1_y".into()), Signal::Float(0.3));
            m.insert(("pad".into(), "btn_touchpad".into()), Signal::Bool(click));
            m
        };
        let getb = |c: &HashMap<(String, String), Signal>, pin: &str|
            c.get(&("touchmap:1".to_string(), pin.to_string())).map(|s| s.as_bool()).unwrap_or(false);
        let mut state = HashMap::new();
        let mut c = HashMap::new();
        eval_touch_zones_map_node(&n, 1, &input(false), &mut c, &mut state, 0.016);
        c.clear();
        eval_touch_zones_map_node(&n, 1, &input(true), &mut c, &mut state, 0.016);
        assert!(getb(&c, "btn_east"), "click on an analog zone must still fire the click mapping");
        assert!(c.contains_key(&("touchmap:1".to_string(), "mouse".to_string())),
            "analog output still runs alongside the click");
    }

    // Analog swipe drives a finger coordinate continuously (absolute position).
    #[test]
    fn remapper_swipe_tracks_analog_input() {
        let dev = "gilrs:switch_pro:0";
        let remap_uid = 1usize;
        let src = source_node(3, dev, 0.0);
        let mut remap = empty_node(remap_uid, "module.remapper");
        remap.params.insert("_automap_device_id".into(), Value::String(dev.into()));
        remap.params.insert("mappings".into(), serde_json::json!([
            { "in": ["left_stick_right"], "out": ["touch_swipe_x"], "mode": "analog" }
        ]));
        let sink = sink_node(2, "virtual.xinput:0", &format!("remap:{remap_uid}"), false);
        let graph = ProcessingGraph { nodes: vec![src, remap, sink] };
        let mut state = HashMap::new();
        let mut out = TickOutput::default();
        let getf = |out: &TickOutput, pin: &str| out.sink_outputs
            .get(&("virtual.xinput:0".to_string(), pin.to_string())).map(|s| s.as_float());
        let getb = |out: &TickOutput, pin: &str| out.sink_outputs
            .get(&("virtual.xinput:0".to_string(), pin.to_string())).map(|s| s.as_bool()).unwrap_or(false);

        // Half deflection → finger at ~+0.5.
        let mut dev_sigs = HashMap::new();
        dev_sigs.insert((dev.to_string(), "left_stick_x".to_string()), Signal::Float(0.5));
        eval_graph_tick(&graph, &mut state, &dev_sigs, 0.016, &mut out);
        assert!(getb(&out, "touch1_active"), "deflected swipe must activate the finger");
        assert!((getf(&out, "touch1_x").unwrap_or(0.0) - 0.5).abs() < 0.05,
            "swipe finger x should track deflection ~0.5, got {:?}", getf(&out, "touch1_x"));

        // Neutral stick → finger released.
        dev_sigs.insert((dev.to_string(), "left_stick_x".to_string()), Signal::Float(0.0));
        eval_graph_tick(&graph, &mut state, &dev_sigs, 0.016, &mut out);
        assert!(!getb(&out, "touch1_active"), "neutral swipe must release the finger");
    }

    // Combo: a BUTTON gates the finger, the LS axes drive both swipe axes (routed
    // by orientation). Buttons must NOT contribute a value (regression for the
    // "stuck at full" bug). Both directions of an axis cover both halves.
    #[test]
    fn remapper_swipe_button_gate_with_two_axis_inputs() {
        let dev = "gilrs:switch_pro:0";
        let remap_uid = 1usize;
        let mut remap = empty_node(remap_uid, "module.remapper");
        remap.params.insert("_automap_device_id".into(), Value::String(dev.into()));
        // Button gate + LS in all 4 directions → swipe X + swipe Y.
        remap.params.insert("mappings".into(), serde_json::json!([
            { "in": ["btn_lb", "left_stick_left", "left_stick_right",
                     "left_stick_up", "left_stick_down"],
              "out": ["touch_swipe_x", "touch_swipe_y"], "mode": "analog" }
        ]));
        // Zero-deadzone source so the test measures the mapping, not the curve.
        let src = source_node(3, dev, 0.0);
        let sink = sink_node(2, "virtual.xinput:0", &format!("remap:{remap_uid}"), false);
        let graph = ProcessingGraph { nodes: vec![src, remap, sink] };
        let mut state = HashMap::new();
        let mut out = TickOutput::default();
        let getf = |out: &TickOutput, pin: &str| out.sink_outputs
            .get(&("virtual.xinput:0".to_string(), pin.to_string())).map(|s| s.as_float()).unwrap_or(0.0);
        let getb = |out: &TickOutput, pin: &str| out.sink_outputs
            .get(&("virtual.xinput:0".to_string(), pin.to_string())).map(|s| s.as_bool()).unwrap_or(false);

        // Stick deflected but button UP → no finger (button gates).
        let mut dev_sigs = HashMap::new();
        dev_sigs.insert((dev.to_string(), "left_stick_x".to_string()), Signal::Float(0.6));
        eval_graph_tick(&graph, &mut state, &dev_sigs, 0.016, &mut out);
        assert!(!getb(&out, "touch1_active"), "button not held → no finger");

        // Button held, stick centered → finger DOWN at center (button gates,
        // analog at rest → NOT stuck at full).
        dev_sigs.insert((dev.to_string(), "btn_lb".to_string()), Signal::Bool(true));
        dev_sigs.insert((dev.to_string(), "left_stick_x".to_string()), Signal::Float(0.0));
        eval_graph_tick(&graph, &mut state, &dev_sigs, 0.016, &mut out);
        assert!(getb(&out, "touch1_active"), "button held → finger active even centered");
        assert!(getf(&out, "touch1_x").abs() < 0.05 && getf(&out, "touch1_y").abs() < 0.05,
            "centered stick → finger at center, got ({},{})", getf(&out,"touch1_x"), getf(&out,"touch1_y"));

        // Button held + stick right → X tracks; right uses the positive half.
        dev_sigs.insert((dev.to_string(), "left_stick_x".to_string()), Signal::Float(0.6));
        eval_graph_tick(&graph, &mut state, &dev_sigs, 0.016, &mut out);
        assert!((getf(&out, "touch1_x") - 0.6).abs() < 0.05,
            "stick right → swipe x ~+0.6, got {}", getf(&out, "touch1_x"));

        // Button held + stick left → negative half of the SAME axis.
        dev_sigs.insert((dev.to_string(), "left_stick_x".to_string()), Signal::Float(-0.8));
        eval_graph_tick(&graph, &mut state, &dev_sigs, 0.016, &mut out);
        assert!((getf(&out, "touch1_x") - (-0.8)).abs() < 0.05,
            "stick left → swipe x ~-0.8, got {}", getf(&out, "touch1_x"));

        // Vertical axis drives swipe Y independently (stick up = +Y).
        dev_sigs.insert((dev.to_string(), "left_stick_x".to_string()), Signal::Float(0.0));
        dev_sigs.insert((dev.to_string(), "left_stick_y".to_string()), Signal::Float(0.5));
        eval_graph_tick(&graph, &mut state, &dev_sigs, 0.016, &mut out);
        assert!((getf(&out, "touch1_y") - 0.5).abs() < 0.05,
            "stick up → swipe y ~+0.5, got {}", getf(&out, "touch1_y"));
    }

    // A touch combo that mixes opposite cardinals of one axis (left+right) can
    // never be "all held at once", so the generic suppression test would never
    // consume its gate button — the button would leak through to pass-through.
    // The touch-combo activation rule must drive suppression: while the combo is
    // active, the gate button (and the driving stick) are consumed.
    #[test]
    fn remapper_touch_combo_suppresses_gate_button_with_multi_axis() {
        let dev = "gilrs:switch_pro:0";
        let remap_uid = 1usize;
        let mut remap = empty_node(remap_uid, "module.remapper");
        remap.params.insert("_automap_device_id".into(), Value::String(dev.into()));
        // Gate button + LS in all 4 directions → swipe X + swipe Y.
        remap.params.insert("mappings".into(), serde_json::json!([
            { "in": ["btn_lb", "left_stick_left", "left_stick_right",
                     "left_stick_up", "left_stick_down"],
              "out": ["touch_swipe_x", "touch_swipe_y"], "mode": "analog" }
        ]));
        let src = source_node(3, dev, 0.0);
        let sink = sink_node(2, "virtual.xinput:0", &format!("remap:{remap_uid}"), false);
        let graph = ProcessingGraph { nodes: vec![src, remap, sink] };
        let mut state = HashMap::new();
        let mut out = TickOutput::default();
        let getf = |out: &TickOutput, pin: &str| out.sink_outputs
            .get(&("virtual.xinput:0".to_string(), pin.to_string())).map(|s| s.as_float()).unwrap_or(0.0);
        let getb = |out: &TickOutput, pin: &str| out.sink_outputs
            .get(&("virtual.xinput:0".to_string(), pin.to_string())).map(|s| s.as_bool()).unwrap_or(false);

        // Button up: combo inactive → button passes through normally.
        let mut dev_sigs = HashMap::new();
        dev_sigs.insert((dev.to_string(), "btn_lb".to_string()), Signal::Bool(false));
        eval_graph_tick(&graph, &mut state, &dev_sigs, 0.016, &mut out);
        assert!(!getb(&out, "btn_lb"), "button up → nothing to pass through");

        // Button held + stick deflected (one direction, combo active): the finger
        // is down AND the gate button is suppressed from pass-through, even though
        // the opposite cardinal of the same axis is also in the combo.
        dev_sigs.insert((dev.to_string(), "btn_lb".to_string()), Signal::Bool(true));
        dev_sigs.insert((dev.to_string(), "left_stick_x".to_string()), Signal::Float(0.6));
        eval_graph_tick(&graph, &mut state, &dev_sigs, 0.016, &mut out);
        assert!(getb(&out, "touch1_active"), "combo active → finger down");
        assert!((getf(&out, "touch1_x") - 0.6).abs() < 0.05, "stick drives swipe x");
        assert!(!getb(&out, "btn_lb"),
            "active touch combo must suppress its gate button (was leaking with multi-axis)");

        // Button held, stick centered: finger down at center, button still consumed.
        dev_sigs.insert((dev.to_string(), "left_stick_x".to_string()), Signal::Float(0.0));
        eval_graph_tick(&graph, &mut state, &dev_sigs, 0.016, &mut out);
        assert!(getb(&out, "touch1_active"), "button held → finger active even centered");
        assert!(!getb(&out, "btn_lb"), "gate button stays suppressed while combo held");

        // Button released → combo inactive → finger up, button no longer consumed.
        dev_sigs.insert((dev.to_string(), "btn_lb".to_string()), Signal::Bool(false));
        eval_graph_tick(&graph, &mut state, &dev_sigs, 0.016, &mut out);
        assert!(!getb(&out, "touch1_active"), "button released → finger up");
    }

    // The DualSense mic button is a canonical pin: a normal button→btn_mute map
    // reaches the sink with no special handling.
    #[test]
    fn remapper_mic_button_reaches_sink() {
        let dev = "gilrs:switch_pro:0";
        let remap_uid = 1usize;
        let mut remap = empty_node(remap_uid, "module.remapper");
        remap.params.insert("_automap_device_id".into(), Value::String(dev.into()));
        remap.params.insert("mappings".into(), serde_json::json!([
            { "in": ["btn_south"], "out": ["btn_mute"] }
        ]));
        let sink = sink_node(2, "virtual.xinput:0", &format!("remap:{remap_uid}"), false);
        let graph = ProcessingGraph { nodes: vec![remap, sink] };
        let mut state = HashMap::new();
        let mut out = TickOutput::default();
        let mut dev_sigs = HashMap::new();
        dev_sigs.insert((dev.to_string(), "btn_south".to_string()), Signal::Bool(true));
        eval_graph_tick(&graph, &mut state, &dev_sigs, 0.016, &mut out);
        assert!(out.sink_outputs.get(&("virtual.xinput:0".to_string(), "btn_mute".to_string()))
            .map(|s| s.as_bool()).unwrap_or(false), "btn_south→btn_mute must reach the sink");
    }

    // The implicit digital→analog bridge must RELEASE: pressing then releasing
    // the Switch digital ZR should drive the virtual analog RT to 1.0 then back
    // to 0.0 (regression guard for the "stuck at full press" bug).
    #[test]
    fn digital_bridge_presses_and_releases() {
        // Direct device → sink (no remapper); src_dev is the physical device.
        let sink = sink_node(1, "virtual.xinput:0", "gilrs:switch_pro:0", true);
        let graph = ProcessingGraph { nodes: vec![sink] };
        let mut state = HashMap::new();
        let mut out = TickOutput::default();

        // ZR pressed.
        let mut dev = HashMap::new();
        dev.insert(("gilrs:switch_pro:0".to_string(), "btn_rt_dig".to_string()), Signal::Bool(true));
        eval_graph_tick(&graph, &mut state, &dev, 0.016, &mut out);
        let v = out.sink_outputs.get(&("virtual.xinput:0".to_string(), "right_trigger".to_string()))
            .map(|s| s.as_float()).unwrap_or(-1.0);
        assert!((v - 1.0).abs() < 0.01, "pressed ZR should give full RT, got {v}");

        // ZR released → must go back to 0, not latch.
        dev.insert(("gilrs:switch_pro:0".to_string(), "btn_rt_dig".to_string()), Signal::Bool(false));
        eval_graph_tick(&graph, &mut state, &dev, 0.016, &mut out);
        let v = out.sink_outputs.get(&("virtual.xinput:0".to_string(), "right_trigger".to_string()))
            .map(|s| s.as_float()).unwrap_or(-1.0);
        assert!(v.abs() < 0.01, "released ZR should release RT to 0, got {v}");
    }

    // With the bridge DISABLED (analog-capable pad, toggle off), the digital
    // button must NOT leak into the analog trigger.
    #[test]
    fn digital_bridge_disabled_does_not_leak() {
        let sink = sink_node(1, "virtual.xinput:0", "gilrs:xinput:0", false);
        let graph = ProcessingGraph { nodes: vec![sink] };
        let mut state = HashMap::new();
        let mut out = TickOutput::default();

        let mut dev = HashMap::new();
        dev.insert(("gilrs:xinput:0".to_string(), "btn_rt_dig".to_string()), Signal::Bool(true));
        eval_graph_tick(&graph, &mut state, &dev, 0.016, &mut out);
        let leaked = out.sink_outputs.get(&("virtual.xinput:0".to_string(), "right_trigger".to_string()));
        assert!(leaked.is_none(), "bridge off: digital button must not drive analog trigger, got {leaked:?}");
    }

    // "Digital triggers" opt-in on an analog-capable pad: the calibrated analog
    // trigger must SNAP to full/zero at the Calibration threshold (not pass
    // through continuously), and the digital LT/RT buttons must be re-derived
    // from that SAME threshold rather than the pad's early-firing L2/R2 flag.
    #[test]
    fn digital_triggers_snap_analog_and_rederive_button() {
        let dev_id = "gilrs:dualsense:0";
        let mut src = empty_node(1, "device.source");
        src.device_id = Some(dev_id.to_string());
        src.params.insert("digital_triggers".into(), Value::Bool(true));
        src.params.insert("ltrig_digital_threshold".into(), Value::from(0.5));
        let graph = ProcessingGraph { nodes: vec![src] };

        // Below threshold: analog snaps to 0, digital button ignores the pad's
        // early-fired flag and stays off.
        let mut dev = HashMap::new();
        dev.insert((dev_id.to_string(), "left_trigger".to_string()), Signal::Float(0.3));
        dev.insert((dev_id.to_string(), "btn_lt_dig".to_string()),  Signal::Bool(true));
        let out = preprocess_dev_sigs(&graph, &dev);
        assert_eq!(out.get(&(dev_id.to_string(), "left_trigger".to_string())).map(|s| s.as_float()),
            Some(0.0), "below threshold must snap analog trigger to 0");
        assert_eq!(out.get(&(dev_id.to_string(), "btn_lt_dig".to_string())).map(|s| s.as_bool()),
            Some(false), "digital button must follow the calibration threshold, not the pad flag");

        // Above threshold: analog snaps to full (staying Float), button on.
        dev.insert((dev_id.to_string(), "left_trigger".to_string()), Signal::Float(0.7));
        dev.insert((dev_id.to_string(), "btn_lt_dig".to_string()),  Signal::Bool(false));
        let out = preprocess_dev_sigs(&graph, &dev);
        assert_eq!(out.get(&(dev_id.to_string(), "left_trigger".to_string())).copied(),
            Some(Signal::Float(1.0)), "above threshold must snap analog trigger to full Float(1.0)");
        assert_eq!(out.get(&(dev_id.to_string(), "btn_lt_dig".to_string())).map(|s| s.as_bool()),
            Some(true), "above threshold must fire the digital button");
    }

    // With "Digital triggers" OFF the analog trigger passes through unchanged —
    // no thresholding, full continuous travel.
    #[test]
    fn digital_triggers_off_passes_analog_through() {
        let dev_id = "gilrs:dualsense:0";
        let mut src = empty_node(1, "device.source");
        src.device_id = Some(dev_id.to_string());
        // digital_triggers absent → defaults to off.
        let graph = ProcessingGraph { nodes: vec![src] };

        let mut dev = HashMap::new();
        dev.insert((dev_id.to_string(), "left_trigger".to_string()), Signal::Float(0.3));
        let out = preprocess_dev_sigs(&graph, &dev);
        let v = out.get(&(dev_id.to_string(), "left_trigger".to_string())).map(|s| s.as_float()).unwrap_or(-1.0);
        assert!((v - 0.3).abs() < 1e-4, "digital triggers off must pass analog through, got {v}");
    }

    /// Build a `device.source` node carrying a `deadzone` param so
    /// `preprocess_dev_sigs` picks it up for the named device.
    fn source_node(uid: usize, device_id: &str, deadzone: f32) -> NodeSnap {
        let mut n = empty_node(uid, "device.source");
        n.device_id = Some(device_id.to_string());
        n.params.insert("deadzone".into(), Value::from(deadzone as f64));
        n
    }

    // A direct AutoMap wire (device.source → sink) must apply the source
    // node's stick deadzone. A small stick value inside the deadzone must
    // collapse to 0 at the sink; a value past it must pass through (rescaled).
    #[test]
    fn automap_stick_respects_source_deadzone() {
        let src = source_node(1, "gilrs:xinput:0", 0.2);
        let sink = sink_node(2, "virtual.xinput:0", "gilrs:xinput:0", false);
        let graph = ProcessingGraph { nodes: vec![src, sink] };
        let mut state = HashMap::new();
        let mut out = TickOutput::default();

        // Stick nudged to 0.1 — inside the 0.2 deadzone → sink must read 0.
        let mut dev = HashMap::new();
        dev.insert(("gilrs:xinput:0".to_string(), "left_stick_x".to_string()), Signal::Float(0.1));
        eval_graph_tick(&graph, &mut state, &dev, 0.016, &mut out);
        let x = out.sink_outputs.get(&("virtual.xinput:0".to_string(), "left_stick_x".to_string()))
            .map(|s| s.as_float());
        let lstick_x = out.sink_outputs.get(&("virtual.xinput:0".to_string(), "left_stick".to_string()))
            .and_then(|s| if let Signal::Vec2(v) = s { Some(v.x) } else { None });
        let v = x.or(lstick_x).unwrap_or(0.0);
        assert!(v.abs() < 1e-4, "stick inside deadzone must collapse to 0 at sink, got {v}");

        // Stick pushed to 0.6 — past the deadzone → passes through (rescaled).
        dev.insert(("gilrs:xinput:0".to_string(), "left_stick_x".to_string()), Signal::Float(0.6));
        eval_graph_tick(&graph, &mut state, &dev, 0.016, &mut out);
        let x = out.sink_outputs.get(&("virtual.xinput:0".to_string(), "left_stick_x".to_string()))
            .map(|s| s.as_float());
        let lstick_x = out.sink_outputs.get(&("virtual.xinput:0".to_string(), "left_stick".to_string()))
            .and_then(|s| if let Signal::Vec2(v) = s { Some(v.x) } else { None });
        let v = x.or(lstick_x).unwrap_or(0.0);
        assert!(v > 0.01, "stick past deadzone must reach sink, got {v}");
    }

    /// device.source → Remapper (analog stick-cardinal → key) → keymouse sink.
    /// Reproduces the user-reported case: WASD via analog-mode stick mapping.
    fn keymouse_sink_from_remap(uid: usize, remap_uid: usize) -> NodeSnap {
        let mut n = empty_node(uid, "device.sink");
        n.sink_target = Some(SinkTarget {
            device_id: "virtual.keymouse:0".to_string(),
            pin_ids: canonical_pins(),
            multi_sources: vec![Vec::new(); canonical_pins().len()],
            automap_source: Some((format!("remap:{remap_uid}"), canonical_pins())),
            automap_fallback_dev: None,
            feedback_sources: Vec::new(),
            is_self_sink: false,
            digital_trigger_bridge: false,
        });
        n
    }

    // End-to-end: a zone mapped touch→mouse_left must drive the keymouse sink's
    // mouse_left pin (regression guard for "touch/click → mouse button does
    // nothing"). Exercises the full graph tick: tz node → touchmap bus → sink.
    #[test]
    fn touch_zone_button_reaches_keymouse_sink() {
        let dev = "pad";
        let tz_uid = 2usize;
        let mut tz = empty_node(tz_uid, "module.touch_zones");
        tz.params.insert("zone_mode".into(), Value::String("mapping".into()));
        tz.params.insert("_automap_device_id".into(), Value::String(dev.into()));
        tz.params.insert("col_edges".into(), serde_json::json!([]));
        tz.params.insert("row_edges".into(), serde_json::json!([]));
        tz.params.insert("zone_maps".into(), serde_json::json!([
            {"f":0,"z":0,"in":["tz_touch"],"out":["mouse_left"],"mode":"down"},
        ]));
        let mut sink = empty_node(3, "device.sink");
        sink.sink_target = Some(SinkTarget {
            device_id: "virtual.keymouse:0".to_string(),
            pin_ids: canonical_pins(),
            multi_sources: vec![Vec::new(); canonical_pins().len()],
            automap_source: Some((format!("touchmap:{tz_uid}"), canonical_pins())),
            automap_fallback_dev: None,
            feedback_sources: Vec::new(),
            is_self_sink: false,
            digital_trigger_bridge: false,
        });
        let graph = ProcessingGraph { nodes: vec![tz, sink] };
        let mut state = HashMap::new();
        let mut out = TickOutput::default();
        let mut sigs = HashMap::new();
        sigs.insert((dev.to_string(), "touch1_active".to_string()), Signal::Bool(true));
        sigs.insert((dev.to_string(), "touch1_x".to_string()), Signal::Float(0.0));
        sigs.insert((dev.to_string(), "touch1_y".to_string()), Signal::Float(0.0));
        eval_graph_tick(&graph, &mut state, &sigs, 0.016, &mut out);
        let lmb = out.sink_outputs.get(&("virtual.keymouse:0".to_string(), "mouse_left".to_string()))
            .map(|s| s.as_bool()).unwrap_or(false);
        assert!(lmb, "touch→mouse_left must reach the keymouse sink");
    }

    #[test]
    fn analog_stick_to_key_respects_source_deadzone() {
        let dev = "gilrs:xinput:0";
        let remap_uid = 2usize;
        let src = source_node(1, dev, 0.3); // 0.3 deadzone on the device.
        let mut remap = empty_node(remap_uid, "module.remapper");
        remap.params.insert("_automap_device_id".into(), Value::String(dev.into()));
        remap.params.insert("mappings".into(), serde_json::json!([
            { "in": ["left_stick_up"], "out": ["key_w"], "mode": "analog" }
        ]));
        let sink = keymouse_sink_from_remap(3, remap_uid);
        let graph = ProcessingGraph { nodes: vec![src, remap, sink] };
        let mut state = HashMap::new();
        let mut out = TickOutput::default();

        // Stick pushed UP to 0.15 — INSIDE the 0.3 deadzone. key_w must NOT fire.
        let mut sigs = HashMap::new();
        sigs.insert((dev.to_string(), "left_stick_y".to_string()), Signal::Float(0.15));
        eval_graph_tick(&graph, &mut state, &sigs, 0.016, &mut out);
        let w = out.sink_outputs.get(&("virtual.keymouse:0".to_string(), "key_w".to_string()))
            .map(|s| s.as_bool()).unwrap_or(false);
        assert!(!w, "stick inside deadzone must NOT fire key_w, but it did");

        // Stick pushed UP to 0.8 — past the deadzone. key_w must fire.
        sigs.insert((dev.to_string(), "left_stick_y".to_string()), Signal::Float(0.8));
        eval_graph_tick(&graph, &mut state, &sigs, 0.016, &mut out);
        let w = out.sink_outputs.get(&("virtual.keymouse:0".to_string(), "key_w".to_string()))
            .map(|s| s.as_bool()).unwrap_or(false);
        assert!(w, "stick past deadzone must fire key_w, but it didn't");
    }

    /// Even when no device.source node carries the deadzone (the device feeds
    /// AutoMap consumers like the Remapper without its source node present in
    /// the graph), stick pins must still get the default deadzone rather than
    /// passing through raw. Regression guard for the "analog stick→key ignores
    /// deadzone" report.
    #[test]
    fn analog_stick_to_key_default_deadzone_without_source_node() {
        let dev = "gilrs:xinput:0";
        let remap_uid = 2usize;
        // No source_node: only remapper + sink. The default deadzone must
        // still apply (DEFAULT_STICK_DEADZONE), not 0.
        let mut remap = empty_node(remap_uid, "module.remapper");
        remap.params.insert("_automap_device_id".into(), Value::String(dev.into()));
        remap.params.insert("mappings".into(), serde_json::json!([
            { "in": ["left_stick_up"], "out": ["key_w"], "mode": "analog" }
        ]));
        let sink = keymouse_sink_from_remap(3, remap_uid);
        let graph = ProcessingGraph { nodes: vec![remap, sink] };
        let mut state = HashMap::new();
        let mut out = TickOutput::default();

        // 0.05 is below the default deadzone → key_w must NOT fire.
        let mut sigs = HashMap::new();
        sigs.insert((dev.to_string(), "left_stick_y".to_string()), Signal::Float(0.05));
        eval_graph_tick(&graph, &mut state, &sigs, 0.016, &mut out);
        let w = out.sink_outputs.get(&("virtual.keymouse:0".to_string(), "key_w".to_string()))
            .map(|s| s.as_bool()).unwrap_or(false);
        assert!(!w, "small stick (below default deadzone) must NOT fire key_w");
    }

    /// Run the graph for `ticks` frames at `dt`, holding `stick_y`, and return
    /// (on_count, edge_count) for the keymouse `out_pin`. Edges count rising
    /// transitions so we can tell a tap train apart from a steady gate.
    fn count_pulses(
        dev: &str, remap_uid: usize, out_pin: &str, mode_extra: serde_json::Value,
        stick_y: f32, ticks: usize, dt: f32,
    ) -> (usize, usize) {
        let src = source_node(1, dev, 0.0); // zero deadzone: measure modulation only.
        let mut remap = empty_node(remap_uid, "module.remapper");
        remap.params.insert("_automap_device_id".into(), Value::String(dev.into()));
        let mut mapping = serde_json::json!(
            { "in": ["left_stick_up"], "out": [out_pin], "mode": "analog" }
        );
        if let Some(obj) = mapping.as_object_mut() {
            if let Some(extra) = mode_extra.as_object() {
                for (k, v) in extra { obj.insert(k.clone(), v.clone()); }
            }
        }
        remap.params.insert("mappings".into(), serde_json::Value::Array(vec![mapping]));
        let sink = keymouse_sink_from_remap(3, remap_uid);
        let graph = ProcessingGraph { nodes: vec![src, remap, sink] };
        let mut state = HashMap::new();
        let mut out = TickOutput::default();
        let mut sigs = HashMap::new();
        sigs.insert((dev.to_string(), "left_stick_y".to_string()), Signal::Float(stick_y));

        let mut on = 0usize;
        let mut edges = 0usize;
        let mut prev = false;
        for _ in 0..ticks {
            eval_graph_tick(&graph, &mut state, &sigs, dt, &mut out);
            let w = out.sink_outputs.get(&("virtual.keymouse:0".to_string(), out_pin.to_string()))
                .map(|s| s.as_bool()).unwrap_or(false);
            if w { on += 1; }
            if w && !prev { edges += 1; }
            prev = w;
        }
        (on, edges)
    }

    // Plain analog → digital must produce a TAP TRAIN, and a harder push must
    // tap MORE often than a light push (frequency tracks amplitude).
    #[test]
    fn analog_digital_tap_train_frequency_tracks_amplitude() {
        let extra = serde_json::json!({ "window_ms": 30.0 });
        // 1 second at 1ms ticks for clean frequency counting.
        let (_, edges_light) = count_pulses("gilrs:xinput:0", 2, "key_w", extra.clone(), 0.3, 1000, 0.001);
        let (_, edges_hard)  = count_pulses("gilrs:xinput:0", 2, "key_w", extra, 1.0, 1000, 0.001);
        assert!(edges_light >= 2, "light push should still tap a few times, got {edges_light}");
        assert!(edges_hard > edges_light,
            "harder push must tap more often: hard={edges_hard} light={edges_light}");
    }

    // Hold mode → PWM: duty cycle (ON fraction) must track amplitude. A
    // light push has a low duty; full deflection is (near) always on.
    #[test]
    fn analog_digital_hold_pwm_duty_tracks_amplitude() {
        let extra = serde_json::json!({ "window_ms": 40.0, "sustain": true });
        let (on_light, _) = count_pulses("gilrs:xinput:0", 2, "key_w", extra.clone(), 0.25, 1000, 0.001);
        let (on_full, _)  = count_pulses("gilrs:xinput:0", 2, "key_w", extra, 1.0, 1000, 0.001);
        let duty_light = on_light as f32 / 1000.0;
        let duty_full  = on_full as f32 / 1000.0;
        assert!(duty_light > 0.05 && duty_light < 0.6,
            "light Hold duty should be low-ish, got {duty_light}");
        assert!(duty_full > 0.9, "full deflection Hold should be near-always-on, got {duty_full}");
    }

    // Turbo (no Hold) doubles the max frequency, so at full deflection it
    // taps more often than plain analog at the same window_ms.
    #[test]
    fn analog_digital_turbo_doubles_frequency() {
        let plain = serde_json::json!({ "window_ms": 30.0 });
        let turbo = serde_json::json!({ "window_ms": 30.0, "turbo": true });
        let (_, edges_plain) = count_pulses("gilrs:xinput:0", 2, "key_w", plain, 1.0, 1000, 0.001);
        let (_, edges_turbo) = count_pulses("gilrs:xinput:0", 2, "key_w", turbo, 1.0, 1000, 0.001);
        assert!(edges_turbo > edges_plain,
            "turbo must tap faster at full deflection: turbo={edges_turbo} plain={edges_plain}");
    }

    // Unit-level coverage of the shared analog→digital modulator.
    #[test]
    fn analog_digital_pulse_unit() {
        let dt = 0.001;
        let run = |mag: f32, window_ms: f32, sustain: bool, turbo: bool| -> (usize, usize) {
            let mut slots = [0.0f32; PRESS_SLOTS_PER_MAPPING];
            let mut on = 0usize;
            let mut edges = 0usize;
            let mut prev = false;
            for _ in 0..1000 {
                let v = analog_digital_pulse(mag, window_ms, sustain, turbo, &mut slots, dt);
                if v { on += 1; }
                if v && !prev { edges += 1; }
                prev = v;
            }
            (on, edges)
        };

        // Zero magnitude → never on.
        assert_eq!(run(0.0, 30.0, false, false).0, 0, "mag 0 must be silent");

        // Plain tap train: more deflection → more taps.
        let (_, e_light) = run(0.3, 30.0, false, false);
        let (_, e_hard)  = run(1.0, 30.0, false, false);
        assert!(e_hard > e_light, "freq must rise with mag: {e_hard} > {e_light}");

        // Regression: at the REALISTIC default window_ms (200ms) the plain
        // tap train must be ~50% duty (a clean tap), NOT a near-held key.
        // The old tap_on=window_ms made this ~90% duty → felt held.
        let (on_default, edges_default) = run(1.0, 200.0, false, false);
        let duty = on_default as f32 / 1000.0;
        assert!(duty > 0.35 && duty < 0.65,
            "plain tap at default window must be ~50% duty, got {duty} (held-key regression)");
        assert!(edges_default >= 4, "must actually tap multiple times in 1s, got {edges_default}");

        // Hold PWM: duty tracks magnitude; full → always on.
        let (on_q, _)    = run(0.25, 40.0, true, false);
        let (on_full, _) = run(1.0, 40.0, true, false);
        assert!(on_q > 0 && (on_q as f32 / 1000.0) < 0.6, "quarter duty should be low, got {on_q}/1000");
        assert!(on_full as f32 / 1000.0 > 0.9, "full Hold should be near always-on, got {on_full}/1000");

        // Turbo doubles frequency at full deflection.
        let (_, e_plain) = run(1.0, 30.0, false, false);
        let (_, e_turbo) = run(1.0, 30.0, false, true);
        assert!(e_turbo > e_plain, "turbo faster: {e_turbo} > {e_plain}");
    }

    // on_press / on_release now honor `window_ms` as the emitted trigger
    // duration (floored at the 10ms minimum pulse).
    #[test]
    fn on_press_release_trigger_duration_tracks_window_ms() {
        let dt = 0.001; // 1 ms/tick

        // Drive a press: hold for `hold_ticks`, then release; count how many
        // ticks the output stays ON after the relevant edge.
        let run_on_press = |window_ms: f32| -> usize {
            let mut slots = [0.0f32; PRESS_SLOTS_PER_MAPPING];
            let mut on = 0usize;
            // rising edge at tick 0; hold a few ticks then release.
            for t in 0..1000 {
                let raw = t < 5; // pressed for 5 ms
                if apply_press_mode(raw, PressMode::OnPress, window_ms, false, &mut slots, dt) {
                    on += 1;
                }
            }
            on
        };

        // ~50 ms window → ~50 on-ticks (within tolerance for the dt countdown).
        let n50 = run_on_press(50.0);
        assert!((45..=55).contains(&n50), "50ms on_press should stay ~50 ticks, got {n50}");
        // ~200 ms window → ~200 on-ticks.
        let n200 = run_on_press(200.0);
        assert!((190..=210).contains(&n200), "200ms on_press should stay ~200 ticks, got {n200}");
        // Longer window → strictly longer trigger.
        assert!(n200 > n50, "larger window_ms must lengthen the trigger");

        // Floor: a 0 ms window still emits at least the 10ms minimum pulse.
        let n0 = run_on_press(0.0);
        assert!(n0 >= 9 && n0 <= 12, "0ms window floors to ~10ms pulse, got {n0}");

        // on_release fires on the falling edge with the same duration rule.
        let run_on_release = |window_ms: f32| -> usize {
            let mut slots = [0.0f32; PRESS_SLOTS_PER_MAPPING];
            let mut on = 0usize;
            for t in 0..1000 {
                let raw = t < 5; // release happens at tick 5
                if apply_press_mode(raw, PressMode::OnRelease, window_ms, false, &mut slots, dt) {
                    on += 1;
                }
            }
            on
        };
        let r100 = run_on_release(100.0);
        assert!((95..=105).contains(&r100), "100ms on_release should stay ~100 ticks, got {r100}");
    }

    // Processing wired BEFORE a Collector (explicit input port) must be what
    // the downstream Remapper sees — not the raw device sample. Here a
    // `module.constant` stands in for a Response Curve that re-maps the stick
    // amplitude: the device pushes left_stick_y small (raw), but the constant
    // feeds left_stick_y = 0.9 into the collector port. The Remapper's analog
    // stick→key mapping must therefore see ~0.9, firing key_w.
    #[test]
    fn processing_through_collector_drives_remapper_amplitude() {
        let dev = "gilrs:xinput:0";
        let src = source_node(1, dev, 0.0);

        // Constant node → emulates Response Curve output (Float 0.9).
        let mut konst = empty_node(2, "module.constant");
        konst.n_outputs = 1;
        konst.params.insert("value".into(), Value::from(0.9_f64));

        // Collector: AutoMap bus (input 0, from device) + explicit port for
        // left_stick_y (input 1, from the constant). _collect_pin_ids[0] names
        // that port's pin.
        let mut collect = empty_node(3, "module.automap_collect");
        collect.params.insert("_automap_device_id".into(), Value::String(dev.into()));
        collect.params.insert("_collect_pin_ids".into(),
            Value::Array(vec![Value::String("left_stick_y".into())]));
        // input_sources: [0]=bus (device.source idx 0 out 0), [1]=constant out 0.
        collect.input_sources = vec![Some((0, 0)), Some((1, 0))];

        // Remapper reads the collector, maps left_stick_up (analog) → key_w.
        let remap_uid = 4usize;
        let mut remap = empty_node(remap_uid, "module.remapper");
        remap.params.insert("_automap_collector_id".into(),
            Value::String("collector:3".into()));
        remap.params.insert("mappings".into(), serde_json::json!([
            { "in": ["left_stick_up"], "out": ["key_w"], "mode": "analog", "window_ms": 30.0 }
        ]));
        let sink = keymouse_sink_from_remap(5, remap_uid);

        let graph = ProcessingGraph { nodes: vec![src, konst, collect, remap, sink] };
        let mut state = HashMap::new();
        let mut out = TickOutput::default();

        // Device stick is near-neutral (0.05) — raw would not fire. The
        // collector override (0.9) should drive the Remapper instead.
        let mut sigs = HashMap::new();
        sigs.insert((dev.to_string(), "left_stick_y".to_string()), Signal::Float(0.05));

        // Run several ticks; key_w should tap at least once (proves the
        // processed 0.9 amplitude reached the Remapper, not the raw 0.05).
        let mut fired = false;
        for _ in 0..200 {
            eval_graph_tick(&graph, &mut state, &sigs, 0.001, &mut out);
            if out.sink_outputs.get(&("virtual.keymouse:0".to_string(), "key_w".to_string()))
                .map(|s| s.as_bool()).unwrap_or(false)
            {
                fired = true; break;
            }
        }
        assert!(fired, "processed amplitude through Collector must drive the Remapper (key_w never fired)");
    }

    // Combiner hierarchy: a pin a Remapper CONSUMED (mapped away) must not leak
    // through a lower-priority raw-device port under a non-ADD policy, but ADD
    // explicitly opts back into mixing.
    fn combiner_node(
        uid: usize, remap_uid: usize, raw_dev: &str, policy: &str,
    ) -> NodeSnap {
        let mut n = empty_node(uid, "module.automap_combiner");
        // Port 0 = Remapper collector; Port 1 = raw device.
        n.params.insert("_automap_input_devs".into(), Value::Array(vec![
            Value::String(String::new()),
            Value::String(raw_dev.into()),
        ]));
        n.params.insert("_automap_input_collectors".into(), Value::Array(vec![
            Value::String(format!("remap:{remap_uid}")),
            Value::String(String::new()),
        ]));
        let mut policy_obj = serde_json::Map::new();
        policy_obj.insert("btn_south".into(), Value::String(policy.into()));
        n.params.insert("combiner_pin_policy".into(), Value::Object(policy_obj));
        n.input_sources = vec![Some((0, 0)), Some((1, 0))]; // shape only
        n
    }

    fn run_combiner_leak(policy: &str) -> bool {
        let dev = "gilrs:xinput:0";
        let src = source_node(1, dev, 0.0);
        // Remapper consumes btn_south (maps it to btn_west), so btn_south is
        // claimed and should be suppressed downstream.
        let remap_uid = 2usize;
        let mut remap = empty_node(remap_uid, "module.remapper");
        remap.params.insert("_automap_device_id".into(), Value::String(dev.into()));
        remap.params.insert("mappings".into(), serde_json::json!([
            { "in": ["btn_south"], "out": ["btn_west"], "mode": "down" }
        ]));
        let combiner = combiner_node(3, remap_uid, dev, policy);
        // Sink auto-maps FROM the combiner.
        let sink = sink_node(4, "virtual.xinput:0", "combiner:3", false);
        let graph = ProcessingGraph { nodes: vec![src, remap, combiner, sink] };
        let mut state = HashMap::new();
        let mut out = TickOutput::default();

        // Physical btn_south held.
        let mut sigs = HashMap::new();
        sigs.insert((dev.to_string(), "btn_south".to_string()), Signal::Bool(true));
        eval_graph_tick(&graph, &mut state, &sigs, 0.016, &mut out);
        // Did btn_south leak to the sink?
        out.sink_outputs.get(&("virtual.xinput:0".to_string(), "btn_south".to_string()))
            .map(|s| s.as_bool()).unwrap_or(false)
    }

    #[test]
    fn combiner_suppresses_consumed_pin_unless_add() {
        // SORT (default) and OR must NOT leak the consumed btn_south.
        assert!(!run_combiner_leak("SORT"), "SORT must suppress consumed btn_south");
        assert!(!run_combiner_leak("OR"),   "OR must suppress consumed btn_south");
        // ADD explicitly mixes → the raw-port btn_south is allowed through.
        assert!(run_combiner_leak("ADD"), "ADD must let the raw btn_south mix through");
    }

    // Per-PORT default policy: setting the raw port's default to ADD opts that
    // port back into mixing for ALL its pins (no per-pin override needed), so a
    // consumed pin leaks through exactly as an explicit per-pin ADD would.
    #[test]
    fn combiner_per_port_default_add_opts_into_mixing() {
        let dev = "gilrs:xinput:0";
        let src = source_node(1, dev, 0.0);
        let remap_uid = 2usize;
        let mut remap = empty_node(remap_uid, "module.remapper");
        remap.params.insert("_automap_device_id".into(), Value::String(dev.into()));
        remap.params.insert("mappings".into(), serde_json::json!([
            { "in": ["btn_south"], "out": ["btn_west"], "mode": "down" }
        ]));
        // Combiner with NO per-pin policy, but port 1 (raw) default = ADD.
        let mut combiner = empty_node(3, "module.automap_combiner");
        combiner.params.insert("_automap_input_devs".into(), Value::Array(vec![
            Value::String(String::new()), Value::String(dev.into()),
        ]));
        combiner.params.insert("_automap_input_collectors".into(), Value::Array(vec![
            Value::String(format!("remap:{remap_uid}")), Value::String(String::new()),
        ]));
        let mut port_def = serde_json::Map::new();
        port_def.insert("1".into(), Value::String("ADD".into()));
        combiner.params.insert("combiner_port_default".into(), Value::Object(port_def));
        combiner.input_sources = vec![Some((0, 0)), Some((1, 0))];

        let sink = sink_node(4, "virtual.xinput:0", "combiner:3", false);
        let graph = ProcessingGraph { nodes: vec![src, remap, combiner, sink] };
        let mut state = HashMap::new();
        let mut out = TickOutput::default();
        let mut sigs = HashMap::new();
        sigs.insert((dev.to_string(), "btn_south".to_string()), Signal::Bool(true));
        eval_graph_tick(&graph, &mut state, &sigs, 0.016, &mut out);

        let leaked = out.sink_outputs.get(&("virtual.xinput:0".to_string(), "btn_south".to_string()))
            .map(|s| s.as_bool()).unwrap_or(false);
        assert!(leaked, "port-default ADD must opt the raw port into mixing (btn_south should pass)");
    }

    // D-pad PER-SIDE suppression: mapping only `dpad_left` away must suppress
    // the left direction across ALL three representations (Bool, dpad_x
    // negative side, dpad Vec2 x-negative) — but leave `dpad_right`, the
    // positive X side, and the entire Y axis / up-down untouched.
    #[test]
    fn dpad_left_mapped_away_suppresses_only_left_side() {
        let dev = "gilrs:xinput:0";
        let src = source_node(1, dev, 0.0);
        let remap_uid = 2usize;
        let mut remap = empty_node(remap_uid, "module.remapper");
        remap.params.insert("_automap_device_id".into(), Value::String(dev.into()));
        remap.params.insert("mappings".into(), serde_json::json!([
            { "in": ["dpad_left"], "out": ["btn_south"], "mode": "down" }
        ]));
        let sink = sink_node(3, "virtual.xinput:0", &format!("remap:{remap_uid}"), false);
        let graph = ProcessingGraph { nodes: vec![src, remap, sink] };
        let mut state = HashMap::new();
        let mut out = TickOutput::default();

        // Physical D-pad: LEFT held (claimed) AND DOWN held (NOT claimed).
        let mut sigs = HashMap::new();
        sigs.insert((dev.to_string(), "dpad_left".to_string()), Signal::Bool(true));
        sigs.insert((dev.to_string(), "dpad_down".to_string()), Signal::Bool(true));
        sigs.insert((dev.to_string(), "dpad_x".to_string()),    Signal::Float(-1.0));
        sigs.insert((dev.to_string(), "dpad_y".to_string()),    Signal::Float(-1.0));
        sigs.insert((dev.to_string(), "dpad".to_string()),      Signal::Vec2(Vec2::new(-1.0, -1.0)));
        eval_graph_tick(&graph, &mut state, &sigs, 0.016, &mut out);

        let get_b = |p: &str| out.sink_outputs
            .get(&("virtual.xinput:0".to_string(), p.to_string())).map(|s| s.as_bool()).unwrap_or(false);
        let get_f = |p: &str| out.sink_outputs
            .get(&("virtual.xinput:0".to_string(), p.to_string())).map(|s| s.as_float()).unwrap_or(0.0);
        let dpad_vec = || out.sink_outputs
            .get(&("virtual.xinput:0".to_string(), "dpad".to_string()))
            .and_then(|s| if let Signal::Vec2(v) = s { Some(*v) } else { None }).unwrap_or(Vec2::ZERO);

        // Mapped target fires.
        assert!(get_b("btn_south"), "dpad_left→btn_south must fire");

        // The sink resolves Vec2-vs-axis conflicts by keeping ONE form, so read
        // the effective X/Y as (axis pin) OR (Vec2 component) — whichever the
        // sink kept.
        let eff_x = if out.sink_outputs.contains_key(&("virtual.xinput:0".to_string(), "dpad_x".to_string())) {
            get_f("dpad_x") } else { dpad_vec().x };
        let eff_y = if out.sink_outputs.contains_key(&("virtual.xinput:0".to_string(), "dpad_y".to_string())) {
            get_f("dpad_y") } else { dpad_vec().y };

        // LEFT is fully suppressed across all representations.
        assert!(!get_b("dpad_left"), "dpad_left Bool must be suppressed");
        assert!(eff_x >= -1e-4, "dpad left (x-negative) must be clamped, got {eff_x}");

        // DOWN (not claimed) must SURVIVE.
        assert!(get_b("dpad_down"), "unmapped dpad_down Bool must pass through");
        assert!((eff_y - (-1.0)).abs() < 1e-4, "dpad_y (down) must be untouched, got {eff_y}");
    }

    // Vec2-authoritative: when the device provides a strong `left_stick` Vec2
    // but near-zero axis floats, a Collector forwards both, and the Remapper
    // must derive its axes (and cardinals) from the Vec2 — so an analog
    // stick→key mapping fires. Guards the "processed whole-stick Vec2 before a
    // Collector doesn't reach the Remapper" gap.
    #[test]
    fn processed_vec2_on_collector_drives_remapper_axes() {
        let dev = "gilrs:xinput:0";
        let src = source_node(1, dev, 0.0);

        // Collector: pure AutoMap bus pass-through from the device (no explicit
        // ports). Phase-1 forwards left_stick Vec2 AND the axis floats.
        let mut collect = empty_node(3, "module.automap_collect");
        collect.params.insert("_automap_device_id".into(), Value::String(dev.into()));
        collect.input_sources = vec![Some((0, 0))];

        let remap_uid = 4usize;
        let mut remap = empty_node(remap_uid, "module.remapper");
        remap.params.insert("_automap_collector_id".into(), Value::String("collector:3".into()));
        remap.params.insert("mappings".into(), serde_json::json!([
            { "in": ["left_stick_up"], "out": ["key_w"], "mode": "analog", "window_ms": 30.0 }
        ]));
        let sink = keymouse_sink_from_remap(5, remap_uid);
        let graph = ProcessingGraph { nodes: vec![src, collect, remap, sink] };
        let mut state = HashMap::new();
        let mut out = TickOutput::default();

        // Axes near-zero, but the left_stick VEC2 pushed up (y=0.9).
        let mut sigs = HashMap::new();
        sigs.insert((dev.to_string(), "left_stick_x".to_string()), Signal::Float(0.0));
        sigs.insert((dev.to_string(), "left_stick_y".to_string()), Signal::Float(0.0));
        sigs.insert((dev.to_string(), "left_stick".to_string()), Signal::Vec2(Vec2::new(0.0, 0.9)));

        let mut fired = false;
        for _ in 0..200 {
            eval_graph_tick(&graph, &mut state, &sigs, 0.001, &mut out);
            if out.sink_outputs.get(&("virtual.keymouse:0".to_string(), "key_w".to_string()))
                .map(|s| s.as_bool()).unwrap_or(false) { fired = true; break; }
        }
        assert!(fired, "processed left_stick Vec2 must drive the axes the Remapper reads (key_w never fired)");
    }

    // A consumed input must stay suppressed for as long as it is HELD, even in
    // a press mode whose output gate is momentary (on-press fires a ~10ms pulse
    // then closes). Regression for "on-press mapping fires its output then leaks
    // the raw input while still held".
    #[test]
    fn consumed_input_suppressed_while_held_in_on_press_mode() {
        let dev = "gilrs:xinput:0";
        let src = source_node(1, dev, 0.0);
        let remap_uid = 2usize;
        let mut remap = empty_node(remap_uid, "module.remapper");
        remap.params.insert("_automap_device_id".into(), Value::String(dev.into()));
        remap.params.insert("mappings".into(), serde_json::json!([
            { "in": ["dpad_left"], "out": ["btn_west"], "mode": "on_press" }
        ]));
        let sink = sink_node(3, "virtual.xinput:0", &format!("remap:{remap_uid}"), false);
        let graph = ProcessingGraph { nodes: vec![src, remap, sink] };
        let mut state = HashMap::new();
        let mut out = TickOutput::default();

        // Hold D-pad LEFT across many frames (well past the on-press pulse).
        let mut sigs = HashMap::new();
        sigs.insert((dev.to_string(), "dpad_left".to_string()), Signal::Bool(true));
        sigs.insert((dev.to_string(), "dpad_x".to_string()),    Signal::Float(-1.0));
        sigs.insert((dev.to_string(), "dpad".to_string()),      Signal::Vec2(Vec2::new(-1.0, 0.0)));

        let mut leaked_after_pulse = false;
        for frame in 0..60 {
            eval_graph_tick(&graph, &mut state, &sigs, 0.016, &mut out);
            // After the first few frames (pulse over), dpad_left must NOT leak.
            if frame >= 10 {
                let dl = out.sink_outputs.get(&("virtual.xinput:0".to_string(), "dpad_left".to_string()))
                    .map(|s| s.as_bool()).unwrap_or(false);
                let dx = out.sink_outputs.get(&("virtual.xinput:0".to_string(), "dpad_x".to_string()))
                    .map(|s| s.as_float()).unwrap_or(0.0);
                let dvx = out.sink_outputs.get(&("virtual.xinput:0".to_string(), "dpad".to_string()))
                    .and_then(|s| if let Signal::Vec2(v) = s { Some(v.x) } else { None }).unwrap_or(0.0);
                // effective left value via whichever representation the sink kept
                let eff = if dl { -1.0 } else { dx.min(dvx) };
                if eff < -1e-4 { leaked_after_pulse = true; }
            }
        }
        assert!(!leaked_after_pulse, "held dpad_left leaked through after the on-press pulse ended");
    }

    // The self-map exception: a mapping that routes an input back to ITSELF must
    // NOT suppress it (deliberate pass-through), even alongside another mapping.
    #[test]
    fn self_mapped_input_is_not_suppressed() {
        let dev = "gilrs:xinput:0";
        let src = source_node(1, dev, 0.0);
        let remap_uid = 2usize;
        let mut remap = empty_node(remap_uid, "module.remapper");
        remap.params.insert("_automap_device_id".into(), Value::String(dev.into()));
        remap.params.insert("mappings".into(), serde_json::json!([
            { "in": ["dpad_left"], "out": ["btn_west"],  "mode": "on_press" },
            { "in": ["dpad_left"], "out": ["dpad_left"],  "mode": "down" }
        ]));
        let sink = sink_node(3, "virtual.xinput:0", &format!("remap:{remap_uid}"), false);
        let graph = ProcessingGraph { nodes: vec![src, remap, sink] };
        let mut state = HashMap::new();
        let mut out = TickOutput::default();

        let mut sigs = HashMap::new();
        sigs.insert((dev.to_string(), "dpad_left".to_string()), Signal::Bool(true));
        for _ in 0..20 { eval_graph_tick(&graph, &mut state, &sigs, 0.016, &mut out); }

        let dl = out.sink_outputs.get(&("virtual.xinput:0".to_string(), "dpad_left".to_string()))
            .map(|s| s.as_bool()).unwrap_or(false);
        assert!(dl, "self-mapped dpad_left must pass through (not be suppressed)");
    }
}

#[cfg(test)]
mod menu_eval_tests {
    use super::*;

    fn menu_snap(uid: usize) -> NodeSnap {
        let mut n = NodeSnap {
            node_uid: uid,
            module_id: "module.menu".to_string(),
            params: HashMap::new(),
            n_outputs: 3,
            input_sources: Vec::new(),
            device_id: None,
            output_pin_ids: vec![
                "automap_pass".to_string(), "menu_open".to_string(), "menu_hover".to_string(),
            ],
            aux_f32_override: None,
            sink_target: None,
            inline_subgraph: None,
        };
        n.params.insert("_automap_device_id".into(), Value::String("dev".into()));
        n.params.insert("menu_id".into(), Value::String("abcd1234".into()));
        // 2x2 grid: zone ids row-major (0 TL, 1 TR, 2 BL, 3 BR).
        n.params.insert("col_edges".into(), serde_json::json!([0.5]));
        n.params.insert("row_edges".into(), serde_json::json!([0.5]));
        n.params.insert("zone_mode".into(), Value::String("mapping".into()));
        n
    }

    fn dev_stick(x: f32, y: f32) -> HashMap<(String, String), Signal> {
        let mut m = HashMap::new();
        m.insert(("dev".to_string(), "left_stick".to_string()), Signal::Vec2(Vec2::new(x, y)));
        m
    }

    fn show(b: bool) -> Vec<Option<Signal>> {
        vec![None, Some(Signal::Bool(b)), None, None]
    }

    // Hold activation + release-select: wired Show opens, stick bottom-right
    // highlights zone 3 (hover is sticky once the stick returns to center),
    // releasing Show closes AND selects — the zone's card fires for the pulse.
    #[test]
    fn hold_open_stick_hover_release_selects() {
        let mut snap = menu_snap(1);
        snap.params.insert("zone_maps".into(), serde_json::json!([
            { "f": 0, "z": 3, "in": ["menu_sel"], "out": ["btn_south"] }
        ]));
        let mut c: HashMap<(String, String), Signal> = HashMap::new();
        let mut state = HashMap::new();

        let out = eval_menu_node(&snap, 1, &show(false), &dev_stick(0.0, 0.0), &mut c, &mut state, 0.016);
        assert_eq!(out[1], Some(Signal::Bool(false)));
        assert_eq!(out[2], Some(Signal::Float(-1.0)));

        // Open + point bottom-right (stick +x, -y => unit (0.9, 0.9) => zone 3).
        let out = eval_menu_node(&snap, 1, &show(true), &dev_stick(0.8, -0.8), &mut c, &mut state, 0.016);
        assert_eq!(out[1], Some(Signal::Bool(true)));
        assert_eq!(out[2], Some(Signal::Float(3.0)));
        // Trailing mirror slots: no selection yet, live pointer present.
        assert_eq!(out[out.len() - 2], None);
        assert!(matches!(out[out.len() - 1], Some(Signal::Vec2(_))));
        // Not selected yet — the card hasn't fired.
        assert!(c.get(&("menumap:1".to_string(), "btn_south".to_string()))
            .map(|s| !s.as_bool()).unwrap_or(true));
        // Suppression: the pointing stick is zeroed on the passthrough while open.
        assert_eq!(c.get(&("menumap:1".to_string(), "left_stick".to_string())).copied(),
            Some(Signal::Vec2(Vec2::ZERO)));

        // Stick back to center — hover STAYS on 3 (sticky).
        let out = eval_menu_node(&snap, 1, &show(true), &dev_stick(0.0, 0.0), &mut c, &mut state, 0.016);
        assert_eq!(out[2], Some(Signal::Float(3.0)));

        // Release Show: closes, selects zone 3, card fires during the pulse.
        let out = eval_menu_node(&snap, 1, &show(false), &dev_stick(0.0, 0.0), &mut c, &mut state, 0.016);
        assert_eq!(out[1], Some(Signal::Bool(false)));
        assert_eq!(out[2], Some(Signal::Float(-1.0)));
        assert_eq!(c.get(&("menumap:1".to_string(), "btn_south".to_string())).copied(),
            Some(Signal::Bool(true)));
        // Selection mirror: zone 3, seq 1 — persists for the overlay's linger.
        assert_eq!(out[out.len() - 2], Some(Signal::Vec2(Vec2::new(3.0, 1.0))));
        assert_eq!(out[out.len() - 1], None, "pointer mirror clears when closed");
    }

    // The menu republishes the FULL upstream bus under `menumap:{uid}` (like
    // Touch Zones' touchmap:), not a sparse override map — so the AutoMap output
    // port glows and downstream reads a complete, coherently-suppressed bus.
    #[test]
    fn passthrough_republishes_full_bus() {
        // Closed menu = pure passthrough: the stick passes straight through.
        let snap = menu_snap(8);
        let mut c: HashMap<(String, String), Signal> = HashMap::new();
        let mut state = HashMap::new();
        eval_menu_node(&snap, 8, &show(false), &dev_stick(0.6, -0.3), &mut c, &mut state, 0.016);
        assert_eq!(
            c.get(&("menumap:8".to_string(), "left_stick".to_string())).copied(),
            Some(Signal::Vec2(Vec2::new(0.6, -0.3))),
            "closed menu passes the stick through under menumap:"
        );

        // Open with suppression OFF: still passes through (suppression is opt-in).
        let mut snap2 = menu_snap(9);
        snap2.params.insert("suppress_while_open".into(), Value::Bool(false));
        let mut c2: HashMap<(String, String), Signal> = HashMap::new();
        let mut st2 = HashMap::new();
        eval_menu_node(&snap2, 9, &show(true), &dev_stick(0.6, -0.3), &mut c2, &mut st2, 0.016);
        assert_eq!(
            c2.get(&("menumap:9".to_string(), "left_stick".to_string())).copied(),
            Some(Signal::Vec2(Vec2::new(0.6, -0.3))),
            "suppression off → stick passes through even while open"
        );
    }

    // Suppression zeros the enabled pointer pins on the menu's OWN passthrough
    // AND publishes a SOURCE-BLOCK request keyed by the physical source device
    // (`__src_block__:{dev}`), drained into `dev_sigs` next tick so the input
    // reaches ONLY the menu's navigation — not a mouse mapping, another module,
    // or the pad.
    #[test]
    fn suppress_publishes_source_block() {
        let snap = menu_snap(11); // default: suppress = true, left-stick pointer
        let mut c: HashMap<(String, String), Signal> = HashMap::new();
        let mut state = HashMap::new();
        // Open (wired Show) while the left stick is deflected (active).
        eval_menu_node(&snap, 11, &show(true), &dev_stick(0.8, -0.8), &mut c, &mut state, 0.016);

        let sk = format!("{SRC_BLOCK_PREFIX}dev");
        // Zeroed on the menu's own passthrough bus …
        assert_eq!(
            c.get(&("menumap:11".to_string(), "left_stick".to_string())).copied(),
            Some(Signal::Vec2(Vec2::ZERO)),
        );
        // … and the active driver's pins are flagged on the source-block channel.
        for pin in ["left_stick", "left_stick_x", "left_stick_y"] {
            assert_eq!(
                c.get(&(sk.clone(), pin.to_string())).map(|s| s.as_bool()),
                Some(true),
                "{pin} must be requested for source-block while open",
            );
        }
        // A source that ISN'T an enabled driver is not blocked.
        assert!(c.get(&(sk.clone(), "right_stick".to_string())).is_none());

        // Partial mode: an IDLE enabled driver (stick inside deadzone) is NOT
        // blocked, so it still reaches the game.
        let mut c_idle: HashMap<(String, String), Signal> = HashMap::new();
        let mut st_idle = HashMap::new();
        eval_menu_node(&snap, 11, &show(true), &dev_stick(0.05, 0.0), &mut c_idle, &mut st_idle, 0.016);
        assert!(c_idle.get(&(sk.clone(), "left_stick".to_string())).is_none(),
            "partial mode must not block an idle driver");

        // Full mode: the enabled driver is blocked even when idle.
        let mut snap_full = menu_snap(14);
        snap_full.params.insert("suppress_mode".into(), Value::String("full".into()));
        let mut c_full: HashMap<(String, String), Signal> = HashMap::new();
        let mut st_full = HashMap::new();
        eval_menu_node(&snap_full, 14, &show(true), &dev_stick(0.0, 0.0), &mut c_full, &mut st_full, 0.016);
        assert_eq!(
            c_full.get(&(format!("{SRC_BLOCK_PREFIX}dev"), "left_stick".to_string())).map(|s| s.as_bool()),
            Some(true), "full mode blocks the enabled driver even when idle",
        );

        // Suppression OFF → no block published.
        let mut snap2 = menu_snap(12);
        snap2.params.insert("suppress_while_open".into(), Value::Bool(false));
        let mut c2: HashMap<(String, String), Signal> = HashMap::new();
        let mut st2 = HashMap::new();
        eval_menu_node(&snap2, 12, &show(true), &dev_stick(0.8, -0.8), &mut c2, &mut st2, 0.016);
        assert!(c2.get(&(sk, "left_stick".to_string())).is_none());
    }

    // Partial suppression must LATCH gyro as the active driver off the menu
    // CURSOR being out of the deadzone: the rotation rate drops to ~0 whenever
    // the user holds the cursor on a target, and a rate-only flag would unblock
    // the source for those ticks — leaking gyro to e.g. a mouse mapping one
    // tick at a time while the menu is being steered.
    #[test]
    fn gyro_partial_suppress_latches_while_cursor_deflected() {
        let mut snap = menu_snap(15);
        snap.params.insert("ptr_ls".into(), Value::Bool(false));
        snap.params.insert("ptr_gyro".into(), Value::Bool(true));
        let dev_gyro = |rate: f32| -> HashMap<(String, String), Signal> {
            let mut m = HashMap::new();
            for pin in ["gyro_x", "gyro_y", "gyro_z"] {
                m.insert(("dev".to_string(), pin.to_string()), Signal::Float(rate));
            }
            m
        };
        let gk = (format!("{SRC_BLOCK_PREFIX}dev"), "gyro_x".to_string());
        let mut state = HashMap::new();

        // Tick 1 opens (prev_open false → no integration yet); tick 2 rotates,
        // driving the cursor past the deadzone. Fresh collector map per tick,
        // like the real pipeline — a stale block entry can't fake a pass.
        let mut c: HashMap<(String, String), Signal> = HashMap::new();
        eval_menu_node(&snap, 15, &show(true), &dev_gyro(1.0), &mut c, &mut state, 0.016);
        let mut c: HashMap<(String, String), Signal> = HashMap::new();
        eval_menu_node(&snap, 15, &show(true), &dev_gyro(1.0), &mut c, &mut state, 0.016);
        assert_eq!(c.get(&gk).map(|s| s.as_bool()), Some(true),
            "rotating gyro must be source-blocked in partial mode");

        // Rotation stops (holding on a target): the cursor is still deflected,
        // so the block must hold.
        let mut c: HashMap<(String, String), Signal> = HashMap::new();
        eval_menu_node(&snap, 15, &show(true), &dev_gyro(0.0), &mut c, &mut state, 0.016);
        assert_eq!(c.get(&gk).map(|s| s.as_bool()), Some(true),
            "block must latch while the gyro cursor is out of the deadzone");

        // A menu opened without ever tilting keeps gyro passing in partial mode.
        let mut st2 = HashMap::new();
        let mut c2: HashMap<(String, String), Signal> = HashMap::new();
        for _ in 0..3 {
            c2 = HashMap::new();
            eval_menu_node(&snap, 15, &show(true), &dev_gyro(0.0), &mut c2, &mut st2, 0.016);
        }
        assert!(c2.get(&gk).is_none(),
            "an untouched gyro driver must not be blocked in partial mode");
    }

    // Latch mode: the first driver to engage owns the menu exclusively — it is
    // blocked at the source while every OTHER enabled driver keeps passing to
    // the game — until it disengages, at which point the next engaged driver
    // takes over.
    #[test]
    fn latch_suppress_first_engaged_driver_owns_menu() {
        let mut snap = menu_snap(16);
        snap.params.insert("ptr_ls".into(), Value::Bool(true));
        snap.params.insert("ptr_gyro".into(), Value::Bool(true));
        snap.params.insert("suppress_mode".into(), Value::String("latch".into()));
        let dev = |x: f32, y: f32, rate: f32| -> HashMap<(String, String), Signal> {
            let mut m = HashMap::new();
            m.insert(("dev".to_string(), "left_stick".to_string()), Signal::Vec2(Vec2::new(x, y)));
            for pin in ["gyro_x", "gyro_y", "gyro_z"] {
                m.insert(("dev".to_string(), pin.to_string()), Signal::Float(rate));
            }
            m
        };
        let sk = format!("{SRC_BLOCK_PREFIX}dev");
        let lk = (sk.clone(), "left_stick".to_string());
        let gk = (sk.clone(), "gyro_x".to_string());
        let mut state = HashMap::new();

        // Tick 1 opens (no engagement while prev_open is false); tick 2: LS
        // deflects AND gyro rotates at once — LS engages first and takes the
        // latch, so LS is blocked while gyro passes untouched.
        let mut c: HashMap<(String, String), Signal> = HashMap::new();
        eval_menu_node(&snap, 16, &show(true), &dev(0.8, -0.8, 1.0), &mut c, &mut state, 0.016);
        let mut c: HashMap<(String, String), Signal> = HashMap::new();
        eval_menu_node(&snap, 16, &show(true), &dev(0.8, -0.8, 1.0), &mut c, &mut state, 0.016);
        assert_eq!(c.get(&lk).map(|s| s.as_bool()), Some(true),
            "the latched driver must be source-blocked");
        assert!(c.get(&gk).is_none(),
            "a non-latched driver must keep passing while another owns the menu");

        // LS returns to the deadzone while gyro keeps rotating: ownership hands
        // over — gyro is now blocked, LS passes again.
        let mut c: HashMap<(String, String), Signal> = HashMap::new();
        eval_menu_node(&snap, 16, &show(true), &dev(0.0, 0.0, 1.0), &mut c, &mut state, 0.016);
        assert!(c.get(&lk).is_none(),
            "a disengaged driver must be released from the block");
        assert_eq!(c.get(&gk).map(|s| s.as_bool()), Some(true),
            "the next engaged driver must take over the latch");
    }

    // A selected zone card must fire a momentary PULSE and then RELEASE, even
    // when its output pin isn't on the passthrough bus (the source device never
    // emits it). Without an explicit off-write the pin would latch "pressed" on
    // the virtual sink forever — the stuck-selection regression.
    #[test]
    fn selected_card_pulse_releases_not_latches() {
        let mut snap = menu_snap(13);
        snap.params.insert("select_on".into(), Value::String("press".into()));
        snap.params.insert("zone_maps".into(), serde_json::json!([
            { "f": 0, "z": 3, "in": ["menu_sel"], "out": ["btn_north"] }
        ]));
        let mut c: HashMap<(String, String), Signal> = HashMap::new();
        let mut state = HashMap::new();
        // Wired Show + Select: [_, Show, Select, _].
        let sel = |sh: bool, se: bool|
            vec![None, Some(Signal::Bool(sh)), Some(Signal::Bool(se)), None];

        // Open + hover zone 3, then press Select (rising edge selects).
        eval_menu_node(&snap, 13, &sel(true, false), &dev_stick(0.8, -0.8), &mut c, &mut state, 0.016);
        eval_menu_node(&snap, 13, &sel(true, true), &dev_stick(0.8, -0.8), &mut c, &mut state, 0.016);
        assert_eq!(
            c.get(&("menumap:13".to_string(), "btn_north".to_string())).map(|s| s.as_bool()),
            Some(true), "the select pulse asserts the zone card's output",
        );

        // Hold past the pulse (120 ms / 16 ms ≈ 8 ticks) with Select released.
        for _ in 0..12 {
            eval_menu_node(&snap, 13, &sel(true, false), &dev_stick(0.0, 0.0), &mut c, &mut state, 0.016);
        }
        assert_eq!(
            c.get(&("menumap:13".to_string(), "btn_north".to_string())).map(|s| s.as_bool()),
            Some(false), "after the pulse the card output must RELEASE (explicit off, no latch)",
        );
    }

    // The macro-style Show target (published by a Remapper mapping via
    // merge_macro_scalar) opens the menu with nothing wired.
    #[test]
    fn macro_style_show_target_opens() {
        let snap = menu_snap(2);
        let mut c: HashMap<(String, String), Signal> = HashMap::new();
        merge_macro_scalar(&mut c, "menu:abcd1234_show", Signal::Bool(true));
        let mut state = HashMap::new();
        let out = eval_menu_node(&snap, 2, &[], &dev_stick(0.0, 0.0), &mut c, &mut state, 0.016);
        assert_eq!(out[1], Some(Signal::Bool(true)));
    }

    // Toggle activation: rising Show opens, holding/releasing changes nothing,
    // the next rising edge closes.
    #[test]
    fn toggle_mode_edges() {
        let mut snap = menu_snap(3);
        snap.params.insert("activation_mode".into(), Value::String("toggle".into()));
        let mut c: HashMap<(String, String), Signal> = HashMap::new();
        let mut state = HashMap::new();
        let dev = dev_stick(0.0, 0.0);

        let out = eval_menu_node(&snap, 3, &show(true), &dev, &mut c, &mut state, 0.016);
        assert_eq!(out[1], Some(Signal::Bool(true)), "rising edge opens");
        let out = eval_menu_node(&snap, 3, &show(true), &dev, &mut c, &mut state, 0.016);
        assert_eq!(out[1], Some(Signal::Bool(true)), "held: stays open");
        let out = eval_menu_node(&snap, 3, &show(false), &dev, &mut c, &mut state, 0.016);
        assert_eq!(out[1], Some(Signal::Bool(true)), "released: stays open");
        let out = eval_menu_node(&snap, 3, &show(true), &dev, &mut c, &mut state, 0.016);
        assert_eq!(out[1], Some(Signal::Bool(false)), "second rising edge closes");
    }

    // select_on = "press": the wired Select input commits the hovered zone
    // while the menu stays open.
    #[test]
    fn press_select_fires_card_while_open() {
        let mut snap = menu_snap(4);
        snap.params.insert("select_on".into(), Value::String("press".into()));
        snap.params.insert("zone_maps".into(), serde_json::json!([
            { "f": 0, "z": 0, "in": ["menu_sel"], "out": ["btn_west"] }
        ]));
        let mut c: HashMap<(String, String), Signal> = HashMap::new();
        let mut state = HashMap::new();
        let sel = |show_b: bool, sel_b: bool| {
            vec![None, Some(Signal::Bool(show_b)), Some(Signal::Bool(sel_b)), None]
        };
        // Open + hover top-left (stick -x, +y => unit (0.1, 0.1) => zone 0).
        let out = eval_menu_node(&snap, 4, &sel(true, false), &dev_stick(-0.8, 0.8), &mut c, &mut state, 0.016);
        assert_eq!(out[2], Some(Signal::Float(0.0)));
        // Select press: card fires; menu stays open.
        let out = eval_menu_node(&snap, 4, &sel(true, true), &dev_stick(-0.8, 0.8), &mut c, &mut state, 0.016);
        assert_eq!(out[1], Some(Signal::Bool(true)));
        assert_eq!(c.get(&("menumap:4".to_string(), "btn_west".to_string())).copied(),
            Some(Signal::Bool(true)));
    }

    // Radial mode: the pointer picks a sector by ANGLE (sector 0 up,
    // clockwise), the dead center hovers nothing, and hover stays sticky
    // when the stick returns to rest.
    #[test]
    fn radial_mode_sector_by_angle() {
        let mut snap = menu_snap(6);
        snap.params.insert("menu_radial".into(), Value::Bool(true));
        // Synthetic 1×4 strip = 4 sectors (up, right, down, left).
        snap.params.insert("col_edges".into(), serde_json::json!([0.25, 0.5, 0.75]));
        snap.params.insert("row_edges".into(), serde_json::json!([] as [f32; 0]));
        let mut c: HashMap<(String, String), Signal> = HashMap::new();
        let mut state = HashMap::new();

        // Stick up (+y = up in stick coords) → sector 0.
        let out = eval_menu_node(&snap, 6, &show(true), &dev_stick(0.0, 0.9), &mut c, &mut state, 0.016);
        assert_eq!(out[1], Some(Signal::Bool(true)));
        assert_eq!(out[2], Some(Signal::Float(0.0)));
        // Stick right → sector 1; down → 2; left → 3.
        let out = eval_menu_node(&snap, 6, &show(true), &dev_stick(0.9, 0.0), &mut c, &mut state, 0.016);
        assert_eq!(out[2], Some(Signal::Float(1.0)));
        let out = eval_menu_node(&snap, 6, &show(true), &dev_stick(0.0, -0.9), &mut c, &mut state, 0.016);
        assert_eq!(out[2], Some(Signal::Float(2.0)));
        let out = eval_menu_node(&snap, 6, &show(true), &dev_stick(-0.9, 0.0), &mut c, &mut state, 0.016);
        assert_eq!(out[2], Some(Signal::Float(3.0)));
        // Back to rest: hover stays sticky on the last sector.
        let out = eval_menu_node(&snap, 6, &show(true), &dev_stick(0.0, 0.0), &mut c, &mut state, 0.016);
        assert_eq!(out[2], Some(Signal::Float(3.0)));
    }

    // hover_sticky = false: returning to the deadzone clears the highlight,
    // and a release there selects nothing (no card fires).
    #[test]
    fn non_sticky_hover_clears_in_deadzone() {
        let mut snap = menu_snap(7);
        snap.params.insert("hover_sticky".into(), Value::Bool(false));
        snap.params.insert("zone_maps".into(), serde_json::json!([
            { "f": 0, "z": 3, "in": ["menu_sel"], "out": ["btn_south"] }
        ]));
        let mut c: HashMap<(String, String), Signal> = HashMap::new();
        let mut state = HashMap::new();

        let out = eval_menu_node(&snap, 7, &show(true), &dev_stick(0.8, -0.8), &mut c, &mut state, 0.016);
        assert_eq!(out[2], Some(Signal::Float(3.0)));
        // Back inside the deadzone: highlight clears instead of sticking.
        let out = eval_menu_node(&snap, 7, &show(true), &dev_stick(0.0, 0.0), &mut c, &mut state, 0.016);
        assert_eq!(out[2], Some(Signal::Float(-1.0)));
        // Release with nothing highlighted: closes without selecting.
        let out = eval_menu_node(&snap, 7, &show(false), &dev_stick(0.0, 0.0), &mut c, &mut state, 0.016);
        assert_eq!(out[1], Some(Signal::Bool(false)));
        assert_eq!(out[out.len() - 2], None, "no selection mirrored");
        assert!(c.get(&("menumap:7".to_string(), "btn_south".to_string()))
            .map(|s| !s.as_bool()).unwrap_or(true));
    }

    // A mapping card targeting ANOTHER macro-style pin routes into the macro
    // namespace instead of the bus (a menu selection can raise a Macro port /
    // another menu's Show).
    #[test]
    fn menu_card_can_target_macro_pin() {
        let mut snap = menu_snap(5);
        snap.params.insert("select_on".into(), Value::String("press".into()));
        snap.params.insert("zone_maps".into(), serde_json::json!([
            { "f": 0, "z": 0, "in": ["menu_sel"], "out": ["macro:ff00ff00"] }
        ]));
        let mut c: HashMap<(String, String), Signal> = HashMap::new();
        let mut state = HashMap::new();
        // Two frames: hover establishes on frame 1, the Select edge rises on
        // frame 2 (prev_sel must be low for the edge to register).
        let f1 = vec![None, Some(Signal::Bool(true)), Some(Signal::Bool(false)), None];
        let f2 = vec![None, Some(Signal::Bool(true)), Some(Signal::Bool(true)), None];
        eval_menu_node(&snap, 5, &f1, &dev_stick(-0.8, 0.8), &mut c, &mut state, 0.016);
        eval_menu_node(&snap, 5, &f2, &dev_stick(-0.8, 0.8), &mut c, &mut state, 0.016);
        assert_eq!(c.get(&("macro".to_string(), "macro:ff00ff00".to_string())).copied(),
            Some(Signal::Bool(true)));
        assert!(!c.contains_key(&("menumap:5".to_string(), "macro:ff00ff00".to_string())),
            "macro-style targets must not leak onto the menu's bus key");
    }
}
