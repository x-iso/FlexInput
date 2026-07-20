//! MIDI In/Out node bodies and MIDI pin add/remove helpers.

use super::*;

pub(crate) fn show_midi_in_body(node_id: NodeId, outputs: &[OutPin], ui: &mut egui::Ui, snarl: &mut Snarl<NodeData>) {
    let is_learning = snarl
        .get_node(node_id)
        .and_then(|n| n.params.get("learning").and_then(|v| v.as_bool()))
        .unwrap_or(false);

    let pin_ids: Vec<String> = snarl
        .get_node(node_id)
        .and_then(|n| n.params.get("output_pin_ids").and_then(|v| v.as_array()))
        .map(|a| a.iter().map(|v| v.as_str().unwrap_or("").to_string()).collect())
        .unwrap_or_default();

    let selected_ccs: Vec<u8> = pin_ids.iter()
        .filter_map(|id| id.strip_prefix("cc_").and_then(|s| s.parse().ok()))
        .collect();

    ui.vertical(|ui| {
        ui.set_min_width(160.0);

        // CC rows: [×] label
        let mut to_remove: Option<usize> = None;
        for (idx, &cc) in selected_ccs.iter().enumerate() {
            ui.horizontal(|ui| {
                if ui.small_button("×").clicked() {
                    to_remove = Some(idx);
                }
                ui.label(egui::RichText::new(cc_display_name(cc)).small());
            });
        }

        if let Some(rm_idx) = to_remove {
            remove_midi_output(node_id, rm_idx, outputs, snarl);
        }

        ui.add_space(4.0);

        // Toolbar row
        ui.horizontal(|ui| {
            egui::ComboBox::from_id_salt((node_id, "add_cc_in"))
                .selected_text(egui::RichText::new("+ Add CC").small())
                .width(100.0)
                .show_ui(ui, |ui| {
                    for cc in 0u8..=127 {
                        if selected_ccs.contains(&cc) { continue; }
                        if ui.selectable_label(false, egui::RichText::new(cc_display_name(cc)).small()).clicked() {
                            if let Some(node) = snarl.get_node_mut(node_id) {
                                node.outputs.push(PinDescriptor::new(&cc_display_name(cc), SignalType::Float));
                                if let Some(Value::Array(ids)) = node.params.get_mut("output_pin_ids") {
                                    ids.push(Value::String(format!("cc_{}", cc)));
                                }
                            }
                        }
                    }
                });

            let learn_label = if is_learning {
                egui::RichText::new("● Stop").small().color(Color32::from_rgb(220, 80, 80))
            } else {
                egui::RichText::new("Learn").small()
            };
            if ui.button(learn_label).clicked() {
                if let Some(node) = snarl.get_node_mut(node_id) {
                    node.params.insert("learning".to_string(), Value::Bool(!is_learning));
                }
            }
        });

        let has_unused = outputs.iter().any(|o| o.remotes.is_empty());
        if has_unused && ui.small_button("Clear unused").clicked() {
            clear_unused_midi_outputs(node_id, outputs, snarl);
        }
    });
}

