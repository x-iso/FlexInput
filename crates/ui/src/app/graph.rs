//! Processing-graph snapshot building, AutoMap device tracing across
//! sub-patch boundaries, and display-state sync back into the UI snarls.

use super::*;

// ── Processing-thread graph snapshot builder ──────────────────────────────────

/// A linked-list frame used by `find_automap_device` to track the outer snarl(s)
/// when descending into nested subpatches. An inlet in an inner snarl uses the top
/// frame to pop back to the outer snarl and continue tracing the AutoMap wire chain.
pub(crate) struct AutomapParent<'a> {
    snarl: &'a Snarl<NodeData>,
    /// NodeId of the subpatch node in `snarl` that we descended through.
    subpatch_id: NodeId,
    prev: Option<&'a AutomapParent<'a>>,
}

/// Reconstructs the eval-side `outer_uid` value at the depth of `p` by folding
/// `namespaced_uid` over the subpatch-id chain from root to `p`. Matches the
/// chain that `eval_subgraph` builds as it descends recursively.
pub(crate) fn fold_outer_uid(p: &AutomapParent<'_>) -> usize {
    match p.prev {
        None => p.subpatch_id.0,
        Some(prev) => flexinput_engine::namespaced_uid(fold_outer_uid(prev), p.subpatch_id.0),
    }
}

/// Walks `snarl` and (recursively) any subpatch inner snarls, populating
/// `extra.last_signals` and `extra.history` from the latest eval results.
/// `parent_uid` is `None` at the root, `Some(ns_uid)` when recursing into a
/// subpatch — matching the `outer_uid` that `eval_subgraph` used so inner
/// nodes look up their own samples.
pub(crate) fn apply_display_state(
    snarl: &mut Snarl<NodeData>,
    parent_uid: Option<usize>,
    last_inputs: &HashMap<usize, Vec<Option<Signal>>>,
    last_outputs: &HashMap<usize, Vec<Option<Signal>>>,
    scope_lookup: &mut HashMap<usize, Vec<Vec<Option<f32>>>>,
) {
    let ids: Vec<NodeId> = snarl.nodes_ids_data().map(|(id, _)| id).collect();
    for id in ids {
        let uid = match parent_uid {
            None => id.0,
            Some(p) => flexinput_engine::namespaced_uid(p, id.0),
        };
        if let Some(node) = snarl.get_node_mut(id) {
            if let Some(sigs) = last_inputs.get(&uid) {
                node.extra.last_signals = sigs.clone();
            }
            if let Some(outs) = last_outputs.get(&uid) {
                node.extra.last_out = outs.clone();
            }
            // Switch: the engine reconciles UI clicks + direct/latch inputs
            // and emits the resulting Bool as output[0]. Mirror that back into
            // `params["active"]` so the UI body reads a value that's already
            // in sync with the wires next frame.
            if node.module_id == "module.switch" {
                if let Some(Some(flexinput_core::Signal::Bool(b))) = node.extra.last_out.first() {
                    let cur = node.params.get("active").and_then(|v| v.as_bool()).unwrap_or(false);
                    if cur != *b {
                        node.params.insert("active".to_string(), serde_json::Value::Bool(*b));
                    }
                }
            }
            // Scope samples: move (drain) the per-uid bucket out of
            // scope_lookup instead of cloning each `Vec<Option<f32>>`.
            // Each sample becomes a single push_back into the history
            // ring with no intermediate copy.
            if let Some(samples) = scope_lookup.remove(&uid) {
                let is_trigscope = node.module_id == "display.trigscope";
                if is_trigscope {
                    let win_samples = {
                        let win_ms = node.params.get("ts_win_ms").and_then(|v| v.as_f64()).unwrap_or(200.0).clamp(10.0, 10_000.0) as f32;
                        (win_ms / 1000.0 * current_sample_rate() as f32) as usize
                    };
                    for s in samples {
                        let trig_val = s.first().copied().flatten().unwrap_or(0.0);
                        let rising = node.extra.trig_prev <= 0.0 && trig_val > 0.0;
                        node.extra.trig_prev = trig_val;
                        if rising && !node.extra.trig_armed {
                            node.extra.trig_armed = true;
                            node.extra.trig_acc.clear();
                        }
                        if node.extra.trig_armed {
                            node.extra.trig_acc.push(s);
                            if node.extra.trig_acc.len() >= win_samples {
                                node.extra.trig_capture = Some(std::mem::take(&mut node.extra.trig_acc));
                                node.extra.trig_armed = false;
                            }
                        }
                    }
                } else {
                    let h = &mut node.extra.history;
                    for s in samples {
                        if h.len() >= HISTORY_LEN { h.pop_front(); }
                        h.push_back(s);
                    }
                }
            }
        }
        // Recurse into subpatch inner snarl.
        let child_uid = uid;
        let is_subpatch = snarl.get_node(id).map(|n| n.module_id == "subpatch").unwrap_or(false);
        if is_subpatch {
            if let Some(node) = snarl.get_node_mut(id) {
                if let Some(sp) = node.subpatch.as_mut() {
                    apply_display_state(&mut sp.snarl, Some(child_uid), last_inputs, last_outputs, scope_lookup);
                }
            }
        }
    }
}

/// Copy live display state (`NodeData.extra`) from `src` snarl into `dst` snarl,
/// matching nodes by `NodeId`, recursing through sub-patches.
///
/// `extra` is `#[serde(skip)]` runtime-only data — scope history, last-signal
/// readouts, meter values — refreshed each frame by `apply_display_state` on the
/// tab canvas (and its nested `node.subpatch.snarl` copies). The sub-patch editor
/// renders a SEPARATE clone of that inner snarl, so its nodes' `extra` would
/// otherwise be frozen at the moment the editor opened. This pushes the fresh
/// `extra` across the boundary every frame WITHOUT touching positions, params,
/// wires, or the node set (all owned by the editor), so live visuals animate in
/// the editor without reverting in-progress edits. NodeIds line up because the
/// editor was seeded from a clone of `sp.snarl` and writes its full snarl back,
/// keeping the slab slots aligned.
pub(crate) fn sync_display_state_into(dst: &mut Snarl<NodeData>, src: &Snarl<NodeData>) {
    let ids: Vec<NodeId> = dst.nodes_ids_data().map(|(id, _)| id).collect();
    for id in ids {
        if let Some(src_node) = src.get_node(id) {
            let src_extra = src_node.extra.clone();
            if let Some(dst_node) = dst.get_node_mut(id) {
                dst_node.extra = src_extra;
            }
        }
        // Recurse into matching sub-patch nodes so nested editors (and pinned
        // bodies showing nested scopes) also receive fresh display state.
        let is_sub = dst.get_node(id).map(|n| n.module_id == "subpatch").unwrap_or(false);
        if is_sub {
            // Take the source child snarl out by reborrow to avoid aliasing dst.
            let src_child = src.get_node(id).and_then(|n| n.subpatch.as_ref()).map(|sp| &sp.snarl);
            if let Some(src_child) = src_child {
                if let Some(dst_node) = dst.get_node_mut(id) {
                    if let Some(dst_sp) = dst_node.subpatch.as_mut() {
                        sync_display_state_into(&mut dst_sp.snarl, src_child);
                    }
                }
            }
        }
    }
}

/// True when `id` names a real I/O device (physical pad, MIDI port, or virtual
/// sink) rather than a synthetic AutoMap-bus key (`collector:`, `remap:`,
/// `forksel:`, `combiner:`, `lean:`). Used to decide when to fall back to the
/// underlying physical device for feedback (reverse) routing.
pub(crate) fn is_real_device_id(id: &str) -> bool {
    id.starts_with("gilrs:")
        || id.starts_with("sdl:")
        || id.starts_with("midi_in:")
        || id.starts_with("midi_out:")
        || id.starts_with("virtual.")
}

