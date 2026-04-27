use egui::Color32;
use egui_snarl::{
    ui::{AnyPins, PinInfo, SnarlViewer},
    InPin, InPinId, NodeId, OutPin, OutPinId, Snarl,
};
use flexinput_core::{ModuleDescriptor, PinDescriptor, Signal, SignalType};
use flexinput_devices::midi::cc_display_name;
use serde_json::{Number, Value};

use super::{curve::sample_curve, node::NodeData};

pub struct FlexViewer<'a> {
    pub descriptors: &'a [ModuleDescriptor],
    pub ctx: egui::Context,
    /// IDs of currently-live physical and virtual devices.  Used to render status dots.
    pub live_device_ids: &'a std::collections::HashSet<String>,
    /// Set by the `disconnect` override when the user right-clicks a wire.
    /// Canvas::show() reads this after snarl.show() and renders the context menu.
    pub pending_wire_menu: Option<(OutPinId, InPinId, egui::Pos2)>,
    /// Set by show_node_menu when the user clicks "Rename…".
    pub rename_request: Option<NodeId>,
}

impl<'a> SnarlViewer<NodeData> for FlexViewer<'a> {
    fn title(&mut self, node: &NodeData) -> String {
        node.display_name.clone()
    }

    fn show_header(
        &mut self,
        node: NodeId,
        _inputs: &[egui_snarl::InPin],
        _outputs: &[egui_snarl::OutPin],
        ui: &mut egui::Ui,
        snarl: &mut Snarl<NodeData>,
    ) {
        // Extract what we need before any UI calls so the borrow of snarl is released.
        let data = &snarl[node];
        let title = data.display_name.clone();
        let status_dot = if matches!(data.module_id.as_str(), "device.source" | "device.sink") {
            let live = data.params.get("device_id")
                .and_then(|v| v.as_str())
                .map(|id| self.live_device_ids.contains(id))
                .unwrap_or(false);
            Some(live)
        } else {
            None
        };

        ui.horizontal(|ui| {
            if let Some(live) = status_dot {
                let color = if live {
                    Color32::from_rgb(80, 200, 100)
                } else {
                    Color32::from_rgb(220, 80, 60)
                };
                ui.label(egui::RichText::new("●").color(color).small());
            }
            ui.label(title);
        });
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
        ui.spacing_mut().item_spacing.y = 0.0;
        let text = egui::RichText::new(&desc.name).small();
        let text = match channel_label_color(&node.module_id, pin.id.input) {
            Some(col) => text.color(col),
            None      => text,
        };
        ui.label(text);
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
        ui.spacing_mut().item_spacing.y = 0.0;
        let text = egui::RichText::new(&desc.name).small();
        let text = match channel_label_color(&node.module_id, pin.id.output) {
            Some(col) => text.color(col),
            None      => text,
        };
        ui.label(text);
        pin_info(desc.signal_type)
    }

    fn connect(&mut self, from: &OutPin, to: &InPin, snarl: &mut Snarl<NodeData>) {
        let from_type = snarl[from.id.node].outputs[from.id.output].signal_type;
        let to_type = snarl[to.id.node].inputs[to.id.input].signal_type;
        if to_type.accepts(from_type) {
            snarl.connect(from.id, to.id);
        }
    }

    fn disconnect(&mut self, from: &OutPin, to: &InPin, _snarl: &mut Snarl<NodeData>) {
        // Intercept right-click-on-wire: show a context menu instead of disconnecting immediately.
        let pos = self.ctx.input(|i| i.pointer.latest_pos()).unwrap_or_default();
        self.pending_wire_menu = Some((from.id, to.id, pos));
    }

    // ── Node bodies ──────────────────────────────────────────────────────────

    fn has_body(&mut self, node: &NodeData) -> bool {
        let dev_id = node.params.get("device_id").and_then(|v| v.as_str()).unwrap_or("");
        let is_midi_source = node.module_id == "device.source" && dev_id.starts_with("midi_in:");
        let is_midi_sink   = node.module_id == "device.sink"   && dev_id.starts_with("midi_out:");
        is_midi_source || is_midi_sink || matches!(
            node.module_id.as_str(),
            "device.sink" | "module.constant" | "module.switch" | "module.knob"
                | "display.readout" | "display.oscilloscope" | "display.vectorscope"
                | "module.delay" | "module.lowpass" | "module.response_curve"
                | "math.add" | "math.subtract" | "math.multiply" | "math.divide"
                | "module.selector" | "module.split"
        )
    }

    fn show_body(
        &mut self,
        node_id: NodeId,
        inputs: &[InPin],
        outputs: &[OutPin],
        ui: &mut egui::Ui,
        snarl: &mut Snarl<NodeData>,
    ) {
        let module_id = snarl
            .get_node(node_id)
            .map(|n| n.module_id.clone())
            .unwrap_or_default();
        let device_id = snarl
            .get_node(node_id)
            .and_then(|n| n.params.get("device_id").and_then(|v| v.as_str()))
            .unwrap_or("")
            .to_string();

        if module_id == "device.source" && device_id.starts_with("midi_in:") {
            show_midi_in_body(node_id, outputs, ui, snarl);
            return;
        }
        if module_id == "device.sink" && device_id.starts_with("midi_out:") {
            show_midi_out_body(node_id, inputs, ui, snarl);
            return;
        }

        match module_id.as_str() {
            "device.sink"          => show_sink_body(node_id, inputs, ui, snarl),
            "module.constant"      => show_constant_body(node_id, ui, snarl),
            "module.switch"        => show_switch_body(node_id, ui, snarl),
            "module.knob"          => show_knob_body(node_id, ui, snarl),
            "display.readout"       => show_readout_body(node_id, ui, snarl),
            "display.oscilloscope"  => show_oscilloscope_body(node_id, inputs, ui, snarl),
            "display.vectorscope"   => show_vectorscope_body(node_id, ui, snarl),
            "module.delay"          => show_delay_body(node_id, ui, snarl),
            "module.lowpass"        => show_lowpass_body(node_id, ui, snarl),
            "module.response_curve" => show_response_curve_body(node_id, inputs, outputs, ui, snarl),
            "math.add" | "math.subtract" | "math.multiply" | "math.divide" => {
                show_math_variadic_body(node_id, inputs, ui, snarl);
            }
            "module.selector" => show_selector_body(node_id, inputs, ui, snarl),
            "module.split"    => show_split_body(node_id, outputs, ui, snarl),
            _ => {}
        }
    }

    // ── Graph context menu ───────────────────────────────────────────────────

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

