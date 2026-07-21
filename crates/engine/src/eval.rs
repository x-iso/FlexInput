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
mod publish;

pub use activation::*;
pub use compute::*;
pub use config::*;
pub use curves::*;
pub use device_cal::*;
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

/// Evaluate a Remapper node — shared by the top-level loop and the sub-patch
/// (`eval_subgraph`) loop so the two can never diverge. `uid` is the publishing
/// id: `snap.node_uid` at top level, the namespaced uid inside a sub-patch. It
/// keys `collector_sigs["remap:{uid}"]`, the per-node `state`, and `last_outputs`.
fn eval_remapper_node(
    snap: &NodeSnap,
    uid: usize,
    dev_sigs: &HashMap<(String, String), Signal>,
    collector_sigs: &mut HashMap<(String, String), Signal>,
    state: &mut HashMap<usize, NodeState>,
    dt: f32,
) {
            let dev_id = snap.params.get("_automap_device_id").and_then(|v| v.as_str()).unwrap_or("");
            let collector_id = snap.params.get("_automap_collector_id").and_then(|v| v.as_str()).unwrap_or("");
            let mappings = snap.params.get("mappings").and_then(|v| v.as_array()).cloned().unwrap_or_default();
            let key = format!("remap:{}", uid);

            // Snapshot upstream values for every canonical pin once, so we can
            // freely mutate collector_sigs below without aliasing the read side.
            let mut upstream: HashMap<String, Signal> = HashMap::new();
            for ap in automap::ALL_PINS {
                let sig = if !collector_id.is_empty() {
                    collector_sigs.get(&(collector_id.to_string(), ap.id.to_string())).copied()
                } else { None }
                .or_else(|| {
                    if !dev_id.is_empty() {
                        dev_sigs.get(&(dev_id.to_string(), ap.id.to_string())).copied()
                    } else { None }
                });
                if let Some(s) = sig { upstream.insert(ap.id.to_string(), s); }
            }
            // A processed Vec2 on the collector is authoritative over raw axes.
            vec2_authoritative_axis_fill(&mut upstream, collector_id, &*collector_sigs);
            // Derive synthetic cardinal-direction Bool pins from each stick's
            // (x, y) so they can participate in mapping triggers just like
            // buttons. See `derive_stick_cardinals` for the dominant-axis rule.
            derive_stick_cardinals(&mut upstream);

            // Derive touchpad zone pins. Two parallel variants:
            //   touch_*       — fire whenever a finger is in that zone, click
            //                   or not. Up to 2 zones at once (one per finger).
            //                   No accumulation; transient, instantaneous.
            //   touchpad_*    — fire only while btn_touchpad is held. While
            //                   held, every zone any finger has visited stays
            //                   asserted (swipe accumulation) so a drag
            //                   across all three zones produces a 3-pin chord.
            //                   Release of btn_touchpad clears the accumulator.
            // Per-zone override: if touchpad_N (click variant) fires, touch_N
            // (touch-only) is forced false so a click-mapped zone takes over
            // from a touch-mapped one rather than firing both.
            let touch_click = upstream.get("btn_touchpad")
                .map(|s| s.as_bool()).unwrap_or(false);
            let zone_of_x = |x: f32| -> usize {
                if x < -1.0/3.0 { 0 } else if x > 1.0/3.0 { 2 } else { 1 }
            };
            // Touch-only zones — each active finger asserts exactly one zone
            // (the one its X currently sits in). Moving a finger from zone A
            // to zone B drops A and asserts B for that finger. With two
            // fingers active, two zones can fire simultaneously. No swipe
            // accumulation here — that's reserved for the click variant.
            let mut touch_only = [false; 3];
            for (xpin, apin) in [("touch1_x","touch1_active"),
                                 ("touch2_x","touch2_active")] {
                let active = upstream.get(apin).map(|s| s.as_bool()).unwrap_or(false);
                if !active { continue; }
                let x = upstream.get(xpin).map(|s| sig_scalar(*s)).unwrap_or(0.0);
                touch_only[zone_of_x(x)] = true;
            }
            // Click-variant zones — accumulated in per-node aux_f32.
            let ns = state.entry(uid).or_insert_with(NodeState::default);
            if ns.aux_f32.len() < 3 { ns.aux_f32.resize(3, 0.0); }
            if !touch_click {
                ns.aux_f32[0] = 0.0;
                ns.aux_f32[1] = 0.0;
                ns.aux_f32[2] = 0.0;
            } else {
                for (xpin, apin) in [("touch1_x","touch1_active"),
                                     ("touch2_x","touch2_active")] {
                    let active = upstream.get(apin).map(|s| s.as_bool()).unwrap_or(false);
                    if !active { continue; }
                    let x = upstream.get(xpin).map(|s| sig_scalar(*s)).unwrap_or(0.0);
                    ns.aux_f32[zone_of_x(x)] = 1.0;
                }
            }
            let click_zone = [
                ns.aux_f32[0] > 0.5,
                ns.aux_f32[1] > 0.5,
                ns.aux_f32[2] > 0.5,
            ];
            let any_zone = click_zone[0] || click_zone[1] || click_zone[2];
            // Click suppresses all touch-only zones — once btn_touchpad
            // fires, the click variants own the touchpad.
            if touch_click {
                touch_only[0] = false;
                touch_only[1] = false;
                touch_only[2] = false;
            }
            upstream.insert("touchpad_left".to_string(),   Signal::Bool(click_zone[0]));
            upstream.insert("touchpad_center".to_string(), Signal::Bool(click_zone[1]));
            upstream.insert("touchpad_right".to_string(),  Signal::Bool(click_zone[2]));
            // touchpad_any — "click anywhere on the pad". Available via the
            // Special… dropdown only (not auto-captured) so users opt in.
            // Fires together with the specific-zone pin additively.
            upstream.insert("touchpad_any".to_string(),    Signal::Bool(touch_click && any_zone));
            upstream.insert("touch_left".to_string(),      Signal::Bool(touch_only[0]));
            upstream.insert("touch_center".to_string(),    Signal::Bool(touch_only[1]));
            upstream.insert("touch_right".to_string(),     Signal::Bool(touch_only[2]));

            let read_upstream = |pin_id: &str| -> Option<Signal> { upstream.get(pin_id).copied() };

            // Per-mapping press mode is stored under `mode` + `window_ms` +
            // `sustain` on each mapping. The state machine must run for every
            // mapping every tick (not just claimed ones) so Short / Long /
            // Double detect edges without dropouts. Compute `effective_held`
            // for each in original index order, then run the sort + claim pass
            // using those values instead of re-reading raw input state.
            //
            // Analog mode is gated differently from digital modes:
            //   - Non-cardinal `in` pins must all be held (combo gate).
            //   - If any cardinal `in` pin exists, its axis magnitude must
            //     exceed GESTURE_ACTIVATE_MAG so we know the stick is being
            //     pushed in (one of) the mapped direction(s).
            //   - Pure cardinal `in`: just magnitude check, no gesture trace.
            //   - Press-mode pipeline is bypassed; analog mode owns its own
            //     "active" definition. Turbo on analog button-outputs is
            //     applied during the publish pass below.
            let ns = state.entry(uid).or_insert_with(NodeState::default);
            let effective: Vec<bool> = mappings.iter().enumerate().map(|(i, m)| {
                let in_pins: Vec<&str> = m.get("in").and_then(|v| v.as_array())
                    .map(|a| a.iter().filter_map(|v| v.as_str()).collect())
                    .unwrap_or_default();
                if in_pins.is_empty() { return false; }
                let mode_s = m.get("mode").and_then(|v| v.as_str()).unwrap_or("down");
                if mode_s == "analog" {
                    // Buttons (non-cardinal) all held? Cardinals: any
                    // non-zero magnitude is enough — analog mode passes the
                    // live magnitude through, no activation threshold.
                    let mut has_cardinal = false;
                    let mut any_cardinal_active = false;
                    let mut all_buttons_held = true;
                    for p in &in_pins {
                        if analog_axis_for_cardinal(p).is_some() {
                            has_cardinal = true;
                            if analog_cardinal_input_value(&upstream, p) > 0.0 {
                                any_cardinal_active = true;
                            }
                        } else if !read_upstream(p).map(|s| s.as_bool()).unwrap_or(false) {
                            all_buttons_held = false;
                        }
                    }
                    // Pure-button analog mappings (no cardinal in) reduce to
                    // "all held" — same as Down mode. Reasonable fallback.
                    return all_buttons_held && (!has_cardinal || any_cardinal_active);
                }
                // Stick-gesture path: when every `in` pin is a stick cardinal,
                // the chord can never be "simultaneously held" (a single stick
                // can't be Left AND Right at the same instant). Instead we
                // track which cardinals have been visited during the active
                // gesture and fire when all required cardinals across both
                // sticks have been visited at least once.
                // Manual activation threshold: an explicit "fire at this
                // magnitude" instruction. It BYPASSES the stick-gesture
                // accumulator (visit-all-cardinals semantics conflict with a
                // hold-above-the-line gate) and replaces the built-in
                // cardinal derivation / 0.5 trigger coercion: each analog in
                // pin gates on the card's curve-shaped magnitude crossing the
                // line, releasing the moment it dips back below.
                let thr = mapping_threshold(m);
                let raw_held = if let (Some(required), None) = (gesture_required_bits(&in_pins), thr) {
                    let buttons_held = in_pins.iter().all(|p| {
                        if gesture_pin_to_bit(p).is_some() { return true; }
                        read_upstream(p).map(|s| s.as_bool()).unwrap_or(false)
                    });
                    let visited = gesture_state_get(ns, i);
                    buttons_held && gesture_tick(required, visited, &upstream)
                } else {
                    let curve = mapping_curve_pts(m);
                    in_pins.iter().all(|p| {
                        if let (Some(t), Some(v)) = (thr, analog_in_value(&upstream, p)) {
                            return shape_mag(&curve, v) >= t;
                        }
                        read_upstream(p).map(|s| s.as_bool()).unwrap_or(false)
                    })
                };
                let mode = PressMode::from_str(mode_s);
                let window_ms = m.get("window_ms").and_then(|v| v.as_f64()).unwrap_or(200.0) as f32;
                let sustain   = m.get("sustain").and_then(|v| v.as_bool()).unwrap_or(false);
                let turbo     = m.get("turbo").and_then(|v| v.as_bool()).unwrap_or(false);
                let slots = press_state_get(ns, i);
                let held = apply_press_mode(raw_held, mode, window_ms, sustain, slots, dt);
                if turbo { apply_turbo(held, window_ms, slots, dt) } else { held }
            }).collect();

            // Physical-hold state per mapping, INDEPENDENT of press mode — true
            // whenever the mapping's input chord is currently held/deflected.
            // Used for input SUPPRESSION: a consumed input must stay suppressed
            // for as long as it is held, even when the press-mode gate (on-press
            // pulse, double-tap window, etc.) is momentarily closed. Otherwise
            // the raw input would leak through while the user keeps holding it.
            let held_now: Vec<bool> = mappings.iter().map(|m| {
                let in_pins: Vec<&str> = m.get("in").and_then(|v| v.as_array())
                    .map(|a| a.iter().filter_map(|v| v.as_str()).collect())
                    .unwrap_or_default();
                if in_pins.is_empty() { return false; }
                // Touch-output combos can mix opposite cardinals of one axis
                // (left+right), which can never be "simultaneously held"; use the
                // touch-combo activation rule so their gate buttons + sticks get
                // consumed whenever the combo is active (gate buttons held, analog
                // deflection optional). Otherwise the generic all-held check below
                // would never fire and the buttons would leak through.
                if mapping_targets_touch(m) {
                    return eval_touch_combo(&in_pins, &upstream).active;
                }
                // With a manual threshold, suppression tracks the same
                // shaped-magnitude gate as activation so a below-threshold
                // deflection doesn't consume the input it isn't firing on.
                let thr = mapping_threshold(m);
                let curve = mapping_curve_pts(m);
                in_pins.iter().all(|p| {
                    if let (Some(t), Some(v)) = (thr, analog_in_value(&upstream, p)) {
                        return shape_mag(&curve, v) >= t;
                    }
                    if analog_axis_for_cardinal(p).is_some() {
                        analog_cardinal_input_value(&upstream, p) > 0.0
                    } else {
                        read_upstream(p).map(|s| s.as_bool()).unwrap_or(false)
                    }
                })
            }).collect();

            // Determine which mappings are currently triggered. Sort indices
            // by descending input-set size so longer combos win conflicts;
            // original indices are preserved so we can look up `effective`
            // and mapping fields afterwards.
            let mut sorted_idx: Vec<usize> = (0..mappings.len()).collect();
            sorted_idx.sort_by(|&a, &b| {
                let la = mappings[a].get("in").and_then(|v| v.as_array()).map(|a| a.len()).unwrap_or(0);
                let lb = mappings[b].get("in").and_then(|v| v.as_array()).map(|a| a.len()).unwrap_or(0);
                lb.cmp(&la)
            });

            // Trigger pass 1: identify triggered mappings and the pins they consume.
            //
            // Suppression rule for overlapping mappings:
            //   - A mapping is suppressed iff a STRICTLY LONGER triggered
            //     mapping has already claimed all of its inputs (longer
            //     chord wins over shorter sub-chord).
            //   - Mappings with the SAME input set are allowed to coexist
            //     so users can fan one button out to multiple outputs:
            //     `Y → X` and `Y → Y` both fire when Y is pressed.
            //
            // Analog mappings with IDENTICAL input chords have an extra
            // last-wins override applied during the publish pass below
            // (user-error guard for conflicting analog writes).
            let mut triggered: Vec<(Vec<String>, Vec<String>, bool, usize)> = Vec::new(); // (in, out, is_analog, orig_idx)
            let mut triggered_claims: Vec<(usize, Vec<String>)> = Vec::new();
            for &i in &sorted_idx {
                let m = &mappings[i];
                let in_pins: Vec<String> = m.get("in").and_then(|v| v.as_array())
                    .map(|a| a.iter().filter_map(|v| v.as_str().map(|s| s.to_string())).collect())
                    .unwrap_or_default();
                let out_pins: Vec<String> = m.get("out").and_then(|v| v.as_array())
                    .map(|a| a.iter().filter_map(|v| v.as_str().map(|s| s.to_string())).collect())
                    .unwrap_or_default();
                if in_pins.is_empty() { continue; }
                if !effective[i] { continue; }
                let my_len = in_pins.len();
                let suppressed = triggered_claims.iter().any(|(claim_len, claim_pins)| {
                    *claim_len > my_len && in_pins.iter().all(|p| claim_pins.contains(p))
                });
                if suppressed { continue; }
                let is_analog = m.get("mode").and_then(|v| v.as_str()) == Some("analog");
                let mut sorted_in = in_pins.clone();
                sorted_in.sort();
                triggered_claims.push((my_len, sorted_in));
                triggered.push((in_pins, out_pins, is_analog, i));
            }

            // Claimed inputs split by mode so pass-through suppression for
            // analog cardinal claims can use axis-side clamping rather than
            // hard-zeroing the entire axis.
            //
            // Suppression follows PHYSICAL HOLD (`held_now`), not the press-mode
            // gate (`effective`/`triggered`): once a mapping consumes an input,
            // that input is suppressed for as long as it's held, regardless of
            // press mode. EXCEPTION — an input a mapping routes back to ITSELF
            // (e.g. `dpad_left → dpad_left`, a deliberate pass-through) is NOT
            // suppressed, so the user can keep an input while also reacting to it.
            let mut self_mapped: HashSet<String> = HashSet::new();
            for m in &mappings {
                let ins: Vec<&str> = m.get("in").and_then(|v| v.as_array())
                    .map(|a| a.iter().filter_map(|v| v.as_str()).collect()).unwrap_or_default();
                let outs: Vec<&str> = m.get("out").and_then(|v| v.as_array())
                    .map(|a| a.iter().filter_map(|v| v.as_str()).collect()).unwrap_or_default();
                for p in &ins {
                    if outs.contains(p) { self_mapped.insert((*p).to_string()); }
                }
            }
            let mut claimed_inputs_digital: HashSet<String> = HashSet::new();
            let mut claimed_inputs_analog: HashSet<String>  = HashSet::new();
            for (i, m) in mappings.iter().enumerate() {
                if !held_now[i] { continue; }
                let is_analog = m.get("mode").and_then(|v| v.as_str()) == Some("analog");
                let in_pins: Vec<String> = m.get("in").and_then(|v| v.as_array())
                    .map(|a| a.iter().filter_map(|v| v.as_str().map(|s| s.to_string())).collect())
                    .unwrap_or_default();
                let target = if is_analog { &mut claimed_inputs_analog } else { &mut claimed_inputs_digital };
                for p in in_pins {
                    if self_mapped.contains(&p) { continue; }
                    target.insert(p);
                }
            }
            // Pass-through + per-side suppression (sticks + D-pad) + consumed
            // markers — shared with the sub-patch arm so they never diverge.
            remapper_pass_through_and_suppress(
                &key, &upstream,
                &claimed_inputs_digital, &claimed_inputs_analog,
                collector_sigs,
            );

            // ── Analog publish pass ──────────────────────────────────────
            //
            // Apply identical input+output-chord override (last wins). Build the
            // set of analog mappings to actually emit, suppressing any earlier
            // analog mapping that is a TRUE duplicate (same inputs AND same
            // outputs) of a later one. Mappings sharing an input but targeting
            // different outputs (e.g. left_stick_up→right_trigger alongside
            // left_stick_up→left_stick_up to keep the stick) both fire.
            let mut analog_emit_idx: Vec<usize> = Vec::new();
            {
                // Walk triggered in original mapping order so "later in the
                // user's list wins". `triggered` was built in sorted_idx
                // (longest-first) order; recover original order via the
                // orig_idx we stored.
                let mut analog_indices: Vec<usize> = (0..triggered.len())
                    .filter(|&t| triggered[t].2)
                    .collect();
                analog_indices.sort_by_key(|&t| triggered[t].3);
                let sorted_set = |v: &Vec<String>| -> Vec<String> {
                    let mut s = v.clone(); s.sort(); s
                };
                let mut keep: Vec<bool> = vec![true; analog_indices.len()];
                for a in 0..analog_indices.len() {
                    if !keep[a] { continue; }
                    let (ref ain, ref aout, _, _) = triggered[analog_indices[a]];
                    let (a_in, a_out) = (sorted_set(ain), sorted_set(aout));
                    for b in (a + 1)..analog_indices.len() {
                        let (ref bin, ref bout, _, _) = triggered[analog_indices[b]];
                        if a_in == sorted_set(bin) && a_out == sorted_set(bout) {
                            // Later (higher index) wins → suppress earlier dup.
                            keep[a] = false;
                            break;
                        }
                    }
                }
                for (a, t_idx) in analog_indices.iter().enumerate() {
                    if keep[a] { analog_emit_idx.push(*t_idx); }
                }
            }

            // Accumulate cardinal-axis writes additively; track button-output
            // emissions per output-pin for turbo / sustain handling.
            let mut analog_axis_acc: HashMap<&'static str, f32> = HashMap::new();
            let mut analog_button_out: HashSet<String> = HashSet::new();
            let mut analog_out_pins: HashSet<String> = HashSet::new();
            for &t_idx in &analog_emit_idx {
                let (ref in_pins, ref out_pins, _, orig_i) = triggered[t_idx];
                let m = &mappings[orig_i];
                let turbo  = m.get("turbo").and_then(|v| v.as_bool()).unwrap_or(false);
                let sustain = m.get("sustain").and_then(|v| v.as_bool()).unwrap_or(false);
                let window_ms = m.get("window_ms").and_then(|v| v.as_f64()).unwrap_or(200.0) as f32;
                let slots = press_state_get(ns, orig_i);
                // Per-card response curve + manual threshold: the curve
                // reshapes every magnitude this mapping emits (axis, trigger,
                // macro, pulse rate); the threshold turns digital outs into a
                // plain hold gate on the shaped value (see the button arm).
                let curve = mapping_curve_pts(m);
                let thr = mapping_threshold(m);
                // Zip in↔out by index; drop the excess from whichever side
                // is longer.
                let n = in_pins.len().min(out_pins.len());
                for (in_p, out_p) in in_pins[..n].iter().zip(out_pins[..n].iter()) {
                    // Touchpad zone/swipe outputs are handled by the touchpad
                    // synthesis pass below, not as axis/trigger/button writes.
                    if touchpad_out_kind(out_p).is_some() { continue; }
                    // Macro-port target: publish the live input magnitude into
                    // the macro namespace and skip the bus handling below
                    // (macro pins never reach sinks or the release pass).
                    if is_macro_style_target(out_p) {
                        let mag = if analog_axis_for_cardinal(in_p).is_some() {
                            analog_cardinal_input_value(&upstream, in_p)
                        } else {
                            1.0 // gate buttons all held (checked by effective[])
                        };
                        let mag = shape_mag(&curve, mag);
                        if mag > 0.0 {
                            merge_macro_scalar(collector_sigs, out_p, Signal::Float(mag.min(1.0)));
                        }
                        continue;
                    }
                    analog_out_pins.insert(out_p.clone());
                    let in_is_cardinal  = analog_axis_for_cardinal(in_p).is_some();
                    let out_axis_opt    = analog_axis_for_cardinal(out_p);
                    let out_trigger     = analog_trigger_out(out_p);
                    let mag_from_input = if in_is_cardinal {
                        analog_cardinal_input_value(&upstream, in_p)
                    } else {
                        // Non-cardinal in pin in this slot — when paired with
                        // a cardinal out, drive it at full magnitude while the
                        // gate is open (the effective[] check guaranteed all
                        // non-cardinal buttons are held).
                        1.0
                    };
                    let mag_from_input = shape_mag(&curve, mag_from_input);
                    if let Some((axis_pin, sign)) = out_axis_opt {
                        let contrib = sign * mag_from_input;
                        // Sum across all (mapping × in/out pair) contributions.
                        let entry = analog_axis_acc.entry(axis_pin).or_insert(0.0);
                        *entry += contrib;
                    } else if let Some(trigger_pin) = out_trigger {
                        // One-sided 0..1 trigger axis — drive it with the input's
                        // live magnitude (converts analog stick direction into
                        // analog trigger travel, incl. on pads lacking analog
                        // triggers like Switch Pro).
                        let entry = analog_axis_acc.entry(trigger_pin).or_insert(0.0);
                        *entry += mag_from_input.max(0.0);
                    } else {
                        // Non-cardinal out: button / key.
                        // With a manual threshold, the output is a PLAIN HOLD:
                        // pressed while the shaped magnitude sits on/above the
                        // line, released the moment it dips below (Turbo still
                        // taps while held). Without one, the legacy behaviour:
                        // a freq-modulated tap train (or PWM under Hold) so the
                        // digital destination reflects HOW FAR the stick is
                        // pushed — matching the 3DOF-Lean analog→digital path.
                        let active = if let Some(t) = thr {
                            let held = mag_from_input >= t;
                            if turbo { apply_turbo(held, window_ms, slots, dt) } else { held }
                        } else {
                            analog_digital_pulse(
                                mag_from_input, window_ms, sustain, turbo, slots, dt,
                            )
                        };
                        if active {
                            analog_button_out.insert(out_p.clone());
                        }
                    }
                }
            }
            // Commit axis accumulator: clamp ±1 then write.
            for (axis_pin, v) in &analog_axis_acc {
                let clamped = v.clamp(-1.0, 1.0);
                collector_sigs.insert((key.clone(), (*axis_pin).to_string()), Signal::Float(clamped));
            }
            // Update bundled Vec2 pins so downstream sinks that read the
            // Vec2 form (`left_stick`/`right_stick`) see the analog-driven
            // values too. Without this, the sink's Vec2-vs-axis conflict
            // resolver picks the Vec2 (which still carries the suppressed
            // pass-through) and drops the analog axis writes.
            for (vec2_pin, x_axis, y_axis) in [
                ("left_stick", "left_stick_x", "left_stick_y"),
                ("right_stick", "right_stick_x", "right_stick_y"),
            ] {
                let x_override = analog_axis_acc.get(&x_axis).copied();
                let y_override = analog_axis_acc.get(&y_axis).copied();
                if x_override.is_none() && y_override.is_none() { continue; }
                let cur = collector_sigs.get(&(key.clone(), vec2_pin.to_string()))
                    .and_then(|s| if let Signal::Vec2(v) = s { Some(*v) } else { None })
                    .unwrap_or(Vec2::ZERO);
                let x = x_override.map(|v| v.clamp(-1.0, 1.0)).unwrap_or(cur.x);
                let y = y_override.map(|v| v.clamp(-1.0, 1.0)).unwrap_or(cur.y);
                collector_sigs.insert((key.clone(), vec2_pin.to_string()), Signal::Vec2(Vec2::new(x, y)));
            }

            // ── Digital publish pass (existing semantics) ────────────────
            //
            // Collect every output pin mentioned in any DIGITAL mapping so
            // released ones can publish false/0. Analog-only out pins are
            // handled by the analog pass above.
            let mut digital_all_out_pins: HashSet<String> = HashSet::new();
            for (i, m) in mappings.iter().enumerate() {
                let is_analog = m.get("mode").and_then(|v| v.as_str()) == Some("analog");
                if is_analog { continue; }
                let _ = i;
                if let Some(arr) = m.get("out").and_then(|v| v.as_array()) {
                    for v in arr {
                        if let Some(s) = v.as_str() {
                            if touchpad_out_kind(s).is_some() { continue; } // synthesized below
                            // Macro pins skip the bus release pass entirely —
                            // absent from the macro namespace = released.
                            if is_macro_style_target(s) { continue; }
                            digital_all_out_pins.insert(s.to_string());
                        }
                    }
                }
            }
            let mut digital_asserted: HashSet<String> = HashSet::new();
            for (_, out_pins, is_analog, _) in &triggered {
                if *is_analog { continue; }
                for p in out_pins {
                    if touchpad_out_kind(p).is_some() { continue; } // synthesized below
                    digital_asserted.insert(p.clone());
                }
            }
            // Macro-port targets of triggered digital mappings: publish into
            // the macro namespace (press-mode shaping already applied via
            // `effective[]` → `triggered`). Bus pins continue below.
            for p in &digital_asserted {
                if is_macro_style_target(p) {
                    merge_macro_scalar(collector_sigs, p, Signal::Bool(true));
                }
            }
            for out_pin in &digital_all_out_pins {
                let sig_type = automap::ALL_PINS.iter()
                    .find(|p| p.id == out_pin.as_str())
                    .map(|p| p.signal_type)
                    .unwrap_or(SignalType::Bool);
                let on = digital_asserted.contains(out_pin);
                if on {
                    let sig = match sig_type {
                        SignalType::Float => Signal::Float(1.0),
                        SignalType::Vec2  => continue,
                        SignalType::Int   => Signal::Int(1),
                        _                 => Signal::Bool(true),
                    };
                    collector_sigs.insert((key.clone(), out_pin.clone()), sig);
                } else {
                    if upstream.contains_key(out_pin.as_str()) { continue; }
                    // If an analog mapping has already written this same out
                    // pin (e.g., the user fans a button to it from a different
                    // mapping), don't overwrite with zero.
                    if analog_button_out.contains(out_pin)
                        || analog_axis_acc.iter().any(|(ap, _)| *ap == out_pin.as_str())
                    {
                        continue;
                    }
                    let sig = match sig_type {
                        SignalType::Float => Signal::Float(0.0),
                        SignalType::Vec2  => continue,
                        SignalType::Int   => Signal::Int(0),
                        _                 => Signal::Bool(false),
                    };
                    collector_sigs.insert((key.clone(), out_pin.clone()), sig);
                }
            }

            // ── Analog button-out emissions + release pass ───────────────
            //
            // Bool/Int analog out pins: write true while active. For released
            // analog out pins (mapping inactive this tick), write false/0
            // only when upstream doesn't naturally emit it (mirrors digital
            // release rule).
            let mut analog_button_pins: HashSet<String> = HashSet::new();
            for m in &mappings {
                let is_analog = m.get("mode").and_then(|v| v.as_str()) == Some("analog");
                if !is_analog { continue; }
                if let Some(arr) = m.get("out").and_then(|v| v.as_array()) {
                    for v in arr {
                        if let Some(s) = v.as_str() {
                            // Triggers are analog axes (handled by analog_axis_acc),
                            // not buttons — exclude them from the binary on/off
                            // release pass or it would clobber the analog value.
                            // Macro pins are published via the macro namespace
                            // in the emit loop above, never as bus buttons.
                            if analog_axis_for_cardinal(s).is_none()
                                && analog_trigger_out(s).is_none()
                                && touchpad_out_kind(s).is_none()
                                && !is_macro_style_target(s)
                            {
                                analog_button_pins.insert(s.to_string());
                            }
                        }
                    }
                }
            }
            for out_pin in &analog_button_pins {
                if digital_asserted.contains(out_pin) { continue; } // digital wins for this pin
                let on = analog_button_out.contains(out_pin);
                let sig_type = automap::ALL_PINS.iter()
                    .find(|p| p.id == out_pin.as_str())
                    .map(|p| p.signal_type)
                    .unwrap_or(SignalType::Bool);
                if on {
                    let sig = match sig_type {
                        SignalType::Float => Signal::Float(1.0),
                        SignalType::Vec2  => continue,
                        SignalType::Int   => Signal::Int(1),
                        _                 => Signal::Bool(true),
                    };
                    collector_sigs.insert((key.clone(), out_pin.clone()), sig);
                } else {
                    if upstream.contains_key(out_pin.as_str()) { continue; }
                    let sig = match sig_type {
                        SignalType::Float => Signal::Float(0.0),
                        SignalType::Vec2  => continue,
                        SignalType::Int   => Signal::Int(0),
                        _                 => Signal::Bool(false),
                    };
                    collector_sigs.insert((key.clone(), out_pin.clone()), sig);
                }
            }

            // ── Touchpad output synthesis (zones + analog swipe) ──────────
            //
            // If ANY mapping targets a touchpad zone/swipe pin, the Remapper owns
            // the virtual touchpad. Each touch mapping yields ONE finger; stack up
            // to the 2 hardware touch points (original mapping order). Plain
            // `btn_touchpad` (click) / `btn_mute` are canonical and handled above.
            //
            // Input roles within a touch mapping (this is NOT index-zip):
            //   • BUTTONS gate the finger — all must be held for it to be active;
            //     they never contribute a value (fixes the "stuck at full" bug).
            //   • ANALOG inputs (stick cardinals / triggers) drive the swipe axes,
            //     routed by orientation: horizontal cardinals → swipe_x, vertical
            //     → swipe_y. Both directions of an axis cover both halves (e.g.
            //     left_stick_left AND left_stick_right → full −1..+1 on X).
            //   • A mapping with buttons + analog: the buttons gate (finger down
            //     while held, even centered) and the analog drives the position.
            //   • Analog-only: deflection both activates and positions.
            let has_touch_mappings = mappings.iter().any(mapping_targets_touch);
            if has_touch_mappings {
                let mut fingers: Vec<(f32, f32)> = Vec::new();
                for m in &mappings {
                    if fingers.len() >= 2 { break; }
                    if !mapping_targets_touch(m) { continue; }
                    let out_pins: Vec<&str> = m.get("out").and_then(|v| v.as_array())
                        .map(|a| a.iter().filter_map(|v| v.as_str()).collect()).unwrap_or_default();
                    let in_pins: Vec<&str> = m.get("in").and_then(|v| v.as_array())
                        .map(|a| a.iter().filter_map(|v| v.as_str()).collect()).unwrap_or_default();

                    // Evaluate inputs by role (buttons gate, analog drives axes).
                    let ev = eval_touch_combo(&in_pins, &upstream);
                    if !ev.active { continue; }

                    let mut fx = 0.0f32;
                    let mut fy = 0.0f32;
                    for p in &out_pins {
                        match touchpad_out_kind(p) {
                            Some(TouchOutKind::Zone(zx)) => { fx = zx; }
                            Some(TouchOutKind::SwipeX) => { fx += ev.axis_x; }
                            Some(TouchOutKind::SwipeY) => { fy += ev.axis_y; }
                            None => {}
                        }
                    }
                    fingers.push((fx.clamp(-1.0, 1.0), fy.clamp(-1.0, 1.0)));
                }
                publish_touch_points(&key, &fingers, collector_sigs);
            }
}