/// Public helper for the viewer: resolve an AutoMap chain back to the
/// originating physical device id (or a sensible fallback) for UI capture.
/// Returns `Some(device_id)` when resolved, or `None` when not wired.
pub fn find_automap_device_id_for_viewer(
    snarl: &Snarl<NodeData>,
    src: OutPinId,
    parent: Option<&crate::canvas::viewer::AutomapGlowParent<'_>>,
) -> Option<String> {
    // Mirror of `find_automap_device_rec` but accepting the viewer's
    // `AutomapGlowParent` chain so the UI can resolve AutoMap origins when
    // rendering inner canvases. Returns (dev_id, pins, fallback) and we
    // surface the fallback or dev_id to the caller.
    fn rec(
        snarl: &Snarl<NodeData>,
        src: OutPinId,
        parents: Option<&crate::canvas::viewer::AutomapGlowParent<'_>>,
    ) -> Option<(String, Vec<String>, Option<String>)> {
        let node = snarl.get_node(src.node)?;
        if node.module_id == "device.source" {
            let dev_id = node.params.get("device_id")?.as_str()?.to_string();
            let pin_ids: Vec<String> = node.params.get("output_pin_ids")?.as_array()?
                .iter().map(|v| v.as_str().unwrap_or("").to_string()).collect();
            return Some((dev_id, pin_ids, None));
        }
        // Touch Zones mapping mode = injector under `touchmap:{uid}` (mirror of
        // find_automap_device_rec); ports mode = passthrough (next arm).
        // The Virtual Menu is ALWAYS an injector (`menumap:{uid}`) — its
        // suppression applies in ports mode too.
        if node.module_id == "module.menu"
            || (node.module_id == "module.touch_zones"
                && node.params.get("zone_mode").and_then(|v| v.as_str()) == Some("mapping"))
        {
            let upstream_dev_id = node.inputs.iter()
                .position(|p| p.signal_type == SignalType::AutoMap)
                .and_then(|am_idx| {
                    let in_pin = snarl.in_pin(InPinId { node: src.node, input: am_idx });
                    in_pin.remotes.first().copied()
                })
                .and_then(|s| rec(snarl, s, parents).map(|(id, _, _)| id));
            let map_uid = match parents {
                None => src.node.0,
                Some(p) => flexinput_engine::namespaced_uid(fold_outer_uid_app(p), src.node.0),
            };
            let prefix = if node.module_id == "module.menu" { "menumap" } else { "touchmap" };
            let map_id = format!("{prefix}:{map_uid}");
            let canonical_pins: Vec<String> = flexinput_core::automap::ALL_PINS
                .iter().map(|p| p.id.to_string()).collect();
            return Some((map_id, canonical_pins, upstream_dev_id));
        }
        if node.module_id == "module.automap_split"
            || node.module_id == "module.feedback_control"
            || node.module_id == "module.touch_zones"
            || node.module_id == "module.input_viewer"
        {
            let am_idx = node.inputs.iter().position(|p| p.signal_type == SignalType::AutoMap)?;
            let in_pin = snarl.in_pin(InPinId { node: src.node, input: am_idx });
            let upstream = *in_pin.remotes.first()?;
            return rec(snarl, upstream, parents);
        }
        if node.module_id == "module.automap_fork" || node.module_id == "module.automap_selector" {
            let node_uid = match parents {
                None => src.node.0,
                Some(p) => flexinput_engine::namespaced_uid(fold_outer_uid_app(p), src.node.0),
            };
            let collector_id = format!("forksel:{}:{}", node_uid, src.output);
            let canonical_pins: Vec<String> = flexinput_core::automap::ALL_PINS
                .iter().map(|p| p.id.to_string()).collect();
            return Some((collector_id, canonical_pins, None));
        }
        // Modules that re-publish the AutoMap bus into their OWN `collector:{uid}`
        // key (see module_ui_info): a node downstream of one must read from that
        // collector key — not recurse past it like feedback_control does.
        if crate::module_ui_info::republishes_automap_bus(&node.module_id) {
            let upstream_dev_id = node.inputs.iter()
                .position(|p| p.signal_type == SignalType::AutoMap)
                .and_then(|am_idx| {
                    let in_pin = snarl.in_pin(InPinId { node: src.node, input: am_idx });
                    in_pin.remotes.first().copied()
                })
                .and_then(|s| rec(snarl, s, parents).map(|(id, _, _)| id));
            let collector_uid = match parents {
                None => src.node.0,
                Some(p) => flexinput_engine::namespaced_uid(fold_outer_uid_app(p), src.node.0),
            };
            let collector_id = format!("collector:{}", collector_uid);
            let canonical_pins: Vec<String> = flexinput_core::automap::ALL_PINS
                .iter().map(|p| p.id.to_string()).collect();
            return Some((collector_id, canonical_pins, upstream_dev_id));
        }
        if node.module_id == "module.automap_combiner" {
            let combiner_uid = match parents {
                None => src.node.0,
                Some(p) => flexinput_engine::namespaced_uid(fold_outer_uid_app(p), src.node.0),
            };
            let combiner_id = format!("combiner:{}", combiner_uid);
            let canonical_pins: Vec<String> = flexinput_core::automap::ALL_PINS
                .iter().map(|p| p.id.to_string()).collect();
            let upstream_dev_id = (0..node.inputs.len())
                .find_map(|i| {
                    if node.inputs[i].signal_type != SignalType::AutoMap { return None; }
                    let in_pin = snarl.in_pin(InPinId { node: src.node, input: i });
                    let &s = in_pin.remotes.first()?;
                    rec(snarl, s, parents).map(|(id, _, fallback)| fallback.unwrap_or(id))
                });
            return Some((combiner_id, canonical_pins, upstream_dev_id));
        }
        if node.module_id == "module.remapper" {
            let upstream_dev_id = node.inputs.iter()
                .position(|p| p.signal_type == SignalType::AutoMap)
                .and_then(|am_idx| {
                    let in_pin = snarl.in_pin(InPinId { node: src.node, input: am_idx });
                    in_pin.remotes.first().copied()
                })
                .and_then(|s| rec(snarl, s, parents).map(|(id, _, _)| id));
            let remap_uid = match parents {
                None => src.node.0,
                Some(p) => flexinput_engine::namespaced_uid(fold_outer_uid_app(p), src.node.0),
            };
            let remap_id = format!("remap:{}", remap_uid);
            let canonical_pins: Vec<String> = flexinput_core::automap::ALL_PINS
                .iter().map(|p| p.id.to_string()).collect();
            return Some((remap_id, canonical_pins, upstream_dev_id));
        }
        if node.module_id == "processing.gyro_3dof" {
            let pin_type = node.outputs.get(src.output).map(|p| p.signal_type);
            if pin_type != Some(SignalType::AutoMap) { return None; }
            let lean_uid = match parents {
                None => src.node.0,
                Some(p) => flexinput_engine::namespaced_uid(fold_outer_uid_app(p), src.node.0),
            };
            let lean_id = format!("lean:{}", lean_uid);
            let canonical_pins: Vec<String> = flexinput_core::automap::ALL_PINS
                .iter().map(|p| p.id.to_string()).collect();
            return Some((lean_id, canonical_pins, None));
        }
        if node.module_id == "subpatch" {
            let sp = node.subpatch.as_ref()?;
            let outlet_id: NodeId = sp.snarl.nodes_ids_data()
                .find(|(_, n)| n.value.module_id == "subpatch.outlet"
                    && n.value.params.get("pin_index").and_then(|v| v.as_u64())
                        == Some(src.output as u64))
                .map(|(id, _)| id)?;
            let outlet_in = sp.snarl.in_pin(InPinId { node: outlet_id, input: 0 });
            let inner_upstream = *outlet_in.remotes.first()?;
            let frame = crate::canvas::viewer::AutomapGlowParent { snarl, subpatch_node_id: src.node, prev: parents };
            return rec(&sp.snarl, inner_upstream, Some(&frame));
        }
        if node.module_id == "subpatch.inlet" {
            let pin_idx = node.params.get("pin_index").and_then(|v| v.as_u64())? as usize;
            let p = parents?;
            let outer_in = p.snarl.in_pin(InPinId { node: p.subpatch_node_id, input: pin_idx });
            let upstream = *outer_in.remotes.first()?;
            return rec(p.snarl, upstream, p.prev);
        }
        None
    }
    rec(snarl, src, parent).map(|(dev, _pins, fallback)| fallback.unwrap_or(dev))
}

// Helper to reconstruct the fold_outer_uid value for the viewer parent chain.
pub(crate) fn fold_outer_uid_app(p: &crate::canvas::viewer::AutomapGlowParent<'_>) -> usize {
    match p.prev {
        None => p.subpatch_node_id.0,
        Some(prev) => flexinput_engine::namespaced_uid(fold_outer_uid_app(prev), p.subpatch_node_id.0),
    }
}

/// A physical INPUT device id (a blockable source), as opposed to a virtual
/// sink (`virtual.*`), a MIDI *output*, or a synthetic AutoMap-bus key
/// (`collector:`, `remap:`, `combiner:`…). The config overlay's source-block
/// and passthrough (M3) operate only on these.
pub(crate) fn is_physical_input_device(id: &str) -> bool {
    id.starts_with("gilrs:") || id.starts_with("sdl:") || id.starts_with("midi_in:")
}