    fn has_dropped_wire_menu(&mut self, _src_pins: AnyPins, _snarl: &mut Snarl<NodeData>) -> bool {
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

    // ── Node context menu ────────────────────────────────────────────────────

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
        if ui.button("Rename…").clicked() {
            self.rename_request = Some(node);
            ui.close();
        }
        if ui.button("Remove node").clicked() {
            snarl.remove_node(node);
            ui.close();
        }
    }
}

// ── Body renderers ────────────────────────────────────────────────────────────

fn show_midi_in_body(node_id: NodeId, outputs: &[OutPin], ui: &mut egui::Ui, snarl: &mut Snarl<NodeData>) {
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

fn show_midi_out_body(node_id: NodeId, inputs: &[InPin], ui: &mut egui::Ui, snarl: &mut Snarl<NodeData>) {
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

fn remove_midi_output(node_id: NodeId, rm_idx: usize, outputs: &[OutPin], snarl: &mut Snarl<NodeData>) {
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

fn remove_midi_input(node_id: NodeId, rm_idx: usize, inputs: &[InPin], snarl: &mut Snarl<NodeData>) {
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

fn clear_unused_midi_outputs(node_id: NodeId, outputs: &[OutPin], snarl: &mut Snarl<NodeData>) {
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

fn clear_unused_midi_inputs(node_id: NodeId, inputs: &[InPin], snarl: &mut Snarl<NodeData>) {
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

// ── Generic pin removal helpers ───────────────────────────────────────────────

fn remove_input_pin(node_id: NodeId, rm_idx: usize, inputs: &[InPin], snarl: &mut Snarl<NodeData>) {
    let tail: Vec<Vec<OutPinId>> = inputs[rm_idx..].iter().map(|p| p.remotes.clone()).collect();
    for i in 0..tail.len() {
        snarl.drop_inputs(InPinId { node: node_id, input: rm_idx + i });
    }
    if let Some(node) = snarl.get_node_mut(node_id) {
        node.inputs.remove(rm_idx);
    }
    for (shift, remotes) in tail.into_iter().enumerate().skip(1) {
        let new_in = InPinId { node: node_id, input: rm_idx + shift - 1 };
        for remote in remotes {
            snarl.connect(remote, new_in);
        }
    }
}

fn remove_output_pin(node_id: NodeId, rm_idx: usize, outputs: &[OutPin], snarl: &mut Snarl<NodeData>) {
    let tail: Vec<Vec<egui_snarl::InPinId>> = outputs[rm_idx..].iter().map(|o| o.remotes.clone()).collect();
    for i in 0..tail.len() {
        snarl.drop_outputs(OutPinId { node: node_id, output: rm_idx + i });
    }
    if let Some(node) = snarl.get_node_mut(node_id) {
        node.outputs.remove(rm_idx);
    }
    for (shift, remotes) in tail.into_iter().enumerate().skip(1) {
        let new_out = OutPinId { node: node_id, output: rm_idx + shift - 1 };
        for remote in remotes {
            snarl.connect(new_out, remote);
        }
    }
}

// ── Math variadic body ────────────────────────────────────────────────────────

fn pin_letter(idx: usize) -> String {
    if idx < 26 { format!("{}", (b'a' + idx as u8) as char) }
    else { format!("in_{}", idx) }
}

fn show_math_variadic_body(
    node_id: NodeId,
    inputs: &[InPin],
    ui: &mut egui::Ui,
    snarl: &mut Snarl<NodeData>,
) {
    let n = snarl.get_node(node_id).map(|n| n.inputs.len()).unwrap_or(2);
    ui.horizontal(|ui| {
        if ui.small_button("+").on_hover_text("Add input").clicked() {
            if let Some(node) = snarl.get_node_mut(node_id) {
                let name = pin_letter(node.inputs.len());
                node.inputs.push(PinDescriptor::new(name, SignalType::Float));
            }
        }
        if n > 2 && ui.small_button("−").on_hover_text("Remove last input").clicked() {
            remove_input_pin(node_id, n - 1, inputs, snarl);
        }
    });
}

// ── Selector body ─────────────────────────────────────────────────────────────

fn show_selector_body(
    node_id: NodeId,
    inputs: &[InPin],
    ui: &mut egui::Ui,
    snarl: &mut Snarl<NodeData>,
) {
    // inputs[0] = select (fixed); inputs[1..] = in_0, in_1, ... (dynamic)
    let n_value = snarl.get_node(node_id).map(|n| n.inputs.len().saturating_sub(1)).unwrap_or(2);

    ui.vertical(|ui| {
        ui.set_min_width(80.0);
        let mut to_remove: Option<usize> = None;
        for i in 0..n_value {
            ui.horizontal(|ui| {
                if n_value > 2 {
                    if ui.small_button("×").clicked() { to_remove = Some(i + 1); }
                } else {
                    ui.add_space(18.0);
                }
                ui.label(egui::RichText::new(format!("in_{i}")).small());
            });
        }
        if let Some(rm) = to_remove {
            remove_input_pin(node_id, rm, inputs, snarl);
        }
        ui.add_space(2.0);
        if ui.small_button("+ input").clicked() {
            if let Some(node) = snarl.get_node_mut(node_id) {
                let next = node.inputs.len() - 1;
                node.inputs.push(PinDescriptor::new(format!("in_{next}"), SignalType::Float));
            }
        }
    });
}

// ── Split body ────────────────────────────────────────────────────────────────

fn show_split_body(
    node_id: NodeId,
    outputs: &[OutPin],
    ui: &mut egui::Ui,
    snarl: &mut Snarl<NodeData>,
) {
    let n_out = snarl.get_node(node_id).map(|n| n.outputs.len()).unwrap_or(2);

    ui.vertical(|ui| {
        ui.set_min_width(80.0);
        let mut to_remove: Option<usize> = None;
        for i in 0..n_out {
            ui.horizontal(|ui| {
                if n_out > 2 {
                    if ui.small_button("×").clicked() { to_remove = Some(i); }
                } else {
                    ui.add_space(18.0);
                }
                ui.label(egui::RichText::new(format!("out_{i}")).small());
            });
        }
        if let Some(rm) = to_remove {
            remove_output_pin(node_id, rm, outputs, snarl);
        }
        ui.add_space(2.0);
        if ui.small_button("+ output").clicked() {
            if let Some(node) = snarl.get_node_mut(node_id) {
                let next = node.outputs.len();
                node.outputs.push(PinDescriptor::new(format!("out_{next}"), SignalType::Float));
            }
        }
    });
}

fn show_sink_body(node_id: NodeId, inputs: &[InPin], ui: &mut egui::Ui, snarl: &mut Snarl<NodeData>) {
    let device_id = snarl
        .get_node(node_id)
        .and_then(|n| n.params.get("device_id").and_then(|v| v.as_str()))
        .unwrap_or("")
        .to_string();

    if device_id != "virtual.keymouse" {
        return;
    }

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
                        node.inputs.push(PinDescriptor::new(&pin_name, SignalType::Bool));
                        // Keep input_pin_ids in sync for routing.
                        if let Some(Value::Array(ids)) = node.params.get_mut("input_pin_ids") {
                            ids.push(Value::String(pin_name));
                        }
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
            let has_unused_learned = inputs.iter().skip(fixed_count).any(|p| p.remotes.is_empty());
            if has_unused_learned && ui.small_button("Clear unused").clicked() {
                clear_unused_inputs(node_id, inputs, fixed_count, snarl);
            }
        });
    }
}

fn show_constant_body(node_id: NodeId, ui: &mut egui::Ui, snarl: &mut Snarl<NodeData>) {
    let value = snarl
        .get_node(node_id)
        .and_then(|n| n.params.get("value").and_then(|v| v.as_f64()))
        .unwrap_or(0.0) as f32;
    let mut v = value;
    if ui.add(egui::DragValue::new(&mut v).speed(0.01)).changed() {
        if let Some(node) = snarl.get_node_mut(node_id) {
            if let Some(n) = Number::from_f64(v as f64) {
                node.params.insert("value".to_string(), Value::Number(n));
            }
        }
    }
}

fn show_switch_body(node_id: NodeId, ui: &mut egui::Ui, snarl: &mut Snarl<NodeData>) {
    let active = snarl
        .get_node(node_id)
        .and_then(|n| n.params.get("active").and_then(|v| v.as_bool()))
        .unwrap_or(false);
    let mut a = active;
    let label = if a { "ON" } else { "OFF" };
    if ui.toggle_value(&mut a, label).changed() {
        if let Some(node) = snarl.get_node_mut(node_id) {
            node.params.insert("active".to_string(), Value::Bool(a));
        }
    }
}

fn show_knob_body(node_id: NodeId, ui: &mut egui::Ui, snarl: &mut Snarl<NodeData>) {
    let value = snarl
        .get_node(node_id)
        .and_then(|n| n.params.get("value").and_then(|v| v.as_f64()))
        .unwrap_or(0.0) as f32;
    let mut v = value;
    if ui.add(egui::Slider::new(&mut v, 0.0..=1.0).show_value(true)).changed() {
        if let Some(node) = snarl.get_node_mut(node_id) {
            if let Some(n) = Number::from_f64(v as f64) {
                node.params.insert("value".to_string(), Value::Number(n));
            }
        }
    }
}

// ── clear_unused helper ───────────────────────────────────────────────────────

fn clear_unused_inputs(
    node_id: NodeId,
    inputs: &[InPin],
    fixed_count: usize,
    snarl: &mut Snarl<NodeData>,
) {
    let connected_removable: Vec<(usize, Vec<OutPinId>)> = inputs
        .iter()
        .skip(fixed_count)
        .filter(|p| !p.remotes.is_empty())
        .map(|p| (p.id.input, p.remotes.clone()))
        .collect();

    for pin in inputs.iter().skip(fixed_count) {
        snarl.drop_inputs(InPinId { node: node_id, input: pin.id.input });
    }

    if let Some(node) = snarl.get_node_mut(node_id) {
        let kept_pins: Vec<_> = connected_removable
            .iter()
            .map(|(idx, _)| node.inputs[*idx].clone())
            .collect();
        let kept_ids: Vec<_> = if let Some(Value::Array(ids)) = node.params.get("input_pin_ids") {
            connected_removable.iter()
                .map(|(idx, _)| ids.get(*idx).cloned().unwrap_or(Value::String(String::new())))
                .collect()
        } else {
            vec![]
        };

        node.inputs.truncate(fixed_count);
        node.inputs.extend(kept_pins);

        if let Some(Value::Array(ids)) = node.params.get_mut("input_pin_ids") {
            ids.truncate(fixed_count);
            ids.extend(kept_ids);
        }
    } else {
        return;
    }

    for (new_idx, (_, remotes)) in connected_removable.iter().enumerate() {
        let new_pin = InPinId { node: node_id, input: fixed_count + new_idx };
        for &remote in remotes {
            snarl.connect(remote, new_pin);
        }
    }
}

// ── helpers ───────────────────────────────────────────────────────────────────

fn pin_info(t: SignalType) -> PinInfo {
    let [r, g, b] = t.color_rgb();
    PinInfo::circle().with_fill(Color32::from_rgb(r, g, b))
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

// ── Pin label color helpers ───────────────────────────────────────────────────

fn channel_label_color(module_id: &str, ch: usize) -> Option<Color32> {
    match module_id {
        "display.vectorscope" => SCOPE_COLORS.get(ch).copied(),
        "display.oscilloscope" | "module.response_curve" => {
            Some(MULTI_COLORS[ch % MULTI_COLORS.len()])
        }
        // selector: ch 0 is "select" (no color), ch 1+ are the value inputs
        "module.selector" => if ch == 0 { None } else { Some(MULTI_COLORS[(ch - 1) % MULTI_COLORS.len()]) },
        // split: all outputs are colored
        "module.split" => Some(MULTI_COLORS[ch % MULTI_COLORS.len()]),
        _ => None,
    }
}

// ── Display module body renderers ─────────────────────────────────────────────

const SCOPE_COLORS: [Color32; 4] = [
    Color32::from_rgb(255, 80,  80),   // red
    Color32::from_rgb(80,  220, 80),   // green
    Color32::from_rgb(80,  140, 255),  // blue
    Color32::from_rgb(255, 220, 50),   // yellow
];

// 12 perceptually-spread colors for multi-pin modules (selector inputs, split outputs, etc.).
// The first four match SCOPE_COLORS so oscilloscope channels stay consistent.
const MULTI_COLORS: [Color32; 12] = [
    Color32::from_rgb(255, 80,  80),   //  0 red
    Color32::from_rgb(80,  220, 80),   //  1 green
    Color32::from_rgb(80,  140, 255),  //  2 blue
    Color32::from_rgb(255, 220, 50),   //  3 yellow
    Color32::from_rgb(80,  220, 220),  //  4 cyan
    Color32::from_rgb(220, 80,  220),  //  5 magenta
    Color32::from_rgb(255, 140, 40),   //  6 orange
    Color32::from_rgb(140, 255, 80),   //  7 lime
    Color32::from_rgb(180, 100, 255),  //  8 violet
    Color32::from_rgb(255, 120, 160),  //  9 pink
    Color32::from_rgb(40,  200, 160),  // 10 teal
    Color32::from_rgb(200, 200, 80),   // 11 olive
];

fn show_readout_body(node_id: NodeId, ui: &mut egui::Ui, snarl: &mut Snarl<NodeData>) {
    let sig = snarl
        .get_node(node_id)
        .and_then(|n| n.extra.last_signals.first().copied().flatten());

    use flexinput_core::Signal;
    let text = match sig {
        Some(Signal::Float(f)) => format!("{f:.4}"),
        Some(Signal::Bool(b))  => if b { "true".into() } else { "false".into() },
        Some(Signal::Vec2(v))  => format!("({:.3}, {:.3})", v.x, v.y),
        Some(Signal::Int(i))   => format!("{i}"),
        None                   => "—".into(),
    };
    ui.add_sized(
        [120.0, 24.0],
        egui::Label::new(egui::RichText::new(text).monospace().size(14.0)),
    );
}

fn show_oscilloscope_body(node_id: NodeId, inputs: &[InPin], ui: &mut egui::Ui, snarl: &mut Snarl<NodeData>) {
    // ── Init params on first use ──────────────────────────────────────────────
    let needs_init = snarl.get_node(node_id).map(|n| !n.params.contains_key("osc_win")).unwrap_or(false);
    if needs_init {
        if let Some(node) = snarl.get_node_mut(node_id) {
            node.params.insert("osc_win".into(),   serde_json::json!(256i64));
            node.params.insert("osc_scale".into(), serde_json::json!(1.0));
            node.params.insert("osc_auto".into(),  Value::Bool(false));
            node.params.insert("osc_uni".into(),   Value::Bool(false));
        }
    }

    // ── Read params ───────────────────────────────────────────────────────────
    let (osc_win, osc_scale, osc_auto, osc_uni) = snarl.get_node(node_id).map(|n| {
        let win = n.params.get("osc_win")  .and_then(|v| v.as_i64()).unwrap_or(256).clamp(16, 512) as usize;
        let sc  = n.params.get("osc_scale").and_then(|v| v.as_f64()).unwrap_or(1.0).max(0.001) as f32;
        let au  = n.params.get("osc_auto") .and_then(|v| v.as_bool()).unwrap_or(false);
        let uni = n.params.get("osc_uni")  .and_then(|v| v.as_bool()).unwrap_or(false);
        (win, sc, au, uni)
    }).unwrap_or((256, 1.0, false, false));

    let history = snarl.get_node(node_id).map(|n| n.extra.history.clone()).unwrap_or_default();
    let n_channels = snarl.get_node(node_id).map(|n| n.inputs.len()).unwrap_or(1).max(1);
    let n_total = history.len();
    let start   = n_total.saturating_sub(osc_win);
    let visible: Vec<Vec<Option<f32>>> = history.iter().skip(start).cloned().collect();
    let n       = visible.len();

    // Auto-scale: max absolute value across all visible channels.
    let eff_scale = if osc_auto {
        let max_v = visible.iter()
            .flat_map(|s| s.iter().filter_map(|v| *v))
            .map(|v: f32| v.abs())
            .fold(0.0f32, f32::max);
        if max_v > 0.0 { max_v } else { 1.0 }
    } else {
        osc_scale
    };

    ui.vertical(|ui| {
        egui::Resize::default()
            .id_salt(("osc", node_id))
            .default_size([240.0, 100.0])
            .min_size([60.0, 30.0])
            .show(ui, |ui| {
                let (rect, _) = ui.allocate_exact_size(ui.available_size(), egui::Sense::hover());
                let painter = ui.painter_at(rect);
                painter.rect_filled(rect, 2.0, Color32::from_gray(16));

                // Grid lines.
                for i in 1..4 {
                    let y = if osc_uni {
                        rect.bottom() - rect.height() * (i as f32 / 4.0)
                    } else {
                        rect.top() + rect.height() * (i as f32 / 4.0)
                    };
                    let is_zero = !osc_uni && i == 2;
                    painter.line_segment(
                        [egui::pos2(rect.left(), y), egui::pos2(rect.right(), y)],
                        egui::Stroke::new(
                            if is_zero { 1.0 } else { 0.5 },
                            if is_zero { Color32::from_gray(55) } else { Color32::from_gray(40) },
                        ),
                    );
                }
                // Baseline for uni mode.
                if osc_uni {
                    painter.line_segment(
                        [egui::pos2(rect.left(), rect.bottom()), egui::pos2(rect.right(), rect.bottom())],
                        egui::Stroke::new(1.0, Color32::from_gray(55)),
                    );
                }

                // Signal lines.
                if n >= 2 {
                    for ch in 0..n_channels {
                        let pts: Vec<egui::Pos2> = visible.iter().enumerate().filter_map(|(i, s)| {
                            s.get(ch).copied().flatten().map(|v| {
                                let x = rect.left() + (i as f32 / (n - 1) as f32) * rect.width();
                                let norm = v / eff_scale;
                                let y = if osc_uni {
                                    rect.bottom() - norm.clamp(0.0, 1.0) * rect.height() * 0.92
                                } else {
                                    rect.center().y - norm.clamp(-1.0, 1.0) * rect.height() * 0.45
                                };
                                egui::pos2(x, y)
                            })
                        }).collect();
                        for w in pts.windows(2) {
                            painter.line_segment([w[0], w[1]], egui::Stroke::new(1.5, MULTI_COLORS[ch % MULTI_COLORS.len()]));
                        }
                    }
                }
            });

        // ── Controls ─────────────────────────────────────────────────────────
        let mut win     = osc_win as i64;
        let mut sc      = osc_scale;
        let mut au      = osc_auto;
        let mut uni     = osc_uni;
        let mut changed = false;

        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("Win").small().weak());
            changed |= ui.add(egui::DragValue::new(&mut win).speed(4.0)
                .range(16i64..=512).suffix("pt")).changed();
            ui.separator();
            ui.label(egui::RichText::new("Scale").small().weak());
            if au {
                ui.label(egui::RichText::new(format!("{:.3}", eff_scale)).small().weak());
            } else {
                changed |= ui.add(egui::DragValue::new(&mut sc).speed(0.01)
                    .range(0.001f32..=100.0).max_decimals(3)).changed();
            }
            let au_before = au;
            ui.checkbox(&mut au, egui::RichText::new("Auto").small());
            changed |= au != au_before;
            ui.separator();
            let uni_before = uni;
            ui.selectable_value(&mut uni, false, egui::RichText::new("Bi").small());
            ui.selectable_value(&mut uni, true,  egui::RichText::new("Uni").small());
            changed |= uni != uni_before;
        });

        if changed {
            if let Some(node) = snarl.get_node_mut(node_id) {
                node.params.insert("osc_win".into(),   serde_json::json!(win));
                node.params.insert("osc_auto".into(),  Value::Bool(au));
                node.params.insert("osc_uni".into(),   Value::Bool(uni));
                if let Some(n) = Number::from_f64(sc as f64) {
                    node.params.insert("osc_scale".into(), Value::Number(n));
                }
            }
        }

        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("Ch").small().weak());
            if ui.small_button("+").on_hover_text("Add channel").clicked() {
                if let Some(node) = snarl.get_node_mut(node_id) {
                    let next = node.inputs.len() + 1;
                    node.inputs.push(PinDescriptor::new(format!("ch{}", next), SignalType::Float));
                }
            }
            if n_channels > 1 && ui.small_button("−").on_hover_text("Remove channel").clicked() {
                remove_input_pin(node_id, n_channels - 1, inputs, snarl);
            }
        });
    });
}