/// Evaluate a Touch Zones node in MAPPING mode — shared by the top-level and
/// sub-patch loops. Resolves each active finger to its zone (per field), then
/// applies every mapping card, publishing bus overrides into
/// `collector_sigs[("touchmap:{uid}", pin)]` (mirrors [`eval_remapper_node`]).
///
/// Card schema (node.params["zone_maps"], array of objects):
///   { "f": field, "z": zone, "behavior": "button"|"analog"|..., ... }
///   button → { "src": "touch"|"click", "out": [bus_pin, …] }
///   analog → { "out_stick": "left_stick"|"right_stick" }  (absolute: zone-local
///            X/Y → axis pair, +Y = up)
/// Stateful gestures (tap / double-tap / hold / swipe) are handled by a later
/// pass; only `button` and `analog` are wired here.
fn eval_touch_zones_map_node(
    snap: &NodeSnap,
    uid: usize,
    dev_sigs: &HashMap<(String, String), Signal>,
    collector_sigs: &mut HashMap<(String, String), Signal>,
    state: &mut HashMap<usize, NodeState>,
    dt: f32,
) {
    use flexinput_core::touchzones as tz;
    let dev_id = snap.params.get("_automap_device_id").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let collector_id = snap.params.get("_automap_collector_id").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let key = format!("touchmap:{}", uid);

    // Snapshot every canonical upstream pin once (collector override first, else
    // raw device) into an owned map, so the publish pass can mutate collector_sigs
    // without aliasing the read side. Mirrors eval_remapper_node's `upstream`.
    let mut upstream: HashMap<String, Signal> = HashMap::new();
    for ap in automap::ALL_PINS {
        let sig = if !collector_id.is_empty() {
            collector_sigs.get(&(collector_id.clone(), ap.id.to_string())).copied()
        } else { None }
        .or_else(|| {
            if !dev_id.is_empty() {
                dev_sigs.get(&(dev_id.clone(), ap.id.to_string())).copied()
            } else { None }
        });
        if let Some(s) = sig { upstream.insert(ap.id.to_string(), s); }
    }
    let read = |pin: &str| -> Option<Signal> { upstream.get(pin).copied() };
    let read_edges = |field: usize, which: &str| -> Vec<f32> {
        let k = if field == 0 { which.to_string() } else { format!("{which}{field}") };
        snap.params.get(&k).and_then(|v| v.as_array())
            .map(|a| a.iter().filter_map(|x| x.as_f64().map(|f| f as f32)).collect())
            .unwrap_or_default()
    };

    // Resolve which zone each active finger occupies, per field, keeping local
    // coords — identical to the ports-mode arm in compute_node.
    let split = snap.params.get("field_mode").and_then(|v| v.as_str()) == Some("split");
    const SLOTS_PER: usize = 9; // per-finger aux slots (see per-finger loop below)
    // Zones the user marked "hold": once a gesture STARTS in one, that finger
    // stays attributed to it for the whole touch even if it slides into a
    // neighbour — so the neighbour doesn't also fire ("hold zone" option). Only
    // the `zone_hit` gate (tz_touch / tz_click) needs this; analog + swipe are
    // already attributed to the start zone.
    let hold_zones: std::collections::HashSet<(usize, usize)> =
        snap.params.get("hold_zones").and_then(|v| v.as_array()).map(|a| {
            a.iter().filter_map(|p| {
                let q = p.as_array()?;
                Some((q.first()?.as_u64()? as usize, q.get(1)?.as_u64()? as usize))
            }).collect()
        }).unwrap_or_default();
    // Read-only peek at last frame's per-finger tracking (start zone lives in
    // aux_f32[base+4]); absent on the first frame → no holds yet.
    let prev_aux: Vec<f32> = state.get(&uid).map(|s| s.aux_f32.clone()).unwrap_or_default();
    // Zone geometry: an explicit BSP tree (`zone_tree`/`zone_tree{field}`) once the
    // user has added partial dividers, else derived from the legacy grid (lossless
    // migration — leaf ids == the old row-major indices, so cards keep binding).
    let field_tree = |field: usize| -> tz::ZoneNode {
        let key = if field == 0 { "zone_tree".to_string() } else { format!("zone_tree{field}") };
        snap.params.get(&key).and_then(tz::ZoneNode::from_value)
            .unwrap_or_else(|| tz::ZoneNode::from_grid(
                &read_edges(field, "col_edges"), &read_edges(field, "row_edges")))
    };
    let trees = [field_tree(0), field_tree(1)];
    let mut zone_hit: HashMap<(usize, usize), (f32, f32)> = HashMap::new();
    for finger in 0..2usize {
        let (px, py, pa) = [("touch1_x", "touch1_y", "touch1_active"),
                            ("touch2_x", "touch2_y", "touch2_active")][finger];
        let field = if split { finger } else { 0 };
        if !read(pa).map(|s| s.as_bool()).unwrap_or(false) { continue; }
        let (x, y) = tz::pad_point_to_unit(
            read(px).map(|s| s.as_float()).unwrap_or(0.0),
            read(py).map(|s| s.as_float()).unwrap_or(0.0),
        );
        let (idx, lx, ly) = { let (i, lx, ly) = trees[field].locate(x, y); (i as usize, lx, ly) };
        // If this finger was already down and its START zone is a hold zone, lock
        // the hit to that start zone; the wandered-into zone gets no hit from it.
        let base = finger * SLOTS_PER;
        let prev_active = prev_aux.get(base).copied().unwrap_or(0.0) > 0.5;
        let start_zone = prev_aux.get(base + 4).copied().unwrap_or(0.0) as usize;
        let eff = if prev_active && hold_zones.contains(&(field, start_zone)) {
            start_zone
        } else { idx };
        zone_hit.insert((field, eff), (lx, ly));
    }
    let click = |field: usize| -> bool {
        let pin = if field == 0 { "btn_touchpad" } else { "btn_touchpad2" };
        read(pin).map(|s| s.as_bool()).unwrap_or(false)
    };

    // ── Apply mapping cards ───────────────────────────────────────────────
    let cards = snap.params.get("zone_maps").and_then(|v| v.as_array()).cloned().unwrap_or_default();
    // Button out pins: OR every card targeting the same pin so two zones can
    // share a button. `button_pins` tracks the full set for the release pass.
    let mut button_on: HashMap<String, bool> = HashMap::new();
    // Relative analog (adaptive-center): stick target → (x, y). Last card wins.
    let mut sticks: HashMap<&'static str, (f32, f32)> = HashMap::new();
    // Relative mouse delta accumulator. The `mouse`/`mouse_x`/`mouse_y` pins use
    // the +Y-UP convention (the keymouse sink negates y to screen space itself),
    // so we accumulate the deflection directly WITHOUT flipping y.
    let mut mouse_dx = 0.0f32;
    let mut mouse_dy = 0.0f32;
    let mut mouse_active = false;
    // Analog scroll rate from a zone deflection (+Y up, +X right). Published as
    // the Float scroll_y/scroll_x pins; the KB/M sink integrates them over time.
    let mut scroll_vx = 0.0f32;
    let mut scroll_vy = 0.0f32;
    let mut scroll_active = false;
    // Mouse gain. The emitted value stacks with the SINK's own mouse_sensitivity
    // (like gyro / right-stick sources do), so a raw ±1 deflection would be wildly
    // hot at typical sink sensitivities. `TZ_MOUSE_BASE` attenuates a full-zone
    // deflection to a firm-but-controlled velocity comparable to gyro/RS at the
    // same sink sensitivity; the per-node `mouse_speed` multiplier (default 1.0)
    // tunes it from there.
    const TZ_MOUSE_BASE: f32 = 0.03;
    let mouse_speed = snap.params.get("mouse_speed").and_then(|v| v.as_f64()).unwrap_or(1.0) as f32;
    let mouse_gain = mouse_speed * TZ_MOUSE_BASE;
    // Analog scroll shares the same node multiplier so the "Relative sensitivity"
    // slider also scales max scroll speed. The sink applies the per-notch base
    // rate (SCROLL_REF), so here we pass the shaped deflection × the multiplier.

    // Cards use the shared Remapper schema: "in" = trigger token(s), "out" =
    // target bus pins, "mode"/"window_ms"/"sustain"/"turbo" = the Remapper press
    // pipeline. Per card: derive a raw gate from the zone trigger (touch/click),
    // run it through the SAME `apply_press_mode` (+`apply_turbo`) the Remapper
    // uses, then assert the target — button gets the shaped gate, a stick target
    // is driven with the absolute zone-local position while active.
    let ns = state.entry(uid).or_insert_with(NodeState::default);

    // ── Per-finger tracking: swipe detection + relative analog ─────────────
    // Track each finger (touch1/touch2) across frames. On touch-down record its
    // start field/zone/position AND an ADAPTIVE CENTER: if the finger lands in the
    // inner 30% of the zone, that landing point is the center (relative from where
    // you touched); otherwise the zone's geometric center is used. While held we
    // (a) latch a swipe direction once displacement passes a threshold (attributed
    // to the START zone), and (b) emit a relative analog deflection = (current −
    // center) / zone-half-extent, clamped to ±1. 9 aux_f32 slots per finger:
    // [active, sx, sy, field, zone, dir, pulse_ms, cx, cy].
    const SWIPE_THRESH: f32 = 0.18;   // fraction of the field
    const SWIPE_PULSE_MS: f32 = 120.0;
    // Per-zone "adaptive centre" inner fraction (0..1): the central region within
    // which a touchdown becomes the RELATIVE centre. 0 = always the zone centre
    // (absolute deflection across the whole zone); 1 = wherever you land is the
    // centre (fully relative). Stored on the zone's analog card ("adaptive"),
    // edited below the response-curve graph. Default 0.30.
    let adaptive_for = |field: usize, zone: usize| -> f32 {
        cards.iter().filter(|c|
            c.get("f").and_then(|v| v.as_u64()).unwrap_or(0) == field as u64 &&
            c.get("z").and_then(|v| v.as_u64()).unwrap_or(0) == zone as u64)
            .find_map(|c| c.get("adaptive").and_then(|v| v.as_f64()))
            .map(|v| (v as f32).clamp(0.0, 1.0)).unwrap_or(0.30)
    };
    let slots_per = SLOTS_PER;
    while ns.aux_f32.len() < 2 * slots_per { ns.aux_f32.push(0.0); }
    let mut swipes: Vec<(usize, usize, u8)> = Vec::new(); // (field, zone, dir 1=U 2=D 3=L 4=R)
    let mut analog_by_zone: HashMap<(usize, usize), (f32, f32)> = HashMap::new(); // deflection, +Y up
    for finger in 0..2 {
        let (px, py, pa) = [("touch1_x", "touch1_y", "touch1_active"),
                            ("touch2_x", "touch2_y", "touch2_active")][finger];
        let field = if split { finger } else { 0 };
        let base = finger * slots_per;
        let active = read(pa).map(|s| s.as_bool()).unwrap_or(false);
        let prev_active = ns.aux_f32[base] > 0.5;
        if active {
            let (ux, uy) = tz::pad_point_to_unit(
                read(px).map(|s| s.as_float()).unwrap_or(0.0),
                read(py).map(|s| s.as_float()).unwrap_or(0.0));
            if !prev_active {
                let (zid, _, _) = trees[field].locate(ux, uy);
                let zidx = zid as usize;
                let [x0, y0, x1, y1] = trees[field].zone_rect(zid).unwrap_or([0.0, 0.0, 1.0, 1.0]);
                let (zcx, zcy) = ((x0 + x1) * 0.5, (y0 + y1) * 0.5);
                let (hw, hh) = ((x1 - x0) * 0.5, (y1 - y0) * 0.5);
                // Adaptive centre: landing inside the (configurable) inner region
                // → centre = landing (relative); otherwise the zone's centre.
                let inner = adaptive_for(field, zidx);
                let (cx, cy) = if (ux - zcx).abs() <= inner * hw && (uy - zcy).abs() <= inner * hh {
                    (ux, uy)
                } else { (zcx, zcy) };
                ns.aux_f32[base + 1] = ux;
                ns.aux_f32[base + 2] = uy;
                ns.aux_f32[base + 3] = field as f32;
                ns.aux_f32[base + 4] = zidx as f32;
                ns.aux_f32[base + 5] = 0.0;
                ns.aux_f32[base + 6] = 0.0;
                ns.aux_f32[base + 7] = cx;
                ns.aux_f32[base + 8] = cy;
            } else if ns.aux_f32[base + 5] < 0.5 {
                let dx = ux - ns.aux_f32[base + 1];
                let dy = uy - ns.aux_f32[base + 2];
                if dx.abs().max(dy.abs()) > SWIPE_THRESH {
                    // Field space is y-down, so an upward swipe has dy < 0.
                    let dir: u8 = if dx.abs() >= dy.abs() {
                        if dx > 0.0 { 4 } else { 3 }
                    } else if dy < 0.0 { 1 } else { 2 };
                    ns.aux_f32[base + 5] = dir as f32;
                    ns.aux_f32[base + 6] = SWIPE_PULSE_MS;
                }
            }
            ns.aux_f32[base] = 1.0;

            // Relative analog deflection from the adaptive centre, scaled by the
            // START zone's half-extent (so a half-zone move = full deflection).
            let sz = ns.aux_f32[base + 4] as usize;
            let (cx, cy) = (ns.aux_f32[base + 7], ns.aux_f32[base + 8]);
            let [x0, y0, x1, y1] = trees[field].zone_rect(sz as u32).unwrap_or([0.0, 0.0, 1.0, 1.0]);
            let hw = ((x1 - x0) * 0.5).max(1e-3);
            let hh = ((y1 - y0) * 0.5).max(1e-3);
            let ax = ((ux - cx) / hw).clamp(-1.0, 1.0);
            let ay = (-(uy - cy) / hh).clamp(-1.0, 1.0); // +Y up
            analog_by_zone.insert((field, sz), (ax, ay));
        } else {
            ns.aux_f32[base] = 0.0;
        }
        if ns.aux_f32[base + 6] > 0.0 {
            swipes.push((ns.aux_f32[base + 3] as usize,
                         ns.aux_f32[base + 4] as usize,
                         ns.aux_f32[base + 5] as u8));
            ns.aux_f32[base + 6] = (ns.aux_f32[base + 6] - dt * 1000.0).max(0.0);
        }
    }

    for (i, card) in cards.iter().enumerate() {
        let field = card.get("f").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
        let zone = card.get("z").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
        let hit = zone_hit.get(&(field, zone)).copied();
        let trigger = card.get("in").and_then(|v| v.as_array())
            .and_then(|a| a.first()).and_then(|v| v.as_str()).unwrap_or("tz_touch");
        let swipe_code: Option<u8> = match trigger {
            "tz_swipe_up" => Some(1), "tz_swipe_down" => Some(2),
            "tz_swipe_left" => Some(3), "tz_swipe_right" => Some(4),
            _ => None,
        };
        let raw_held = match swipe_code {
            Some(code) => swipes.iter().any(|&(f, z, d)| f == field && z == zone && d == code),
            None => match trigger {
                "tz_click" => hit.is_some() && click(field),
                _          => hit.is_some(), // tz_touch (default)
            },
        };

        let mode_s = card.get("mode").and_then(|v| v.as_str()).unwrap_or("down");
        let mode = PressMode::from_str(mode_s);
        let window_ms = card.get("window_ms").and_then(|v| v.as_f64()).unwrap_or(200.0) as f32;
        let sustain = card.get("sustain").and_then(|v| v.as_bool()).unwrap_or(false);
        let turbo = card.get("turbo").and_then(|v| v.as_bool()).unwrap_or(false);
        let slots = press_state_get(ns, i);
        let held = apply_press_mode(raw_held, mode, window_ms, sustain, slots, dt);
        let held = if turbo { apply_turbo(held, window_ms, slots, dt) } else { held };

        // Relative analog deflection for this card's zone (present only while a
        // finger is down in it). Analog outputs ignore the press-mode gate — the
        // contact itself drives them. A per-card response `curve` (points over the
        // 0..1 deflection MAGNITUDE) reshapes the response while keeping direction
        // — the touch-zone analog can't have a Response Curve module wired onto it.
        let curve_pts: Vec<[f32; 2]> = card.get("curve").and_then(|v| v.as_array())
            .map(|a| a.iter().filter_map(|p| {
                let q = p.as_array()?;
                Some([q.first()?.as_f64()? as f32, q.get(1)?.as_f64()? as f32])
            }).collect())
            .unwrap_or_default();
        let deflect = analog_by_zone.get(&(field, zone)).copied().map(|(ax, ay)| {
            if curve_pts.len() >= 2 {
                let mag = (ax * ax + ay * ay).sqrt().min(1.0);
                if mag > 1e-4 {
                    let m2 = sample_curve(&curve_pts, mag, &[]).clamp(0.0, 1.0);
                    let s = m2 / mag;
                    (ax * s, ay * s)
                } else { (ax, ay) }
            } else { (ax, ay) }
        });
        for p in card.get("out").and_then(|v| v.as_array()).into_iter().flatten()
            .filter_map(|v| v.as_str())
        {
            match p {
                "left_stick" | "right_stick" => {
                    if let Some((ax, ay)) = deflect {
                        sticks.insert(if p == "left_stick" { "left_stick" } else { "right_stick" }, (ax, ay));
                    }
                }
                // Relative mouse: deflection → velocity, +Y up (the sink flips to
                // screen). "mouse" drives both axes.
                "mouse" | "mouse_x" | "mouse_y" => {
                    if let Some((ax, ay)) = deflect {
                        if p == "mouse" || p == "mouse_x" { mouse_dx += ax * mouse_gain; }
                        if p == "mouse" || p == "mouse_y" { mouse_dy += ay * mouse_gain; }
                        mouse_active = true;
                    }
                }
                // Analog scroll: the (curve-shaped) deflection IS the scroll rate.
                // +Y up, +X right; the sink applies its own per-notch scaling.
                "scroll_x" | "scroll_y" => {
                    if let Some((ax, ay)) = deflect {
                        if p == "scroll_x" { scroll_vx += ax * mouse_speed; }
                        if p == "scroll_y" { scroll_vy += ay * mouse_speed; }
                        scroll_active = true;
                    }
                }
                _ => {
                    // Macro-port target: the shaped gate drives the Bool
                    // aspect; the zone's (curve-shaped) relative deflection
                    // publishes the Vec2 aspect for Vec2/Float ports. Macro
                    // pins never enter `button_on` — they aren't bus pins.
                    if is_macro_style_target(p) {
                        if held {
                            merge_macro_scalar(collector_sigs, p, Signal::Bool(true));
                        }
                        if let Some((ax, ay)) = deflect {
                            merge_macro_vec2(collector_sigs, p, Vec2::new(ax, ay));
                        }
                        continue;
                    }
                    let e = button_on.entry(p.to_string()).or_insert(false);
                    *e = *e || held;
                }
            }
        }
    }

    // Publish button pins. We OWN each targeted pin: assert true when any card
    // is active, else write the released value only if upstream doesn't already
    // emit it (matches the Remapper release rule so passthrough stays intact).
    for (pin, on) in &button_on {
        let sig_type = automap::ALL_PINS.iter()
            .find(|ap| ap.id == pin.as_str())
            .map(|ap| ap.signal_type).unwrap_or(SignalType::Bool);
        if *on {
            let sig = match sig_type {
                SignalType::Float => Signal::Float(1.0),
                SignalType::Int   => Signal::Int(1),
                SignalType::Vec2  => continue,
                _                 => Signal::Bool(true),
            };
            collector_sigs.insert((key.clone(), pin.clone()), sig);
        } else {
            // Upstream already carries this pin (e.g. a real gamepad button) →
            // leave it to passthrough instead of forcing a released value.
            if read(pin).is_some() { continue; }
            let sig = match sig_type {
                SignalType::Float => Signal::Float(0.0),
                SignalType::Int   => Signal::Int(0),
                SignalType::Vec2  => continue,
                _                 => Signal::Bool(false),
            };
            collector_sigs.insert((key.clone(), pin.clone()), sig);
        }
    }

    // Publish analog sticks (Vec2 authoritative + component floats). Only when a
    // finger is in the zone this frame; absent, the pin falls back to upstream so
    // the physical stick still passes through.
    for (target, (x, y)) in &sticks {
        let (xp, yp) = match *target {
            "left_stick" => ("left_stick_x", "left_stick_y"),
            _            => ("right_stick_x", "right_stick_y"),
        };
        collector_sigs.insert((key.clone(), target.to_string()), Signal::Vec2(Vec2::new(*x, *y)));
        collector_sigs.insert((key.clone(), xp.to_string()), Signal::Float(*x));
        collector_sigs.insert((key.clone(), yp.to_string()), Signal::Float(*y));
    }
    // Publish relative mouse delta (Vec2 authoritative + component floats) while
    // a finger drives it. Absent, the pins fall back to upstream.
    if mouse_active {
        collector_sigs.insert((key.clone(), "mouse".to_string()), Signal::Vec2(Vec2::new(mouse_dx, mouse_dy)));
        collector_sigs.insert((key.clone(), "mouse_x".to_string()), Signal::Float(mouse_dx));
        collector_sigs.insert((key.clone(), "mouse_y".to_string()), Signal::Float(mouse_dy));
    }
    // Publish analog scroll rate while a finger drives it; else fall back upstream.
    if scroll_active {
        collector_sigs.insert((key.clone(), "scroll_x".to_string()), Signal::Float(scroll_vx));
        collector_sigs.insert((key.clone(), "scroll_y".to_string()), Signal::Float(scroll_vy));
    }
}

