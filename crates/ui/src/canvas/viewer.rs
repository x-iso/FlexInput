use egui::Color32;
use egui_snarl::{
    ui::{AnyPins, PinInfo, SnarlViewer},
    InPin, InPinId, NodeId, OutPin, OutPinId, Snarl,
};
use flexinput_core::{ModuleDescriptor, PinDescriptor, Signal, SignalType};
use flexinput_devices::midi::cc_display_name;
use flexinput_engine::SAMPLE_RATE;
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
                | "module.delay" | "module.average" | "module.dc_filter" | "module.response_curve" | "module.vec_response_curve"
                | "math.add" | "math.subtract" | "math.multiply" | "math.divide"
                | "module.selector" | "module.split"
                | "logic.greater_than" | "logic.less_than" | "logic.delay" | "logic.counter"
                | "generator.oscillator" | "processing.gyro_3dof"
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
            "display.vectorscope"   => show_vectorscope_body(node_id, inputs, ui, snarl),
            "module.delay"     => show_delay_body(node_id, inputs, outputs, ui, snarl),
            "module.average"   => show_average_body(node_id, inputs, outputs, ui, snarl),
            "module.dc_filter" => show_dc_filter_body(node_id, inputs, outputs, ui, snarl),
            "module.response_curve"     => show_response_curve_body(node_id, inputs, outputs, ui, snarl),
            "module.vec_response_curve" => show_vec_response_curve_body(node_id, inputs, outputs, ui, snarl),
            "math.add" | "math.subtract" | "math.multiply" | "math.divide" => {
                show_math_variadic_body(node_id, inputs, ui, snarl);
            }
            "module.selector" => show_selector_body(node_id, inputs, ui, snarl),
            "module.split"    => show_split_body(node_id, outputs, ui, snarl),
            "logic.greater_than" | "logic.less_than" => show_or_equal_body(node_id, ui, snarl),
            "logic.delay"   => show_logic_delay_body(node_id, ui, snarl),
            "logic.counter"        => show_counter_body(node_id, inputs, ui, snarl),
            "generator.oscillator"  => show_oscillator_body(node_id, inputs, ui, snarl),
            "processing.gyro_3dof"  => show_gyro_3dof_body(node_id, ui, snarl),
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
    let mut interp = snarl.get_node(node_id)
        .and_then(|n| n.params.get("interpolate").and_then(|v| v.as_bool()))
        .unwrap_or(false);

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
        let interp_before = interp;
        ui.checkbox(&mut interp, egui::RichText::new("Interp").small());
        if interp != interp_before {
            if let Some(node) = snarl.get_node_mut(node_id) {
                node.params.insert("interpolate".to_string(), Value::Bool(interp));
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
    let mut interp = snarl.get_node(node_id)
        .and_then(|n| n.params.get("interpolate").and_then(|v| v.as_bool()))
        .unwrap_or(false);

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
        let interp_before = interp;
        ui.checkbox(&mut interp, egui::RichText::new("Interp").small());
        if interp != interp_before {
            if let Some(node) = snarl.get_node_mut(node_id) {
                node.params.insert("interpolate".to_string(), Value::Bool(interp));
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

fn show_oscillator_body(node_id: NodeId, inputs: &[InPin], ui: &mut egui::Ui, snarl: &mut Snarl<NodeData>) {
    let (shape, freq_unit, freq_p, phase_p, bipolar) = snarl.get_node(node_id).map(|n| {
        let shape      = n.params.get("shape")     .and_then(|v| v.as_str()) .unwrap_or("sine").to_string();
        let freq_unit  = n.params.get("freq_unit") .and_then(|v| v.as_str()) .unwrap_or("hz").to_string();
        let freq_p     = n.params.get("freq_param") .and_then(|v| v.as_f64()).unwrap_or(1.0)  as f32;
        let phase_p    = n.params.get("phase_param").and_then(|v| v.as_f64()).unwrap_or(0.0)  as f32;
        let bipolar    = n.params.get("bipolar")   .and_then(|v| v.as_bool()).unwrap_or(true);
        (shape, freq_unit, freq_p, phase_p, bipolar)
    }).unwrap_or_default();

    let freq_wired  = inputs.get(0).map(|p| !p.remotes.is_empty()).unwrap_or(false);
    let phase_wired = inputs.get(1).map(|p| !p.remotes.is_empty()).unwrap_or(false);

    let mut shape     = shape;
    let mut freq_unit = freq_unit;
    let mut freq_p    = freq_p;
    let mut phase_p   = phase_p;
    let mut bipolar   = bipolar;
    let mut changed   = false;

    ui.vertical(|ui| {
        // Row 1: shape selector
        ui.horizontal(|ui| {
            changed |= ui.selectable_value(&mut shape, "sine".into(),     egui::RichText::new("Sine").small()).changed();
            changed |= ui.selectable_value(&mut shape, "triangle".into(), egui::RichText::new("Tri").small()).changed();
            changed |= ui.selectable_value(&mut shape, "saw".into(),      egui::RichText::new("Saw").small()).changed();
            changed |= ui.selectable_value(&mut shape, "square".into(),   egui::RichText::new("Sqr").small()).changed();
        });

        // Row 2: frequency unit toggle + value
        ui.horizontal(|ui| {
            changed |= ui.selectable_value(&mut freq_unit, "hz".into(), egui::RichText::new("Hz").small()).changed();
            changed |= ui.selectable_value(&mut freq_unit, "ms".into(), egui::RichText::new("ms").small()).changed();
            let (lo, hi, spd) = if freq_unit == "hz" { (0.01, 200.0, 0.1) } else { (1.0, 60_000.0, 10.0) };
            changed |= ui.add_enabled(!freq_wired, egui::DragValue::new(&mut freq_p).speed(spd).range(lo..=hi)).changed();
        });

        // Row 3: phase offset
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("Phase").small().weak());
            changed |= ui.add_enabled(!phase_wired, egui::DragValue::new(&mut phase_p).speed(0.01).range(0.0..=1.0)).changed();
            // Bi/Uni toggle
            ui.separator();
            changed |= ui.selectable_value(&mut bipolar, true,  egui::RichText::new("Bi").small()).changed();
            changed |= ui.selectable_value(&mut bipolar, false, egui::RichText::new("Uni").small()).changed();
        });

        // Row 4: waveform preview
        let preview_size = egui::vec2(ui.available_width().max(80.0), 36.0);
        let (rect, _) = ui.allocate_exact_size(preview_size, egui::Sense::hover());
        if ui.is_rect_visible(rect) {
            let painter = ui.painter_at(rect);
            painter.rect_filled(rect, 2.0, egui::Color32::from_gray(18));

            // Zero / baseline grid line
            let zero_y = if bipolar {
                rect.center().y
            } else {
                rect.bottom()
            };
            painter.line_segment(
                [egui::pos2(rect.left(), zero_y), egui::pos2(rect.right(), zero_y)],
                egui::Stroke::new(0.5, egui::Color32::from_gray(55)),
            );

            // Waveform
            let n = 128usize;
            let pts: Vec<egui::Pos2> = (0..=n).map(|i| {
                let t = i as f32 / n as f32;
                let phase = (t + phase_p).rem_euclid(1.0);
                let v = {
                    let raw = flexinput_engine::osc_sample(&shape, phase);
                    if bipolar { raw } else { (raw + 1.0) * 0.5 }
                };
                let x = rect.left() + t * rect.width();
                let y = if bipolar {
                    rect.center().y - v * rect.height() * 0.45
                } else {
                    rect.bottom() - v * rect.height() * 0.9
                };
                egui::pos2(x, y.clamp(rect.top(), rect.bottom()))
            }).collect();
            painter.add(egui::Shape::line(pts, egui::Stroke::new(1.5, egui::Color32::from_rgb(100, 180, 255))));
        }
    });

    if changed {
        if let Some(node) = snarl.get_node_mut(node_id) {
            node.params.insert("shape".into(),      Value::String(shape));
            node.params.insert("freq_unit".into(),  Value::String(freq_unit));
            node.params.insert("bipolar".into(),    Value::Bool(bipolar));
            if let Some(n) = Number::from_f64(freq_p  as f64) { node.params.insert("freq_param".into(),  Value::Number(n)); }
            if let Some(n) = Number::from_f64(phase_p as f64) { node.params.insert("phase_param".into(), Value::Number(n)); }
        }
    }
}

fn show_gyro_3dof_body(node_id: NodeId, ui: &mut egui::Ui, snarl: &mut Snarl<NodeData>) {
    let (mode, inv_yaw, inv_pitch, inv_roll, inv_ax, inv_ay, inv_az, out_x, out_y) =
        snarl.get_node(node_id).map(|n| {
            let mode      = n.params.get("mode")      .and_then(|v| v.as_str()) .unwrap_or("local").to_string();
            let inv_yaw   = n.params.get("inv_yaw")   .and_then(|v| v.as_bool()).unwrap_or(false);
            let inv_pitch = n.params.get("inv_pitch")  .and_then(|v| v.as_bool()).unwrap_or(false);
            let inv_roll  = n.params.get("inv_roll")   .and_then(|v| v.as_bool()).unwrap_or(false);
            let inv_ax    = n.params.get("inv_accel_x").and_then(|v| v.as_bool()).unwrap_or(false);
            let inv_ay    = n.params.get("inv_accel_y").and_then(|v| v.as_bool()).unwrap_or(false);
            let inv_az    = n.params.get("inv_accel_z").and_then(|v| v.as_bool()).unwrap_or(false);
            let out_x = if let Some(Some(Signal::Float(f))) = n.extra.last_signals.get(1) { *f } else { 0.0_f32 };
            let out_y = if let Some(Some(Signal::Float(f))) = n.extra.last_signals.get(2) { *f } else { 0.0_f32 };
            (mode, inv_yaw, inv_pitch, inv_roll, inv_ax, inv_ay, inv_az, out_x, out_y)
        }).unwrap_or_default();

    let mut mode      = mode;
    let mut inv_gyro  = [inv_yaw, inv_pitch, inv_roll];
    let mut inv_accel = [inv_ax, inv_ay, inv_az];
    let mut changed   = false;

    const GYR_LABELS: [(&str, &str); 3] = [
        ("yaw",   "gyro_z — invert if rotating right gives negative X\n(expected: right = positive X)"),
        ("pitch", "gyro_y — invert if tilting up gives negative Y\n(expected: up = positive Y)"),
        ("roll",  "gyro_x — only affects Player/World space gravity correction"),
    ];
    const ACC_LABELS: [(&str, &str); 3] = [
        ("X",  "accel_x — invert if Player/World horizontal correction is backwards"),
        ("Y",  "accel_y — invert if Player/World vertical correction is backwards"),
        ("+Z", "accel_z — expected POSITIVE when controller is held flat face-up (≈ +1 G).\nInvert if your device reports negative when flat."),
    ];

    ui.vertical(|ui| {
        ui.spacing_mut().item_spacing = egui::vec2(2.0, 2.0);

        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 2.0;
            for (label, id) in [("Local", "local"), ("Player", "player"), ("World", "world"), ("Laser", "laser")] {
                changed |= ui.selectable_value(&mut mode, id.to_string(), egui::RichText::new(label).small()).changed();
            }
        });

        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 3.0;
            ui.label(egui::RichText::new("Gyr:").small().weak());
            for i in 0..3 {
                let (label, tip) = GYR_LABELS[i];
                changed |= ui.checkbox(&mut inv_gyro[i], egui::RichText::new(label).small())
                    .on_hover_text(tip).changed();
            }
        });

        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 3.0;
            ui.label(egui::RichText::new("Acc:").small().weak());
            for i in 0..3 {
                let (label, tip) = ACC_LABELS[i];
                changed |= ui.checkbox(&mut inv_accel[i], egui::RichText::new(label).small())
                    .on_hover_text(tip).changed();
            }
        });

        ui.label(egui::RichText::new(format!("X:{:+.3}  Y:{:+.3}", out_x, out_y)).small().weak());
    });

    if changed {
        if let Some(node) = snarl.get_node_mut(node_id) {
            node.params.insert("mode".into(),       Value::String(mode));
            node.params.insert("inv_yaw".into(),    Value::Bool(inv_gyro[0]));
            node.params.insert("inv_pitch".into(),  Value::Bool(inv_gyro[1]));
            node.params.insert("inv_roll".into(),   Value::Bool(inv_gyro[2]));
            node.params.insert("inv_accel_x".into(),Value::Bool(inv_accel[0]));
            node.params.insert("inv_accel_y".into(),Value::Bool(inv_accel[1]));
            node.params.insert("inv_accel_z".into(),Value::Bool(inv_accel[2]));
        }
    }
}

