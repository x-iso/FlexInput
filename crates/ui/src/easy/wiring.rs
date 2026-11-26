//! Auto-Map–first wiring helper for Easy mode.
//!
//! See PLAN §7. Connects each of Easy mode's `device.source`s → one of the
//! PRIMARY sub-patch's AutoMap inlets, and that sub-patch's first AutoMap
//! outlet → each of Easy mode's `device.sink`s (`subpatches::primary`).
//!
//! **Only Easy mode's own wires.** A device node is Easy mode's when the primary
//! is what its AutoMap pin is wired to (a source) or from (a sink), or when
//! nothing at all is wired to it yet - one the panel just added. Every other
//! wire is Advanced mode's and left alone: a sub-patch beside the primary, a
//! module fed from a pad, an AutoMap inlet of the primary that a module feeds
//! (no device can take that one). The old rewire dropped every wire touching
//! any device node or the sub-patch, which was only harmless while Easy mode
//! refused any canvas but its own.
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

use egui_snarl::{InPinId, NodeId, OutPinId, Snarl};
use flexinput_core::SignalType;

use super::subpatches;
use crate::canvas::{Canvas, NodeData};

/// Easy mode's device nodes on a canvas: those it wires into and out of the
/// primary sub-patch.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct Managed {
    pub sources: Vec<NodeId>,
    pub sinks: Vec<NodeId>,
}

impl Managed {
    pub fn of(snarl: &Snarl<NodeData>, primary: Option<NodeId>) -> Self {
        Self { sources: managed_sources(snarl, primary), sinks: managed_sinks(snarl, primary) }
    }
}

fn has_any_wire(snarl: &Snarl<NodeData>, id: NodeId) -> bool {
    snarl.wires().any(|(o, i)| o.node == id || i.node == id)
}

/// Whether source `id`'s AutoMap output feeds sub-patch `p`.
fn source_feeds(snarl: &Snarl<NodeData>, id: NodeId, p: NodeId) -> bool {
    first_automap_output_idx(snarl.get_node(id))
        .is_some_and(|pin| snarl.out_pin(OutPinId { node: id, output: pin }).remotes.iter().any(|r| r.node == p))
}

/// Whether sink `id`'s AutoMap input is fed by sub-patch `p`.
fn sink_fed_by(snarl: &Snarl<NodeData>, id: NodeId, p: NodeId) -> bool {
    first_automap_input_idx(snarl.get_node(id))
        .is_some_and(|pin| snarl.in_pin(InPinId { node: id, input: pin }).remotes.iter().any(|r| r.node == p))
}

/// Easy mode's input devices: sources whose AutoMap output feeds `primary`, and
/// sources nothing is wired to.
pub fn managed_sources(snarl: &Snarl<NodeData>, primary: Option<NodeId>) -> Vec<NodeId> {
    snarl
        .nodes_ids_data()
        .filter(|(_, n)| n.value.module_id == "device.source")
        .map(|(id, _)| id)
        .filter(|&id| primary.is_some_and(|p| source_feeds(snarl, id, p)) || !has_any_wire(snarl, id))
        .collect()
}

/// Easy mode's output devices: sinks whose AutoMap input `primary` feeds, and
/// sinks nothing is wired to.
pub fn managed_sinks(snarl: &Snarl<NodeData>, primary: Option<NodeId>) -> Vec<NodeId> {
    snarl
        .nodes_ids_data()
        .filter(|(_, n)| n.value.module_id == "device.sink")
        .map(|(id, _)| id)
        .filter(|&id| primary.is_some_and(|p| sink_fed_by(snarl, id, p)) || !has_any_wire(snarl, id))
        .collect()
}

/// Recompute Easy-mode wiring on the canvas.
///
/// Strategy:
/// 1. Find the primary sub-patch and Easy mode's device nodes ([`Managed`]).
/// 2. Drop Easy mode's own wires: those device nodes' AutoMap wires to and
///    from the primary. Nothing else.
/// 3. Locate Auto-Map pins by signal_type — `source.outputs` for the
///    physical input, `sink.inputs` for each virtual output, and the
///    subpatch node's own outer pins (`inputs` / `outputs` on the
///    snarl-side NodeData mirror the subpatch's declared
///    `pins_in` / `pins_out`).
/// 4. Wire each source's AutoMap-out → the primary's AutoMap inlet its
///    `automap_port` names (see [`resolve_ports`]) - never one another wire
///    already feeds - and primary.AutoMap-out[0] → each sink.AutoMap-in.
pub fn rewire(canvas: &mut Canvas) {
    let Some(p) = subpatches::primary(&canvas.snarl) else { return };
    let managed = Managed::of(&canvas.snarl, Some(p));
    rewire_into(&mut canvas.snarl, p, &managed);
}