/// Evaluate a Virtual Menu node — shared by the top-level and sub-patch loops.
/// Runs the open → hover → select state machine, fires mapping-mode cards,
/// publishes overrides + pointer suppression under `menumap:{uid}`, and
/// returns the typed outputs (Open / Hover + ports-mode zone pins) for wired
/// consumers and the UI mirror (the body's zone-live highlight).
///
/// Control resolution — macro-style targets first, wired pins as alternates:
///   Show    = ("macro", menu:{menu_id}_show) OR wired Show (slot 1)
///   Select  = ("macro", menu:{menu_id}_sel)  OR wired Select (slot 2)
///   Pointer = wired Pointer Vec2 (slot 3) when connected, else the SUM of
///             every enabled source checkbox (`ptr_ls`/`ptr_rs`/`ptr_touch`/
///             `ptr_gyro`): stick deflection past the deadzone maps onto the
///             menu rect (full deflection = rect edge), a touch point adds
///             its absolute pad position as a centered deflection, and gyro
///             integrates rotation rate while open (Pitch+Yaw or Pitch+Roll
///             pairs, matching the 3DOF→2D module, scaled by
///             `ptr_gyro_sens` where 1 ≈ 10×). By default the hover is
///             STICKY — a pointer inside the deadzone (or a lifted finger)
///             keeps the last highlighted zone so flick-and-release selection
///             works; `hover_sticky: false` clears the highlight instead.
fn eval_menu_node(
    snap: &NodeSnap,
    uid: usize,
    inputs: &[Option<Signal>],
    dev_sigs: &HashMap<(String, String), Signal>,
    collector_sigs: &mut HashMap<(String, String), Signal>,
    state: &mut HashMap<usize, NodeState>,
    dt: f32,
) -> Vec<Option<Signal>> {
    use flexinput_core::menu as fm;
    use flexinput_core::touchzones as tz;
    let dev_id = snap.params.get("_automap_device_id").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let collector_id = snap.params.get("_automap_collector_id").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let key = format!("menumap:{}", uid);

    // Upstream snapshot (collector override first, else raw device), owned so
    // the publish pass below can mutate collector_sigs freely.
    let mut upstream: HashMap<String, Signal> = HashMap::new();
    for ap in automap::ALL_PINS {
        let sig = if !collector_id.is_empty() {
            collector_sigs.get(&(collector_id.clone(), ap.id.to_string())).copied()
        } else { None }
        .or_else(|| {
            if !dev_id.is_empty() {
                dev_sigs.get(&(dev_id.clone(), ap.id.to_string())).copied()
            } else { None }
        });
        if let Some(s) = sig { upstream.insert(ap.id.to_string(), s); }
    }
    // Navigation reads: the source-block zeroed the menu's pointer pins in
    // `dev_sigs` (so nothing ELSE in the patch sees them), but the menu is the
    // reason they're blocked and must still steer from them — so it reads the
    // pre-block snapshot, falling back to the (unblocked) bus for everything else.
    let unblocked_nav: HashMap<(String, String), Signal> = state.get(&MACRO_CARRY_UID)
        .map(|s| s.unblocked_src.clone()).unwrap_or_default();
    let read_nav = |pin: &str| -> Option<Signal> {
        unblocked_nav.get(&(dev_id.clone(), pin.to_string())).copied()
            .or_else(|| upstream.get(pin).copied())
    };

    let pstr = |k: &str, d: &'static str| -> String {
        snap.params.get(k).and_then(|v| v.as_str()).unwrap_or(d).to_string()
    };
    let menu_id = pstr("menu_id", "");
    let act = pstr("activation_mode", "hold");
    let sel_on = pstr("select_on", "release");
    let deadzone = snap.params.get("pointer_deadzone").and_then(|v| v.as_f64()).unwrap_or(0.25) as f32;
    let suppress = snap.params.get("suppress_while_open").and_then(|v| v.as_bool()).unwrap_or(true);
    // Suppression scope: "full" blocks every enabled driver while the menu is
    // open; "partial" blocks a driver only while it's actually being used (past
    // its deadzone / tilting / touching), so an idle enabled driver still reaches
    // the game; "latch" gives the FIRST driver to engage exclusive ownership —
    // it alone steers and is blocked, every other driver passes through
    // untouched until the owner disengages (back to deadzone / finger up /
    // gyro cursor re-centred).
    let sup_mode = pstr("suppress_mode", "partial");
    let suppress_full = sup_mode == "full";
    let suppress_latch = sup_mode == "latch";
    // Sticky hover: keep the last highlighted zone when the pointer returns
    // to the deadzone (flick-and-release selection). Off = the highlight
    // clears, and a release inside the deadzone selects nothing.
    let sticky = snap.params.get("hover_sticky").and_then(|v| v.as_bool()).unwrap_or(true);
    // Pointer sources are ADDITIVE checkboxes (any combination sums into one
    // deflection vector). The legacy single-choice `pointer_source` seeds the
    // defaults so pre-existing patches keep their behaviour.
    let legacy_src = pstr("pointer_source", "left_stick");
    let pbp = |k: &str, d: bool| snap.params.get(k).and_then(|v| v.as_bool()).unwrap_or(d);
    let src_ls = pbp("ptr_ls", legacy_src == "left_stick");
    let src_rs = pbp("ptr_rs", legacy_src == "right_stick");
    let src_touch = pbp("ptr_touch", legacy_src == "touch1" || legacy_src == "touch2");
    let touch_which = pstr("ptr_touch_which",
        if legacy_src == "touch2" { "touch2" } else { "touch1" });
    let src_gyro = pbp("ptr_gyro", false);
    let gyro_axes = pstr("ptr_gyro_axes", "pitch_yaw");
    // Gyro pointer sensitivity: 1 ≈ 10× the raw rotation rate (the raw IMU
    // stream alone is too slow to sweep the menu). Range 0.5..8, default 4.
    let gyro_sens = snap.params.get("ptr_gyro_sens").and_then(|v| v.as_f64()).unwrap_or(4.0) as f32;

    // ── Controls (macro-style targets OR wired pins) ──
    // A macro target resolves to this tick's published value first, else the
    // previous tick's carry-over snapshot. The snapshot is what makes a Select /
    // Show mapping work when the menu sits UPSTREAM of the Remapper that targets
    // it: that Remapper is forced to evaluate AFTER the menu this tick (a
    // feedback cycle), so `collector_sigs` doesn't hold the value yet — one tick
    // stale is imperceptible at kHz. Captured before the mutable `state.entry`
    // below so the immutable borrow ends first (NLL).
    let macro_prev = state.get(&MACRO_CARRY_UID).map(|s| &s.macro_prev);
    let macro_on = |cs: &HashMap<(String, String), Signal>, pin: String| -> bool {
        let key = (flexinput_core::macros::SIGS_NS.to_string(), pin);
        cs.get(&key).map(|s| s.as_bool()).unwrap_or(false)
            || macro_prev.and_then(|m| m.get(&key)).map(|s| s.as_bool()).unwrap_or(false)
    };
    let show_raw = inputs.get(1).and_then(|s| *s).map(|s| s.as_bool()).unwrap_or(false)
        || (!menu_id.is_empty()
            && macro_on(collector_sigs, fm::target_pin(&menu_id, fm::TargetPin::Show)));
    let sel_raw = inputs.get(2).and_then(|s| *s).map(|s| s.as_bool()).unwrap_or(false)
        || (!menu_id.is_empty()
            && macro_on(collector_sigs, fm::target_pin(&menu_id, fm::TargetPin::Select)));
    let wired_ptr: Option<Vec2> = inputs.get(3).and_then(|s| *s)
        .and_then(|s| if let Signal::Vec2(v) = s { Some(v) } else { None });

    // ── State machine slots — created BEFORE the pointer resolves because
    // the gyro source integrates into per-node accumulators and needs
    // prev_open. aux_f32: [0] open, [1] prev_show, [2] prev_sel,
    // [3] hover+1 (0 = none), [4] select-pulse ms left, [5] selected zone+1,
    // [6] prev_click, [7] hover local x, [8] hover local y,
    // [9] selection sequence (increments on each accepted selection — the
    //     overlay's linger animation keys off changes, so it can't miss a
    //     short pulse at low overlay FPS),
    // [10]/[11] gyro pointer accumulator X/Y (integrated rad, reset while
    //     closed so the pointer always starts centered),
    // [12] latch-mode owner (0 = none, 1 = LS, 2 = RS, 3 = touch, 4 = gyro) ──
    const SLOTS: usize = 13;
    const SELECT_PULSE_MS: f32 = 120.0;
    let ns = state.entry(uid).or_insert_with(NodeState::default);
    while ns.aux_f32.len() < SLOTS { ns.aux_f32.push(0.0); }
    let prev_open = ns.aux_f32[0] > 0.5;
    let prev_show = ns.aux_f32[1] > 0.5;
    let prev_sel = ns.aux_f32[2] > 0.5;
    let prev_hover: i32 = ns.aux_f32[3] as i32 - 1;
    let prev_click = ns.aux_f32[6] > 0.5;
    // The touchpad click doubles as the Select gesture, and it may itself be a
    // blocked pin — read it unblocked.
    let click_now = read_nav("btn_touchpad").map(|s| s.as_bool()).unwrap_or(false);

    // ── Pointer → unit point in the menu rect (0..1, y down) + a "touching"
    // gate for activation_mode = touch. Enabled sources SUM into one
    // deflection vector (stick convention, +Y up); the wired Pointer inlet
    // overrides them all. ──
    let stick_read = |name: &str| -> Vec2 {
        if let Some(Signal::Vec2(v)) = read_nav(name) { return v; }
        Vec2::new(
            read_nav(&format!("{name}_x")).map(|s| s.as_float()).unwrap_or(0.0),
            read_nav(&format!("{name}_y")).map(|s| s.as_float()).unwrap_or(0.0),
        )
    };
    // Deflection vector (+Y up) → unit point: full deflection = rect edge.
    let deflect_to_unit = |v: Vec2| -> (f32, f32) {
        ((0.5 + v.x * 0.5).clamp(0.0, 1.0), (0.5 - v.y * 0.5).clamp(0.0, 1.0))
    };
    // Accumulated gyro tilt of this many radians = full deflection.
    const GYRO_FULL_RAD: f32 = 0.35;
    // Gyro rate (post-noise-floor) above this counts as "actively used" for
    // partial suppression — the source-block noise floor already zeros rest.
    const GYRO_ACTIVE_RATE: f32 = 0.05;
    // Per-source "actively used" flags (past deadzone / touching / tilting) —
    // partial suppression blocks only the sources that are actually steering.
    let mut ls_active = false;
    let mut rs_active = false;
    let mut touch_active_now = false;
    let mut gyro_active = false;
    // Latch-mode owner this tick (0 = none, 1 = LS, 2 = RS, 3 = touch,
    // 4 = gyro) — read by the suppression block below.
    let mut latched: u8 = 0;
    let (ptr_unit, touching): (Option<(f32, f32)>, bool) = if let Some(v) = wired_ptr {
        ns.aux_f32[12] = 0.0;
        let on = v.length() > deadzone;
        (if on { Some(deflect_to_unit(v)) } else { None }, on)
    } else {
        // Per-source candidate vectors, summed (or latch-selected) below.
        let mut touch_on = false;
        let ls_vec = if src_ls { stick_read("left_stick") } else { Vec2::ZERO };
        ls_active = src_ls && ls_vec.length() > deadzone;
        let rs_vec = if src_rs { stick_read("right_stick") } else { Vec2::ZERO };
        rs_active = src_rs && rs_vec.length() > deadzone;
        let mut touch_vec = Vec2::ZERO;
        if src_touch {
            let (px, py, pa) = if touch_which == "touch2" {
                ("touch2_x", "touch2_y", "touch2_active")
            } else {
                ("touch1_x", "touch1_y", "touch1_active")
            };
            if read_nav(pa).map(|s| s.as_bool()).unwrap_or(false) {
                touch_on = true;
                touch_active_now = true;
                let (ux, uy) = tz::pad_point_to_unit(
                    read_nav(px).map(|s| s.as_float()).unwrap_or(0.0),
                    read_nav(py).map(|s| s.as_float()).unwrap_or(0.0),
                );
                // Absolute pad position → centered deflection: a lone touch
                // source reproduces the old absolute mapping exactly.
                touch_vec = Vec2::new((ux - 0.5) * 2.0, (0.5 - uy) * 2.0);
            }
        }
        // Gyro rate is read BEFORE the latch decision — the decision needs the
        // gyro "engaged" signal, and integration below is gated on ownership.
        // Axis pairs mirror the 3DOF→2D module: X ← yaw (gz) or roll (gx),
        // Y ← pitch (gy).
        let (g_rate, g_delta) = if src_gyro && prev_open {
            let gx = read_nav("gyro_x").map(|s| s.as_float()).unwrap_or(0.0);
            let gy = read_nav("gyro_y").map(|s| s.as_float()).unwrap_or(0.0);
            let gz = read_nav("gyro_z").map(|s| s.as_float()).unwrap_or(0.0);
            let (dx, dy) = if gyro_axes == "pitch_roll" { (gx, gy) } else { (gz, gy) };
            ((gx * gx + gy * gy + gz * gz).sqrt(), Vec2::new(dx, dy))
        } else {
            (0.0, Vec2::ZERO)
        };
        let gyro_engaged = src_gyro && prev_open
            && (Vec2::new(ns.aux_f32[10], ns.aux_f32[11]).length() > deadzone
                || g_rate > GYRO_ACTIVE_RATE);

        // Latch mode: the FIRST driver to engage owns the menu — it alone
        // steers and gets blocked; the others are ignored here and keep
        // passing to the game until the owner disengages (stick back inside
        // the deadzone / finger up / gyro cursor re-centred), at which point
        // the next engaged driver can take over.
        if suppress_latch {
            latched = if prev_open { ns.aux_f32[12] as u8 } else { 0 };
            let engaged = [false, ls_active, rs_active, touch_active_now, gyro_engaged];
            if latched != 0 && !engaged[latched as usize] { latched = 0; }
            if latched == 0 && prev_open {
                latched = engaged.iter().position(|&e| e).map(|i| i as u8).unwrap_or(0);
            }
        }
        ns.aux_f32[12] = latched as f32;

        // Integrate rotation rate while gyro steers (tilt to point) — in latch
        // mode only while it owns the latch; closed / ignored resets so the
        // pointer starts centered whenever gyro (re)takes control.
        if src_gyro && prev_open && (!suppress_latch || latched == 4) {
            let gain = gyro_sens * 10.0 / GYRO_FULL_RAD;
            ns.aux_f32[10] = (ns.aux_f32[10] + g_delta.x * dt * gain).clamp(-1.5, 1.5);
            ns.aux_f32[11] = (ns.aux_f32[11] + g_delta.y * dt * gain).clamp(-1.5, 1.5);
            // "Actively used" latches off the gyro CURSOR being out of the
            // deadzone, not the rotation rate alone: the accumulator is an
            // integrator, so its deflection persists while the user holds
            // on a target even though the rate drops to ~0 — a rate-only
            // flag flickers there and leaks single ticks of gyro to e.g. a
            // mouse mapping between block requests. The rate term only
            // covers the first few ms of a tilt, before the cursor crosses
            // the deadzone.
            gyro_active = Vec2::new(ns.aux_f32[10], ns.aux_f32[11]).length() > deadzone
                || g_rate > GYRO_ACTIVE_RATE;
        } else {
            ns.aux_f32[10] = 0.0;
            ns.aux_f32[11] = 0.0;
        }
        let gyro_vec = Vec2::new(ns.aux_f32[10], ns.aux_f32[11]);

        let mut v = if suppress_latch {
            match latched {
                1 => ls_vec,
                2 => rs_vec,
                3 => touch_vec,
                4 => gyro_vec,
                _ => Vec2::ZERO,
            }
        } else {
            ls_vec + rs_vec + touch_vec + gyro_vec
        };
        if v.length() > 1.0 { v = v.normalize(); }
        let past_dz = v.length() > deadzone;
        (
            if past_dz || touch_on { Some(deflect_to_unit(v)) } else { None },
            past_dz || touch_on,
        )
    };

    // Zone geometry: explicit BSP tree once partial dividers exist, else the
    // legacy grid (identical to Touch Zones, single field).
    let read_edges = |which: &str| -> Vec<f32> {
        snap.params.get(which).and_then(|v| v.as_array())
            .map(|a| a.iter().filter_map(|x| x.as_f64().map(|f| f as f32)).collect())
            .unwrap_or_default()
    };
    let tree = snap.params.get("zone_tree").and_then(tz::ZoneNode::from_value)
        .unwrap_or_else(|| tz::ZoneNode::from_grid(
            &read_edges("col_edges"), &read_edges("row_edges")));
    // Radial mode: the SAME zone tree projected into polar space — x is the
    // angle (clockwise from 12 o'clock), y the radius past the dead center.
    // Columns are sectors, rows are concentric rings; ids and dividers are
    // shared with grid mode. The dead center — below `pointer_deadzone` of
    // the unit radius — hovers nothing, so a stick can rest without
    // committing.
    let radial = snap.params.get("menu_radial").and_then(|v| v.as_bool()).unwrap_or(false);
    // Angular origin offset (fraction, clockwise): the display rotates the
    // ring by this, so the input mapping must subtract it back out — pushing
    // toward a zone's on-screen direction has to select THAT zone.
    let radial_origin = snap.params.get("menu_radial_origin").and_then(|v| v.as_f64()).unwrap_or(0.0) as f32;

    let open = match act.as_str() {
        "toggle" => prev_open ^ (show_raw && !prev_show),
        "touch" => touching,
        _ => show_raw, // hold
    };

    // Hover: sticky while open (default) — the frame the menu closes still
    // sees the last hover (release-select reads it), then it resets to none.
    // With `hover_sticky` off, a pointer back inside the deadzone clears the
    // highlight instead, so releasing there selects nothing.
    let mut hover: i32 = if prev_open { prev_hover } else { -1 };
    let mut hover_local = (ns.aux_f32[7], ns.aux_f32[8]);
    if open {
        match ptr_unit {
            Some((ux, uy)) => {
                if radial {
                    // Centered vector (unit-rect coords → ±1, +y down); ptr_unit
                    // is already deadzone-gated for sticks, but touch pointers
                    // aren't — gate the dead center here for both.
                    let (cx, cy) = ((ux - 0.5) * 2.0, (uy - 0.5) * 2.0);
                    let (au, mag) = fm::radial_unit(cx, cy);
                    if mag > deadzone {
                        // Radius past the hub → tree y; angle (minus the origin
                        // offset) → tree x. Local coords come from the zone
                        // itself, exactly like grid mode: X across the zone's
                        // arc, Y across its ring band.
                        let rv = ((mag - deadzone) / (1.0 - deadzone).max(1e-3)).clamp(0.0, 0.999);
                        let ax = (au - radial_origin).rem_euclid(1.0).min(0.9999);
                        let (zid, lx, ly) = tree.locate(ax, rv);
                        hover = zid as i32;
                        hover_local = (lx, ly);
                    } else if !sticky {
                        hover = -1;
                    }
                } else {
                    let (zid, lx, ly) = tree.locate(ux, uy);
                    hover = zid as i32;
                    hover_local = (lx, ly);
                }
            }
            None => {
                if !sticky { hover = -1; }
            }
        }
    }

    let select_now = hover >= 0 && match sel_on.as_str() {
        "press" => open && sel_raw && !prev_sel,
        "click" => open && click_now && !prev_click,
        _ => prev_open && !open, // release: the closing edge selects
    };
    if select_now {
        ns.aux_f32[4] = SELECT_PULSE_MS;
        ns.aux_f32[5] = (hover + 1) as f32;
        ns.aux_f32[9] += 1.0;
    }
    let pulse_on = ns.aux_f32[4] > 0.0;
    let selected: i32 = ns.aux_f32[5] as i32 - 1;
    ns.aux_f32[4] = (ns.aux_f32[4] - dt * 1000.0).max(0.0);
    if !open { hover = -1; }

    ns.aux_f32[0] = if open { 1.0 } else { 0.0 };
    ns.aux_f32[1] = if show_raw { 1.0 } else { 0.0 };
    ns.aux_f32[2] = if sel_raw { 1.0 } else { 0.0 };
    ns.aux_f32[3] = (hover + 1) as f32;
    ns.aux_f32[6] = if click_now { 1.0 } else { 0.0 };
    ns.aux_f32[7] = hover_local.0;
    ns.aux_f32[8] = hover_local.1;

    // ── Mapping-mode cards (shared Remapper card schema; trigger tokens
    // "menu_sel" = the select pulse of this card's zone, "menu_hover" = held
    // while the zone is highlighted) ──
    let mapping = pstr("zone_mode", "mapping") == "mapping";
    let cards = snap.params.get("zone_maps").and_then(|v| v.as_array()).cloned().unwrap_or_default();
    let mut button_on: HashMap<String, bool> = HashMap::new();
    if mapping {
        for (i, card) in cards.iter().enumerate() {
            let zone = card.get("z").and_then(|v| v.as_u64()).unwrap_or(0) as i32;
            let trigger = card.get("in").and_then(|v| v.as_array())
                .and_then(|a| a.first()).and_then(|v| v.as_str()).unwrap_or("menu_sel");
            let raw_held = match trigger {
                "menu_hover" => open && hover == zone,
                _ => pulse_on && selected == zone, // menu_sel (default)
            };
            let mode = PressMode::from_str(card.get("mode").and_then(|v| v.as_str()).unwrap_or("down"));
            let window_ms = card.get("window_ms").and_then(|v| v.as_f64()).unwrap_or(200.0) as f32;
            let sustain = card.get("sustain").and_then(|v| v.as_bool()).unwrap_or(false);
            let turbo = card.get("turbo").and_then(|v| v.as_bool()).unwrap_or(false);
            let slots = press_state_get(ns, i);
            let held = apply_press_mode(raw_held, mode, window_ms, sustain, slots, dt);
            let held = if turbo { apply_turbo(held, window_ms, slots, dt) } else { held };
            for p in card.get("out").and_then(|v| v.as_array()).into_iter().flatten()
                .filter_map(|v| v.as_str())
            {
                // Macro-style targets (macro ports, OTHER menus) route into
                // the macro namespace; self-targeting is blocked in the UI.
                if is_macro_style_target(p) {
                    if held {
                        merge_macro_scalar(collector_sigs, p, Signal::Bool(true));
                    }
                    continue;
                }
                let e = button_on.entry(p.to_string()).or_insert(false);
                *e = *e || held;
            }
        }
    }

    // Republish the FULL upstream bus under `menumap:{uid}` — a passthrough,
    // exactly like Touch Zones' `touchmap:{uid}`. Previously only card overrides
    // and suppression zeros were written, leaving `menumap:` a SPARSE map: the
    // AutoMap output port had no bus to glow from, and downstream consumers had
    // to fall back to the raw device for every un-overridden pin (so suppression
    // leaked). Card overrides + suppression below overwrite specific pins on top
    // of this complete bus.
    for (pin, sig) in &upstream {
        collector_sigs.insert((key.clone(), pin.clone()), *sig);
    }

    // Card button pins. While the card is active, assert the pin. When the card
    // is INACTIVE we must still drive the pin to OFF unless the passthrough bus
    // already carries it: the select pulse is momentary, and a virtual sink
    // LATCHES the last value for any pin it stops receiving — so a card output
    // the source device never emits (not in `upstream`) would stick "pressed"
    // forever after one selection. A pin that IS on the bus keeps its passthrough
    // value (OR semantics — a real press of the same button still comes through).
    for (pin, on) in &button_on {
        let sig_type = automap::ALL_PINS.iter()
            .find(|ap| ap.id == pin.as_str())
            .map(|ap| ap.signal_type).unwrap_or(SignalType::Bool);
        if *on {
            let sig = match sig_type {
                SignalType::Float => Signal::Float(1.0),
                SignalType::Int   => Signal::Int(1),
                SignalType::Vec2  => continue,
                _                 => Signal::Bool(true),
            };
            collector_sigs.insert((key.clone(), pin.clone()), sig);
        } else if !upstream.contains_key(pin) {
            let off = match sig_type {
                SignalType::Float => Signal::Float(0.0),
                SignalType::Int   => Signal::Int(0),
                SignalType::Vec2  => continue,
                _                 => Signal::Bool(false),
            };
            collector_sigs.insert((key.clone(), pin.clone()), off);
        }
    }

    // ── Suppress the pointing inputs steering the menu ──
    //
    // Two layers. (1) A SOURCE-BLOCK request keyed by the physical source device
    // — applied to `dev_sigs` at the start of NEXT tick so the blocked pins reach
    // ONLY the menu's navigation, not a mouse mapping, another module, or any
    // sink (the menu reads the pre-block snapshot to keep steering). (2) Zeroing
    // the same pins on the menu's OWN passthrough now, so a downstream module on
    // the menu's route doesn't react this tick before the 1-tick source-block
    // lands. `suppress_full` blocks every enabled driver; "latch" blocks ONLY
    // the driver currently owning the menu (the others pass untouched);
    // otherwise (partial) only the drivers actually being used (past deadzone /
    // touching / tilting) are blocked, so an idle enabled driver still reaches
    // the game.
    if suppress && open && wired_ptr.is_none() {
        let (block_ls, block_rs, block_touch, block_gyro) = if suppress_latch {
            (latched == 1, latched == 2, latched == 3, latched == 4)
        } else {
            (src_ls    && (suppress_full || ls_active),
             src_rs    && (suppress_full || rs_active),
             src_touch && (suppress_full || touch_active_now),
             src_gyro  && (suppress_full || gyro_active))
        };

        // (2) Zero the blocked pins on our own passthrough bus.
        for (on, name) in [(block_ls, "left_stick"), (block_rs, "right_stick")] {
            if !on { continue; }
            collector_sigs.insert((key.clone(), name.to_string()), Signal::Vec2(Vec2::ZERO));
            collector_sigs.insert((key.clone(), format!("{name}_x")), Signal::Float(0.0));
            collector_sigs.insert((key.clone(), format!("{name}_y")), Signal::Float(0.0));
        }
        if block_touch {
            collector_sigs.insert((key.clone(), format!("{touch_which}_active")), Signal::Bool(false));
            collector_sigs.insert((key.clone(), format!("{touch_which}_x")), Signal::Float(0.0));
            collector_sigs.insert((key.clone(), format!("{touch_which}_y")), Signal::Float(0.0));
            collector_sigs.insert((key.clone(), "btn_touchpad".to_string()), Signal::Bool(false));
        }
        if block_gyro {
            for pin in ["gyro_x", "gyro_y", "gyro_z"] {
                collector_sigs.insert((key.clone(), pin.to_string()), Signal::Float(0.0));
            }
        }

        // (1) Publish the SOURCE-BLOCK request (drained into NodeState::source_block
        // at tick end, applied to dev_sigs next tick).
        if !dev_id.is_empty() {
            let bk = format!("{SRC_BLOCK_PREFIX}{dev_id}");
            let mut blocked: Vec<String> = Vec::new();
            if block_ls { for p in ["left_stick", "left_stick_x", "left_stick_y"] { blocked.push(p.to_string()); } }
            if block_rs { for p in ["right_stick", "right_stick_x", "right_stick_y"] { blocked.push(p.to_string()); } }
            if block_touch {
                blocked.push(format!("{touch_which}_active"));
                blocked.push(format!("{touch_which}_x"));
                blocked.push(format!("{touch_which}_y"));
                blocked.push("btn_touchpad".to_string());
            }
            if block_gyro { for p in ["gyro_x", "gyro_y", "gyro_z"] { blocked.push(p.to_string()); } }
            for p in blocked {
                collector_sigs.insert((bk.clone(), p), Signal::Bool(true));
            }
        }

        // Re-derive stick cardinals from the (now zeroed) axes so a suppressed
        // stick can't leak through synthetic left_stick_up/down/... pins, which
        // a pass-through Collector would otherwise copy verbatim from the bus.
        let mut local: HashMap<String, Signal> = HashMap::new();
        for axis in ["left_stick_x", "left_stick_y", "right_stick_x", "right_stick_y"] {
            if let Some(&sig) = collector_sigs.get(&(key.clone(), axis.to_string())) {
                local.insert(axis.to_string(), sig);
            }
        }
        derive_stick_cardinals(&mut local);
        for (k, v) in local {
            if k.contains("_stick_") && (k.ends_with("_up") || k.ends_with("_down")
                || k.ends_with("_left") || k.ends_with("_right"))
            {
                collector_sigs.insert((key.clone(), k), v);
            }
        }
    }

    // ── Typed outputs: fixed Open/Hover + ports-mode zone pins (TZ vocabulary,
    // field 0). X/Y carry the hovered zone's local pointer coords. ──
    let mut out: Vec<Option<Signal>> = (0..snap.n_outputs).map(|i| {
        let pin_id = snap.output_pin_ids.get(i).map(|s| s.as_str()).unwrap_or("");
        match fm::parse_pin(pin_id) {
            Some(fm::Pin::Open) => return Some(Signal::Bool(open)),
            Some(fm::Pin::Hover) => return Some(Signal::Float(hover as f32)),
            _ => {}
        }
        match tz::parse_pin(pin_id)? {
            tz::Pin::Zone { idx, comp: tz::ZoneComp::Active, .. } =>
                Some(Signal::Bool(open && hover == idx as i32)),
            tz::Pin::Zone { idx, comp: tz::ZoneComp::X, .. } =>
                Some(Signal::Float(if hover == idx as i32 { hover_local.0 } else { 0.0 })),
            tz::Pin::Zone { idx, comp: tz::ZoneComp::Y, .. } =>
                Some(Signal::Float(if hover == idx as i32 { hover_local.1 } else { 0.0 })),
            tz::Pin::Click { .. } => Some(Signal::Bool(pulse_on)),
        }
    }).collect();
    // TWO extra trailing slots beyond the real ports (invisible to the port
    // UI, carried by the last_out mirror; the UI reads them from the END so
    // the count of real ports never matters):
    //   [len-2] last selection as Vec2(zone id, selection seq) — None until
    //           the first selection ever; the overlay lingers the selected
    //           cell when it sees the seq change (`menu_sel_info`).
    //   [len-1] the live pointer as a unit-rect Vec2 (0..1, y down) while the
    //           menu is open — the overlay / body fields draw the
    //           cursor-deflection indicator from it (`menu_pointer`). None
    //           when closed or centered.
    let sel_seq = ns.aux_f32[9];
    let sel_zone: i32 = ns.aux_f32[5] as i32 - 1;
    out.push(if sel_seq > 0.0 && sel_zone >= 0 {
        Some(Signal::Vec2(Vec2::new(sel_zone as f32, sel_seq)))
    } else {
        None
    });
    out.push(match (open, ptr_unit) {
        (true, Some((ux, uy))) => Some(Signal::Vec2(Vec2::new(ux, uy))),
        _ => None,
    });
    out
}