fn show_vectorscope_body(node_id: NodeId, ui: &mut egui::Ui, snarl: &mut Snarl<NodeData>) {
    let history = snarl
        .get_node(node_id)
        .map(|n| n.extra.history.clone())
        .unwrap_or_default();

    egui::Resize::default()
        .id_salt(("vs", node_id))
        .default_size([140.0, 140.0])
        .min_size([40.0, 40.0])
        .show(ui, |ui| {
            let side = ui.available_size().min_elem();
            let (rect, _) = ui.allocate_exact_size(egui::Vec2::splat(side), egui::Sense::hover());
            let painter = ui.painter_at(rect);
            painter.rect_filled(rect, 2.0, Color32::from_gray(16));
            painter.line_segment(
                [egui::pos2(rect.center().x, rect.top()), egui::pos2(rect.center().x, rect.bottom())],
                egui::Stroke::new(0.5, Color32::from_gray(50)),
            );
            painter.line_segment(
                [egui::pos2(rect.left(), rect.center().y), egui::pos2(rect.right(), rect.center().y)],
                egui::Stroke::new(0.5, Color32::from_gray(50)),
            );
            painter.circle_stroke(rect.center(), rect.width().min(rect.height()) * 0.45,
                egui::Stroke::new(0.5, Color32::from_gray(40)));

            let n = history.len();
            for (idx, sample) in history.iter().enumerate() {
                let (Some(x), Some(y)) = (sample[0], sample[1]) else { continue; };
                let px = rect.center().x + x.clamp(-1.0, 1.0) * rect.width()  * 0.45;
                let py = rect.center().y - y.clamp(-1.0, 1.0) * rect.height() * 0.45;
                let alpha = ((idx as f32 / n as f32) * 220.0) as u8 + 35;
                painter.circle_filled(egui::pos2(px, py), 1.5,
                    Color32::from_rgba_unmultiplied(80, 200, 255, alpha));
            }
            // Current-position dot: read from last_signals (freshest, set by update_display_nodes).
            let cur = snarl.get_node(node_id).and_then(|n| {
                let x = sig_f32(n.extra.last_signals.get(0)?.as_ref()?);
                let y = sig_f32(n.extra.last_signals.get(1)?.as_ref()?);
                Some((x, y))
            });
            if let Some((x, y)) = cur {
                let px = rect.center().x + x.clamp(-1.0, 1.0) * rect.width()  * 0.45;
                let py = rect.center().y - y.clamp(-1.0, 1.0) * rect.height() * 0.45;
                painter.circle_filled(egui::pos2(px, py), 4.0, Color32::WHITE);
                painter.circle_stroke(egui::pos2(px, py), 4.0,
                    egui::Stroke::new(1.0, Color32::from_gray(100)));
            }
        });
}