fn show_counter_body(node_id: NodeId, inputs: &[InPin], ui: &mut egui::Ui, snarl: &mut Snarl<NodeData>) {
    let (mode, normalized, step_p, min_p, max_p) = snarl.get_node(node_id).map(|n| {
        let mode       = n.params.get("mode")      .and_then(|v| v.as_str()) .unwrap_or("loop").to_string();
        let normalized = n.params.get("normalized").and_then(|v| v.as_bool()).unwrap_or(false);
        let step_p     = n.params.get("step_param").and_then(|v| v.as_f64()).unwrap_or(1.0)  as f32;
        let min_p      = n.params.get("min_param") .and_then(|v| v.as_f64()).unwrap_or(0.0)  as f32;
        let max_p      = n.params.get("max_param") .and_then(|v| v.as_f64()).unwrap_or(10.0) as f32;
        (mode, normalized, step_p, min_p, max_p)
    }).unwrap_or_default();

    let step_wired  = inputs.get(3).map(|p| !p.remotes.is_empty()).unwrap_or(false);
    let min_wired   = inputs.get(4).map(|p| !p.remotes.is_empty()).unwrap_or(false);
    let max_wired   = inputs.get(5).map(|p| !p.remotes.is_empty()).unwrap_or(false);

    let mut mode       = mode;
    let mut normalized = normalized;
    let mut step_p     = step_p;
    let mut min_p      = min_p;
    let mut max_p      = max_p;
    let mut changed    = false;

    ui.vertical(|ui| {
        // Row 1: counting mode
        ui.horizontal(|ui| {
            changed |= ui.selectable_value(&mut mode, "loop".into(),      egui::RichText::new("Loop").small()).changed();
            changed |= ui.selectable_value(&mut mode, "limit".into(),     egui::RichText::new("Limit").small()).changed();
            changed |= ui.selectable_value(&mut mode, "bounce".into(),    egui::RichText::new("Bounce").small()).changed();
            changed |= ui.selectable_value(&mut mode, "unlimited".into(), egui::RichText::new("Unlimited").small()).changed();
        });

        // Row 2: output range + reset button
        ui.horizontal(|ui| {
            changed |= ui.selectable_value(&mut normalized, false, egui::RichText::new("Raw").small()).changed();
            changed |= ui.selectable_value(&mut normalized, true,  egui::RichText::new("0..1").small()).changed();
            if ui.small_button("↺").on_hover_text("Reset counter").clicked() {
                if let Some(node) = snarl.get_node_mut(node_id) {
                    while node.extra.aux_f32.len() < 2 { node.extra.aux_f32.push(0.0); }
                    node.extra.aux_f32[0] = 0.0;
                    node.extra.aux_f32[1] = 1.0;
                    node.extra.aux_f32_dirty = true;
                }
            }
        });

        // Row 3: Step
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("Step").small().weak());
            changed |= ui.add_enabled(!step_wired, egui::DragValue::new(&mut step_p).speed(0.1).range(0.001..=10000.0)).changed();
        });

        // Row 4: Min / Max
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("Min").small().weak());
            changed |= ui.add_enabled(!min_wired, egui::DragValue::new(&mut min_p).speed(0.1)).changed();
            ui.label(egui::RichText::new("Max").small().weak());
            let max_active = !max_wired && mode != "unlimited";
            changed |= ui.add_enabled(max_active, egui::DragValue::new(&mut max_p).speed(0.1)).changed();
        });
    });

    if changed {
        if let Some(node) = snarl.get_node_mut(node_id) {
            node.params.insert("mode".into(),       Value::String(mode));
            node.params.insert("normalized".into(), Value::Bool(normalized));
            if let Some(n) = Number::from_f64(step_p as f64) { node.params.insert("step_param".into(), Value::Number(n)); }
            if let Some(n) = Number::from_f64(min_p  as f64) { node.params.insert("min_param".into(),  Value::Number(n)); }
            if let Some(n) = Number::from_f64(max_p  as f64) { node.params.insert("max_param".into(),  Value::Number(n)); }
        }
    }
}

