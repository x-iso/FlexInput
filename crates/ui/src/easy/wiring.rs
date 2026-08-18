//! Auto-Map–first wiring helper for Easy mode.
//!
//! See PLAN §7. Connects each `device.source` → one of the subpatch's
//! AutoMap inlets, and the subpatch's first AutoMap outlet →
//! every active `device.sink`. Existing wires that touch any of those
//! nodes are dropped before rewiring.
//!
//! **One device per inlet, never several fanned into one.** A preset declaring
//! N AutoMap inlets accepts up to N input devices, and each keeps its own port.
//! Merging them would force every downstream consumer to work out which device a
//! signal came from, leaking that concern into curves and gates that have no
//! business knowing — whereas a port keeps device identity in the WIRE, so a
//! Remapper reading inlet 2 simply knows it is player 2.
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
/// 1. Find every `device.source`, the (at most one) `subpatch`, and any
///    `device.sink` nodes.
/// 2. Drop every wire that touches any of those nodes.
/// 3. Locate Auto-Map pins by signal_type — `source.outputs` for the
///    physical input, `sink.inputs` for each virtual output, and the
///    subpatch node's own outer pins (`inputs` / `outputs` on the
///    snarl-side NodeData mirror the subpatch's declared
///    `pins_in` / `pins_out`).
/// 4. Wire source[i].AutoMap-out → subpatch.AutoMap-in[i] and
///    subpatch.AutoMap-out[0] → each sink.AutoMap-in.
pub fn rewire(canvas: &mut Canvas) {
    let snarl = &mut canvas.snarl;

    // (port, node). The port comes from the node's own `automap_port` param so
    // it survives save/reload and node reordering; without it, which device
    // landed on which inlet would depend on snarl's iteration order and could
    // silently repoint a saved patch at a different player.
    let mut sources: Vec<(usize, NodeId)> = Vec::new();
    let mut subpatch: Option<NodeId> = None;
    let mut sinks: Vec<NodeId> = Vec::new();
    for (id, node) in snarl.nodes_ids_data() {
        match node.value.module_id.as_str() {
            "device.source" => sources.push((source_port(&node.value), id)),
            "subpatch"      => subpatch = Some(id),
            "device.sink"   => sinks.push(id),
            _ => {}
        }
    }
    sources.sort_by_key(|(port, _)| *port);

    let touched: std::collections::HashSet<NodeId> = sources.iter().map(|(_, id)| *id)
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

    // Each source gets its own inlet, in port order. Surplus devices beyond the
    // preset's inlet count are left unwired rather than doubled up on the last
    // inlet — silently sharing a port would look like it worked and then
    // deliver two devices' signals fighting over the same pins.
    let sp_inlets = automap_input_indices(snarl.get_node(sp_id));
    for (slot, (_, src_id)) in sources.iter().enumerate() {
        let (Some(src_pin), Some(sp_in_pin)) =
            (first_automap_output_idx(snarl.get_node(*src_id)), sp_inlets.get(slot).copied())
        else {
            continue;
        };
        snarl.connect(
            OutPinId { node: *src_id, output: src_pin },
            InPinId  { node: sp_id,   input:  sp_in_pin },
        );
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

/// Which AutoMap port a source node occupies.
///
/// Defaults to 0 so a patch saved before multi-device support still wires its
/// single source to the first inlet exactly as it used to.
fn source_port(node: &NodeData) -> usize {
    node.params
        .get("automap_port")
        .and_then(|v| v.as_u64())
        .unwrap_or(0) as usize
}

/// Every AutoMap inlet on a node, in declaration order — this is what caps how
/// many input devices a preset accepts.
pub fn automap_input_indices(node: Option<&NodeData>) -> Vec<usize> {
    node.map(|n| {
        n.inputs.iter().enumerate()
            .filter(|(_, p)| p.signal_type == SignalType::AutoMap)
            .map(|(i, _)| i)
            .collect()
    })
    .unwrap_or_default()
}

fn first_automap_input_idx(node: Option<&NodeData>) -> Option<usize> {
    node?.inputs.iter().position(|p| p.signal_type == SignalType::AutoMap)
}

fn first_automap_output_idx(node: Option<&NodeData>) -> Option<usize> {
    node?.outputs.iter().position(|p| p.signal_type == SignalType::AutoMap)
}

#[cfg(test)]
mod tests {
    use super::*;
    use flexinput_core::PinDescriptor;

    fn node(inputs: Vec<SignalType>, port: Option<u64>) -> NodeData {
        let mut params = std::collections::HashMap::new();
        if let Some(p) = port {
            params.insert("automap_port".to_string(), serde_json::Value::from(p));
        }
        NodeData {
            module_id: "device.source".into(),
            display_name: String::new(),
            category: String::new(),
            inputs: inputs.into_iter().map(|t| PinDescriptor::new("p", t)).collect(),
            outputs: Vec::new(),
            params,
            subpatch: None,
            extra: Default::default(),
        }
    }

    /// A preset's AutoMap inlet count is what caps how many input devices it
    /// accepts, and the indices must be the REAL pin positions — non-AutoMap
    /// pins in between would otherwise shift every wire onto the wrong pin.
    #[test]
    fn automap_inlets_are_found_by_type_not_position() {
        let n = node(
            vec![SignalType::Float, SignalType::AutoMap, SignalType::Bool, SignalType::AutoMap],
            None,
        );
        assert_eq!(automap_input_indices(Some(&n)), vec![1, 3]);
    }

    #[test]
    fn a_preset_with_no_automap_inlet_accepts_nothing() {
        let n = node(vec![SignalType::Float], None);
        assert!(automap_input_indices(Some(&n)).is_empty());
    }

    /// Patches saved before multi-device support carry no port param. They must
    /// keep wiring their single source to inlet 0 rather than landing somewhere
    /// arbitrary.
    #[test]
    fn a_source_without_a_port_param_defaults_to_inlet_zero() {
        assert_eq!(source_port(&node(Vec::new(), None)), 0);
    }

    #[test]
    fn an_explicit_port_is_honoured() {
        assert_eq!(source_port(&node(Vec::new(), Some(2))), 2);
    }
}