// ── Processing module body renderers ──────────────────────────────────────────

fn show_delay_body(node_id: NodeId, ui: &mut egui::Ui, snarl: &mut Snarl<NodeData>) {
    let delay_ms = snarl
        .get_node(node_id)
        .and_then(|n| n.params.get("delay_ms").and_then(|v| v.as_f64()))
        .unwrap_or(100.0) as f32;
    let mut v = delay_ms;
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new("ms").small());
        if ui
            .add(egui::DragValue::new(&mut v).speed(1.0).range(0.0..=60_000.0))
            .changed()
        {
            if let (Some(node), Some(n)) = (
                snarl.get_node_mut(node_id),
                Number::from_f64(v as f64),
            ) {
                node.params.insert("delay_ms".into(), Value::Number(n));
            }
        }
    });
}

fn show_lowpass_body(node_id: NodeId, ui: &mut egui::Ui, snarl: &mut Snarl<NodeData>) {
    let (cutoff, q) = snarl
        .get_node(node_id)
        .map(|n| {
            let c = n.params.get("cutoff_hz").and_then(|v| v.as_f64()).unwrap_or(10.0) as f32;
            let q = n.params.get("q").and_then(|v| v.as_f64()).unwrap_or(0.707) as f32;
            (c, q)
        })
        .unwrap_or((10.0, 0.707));

    let mut c = cutoff;
    let mut q = q;

    let mut changed = false;
    egui::Grid::new(("lp_grid", node_id)).num_columns(2).show(ui, |ui| {
        ui.label(egui::RichText::new("Hz").small());
        changed |= ui
            .add(egui::DragValue::new(&mut c).speed(0.5).range(0.1..=1000.0))
            .changed();
        ui.end_row();
        ui.label(egui::RichText::new("Q").small());
        changed |= ui
            .add(egui::DragValue::new(&mut q).speed(0.01).range(0.1..=0.707))
            .changed();
        ui.end_row();
    });

    if changed {
        if let Some(node) = snarl.get_node_mut(node_id) {
            if let Some(n) = Number::from_f64(c as f64) {
                node.params.insert("cutoff_hz".into(), Value::Number(n));
            }
            if let Some(n) = Number::from_f64(q as f64) {
                node.params.insert("q".into(), Value::Number(n));
            }
        }
    }
}