pub(crate) fn show_midi_out_body(node_id: NodeId, inputs: &[InPin], ui: &mut egui::Ui, snarl: &mut Snarl<NodeData>) {
    let pin_ids: Vec<String> = snarl
        .get_node(node_id)
        .and_then(|n| n.params.get("input_pin_ids").and_then(|v| v.as_array()))
        .map(|a| a.iter().map(|v| v.as_str().unwrap_or("").to_string()).collect())
        .unwrap_or_default();

    let selected_ccs: Vec<u8> = pin_ids.iter()
        .filter_map(|id| id.strip_prefix("cc_").and_then(|s| s.parse().ok()))
        .collect();

    ui.vertical(|ui| {
        ui.set_min_width(160.0);

        let mut to_remove: Option<usize> = None;
        for (idx, &cc) in selected_ccs.iter().enumerate() {
            ui.horizontal(|ui| {
                if ui.small_button("×").clicked() {
                    to_remove = Some(idx);
                }
                ui.label(egui::RichText::new(cc_display_name(cc)).small());
            });
        }

        if let Some(rm_idx) = to_remove {
            remove_midi_input(node_id, rm_idx, inputs, snarl);
        }

        ui.add_space(4.0);

        egui::ComboBox::from_id_salt((node_id, "add_cc_out"))
            .selected_text(egui::RichText::new("+ Add CC").small())
            .width(130.0)
            .show_ui(ui, |ui| {
                for cc in 0u8..=127 {
                    if selected_ccs.contains(&cc) { continue; }
                    if ui.selectable_label(false, egui::RichText::new(cc_display_name(cc)).small()).clicked() {
                        if let Some(node) = snarl.get_node_mut(node_id) {
                            node.inputs.push(PinDescriptor::new(&cc_display_name(cc), SignalType::Float));
                            if let Some(Value::Array(ids)) = node.params.get_mut("input_pin_ids") {
                                ids.push(Value::String(format!("cc_{}", cc)));
                            }
                        }
                    }
                }
            });

        let has_unused = inputs.iter().any(|p| p.remotes.is_empty());
        if has_unused && ui.small_button("Clear unused").clicked() {
            clear_unused_midi_inputs(node_id, inputs, snarl);
        }
    });
}

// ── MIDI pin removal helpers ──────────────────────────────────────────────────

/// One side (inputs or outputs) of a node's dynamic MIDI pin list.
///
/// The removal/compaction logic below is identical for both sides; only the
/// primitives differ — which snarl id type addresses a pin, which end of
/// `connect(from_out, to_in)` the remote goes on, which pin vector and which
/// `*_pin_ids` param to rewrite. Naming those five things lets the two
/// algorithms exist once instead of as mirrored pairs that can drift apart.
trait MidiSide {
    /// The pin type snarl hands the body (`OutPin` / `InPin`).
    type Pin;
    /// The id of the pin at the OTHER end of a wire, as stored in `remotes`.
    type Remote: Copy;
    /// Params key holding this side's stable pin ids.
    const IDS_KEY: &'static str;

    fn index(pin: &Self::Pin) -> usize;
    fn remotes(pin: &Self::Pin) -> &[Self::Remote];
    /// Drop every wire attached to this side's pin `idx`.
    fn drop_at(snarl: &mut Snarl<NodeData>, node: NodeId, idx: usize);
    /// Reconnect `remote` to this side's pin `idx`.
    fn connect(snarl: &mut Snarl<NodeData>, node: NodeId, idx: usize, remote: Self::Remote);
    fn pins_mut(node: &mut NodeData) -> &mut Vec<PinDescriptor>;
}

struct Outputs;
struct Inputs;

impl MidiSide for Outputs {
    type Pin = OutPin;
    type Remote = egui_snarl::InPinId;
    const IDS_KEY: &'static str = "output_pin_ids";

    fn index(pin: &OutPin) -> usize { pin.id.output }
    fn remotes(pin: &OutPin) -> &[Self::Remote] { &pin.remotes }
    fn drop_at(snarl: &mut Snarl<NodeData>, node: NodeId, idx: usize) {
        snarl.drop_outputs(OutPinId { node, output: idx });
    }
    fn connect(snarl: &mut Snarl<NodeData>, node: NodeId, idx: usize, remote: Self::Remote) {
        snarl.connect(OutPinId { node, output: idx }, remote);
    }
    fn pins_mut(node: &mut NodeData) -> &mut Vec<PinDescriptor> { &mut node.outputs }
}

impl MidiSide for Inputs {
    type Pin = InPin;
    type Remote = OutPinId;
    const IDS_KEY: &'static str = "input_pin_ids";

