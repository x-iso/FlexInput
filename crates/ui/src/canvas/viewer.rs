use egui::Color32;
use egui_snarl::{
    ui::{AnyPins, PinInfo, SnarlViewer},
    InPin, InPinId, NodeId, OutPin, OutPinId, Snarl,
};
use flexinput_core::{ModuleDescriptor, PinDescriptor, SignalType};
use serde_json::Value;

use super::node::NodeData;

pub struct FlexViewer<'a> {
    pub descriptors: &'a [ModuleDescriptor],
}

impl<'a> SnarlViewer<NodeData> for FlexViewer<'a> {
    fn title(&mut self, node: &NodeData) -> String {
        node.display_name.clone()
    }

    fn inputs(&mut self, node: &NodeData) -> usize {
        node.inputs.len()
    }

    fn outputs(&mut self, node: &NodeData) -> usize {
        node.outputs.len()
    }

    fn show_input(
        &mut self,
        pin: &InPin,
        ui: &mut egui::Ui,
        snarl: &mut Snarl<NodeData>,
    ) -> impl egui_snarl::ui::SnarlPin + 'static {
        let node = &snarl[pin.id.node];
        let desc = &node.inputs[pin.id.input];
        ui.set_min_width(80.0);
        ui.add(egui::Label::new(egui::RichText::new(&desc.name).small()).truncate());
        pin_info(desc.signal_type)
    }

    fn show_output(
        &mut self,
        pin: &OutPin,
        ui: &mut egui::Ui,
        snarl: &mut Snarl<NodeData>,
    ) -> impl egui_snarl::ui::SnarlPin + 'static {
        let node = &snarl[pin.id.node];
        let desc = &node.outputs[pin.id.output];
        ui.set_min_width(80.0);
        // Do NOT use .truncate() here — in RTL output layout it collapses to zero width.
        ui.label(egui::RichText::new(&desc.name).small());
        pin_info(desc.signal_type)
    }

    fn connect(&mut self, from: &OutPin, to: &InPin, snarl: &mut Snarl<NodeData>) {
        let from_type = snarl[from.id.node].outputs[from.id.output].signal_type;
        let to_type = snarl[to.id.node].inputs[to.id.input].signal_type;
        if to_type.accepts(from_type) {
            snarl.connect(from.id, to.id);
        }
    }

    // ── Keyboard & Mouse sink: dynamic key learning ──────────────────────────

    fn has_body(&mut self, node: &NodeData) -> bool {
        node.module_id == "device.sink"
            && node.params.get("device_id").and_then(|v| v.as_str()) == Some("virtual.keymouse")
    }

    fn show_body(
        &mut self,
        node_id: NodeId,
        inputs: &[InPin],
        _outputs: &[OutPin],
        ui: &mut egui::Ui,
        snarl: &mut Snarl<NodeData>,
    ) {
        let fixed_count = snarl
            .get_node(node_id)
            .and_then(|n| n.params.get("fixed_input_count").and_then(|v| v.as_u64()))
            .unwrap_or(0) as usize;

        let is_learning = snarl
            .get_node(node_id)
            .and_then(|n| n.params.get("learning").and_then(|v| v.as_bool()))
            .unwrap_or(false);

        if is_learning {
            ui.label(egui::RichText::new("Press a key… (Esc cancels)").italics().small());

            let key_pressed = ui.input(|i| {
                i.events.iter().find_map(|e| {
                    if let egui::Event::Key { key, pressed: true, .. } = e {
                        Some(*key)
                    } else {
                        None
                    }
                })
            });

            if let Some(key) = key_pressed {
                if let Some(node) = snarl.get_node_mut(node_id) {
                    node.params.insert("learning".to_string(), Value::Bool(false));
                }

                if key != egui::Key::Escape {
                    let pin_name = format!("{key:?}");
                    let already_has = snarl
                        .get_node(node_id)
                        .map(|n| n.inputs.iter().any(|p| p.name == pin_name))
                        .unwrap_or(false);

                    if !already_has {
                        if let Some(node) = snarl.get_node_mut(node_id) {
                            node.inputs.push(PinDescriptor::new(pin_name, SignalType::Bool));
                        }
                    }
                }
            }
        } else {
            ui.horizontal(|ui| {
                if ui.small_button("+ Learn key").clicked() {
                    if let Some(node) = snarl.get_node_mut(node_id) {
                        node.params.insert("learning".to_string(), Value::Bool(true));
                    }
                }
                // Only learned pins (beyond fixed_count) can be cleared.
                let has_unused_learned = inputs.iter().skip(fixed_count).any(|p| p.remotes.is_empty());
                if has_unused_learned && ui.small_button("Clear unused").clicked() {
                    clear_unused_inputs(node_id, inputs, fixed_count, snarl);
                }
            });
        }
    }

    // ── Graph context menu (right-click on empty canvas) ─────────────────────

    fn has_graph_menu(&mut self, _pos: egui::Pos2, _snarl: &mut Snarl<NodeData>) -> bool {
        true
    }

    fn show_graph_menu(
        &mut self,
        pos: egui::Pos2,
        ui: &mut egui::Ui,
        snarl: &mut Snarl<NodeData>,
    ) {
        ui.label("Add module");
        ui.separator();
        show_module_menu(pos, ui, snarl, self.descriptors, None);
    }

    // ── Drop-wire menu ───────────────────────────────────────────────────────

    fn has_dropped_wire_menu(
        &mut self,
        _src_pins: AnyPins,
        _snarl: &mut Snarl<NodeData>,
    ) -> bool {
        true
    }

    fn show_dropped_wire_menu(
        &mut self,
        pos: egui::Pos2,
        ui: &mut egui::Ui,
        src_pins: AnyPins,
        snarl: &mut Snarl<NodeData>,
    ) {
        match src_pins {
            AnyPins::Out(out_pins) => {
                if let Some(&src) = out_pins.first() {
                    let from_type = snarl[src.node].outputs[src.output].signal_type;
                    ui.label("Connect to input of…");
                    ui.separator();
                    show_module_menu(pos, ui, snarl, self.descriptors, Some(WireDir::FromOutput { src, from_type }));
                }
            }
            AnyPins::In(in_pins) => {
                if let Some(&dst) = in_pins.first() {
                    let to_type = snarl[dst.node].inputs[dst.input].signal_type;
                    ui.label("Connect to output of…");
                    ui.separator();
                    show_module_menu(pos, ui, snarl, self.descriptors, Some(WireDir::FromInput { dst, to_type }));
                }
            }
        }
    }

    // ── Node context menu ─────────────────────────────────────────────────────

    fn has_node_menu(&mut self, _node: &NodeData) -> bool {
        true
    }

    fn show_node_menu(
        &mut self,
        node: NodeId,
        _inputs: &[InPin],
        _outputs: &[OutPin],
        ui: &mut egui::Ui,
        snarl: &mut Snarl<NodeData>,
    ) {
        if ui.button("Remove node").clicked() {
            snarl.remove_node(node);
            ui.close();
        }
    }
}