fn show_response_curve_body(node_id: NodeId, inputs: &[InPin], outputs: &[OutPin], ui: &mut egui::Ui, snarl: &mut Snarl<NodeData>) {
    // ── Initialise params on first use ────────────────────────────────────────
    let needs_init = snarl.get_node(node_id).map(|n| !n.params.contains_key("points")).unwrap_or(false);
    if needs_init {
        if let Some(node) = snarl.get_node_mut(node_id) {
            node.params.insert("points".into(), serde_json::json!([[0.0, 0.0], [1.0, 1.0]]));
            node.params.insert("biases".into(),  serde_json::json!([0.0]));
            node.params.insert("absolute".into(), Value::Bool(true));
            node.params.insert("in_min".into(),   serde_json::json!(-1.0));
            node.params.insert("in_max".into(),   serde_json::json!( 1.0));
            node.params.insert("out_min".into(),  serde_json::json!(-1.0));
            node.params.insert("out_max".into(),  serde_json::json!( 1.0));
            node.params.insert("grid_x".into(),   serde_json::json!(4i64));
            node.params.insert("grid_y".into(),   serde_json::json!(4i64));
            node.params.insert("snap".into(),     Value::Bool(false));
            node.params.insert("in_scale".into(), serde_json::json!(0i64));
            node.params.insert("trail_ms".into(), serde_json::json!(300i64));
        }
    }

    // ── Read params ───────────────────────────────────────────────────────────
    let (points, biases, absolute, in_min, in_max, out_min, out_max, grid_x, grid_y, snap, in_scale, trail_ms) = snarl
        .get_node(node_id)
        .map(|n| {
            let pts: Vec<[f32; 2]> = n.params.get("points")
                .and_then(|v| v.as_array())
                .map(|arr| arr.iter().filter_map(|p| {
                    let a = p.as_array()?;
                    Some([a.get(0)?.as_f64()? as f32, a.get(1)?.as_f64()? as f32])
                }).collect())
                .unwrap_or_else(|| vec![[0.0, 0.0], [1.0, 1.0]]);
            let bss: Vec<f32> = n.params.get("biases")
                .and_then(|v| v.as_array())
                .map(|arr| arr.iter().filter_map(|b| b.as_f64().map(|f| f as f32)).collect())
                .unwrap_or_default();
            let abs  = n.params.get("absolute").and_then(|v| v.as_bool()).unwrap_or(true);
            let i0   = n.params.get("in_min") .and_then(|v| v.as_f64()).unwrap_or(-1.0) as f32;
            let i1   = n.params.get("in_max") .and_then(|v| v.as_f64()).unwrap_or( 1.0) as f32;
            let o0   = n.params.get("out_min").and_then(|v| v.as_f64()).unwrap_or(-1.0) as f32;
            let o1   = n.params.get("out_max").and_then(|v| v.as_f64()).unwrap_or( 1.0) as f32;
            let gx   = n.params.get("grid_x").and_then(|v| v.as_i64()).unwrap_or(4).max(1) as usize;
            let gy   = n.params.get("grid_y").and_then(|v| v.as_i64()).unwrap_or(4).max(1) as usize;
            let sn   = n.params.get("snap").and_then(|v| v.as_bool()).unwrap_or(false);
            let sc   = n.params.get("in_scale").and_then(|v| v.as_i64()).unwrap_or(0);
            let tm   = n.params.get("trail_ms").and_then(|v| v.as_i64()).unwrap_or(300).clamp(0, 1000);
            (pts, bss, abs, i0, i1, o0, o1, gx, gy, sn, sc, tm)
        })
        .unwrap_or_else(|| (vec![[0.0, 0.0], [1.0, 1.0]], vec![], true, -1.0, 1.0, -1.0, 1.0, 4, 4, false, 0, 300));

    let n_channels = snarl.get_node(node_id)
        .map(|n| n.inputs.len().min(n.outputs.len()))
        .unwrap_or(1)
        .max(1);
    let live_inputs: Vec<Option<f32>> = (0..n_channels)
        .map(|ch| snarl.get_node(node_id)
            .and_then(|n| n.extra.last_signals.get(ch)?.as_ref())
            .map(sig_f32))
        .collect();

    let (x_lo, x_hi): (f32, f32) = if absolute { (0.0, 1.0) } else { (-1.0, 1.0) };
    let (y_lo, y_hi): (f32, f32) = if absolute { (0.0, 1.0) } else { (-1.0, 1.0) };
    let x_range = x_hi - x_lo;
    let y_range = y_hi - y_lo;

    let mut new_points   = points.clone();
    let mut new_biases   = biases.clone();
    let mut pts_changed  = false;
    let mut bias_changed = false;

    ui.vertical(|ui| {
        // ── Graph ─────────────────────────────────────────────────────────────
        egui::Resize::default()
            .id_salt(("crv", node_id))
            .default_size([180.0, 180.0])
            .min_size([80.0, 80.0])
            .show(ui, |ui| {
                let (rect, bg_resp) =
                    ui.allocate_exact_size(ui.available_size(), egui::Sense::click());
                let painter = ui.painter_at(rect);

                let c2s = |x: f32, y: f32| egui::pos2(
                    rect.left() + (x - x_lo) / x_range * rect.width(),
                    rect.bottom() - (y - y_lo) / y_range * rect.height(),
                );
                let s2c = |pos: egui::Pos2| -> [f32; 2] {[
                    x_lo + (pos.x - rect.left()) / rect.width() * x_range,
                    y_lo + (rect.bottom() - pos.y) / rect.height() * y_range,
                ]};
                let do_snap = |x: f32, y: f32| -> (f32, f32) {
                    if snap {
                        let nx = ((x - x_lo) / x_range * grid_x as f32).round() / grid_x as f32;
                        let ny = ((y - y_lo) / y_range * grid_y as f32).round() / grid_y as f32;
                        (x_lo + nx * x_range, y_lo + ny * y_range)
                    } else { (x, y) }
                };

                painter.rect_filled(rect, 2.0, Color32::from_gray(16));

                let gs = egui::Stroke::new(0.5, Color32::from_gray(35));
                for i in 1..grid_x {
                    let x = x_lo + x_range * i as f32 / grid_x as f32;
                    painter.line_segment([c2s(x, y_lo), c2s(x, y_hi)], gs);
                }
                for i in 1..grid_y {
                    let y = y_lo + y_range * i as f32 / grid_y as f32;
                    painter.line_segment([c2s(x_lo, y), c2s(x_hi, y)], gs);
                }
                painter.line_segment([c2s(x_lo, y_lo), c2s(x_hi, y_hi)],
                    egui::Stroke::new(0.5, Color32::from_gray(55)));

                if new_points.len() >= 2 {
                    let steps = 120usize;
                    let curve_pts: Vec<egui::Pos2> = (0..=steps)
                        .map(|i| {
                            let x = x_lo + x_range * i as f32 / steps as f32;
                            let y = sample_curve(&new_points, x, &new_biases).clamp(y_lo, y_hi);
                            c2s(x, y)
                        })
                        .collect();
                    for w in curve_pts.windows(2) {
                        painter.line_segment([w[0], w[1]],
                            egui::Stroke::new(1.5, Color32::from_gray(200)));
                    }
                }

                let alt_held = ui.input(|i| i.modifiers.alt);
                if alt_held && new_points.len() >= 2 {
                    while new_biases.len() < new_points.len() - 1 { new_biases.push(0.0); }
                    for seg in 0..(new_points.len() - 1) {
                        let mid_x = (new_points[seg][0] + new_points[seg + 1][0]) * 0.5;
                        let mid_y = sample_curve(&new_points, mid_x, &new_biases).clamp(y_lo, y_hi);
                        let hpos  = c2s(mid_x, mid_y);
                        let hid   = ui.id().with(("bias_h", node_id, seg));
                        let hresp = ui.interact(
                            egui::Rect::from_center_size(hpos, egui::Vec2::splat(14.0)),
                            hid, egui::Sense::click_and_drag());
                        if hresp.double_clicked() {
                            new_biases[seg] = 0.0;
                            bias_changed = true;
                        } else if hresp.dragged() {
                            let dy = -hresp.drag_delta().y / rect.height() * y_range;
                            new_biases[seg] = (new_biases[seg] + dy).clamp(-2.0, 2.0);
                            bias_changed = true;
                        }
                        let hcol = if hresp.hovered() || hresp.dragged() {
                            Color32::from_rgb(255, 220, 50)
                        } else {
                            Color32::from_rgb(180, 140, 20)
                        };
                        painter.circle_filled(hpos, 4.0, hcol);
                        painter.circle_stroke(hpos, 4.0,
                            egui::Stroke::new(1.0, Color32::from_gray(100)));
                    }
                }

                let mut remove_idx: Option<usize> = None;
                for i in 0..new_points.len() {
                    let [px, py] = new_points[i];
                    let screen   = c2s(px, py);
                    let pt_id    = ui.id().with(("cpt", node_id, i));
                    let pt_resp  = ui.interact(
                        egui::Rect::from_center_size(screen, egui::Vec2::splat(12.0)),
                        pt_id, egui::Sense::click_and_drag());

                    if pt_resp.dragged() && !alt_held {
                        let d      = pt_resp.drag_delta();
                        let nx_raw = px + d.x * x_range / rect.width();
                        let ny_raw = py - d.y * y_range / rect.height();
                        let lo_x   = new_points.get(i.wrapping_sub(1)).map(|p| p[0] + 0.001).unwrap_or(x_lo);
                        let hi_x   = new_points.get(i + 1).map(|p| p[0] - 0.001).unwrap_or(x_hi);
                        let (sx, sy) = do_snap(nx_raw, ny_raw);
                        new_points[i] = [sx.clamp(lo_x, hi_x), sy.clamp(y_lo, y_hi)];
                        pts_changed = true;
                    }
                    if pt_resp.secondary_clicked() && new_points.len() > 2 {
                        remove_idx = Some(i);
                        pts_changed = true;
                    }
                    let col = if pt_resp.hovered() || pt_resp.dragged() { Color32::WHITE } else { Color32::from_gray(190) };
                    painter.circle_filled(screen, 5.0, col);
                    painter.circle_stroke(screen, 5.0, egui::Stroke::new(1.0, Color32::from_gray(80)));
                }

                if bg_resp.double_clicked() {
                    if let Some(pos) = bg_resp.interact_pointer_pos() {
                        let [gx_raw, gy_raw] = s2c(pos);
                        let (gx_sn, gy_sn)   = do_snap(gx_raw, gy_raw);
                        let gx = gx_sn.clamp(x_lo, x_hi);
                        let gy = gy_sn.clamp(y_lo, y_hi);
                        let idx = new_points.partition_point(|p| p[0] < gx);
                        new_points.insert(idx, [gx, gy]);
                        pts_changed = true;
                    }
                }
                if let Some(idx) = remove_idx { new_points.remove(idx); }

                // Live-position trails — trail_ms history, y always recomputed
                // from the live curve so dragging control points leaves no streaks.
                let abs_max   = in_max.abs().max(in_min.abs()).max(f32::EPSILON);
                let trail_dur = std::time::Duration::from_millis(trail_ms as u64);
                let now       = std::time::Instant::now();
                let mut has_active = false;
                for (ch, raw_opt) in live_inputs.iter().enumerate() {
                    let Some(raw) = raw_opt else { continue; };
                    has_active = true;
                    let graph_x = if absolute {
                        curve_scale((raw.abs() / abs_max).clamp(0.0, 1.0), in_scale)
                    } else {
                        let in_range = (in_max - in_min).abs().max(f32::EPSILON);
                        let norm     = ((raw - in_min) / in_range * 2.0 - 1.0).clamp(-1.0, 1.0);
                        let sign     = if norm < 0.0 { -1.0f32 } else { 1.0 };
                        sign * curve_scale(norm.abs(), in_scale)
                    };
                    // Store only graph_x; y is recomputed at draw time from the current curve.
                    type Trail = std::collections::VecDeque<(f32, std::time::Instant)>;
                    let trail_id = ui.id().with(("trail", node_id, ch as u32));
                    let mut trail: Trail = ui.data(|d| d.get_temp::<Trail>(trail_id).clone().unwrap_or_default());
                    if trail_ms > 0 {
                        trail.push_back((graph_x, now));
                        while trail.front().map(|&(_, t)| now.duration_since(t) > trail_dur).unwrap_or(false) {
                            trail.pop_front();
                        }
                    } else {
                        trail.clear();
                    }
                    let trail_pts: Vec<(f32, std::time::Instant)> = trail.iter().cloned().collect();
                    ui.data_mut(|d| d.insert_temp(trail_id, trail));
                    let ch_col = MULTI_COLORS[ch % MULTI_COLORS.len()];
                    for w in trail_pts.windows(2) {
                        let (x0, _)  = w[0];
                        let (x1, t1) = w[1];
                        let age   = now.duration_since(t1).as_secs_f32() / trail_dur.as_secs_f32();
                        let alpha = ((1.0 - age.clamp(0.0, 1.0)) * 220.0) as u8;
                        let col   = Color32::from_rgba_unmultiplied(ch_col.r(), ch_col.g(), ch_col.b(), alpha);
                        let steps = (((x1 - x0).abs() / x_range * 80.0) as usize).max(1);
                        let x0_y  = sample_curve(&new_points, x0, &new_biases).clamp(y_lo, y_hi);
                        let mut prev = c2s(x0, x0_y);
                        for s in 1..=steps {
                            let t  = s as f32 / steps as f32;
                            let ix = x0 + (x1 - x0) * t;
                            let iy = sample_curve(&new_points, ix, &new_biases).clamp(y_lo, y_hi);
                            let next = c2s(ix, iy);
                            painter.line_segment([prev, next], egui::Stroke::new(1.5, col));
                            prev = next;
                        }
                    }
                    let graph_y = sample_curve(&new_points, graph_x, &new_biases).clamp(y_lo, y_hi);
                    let head_col = Color32::from_rgba_unmultiplied(ch_col.r(), ch_col.g(), ch_col.b(), 220);
                    painter.circle_filled(c2s(graph_x, graph_y), 3.5, head_col);
                }
                if has_active {
                    ui.ctx().request_repaint();
                }
            });

        // ── Write back curve points / biases ──────────────────────────────────
        if pts_changed || bias_changed {
            if let Some(node) = snarl.get_node_mut(node_id) {
                if pts_changed {
                    new_biases.resize(new_points.len().saturating_sub(1), 0.0);
                    let json: Vec<Value> = new_points.iter().map(|p| serde_json::json!([p[0], p[1]])).collect();
                    node.params.insert("points".into(), Value::Array(json));
                }
                let bj: Vec<Value> = new_biases.iter()
                    .filter_map(|&b| Number::from_f64(b as f64).map(Value::Number))
                    .collect();
                node.params.insert("biases".into(), Value::Array(bj));
            }
        }

        // ── Controls below graph ──────────────────────────────────────────────
        let mut i0       = in_min;
        let mut i1       = in_max;
        let mut o0       = out_min;
        let mut o1       = out_max;
        let mut gx_f     = grid_x as f64;
        let mut gy_f     = grid_y as f64;
        let mut abs      = absolute;
        let mut snap_on  = snap;
        let mut sc       = in_scale;
        let mut tm       = trail_ms;
        let mut changed  = false;

        // Row 1: Input scale mode + Absolute + Snap
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("Scale").small().weak());
            for (label, val) in [("Lin", 0i64), ("Log", 1), ("Exp", 2)] {
                let selected = sc == val;
                if ui.selectable_label(selected, egui::RichText::new(label).small()).clicked() && !selected {
                    sc = val;
                    changed = true;
                }
            }
            ui.separator();
            let abs_before = abs;
            ui.checkbox(&mut abs, egui::RichText::new("Abs").small());
            changed |= abs != abs_before;
            let snap_before = snap_on;
            ui.checkbox(&mut snap_on, egui::RichText::new("Snap").small());
            changed |= snap_on != snap_before;
        });

        // Row 2: In/Out range
        egui::Grid::new(("crv_rng", node_id)).num_columns(5).spacing([4.0, 2.0]).show(ui, |ui| {
            ui.label(egui::RichText::new("In").small().weak());
            changed |= ui.add(egui::DragValue::new(&mut i0).speed(0.01).prefix("↓").max_decimals(2)).changed();
            changed |= ui.add(egui::DragValue::new(&mut i1).speed(0.01).prefix("↑").max_decimals(2)).changed();
            ui.label(egui::RichText::new("Out").small().weak());
            changed |= ui.add(egui::DragValue::new(&mut o0).speed(0.01).prefix("↓").max_decimals(2)).changed();
            ui.end_row();
            ui.label(egui::RichText::new("").small());
            ui.label(egui::RichText::new("").small());
            ui.label(egui::RichText::new("").small());
            ui.label(egui::RichText::new("").small());
            changed |= ui.add(egui::DragValue::new(&mut o1).speed(0.01).prefix("↑").max_decimals(2)).changed();
            ui.end_row();
        });

        // Row 3: Grid + Trail
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("Grid").small().weak());
            changed |= ui.add(egui::DragValue::new(&mut gx_f).speed(0.25)
                .range(1.0..=20.0).max_decimals(0).prefix("H ")).changed();
            changed |= ui.add(egui::DragValue::new(&mut gy_f).speed(0.25)
                .range(1.0..=20.0).max_decimals(0).prefix("V ")).changed();
            ui.separator();
            ui.label(egui::RichText::new("Trail").small().weak());
            changed |= ui.add(egui::DragValue::new(&mut tm).speed(5.0)
                .range(0i64..=1000).suffix("ms")).changed();
        });

        if changed {
            if let Some(node) = snarl.get_node_mut(node_id) {
                for (k, v) in [
                    ("in_min", i0 as f64), ("in_max", i1 as f64),
                    ("out_min", o0 as f64), ("out_max", o1 as f64),
                ] {
                    if let Some(n) = Number::from_f64(v) { node.params.insert(k.into(), Value::Number(n)); }
                }
                node.params.insert("absolute".into(), Value::Bool(abs));
                node.params.insert("grid_x".into(),   serde_json::json!(gx_f as i64));
                node.params.insert("grid_y".into(),   serde_json::json!(gy_f as i64));
                node.params.insert("snap".into(),     Value::Bool(snap_on));
                node.params.insert("in_scale".into(), serde_json::json!(sc));
                node.params.insert("trail_ms".into(), serde_json::json!(tm));
            }
        }

        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("Ch").small().weak());
            if ui.small_button("+").on_hover_text("Add parallel channel").clicked() {
                if let Some(node) = snarl.get_node_mut(node_id) {
                    let next = node.inputs.len() + 1;
                    node.inputs.push(PinDescriptor::new(format!("In {}", next), SignalType::Float));
                    node.outputs.push(PinDescriptor::new(format!("Out {}", next), SignalType::Float));
                }
            }
            if n_channels > 1 && ui.small_button("−").on_hover_text("Remove last channel").clicked() {
                remove_input_pin(node_id, n_channels - 1, inputs, snarl);
                remove_output_pin(node_id, n_channels - 1, outputs, snarl);
            }
        });
    });
}

/// Maps x ∈ [0,1] → [0,1] with the chosen scale mode.
/// Lin (0): identity. Log (1): high resolution for small x. Exp (2): high resolution for large x.
fn curve_scale(x: f32, mode: i64) -> f32 {
    use std::f32::consts::E;
    match mode {
        1 => (1.0 + x * (E - 1.0)).ln(),           // ln(1 + x*(e-1)): f(0)=0, f(1)=1
        2 => (x.exp() - 1.0) / (E - 1.0),          // (e^x - 1)/(e-1): f(0)=0, f(1)=1
        _ => x,
    }
}

// ── Signal helpers ────────────────────────────────────────────────────────────

fn sig_f32(s: &Signal) -> f32 {
    match s {
        Signal::Float(f) => *f,
        Signal::Bool(b)  => if *b { 1.0 } else { 0.0 },
        Signal::Int(i)   => *i as f32,
        Signal::Vec2(v)  => v.length(),
    }
}