/// Evaluate a Map Action node — shared by the top-level and sub-patch loops.
/// Returns the 2-element output vec [Bool gate, Float analog]. `uid` is the
/// publishing id (snap.node_uid at top level, namespaced uid in a sub-patch);
/// it keys the per-node `state`.
fn eval_map_action_node(
    snap: &NodeSnap,
    uid: usize,
    dev_sigs: &HashMap<(String, String), Signal>,
    collector_sigs: &HashMap<(String, String), Signal>,
    state: &mut HashMap<usize, NodeState>,
    dt: f32,
) -> Vec<Option<Signal>> {
            let dev_id = snap.params.get("_automap_device_id").and_then(|v| v.as_str()).unwrap_or("");
            let collector_id = snap.params.get("_automap_collector_id").and_then(|v| v.as_str()).unwrap_or("");
            let mappings = snap.params.get("mappings").and_then(|v| v.as_array()).cloned().unwrap_or_default();
            // Snapshot upstream values for every canonical pin once.
            let mut upstream: HashMap<String, Signal> = HashMap::new();
            for ap in automap::ALL_PINS {
                let sig = if !collector_id.is_empty() {
                    collector_sigs.get(&(collector_id.to_string(), ap.id.to_string())).copied()
                } else { None }
                .or_else(|| {
                    if !dev_id.is_empty() {
                        dev_sigs.get(&(dev_id.to_string(), ap.id.to_string())).copied()
                    } else { None }
                });
                if let Some(s) = sig { upstream.insert(ap.id.to_string(), s); }
            }
            // A processed Vec2 on the collector is authoritative over raw axes.
            vec2_authoritative_axis_fill(&mut upstream, collector_id, &collector_sigs);
            // Derive synthetic pins (stick cardinals + touchpad variants)
            derive_stick_cardinals(&mut upstream);
            // Touchpad handling mirrors Remapper's behaviour (click accumulation)
            let touch_click = upstream.get("btn_touchpad").map(|s| s.as_bool()).unwrap_or(false);
            let zone_of_x = |x: f32| -> usize {
                if x < -1.0/3.0 { 0 } else if x > 1.0/3.0 { 2 } else { 1 }
            };
            let mut touch_only = [false; 3];
            for (xpin, apin) in [("touch1_x","touch1_active"), ("touch2_x","touch2_active")] {
                let active = upstream.get(apin).map(|s| s.as_bool()).unwrap_or(false);
                if !active { continue; }
                let x = upstream.get(xpin).map(|s| sig_scalar(*s)).unwrap_or(0.0);
                touch_only[zone_of_x(x)] = true;
            }
            // Click-variant zones are stored in per-node state; reuse NodeState aux_f32
            let ns = state.entry(uid).or_insert_with(NodeState::default);
            if ns.aux_f32.len() < 3 { ns.aux_f32.resize(3, 0.0); }
            if !touch_click {
                ns.aux_f32[0] = 0.0; ns.aux_f32[1] = 0.0; ns.aux_f32[2] = 0.0;
            } else {
                for (xpin, apin) in [("touch1_x","touch1_active"), ("touch2_x","touch2_active")] {
                    let active = upstream.get(apin).map(|s| s.as_bool()).unwrap_or(false);
                    if !active { continue; }
                    let x = upstream.get(xpin).map(|s| sig_scalar(*s)).unwrap_or(0.0);
                    ns.aux_f32[zone_of_x(x)] = 1.0;
                }
            }
            let click_zone = [ ns.aux_f32[0] > 0.5, ns.aux_f32[1] > 0.5, ns.aux_f32[2] > 0.5 ];
            let any_zone = click_zone[0] || click_zone[1] || click_zone[2];
            if touch_click { touch_only = [false;3]; }
            upstream.insert("touchpad_left".to_string(),   Signal::Bool(click_zone[0]));
            upstream.insert("touchpad_center".to_string(), Signal::Bool(click_zone[1]));
            upstream.insert("touchpad_right".to_string(),  Signal::Bool(click_zone[2]));
            upstream.insert("touchpad_any".to_string(),    Signal::Bool(touch_click && any_zone));
            upstream.insert("touch_left".to_string(),      Signal::Bool(touch_only[0]));
            upstream.insert("touch_center".to_string(),    Signal::Bool(touch_only[1]));
            upstream.insert("touch_right".to_string(),     Signal::Bool(touch_only[2]));

            let read_upstream = |pin_id: &str| -> Option<Signal> { upstream.get(pin_id).copied() };

            // Mappings may be in legacy Array<String> form (chord only, mode=down)
            // or in the new Object form `{ in, mode, window_ms, sustain }`.
            //
            // Output signal kind depends on which mode(s) are present:
            //   - All-digital mappings → emit Bool ("any active").
            //   - Any analog mapping present → emit Float (max magnitude
            //     across all active analog mappings, falling back to 1.0
            //     when only a digital mapping is active so digital triggers
            //     still drive Float-consuming wires at full deflection).
            let ns_map = state.entry(uid).or_insert_with(NodeState::default);
            let mut any_trigger = false;
            let mut any_analog_present = false;
            let mut max_analog_mag: f32 = 0.0;
            for (i, m) in mappings.iter().enumerate() {
                let (in_pins, mode_s, window_ms, sustain, turbo) = if let Some(arr) = m.as_array() {
                    let pins: Vec<&str> = arr.iter().filter_map(|v| v.as_str()).collect();
                    (pins, "down", 200.0_f32, false, false)
                } else {
                    let pins: Vec<&str> = m.get("in").and_then(|v| v.as_array())
                        .map(|a| a.iter().filter_map(|v| v.as_str()).collect())
                        .unwrap_or_default();
                    let mode = m.get("mode").and_then(|v| v.as_str()).unwrap_or("down");
                    let win  = m.get("window_ms").and_then(|v| v.as_f64()).unwrap_or(200.0) as f32;
                    let sus  = m.get("sustain").and_then(|v| v.as_bool()).unwrap_or(false);
                    let tur  = m.get("turbo").and_then(|v| v.as_bool()).unwrap_or(false);
                    (pins, mode, win, sus, tur)
                };
                if in_pins.is_empty() { continue; }
                if mode_s == "analog" {
                    any_analog_present = true;
                    // Combo gate (same as Remapper): all non-cardinal pins
                    // held AND any cardinal contributing a non-zero mag.
                    // Track the strongest cardinal magnitude for Float out.
                    let mut has_cardinal = false;
                    let mut any_cardinal_active = false;
                    let mut all_buttons_held = true;
                    let mut local_max: f32 = 0.0;
                    for p in &in_pins {
                        if analog_axis_for_cardinal(p).is_some() {
                            has_cardinal = true;
                            let mag = analog_cardinal_input_value(&upstream, p);
                            if mag > 0.0 { any_cardinal_active = true; }
                            if mag > local_max { local_max = mag; }
                        } else if !read_upstream(p).map(|s| s.as_bool()).unwrap_or(false) {
                            all_buttons_held = false;
                        }
                    }
                    let active = all_buttons_held && (!has_cardinal || any_cardinal_active);
                    // For pure-button analog (no cardinal), magnitude defaults
                    // to 1.0 while gated so the Float output reads full.
                    let mag = if !active {
                        0.0
                    } else if has_cardinal { local_max } else { 1.0 };
                    // out_analog: pure magnitude (max across active mappings).
                    if mag > max_analog_mag { max_analog_mag = mag; }
                    // out (Bool): freq-modulated tap train / PWM (Hold) / ×2
                    // (Turbo) driven by the magnitude, so a digital destination
                    // reflects how far the input is pushed.
                    let slots = press_state_get(ns_map, i);
                    if analog_digital_pulse(mag, window_ms, sustain, turbo, slots, dt) {
                        any_trigger = true;
                    }
                    continue;
                }
                // All-cardinal chords on a single stick can't be
                // simultaneously held — use the gesture-visited bitmap so
                // half-circles and full sweeps complete the combo. Mirrors
                // Remapper's digital path.
                let raw_held = if let Some(required) = gesture_required_bits(&in_pins) {
                    let buttons_held = in_pins.iter().all(|p| {
                        if gesture_pin_to_bit(p).is_some() { return true; }
                        read_upstream(p).map(|s| s.as_bool()).unwrap_or(false)
                    });
                    let visited = gesture_state_get(ns_map, i);
                    buttons_held && gesture_tick(required, visited, &upstream)
                } else {
                    in_pins.iter().all(|p| {
                        read_upstream(p).map(|s| s.as_bool()).unwrap_or(false)
                    })
                };
                let mode = PressMode::from_str(mode_s);
                let slots = press_state_get(ns_map, i);
                let held = apply_press_mode(raw_held, mode, window_ms, sustain, slots, dt);
                let held = if turbo { apply_turbo(held, window_ms, slots, dt) } else { held };
                if held { any_trigger = true; }
            }

            // Two outputs: out (Bool gate/tap-train) + out_analog (Float mag).
            // out_analog falls back to 1.0 when only digital mappings drove the
            // gate so a Float-consuming wire still sees full deflection.
            let analog_mag = if max_analog_mag > 0.0 {
                max_analog_mag
            } else if any_trigger && !any_analog_present { 1.0 } else { max_analog_mag };
            return vec![
                Some(Signal::Bool(any_trigger)),
                Some(Signal::Float(analog_mag.clamp(0.0, 1.0))),
            ];
}