    fn index(pin: &InPin) -> usize { pin.id.input }
    fn remotes(pin: &InPin) -> &[Self::Remote] { &pin.remotes }
    fn drop_at(snarl: &mut Snarl<NodeData>, node: NodeId, idx: usize) {
        snarl.drop_inputs(InPinId { node, input: idx });
    }
    fn connect(snarl: &mut Snarl<NodeData>, node: NodeId, idx: usize, remote: Self::Remote) {
        snarl.connect(remote, InPinId { node, input: idx });
    }
    fn pins_mut(node: &mut NodeData) -> &mut Vec<PinDescriptor> { &mut node.inputs }
}

/// Remove pin `rm_idx`, then slide every later pin down one slot, carrying its
/// wires with it (snarl addresses wires by index, so the tail must be dropped
/// and re-made rather than left dangling on stale indices).
fn remove_midi_pin<S: MidiSide>(
    node_id: NodeId,
    rm_idx: usize,
    pins: &[S::Pin],
    snarl: &mut Snarl<NodeData>,
) {
    let tail: Vec<Vec<S::Remote>> = pins[rm_idx..].iter().map(|p| S::remotes(p).to_vec()).collect();
    for i in 0..tail.len() {
        S::drop_at(snarl, node_id, rm_idx + i);
    }
    if let Some(node) = snarl.get_node_mut(node_id) {
        S::pins_mut(node).remove(rm_idx);
        if let Some(Value::Array(ids)) = node.params.get_mut(S::IDS_KEY) {
            ids.remove(rm_idx);
        }
    }
    // `skip(1)` drops the removed pin's own wires; the rest shift down one.
    for (shift, remotes) in tail.into_iter().enumerate().skip(1) {
        for remote in remotes {
            S::connect(snarl, node_id, rm_idx + shift - 1, remote);
        }
    }
}

/// Keep only pins that have at least one wire, compacting the rest away.
fn clear_unused_midi_pins<S: MidiSide>(
    node_id: NodeId,
    pins: &[S::Pin],
    snarl: &mut Snarl<NodeData>,
) {
    let connected: Vec<(usize, Vec<S::Remote>)> = pins
        .iter()
        .filter(|p| !S::remotes(p).is_empty())
        .map(|p| (S::index(p), S::remotes(p).to_vec()))
        .collect();

    for p in pins {
        S::drop_at(snarl, node_id, S::index(p));
    }

    if let Some(node) = snarl.get_node_mut(node_id) {
        let kept_pins: Vec<PinDescriptor> = connected
            .iter()
            .map(|(idx, _)| S::pins_mut(node)[*idx].clone())
            .collect();
        let kept_ids: Vec<Value> = node.params.get(S::IDS_KEY)
            .and_then(|v| v.as_array())
            .map(|ids| connected.iter()
                .map(|(idx, _)| ids.get(*idx).cloned().unwrap_or(Value::String(String::new())))
                .collect())
            .unwrap_or_default();
        *S::pins_mut(node) = kept_pins;
        if let Some(Value::Array(ids)) = node.params.get_mut(S::IDS_KEY) {
            *ids = kept_ids;
        }
    }

    for (new_idx, (_, remotes)) in connected.iter().enumerate() {
        for &remote in remotes {
            S::connect(snarl, node_id, new_idx, remote);
        }
    }
}

pub(crate) fn remove_midi_output(node_id: NodeId, rm_idx: usize, outputs: &[OutPin], snarl: &mut Snarl<NodeData>) {
    remove_midi_pin::<Outputs>(node_id, rm_idx, outputs, snarl);
}

pub(crate) fn remove_midi_input(node_id: NodeId, rm_idx: usize, inputs: &[InPin], snarl: &mut Snarl<NodeData>) {
    remove_midi_pin::<Inputs>(node_id, rm_idx, inputs, snarl);
}

pub(crate) fn clear_unused_midi_outputs(node_id: NodeId, outputs: &[OutPin], snarl: &mut Snarl<NodeData>) {
    clear_unused_midi_pins::<Outputs>(node_id, outputs, snarl);
}

pub(crate) fn clear_unused_midi_inputs(node_id: NodeId, inputs: &[InPin], snarl: &mut Snarl<NodeData>) {
    clear_unused_midi_pins::<Inputs>(node_id, inputs, snarl);
}
