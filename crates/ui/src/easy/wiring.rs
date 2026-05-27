//! Auto-Map–first wiring helper for Easy mode.
//!
//! See PLAN §7. Connects the lone `device.source` → the subpatch's
//! first AutoMap inlet, and the subpatch's first AutoMap outlet →
//! every active `device.sink`. Existing wires that touch any of those
//! nodes are dropped before rewiring.
//!
//! Presets that don't declare at least one AutoMap-typed inlet and
//! one AutoMap-typed outlet aren't usable in Easy mode — for those
//! the rewire is a no-op on the relevant leg.

use egui_snarl::{InPinId, NodeId, OutPinId};
use flexinput_core::SignalType;

use crate::canvas::{Canvas, NodeData};

/// Recompute Easy-mode wiring on the canvas.
///
/// Strategy:
/// 1. Find the (at most one) `device.source`, the (at most one)
///    `subpatch`, and any `device.sink` nodes.
/// 2. Drop every wire that touches any of those nodes.
/// 3. Locate Auto-Map pins by signal_type — `source.outputs` for the
///    physical input, `sink.inputs` for each virtual output, and the
///    subpatch node's own outer pins (`inputs` / `outputs` on the
///    snarl-side NodeData mirror the subpatch's declared
///    `pins_in` / `pins_out`).
/// 4. Wire source.AutoMap-out → subpatch.AutoMap-in[0] and
///    subpatch.AutoMap-out[0] → each sink.AutoMap-in.
pub fn rewire(canvas: &mut Canvas) {
    let snarl = &mut canvas.snarl;

    let mut source: Option<NodeId> = None;
    let mut subpatch: Option<NodeId> = None;
    let mut sinks: Vec<NodeId> = Vec::new();
    for (id, node) in snarl.nodes_ids_data() {
        match node.value.module_id.as_str() {
            "device.source" => source = Some(id),
            "subpatch"      => subpatch = Some(id),
            "device.sink"   => sinks.push(id),
            _ => {}
        }
    }

    let touched: std::collections::HashSet<NodeId> = source.iter().copied()
        .chain(subpatch.iter().copied())
        .chain(sinks.iter().copied())
        .collect();

    let wires_to_drop: Vec<(OutPinId, InPinId)> = snarl.wires()
        .filter(|(o, i)| touched.contains(&o.node) || touched.contains(&i.node))
        .collect();
    for (o, i) in wires_to_drop {
        snarl.disconnect(o, i);
    }

    let Some(sp_id) = subpatch else { return; };

    if let Some(src_id) = source {
        if let (Some(src_pin), Some(sp_in_pin)) = (
            first_automap_output_idx(snarl.get_node(src_id)),
            first_automap_input_idx(snarl.get_node(sp_id)),
        ) {
            snarl.connect(
                OutPinId { node: src_id, output: src_pin },
                InPinId  { node: sp_id,  input:  sp_in_pin },
            );
        }
    }

    if let Some(sp_out_pin) = first_automap_output_idx(snarl.get_node(sp_id)) {
        for sink_id in &sinks {
            if let Some(sink_in_pin) = first_automap_input_idx(snarl.get_node(*sink_id)) {
                snarl.connect(
                    OutPinId { node: sp_id,   output: sp_out_pin },
                    InPinId  { node: *sink_id, input:  sink_in_pin },
                );
            }
        }
    }
}

fn first_automap_input_idx(node: Option<&NodeData>) -> Option<usize> {
    node?.inputs.iter().position(|p| p.signal_type == SignalType::AutoMap)
}

fn first_automap_output_idx(node: Option<&NodeData>) -> Option<usize> {
    node?.outputs.iter().position(|p| p.signal_type == SignalType::AutoMap)
}
