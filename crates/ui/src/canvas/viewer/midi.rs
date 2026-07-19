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

pub(crate) fn remove_midi_output(node_id: NodeId, rm_idx: usize, outputs: &[OutPin], snarl: &mut Snarl<NodeData>) {
    let tail: Vec<Vec<egui_snarl::InPinId>> = outputs[rm_idx..]
        .iter()
        .map(|o| o.remotes.clone())
        .collect();
    for i in 0..tail.len() {
        snarl.drop_outputs(OutPinId { node: node_id, output: rm_idx + i });
    }
    if let Some(node) = snarl.get_node_mut(node_id) {
        node.outputs.remove(rm_idx);
        if let Some(Value::Array(ids)) = node.params.get_mut("output_pin_ids") {
            ids.remove(rm_idx);
        }
    }
    for (shift, remotes) in tail.into_iter().enumerate().skip(1) {
        let new_out = OutPinId { node: node_id, output: rm_idx + shift - 1 };
        for remote in remotes {
            snarl.connect(new_out, remote);
        }
    }
}

pub(crate) fn remove_midi_input(node_id: NodeId, rm_idx: usize, inputs: &[InPin], snarl: &mut Snarl<NodeData>) {
    let tail: Vec<Vec<OutPinId>> = inputs[rm_idx..]
        .iter()
        .map(|p| p.remotes.clone())
        .collect();
    for i in 0..tail.len() {
        snarl.drop_inputs(InPinId { node: node_id, input: rm_idx + i });
    }
    if let Some(node) = snarl.get_node_mut(node_id) {
        node.inputs.remove(rm_idx);
        if let Some(Value::Array(ids)) = node.params.get_mut("input_pin_ids") {
            ids.remove(rm_idx);
        }
    }
    for (shift, remotes) in tail.into_iter().enumerate().skip(1) {
        let new_in = InPinId { node: node_id, input: rm_idx + shift - 1 };
        for remote in remotes {
            snarl.connect(remote, new_in);
        }
    }
}

pub(crate) fn clear_unused_midi_outputs(node_id: NodeId, outputs: &[OutPin], snarl: &mut Snarl<NodeData>) {
    // Keep only outputs that have at least one downstream connection.
    let connected: Vec<(usize, Vec<egui_snarl::InPinId>)> = outputs.iter()
        .filter(|o| !o.remotes.is_empty())
        .map(|o| (o.id.output, o.remotes.clone()))
        .collect();

    for o in outputs {
        snarl.drop_outputs(OutPinId { node: node_id, output: o.id.output });
    }

    if let Some(node) = snarl.get_node_mut(node_id) {
        let kept_pins: Vec<PinDescriptor> = connected.iter()
            .map(|(idx, _)| node.outputs[*idx].clone())
            .collect();
        let kept_ids: Vec<Value> = node.params.get("output_pin_ids")
            .and_then(|v| v.as_array())
            .map(|ids| connected.iter()
                .map(|(idx, _)| ids.get(*idx).cloned().unwrap_or(Value::String(String::new())))
                .collect())
            .unwrap_or_default();
        node.outputs = kept_pins;
        if let Some(Value::Array(ids)) = node.params.get_mut("output_pin_ids") {
            *ids = kept_ids;
        }
    }

    for (new_idx, (_, remotes)) in connected.iter().enumerate() {
        let new_out = OutPinId { node: node_id, output: new_idx };
        for &remote in remotes {
            snarl.connect(new_out, remote);
        }
    }
}

pub(crate) fn clear_unused_midi_inputs(node_id: NodeId, inputs: &[InPin], snarl: &mut Snarl<NodeData>) {
    let connected: Vec<(usize, Vec<OutPinId>)> = inputs.iter()
        .filter(|p| !p.remotes.is_empty())
        .map(|p| (p.id.input, p.remotes.clone()))
        .collect();

    for p in inputs {
        snarl.drop_inputs(InPinId { node: node_id, input: p.id.input });
    }

    if let Some(node) = snarl.get_node_mut(node_id) {
        let kept_pins: Vec<PinDescriptor> = connected.iter()
            .map(|(idx, _)| node.inputs[*idx].clone())
            .collect();
        let kept_ids: Vec<Value> = node.params.get("input_pin_ids")
            .and_then(|v| v.as_array())
            .map(|ids| connected.iter()
                .map(|(idx, _)| ids.get(*idx).cloned().unwrap_or(Value::String(String::new())))
                .collect())
            .unwrap_or_default();
        node.inputs = kept_pins;
        if let Some(Value::Array(ids)) = node.params.get_mut("input_pin_ids") {
            *ids = kept_ids;
        }
    }

    for (new_idx, (_, remotes)) in connected.iter().enumerate() {
        let new_in = InPinId { node: node_id, input: new_idx };
        for &remote in remotes {
            snarl.connect(remote, new_in);
        }
    }
}
