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
//
// Direction-bound wrappers over the shared dynamic-pin editing in `pins.rs`.
// Every MIDI pin is removable (no protected passthrough) and the id arrays
// cover every pin, hence `id_offset` 0.

pub(crate) fn remove_midi_output(node_id: NodeId, rm_idx: usize, outputs: &[OutPin], snarl: &mut Snarl<NodeData>) {
    remove_dynamic_pin::<Outputs>(node_id, rm_idx, outputs, snarl, "output_pin_ids", 0);
}

pub(crate) fn remove_midi_input(node_id: NodeId, rm_idx: usize, inputs: &[InPin], snarl: &mut Snarl<NodeData>) {
    remove_dynamic_pin::<Inputs>(node_id, rm_idx, inputs, snarl, "input_pin_ids", 0);
}

pub(crate) fn clear_unused_midi_outputs(node_id: NodeId, outputs: &[OutPin], snarl: &mut Snarl<NodeData>) {
    clear_unused_dynamic_pins::<Outputs>(node_id, outputs, snarl, "output_pin_ids");
}

pub(crate) fn clear_unused_midi_inputs(node_id: NodeId, inputs: &[InPin], snarl: &mut Snarl<NodeData>) {
    clear_unused_dynamic_pins::<Inputs>(node_id, inputs, snarl, "input_pin_ids");
}