/// Shared Remapper pass-through + suppression pass, called identically by the
/// top-level and sub-patch Remapper arms (so the two never diverge). For every
/// canonical pin it writes `collector_sigs[(key, pin)]`:
///   - consumed input pins → explicit off
///   - claimed cardinals → per-side axis/Vec2 clamp + Bool off (sticks + D-pad)
///   - unmapped pins → raw pass-through
/// Then recomputes synthetic stick cardinals from the clamped axes and publishes
/// the consumed-pin markers for downstream Combiner hierarchy suppression.
fn remapper_pass_through_and_suppress(
    key: &str,
    upstream: &HashMap<String, Signal>,
    claimed_digital: &HashSet<String>,
    claimed_analog: &HashSet<String>,
    collector_sigs: &mut HashMap<(String, String), Signal>,
) {
    let mut all_claimed: HashSet<String> = claimed_digital.clone();
    all_claimed.extend(claimed_analog.iter().cloned());
    let suppression = cardinal_suppression(&all_claimed);

    for ap in automap::ALL_PINS {
        let raw = if all_claimed.contains(ap.id) {
            None
        } else {
            upstream.get(ap.id).copied()
        };
        let Some(raw) = raw else {
            if all_claimed.contains(ap.id) {
                let off = match ap.signal_type {
                    SignalType::Bool  => Signal::Bool(false),
                    SignalType::Float => Signal::Float(0.0),
                    SignalType::Vec2  => Signal::Vec2(Vec2::ZERO),
                    SignalType::Int   => Signal::Int(0),
                    _ => continue,
                };
                collector_sigs.insert((key.to_string(), ap.id.to_string()), off);
            }
            continue;
        };
        let sig = suppress_signal_for_pin(ap.id, raw, &suppression);
        collector_sigs.insert((key.to_string(), ap.id.to_string()), sig);
    }

    // Recompute synthetic stick cardinals from the (possibly clamped) axes so
    // downstream consumers see consistent cardinal bools.
    {
        let mut local_up: HashMap<String, Signal> = HashMap::new();
        for axis in ["left_stick_x", "left_stick_y", "right_stick_x", "right_stick_y"] {
            if let Some(&sig) = collector_sigs.get(&(key.to_string(), axis.to_string())) {
                local_up.insert(axis.to_string(), sig);
            }
        }
        derive_stick_cardinals(&mut local_up);
        for (k, v) in local_up {
            if k.contains("_stick_") && (k.ends_with("_up") || k.ends_with("_down")
                || k.ends_with("_left") || k.ends_with("_right"))
            {
                collector_sigs.insert((key.to_string(), k), v);
            }
        }
    }

    publish_consumed_markers(key, claimed_digital, claimed_analog, collector_sigs);
}