/// Resolve the upstream PHYSICAL input device that a config tweak-pin's module
/// reads, so the config overlay can pass that device through while the pin is
/// being tweaked (M3.4): you feel the parameter's effect in-game and the pin's
/// live graph dot keeps moving, while every OTHER device stays blocked. Returns
/// the physical device id, or `None` for a source-like pin with no upstream
/// device (a Knob / Constant / Dropdown) or an unresolvable chain — in which
/// case the full block stands.
///
/// M3.4 grain is the WHOLE upstream device (every pin). Narrowing to the exact
/// consumed pin(s) is a later refinement; whole-device is the plan's sanctioned
/// fallback and is what makes the feature testable. Handles tweak-pins on the
/// tab canvas (`source_path == []`) and inside a first-level sub-patch (`[sp]`),
/// reusing [`find_automap_device_id_for_viewer`] to walk the AutoMap chain (it
/// pops back to the outer snarl through the sub-patch inlet on its own).
pub(crate) fn config_passthrough_device(
    tab_snarl: &Snarl<NodeData>,
    source_path: &[usize],
    inner_node_id: usize,
) -> Option<String> {
    // Trace the module's FIRST connected input back to a physical device. A
    // module reads its driving signal on that input (a Response Curve's scalar,
    // a Reshaper's Vec, a gyro's AutoMap bus…); source-like modules with no
    // inputs (Knob/Constant) simply resolve to None.
    fn trace(
        snarl: &Snarl<NodeData>,
        node_id: NodeId,
        parent: Option<&crate::canvas::viewer::AutomapGlowParent<'_>>,
    ) -> Option<String> {
        let node = snarl.get_node(node_id)?;
        for i in 0..node.inputs.len() {
            let in_pin = snarl.in_pin(InPinId { node: node_id, input: i });
            if let Some(&remote) = in_pin.remotes.first() {
                if let Some(dev) = find_automap_device_id_for_viewer(snarl, remote, parent) {
                    if is_physical_input_device(&dev) {
                        return Some(dev);
                    }
                }
            }
        }
        None
    }
    let traced = match source_path {
        [] => trace(tab_snarl, NodeId(inner_node_id), None),
        [sp] => {
            let sp_node = NodeId(*sp);
            tab_snarl.get_node(sp_node).and_then(|n| n.subpatch.as_ref()).and_then(|inner| {
                let frame = crate::canvas::viewer::AutomapGlowParent {
                    snarl: tab_snarl,
                    subpatch_node_id: sp_node,
                    prev: None,
                };
                trace(&inner.snarl, NodeId(inner_node_id), Some(&frame))
            })
        }
        _ => None,
    };
    // Fallback: when the per-pin trace can't follow the chain (an input type the
    // AutoMap walker doesn't model, an odd wiring…), pass the tab's own physical
    // device.source through. For the common single-device patch this is exactly
    // the device the pin depends on, and it guarantees the tweaked input is
    // never left blocked. Top-level device.source feeds sub-patch pins too.
    traced.or_else(|| tab_physical_source_device(tab_snarl))
}

/// The first physical `device.source` device id on the tab canvas, if any.
/// Used as the config-overlay passthrough fallback (single-device patches).
pub(crate) fn tab_physical_source_device(tab_snarl: &Snarl<NodeData>) -> Option<String> {
    tab_snarl
        .nodes_ids_data()
        .filter_map(|(_, n)| {
            if n.value.module_id == "device.source" {
                n.value
                    .params
                    .get("device_id")
                    .and_then(|v| v.as_str())
                    .filter(|d| is_physical_input_device(d))
                    .map(|s| s.to_string())
            } else {
                None
            }
        })
        .next()
}