fn show_logic_delay_body(node_id: NodeId, ui: &mut egui::Ui, snarl: &mut Snarl<NodeData>) {
    let (mode, time, unit) = snarl.get_node(node_id).map(|n| {
        let mode = n.params.get("mode").and_then(|v| v.as_str()).unwrap_or("delay_false").to_string();
        let time = n.params.get("time").and_then(|v| v.as_f64()).unwrap_or(100.0);
        let unit = n.params.get("unit").and_then(|v| v.as_str()).unwrap_or("ms").to_string();
        (mode, time, unit)
    }).unwrap_or_default();

    let mut mode = mode;
    let mut time = time as f32;
    let mut unit = unit;
    let mut changed = false;

    ui.horizontal(|ui| {
        changed |= ui.selectable_value(&mut mode, "delay_true".into(),  egui::RichText::new("Delay ON").small()).changed();
        changed |= ui.selectable_value(&mut mode, "delay_false".into(), egui::RichText::new("Delay OFF").small()).changed();
    });
    ui.horizontal(|ui| {
        let limit = if unit == "ms" { 60_000.0 } else { 10_000.0 };
        changed |= ui.add(egui::DragValue::new(&mut time).speed(1.0).range(0.0..=limit)).changed();
        changed |= ui.selectable_value(&mut unit, "ms".into(),      egui::RichText::new("ms").small()).changed();
        changed |= ui.selectable_value(&mut unit, "samples".into(), egui::RichText::new("frames").small()).changed();
    });

    if changed {
        if let Some(node) = snarl.get_node_mut(node_id) {
            node.params.insert("mode".into(), Value::String(mode));
            node.params.insert("unit".into(), Value::String(unit));
            if let Some(n) = Number::from_f64(time as f64) {
                node.params.insert("time".into(), Value::Number(n));
            }
        }
    }
}