/// Apply per-side `CardinalSuppression` to one pin's raw pass-through value:
///   - axis Float (`dpad_x`, `left_stick_y`, …): clamp the consumed side(s).
///   - bundled Vec2 (`dpad`, `left_stick`, …): clamp each component's side(s).
///   - claimed cardinal Bool: forced false.
///   - everything else: unchanged.
/// Only the directions the user mapped are affected; the rest pass through.
fn suppress_signal_for_pin(
    pin_id: &str,
    raw: Signal,
    sup: &CardinalSuppression,
) -> Signal {
    // Claimed cardinal Bool → off.
    if sup.bool_pins.contains(pin_id) {
        return Signal::Bool(false);
    }
    // Axis Float → side clamp.
    if let Some(&(neg, pos)) = sup.axis_sides.get(pin_id) {
        if let Signal::Float(v) = raw {
            return Signal::Float(apply_axis_clamp(v, (neg, pos)));
        }
        return raw;
    }
    // Bundled Vec2 → per-component side clamp.
    let axes: Option<(&str, &str)> = match pin_id {
        "left_stick"  => Some(("left_stick_x",  "left_stick_y")),
        "right_stick" => Some(("right_stick_x", "right_stick_y")),
        "dpad"        => Some(("dpad_x",         "dpad_y")),
        _ => None,
    };
    if let Some((xa, ya)) = axes {
        let xs = sup.axis_sides.get(xa).copied().unwrap_or((false, false));
        let ys = sup.axis_sides.get(ya).copied().unwrap_or((false, false));
        if xs == (false, false) && ys == (false, false) {
            return raw;
        }
        if let Signal::Vec2(v) = raw {
            return Signal::Vec2(Vec2::new(
                apply_axis_clamp(v.x, xs),
                apply_axis_clamp(v.y, ys),
            ));
        }
    }
    raw
}

/// Map an analog-mode output pin to its one-sided trigger axis, if it is one.
/// Triggers are 0..1 (no negative side), so analog mappings drive them with the
/// input's unsigned magnitude. Returns the trigger pin id, or None for non-trigger
/// outputs (which the caller treats as cardinal axes or buttons).
///
/// The digital trigger buttons (`btn_lt_dig`/`btn_rt_dig`) also map here: a
/// Remapper captures its output by chord-learning, so on a pad whose trigger is
/// a digital button (Switch Pro ZL/ZR) the captured `out` pin is the digital
/// button, not the analog trigger. In ANALOG mode the user's intent is analog
/// travel, so we route the digital-trigger-button target to its analog pin.
fn analog_trigger_out(pin_id: &str) -> Option<&'static str> {
    match pin_id {
        "left_trigger"  | "btn_lt_dig" => Some("left_trigger"),
        "right_trigger" | "btn_rt_dig" => Some("right_trigger"),
        _ => None,
    }
}

/// Return the signed analog magnitude an input cardinal currently contributes
/// to its axis: 0.0 when the stick is neutral or pushed in the opposite
/// direction; up to ±1.0 at full deflection in the cardinal's direction.
/// Used by analog-mode Remapper / Map Action to drive output axes from
/// input cardinals' live magnitudes (no gesture gate).
fn analog_cardinal_input_value(upstream: &HashMap<String, Signal>, pin_id: &str) -> f32 {
    let Some((axis_pin, cardinal_sign)) = analog_axis_for_cardinal(pin_id) else { return 0.0; };
    let axis_val = upstream.get(axis_pin).map(|s| sig_scalar(*s)).unwrap_or(0.0);
    let signed = axis_val * cardinal_sign;
    signed.max(0.0).min(1.0)
}

// ── Per-mapping response curve + activation threshold ────────────────────────
//
// Every mapping card (Remapper `mappings`, Lean `lean_left`/`lean_right`,
// Touch Zones `zone_maps`) may carry:
//   curve:     [[x, y], …] — response curve over the analog input magnitude
//              (0..1 → 0..1). Absent = identity.
//   threshold: f32 0..1 — a HORIZONTAL line on the curve's OUTPUT: a digital
//              binding is held while the shaped magnitude sits on/above it
//              and releases the moment it dips below (manual activation
//              point). Absent = legacy behaviour (derived cardinal bools /
//              0.5 trigger coercion / freq-modulated pulse train).

/// The card's `curve` points, or empty when absent/malformed.
fn mapping_curve_pts(m: &Value) -> Vec<[f32; 2]> {
    m.get("curve").and_then(|v| v.as_array())
        .map(|a| a.iter().filter_map(|p| {
            let q = p.as_array()?;
            Some([q.first()?.as_f64()? as f32, q.get(1)?.as_f64()? as f32])
        }).collect())
        .unwrap_or_default()
}

/// The card's manual activation threshold, when set.
fn mapping_threshold(m: &Value) -> Option<f32> {
    m.get("threshold").and_then(|v| v.as_f64()).map(|v| (v as f32).clamp(0.0, 1.0))
}

/// Shape an input magnitude through a card's curve (identity when no curve).
fn shape_mag(pts: &[[f32; 2]], mag: f32) -> f32 {
    if pts.len() >= 2 {
        sample_curve(pts, mag.clamp(0.0, 1.0), &[]).clamp(0.0, 1.0)
    } else {
        mag
    }
}

/// Live analog INPUT value of a mapping in-pin: a stick cardinal's one-sided
/// deflection or an analog trigger's travel. `None` for digital pins.
fn analog_in_value(upstream: &HashMap<String, Signal>, pin_id: &str) -> Option<f32> {
    if analog_axis_for_cardinal(pin_id).is_some() {
        return Some(analog_cardinal_input_value(upstream, pin_id));
    }
    if matches!(pin_id, "left_trigger" | "right_trigger") {
        return Some(upstream.get(pin_id).map(|s| sig_scalar(*s)).unwrap_or(0.0).clamp(0.0, 1.0));
    }
    None
}

/// True when a pin id is an analog INPUT source — a stick cardinal or an analog
/// trigger. The Remapper/Lean UI uses this to gate analog-only outputs (e.g. the
/// touchpad swipe bindings) so they're only offered once an analog input chord
/// has been captured.
pub fn pin_is_analog_input(pin_id: &str) -> bool {
    analog_axis_for_cardinal(pin_id).is_some()
        || matches!(pin_id, "left_trigger" | "right_trigger")
}

/// Synthetic Remapper/Lean OUTPUT pins that drive the virtual touchpad rather
/// than a canonical sink pin. `touch_left/center/right` place a finger TOUCH at a
/// fixed X zone; `touch_swipe_x/_y` move a finger along an axis by the input's
/// signed analog magnitude (absolute-position model). These are translated into
/// canonical `touch1_*`/`touch2_*` points by [`publish_touch_points`]; the plain
/// `btn_touchpad` (click) and `btn_mute` outputs are canonical and need no
/// translation.
#[derive(Clone, Copy, PartialEq)]
enum TouchOutKind { Zone(f32), SwipeX, SwipeY }

/// Horizontal offset of the left/right touch zones (center = 0). Matches the
/// input-side `zone_of_x` thresholds (±1/3) comfortably.
const TOUCH_ZONE_X: f32 = 0.66;

fn touchpad_out_kind(pin_id: &str) -> Option<TouchOutKind> {
    match pin_id {
        "touch_left"   => Some(TouchOutKind::Zone(-TOUCH_ZONE_X)),
        "touch_center" => Some(TouchOutKind::Zone(0.0)),
        "touch_right"  => Some(TouchOutKind::Zone(TOUCH_ZONE_X)),
        "touch_swipe_x" => Some(TouchOutKind::SwipeX),
        "touch_swipe_y" => Some(TouchOutKind::SwipeY),
        _ => None,
    }
}

/// True when any of a mapping's `out` pins drives the touchpad (zone or swipe).
fn mapping_targets_touch(m: &serde_json::Value) -> bool {
    m.get("out").and_then(|v| v.as_array()).map(|a| a.iter().any(|v|
        v.as_str().map(|s| touchpad_out_kind(s).is_some()).unwrap_or(false)
    )).unwrap_or(false)
}

/// Result of evaluating a touch-output combo's inputs by role.
struct TouchComboEval {
    /// Whether the finger should be down this tick.
    active: bool,
    /// Signed horizontal contribution (sum of `*_x` cardinals + triggers, ±1 range).
    axis_x: f32,
    /// Signed vertical contribution (sum of `*_y` cardinals, ±1 range).
    axis_y: f32,
}

/// Evaluate a touch-output combo's inputs by ROLE — the single source of truth
/// shared by the synthesis pass (positions the finger) and the suppression pass
/// (`held_now`, decides when to consume the combo's inputs from pass-through).
///
/// Inputs split into:
///   • BUTTONS — gate the finger: ALL must be held for it to activate; they
///     contribute no axis value.
///   • ANALOG cardinals / triggers — drive the axes, routed by orientation
///     (`*_x` → axis_x, `*_y` → axis_y; triggers → axis_x). Opposite cardinals
///     of one axis (left+right) sum with their signs to cover both halves.
///
/// Activation: gate buttons held AND (a gate button present → always; else any
/// analog deflected). This must NOT require every cardinal at once — a combo
/// mixing left+right of one axis can never be "simultaneously held", which is
/// exactly why a generic all-held check would never suppress its gate buttons.
fn eval_touch_combo(in_pins: &[&str], upstream: &HashMap<String, Signal>) -> TouchComboEval {
    let mut gate_buttons_held = true;
    let mut has_gate_button = false;
    let mut has_analog = false;
    let mut any_analog_active = false;
    let mut axis_x = 0.0f32;
    let mut axis_y = 0.0f32;
    for ip in in_pins {
        if let Some((axis, sign)) = analog_axis_for_cardinal(ip) {
            has_analog = true;
            let v = analog_cardinal_input_value(upstream, ip); // 0..1
            if v > 0.0 { any_analog_active = true; }
            if axis.ends_with("_x") { axis_x += sign * v; } else { axis_y += sign * v; }
        } else if matches!(*ip, "left_trigger" | "right_trigger") {
            has_analog = true;
            let v = upstream.get(*ip).map(|s| sig_scalar(*s)).unwrap_or(0.0).clamp(0.0, 1.0);
            if v > 0.0 { any_analog_active = true; }
            axis_x += v; // one-sided; drives the positive side
        } else {
            has_gate_button = true;
            if !upstream.get(*ip).map(|s| s.as_bool()).unwrap_or(false) {
                gate_buttons_held = false;
            }
        }
    }
    let active = if !gate_buttons_held {
        false
    } else if has_gate_button {
        true // buttons gate: finger down while held (analog only positions)
    } else if has_analog {
        any_analog_active // analog-only: deflection activates
    } else {
        false
    };
    TouchComboEval { active, axis_x, axis_y }
}