/// Walk an AutoMap wire chain from `src` back to the originating device.source.
/// Returns (source_dev_id, source_pin_ids, fallback_dev_id).
/// - For device.source: (real_dev_id, output_pins, None).
/// - For automap_split: transparent passthrough (result of upstream).
/// - For automap_collect: ("collector:{uid}", canonical_pins, Some(upstream_real_dev_id)).
/// - For subpatch: descends into the inner snarl through the matching outlet.
/// - For subpatch.inlet (only reached during inner traversal): pops back to outer snarl.
pub(crate) fn find_automap_device_rec(
    snarl: &Snarl<NodeData>,
    src: OutPinId,
    parents: Option<&AutomapParent<'_>>,
) -> Option<(String, Vec<String>, Option<String>)> {
    let node = snarl.get_node(src.node)?;
    if node.module_id == "device.source" {
        let dev_id = node.params.get("device_id")?.as_str()?.to_string();
        let pin_ids: Vec<String> = node.params.get("output_pin_ids")?.as_array()?
            .iter().map(|v| v.as_str().unwrap_or("").to_string()).collect();
        return Some((dev_id, pin_ids, None));
    }
    // Touch Zones in MAPPING mode is an injector: it publishes per-zone behaviour
    // overrides into collector_sigs under a `touchmap:{uid}` key, exactly like the
    // Remapper. In PORTS mode it's a plain passthrough (zone data lives on the
    // typed outputs, nothing is injected onto the bus) — handled below.
    if node.module_id == "module.menu"
        || (node.module_id == "module.touch_zones"
            && node.params.get("zone_mode").and_then(|v| v.as_str()) == Some("mapping"))
    {
        // The Virtual Menu is ALWAYS an injector (`menumap:{uid}`) — its
        // suppression applies in ports mode too.
        let upstream_dev_id = node.inputs.iter()
            .position(|p| p.signal_type == SignalType::AutoMap)
            .and_then(|am_idx| {
                let in_pin = snarl.in_pin(InPinId { node: src.node, input: am_idx });
                in_pin.remotes.first().copied()
            })
            .and_then(|s| find_automap_device_rec(snarl, s, parents).map(|(id, _, _)| id));
        let map_uid = match parents {
            None => src.node.0,
            Some(p) => flexinput_engine::namespaced_uid(fold_outer_uid(p), src.node.0),
        };
        let prefix = if node.module_id == "module.menu" { "menumap" } else { "touchmap" };
        let map_id = format!("{prefix}:{map_uid}");
        let canonical_pins: Vec<String> = flexinput_core::automap::ALL_PINS
            .iter().map(|p| p.id.to_string()).collect();
        return Some((map_id, canonical_pins, upstream_dev_id));
    }
    if node.module_id == "module.automap_split"
        || node.module_id == "module.feedback_control"
        || node.module_id == "module.touch_zones"
        || node.module_id == "module.input_viewer"
    {
        // All pass the AutoMap bus through on output 0 from their AutoMap input.
        // (Touch Zones in ports mode injects nothing; its zone data is on the
        // dynamic typed outputs, not the AutoMap passthrough.)
        let am_idx = node.inputs.iter().position(|p| p.signal_type == SignalType::AutoMap)?;
        let in_pin = snarl.in_pin(InPinId { node: src.node, input: am_idx });
        let upstream = *in_pin.remotes.first()?;
        return find_automap_device_rec(snarl, upstream, parents);
    }
    // Fork and Selector act as gating collectors: they inject signals into collector_sigs
    // only on the active output/input, so non-active paths produce silence.
    // No fallback device — the collector key alone controls what the sink sees.
    if node.module_id == "module.automap_fork"
        || node.module_id == "module.automap_selector"
    {
        let node_uid = match parents {
            None => src.node.0,
            Some(p) => flexinput_engine::namespaced_uid(fold_outer_uid(p), src.node.0),
        };
        // Encode which output pin the sink is downstream of so eval can gate per-output.
        let collector_id = format!("forksel:{}:{}", node_uid, src.output);
        let canonical_pins: Vec<String> = flexinput_core::automap::ALL_PINS
            .iter().map(|p| p.id.to_string()).collect();
        return Some((collector_id, canonical_pins, None));
    }
    // Modules that re-publish the AutoMap bus into their own `collector:{uid}` key
    // (see module_ui_info): a downstream node must read from that collector key.
    // Without this arm, ASTH fell through unhandled → its AutoMap output resolved
    // to nothing → the port produced no signal.
    if crate::module_ui_info::republishes_automap_bus(&node.module_id) {
        let upstream_dev_id = node.inputs.iter()
            .position(|p| p.signal_type == SignalType::AutoMap)
            .and_then(|am_idx| {
                let in_pin = snarl.in_pin(InPinId { node: src.node, input: am_idx });
                in_pin.remotes.first().copied()
            })
            .and_then(|s| find_automap_device_rec(snarl, s, parents).map(|(id, _, _)| id));
        // The collector ID must match the key the eval thread uses when injecting
        // signals: root-level collectors use NodeId.0, subpatch-nested collectors
        // use namespaced_uid folded through the parent chain.
        let collector_uid = match parents {
            None => src.node.0,
            Some(p) => flexinput_engine::namespaced_uid(fold_outer_uid(p), src.node.0),
        };
        let collector_id = format!("collector:{}", collector_uid);
        let canonical_pins: Vec<String> = flexinput_core::automap::ALL_PINS
            .iter().map(|p| p.id.to_string()).collect();
        return Some((collector_id, canonical_pins, upstream_dev_id));
    }
    if node.module_id == "module.automap_combiner" {
        // Combiner is a virtual bus: per-pin priority merge of its N AutoMap
        // inputs, written into collector_sigs under "combiner:{uid}". Downstream
        // consumers read it the same way they read any other collector.
        let combiner_uid = match parents {
            None => src.node.0,
            Some(p) => flexinput_engine::namespaced_uid(fold_outer_uid(p), src.node.0),
        };
        let combiner_id = format!("combiner:{}", combiner_uid);
        let canonical_pins: Vec<String> = flexinput_core::automap::ALL_PINS
            .iter().map(|p| p.id.to_string()).collect();
        // Use the first connected input's underlying physical device as the
        // fallback so haptic-feedback reverse-routing has something to bind
        // (matches Collector's behaviour).
        let upstream_dev_id = (0..node.inputs.len())
            .find_map(|i| {
                if node.inputs[i].signal_type != SignalType::AutoMap { return None; }
                let in_pin = snarl.in_pin(InPinId { node: src.node, input: i });
                let &s = in_pin.remotes.first()?;
                find_automap_device_rec(snarl, s, parents).map(|(id, _, fallback)| {
                    fallback.unwrap_or(id)
                })
            });
        return Some((combiner_id, canonical_pins, upstream_dev_id));
    }
    if node.module_id == "module.remapper" {
        // Acts as a collector: publishes per-pin signals (pass-through + mapping
        // overrides) into collector_sigs under a `remap:{uid}` key. Downstream
        // sinks find these the same way they find collector / forksel signals.
        let upstream_dev_id = node.inputs.iter()
            .position(|p| p.signal_type == SignalType::AutoMap)
            .and_then(|am_idx| {
                let in_pin = snarl.in_pin(InPinId { node: src.node, input: am_idx });
                in_pin.remotes.first().copied()
            })
            .and_then(|s| find_automap_device_rec(snarl, s, parents).map(|(id, _, _)| id));
        let remap_uid = match parents {
            None => src.node.0,
            Some(p) => flexinput_engine::namespaced_uid(fold_outer_uid(p), src.node.0),
        };
        let remap_id = format!("remap:{}", remap_uid);
        let canonical_pins: Vec<String> = flexinput_core::automap::ALL_PINS
            .iter().map(|p| p.id.to_string()).collect();
        return Some((remap_id, canonical_pins, upstream_dev_id));
    }
    if node.module_id == "processing.gyro_3dof" {
        // Lean dispatch publishes per-pin signals into collector_sigs under
        // `lean:{uid}`. Only the `Map` AutoMap output pin (the last output)
        // resolves to this collector — wires from the other outputs (Out,
        // X, Y, Lean, Lean!) are typed Float / Vec2 / Bool and shouldn't
        // hit this path, but we guard on signal type just in case.
        let pin_type = node.outputs.get(src.output).map(|p| p.signal_type);
        if pin_type != Some(SignalType::AutoMap) { return None; }
        // No upstream fallback — the 3DOF module's Device input feeds gyro
        // data, not a passthrough for other gamepad pins.
        let lean_uid = match parents {
            None => src.node.0,
            Some(p) => flexinput_engine::namespaced_uid(fold_outer_uid(p), src.node.0),
        };
        let lean_id = format!("lean:{}", lean_uid);
        let canonical_pins: Vec<String> = flexinput_core::automap::ALL_PINS
            .iter().map(|p| p.id.to_string()).collect();
        return Some((lean_id, canonical_pins, None));
    }
    if node.module_id == "subpatch" {
        // Wire enters the subpatch via output pin `src.output`. Find the outlet
        // inside whose pin_index matches, and continue tracing from its input.
        let sp = node.subpatch.as_ref()?;
        let outlet_id: NodeId = sp.snarl.nodes_ids_data()
            .find(|(_, n)| n.value.module_id == "subpatch.outlet"
                && n.value.params.get("pin_index").and_then(|v| v.as_u64())
                    == Some(src.output as u64))
            .map(|(id, _)| id)?;
        let outlet_in = sp.snarl.in_pin(InPinId { node: outlet_id, input: 0 });
        let inner_upstream = *outlet_in.remotes.first()?;
        let frame = AutomapParent { snarl, subpatch_id: src.node, prev: parents };
        return find_automap_device_rec(&sp.snarl, inner_upstream, Some(&frame));
    }
    if node.module_id == "subpatch.inlet" {
        // Inner trace reached an inlet — pop back to the outer snarl and follow
        // the outer subpatch's matching input pin upstream.
        let pin_idx = node.params.get("pin_index").and_then(|v| v.as_u64())? as usize;
        let parent = parents?;
        let outer_in = parent.snarl.in_pin(InPinId { node: parent.subpatch_id, input: pin_idx });
        let upstream = *outer_in.remotes.first()?;
        return find_automap_device_rec(parent.snarl, upstream, parent.prev);
    }
    None
}

/// Downstream sibling of [`find_automap_device_rec`]: follow an AutoMap bus
/// FORWARD from an output pin to the destination `device.sink`'s device_id,
/// crossing sub-patch boundaries (out through outlets, in through inlets).
/// Returns the first `device.sink` reached. Used by the Feedback Control node to
/// locate the virtual destination whose rumble/light request its outlets tap.
pub(crate) fn find_automap_dest_sink_rec(
    snarl: &Snarl<NodeData>,
    dst: InPinId,
    parents: Option<&AutomapParent<'_>>,
    depth: u32,
) -> Option<String> {
    if depth > 64 { return None; }
    let node = snarl.get_node(dst.node)?;
    // Destination device sink — the end of the line.
    if node.module_id == "device.sink" {
        return node.params.get("device_id").and_then(|v| v.as_str()).map(|s| s.to_string());
    }
    // Outlet: pop OUT to the parent snarl, continue from the subpatch node's
    // matching output pin.
    if node.module_id == "subpatch.outlet" {
        let pin_idx = node.params.get("pin_index").and_then(|v| v.as_u64())? as usize;
        let parent = parents?;
        let out_pin = parent.snarl.out_pin(OutPinId { node: parent.subpatch_id, output: pin_idx });
        for &downstream in &out_pin.remotes {
            if let Some(d) = find_automap_dest_sink_rec(parent.snarl, downstream, parent.prev, depth + 1) {
                return Some(d);
            }
        }
        return None;
    }
    // Wire entered a subpatch via input pin `dst.input`. Pop IN: find the inlet
    // with the matching pin_index and continue from its output downstream.
    if node.module_id == "subpatch" {
        let sp = node.subpatch.as_ref()?;
        let inlet_id: NodeId = sp.snarl.nodes_ids_data()
            .find(|(_, n)| n.value.module_id == "subpatch.inlet"
                && n.value.params.get("pin_index").and_then(|v| v.as_u64())
                    == Some(dst.input as u64))
            .map(|(id, _)| id)?;
        let frame = AutomapParent { snarl, subpatch_id: dst.node, prev: parents };
        let inlet_out = sp.snarl.out_pin(OutPinId { node: inlet_id, output: 0 });
        for &downstream in &inlet_out.remotes {
            if let Some(d) = find_automap_dest_sink_rec(&sp.snarl, downstream, Some(&frame), depth + 1) {
                return Some(d);
            }
        }
        return None;
    }
    // Pass-through AutoMap modules forward the bus on their AutoMap output 0
    // (Splitter, Collector, Fork, Selector, Combiner, Remapper, Feedback Control).
    // Follow output pin 0's downstream remotes.
    let out_pin = snarl.out_pin(OutPinId { node: dst.node, output: 0 });
    for &downstream in &out_pin.remotes {
        if let Some(d) = find_automap_dest_sink_rec(snarl, downstream, parents, depth + 1) {
            return Some(d);
        }
    }
    None
}