/// Make `id` the primary sub-patch, and move Easy mode's devices along: off the
/// old primary's ports and onto the new one's (each on its port number where it
/// has one), and the output devices onto its outlet.
pub fn move_primary(snarl: &mut Snarl<NodeData>, id: NodeId) {
    let old = subpatches::primary(snarl);
    let mut managed = Managed::of(snarl, old);
    if let Some(old) = old.filter(|&o| o != id) {
        drop_easy_wires(snarl, old, &managed);
    }
    subpatches::set_primary(snarl, id);
    let more = Managed::of(snarl, Some(id));
    for s in more.sources {
        if !managed.sources.contains(&s) {
            managed.sources.push(s);
        }
    }
    for s in more.sinks {
        if !managed.sinks.contains(&s) {
            managed.sinks.push(s);
        }
    }
    rewire_into(snarl, id, &managed);
}

/// Easy mode's wires into and out of sub-patch `p`: each managed source's AutoMap
/// output into it, and its outlet into each managed sink's AutoMap input.
fn easy_wires(snarl: &Snarl<NodeData>, p: NodeId, m: &Managed) -> Vec<(OutPinId, InPinId)> {
    snarl
        .wires()
        .filter(|(o, i)| {
            (i.node == p
                && m.sources.contains(&o.node)
                && first_automap_output_idx(snarl.get_node(o.node)) == Some(o.output))
                || (o.node == p
                    && m.sinks.contains(&i.node)
                    && first_automap_input_idx(snarl.get_node(i.node)) == Some(i.input))
        })
        .collect()
}

fn drop_easy_wires(snarl: &mut Snarl<NodeData>, p: NodeId, m: &Managed) {
    for (o, i) in easy_wires(snarl, p, m) {
        snarl.disconnect(o, i);
    }
}

fn rewire_into(snarl: &mut Snarl<NodeData>, sp_id: NodeId, m: &Managed) {
    drop_easy_wires(snarl, sp_id, m);

    // (port, node). The port comes from the node's own `automap_port` param so
    // it survives save/reload and node reordering; without it, which device
    // landed on which inlet would depend on snarl's iteration order and could
    // silently repoint a saved patch at a different player.
    let mut sources: Vec<(usize, NodeId)> = m
        .sources
        .iter()
        .filter_map(|&id| snarl.get_node(id).map(|n| (source_port(n), id)))
        .collect();
    sources.sort_by_key(|(port, id)| (*port, id.0));

    // Each source gets its own inlet: the port it asked for when that one is
    // free, else the lowest free one. Surplus devices beyond the preset's inlet
    // count are left unwired rather than doubled up on the last inlet —
    // silently sharing a port would look like it worked and then deliver two
    // devices' signals fighting over the same pins. An inlet a wire from
    // Advanced mode feeds is no device's.
    let sp_inlets = automap_input_indices(snarl.get_node(sp_id));
    let free: Vec<bool> = sp_inlets
        .iter()
        .map(|&pin| snarl.in_pin(InPinId { node: sp_id, input: pin }).remotes.is_empty())
        .collect();
    let wanted: Vec<usize> = sources.iter().map(|(port, _)| *port).collect();
    let slots = resolve_free_ports(&wanted, &free);
    for ((_, src_id), slot) in sources.iter().zip(slots) {
        let Some(slot) = slot else { continue; };
        // Store where it actually landed, so the Port row, a later swap and the
        // next rewire all agree on it.
        if let Some(n) = snarl.get_node_mut(*src_id) {
            n.params.insert("automap_port".into(), serde_json::Value::from(slot as u64));
        }
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
        for sink_id in &m.sinks {
            let Some(sink_in_pin) = first_automap_input_idx(snarl.get_node(*sink_id)) else { continue };
            let to = InPinId { node: *sink_id, input: sink_in_pin };
            // Never a second wire into an input another one already feeds.
            if snarl.in_pin(to).remotes.is_empty() {
                snarl.connect(OutPinId { node: sp_id, output: sp_out_pin }, to);
            }
        }
    }
}