/// Publish up to TWO synthesized touch points (`fingers`, ordered, in -1..1) into
/// `collector_sigs[(key, "touch{1,2}_{x,y,active}")]`. Extra requests beyond the
/// hardware's 2 simultaneous points are dropped. Unused slots publish
/// `*_active = false` so a released synthesized touch doesn't latch on the
/// virtual pad. Callers gate this on the patch actually having touchpad-output
/// mappings, so a patch that never targets the touchpad leaves the pass-through
/// touch pins untouched.
fn publish_touch_points(
    key: &str,
    fingers: &[(f32, f32)],
    collector_sigs: &mut HashMap<(String, String), Signal>,
) {
    for (i, (xk, yk, ak)) in [
        ("touch1_x", "touch1_y", "touch1_active"),
        ("touch2_x", "touch2_y", "touch2_active"),
    ].iter().enumerate() {
        if let Some((x, y)) = fingers.get(i) {
            collector_sigs.insert((key.to_string(), xk.to_string()),
                Signal::Float(x.clamp(-1.0, 1.0)));
            collector_sigs.insert((key.to_string(), yk.to_string()),
                Signal::Float(y.clamp(-1.0, 1.0)));
            collector_sigs.insert((key.to_string(), ak.to_string()), Signal::Bool(true));
        } else {
            collector_sigs.insert((key.to_string(), ak.to_string()), Signal::Bool(false));
        }
    }
}


/// Apply axis-side suppression to a stick axis Float value. `(neg, pos)` —
/// when `neg` is true, clamp negative values to 0; when `pos` is true,
/// clamp positive values to 0.
fn apply_axis_clamp(v: f32, suppress: (bool, bool)) -> f32 {
    let (neg, pos) = suppress;
    let mut out = v;
    if neg && out < 0.0 { out = 0.0; }
    if pos && out > 0.0 { out = 0.0; }
    out
}

/// Shared lean-dispatch for the 3DOF module. Called from both the
/// top-level eval loop and the subgraph eval loop with the appropriate
/// UID (snap.node_uid for top-level, ns_uid for subpatches). Writes to
/// `collector_sigs[("lean:UID", pin_id)]` for every output pin named in
/// any `lean_left` / `lean_right` mapping.
fn lean_dispatch_into_collector_sigs(
    snap: &NodeSnap,
    uid: usize,
    node_outputs: &[Option<Signal>],
    node_state: &mut NodeState,
    collector_sigs: &mut HashMap<(String, String), Signal>,
    dt: f32,
) {
    let lean_val = node_outputs.get(3)
        .and_then(|s| *s)
        .map(|s| s.as_float())
        .unwrap_or(0.0);
    let lean_threshold = snap.params.get("lean_threshold")
        .and_then(|v| v.as_f64()).map(|f| f as f32).unwrap_or(0.3);
    let left_active  = lean_val <= -lean_threshold;
    let right_active = lean_val >=  lean_threshold;
    let lean_mag = lean_val.abs().min(1.0);

    let lean_left  = snap.params.get("lean_left")
        .and_then(|v| v.as_array()).cloned().unwrap_or_default();
    let lean_right = snap.params.get("lean_right")
        .and_then(|v| v.as_array()).cloned().unwrap_or_default();

    // Collect every output pin mentioned in any mapping so released ones
    // can publish false/0. Stick cardinals always also publish to their
    // underlying analog axis (Analog and non-Analog modes both emit on
    // the axis — cardinals aren't valid sink pin ids on their own, so
    // without the axis remap nothing reaches the destination device).
    let mut all_out_pins: HashSet<String> = HashSet::new();
    for m in lean_left.iter().chain(lean_right.iter()) {
        if let Some(arr) = m.get("out").and_then(|v| v.as_array()) {
            for v in arr {
                if let Some(s) = v.as_str() {
                    if touchpad_out_kind(s).is_some() { continue; } // synthesized below
                    // Macro pins skip the bus release pass — absent from the
                    // macro namespace = released.
                    if is_macro_style_target(s) { continue; }
                    all_out_pins.insert(s.to_string());
                    if let Some((axis_pin, _)) = analog_axis_for_cardinal(s) {
                        all_out_pins.insert(axis_pin.to_string());
                    }
                }
            }
        }
    }

    let mut asserted: HashMap<String, Signal> = HashMap::new();

    for (side_idx, side_pair) in [
        (left_active, &lean_left), (right_active, &lean_right),
    ].iter().enumerate() {
        let (active, mappings) = side_pair;
        let base_idx = if side_idx == 0 { 0 } else { lean_left.len() };
        for (i, m) in mappings.iter().enumerate() {
            let out_pins: Vec<String> = m.get("out").and_then(|v| v.as_array())
                .map(|a| a.iter().filter_map(|v| v.as_str().map(|s| s.to_string())).collect())
                .unwrap_or_default();
            if out_pins.is_empty() { continue; }
            let mode_s    = m.get("mode").and_then(|v| v.as_str()).unwrap_or("down");
            let window_ms = m.get("window_ms").and_then(|v| v.as_f64()).unwrap_or(200.0) as f32;
            let sustain   = m.get("sustain").and_then(|v| v.as_bool()).unwrap_or(false);
            let turbo     = m.get("turbo").and_then(|v| v.as_bool()).unwrap_or(false);

            let slots = press_state_get(node_state, base_idx + i);

            // Per-card response curve + manual threshold. The curve reshapes
            // the lean magnitude this card emits; a threshold replaces the
            // NODE-level lean_threshold for THIS card's activation, gating on
            // the curve-shaped OUTPUT (dips below → release). `side_sign_ok`
            // is the raw side test (any magnitude), so a card threshold can
            // sit below the node threshold too.
            let curve = mapping_curve_pts(m);
            let thr = mapping_threshold(m);
            let shaped = shape_mag(&curve, lean_mag);
            let side_sign_ok = if side_idx == 0 { lean_val < 0.0 } else { lean_val > 0.0 };

            let (held_now, analog_val_opt): (bool, Option<f32>) = if mode_s == "analog" {
                let gate = match thr {
                    Some(t) => side_sign_ok && shaped >= t,
                    None => *active && lean_mag >= 0.01,
                };
                if !gate {
                    slots[0] = 0.0;
                    (false, Some(0.0))
                } else {
                    // Manual threshold → plain hold while above the line
                    // (Turbo still taps). Otherwise the shared analog→digital
                    // modulation: Hold → PWM (duty = shaped), Turbo → ×2 max
                    // frequency, plain → tap train whose frequency tracks the
                    // shaped magnitude. Float destinations ignore `pulse_on`
                    // and use the shaped magnitude directly below.
                    let pulse_on = match thr {
                        Some(_) => {
                            if turbo { apply_turbo(true, window_ms, slots, dt) } else { true }
                        }
                        None => analog_digital_pulse(
                            shaped, window_ms, sustain, turbo, slots, dt,
                        ),
                    };
                    (pulse_on, Some(shaped))
                }
            } else {
                let card_active = match thr {
                    Some(t) => side_sign_ok && shaped >= t,
                    None => *active,
                };
                let mode = PressMode::from_str(mode_s);
                let held = apply_press_mode(card_active, mode, window_ms, sustain, slots, dt);
                let held = if turbo { apply_turbo(held, window_ms, slots, dt) } else { held };
                (held, None)
            };

            let is_analog_mode = mode_s == "analog";
            for p in &out_pins {
                // Touchpad zone/swipe outputs are synthesized into touch points
                // after this loop, not emitted as axis/button pins.
                if touchpad_out_kind(p).is_some() { continue; }
                // Macro-port target: Analog mode passes the live lean
                // magnitude (unsigned — the port is bound per-side, so
                // direction is implied by which side's mapping fires); other
                // press modes assert while the shaped gate is open. Macro
                // pins never enter `asserted` — they aren't bus pins.
                if is_macro_style_target(p) {
                    if is_analog_mode {
                        // The activation gate is already encoded upstream:
                        // analog_val_opt is Some(0.0) when the card's gate
                        // (node or per-card threshold) didn't pass.
                        let mag = analog_val_opt.unwrap_or(0.0);
                        if mag > 0.0 {
                            merge_macro_scalar(collector_sigs, p, Signal::Float(mag.min(1.0)));
                        }
                    } else if held_now {
                        merge_macro_scalar(collector_sigs, p, Signal::Bool(true));
                    }
                    continue;
                }
                // Cardinal → analog-axis remap (all press modes):
                // A stick-cardinal like `left_stick_right` represents the
                // user's INTENT to drive that axis in that direction. The
                // cardinal pin id isn't a valid sink pin on any virtual
                // gamepad — the actual emit must go to the underlying
                // axis (left_stick_x / left_stick_y) with the cardinal's
                // sign (right/up = +, left/down = -). In Analog mode the
                // magnitude tracks lean_mag; in other press modes it's a
                // gated full-deflection write (±1.0 when held, 0 when not).
                if let Some((axis_pin, cardinal_sign)) = analog_axis_for_cardinal(p.as_str()) {
                    // Analog mode: analog_val_opt already carries the gated,
                    // curve-shaped magnitude (0.0 when the card's gate —
                    // node or per-card threshold — didn't pass).
                    let mag = if is_analog_mode {
                        analog_val_opt.unwrap_or(1.0)
                    } else if held_now {
                        1.0
                    } else {
                        0.0
                    };
                    if mag > 0.0 {
                        let new_v = cardinal_sign * mag;
                        let sig = Signal::Float(new_v);
                        // Combine if multiple mappings target the same axis
                        // — use the larger-magnitude write (winning sign).
                        asserted
                            .entry(axis_pin.to_string())
                            .and_modify(|existing| {
                                if let Signal::Float(prev) = existing {
                                    if new_v.abs() > prev.abs() {
                                        *existing = Signal::Float(new_v);
                                    }
                                }
                            })
                            .or_insert(sig);
                    }
                    continue;
                }
                let sig_type = automap::ALL_PINS.iter()
                    .find(|x| x.id == p.as_str())
                    .map(|x| x.signal_type).unwrap_or(SignalType::Bool);
                let emit = match (is_analog_mode, sig_type) {
                    // Gate already applied upstream: Some(>0) only while the
                    // card's (node- or threshold-based) activation holds.
                    (true, SignalType::Float) => analog_val_opt.map(|v| v > 0.0).unwrap_or(false),
                    (true, SignalType::Vec2)  => false,
                    (true, _)                 => held_now,
                    (false, _)                => held_now,
                };
                if !emit { continue; }
                let sig = match sig_type {
                    SignalType::Float => {
                        let mag = analog_val_opt.unwrap_or(1.0);
                        let signed = if is_analog_mode {
                            if side_idx == 0 { -mag } else { mag }
                        } else { mag };
                        Signal::Float(signed)
                    }
                    SignalType::Vec2 => continue,
                    SignalType::Int   => Signal::Int(1),
                    _                 => Signal::Bool(true),
                };
                asserted.entry(p.clone()).or_insert(sig);
            }
        }
    }

    let key = format!("lean:{}", uid);
    for p in &all_out_pins {
        let sig_type = automap::ALL_PINS.iter().find(|x| x.id == p.as_str())
            .map(|x| x.signal_type).unwrap_or(SignalType::Bool);
        let sig = asserted.get(p).copied().unwrap_or_else(|| {
            match sig_type {
                SignalType::Float => Signal::Float(0.0),
                SignalType::Vec2  => Signal::Vec2(Vec2::ZERO),
                SignalType::Int   => Signal::Int(0),
                _                 => Signal::Bool(false),
            }
        });
        collector_sigs.insert((key.clone(), p.clone()), sig);
    }

    // ── Touchpad output synthesis (zones + analog swipe) ──────────────────
    // Mirror of the Remapper's pass: if any lean mapping targets a touchpad
    // zone/swipe pin, synthesize up to 2 touch points from the ACTIVE side's
    // mappings (left side = negative X swipe, right side = positive).
    let has_touch_mappings = lean_left.iter().chain(lean_right.iter()).any(|m| {
        m.get("out").and_then(|v| v.as_array()).map(|a| a.iter().any(|v|
            v.as_str().map(|s| touchpad_out_kind(s).is_some()).unwrap_or(false)
        )).unwrap_or(false)
    });
    if has_touch_mappings {
        let mut fingers: Vec<(f32, f32)> = Vec::new();
        'sides: for (side_idx, (active, mappings)) in [
            (left_active, &lean_left), (right_active, &lean_right),
        ].iter().enumerate() {
            if !*active { continue; }
            let swipe_sign = if side_idx == 0 { -1.0 } else { 1.0 };
            for m in *mappings {
                if fingers.len() >= 2 { break 'sides; }
                let out_pins: Vec<String> = m.get("out").and_then(|v| v.as_array())
                    .map(|a| a.iter().filter_map(|v| v.as_str().map(|s| s.to_string())).collect())
                    .unwrap_or_default();
                let mut fx = 0.0f32;
                let mut fy = 0.0f32;
                let mut has = false;
                let mut needs_mag = false;
                for out_p in &out_pins {
                    match touchpad_out_kind(out_p) {
                        Some(TouchOutKind::Zone(zx)) => { fx = zx; has = true; }
                        Some(TouchOutKind::SwipeX) => { fx += swipe_sign * lean_mag; has = true; needs_mag = true; }
                        Some(TouchOutKind::SwipeY) => { fy += swipe_sign * lean_mag; has = true; needs_mag = true; }
                        None => {}
                    }
                }
                if has {
                    if needs_mag && fx.abs() < 1e-3 && fy.abs() < 1e-3 { continue; }
                    fingers.push((fx, fy));
                }
            }
        }
        publish_touch_points(&key, &fingers, collector_sigs);
    }
}

/// Translate legacy `mode` strings to the new (family, axis) split so saved
/// patches keep working without manual migration.
fn gyro_resolve_mode(params: &HashMap<String, Value>) -> (&'static str, &'static str) {
    if let Some(family) = params.get("family").and_then(|v| v.as_str()) {
        let axis = params.get("axis").and_then(|v| v.as_str()).unwrap_or("pitch_yaw");
        let f: &'static str = match family { "steering" => "steering", _ => "pointer" };
        let a: &'static str = match axis {
            "pitch_roll" => "pitch_roll",
            "player"     => "player",
            "world"      => "world",
            _            => "pitch_yaw",
        };
        return (f, a);
    }
    // Legacy fallback: old `mode` string.
    match params.get("mode").and_then(|v| v.as_str()).unwrap_or("local") {
        "player" => ("pointer",  "player"),
        "world"  => ("pointer",  "world"),
        "laser"  => ("steering", "pitch_yaw"),
        _        => ("pointer",  "pitch_yaw"),
    }
}