fn show_or_equal_body(node_id: NodeId, ui: &mut egui::Ui, snarl: &mut Snarl<NodeData>) {
    let or_equal = snarl
        .get_node(node_id)
        .and_then(|n| n.params.get("or_equal").and_then(|v| v.as_bool()))
        .unwrap_or(false);
    let mut v = or_equal;
    if ui.checkbox(&mut v, egui::RichText::new("or equal").small()).changed() {
        if let Some(node) = snarl.get_node_mut(node_id) {
            node.params.insert("or_equal".to_string(), Value::Bool(v));
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
    let color = Color32::from_rgb(r, g, b);
    if t == SignalType::AutoMap {
        PinInfo::square()
            .with_fill(color)
            .with_wire_width_factor(4.0)
    } else {
        PinInfo::circle().with_fill(color)
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

// ── Pin label color helpers ───────────────────────────────────────────────────

fn channel_label_color(module_id: &str, ch: usize) -> Option<Color32> {
    match module_id {
        "display.vectorscope" | "display.oscilloscope" | "module.response_curve" | "module.vec_response_curve" => {
            Some(MULTI_COLORS[ch % MULTI_COLORS.len()])
        }
        // selector: ch 0 is "select" (no color), ch 1+ are the value inputs
        "module.selector" => if ch == 0 { None } else { Some(MULTI_COLORS[(ch - 1) % MULTI_COLORS.len()]) },
        "module.split" | "module.delay" | "module.average" | "module.dc_filter" => {
            Some(MULTI_COLORS[ch % MULTI_COLORS.len()])
        }
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
    let needs_init = snarl.get_node(node_id).map(|n| !n.params.contains_key("osc_win_ms")).unwrap_or(false);
    if needs_init {
        if let Some(node) = snarl.get_node_mut(node_id) {
            node.params.insert("osc_win_ms".into(), serde_json::json!(200.0f64));
            node.params.insert("osc_scale".into(), serde_json::json!(1.0));
            node.params.insert("osc_auto".into(),  Value::Bool(false));
            node.params.insert("osc_uni".into(),   Value::Bool(false));
        }
    }

    // ── Read params ───────────────────────────────────────────────────────────
    let (win_ms, osc_scale, osc_auto, osc_uni) = snarl.get_node(node_id).map(|n| {
        let win = n.params.get("osc_win_ms").and_then(|v| v.as_f64()).unwrap_or(200.0).clamp(10.0, 10000.0) as f32;
        let sc  = n.params.get("osc_scale") .and_then(|v| v.as_f64()).unwrap_or(1.0).max(0.001) as f32;
        let au  = n.params.get("osc_auto")  .and_then(|v| v.as_bool()).unwrap_or(false);
        let uni = n.params.get("osc_uni")   .and_then(|v| v.as_bool()).unwrap_or(false);
        (win, sc, au, uni)
    }).unwrap_or((200.0, 1.0, false, false));
    let osc_win = (win_ms / 1000.0 * SAMPLE_RATE as f32) as usize;

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

                // Downsample to pixel budget so line count never exceeds display width.
                let pixel_budget = (rect.width().ceil() as usize).max(2);
                let n_ch_inner = if n > 0 { visible[0].len() } else { 0 };
                let display: Vec<Vec<Option<f32>>> = if n <= pixel_budget {
                    visible.clone()
                } else {
                    (0..pixel_budget).map(|i| {
                        let lo = i * n / pixel_budget;
                        let hi = ((i + 1) * n / pixel_budget).min(n);
                        (0..n_ch_inner).map(|ch| {
                            let vals: Vec<f32> = visible[lo..hi].iter()
                                .filter_map(|s| s.get(ch).copied().flatten())
                                .collect();
                            if vals.is_empty() { None } else { Some(vals.iter().sum::<f32>() / vals.len() as f32) }
                        }).collect()
                    }).collect()
                };
                let nd = display.len();

                // Signal lines.
                if nd >= 2 {
                    for ch in 0..n_channels {
                        let pts: Vec<egui::Pos2> = display.iter().enumerate().filter_map(|(i, s)| {
                            s.get(ch).copied().flatten().map(|v| {
                                let x = rect.left() + (i as f32 / (nd - 1) as f32) * rect.width();
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
        let mut win_ms_ctrl = win_ms;
        let mut sc      = osc_scale;
        let mut au      = osc_auto;
        let mut uni     = osc_uni;
        let mut changed = false;

        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("Win").small().weak());
            changed |= ui.add(egui::Slider::new(&mut win_ms_ctrl, 10.0f32..=10000.0)
                .logarithmic(true).show_value(false)).changed();
            let lbl = if win_ms_ctrl >= 1000.0 {
                format!("{:.1}s", win_ms_ctrl / 1000.0)
            } else {
                format!("{:.0}ms", win_ms_ctrl)
            };
            ui.label(egui::RichText::new(lbl).small().weak());
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
                if let Some(n) = Number::from_f64(win_ms_ctrl as f64) { node.params.insert("osc_win_ms".into(), Value::Number(n)); }
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

fn show_vectorscope_body(node_id: NodeId, inputs: &[InPin], ui: &mut egui::Ui, snarl: &mut Snarl<NodeData>) {
    let (history, n_channels, last_signals) = snarl
        .get_node(node_id)
        .map(|n| (n.extra.history.clone(), n.inputs.len().max(1), n.extra.last_signals.clone()))
        .unwrap_or_default();

    ui.vertical(|ui| {
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

                const MAX_VS_TRAIL: usize = 2000;
                let skip = history.len().saturating_sub(MAX_VS_TRAIL);
                let trail: Vec<_> = history.iter().skip(skip).collect();
                let nt = trail.len();
                for ch in 0..n_channels {
                    let col = MULTI_COLORS[ch % MULTI_COLORS.len()];
                    let xi = ch * 2;
                    let yi = ch * 2 + 1;
                    // Trail
                    for (idx, sample) in trail.iter().enumerate() {
                        let (Some(x), Some(y)) = (
                            sample.get(xi).copied().flatten(),
                            sample.get(yi).copied().flatten(),
                        ) else { continue; };
                        let px = rect.center().x + x.clamp(-1.0, 1.0) * rect.width()  * 0.45;
                        let py = rect.center().y - y.clamp(-1.0, 1.0) * rect.height() * 0.45;
                        let alpha = ((idx as f32 / nt as f32) * 200.0) as u8 + 35;
                        painter.circle_filled(egui::pos2(px, py), 1.5,
                            Color32::from_rgba_unmultiplied(col.r(), col.g(), col.b(), alpha));
                    }
                    // Current-position dot
                    if let Some(Some(Signal::Vec2(v))) = last_signals.get(ch) {
                        let px = rect.center().x + v.x.clamp(-1.0, 1.0) * rect.width()  * 0.45;
                        let py = rect.center().y - v.y.clamp(-1.0, 1.0) * rect.height() * 0.45;
                        painter.circle_filled(egui::pos2(px, py), 4.0, col);
                        painter.circle_stroke(egui::pos2(px, py), 4.0,
                            egui::Stroke::new(1.0, Color32::from_gray(100)));
                    }
                }
            });

        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("Ch").small().weak());
            if ui.small_button("+").on_hover_text("Add channel").clicked() {
                if let Some(node) = snarl.get_node_mut(node_id) {
                    let next = node.inputs.len() + 1;
                    node.inputs.push(PinDescriptor::new(format!("ch{}", next), SignalType::Vec2));
                }
            }
            if n_channels > 1 && ui.small_button("−").on_hover_text("Remove channel").clicked() {
                remove_input_pin(node_id, n_channels - 1, inputs, snarl);
            }
        });
    });
}

// ── Processing module body renderers ──────────────────────────────────────────

fn show_delay_body(node_id: NodeId, inputs: &[InPin], outputs: &[OutPin], ui: &mut egui::Ui, snarl: &mut Snarl<NodeData>) {
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
    let n_channels = snarl.get_node(node_id).map(|n| n.inputs.len()).unwrap_or(1).max(1);
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new("Ch").small().weak());
        if ui.small_button("+").on_hover_text("Add channel").clicked() {
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
}

fn show_average_body(node_id: NodeId, inputs: &[InPin], outputs: &[OutPin], ui: &mut egui::Ui, snarl: &mut Snarl<NodeData>) {
    let (buf_size, spike_mad) = snarl
        .get_node(node_id)
        .map(|n| {
            let bs = n.params.get("buf_size").and_then(|v| v.as_f64()).unwrap_or(10.0) as f32;
            let sm = n.params.get("spike_mad").and_then(|v| v.as_f64()).unwrap_or(0.0) as f32;
            (bs, sm)
        })
        .unwrap_or((10.0, 0.0));

    let mut bs = buf_size;
    let mut sm = spike_mad;
    let mut changed = false;

    egui::Grid::new(("avg_grid", node_id)).num_columns(2).show(ui, |ui| {
        ui.label(egui::RichText::new("Samples").small());
        changed |= ui.add(egui::DragValue::new(&mut bs).speed(1.0).range(1.0..=10_000.0)).changed();
        ui.end_row();
        ui.label(egui::RichText::new("Spike MAD").small())
            .on_hover_text("Outlier threshold in median absolute deviations. 0 = off. Try 3.0 to start.");
        changed |= ui.add(egui::DragValue::new(&mut sm).speed(0.1).range(0.0..=20.0).max_decimals(1)).changed();
        ui.end_row();
    });

    if changed {
        if let Some(node) = snarl.get_node_mut(node_id) {
            if let Some(n) = Number::from_f64(bs as f64) { node.params.insert("buf_size".into(),  Value::Number(n)); }
            if let Some(n) = Number::from_f64(sm as f64) { node.params.insert("spike_mad".into(), Value::Number(n)); }
        }
    }

    let n_channels = snarl.get_node(node_id).map(|n| n.inputs.len()).unwrap_or(1).max(1);
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new("Ch").small().weak());
        if ui.small_button("+").on_hover_text("Add channel").clicked() {
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
}

fn show_dc_filter_body(node_id: NodeId, inputs: &[InPin], outputs: &[OutPin], ui: &mut egui::Ui, snarl: &mut Snarl<NodeData>) {
    let (window_ms, decay_ms) = snarl
        .get_node(node_id)
        .map(|n| {
            let w = n.params.get("window_ms").and_then(|v| v.as_f64()).unwrap_or(500.0) as f32;
            let d = n.params.get("decay_ms").and_then(|v| v.as_f64()).unwrap_or(200.0) as f32;
            (w, d)
        })
        .unwrap_or((500.0, 200.0));

    let mut w = window_ms;
    let mut d = decay_ms;
    let mut changed = false;

    egui::Grid::new(("dcf_grid", node_id)).num_columns(2).show(ui, |ui| {
        ui.label(egui::RichText::new("Window ms").small());
        changed |= ui.add(egui::DragValue::new(&mut w).speed(10.0).range(10.0..=60_000.0)).changed();
        ui.end_row();
        ui.label(egui::RichText::new("Decay ms").small());
        changed |= ui.add(egui::DragValue::new(&mut d).speed(10.0).range(10.0..=60_000.0)).changed();
        ui.end_row();
    });

    if changed {
        if let Some(node) = snarl.get_node_mut(node_id) {
            if let Some(n) = Number::from_f64(w as f64) { node.params.insert("window_ms".into(), Value::Number(n)); }
            if let Some(n) = Number::from_f64(d as f64) { node.params.insert("decay_ms".into(),  Value::Number(n)); }
        }
    }

    let n_channels = snarl.get_node(node_id).map(|n| n.inputs.len()).unwrap_or(1).max(1);
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new("Ch").small().weak());
        if ui.small_button("+").on_hover_text("Add channel").clicked() {
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
            node.params.insert("scale_t".into(),  serde_json::json!(0.0f64));
            node.params.insert("trail_ms".into(), serde_json::json!(300i64));
        }
    }

    // ── Read params ───────────────────────────────────────────────────────────
    let (points, biases, absolute, in_min, in_max, out_min, out_max, grid_x, grid_y, snap, scale_t, trail_ms) = snarl
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
            let sc   = n.params.get("scale_t").and_then(|v| v.as_f64()).map(|f| f as f32)
                .unwrap_or_else(|| match n.params.get("in_scale").and_then(|v| v.as_i64()).unwrap_or(0) {
                    1 => -0.5, 2 => 0.5, _ => 0.0,
                });
            let tm   = n.params.get("trail_ms").and_then(|v| v.as_i64()).unwrap_or(300).clamp(0, 1000);
            (pts, bss, abs, i0, i1, o0, o1, gx, gy, sn, sc, tm)
        })
        .unwrap_or_else(|| (vec![[0.0, 0.0], [1.0, 1.0]], vec![], true, -1.0, 1.0, -1.0, 1.0, 4, 4, false, 0.0f32, 300));

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
                        curve_scale((raw.abs() / abs_max).clamp(0.0, 1.0), scale_t)
                    } else {
                        let in_range = (in_max - in_min).abs().max(f32::EPSILON);
                        let norm     = ((raw - in_min) / in_range * 2.0 - 1.0).clamp(-1.0, 1.0);
                        let sign     = if norm < 0.0 { -1.0f32 } else { 1.0 };
                        sign * curve_scale(norm.abs(), scale_t)
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
        let mut sc_t     = scale_t;
        let mut tm       = trail_ms;
        let mut changed  = false;

        // Row 1: Scale slider (Log←──●──→Exp, double-click resets) + Absolute + Snap
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("Log").small().weak());
            let (slider_rect, slider_resp) = ui.allocate_exact_size(
                egui::vec2(80.0, 14.0), egui::Sense::click_and_drag(),
            );
            if slider_resp.double_clicked() {
                sc_t = 0.0;
                changed = true;
            } else if slider_resp.dragged() {
                sc_t = (sc_t + slider_resp.drag_delta().x / slider_rect.width() * 2.0).clamp(-1.0, 1.0);
                changed = true;
            }
            let painter = ui.painter_at(slider_rect);
            painter.rect_filled(slider_rect, 3.0, Color32::from_gray(35));
            let cx = slider_rect.center().x;
            painter.line_segment(
                [egui::pos2(cx, slider_rect.top() + 2.0), egui::pos2(cx, slider_rect.bottom() - 2.0)],
                egui::Stroke::new(1.0, Color32::from_gray(70)),
            );
            let knob_x = slider_rect.left() + (sc_t + 1.0) * 0.5 * slider_rect.width();
            painter.circle_filled(
                egui::pos2(knob_x, slider_rect.center().y), 5.0,
                if slider_resp.hovered() || slider_resp.dragged() { Color32::WHITE } else { Color32::from_gray(190) },
            );
            ui.label(egui::RichText::new("Exp").small().weak());
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
                if let Some(n) = Number::from_f64(sc_t as f64) { node.params.insert("scale_t".into(), Value::Number(n)); }
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

fn show_vec_response_curve_body(node_id: NodeId, inputs: &[InPin], outputs: &[OutPin], ui: &mut egui::Ui, snarl: &mut Snarl<NodeData>) {
    // ── Initialise params on first use ────────────────────────────────────────
    let needs_init = snarl.get_node(node_id).map(|n| !n.params.contains_key("points")).unwrap_or(false);
    if needs_init {
        if let Some(node) = snarl.get_node_mut(node_id) {
            node.params.insert("points".into(),   serde_json::json!([[0.0, 0.0], [1.0, 1.0]]));
            node.params.insert("biases".into(),   serde_json::json!([0.0]));
            node.params.insert("in_max".into(),   serde_json::json!(1.0f64));
            node.params.insert("out_max".into(),  serde_json::json!(1.0f64));
            node.params.insert("grid_x".into(),   serde_json::json!(4i64));
            node.params.insert("grid_y".into(),   serde_json::json!(4i64));
            node.params.insert("snap".into(),     Value::Bool(false));
            node.params.insert("scale_t".into(),  serde_json::json!(0.0f64));
            node.params.insert("trail_ms".into(), serde_json::json!(300i64));
        }
    }

    // ── Read params ───────────────────────────────────────────────────────────
    let (points, biases, in_max, out_max, grid_x, grid_y, snap, scale_t, trail_ms) = snarl
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
            let i1  = n.params.get("in_max") .and_then(|v| v.as_f64()).unwrap_or(1.0) as f32;
            let o1  = n.params.get("out_max").and_then(|v| v.as_f64()).unwrap_or(1.0) as f32;
            let gx  = n.params.get("grid_x").and_then(|v| v.as_i64()).unwrap_or(4).max(1) as usize;
            let gy  = n.params.get("grid_y").and_then(|v| v.as_i64()).unwrap_or(4).max(1) as usize;
            let sn  = n.params.get("snap").and_then(|v| v.as_bool()).unwrap_or(false);
            let sc  = n.params.get("scale_t").and_then(|v| v.as_f64()).map(|f| f as f32).unwrap_or(0.0);
            let tm  = n.params.get("trail_ms").and_then(|v| v.as_i64()).unwrap_or(300).clamp(0, 1000);
            (pts, bss, i1, o1, gx, gy, sn, sc, tm)
        })
        .unwrap_or_else(|| (vec![[0.0, 0.0], [1.0, 1.0]], vec![], 1.0, 1.0, 4, 4, false, 0.0f32, 300));

    let n_channels = snarl.get_node(node_id)
        .map(|n| n.inputs.len().min(n.outputs.len()))
        .unwrap_or(1).max(1);
    // sig_f32 returns v.length() for Vec2, giving deflection magnitude
    let live_inputs: Vec<Option<f32>> = (0..n_channels)
        .map(|ch| snarl.get_node(node_id)
            .and_then(|n| n.extra.last_signals.get(ch)?.as_ref())
            .map(sig_f32))
        .collect();

    // Vec curve always operates in [0,1] × [0,1] (magnitude space)
    let (x_lo, x_hi) = (0.0f32, 1.0f32);
    let (y_lo, y_hi) = (0.0f32, 1.0f32);
    let x_range = x_hi - x_lo;
    let y_range = y_hi - y_lo;

    let mut new_points  = points.clone();
    let mut new_biases  = biases.clone();
    let mut pts_changed  = false;
    let mut bias_changed = false;

    ui.vertical(|ui| {
        // ── Graph ─────────────────────────────────────────────────────────────
        egui::Resize::default()
            .id_salt(("vcrv", node_id))
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
                        let hid   = ui.id().with(("vbias_h", node_id, seg));
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
                        } else { Color32::from_rgb(180, 140, 20) };
                        painter.circle_filled(hpos, 4.0, hcol);
                        painter.circle_stroke(hpos, 4.0,
                            egui::Stroke::new(1.0, Color32::from_gray(100)));
                    }
                }

                let mut remove_idx: Option<usize> = None;
                for i in 0..new_points.len() {
                    let [px, py] = new_points[i];
                    let screen   = c2s(px, py);
                    let pt_id    = ui.id().with(("vcpt", node_id, i));
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

                // Live-position trails (magnitude of Vec2 input → position on curve)
                let abs_max   = in_max.abs().max(f32::EPSILON);
                let trail_dur = std::time::Duration::from_millis(trail_ms as u64);
                let now       = std::time::Instant::now();
                let mut has_active = false;
                for (ch, raw_opt) in live_inputs.iter().enumerate() {
                    let Some(raw) = raw_opt else { continue; };
                    has_active = true;
                    let graph_x = curve_scale((raw.abs() / abs_max).clamp(0.0, 1.0), scale_t);
                    type Trail = std::collections::VecDeque<(f32, std::time::Instant)>;
                    let trail_id = ui.id().with(("vtrail", node_id, ch as u32));
                    let mut trail: Trail = ui.data(|d| d.get_temp::<Trail>(trail_id).clone().unwrap_or_default());
                    if trail_ms > 0 {
                        trail.push_back((graph_x, now));
                        while trail.front().map(|&(_, t)| now.duration_since(t) > trail_dur).unwrap_or(false) {
                            trail.pop_front();
                        }
                    } else { trail.clear(); }
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
                if has_active { ui.ctx().request_repaint(); }
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
        let mut i1      = in_max;
        let mut o1      = out_max;
        let mut gx_f    = grid_x as f64;
        let mut gy_f    = grid_y as f64;
        let mut snap_on = snap;
        let mut sc_t    = scale_t;
        let mut tm      = trail_ms;
        let mut changed = false;

        // Row 1: Scale slider (Log←──●──→Exp, double-click resets) + Snap
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("Log").small().weak());
            let (slider_rect, slider_resp) = ui.allocate_exact_size(
                egui::vec2(80.0, 14.0), egui::Sense::click_and_drag(),
            );
            if slider_resp.double_clicked() {
                sc_t = 0.0;
                changed = true;
            } else if slider_resp.dragged() {
                sc_t = (sc_t + slider_resp.drag_delta().x / slider_rect.width() * 2.0).clamp(-1.0, 1.0);
                changed = true;
            }
            let painter = ui.painter_at(slider_rect);
            painter.rect_filled(slider_rect, 3.0, Color32::from_gray(35));
            let cx = slider_rect.center().x;
            painter.line_segment(
                [egui::pos2(cx, slider_rect.top() + 2.0), egui::pos2(cx, slider_rect.bottom() - 2.0)],
                egui::Stroke::new(1.0, Color32::from_gray(70)),
            );
            let knob_x = slider_rect.left() + (sc_t + 1.0) * 0.5 * slider_rect.width();
            painter.circle_filled(
                egui::pos2(knob_x, slider_rect.center().y), 5.0,
                if slider_resp.hovered() || slider_resp.dragged() { Color32::WHITE } else { Color32::from_gray(190) },
            );
            ui.label(egui::RichText::new("Exp").small().weak());
            ui.separator();
            let snap_before = snap_on;
            ui.checkbox(&mut snap_on, egui::RichText::new("Snap").small());
            changed |= snap_on != snap_before;
        });

        // Row 2: In/Out max
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("In max").small().weak());
            changed |= ui.add(egui::DragValue::new(&mut i1).speed(0.01).max_decimals(2)).changed();
            ui.separator();
            ui.label(egui::RichText::new("Out max").small().weak());
            changed |= ui.add(egui::DragValue::new(&mut o1).speed(0.01).max_decimals(2)).changed();
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
                if let Some(n) = Number::from_f64(i1 as f64)  { node.params.insert("in_max".into(),  Value::Number(n)); }
                if let Some(n) = Number::from_f64(o1 as f64)  { node.params.insert("out_max".into(), Value::Number(n)); }
                if let Some(n) = Number::from_f64(sc_t as f64) { node.params.insert("scale_t".into(), Value::Number(n)); }
                node.params.insert("grid_x".into(),   serde_json::json!(gx_f as i64));
                node.params.insert("grid_y".into(),   serde_json::json!(gy_f as i64));
                node.params.insert("snap".into(),     Value::Bool(snap_on));
                node.params.insert("trail_ms".into(), serde_json::json!(tm));
            }
        }

        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("Ch").small().weak());
            if ui.small_button("+").on_hover_text("Add Vec2 channel").clicked() {
                if let Some(node) = snarl.get_node_mut(node_id) {
                    let next = node.inputs.len() + 1;
                    node.inputs.push(PinDescriptor::new(format!("In {}", next), SignalType::Vec2));
                    node.outputs.push(PinDescriptor::new(format!("Out {}", next), SignalType::Vec2));
                }
            }
            if n_channels > 1 && ui.small_button("−").on_hover_text("Remove last channel").clicked() {
                remove_input_pin(node_id, n_channels - 1, inputs, snarl);
                remove_output_pin(node_id, n_channels - 1, outputs, snarl);
            }
        });
    });
}

/// Maps x ∈ [0,1] → [0,1] continuously. t=0 → linear; t<0 → log-like; t>0 → exp-like.
/// Power law p = 2^(t*3): at t=±1, p=8 or 1/8 — far more extreme than the old log/exp modes.
fn curve_scale(x: f32, t: f32) -> f32 {
    if t.abs() < 1e-4 { return x; }
    x.clamp(0.0, 1.0).powf(2.0f32.powf(t * 3.0))
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