/// Wire pad `src` into the primary with Easy mode's own pads, though Advanced
/// mode wires it somewhere else as well: it was just picked in the Devices panel.
/// From then on its wire into the primary makes it one of Easy mode's.
pub fn adopt_source(canvas: &mut Canvas, src: NodeId) {
    let Some(p) = subpatches::primary(&canvas.snarl) else { return };
    let mut m = Managed::of(&canvas.snarl, Some(p));
    if !m.sources.contains(&src) {
        m.sources.push(src);
    }
    rewire_into(&mut canvas.snarl, p, &m);
}

/// Take input device `src` off the primary. A node nothing else is wired to goes
/// (its calibration stays with the tab, for when it's picked again); one Advanced
/// mode also wires elsewhere stays, for those wires.
pub fn release_source(canvas: &mut Canvas, src: NodeId) {
    if let Some(p) = subpatches::primary(&canvas.snarl) {
        let m = Managed { sources: vec![src], sinks: Vec::new() };
        drop_easy_wires(&mut canvas.snarl, p, &m);
    }
    if !has_any_wire(&canvas.snarl, src) {
        canvas.remember_device_source(src);
        canvas.snarl.remove_node(src);
    }
}

/// Take output device `sink` off the primary: the node goes unless Advanced mode
/// also wires it elsewhere.
pub fn release_sink(canvas: &mut Canvas, sink: NodeId) {
    if let Some(p) = subpatches::primary(&canvas.snarl) {
        let m = Managed { sources: Vec::new(), sinks: vec![sink] };
        drop_easy_wires(&mut canvas.snarl, p, &m);
    }
    if !has_any_wire(&canvas.snarl, sink) {
        canvas.snarl.remove_node(sink);
    }
}

/// The primary's AutoMap inlets a wire from outside Easy mode feeds - from a
/// module, or a pad wired in Advanced mode - in inlet order. No device takes
/// those.
pub fn held_inlets(canvas: &Canvas) -> Vec<bool> {
    let snarl = &canvas.snarl;
    let Some(p) = subpatches::primary(snarl) else { return Vec::new() };
    let sources = managed_sources(snarl, Some(p));
    automap_input_indices(snarl.get_node(p))
        .into_iter()
        .map(|pin| {
            snarl
                .in_pin(InPinId { node: p, input: pin })
                .remotes
                .iter()
                .any(|r| !sources.contains(&r.node))
        })
        .collect()
}