fn compute_gyro_3dof(
    inputs: &[Option<Signal>],
    state: &mut NodeState,
    params: &HashMap<String, Value>,
    dev_sigs: &HashMap<(String, String), Signal>,
    collector_sigs: &HashMap<(String, String), Signal>,
    dt: f32,
) -> Vec<Option<Signal>> {
    let (family, axis) = gyro_resolve_mode(params);

    let inv = |name: &str| -> f32 {
        if params.get(name).and_then(|v| v.as_bool()).unwrap_or(false) { -1.0 } else { 1.0 }
    };
    let pf = |name: &str, default: f32| -> f32 {
        params.get(name).and_then(|v| v.as_f64()).map(|x| x as f32).unwrap_or(default)
    };
    let pb = |name: &str, default: bool| -> bool {
        params.get(name).and_then(|v| v.as_bool()).unwrap_or(default)
    };

    // Auto-map path: read all six axes from the connected device.
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
    let pin_or = |idx: usize, fallback: f32| -> f32 {
        if inputs.get(idx).and_then(|s| *s).is_some() { get_f(inputs, idx, fallback) } else { fallback }
    };
    let gx = pin_or(2, gx_am) * inv("inv_roll");
    let gy = pin_or(3, gy_am);
    let gz = pin_or(4, gz_am);
    let ax = pin_or(5, ax_am) * inv("inv_accel_x");
    let ay = pin_or(6, ay_am) * inv("inv_accel_y");
    let az = pin_or(7, az_am) * inv("inv_accel_z");
    // (Spike suppression moved to the device polling layer — see
    // `flexinput_devices::gyro::apply_spike_filter`. The engine sees an
    // already-clean IMU stream.)

    // aux_f32 layout:
    //   [0] integrated steering X
    //   [1] integrated steering Y
    //   [2] smoothed gravity X (player/world)
    //   [3] smoothed gravity Y
    //   [4] smoothed gravity Z
    //   [5] prev_reset edge guard
    //   [6] ease-in residual (0..1 progresses while resetting)
    //   [7] quaternion x (orientation integration)
    //   [8] quaternion y
    //   [9] quaternion z
    //   [10] quaternion w (always 1.0 at initialization, updated on each tick)
    //   [11] prev_reset edge guard for reset tracking
    //   [12] ease-in residual for orientation blend during reset
    //   [13..16] captured world-frame gravity reference for drift correction
    //   [16] gyro-still time accumulator for the yaw auto re-center
    while state.aux_f32.len() < 17 { state.aux_f32.push(0.0); }

    // ── Axis selection: decide which gyro components feed X / Y ───────────
    //
    // For Player/World we project gyro onto the gravity-corrected frame.
    // For Pitch+Yaw and Pitch+Roll, the X/Y feed is gyro rates as before.
    //
    // Lean is derived separately below from accel tilt (NOT a gyro rate) so
    // that holding a tilted controller still asserts a steady lean signal
    // and rocking back through center doesn't produce a spurious opposite
    // lean. See the lean derivation block after this match.
    let (raw_x, raw_y, _raw_lean_unused) = match axis {
        "pitch_roll" => (gx, gy, gz),
        "player" | "world" => {
            let gyro  = glam::Vec3::new(gx, gy, gz);
            let accel = glam::Vec3::new(ax, ay, az);
            let tau = if axis == "world" { 3.0_f32 } else { 1.0_f32 };
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
            (world_yaw, gyro_no_yaw.y, 0.0)
        }
        _ => (gz, gy, 0.0), // pitch_yaw: gz=yaw→X, gy=pitch→Y
    };

    // ── Steering integration + auto-recentering ───────────────────────────
    let reset_now = get_b(inputs, 1, false);
    let reset_edge = reset_now && state.aux_f32[5] < 0.5;
    state.aux_f32[5] = if reset_now { 1.0 } else { 0.0 };

    let (out_x, out_y) = if family == "steering" {
        // `exclude_y` suppresses the Y *output* — keeps X integrating as
        // usual, but Y stays at zero. Use when the steering axis is the
        // only thing you want from this module (e.g. a vehicle's wheel).
        let exclude_y = pb("steering_exclude_y", false);
        let recenter_strength = pf("recenter_strength", 0.0).clamp(0.0, 4.0); // sec⁻¹ pull rate
        let ease_in = pf("reset_ease_in", 0.25).clamp(0.0, 2.0);

        // Integrate both accumulators every tick — `exclude_y` only gates
        // the output, not the integration, so toggling it on/off doesn't
        // leave the Y accumulator stale.
        state.aux_f32[0] += raw_x * dt;
        state.aux_f32[1] += raw_y * dt;

        // X recenter — gated by axis (yaw isn't observable when flat).
        //   Pitch+Yaw  : heading = atan2(ay, ax), weight ≈ |sin tilt|
        //   Pitch+Roll : heading = atan2(ay, az), weight ≈ cos pitch
        //   Player/World: skipped (azimuth around gravity unobservable
        //                  from accel alone).
        //
        // Y recenter is intentionally NOT implemented as an independent
        // atan2 — the per-axis approach couples X and Y badly (Y motion
        // → large ax → atan2(ay, ax) whiplash → spurious X drift). The
        // proper fix is to maintain a continuous 3DOF pose estimate and
        // project both axes from it; that rework is pending. Until then,
        // Y centers only via the manual reset (ease_in).
        if recenter_strength > 0.0 && (axis == "pitch_yaw" || axis == "pitch_roll") {
            let acc_mag = (ax * ax + ay * ay + az * az).sqrt().max(1e-3);
            let (heading, weight) = if axis == "pitch_roll" {
                let w = (ay * ay + az * az).sqrt() / acc_mag;
                (ay.atan2(az), w)
            } else {
                // pitch_yaw
                let w = (ax * ax + ay * ay).sqrt() / acc_mag;
                (ay.atan2(ax), w)
            };
            let two_pi = std::f32::consts::TAU;
            let mut delta = heading - state.aux_f32[0];
            // Wrap to (-π, π] without depending on rem_euclid edge cases.
            delta -= two_pi * ((delta / two_pi) + 0.5).floor();
            let alpha = (recenter_strength * weight * dt).clamp(0.0, 1.0);
            state.aux_f32[0] += alpha * delta;
        }

        // Reset edge: start an ease-in toward zero. While ease-in > 0 we
        // blend the steering accumulator toward 0 over `ease_in` seconds.
        if reset_edge { state.aux_f32[6] = 1.0; }
        if state.aux_f32[6] > 0.001 && ease_in > 0.001 {
            let step = (dt / ease_in).clamp(0.0, 1.0);
            state.aux_f32[0] *= 1.0 - step;
            state.aux_f32[1] *= 1.0 - step;
            state.aux_f32[6] = (state.aux_f32[6] - step).max(0.0);
        } else if reset_edge && ease_in <= 0.001 {
            state.aux_f32[0] = 0.0;
            state.aux_f32[1] = 0.0;
            state.aux_f32[6] = 0.0;
        }

        let x_out = state.aux_f32[0];
        let y_out = if exclude_y { 0.0 } else { state.aux_f32[1] };
        (x_out, y_out)
    } else {
        // Pointer family: pass-through angular velocity (or projected component).
        // Reset has no effect — there's no accumulator.
        (raw_x, raw_y)
    };

    // Apply yaw/pitch inversions to final output (NOT inside dot-product math).
    let final_x = out_x * inv("inv_yaw");
    let final_y = out_y * inv("inv_pitch");

    // ── Lean output: tilt fraction from accelerometer ─────────────────────
    //
    // Lean is the controller's signed side-tilt as a fraction of full
    // sideways. Positive = right lean (right grip drops, +ay in FlexInput
    // accel convention). Magnitude in [0, 1] where 1 ≈ on its side.
    //
    // This is derived from accel ONLY, not gyro rate, so:
    //   - Holding a tilted controller produces a STEADY non-zero lean.
    //   - Returning to neutral smoothly ramps back to 0 (no spurious
    //     opposite spike like raw gyro rate would give).
    //
    // For Pitch+Roll / Player / World modes the rotation around gravity
    // is not directly observable from accel; we still use the same side-
    // tilt measure since "is the controller tilted sideways" is the
    // intuitive lean axis regardless of how X/Y are derived.
    let acc_mag_full = (ax * ax + ay * ay + az * az).sqrt().max(1e-3);
    let lean_val = (ay / acc_mag_full).clamp(-1.0, 1.0);
    let lean_threshold = pf("lean_threshold", 0.3).clamp(0.01, 4.0);
    let lean_active = lean_val.abs() >= lean_threshold;

    // ── Quaternion orientation integration (3DOF pose estimate) ───────────
    //
    // Maintains a continuous orientation estimate by integrating angular
    // velocity over time. Uses aux_f32[7..10] for quaternion x,y,z,w.
    // Gravity-based drift correction can be added later if needed.
    let q_reset_now = get_b(inputs, 1, false);
    let q_reset_edge = q_reset_now && state.aux_f32[11] < 0.5;
    state.aux_f32[11] = if q_reset_now { 1.0 } else { 0.0 };

    // Initialize quaternion to identity on first run or reset
    if state.aux_f32[7] == 0.0 && state.aux_f32[8] == 0.0 && state.aux_f32[9] == 0.0 {
        state.aux_f32[7] = 0.0; // qx
        state.aux_f32[8] = 0.0; // qy
        state.aux_f32[9] = 0.0; // qz
        state.aux_f32[10] = 1.0; // qw (identity)
    }

    // Integrate the TRUE physical angular velocity so this orientation tracks
    // the controller 1:1 in the real world — independent of BOTH the pointer/
    // steering sensitivity AND the device's `gyro_multiplier`. Device gyro
    // signals are NORMALIZED: ±1.0 == ±GYRO_REF_DPS deg/s (see
    // crates/devices/src/gyro.rs), so convert to rad/s before integrating, else
    // the model under-rotates by ~34.9×.
    const GYRO_REF_DPS: f32 = 2000.0;
    let norm_to_rad_s = GYRO_REF_DPS * std::f32::consts::PI / 180.0;
    // `gyro_multiplier` (a device.source calibration knob) is already baked into
    // the gyro signals by `preprocess_dev_sigs`; divide it back out so the
    // orientation stays 1:1 no matter what the user sets it to. The multiplier
    // is stashed per-device under the synthetic pin key "__gyro_mult".
    let dev_gyro_mult = params
        .get("_automap_device_id")
        .and_then(|v| v.as_str())
        .and_then(|dev| dev_sigs.get(&(dev.to_string(), "__gyro_mult".to_string())))
        .and_then(|s| if let Signal::Float(f) = s { Some(*f) } else { None })
        .filter(|m| m.abs() > 1e-6)
        .unwrap_or(1.0);
    // Orientation display scale, applied to the RATE (before integration) — the
    // only place scaling a full 3D pose stays continuous. Scaling the finished
    // quaternion (viewer-side) flips discontinuously as the rotation passes
    // ~180°. Affects ONLY this Orientation output, not the 2D pointer/steering.
    // 1.0 = physical for all known controllers (the device layer normalizes
    // every family to the same ±2000 dps reference).
    let orient_disp_scale = pf("orient_scale", 1.0);
    let orient_scale = norm_to_rad_s / dev_gyro_mult * orient_disp_scale;
    // Polarity comes from the module's EXISTING inv_* toggles — the same ones
    // that orient the 2D output — not hardcoded per-device sign guesses. So a
    // device calibrated once (its inv_yaw/inv_pitch/inv_roll) is correct for
    // BOTH the 2D output and this 3D orientation. `gx` already carries inv_roll
    // (applied at read); apply inv_pitch/inv_yaw here to match the 2D output,
    // which negates by `inv("inv_pitch")` / `inv("inv_yaw")` on its Y / X.
    let pitch_rate = gy * inv("inv_pitch");
    let yaw_rate   = gz * inv("inv_yaw");
    let roll_rate  = gx;
    // Device gyro axes (roll, pitch, yaw) → model rotation axes (X=pitch,
    // Y=yaw, Z=roll). Fixed base signs establish the model's handedness; the
    // inv_* toggles above flip per-device as needed. Body-frame angular
    // velocity, composed intrinsically (q_old * dq).
    let gyro_vec = glam::Vec3::new(pitch_rate, -yaw_rate, -roll_rate) * orient_scale;
    let mag = gyro_vec.length();
    if mag > 1e-6 && dt > 0.0 {
        let axis = gyro_vec / mag;
        let angle = mag * dt;
        let rot_q = glam::Quat::from_axis_angle(axis, angle);
        let cur_q = glam::Quat::from_xyzw(
            state.aux_f32[7],
            state.aux_f32[8],
            state.aux_f32[9],
            state.aux_f32[10],
        );
        // Renormalize to shed accumulated floating-point drift over long runs.
        let new_q = (cur_q * rot_q).normalize();
        state.aux_f32[7] = new_q.x;
        state.aux_f32[8] = new_q.y;
        state.aux_f32[9] = new_q.z;
        state.aux_f32[10] = new_q.w;
    }

    // Reset edge: fade quaternion toward identity over ease_in period
    let q_ease_in = pf("reset_ease_in", 0.25).clamp(0.0, 2.0);
    if q_reset_edge { state.aux_f32[12] = 1.0; }
    if state.aux_f32[12] > 0.001 && q_ease_in > 0.001 {
        let step = (dt / q_ease_in).clamp(0.0, 1.0);
        let cur_q = glam::Quat::from_xyzw(
            state.aux_f32[7],
            state.aux_f32[8],
            state.aux_f32[9],
            state.aux_f32[10],
        );
        // Blend toward identity (0,0,0,1)
        let blend_q = cur_q.slerp(glam::Quat::IDENTITY, step);
        state.aux_f32[7] = blend_q.x;
        state.aux_f32[8] = blend_q.y;
        state.aux_f32[9] = blend_q.z;
        state.aux_f32[10] = blend_q.w;
        state.aux_f32[12] = (state.aux_f32[12] - step).max(0.0);
    } else if q_reset_edge && q_ease_in <= 0.001 {
        // Hard reset
        state.aux_f32[7] = 0.0;
        state.aux_f32[8] = 0.0;
        state.aux_f32[9] = 0.0;
        state.aux_f32[10] = 1.0;
        state.aux_f32[12] = 0.0;
    }

    // ── Accel drift correction (complementary filter) ──────────────────────
    //
    // Pure gyro integration accumulates tilt drift. Whenever the controller
    // isn't being shaken, the accelerometer reads gravity — an absolute
    // attitude reference — so nudge the quaternion to keep gravity mapping
    // to a CAPTURED world-frame reference. Capturing (at first steady
    // reading, and re-capturing through a reset) instead of assuming a
    // fixed world "up" means no absolute axis-sign assumptions: whatever
    // pose the user resets in becomes truth, and only tilt drift relative
    // to it is corrected. Yaw (rotation about gravity) is unobservable from
    // accel; the cross-product correction leaves it untouched.
    //
    // Post-inv_* pins are in the canonical device convention, so the accel
    // vector maps into the model body frame with the same fixed axis map
    // the gyro rates use above: dev (x=roll/fwd, y=pitch/side, z=yaw/vert)
    // → model (y, −z, −x).
    // OFF by default: gravity can't distinguish tilt from linear acceleration,
    // so translation (side-to-side / up-down swings) reads as false rotation
    // even behind the steadiness gates. Auto re-center covers rest drift
    // without that failure mode; this stays as an explicit opt-in.
    let drift_corr = pf("orient_drift", 0.0).clamp(0.0, 1.0);
    if drift_corr > 0.0 && dt > 0.0 {
        let a_model = glam::Vec3::new(ay, -az, -ax);
        let acc_len = a_model.length();
        // Accel pins are normalized ±1 == ±8 G; trust the reading only when
        // its magnitude is near 1 g (anything else isn't just gravity).
        const ONE_G: f32 = 1.0 / 8.0;
        if acc_len > 1e-4 {
            // Trust the reading only when BOTH hold:
            //  - |a| ≈ 1 g, TIGHTLY — side-to-side translation adds lateral
            //    acceleration in quadrature, so even a mild shake pushes the
            //    magnitude off 1 g. The old ×4 falloff still corrected at
            //    ~80 % strength during a 0.3 g shake and visibly rotated the
            //    model while it was only being translated.
            //  - the gyro reads near-still — if the pad isn't rotating, a
            //    moving accel vector is translation by definition, and must
            //    never tilt the pose. Drift correction at rest is the whole
            //    point anyway; during motion the gyro integration rules.
            let steady_mag = (1.0 - ((acc_len / ONE_G) - 1.0).abs() * 25.0).clamp(0.0, 1.0);
            let steady_rot = (1.0 - mag / 0.6).clamp(0.0, 1.0); // fades out by ~35°/s
            let steady = steady_mag * steady_rot;
            let u_body = a_model / acc_len;
            let cur_q = glam::Quat::from_xyzw(
                state.aux_f32[7],
                state.aux_f32[8],
                state.aux_f32[9],
                state.aux_f32[10],
            );
            let u_ref = glam::Vec3::new(state.aux_f32[13], state.aux_f32[14], state.aux_f32[15]);
            if u_ref.length_squared() < 0.5 || q_reset_edge || state.aux_f32[12] > 0.001 {
                // First valid reading, reset edge, or mid reset-ease: (re)capture
                // the reference against the current quaternion instead of
                // correcting, so the blend toward identity can't be fought.
                if steady > 0.5 {
                    let w = cur_q * u_body;
                    state.aux_f32[13] = w.x;
                    state.aux_f32[14] = w.y;
                    state.aux_f32[15] = w.z;
                }
            } else if steady > 0.0 {
                let pred = cur_q * u_body; // measured up, world frame
                let err = pred.cross(u_ref); // axis = correction, |err| = sin(angle)
                // Slider → pull rate: 0.25 (default) ≈ τ 2 s, 1.0 ≈ τ 0.125 s.
                let gain = drift_corr * drift_corr * 8.0;
                let step = (gain * steady * dt).min(1.0);
                let new_q = (glam::Quat::from_scaled_axis(err * step) * cur_q).normalize();
                state.aux_f32[7] = new_q.x;
                state.aux_f32[8] = new_q.y;
                state.aux_f32[9] = new_q.z;
                state.aux_f32[10] = new_q.w;
            }
        }
    }

    // ── Auto re-center (Orientation output only) ───────────────────────────
    //
    // With no absolute reference the pose can end up shifted on ANY axis
    // (yaw worst — nothing pins it — but pitch/roll can wander too). When
    // the gyro magnitude stays under the user threshold for 3 s, ease the
    // whole orientation back to identity (τ ≈ 1 s) until it's centered or
    // the threshold is exceeded again.
    if pb("orient_auto_recenter", false) {
        let thresh = pf("orient_recenter_thresh", 0.005).max(1e-5);
        let g_mag = (gx * gx + gy * gy + gz * gz).sqrt();
        if g_mag < thresh {
            state.aux_f32[16] += dt;
        } else {
            state.aux_f32[16] = 0.0;
        }
        if state.aux_f32[16] >= 3.0 && dt > 0.0 {
            let q = glam::Quat::from_xyzw(
                state.aux_f32[7],
                state.aux_f32[8],
                state.aux_f32[9],
                state.aux_f32[10],
            );
            let step = 1.0 - (-dt / 1.0_f32).exp();
            let new_q = q.slerp(glam::Quat::IDENTITY, step).normalize();
            state.aux_f32[7] = new_q.x;
            state.aux_f32[8] = new_q.y;
            state.aux_f32[9] = new_q.z;
            state.aux_f32[10] = new_q.w;
            // Re-anchor the drift-correction gravity reference against the
            // easing pose, exactly like a manual reset does — otherwise the
            // tilt correction would fight the pull toward identity whenever
            // the controller rests in a non-flat pose.
            let a_model = glam::Vec3::new(ay, -az, -ax);
            let len = a_model.length();
            if len > 1e-4 {
                let w = new_q * (a_model / len);
                state.aux_f32[13] = w.x;
                state.aux_f32[14] = w.y;
                state.aux_f32[15] = w.z;
            }
        }
    } else {
        state.aux_f32[16] = 0.0;
    }

    // Emit orientation as Vec4 (x, y, z, w)
    let orientation_signal = Some(Signal::Vec4(glam::Vec4::new(
        state.aux_f32[7],
        state.aux_f32[8],
        state.aux_f32[9],
        state.aux_f32[10],
    )));

    vec![
        Some(Signal::Vec2(glam::Vec2::new(final_x, final_y))),
        Some(Signal::Float(final_x)),
        Some(Signal::Float(final_y)),
        Some(Signal::Float(lean_val)),
        Some(Signal::Bool(lean_active)),
        orientation_signal,
        // Map (AutoMap) — routing-only, no per-frame value. Slot must
        // exist so its index lines up with the module descriptor; the
        // actual per-pin signals are written into collector_sigs under
        // "lean:{uid}" by the dispatch block in `eval_graph_tick`.
        None,
    ]
}

// ── Curve helpers ─────────────────────────────────────────────────────────────

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