// ── helpers ───────────────────────────────────────────────────────────────────

fn pin_info(t: SignalType) -> PinInfo {
    let [r, g, b] = t.color_rgb();
    PinInfo::circle().with_fill(Color32::from_rgb(r, g, b))
}

/// Remove unconnected *learned* input pins (indices >= fixed_count), remapping
/// wire indices so connected wires remain valid.
fn clear_unused_inputs(
    node_id: NodeId,
    inputs: &[InPin],
    fixed_count: usize,
    snarl: &mut Snarl<NodeData>,
) {
    // Among removable (learned) pins, keep only those that have wires.
    let connected_removable: Vec<(usize, Vec<OutPinId>)> = inputs
        .iter()
        .skip(fixed_count)
        .filter(|p| !p.remotes.is_empty())
        .map(|p| (p.id.input, p.remotes.clone()))
        .collect();

    // Drop wires from all removable inputs.
    for pin in inputs.iter().skip(fixed_count) {
        snarl.drop_inputs(InPinId { node: node_id, input: pin.id.input });
    }

    // Rebuild: fixed pins stay; connected learned pins appended after.
    if let Some(node) = snarl.get_node_mut(node_id) {
        let kept: Vec<_> = connected_removable
            .iter()
            .map(|(idx, _)| node.inputs[*idx].clone())
            .collect();
        node.inputs.truncate(fixed_count);
        node.inputs.extend(kept);
    } else {
        return;
    }

    // Reconnect wires at new (compacted) indices.
    for (new_idx, (_, remotes)) in connected_removable.iter().enumerate() {
        let new_pin = InPinId { node: node_id, input: fixed_count + new_idx };
        for &remote in remotes {
            snarl.connect(remote, new_pin);
        }
    }
}

enum WireDir {
    FromOutput { src: OutPinId, from_type: SignalType },
    FromInput  { dst: InPinId,  to_type:   SignalType },
}

fn show_module_menu(
    pos: egui::Pos2,
    ui: &mut egui::Ui,
    snarl: &mut Snarl<NodeData>,
    descriptors: &[ModuleDescriptor],
    wire: Option<WireDir>,
) {
    let mut categories: Vec<&str> = vec![];
    for d in descriptors {
        if !categories.contains(&d.category) {
            categories.push(d.category);
        }
    }

    for cat in categories {
        let cat_modules: Vec<&ModuleDescriptor> = descriptors
            .iter()
            .filter(|d| {
                d.category == cat
                    && match &wire {
                        None => true,
                        Some(WireDir::FromOutput { from_type, .. }) => {
                            d.inputs.iter().any(|p| p.signal_type.accepts(*from_type))
                        }
                        Some(WireDir::FromInput { to_type, .. }) => {
                            d.outputs.iter().any(|p| to_type.accepts(p.signal_type))
                        }
                    }
            })
            .collect();

        if cat_modules.is_empty() {
            continue;
        }

        ui.menu_button(cat, |ui| {
            for desc in cat_modules {
                if ui.button(desc.display_name).clicked() {
                    let node_id = snarl.insert_node(pos, NodeData::from(desc));
                    match &wire {
                        Some(WireDir::FromOutput { src, from_type }) => {
                            if let Some((idx, _)) = desc
                                .inputs
                                .iter()
                                .enumerate()
                                .find(|(_, p)| p.signal_type.accepts(*from_type))
                            {
                                snarl.connect(*src, InPinId { node: node_id, input: idx });
                            }
                        }
                        Some(WireDir::FromInput { dst, to_type }) => {
                            if let Some((idx, _)) = desc
                                .outputs
                                .iter()
                                .enumerate()
                                .find(|(_, p)| to_type.accepts(p.signal_type))
                            {
                                snarl.connect(OutPinId { node: node_id, output: idx }, *dst);
                            }
                        }
                        None => {}
                    }
                    ui.close();
                }
            }
        });
    }
}