/// Builds a topologically-sorted [`ProcessingGraph`] from the current Snarl state.
/// Also returns the UIDs of any counter nodes whose reset was just requested
/// (caller must clear the `aux_f32_dirty` flag on those nodes after writing the snapshot).
pub(crate) fn build_processing_graph(
    snarl: &Snarl<NodeData>,
    defaults: crate::canvas::DeviceParamDefaults,
) -> (ProcessingGraph, Vec<usize>) {
    build_processing_graph_rec(snarl, None, defaults)
}

pub(crate) fn build_processing_graph_rec(
    snarl: &Snarl<NodeData>,
    parents: Option<&AutomapParent<'_>>,
    defaults: crate::canvas::DeviceParamDefaults,
) -> (ProcessingGraph, Vec<usize>) {
    use std::collections::{HashSet, VecDeque};
    use flexinput_engine::graph::{InlineSubgraph, SinkTarget};

    // Collect ALL nodes (including device.sink — they're evaluated last).
    let node_list: Vec<(NodeId, &NodeData)> = snarl.nodes_ids_data()
        .map(|(id, n)| (id, &n.value))
        .collect();

    let id_to_orig: HashMap<NodeId, usize> = node_list.iter()
        .enumerate()
        .map(|(i, (id, _))| (*id, i))
        .collect();

    let mut dirty_uids: Vec<usize> = Vec::new();

    // Pre-pass: physical device ids whose source node enabled the digital→analog
    // trigger bridge (or that are digital-only pads, where it's always on). A sink
    // only honours the bridge when its upstream source is in this set.
    let mut digital_trigger_devs: HashSet<String> = HashSet::new();
    for (_id, node) in &node_list {
        if node.module_id != "device.source" { continue; }
        let Some(dev_id) = node.params.get("device_id").and_then(|v| v.as_str()) else { continue; };
        let opted_in = node.params.get("digital_triggers").and_then(|v| v.as_bool()).unwrap_or(false);
        // Both backends: an SDL-surfaced Switch Pro is digital-only too.
        let digital_only =
            crate::canvas::remapper_icons::phys_pad_slug(dev_id) == Some("switch_pro");
        if opted_in || digital_only {
            digital_trigger_devs.insert(dev_id.to_string());
        }
    }

    // Pre-pass: collect, for each physical device_id used as an AutoMap source,
    // the list of virtual sink device_ids that auto-map from it. Used to wire
    // feedback signals (rumble, lightbar) backward along AutoMap connections.
    let mut feedback_map: HashMap<String, Vec<flexinput_engine::FeedbackSource>> = HashMap::new();
    for (node_id, node) in &node_list {
        let is_sink = node.module_id == "device.sink"
            || (node.module_id == "device.source" && !node.inputs.is_empty());
        if !is_sink { continue; }
        // Find this sink's AutoMap source device_id (if wired).
        //
        // When the wire passes through an AutoMap module (Collector, Fork,
        // Selector, Remapper, Combiner, 3DOF), `find_automap_device_rec`
        // returns a SYNTHETIC key (`collector:{uid}`, `remap:{uid}`, …) as the
        // first element and the real upstream physical device as the fallback
        // (third element). Feedback flows back to the *physical* device, so the
        // map must be keyed by the physical id — fall back to it whenever the
        // resolved id isn't itself a real device id. Without this, routing a pad
        // through a sub-patch full of AutoMap modules silently drops rumble.
        let automap_src_dev = (0..node.inputs.len()).find_map(|i| {
            if node.inputs.get(i).map(|p| p.signal_type) != Some(SignalType::AutoMap) {
                return None;
            }
            let pin = snarl.in_pin(InPinId { node: *node_id, input: i });
            let &src = pin.remotes.first()?;
            find_automap_device_rec(snarl, src, parents).map(|(d, _, fallback)| {
                if is_real_device_id(&d) { d } else { fallback.unwrap_or(d) }
            })
        });
        let Some(src_dev) = automap_src_dev else { continue; };
        let sink_dev = node.params.get("device_id").and_then(|v| v.as_str()).unwrap_or("");
        // Only track virtual sinks (their feedback flows back to physical sources).
        if sink_dev.starts_with("virtual.") {
            // Per-device rumble shaping from the virtual sink node's params.
            // Nodes that never touched their Rumble control have no params and
            // follow the user's Settings defaults (neutral pass-through on a
            // fresh install), same as the node widget displays.
            let p = |k: &str, d: f32| {
                node.params.get(k).and_then(|v| v.as_f64()).map(|f| f as f32).unwrap_or(d)
            };
            feedback_map.entry(src_dev).or_default().push(flexinput_engine::FeedbackSource {
                device_id: sink_dev.to_string(),
                rumble_floor: p("rumble_floor", defaults.rumble_floor).clamp(0.0, 1.0),
                rumble_max:   p("rumble_max", defaults.rumble_max).clamp(0.0, 1.0),
                rumble_exp:   p("rumble_exp", defaults.rumble_exp).max(0.01),
            });
        }
    }

    let mut snaps: Vec<NodeSnap> = node_list.iter().map(|(node_id, node)| {
        let is_sink = node.module_id == "device.sink"
            || (node.module_id == "device.source" && !node.inputs.is_empty());

        // Non-sink: single (first) source per input pin, for the existing eval path.
        let input_sources = if !is_sink {
            (0..node.inputs.len())
                .map(|i| {
                    let pin = snarl.in_pin(InPinId { node: *node_id, input: i });
                    pin.remotes.first().and_then(|&src| {
                        id_to_orig.get(&src.node).map(|&idx| (idx, src.output))
                    })
                })
                .collect()
        } else {
            vec![] // sink nodes use sink_target.multi_sources
        };

        let device_id = node.params.get("device_id")
            .and_then(|v| v.as_str()).map(|s| s.to_string());
        let output_pin_ids = node.params.get("output_pin_ids")
            .and_then(|v| v.as_array())
            .map(|a| a.iter().map(|v| v.as_str().unwrap_or("").to_string()).collect())
            .unwrap_or_default();

        let aux_f32_override = if node.extra.aux_f32_dirty {
            dirty_uids.push(node_id.0);
            Some(node.extra.aux_f32.clone())
        } else {
            None
        };

        // For device.sink: build the full routing metadata.
        let sink_target = if is_sink {
            let sink_dev_id = device_id.clone().unwrap_or_default();
            let mut pin_ids: Vec<String> = node.params
                .get("input_pin_ids")
                .and_then(|v| v.as_array())
                .map(|a| a.iter().map(|v| v.as_str().unwrap_or("").to_string()).collect())
                .unwrap_or_default();
            // `input_pin_ids` is FROZEN at node creation, so sink pins added to a
            // device's layout AFTER a node was saved (e.g. mouse_move on the keymouse
            // sink) are missing — the AutoMap bus then can't route to them ("touchpad
            // mouse does nothing"). Append any current device sink-pin id not already
            // present: it gains no direct-wire slot (multi_sources is shorter, so the
            // direct path skips it) but becomes automap-routable off the bus.
            if let Some((sinks, _, _)) = flexinput_virtual::kind_pin_metadata(
                &flexinput_virtual::kind_prefix(&sink_dev_id), 0)
            {
                for sp in sinks {
                    if !pin_ids.iter().any(|p| p == sp.id) {
                        pin_ids.push(sp.id.to_string());
                    }
                }
            }

            // For each direct-wire input: collect ALL remotes (multi-source, combined additively).
            let multi_sources: Vec<Vec<(usize, usize)>> = (0..node.inputs.len())
                .map(|i| {
                    if node.inputs.get(i).map(|p| p.signal_type) == Some(SignalType::AutoMap) {
                        return vec![];
                    }
                    let pin = snarl.in_pin(InPinId { node: *node_id, input: i });
                    pin.remotes.iter()
                        .filter_map(|&src| id_to_orig.get(&src.node).map(|&idx| (idx, src.output)))
                        .collect()
                })
                .collect();

            // AutoMap: trace the AutoMap wire chain to find the originating device.source.
            let automap_result = (0..node.inputs.len()).find_map(|i| {
                if node.inputs.get(i).map(|p| p.signal_type) != Some(SignalType::AutoMap) {
                    return None;
                }
                let pin = snarl.in_pin(InPinId { node: *node_id, input: i });
                let &src = pin.remotes.first()?;
                find_automap_device_rec(snarl, src, parents)
            });
            let (automap_source, automap_fallback_dev) = match automap_result {
                Some((dev_id, pins, fallback)) => (Some((dev_id, pins)), fallback),
                None => (None, None),
            };

            // Feedback sources: virtual sinks that auto-map FROM this physical device.
            // Their output signals (rumble, lightbar) flow back to this sink's haptic inputs.
            let feedback_sources = feedback_map.get(&sink_dev_id).cloned().unwrap_or_default();

            // Digital→analog trigger bridge: enabled when the upstream PHYSICAL
            // source opted in (or is digital-only). The upstream physical id is the
            // fallback dev when routed through a collector, else the automap source
            // id when it's itself a real device.
            let upstream_phys = automap_fallback_dev.clone().or_else(|| {
                automap_source.as_ref().map(|(d, _)| d.clone())
                    .filter(|d| is_real_device_id(d))
            });
            let digital_trigger_bridge = upstream_phys
                .map(|d| digital_trigger_devs.contains(&d))
                .unwrap_or(false);

            Some(SinkTarget { device_id: sink_dev_id, pin_ids, multi_sources, automap_source, automap_fallback_dev, feedback_sources, is_self_sink: false, digital_trigger_bridge })
        } else {
            None
        };

        // For modules that read device signals by name, inject the originating device_id.
        let mut params = node.params.clone();
        if matches!(node.module_id.as_str(),
            "processing.gyro_3dof" | "module.automap_split"
            | "module.automap_fork" | "module.automap_selector"
            | "module.remapper" | "module.map_action"
            | "module.automap_collect" | "module.audio_stream_haptics"
            | "module.touch_zones" | "module.menu"
            | "module.network_send")
        {
            let automap_idx = node.inputs.iter().position(|p| p.signal_type == SignalType::AutoMap);
            if let Some(idx) = automap_idx {
                let pin = snarl.in_pin(InPinId { node: *node_id, input: idx });
                if let Some(&src) = pin.remotes.first() {
                    if let Some((dev_id, _, fallback)) = find_automap_device_rec(snarl, src, parents) {
                        // _automap_device_id = real physical device (fallback when upstream is a collector/forksel).
                        let real_id = fallback.unwrap_or_else(|| dev_id.clone());
                        params.insert("_automap_device_id".to_string(), serde_json::Value::String(real_id));
                        // _automap_collector_id = virtual collector key to read
                        // from collector_sigs first. The engine owns the list of
                        // node-produced source prefixes, so both sides classify
                        // a source the same way by construction.
                        if flexinput_engine::eval::is_namespaced_source(&dev_id) {
                            params.insert("_automap_collector_id".to_string(),
                                serde_json::Value::String(dev_id));
                        }
                    }
                }
            }
            // Selector: inject parallel dev / collector strings per port so
            // eval can read overrides from upstream Remapper/Collector/etc.
            // before falling back to raw device samples. Mirrors Combiner.
            if node.module_id == "module.automap_selector" {
                let mut extra_devs: Vec<serde_json::Value> = Vec::new();
                let mut extra_collectors: Vec<serde_json::Value> = Vec::new();
                for i in 1..node.inputs.len() {
                    let pin = snarl.in_pin(InPinId { node: *node_id, input: i });
                    let resolved = pin.remotes.first()
                        .and_then(|&src| find_automap_device_rec(snarl, src, parents));
                    let (dev_str, coll_str) = match resolved {
                        Some((dev_id, _, fallback)) => {
                            let is_collector = dev_id.starts_with("collector:")
                                || dev_id.starts_with("forksel:")
                                || dev_id.starts_with("remap:")
                                || dev_id.starts_with("touchmap:")
                                || dev_id.starts_with("menumap:")
                                || dev_id.starts_with("combiner:")
                                || dev_id.starts_with("lean:");
                            let dev = fallback.unwrap_or_else(|| if is_collector { String::new() } else { dev_id.clone() });
                            let coll = if is_collector { dev_id } else { String::new() };
                            (dev, coll)
                        }
                        None => (String::new(), String::new()),
                    };
                    extra_devs.push(serde_json::Value::String(dev_str));
                    extra_collectors.push(serde_json::Value::String(coll_str));
                }
                params.insert("_automap_input_devs".to_string(), serde_json::Value::Array(extra_devs));
                params.insert("_automap_input_collectors".to_string(), serde_json::Value::Array(extra_collectors));
            }
        }
        // Note: we intentionally do NOT mutate the source `snarl` here.
        // The injected `_automap_*` values are stored in the local `params`
        // and carried forward into the returned `NodeSnap.params`, which
        // the UI body renderers read at runtime. Mutating `snarl` would
        // require a mutable borrow of `snarl` which is not available here.
        // Combiner: all inputs are equal AutoMap buses (no select pin). Record
        // dev_id AND collector_id for each port so eval can read collector
        // overrides (Remapper / Collector / Selector / Fork) before falling
        // back to raw device samples. Parallel arrays indexed by port.
        if node.module_id == "module.automap_combiner" {
            let mut devs: Vec<serde_json::Value> = Vec::new();
            let mut collectors: Vec<serde_json::Value> = Vec::new();
            for i in 0..node.inputs.len() {
                let pin = snarl.in_pin(InPinId { node: *node_id, input: i });
                let resolved = pin.remotes.first()
                    .and_then(|&src| find_automap_device_rec(snarl, src, parents));
                let (dev_str, coll_str) = match resolved {
                    Some((dev_id, _, fallback)) => {
                        let is_collector = dev_id.starts_with("collector:")
                            || dev_id.starts_with("forksel:")
                            || dev_id.starts_with("remap:")
                            || dev_id.starts_with("touchmap:")
                            || dev_id.starts_with("menumap:")
                            || dev_id.starts_with("combiner:")
                            || dev_id.starts_with("lean:");
                        let dev = fallback.unwrap_or_else(|| if is_collector { String::new() } else { dev_id.clone() });
                        let coll = if is_collector { dev_id } else { String::new() };
                        (dev, coll)
                    }
                    None => (String::new(), String::new()),
                };
                devs.push(serde_json::Value::String(dev_str));
                collectors.push(serde_json::Value::String(coll_str));
            }
            params.insert("_automap_input_devs".to_string(), serde_json::Value::Array(devs));
            params.insert("_automap_input_collectors".to_string(), serde_json::Value::Array(collectors));
        }
        // For automap_collect: forward the stable pin-ID list so eval.rs can key collector_sigs.
        // IDs are stored separately in collect_input_pin_ids (parallel to inputs[1..]).
        if node.module_id == "module.automap_collect" {
            let collect_ids = node.params.get("collect_input_pin_ids")
                .and_then(|v| v.as_array()).cloned().unwrap_or_default();
            params.insert("_collect_pin_ids".to_string(), serde_json::Value::Array(collect_ids));
        }

        // Feedback Control: stamp the resolved physical source (inlet injection
        // target) and virtual destination (outlet tap source), plus the fixed
        // inlet/outlet pin-id lists so eval can key collector_sigs / dev_sigs.
        if node.module_id == "module.feedback_control" {
            // Inlet injection target: upstream physical pad on AutoMap input 0.
            let src_dev = {
                let pin = snarl.in_pin(InPinId { node: *node_id, input: 0 });
                pin.remotes.first()
                    .and_then(|&src| find_automap_device_rec(snarl, src, parents))
                    .map(|(dev_id, _, fallback)| {
                        if is_real_device_id(&dev_id) { dev_id } else { fallback.unwrap_or(dev_id) }
                    })
                    // Keep any resolved AutoMap target: a real device, or a
                    // synthetic id (collector:/forksel:/combiner:/remap:/lean:).
                    // Synthetic targets are drained either by the network recv
                    // feedback post-pass (collector:{recv_uid}) or reverse-forwarded
                    // to their upstream source (Selector/Fork) so Feedback Control
                    // placed after an AutoMap routing node still reaches the pad or
                    // network. An id nothing drains is harmless.
                    .filter(|d| !d.is_empty())
            };
            if let Some(d) = src_dev {
                params.insert("_fb_source_dev".to_string(), serde_json::Value::String(d));
            }
            // Outlet tap source: downstream virtual destination on AutoMap output 0.
            let dest_dev = {
                let out_pin = snarl.out_pin(OutPinId { node: *node_id, output: 0 });
                out_pin.remotes.iter().find_map(|&downstream| {
                    find_automap_dest_sink_rec(snarl, downstream, parents, 0)
                        .filter(|d| d.starts_with("virtual."))
                })
            };
            if let Some(d) = dest_dev {
                params.insert("_fb_dest_dev".to_string(), serde_json::Value::String(d));
            }
            // Fixed inlet/outlet pin-id lists (parallel to inputs[1..] / outputs[1..]).
            let inlet_ids: Vec<serde_json::Value> = flexinput_core::automap::FEEDBACK_INLET_PINS
                .iter().map(|p| serde_json::Value::String(p.id.to_string())).collect();
            let outlet_ids: Vec<serde_json::Value> = flexinput_core::automap::FEEDBACK_OUTLET_PINS
                .iter().map(|p| serde_json::Value::String(p.id.to_string())).collect();
            params.insert("_fb_inlet_ids".to_string(), serde_json::Value::Array(inlet_ids));
            params.insert("_fb_outlet_ids".to_string(), serde_json::Value::Array(outlet_ids));
        }

        // Audio Stream Haptics: stamp the physical pad the audio-derived rumble is
        // injected into — the upstream physical source on AutoMap input 0 (same
        // resolution as Feedback Control's `_fb_source_dev`). The eval block keys
        // `feedback_inject:{_asth_dest_dev}`, drained by the feedback post-pass.
        if node.module_id == "module.audio_stream_haptics" {
            let dest_dev = {
                let pin = snarl.in_pin(InPinId { node: *node_id, input: 0 });
                pin.remotes.first()
                    .and_then(|&src| find_automap_device_rec(snarl, src, parents))
                    .map(|(dev_id, _, fallback)| {
                        if is_real_device_id(&dev_id) { dev_id } else { fallback.unwrap_or(dev_id) }
                    })
                    // Keep any resolved AutoMap target (real device or synthetic
                    // collector:/forksel:/combiner:/… id), so Audio Stream Haptics
                    // after a Selector/Fork or before a network recv still reaches
                    // its destination. See the matching Feedback Control note above.
                    .filter(|d| !d.is_empty())
            };
            if let Some(d) = dest_dev {
                params.insert("_asth_dest_dev".to_string(), serde_json::Value::String(d));
            }
        }

        // Network Receive feedback: the downstream virtual sinks whose game rumble
        // ships back to the peer are discovered at EVAL time (from resolved
        // `sink_target.automap_source` ids), not stamped here — that traces across
        // sub-patch boundaries, which a per-level stamp can't. See
        // `collect_sink_sources` / `publish_recv_feedback_frames` in engine eval.

        // For subpatch nodes: recursively build the inner graph and locate outlet nodes.
        // The inner build receives a parent frame so any AutoMap traces from inner
        // Splitter / Collector nodes can pop back out through the inlets.
        let inline_subgraph = if node.module_id == "subpatch" {
            node.subpatch.as_ref().map(|sp| {
                let inner_frame = AutomapParent { snarl, subpatch_id: *node_id, prev: parents };
                let (inner_graph, _) = build_processing_graph_rec(&sp.snarl, Some(&inner_frame), defaults);
                let n_out = sp.pins_out.len();
                let mut outlet_locs: Vec<Option<(usize, usize)>> = vec![None; n_out];
                for (flat_idx, inner_snap) in inner_graph.nodes.iter().enumerate() {
                    if inner_snap.module_id == "subpatch.outlet" {
                        let pin_idx = inner_snap.params.get("pin_index")
                            .and_then(|v| v.as_u64()).unwrap_or(0) as usize;
                        if pin_idx < n_out {
                            outlet_locs[pin_idx] = Some((flat_idx, 0));
                        }
                    }
                }
                Box::new(InlineSubgraph { graph: inner_graph, outlet_locs })
            })
        } else {
            None
        };

        NodeSnap {
            node_uid: node_id.0,
            module_id: node.module_id.clone(),
            params,
            n_outputs: node.outputs.len(),
            input_sources,
            device_id,
            output_pin_ids,
            aux_f32_override,
            sink_target,
            inline_subgraph,
        }
    }).collect();

    // Topological sort (Kahn's algorithm).
    // Sink nodes are leaves (no node depends on them), so they naturally end up last.
    //
    // A device.source node with feedback inputs is both source and sink in one
    // physical node. Its sink-half (multi_sources) can legitimately receive a
    // wire that traces back to its own source-half — directly, or through a
    // Splitter/Math chain. That looks like a graph cycle but isn't a real
    // data-flow cycle: hardware reads happen at frame start (dev_sigs) and
    // hardware writes happen at frame end (sink_outputs), with no within-frame
    // dependency between them. We solve this by suppressing the sink-half's
    // incoming edges in the topo sort (so the source-half sorts early as a
    // pure leaf, releasing downstream consumers in Kahn), and eval runs a
    // second pass over self-sinks' multi_sources after the main loop, by which
    // time every upstream `computed[idx]` slot is filled.
    let n = snaps.len();
    let is_source_self_sink: Vec<bool> = snaps.iter().enumerate().map(|(idx, snap)| {
        if snap.module_id != "device.source" { return false; }
        let Some(ref st) = snap.sink_target else { return false; };
        // Direct self-wire: any multi_source pointing back to this node.
        if st.multi_sources.iter().any(|srcs| srcs.iter().any(|&(s, _)| s == idx)) {
            return true;
        }
        // Indirect self-wire: BFS over input_sources of upstream nodes — does any
        // path through the regular signal graph loop back to this node?
        let mut visited: HashSet<usize> = HashSet::new();
        let mut stack: Vec<usize> = st.multi_sources.iter()
            .flat_map(|srcs| srcs.iter().map(|&(s, _)| s))
            .collect();
        while let Some(cur) = stack.pop() {
            if cur == idx { return true; }
            if !visited.insert(cur) { continue; }
            if let Some(up) = snaps.get(cur) {
                for &(s, _) in up.input_sources.iter().flatten() {
                    stack.push(s);
                }
            }
        }
        false
    }).collect();

    // Propagate the detection back into each SinkTarget so eval can drive its
    // post-pass for these nodes.
    for (i, snap) in snaps.iter_mut().enumerate() {
        if is_source_self_sink[i] {
            if let Some(ref mut st) = snap.sink_target {
                st.is_self_sink = true;
            }
        }
    }

    let mut in_degree = vec![0usize; n];
    let mut dependents: Vec<Vec<usize>> = vec![vec![]; n];
    // Helper: parse an _automap_*_id-style string (e.g. "remap:42", "collector:7",
    // "forksel:3:0", "combiner:9") to the UID of the publishing node. Returns
    // None for empty strings and unrecognised prefixes.
    let uid_from_collector_id = |s: &str| -> Option<usize> {
        let stripped = s.strip_prefix("collector:")
            .or_else(|| s.strip_prefix("combiner:"))
            .or_else(|| s.strip_prefix("remap:"))
            .or_else(|| s.strip_prefix("touchmap:"))
            .or_else(|| s.strip_prefix("menumap:"))
            .or_else(|| s.strip_prefix("lean:"))
            .or_else(|| s.strip_prefix("forksel:").and_then(|t| t.split(':').next()))?;
        stripped.parse::<usize>().ok()
    };
    // Inside a sub-patch, `find_automap_device_rec` returns NAMESPACED uids
    // (folded via `namespaced_uid(outer_chain, node.uid)`) so they match the
    // keys the engine's subgraph eval publishes to. But `snap.node_uid` in
    // the inner snap list is the RAW uid (= NodeId.0), since the inner
    // build hasn't applied namespacing to its own snaps. Compare against
    // the raw-uid equivalent: strip the outer chain.
    let outer_chain_uid: Option<usize> = parents.map(fold_outer_uid);
    let match_inner_uid = |target_uid: usize, snap_uid: usize| -> bool {
        if snap_uid == target_uid { return true; }
        if let Some(outer) = outer_chain_uid {
            if flexinput_engine::namespaced_uid(outer, snap_uid) == target_uid {
                return true;
            }
        }
        false
    };
    for (idx, snap) in snaps.iter().enumerate() {
        // Regular nodes: single-source inputs.
        for &(src_idx, _) in snap.input_sources.iter().flatten() {
            dependents[src_idx].push(idx);
            in_degree[idx] += 1;
        }
        // AutoMap-consuming non-sinks (Combiner, Selector, Fork): the
        // `input_sources` chain only reaches the *immediate* upstream node, but
        // these consumers read from `collector_sigs` keyed by the originating
        // collector/remapper UID found by `find_automap_device_rec`. That UID
        // may belong to a node several hops upstream (e.g. through a Splitter)
        // or even inside a sub-patch — in either case the topo edge from the
        // immediate predecessor is not enough to guarantee the collector
        // publishes before this node reads. Add explicit deps so the
        // Remapper / Collector / Combiner / Fork that backs each AutoMap input
        // is scheduled before this consumer.
        let is_am_consumer = matches!(snap.module_id.as_str(),
            "module.automap_combiner"
            | "module.automap_selector"
            | "module.automap_fork"
            | "module.automap_split"
            | "module.automap_collect");
        if is_am_consumer {
            let mut seen: HashSet<usize> = HashSet::new();
            // Combiner: per-port collector IDs are pre-baked in
            // `_automap_input_collectors`. Use them directly to avoid a second
            // call into `find_automap_device_rec`.
            let collector_ids: Vec<String> = snap.params.get("_automap_input_collectors")
                .and_then(|v| v.as_array())
                .map(|a| a.iter().map(|v| v.as_str().unwrap_or("").to_string()).collect())
                .unwrap_or_default();
            for cid in &collector_ids {
                if let Some(uid) = uid_from_collector_id(cid) {
                    if let Some(am_idx) = snaps.iter().position(|s| match_inner_uid(uid, s.node_uid)) {
                        if am_idx != idx && seen.insert(am_idx) {
                            dependents[am_idx].push(idx);
                            in_degree[idx] += 1;
                        }
                    }
                }
            }
            // Fallback: walk every AutoMap input pin and trace through. Catches
            // Selector / Fork / Split (which don't populate
            // `_automap_input_collectors`) and any case where the param array
            // is missing or stale.
            let (outer_node_id, outer_node) = node_list[idx];
            for i in 0..outer_node.inputs.len() {
                if outer_node.inputs.get(i).map(|p| p.signal_type) != Some(SignalType::AutoMap) {
                    continue;
                }
                let pin = snarl.in_pin(InPinId { node: outer_node_id, input: i });
                let Some(&src) = pin.remotes.first() else { continue };
                let Some((am_dev_id, _, _)) = find_automap_device_rec(snarl, src, parents) else { continue };
                let Some(uid) = uid_from_collector_id(&am_dev_id) else { continue };
                let Some(am_idx) = snaps.iter().position(|s| match_inner_uid(uid, s.node_uid)) else { continue };
                if am_idx != idx && seen.insert(am_idx) {
                    dependents[am_idx].push(idx);
                    in_degree[idx] += 1;
                }
            }
        }
        // Sink nodes: multi-source inputs (deduplicated per source node to avoid double-counting).
        // Skip for device.source self-sinks: their sink-half is handled in a
        // post-pass during eval, so we don't add the cycle-inducing incoming edges here.
        if !is_source_self_sink[idx] {
        if let Some(ref st) = snap.sink_target {
            let mut seen: HashSet<usize> = HashSet::new();
            for sources in &st.multi_sources {
                for &(src_idx, _) in sources {
                    if seen.insert(src_idx) {
                        dependents[src_idx].push(idx);
                        in_degree[idx] += 1;
                    }
                }
            }
            // If the AutoMap source is a Collector / Fork / Selector / Combiner / Remapper,
            // add it as a dependency so it is evaluated before this sink (ensuring
            // collector_sigs is populated).
            if let Some((ref am_dev_id, _)) = st.automap_source {
                // "collector:{uid}" → automap_collect node
                // "forksel:{uid}:{out}" → automap_fork or automap_selector node
                // "combiner:{uid}" → automap_combiner node
                // "remap:{uid}" → remapper node
                let uid_str = am_dev_id.strip_prefix("collector:")
                    .or_else(|| am_dev_id.strip_prefix("combiner:"))
                    .or_else(|| am_dev_id.strip_prefix("remap:"))
                    .or_else(|| am_dev_id.strip_prefix("touchmap:"))
                    .or_else(|| am_dev_id.strip_prefix("menumap:"))
                    .or_else(|| am_dev_id.strip_prefix("forksel:").and_then(|s| s.split(':').next()));
                if let Some(uid_str) = uid_str {
                    if let Ok(uid) = uid_str.parse::<usize>() {
                        if let Some(am_idx) = snaps.iter().position(|s| match_inner_uid(uid, s.node_uid)) {
                            if seen.insert(am_idx) {
                                dependents[am_idx].push(idx);
                                in_degree[idx] += 1;
                            }
                        }
                    }
                }
            }
            // Also depend on the immediate outer-snarl source of every AutoMap
            // input pin. Catches subpatches whose inner graph contains a Collector
            // (whose namespaced UID isn't in `snaps`) — depending on the outer
            // subpatch node still guarantees its inner eval runs and populates
            // collector_sigs before this sink reads it.
            let (outer_node_id, outer_node) = node_list[idx];
            for i in 0..outer_node.inputs.len() {
                if outer_node.inputs.get(i).map(|p| p.signal_type) != Some(SignalType::AutoMap) {
                    continue;
                }
                let pin = snarl.in_pin(InPinId { node: outer_node_id, input: i });
                if let Some(&src) = pin.remotes.first() {
                    if let Some(&src_idx) = id_to_orig.get(&src.node) {
                        if seen.insert(src_idx) {
                            dependents[src_idx].push(idx);
                            in_degree[idx] += 1;
                        }
                    }
                }
            }
        }
        } // end !is_source_self_sink guard
    }

    // ── Macro-port ordering edges ─────────────────────────────────────────
    // module.macro nodes have no wired inputs, so the plain topo sort would
    // run them FIRST — before the mapping evaluators (Remapper / Touch Zones
    // cards / 3DOF-Lean) publish this tick's macro values into the shared
    // macro namespace. Add explicit publisher → macro edges so every macro
    // reader evaluates after every potential publisher. Sub-patch nodes count
    // on both sides (their inner graphs may contain either kind); a subpatch
    // containing both is publisher AND reader, and the self-edge is skipped.
    // A mutual pair of such subpatches would cycle — the topo fallback below
    // appends them in insertion order, costing one tick of macro latency but
    // still evaluating everything.
    fn subpatch_has_module(sp: &UiSubPatch, pred: &dyn Fn(&str) -> bool) -> bool {
        sp.snarl.nodes_ids_data().any(|(_, n)| {
            pred(&n.value.module_id)
                || n.value.subpatch.as_deref().is_some_and(|inner| subpatch_has_module(inner, pred))
        })
    }
    let is_macro_publisher = |mid: &str| matches!(mid,
        "module.remapper" | "module.touch_zones" | "processing.gyro_3dof");
    // Virtual Menus read their macro-style Show/Select targets from the same
    // namespaces, so they order after the publishers exactly like Macro nodes.
    let is_macro_reader = |mid: &str| mid == "module.macro" || mid == "module.menu";
    let macro_publishers: Vec<usize> = node_list.iter().enumerate().filter(|(_, (_, node))| {
        is_macro_publisher(&node.module_id)
            || node.subpatch.as_deref().is_some_and(|sp| subpatch_has_module(sp, &is_macro_publisher))
    }).map(|(i, _)| i).collect();
    let macro_readers: Vec<usize> = node_list.iter().enumerate().filter(|(_, (_, node))| {
        is_macro_reader(&node.module_id)
            || node.subpatch.as_deref().is_some_and(|sp| subpatch_has_module(sp, &is_macro_reader))
    }).map(|(i, _)| i).collect();
    if !macro_readers.is_empty() {
        for &p in &macro_publishers {
            for &m in &macro_readers {
                if p == m { continue; }
                dependents[p].push(m);
                in_degree[m] += 1;
            }
        }
    }

    let mut queue: VecDeque<usize> = (0..n).filter(|&i| in_degree[i] == 0).collect();
    let mut sorted: Vec<usize> = Vec::with_capacity(n);
    while let Some(idx) = queue.pop_front() {
        sorted.push(idx);
        for &dep in &dependents[idx] {
            in_degree[dep] -= 1;
            if in_degree[dep] == 0 { queue.push_back(dep); }
        }
    }
    // Append any remaining nodes (cycles — shouldn't happen in practice).
    for i in 0..n { if !sorted.contains(&i) { sorted.push(i); } }

    // Remap indices from original order → sorted order.
    let mut orig_to_sorted = vec![0usize; n];
    for (new_idx, &orig) in sorted.iter().enumerate() { orig_to_sorted[orig] = new_idx; }

    let nodes = sorted.iter().map(|&orig| {
        let mut snap = snaps[orig].clone();
        // Remap single-source inputs.
        for src in snap.input_sources.iter_mut().flatten() { src.0 = orig_to_sorted[src.0]; }
        // Remap multi-source inputs for sink nodes.
        if let Some(ref mut st) = snap.sink_target {
            for sources in &mut st.multi_sources {
                for src in sources.iter_mut() { src.0 = orig_to_sorted[src.0]; }
            }
        }
        snap
    }).collect();

    (ProcessingGraph { nodes }, dirty_uids)
}