/// Drop every wire on `id` whose pin no longer exists - after its pins were
/// replaced by a preset's.
pub fn drop_stale_wires(snarl: &mut Snarl<NodeData>, id: NodeId) {
    let Some(n) = snarl.get_node(id) else { return };
    let (ins, outs) = (n.inputs.len(), n.outputs.len());
    let stale: Vec<(OutPinId, InPinId)> = snarl
        .wires()
        .filter(|(o, i)| (i.node == id && i.input >= ins) || (o.node == id && o.output >= outs))
        .collect();
    for (o, i) in stale {
        snarl.disconnect(o, i);
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

/// The inlet slot each source lands on, given the ports they ask for (in the
/// same order) and how many AutoMap inlets the preset declares.
///
/// A requested port is honoured when it exists and nobody earlier claimed it;
/// everyone else takes the lowest free slot, and once the inlets run out the
/// rest get `None`. So an explicit pick from the Port row sticks — a device on
/// port 2 stays there when the device on port 1 is removed — while a patch
/// whose ports no longer fit (a preset swapped for one with fewer inlets, or an
/// old save where every source defaulted to 0) still packs onto what exists.
#[cfg(test)]
pub(crate) fn resolve_ports(wanted: &[usize], inlet_count: usize) -> Vec<Option<usize>> {
    resolve_free_ports(wanted, &vec![true; inlet_count])
}

/// [`resolve_ports`] over the inlets `free` marks takeable (one per inlet).
pub(crate) fn resolve_free_ports(wanted: &[usize], free: &[bool]) -> Vec<Option<usize>> {
    let mut order: Vec<usize> = (0..wanted.len()).collect();
    order.sort_by_key(|&i| wanted[i]);
    let mut taken: Vec<bool> = free.iter().map(|f| !f).collect();
    let mut slots = vec![None; wanted.len()];
    for &i in &order {
        let p = wanted[i];
        if p < taken.len() && !taken[p] {
            taken[p] = true;
            slots[i] = Some(p);
        }
    }
    for &i in &order {
        if slots[i].is_some() { continue; }
        if let Some(f) = taken.iter().position(|t| !t) {
            taken[f] = true;
            slots[i] = Some(f);
        }
    }
    slots
}

/// Which AutoMap port of the primary sub-patch a source node is wired into, read
/// off the wire itself — `None` when it is unwired (surplus on a full preset).
pub fn source_slot(canvas: &Canvas, src: NodeId) -> Option<usize> {
    let snarl = &canvas.snarl;
    let sp_id = subpatches::primary(snarl)?;
    let sp_inlets = automap_input_indices(snarl.get_node(sp_id));
    snarl.wires()
        .filter(|(o, i)| o.node == src && i.node == sp_id)
        .find_map(|(_, i)| sp_inlets.iter().position(|&pin| pin == i.input))
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

    /// The Port row's pick has to survive the rewire: a lone device on port 2
    /// must not slide down to port 0 just because 0 and 1 are empty.
    #[test]
    fn a_requested_free_port_is_kept() {
        assert_eq!(resolve_ports(&[2], 3), vec![Some(2)]);
        assert_eq!(resolve_ports(&[1, 0], 2), vec![Some(1), Some(0)]);
    }

    /// Old saves default every source to 0, and a swapped-in preset may have
    /// fewer inlets than the stored ports: both pack onto the free inlets, and
    /// once those run out the rest stay unwired rather than sharing one.
    #[test]
    fn clashing_or_missing_ports_pack_and_surplus_stays_unwired() {
        assert_eq!(resolve_ports(&[0, 0], 2), vec![Some(0), Some(1)]);
        assert_eq!(resolve_ports(&[3, 1], 2), vec![Some(0), Some(1)]);
        assert_eq!(resolve_ports(&[0, 0, 1], 2), vec![Some(0), None, Some(1)]);
        assert_eq!(resolve_ports(&[0], 0), vec![None]);
    }

    /// An inlet Advanced mode feeds is never a device's, asked for or not; the pad
    /// that asked for the free one keeps it.
    #[test]
    fn a_held_inlet_is_skipped() {
        assert_eq!(resolve_free_ports(&[0], &[false, true]), vec![Some(1)]);
        assert_eq!(resolve_free_ports(&[0, 1], &[false, true]), vec![None, Some(1)]);
    }

    // ── On a canvas ──────────────────────────────────────────────────

    fn am_node(module_id: &str, ins: usize, outs: usize) -> NodeData {
        NodeData {
            module_id: module_id.into(),
            display_name: String::new(),
            category: String::new(),
            inputs: (0..ins).map(|_| PinDescriptor::new("in", SignalType::AutoMap)).collect(),
            outputs: (0..outs).map(|_| PinDescriptor::new("out", SignalType::AutoMap)).collect(),
            params: Default::default(),
            subpatch: (module_id == "subpatch").then(Default::default),
            extra: Default::default(),
        }
    }

    fn at(canvas: &mut Canvas, n: NodeData) -> NodeId {
        canvas.snarl.insert_node(eframe::egui::pos2(0.0, 0.0), n)
    }

    fn wired(canvas: &Canvas, from: NodeId, to: NodeId) -> bool {
        canvas.snarl.wires().any(|(o, i)| o.node == from && i.node == to)
    }

    /// The Easy patch as it always was: the pad into the sub-patch, the sub-patch
    /// into the virtual pad.
    #[test]
    fn an_easy_patch_wires_as_before() {
        let mut c = Canvas::default();
        let sp = at(&mut c, am_node("subpatch", 2, 1));
        let a = at(&mut c, am_node("device.source", 0, 1));
        let b = at(&mut c, am_node("device.source", 0, 1));
        let out = at(&mut c, am_node("device.sink", 1, 0));
        rewire(&mut c);
        assert!(wired(&c, a, sp) && wired(&c, b, sp) && wired(&c, sp, out));
        assert_eq!(source_slot(&c, a), Some(0));
        assert_eq!(source_slot(&c, b), Some(1));
    }

    /// What Advanced mode wired stays wired: a second sub-patch fed by its own pad
    /// into its own output, and a module feeding the primary's first inlet - which
    /// the Easy pad then can't take.
    #[test]
    fn advanced_wiring_is_left_alone() {
        let mut c = Canvas::default();
        let primary = at(&mut c, am_node("subpatch", 2, 1));
        let other = at(&mut c, am_node("subpatch", 1, 1));
        let fork = at(&mut c, am_node("module.automap_fork", 0, 1));
        let other_pad = at(&mut c, am_node("device.source", 0, 1));
        let other_out = at(&mut c, am_node("device.sink", 1, 0));
        let easy_pad = at(&mut c, am_node("device.source", 0, 1));
        let easy_out = at(&mut c, am_node("device.sink", 1, 0));
        c.snarl.connect(OutPinId { node: other_pad, output: 0 }, InPinId { node: other, input: 0 });
        c.snarl.connect(OutPinId { node: other, output: 0 }, InPinId { node: other_out, input: 0 });
        c.snarl.connect(OutPinId { node: fork, output: 0 }, InPinId { node: primary, input: 0 });
        rewire(&mut c);
        assert!(wired(&c, other_pad, other) && wired(&c, other, other_out), "the other sub-patch keeps its wiring");
        assert!(wired(&c, fork, primary), "the module keeps its inlet");
        assert_eq!(source_slot(&c, easy_pad), Some(1), "the Easy pad takes the free inlet");
        assert!(wired(&c, primary, easy_out));
        assert!(!wired(&c, primary, other_out), "never a second wire into an output that's fed");
        assert_eq!(held_inlets(&c), vec![true, false]);
        assert_eq!(Managed::of(&c.snarl, Some(primary)), Managed { sources: vec![easy_pad], sinks: vec![easy_out] });
    }

    /// A new primary takes Easy mode's devices with it, each on its own port.
    #[test]
    fn the_devices_follow_the_primary() {
        let mut c = Canvas::default();
        let a = at(&mut c, am_node("subpatch", 2, 1));
        let b = at(&mut c, am_node("subpatch", 2, 1));
        let pad = at(&mut c, am_node("device.source", 0, 1));
        let out = at(&mut c, am_node("device.sink", 1, 0));
        c.snarl.get_node_mut(pad).unwrap().params.insert("automap_port".into(), 1u64.into());
        rewire(&mut c);
        assert!(wired(&c, pad, a) && wired(&c, a, out));
        move_primary(&mut c.snarl, b);
        assert!(!wired(&c, pad, a) && !wired(&c, a, out));
        assert!(wired(&c, b, out));
        assert_eq!(source_slot(&c, pad), Some(1));
    }

    /// Letting a pad go removes its node - unless Advanced mode wires it elsewhere
    /// too, when only its wire into the primary goes.
    #[test]
    fn releasing_keeps_a_node_wired_elsewhere() {
        let mut c = Canvas::default();
        let sp = at(&mut c, am_node("subpatch", 1, 1));
        let curve = at(&mut c, am_node("module.automap_curve", 1, 0));
        let pad = at(&mut c, am_node("device.source", 0, 1));
        rewire(&mut c);
        c.snarl.connect(OutPinId { node: pad, output: 0 }, InPinId { node: curve, input: 0 });
        release_source(&mut c, pad);
        assert!(c.snarl.get_node(pad).is_some() && !wired(&c, pad, sp) && wired(&c, pad, curve));
        c.snarl.disconnect(OutPinId { node: pad, output: 0 }, InPinId { node: curve, input: 0 });
        release_source(&mut c, pad);
        assert!(c.snarl.get_node(pad).is_none());
    }

    /// A preset with fewer pins leaves no wire on a pin that's gone.
    #[test]
    fn stale_wires_go_with_their_pins() {
        let mut c = Canvas::default();
        let sp = at(&mut c, am_node("subpatch", 2, 1));
        let fork = at(&mut c, am_node("module.automap_fork", 0, 1));
        c.snarl.connect(OutPinId { node: fork, output: 0 }, InPinId { node: sp, input: 1 });
        c.snarl.get_node_mut(sp).unwrap().inputs.truncate(1);
        drop_stale_wires(&mut c.snarl, sp);
        assert!(!wired(&c, fork, sp));
    }
}
