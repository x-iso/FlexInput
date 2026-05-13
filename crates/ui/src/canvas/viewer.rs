use egui::Color32;
use egui_snarl::{
    ui::{AnyPins, NodeLayout, PinInfo, SnarlViewer},
    InPin, InPinId, NodeId, OutPin, OutPinId, Snarl,
};
use flexinput_core::{ModuleDescriptor, PinDescriptor, Signal, SignalType, automap as am_canon};
use flexinput_devices::{ControllerKind, PhysicalDevice, midi::cc_display_name};
use flexinput_engine::SAMPLE_RATE;
use serde_json::{Number, Value};

use super::{curve::sample_curve, node::{ExposedModule, NodeData}};

pub struct FlexViewer<'a> {
    pub descriptors: &'a [ModuleDescriptor],
    pub ctx: egui::Context,
    /// IDs of currently-live physical and virtual devices.  Used to render status dots.
    pub live_device_ids: &'a std::collections::HashSet<String>,
    /// Physical devices available as hot-swap candidates in the node context menu.
    pub physical_devices: &'a [PhysicalDevice],
    /// Set by the `disconnect` override when the user right-clicks a wire.
    /// Canvas::show() reads this after snarl.show() and renders the context menu.
    pub pending_wire_menu: Option<(OutPinId, InPinId, egui::Pos2)>,
    /// Set by show_node_menu when the user clicks "Rename…".
    pub rename_request: Option<NodeId>,
    /// Set by show_node_menu when the user picks "Replace with…".
    /// Carries (node id, index into physical_devices).
    pub replace_request: Option<(NodeId, usize)>,
    /// Set when the user clicks "Edit…" on a subpatch node.
    pub edit_subpatch_request: Option<NodeId>,
    /// True when rendering the inner canvas of a sub-patch editor.
    /// When false, the "SubPatch" module category (Inlet/Outlet) is hidden.
    pub is_inner_canvas: bool,
    /// Set when the user picks something from the "Pin element" submenu (or
    /// "Unpin"). (NodeId, element_id, source_size). `source_size = [0,0]`
    /// means "no measured size" — placement uses a default.
    pub expose_module_request: Option<(NodeId, String, [f32; 2])>,
    /// NodeId.0 values currently pinned to the outer body (used to toggle menu label).
    pub pinned_inner_ids: std::collections::HashSet<usize>,
    /// Set by show_node_menu when the user clicks "Group into sub-patch…".
    /// Canvas::show() reads this, looks up the snarl selection, and calls group_selected_into_subpatch.
    pub group_request: bool,
}

impl<'a> SnarlViewer<NodeData> for FlexViewer<'a> {
    fn title(&mut self, node: &NodeData) -> String {
        node.display_name.clone()
    }

    fn node_layout(
        &mut self,
        default: NodeLayout,
        node_id: NodeId,
        _inputs: &[InPin],
        _outputs: &[OutPin],
        snarl: &Snarl<NodeData>,
    ) -> NodeLayout {
        if snarl.get_node(node_id).map(|n| n.module_id == "module.knob").unwrap_or(false) {
            NodeLayout::flipped_sandwich()
        } else {
            default
        }
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

        let is_subpatch = snarl.get_node(node).map(|n| n.module_id == "subpatch").unwrap_or(false);
        let (inner_count, has_pinned, is_unlocked) = if is_subpatch {
            let sp = snarl.get_node(node).and_then(|n| n.subpatch.as_ref());
            (
                sp.map(|s| s.snarl.nodes_ids_data().count()).unwrap_or(0),
                sp.map(|s| !s.exposed_modules.is_empty()).unwrap_or(false),
                snarl.get_node(node).map(|n| n.extra.layout_unlocked).unwrap_or(false),
            )
        } else {
            (0, false, false)
        };
        let is_label = snarl.get_node(node).map(|n| n.module_id == "module.label").unwrap_or(false);
        let is_svg   = snarl.get_node(node).map(|n| n.module_id == "module.svg").unwrap_or(false);

        ui.vertical(|ui| {
            ui.horizontal(|ui| {
                if let Some(live) = status_dot {
                    let color = if live { Color32::from_rgb(80, 200, 100) } else { Color32::from_rgb(220, 80, 60) };
                    ui.label(egui::RichText::new("●").color(color).small());
                }
                ui.label(&title);

                if is_subpatch {
                    let _ = has_pinned;
                    let edit_tooltip = format!("{} module{} inside", inner_count, if inner_count == 1 { "" } else { "s" });
                    if ui.small_button("Edit…").on_hover_text(edit_tooltip).clicked() {
                        self.edit_subpatch_request = Some(node);
                    }
                    let (lbl, tip) = if is_unlocked {
                        ("Lock", "Lock element positions and restore interactivity")
                    } else {
                        ("Layout", "Click highlighted elements in the editor to pin; drag/resize on the body")
                    };
                    if ui.small_button(lbl).on_hover_text(tip).clicked() {
                        if let Some(n) = snarl.get_node_mut(node) {
                            n.extra.layout_unlocked = !is_unlocked;
                        }
                    }
                }

                // SVG module: Load… / Clear / tint picker live in the header.
                // The body area is reserved for the rendered image only.
                if is_svg {
                    if ui.small_button("Load…")
                        .on_hover_text("Load an .svg file (the source is embedded in the patch)")
                        .clicked()
                    {
                        if let Some(path) = rfd::FileDialog::new()
                            .add_filter("SVG", &["svg"])
                            .pick_file()
                        {
                            if let Ok(text) = std::fs::read_to_string(&path) {
                                if let Some(n) = snarl.get_node_mut(node) {
                                    n.params.insert("svg_data".into(), Value::String(text));
                                    // New payload → bump cache key so the loader re-decodes.
                                    let prev = n.params.get("svg_rev").and_then(|v| v.as_u64()).unwrap_or(0);
                                    n.params.insert("svg_rev".into(), serde_json::json!(prev + 1));
                                }
                            }
                        }
                    }
                    let has_data = snarl.get_node(node)
                        .and_then(|n| n.params.get("svg_data").and_then(|v| v.as_str()))
                        .map(|s| !s.is_empty())
                        .unwrap_or(false);
                    if has_data && ui.small_button("Clear").on_hover_text("Remove the loaded SVG").clicked() {
                        if let Some(n) = snarl.get_node_mut(node) {
                            n.params.remove("svg_data");
                            let prev = n.params.get("svg_rev").and_then(|v| v.as_u64()).unwrap_or(0);
                            n.params.insert("svg_rev".into(), serde_json::json!(prev + 1));
                        }
                    }
                    let mut tint = snarl.get_node(node)
                        .map(read_svg_tint)
                        .unwrap_or(egui::Color32::TRANSPARENT);
                    let mut mode = snarl.get_node(node)
                        .and_then(|n| n.params.get("color_mode").and_then(|v| v.as_str()).map(|s| s.to_string()))
                        .unwrap_or_else(|| "override".to_string());

                    // Custom color picker: opens a popup with the standard
                    // egui color square + RGBA sliders, but no Additive toggle
                    // (we provide our own Normal / Additive selector right
                    // next to the swatch since their meaning is application-
                    // specific here, not egui's premultiplied-alpha intent).
                    let id = ui.id().with(("svg_color_popup", node.0));
                    let swatch_size = egui::vec2(24.0, 16.0);
                    let (rect, resp) = ui.allocate_exact_size(swatch_size, egui::Sense::click());
                    let painter = ui.painter_at(rect);
                    // Checkerboard background so transparent picks read clearly.
                    let cb = 4.0_f32;
                    for ix in 0..((swatch_size.x / cb).ceil() as i32) {
                        for iy in 0..((swatch_size.y / cb).ceil() as i32) {
                            let dark = (ix + iy) % 2 == 0;
                            let r = egui::Rect::from_min_size(
                                egui::pos2(rect.left() + ix as f32 * cb, rect.top() + iy as f32 * cb),
                                egui::vec2(cb, cb),
                            ).intersect(rect);
                            painter.rect_filled(r, 0.0,
                                if dark { egui::Color32::from_gray(60) } else { egui::Color32::from_gray(110) });
                        }
                    }
                    painter.rect_filled(rect, 2.0, tint);
                    painter.rect_stroke(rect, 2.0,
                        egui::Stroke::new(1.0, egui::Color32::from_gray(80)),
                        egui::StrokeKind::Inside);
                    if resp.clicked() {
                        ui.memory_mut(|m| m.toggle_popup(id));
                    }
                    let mut changed = false;
                    egui::popup_below_widget(ui, id, &resp,
                        egui::PopupCloseBehavior::CloseOnClickOutside,
                        |ui| {
                            ui.set_min_width(220.0);
                            ui.label(egui::RichText::new("Mode").small().weak());
                            ui.horizontal(|ui| {
                                changed |= ui.selectable_value(&mut mode, "override".into(),
                                    egui::RichText::new("Override")).on_hover_text(
                                    "Replace SVG colors with this color; alpha = blend amount toward original").changed();
                                changed |= ui.selectable_value(&mut mode, "additive".into(),
                                    egui::RichText::new("Additive")).on_hover_text(
                                    "Add this color on top of original SVG; alpha = added intensity").changed();
                            });
                            ui.separator();
                            if egui::widgets::color_picker::color_picker_color32(
                                ui, &mut tint, egui::widgets::color_picker::Alpha::OnlyBlend,
                            ) {
                                changed = true;
                            }
                        });
                    if changed {
                        if let Some(n) = snarl.get_node_mut(node) {
                            n.params.insert("tint".into(), serde_json::json!([tint.r() as u64, tint.g() as u64, tint.b() as u64, tint.a() as u64]));
                            n.params.insert("color_mode".into(), Value::String(mode));
                        }
                    }
                }

                // Label module: font size + text color in the header so they
                // don't compete with the text edit area.
                if is_label {
                    let (mut size, mut col) = snarl.get_node(node).map(|n| {
                        let sz = n.params.get("font_size").and_then(|v| v.as_f64()).unwrap_or(14.0) as f32;
                        let arr = n.params.get("color").and_then(|v| v.as_array()).cloned();
                        let c = match arr.as_deref() {
                            Some([r, g, b, a]) => egui::Color32::from_rgba_unmultiplied(
                                r.as_u64().unwrap_or(220) as u8,
                                g.as_u64().unwrap_or(220) as u8,
                                b.as_u64().unwrap_or(220) as u8,
                                a.as_u64().unwrap_or(255) as u8,
                            ),
                            _ => egui::Color32::from_rgb(220, 220, 220),
                        };
                        (sz, c)
                    }).unwrap_or((14.0, egui::Color32::from_rgb(220, 220, 220)));
                    let mut changed = false;
                    let prev_size = size;
                    let prev_col  = col;
                    if ui.add(egui::DragValue::new(&mut size).speed(0.25).range(8.0..=72.0).suffix("px"))
                        .on_hover_text("Font size").changed()
                    {
                        changed = true;
                    }
                    if ui.color_edit_button_srgba(&mut col).on_hover_text("Text color").changed() {
                        changed = true;
                    }
                    let _ = prev_size;
                    let _ = prev_col;
                    if changed {
                        if let Some(n) = snarl.get_node_mut(node) {
                            if let Some(num) = Number::from_f64(size as f64) {
                                n.params.insert("font_size".into(), Value::Number(num));
                            }
                            let arr = serde_json::json!([col.r() as u64, col.g() as u64, col.b() as u64, col.a() as u64]);
                            n.params.insert("color".into(), arr);
                        }
                    }
                }
            });

            // Second header row — only visible while in Layout mode for this
            // sub-patch. Snap settings live on the sub-patch itself (they
            // belong to its body's drag/resize behavior, not to the editor).
            if is_subpatch && is_unlocked {
                let (mut snap_enabled, mut snap_grid_px) = snarl.get_node(node)
                    .and_then(|n| n.subpatch.as_ref())
                    .map(|sp| (sp.snap_enabled, sp.snap_grid_px))
                    .unwrap_or((false, 8));
                let mut changed = false;
                ui.horizontal(|ui| {
                    let was = snap_enabled;
                    ui.checkbox(&mut snap_enabled, egui::RichText::new("Snap").small())
                        .on_hover_text("Snap pinned-element positions and sizes to a grid in Layout mode");
                    if snap_enabled != was { changed = true; }
                    ui.add_enabled_ui(snap_enabled, |ui| {
                        ui.label(egui::RichText::new("grid").small().weak());
                        let mut g = snap_grid_px as i32;
                        if ui.add(egui::DragValue::new(&mut g)
                            .speed(0.5)
                            .range(2i32..=64)
                            .suffix("px"))
                            .on_hover_text("Grid step in pixels (rounded to multiples of 2)")
                            .changed()
                        {
                            let g2 = ((g.max(2)) / 2 * 2) as u32;
                            if g2 != snap_grid_px { snap_grid_px = g2; changed = true; }
                        }
                    });
                });
                if changed {
                    if let Some(sp) = snarl.get_node_mut(node).and_then(|n| n.subpatch.as_mut()) {
                        sp.snap_enabled = snap_enabled;
                        sp.snap_grid_px = snap_grid_px;
                    }
                }
            }
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
        if !to_type.accepts(from_type) { return; }
        // AutoMap input pins accept exactly ONE upstream wire. Multiple wires
        // route ambiguously (only `pin.remotes.first()` is honored by eval and
        // graph-build), so silently dropping later wires confuses users. Drop
        // the existing wire(s) so the new connection visibly replaces them.
        if to_type == SignalType::AutoMap {
            let existing: Vec<OutPinId> = snarl.in_pin(to.id).remotes.iter().copied().collect();
            for src in existing {
                snarl.disconnect(src, to.id);
            }
        }
        snarl.connect(from.id, to.id);
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
            "device.sink" | "module.constant" | "module.switch" | "module.knob" | "module.label" | "module.svg"
                | "display.readout" | "display.oscilloscope" | "display.vectorscope"
                | "module.delay" | "module.average" | "module.dc_filter" | "module.response_curve" | "module.vec_response_curve"
                | "math.add" | "math.subtract" | "math.multiply" | "math.divide"
                | "module.selector" | "module.split"
                | "logic.greater_than" | "logic.less_than" | "logic.delay" | "logic.counter"
                | "generator.oscillator" | "processing.gyro_3dof"
                | "module.automap_split" | "module.automap_collect"
                | "module.automap_fork" | "module.automap_selector"
                | "subpatch" | "subpatch.inlet" | "subpatch.outlet"
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
            "module.label"    => show_label_body(node_id, ui, snarl),
            "module.svg"      => show_svg_body(node_id, ui, snarl),
            "logic.greater_than" | "logic.less_than" => show_or_equal_body(node_id, ui, snarl),
            "logic.delay"   => show_logic_delay_body(node_id, ui, snarl),
            "logic.counter"        => show_counter_body(node_id, inputs, ui, snarl),
            "generator.oscillator"  => show_oscillator_body(node_id, inputs, ui, snarl),
            "processing.gyro_3dof"  => show_gyro_3dof_body(node_id, ui, snarl),
            "module.automap_split"     => show_automap_split_body(node_id, outputs, ui, snarl),
            "module.automap_collect"   => show_automap_collect_body(node_id, inputs, ui, snarl),
            "module.automap_fork"      => show_automap_fork_body(node_id, outputs, ui, snarl),
            "module.automap_selector"  => show_automap_selector_body(node_id, inputs, ui, snarl),
            "subpatch" => {
                if show_subpatch_body(node_id, ui, snarl) {
                    self.edit_subpatch_request = Some(node_id);
                }
            }
            "subpatch.inlet" | "subpatch.outlet" => show_inlet_outlet_body(node_id, ui, snarl),
            _ => {}
        }
    }

    // ── Node footer (below all pins) ─────────────────────────────────────────

    fn has_footer(&mut self, node: &NodeData) -> bool {
        let dev_id = node.params.get("device_id").and_then(|v| v.as_str()).unwrap_or("");
        node.module_id == "device.source" && !dev_id.starts_with("midi_in:")
    }

    fn show_footer(
        &mut self,
        node_id: NodeId,
        _inputs: &[InPin],
        _outputs: &[OutPin],
        ui: &mut egui::Ui,
        snarl: &mut Snarl<NodeData>,
    ) {
        let dz = snarl.get_node(node_id)
            .and_then(|n| n.params.get("deadzone").and_then(|v| v.as_f64()))
            .unwrap_or(0.1) as f32;
        let mut dz_edit = dz;
        ui.horizontal(|ui| {
            ui.label("Deadzone");
            ui.add(egui::Slider::new(&mut dz_edit, 0.0_f32..=0.5).fixed_decimals(2));
        });
        if (dz_edit - dz).abs() > f32::EPSILON {
            if let Some(node) = snarl.get_node_mut(node_id) {
                node.params.insert("deadzone".to_string(), serde_json::Value::from(dz_edit as f64));
            }
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
        show_module_menu(pos, ui, snarl, self.descriptors, None, self.is_inner_canvas);
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
                    show_module_menu(pos, ui, snarl, self.descriptors, Some(WireDir::FromOutput { src, from_type }), self.is_inner_canvas);
                }
            }
            AnyPins::In(in_pins) => {
                if let Some(&dst) = in_pins.first() {
                    let to_type = snarl[dst.node].inputs[dst.input].signal_type;
                    ui.label("Connect to output of…");
                    ui.separator();
                    show_module_menu(pos, ui, snarl, self.descriptors, Some(WireDir::FromInput { dst, to_type }), self.is_inner_canvas);
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

        let module_id = snarl.get_node(node).map(|n| n.module_id.as_str()).unwrap_or("").to_string();
        let display_name = snarl.get_node(node).map(|n| n.display_name.clone()).unwrap_or_default();

        // Inner canvas only — show "Unpin from body" if the node has any pins.
        // Pinning happens exclusively through Layout mode + highlight click;
        // the menu doesn't offer a "Pin to body" option anymore so the user
        // can't accidentally pin a whole-body crop.
        if self.is_inner_canvas
            && !matches!(module_id.as_str(), "subpatch.inlet" | "subpatch.outlet" | "device.source" | "device.sink")
        {
            let already = self.pinned_inner_ids.contains(&node.0);
            if already {
                if ui.button("Unpin from body").clicked() {
                    self.expose_module_request = Some((node, "default".to_string(), [0.0, 0.0]));
                    ui.close();
                }
            } else {
                ui.label(egui::RichText::new("Use Layout mode to pin elements").small().weak());
            }
        }

        // "Edit…" for sub-patch nodes.
        if module_id == "subpatch" {
            if ui.button("Edit…").clicked() {
                self.edit_subpatch_request = Some(node);
                ui.close();
            }
        }

        // "Replace with…" for physical device source/sink nodes.
        let device_id = snarl.get_node(node)
            .and_then(|n| n.params.get("device_id").and_then(|v| v.as_str()))
            .map(|s| s.to_string());

        if matches!(module_id.as_str(), "device.source" | "device.sink") {
            let is_virtual = device_id.as_deref().map(|id| id.starts_with("virtual.")).unwrap_or(false);
            if !is_virtual {
                let is_source = module_id == "device.source";
                let candidates: Vec<(usize, &str)> = self.physical_devices
                    .iter()
                    .enumerate()
                    .filter(|(_, d)| {
                        Some(d.id.as_str()) != device_id.as_deref()
                            && if is_source {
                                !d.outputs.is_empty()
                            } else {
                                // sink: MidiOut or devices that only have inputs
                                matches!(d.kind, ControllerKind::MidiOut)
                                    || (d.outputs.is_empty() && !d.inputs.is_empty())
                            }
                    })
                    .map(|(i, d)| (i, d.display_name.as_str()))
                    .collect();

                if !candidates.is_empty() {
                    ui.separator();
                    ui.menu_button("Replace with…", |ui| {
                        for (idx, name) in candidates {
                            if ui.button(name).clicked() {
                                self.replace_request = Some((node, idx));
                                ui.close();
                            }
                        }
                    });
                }
            }
        }

        ui.separator();
        if ui.button("Group into sub-patch…").clicked() {
            self.group_request = true;
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

// ── AutoMap Splitter body ─────────────────────────────────────────────────────

fn show_automap_split_body(
    node_id: NodeId,
    outputs: &[OutPin],
    ui: &mut egui::Ui,
    snarl: &mut Snarl<NodeData>,
) {
    // Current individual outputs — output_pin_ids[0] = "automap_pass" (skip it).
    let current_ids: Vec<String> = snarl.get_node(node_id)
        .and_then(|n| n.params.get("output_pin_ids").and_then(|v| v.as_array()))
        .map(|a| a.iter().skip(1).map(|v| v.as_str().unwrap_or("").to_string()).collect())
        .unwrap_or_default();

    ui.vertical(|ui| {
        ui.set_min_width(170.0);

        // Existing individual outputs with remove buttons.
        let mut to_remove: Option<usize> = None;
        for (i, pin_id) in current_ids.iter().enumerate() {
            let display = am_canon::ALL_PINS.iter()
                .find(|p| p.id == pin_id.as_str())
                .map(|p| p.display_name)
                .unwrap_or(pin_id.as_str());
            ui.horizontal(|ui| {
                if ui.small_button("×").clicked() { to_remove = Some(i + 1); }
                ui.label(egui::RichText::new(display).small());
            });
        }
        if let Some(rm_idx) = to_remove {
            remove_automap_split_output(node_id, rm_idx, outputs, snarl);
        }

        ui.add_space(4.0);

        egui::ComboBox::from_id_salt((node_id, "add_am_split"))
            .selected_text(egui::RichText::new("+ Add output").small())
            .width(150.0)
            .show_ui(ui, |ui| {
                for ap in am_canon::ALL_PINS {
                    if current_ids.iter().any(|id| id == ap.id) { continue; }
                    if ui.selectable_label(false, egui::RichText::new(ap.display_name).small()).clicked() {
                        if let Some(node) = snarl.get_node_mut(node_id) {
                            node.outputs.push(PinDescriptor::new(ap.display_name, ap.signal_type));
                            match node.params.get_mut("output_pin_ids") {
                                Some(Value::Array(ids)) => { ids.push(Value::String(ap.id.to_string())); }
                                _ => {
                                    node.params.insert("output_pin_ids".to_string(), Value::Array(vec![
                                        Value::String("automap_pass".to_string()),
                                        Value::String(ap.id.to_string()),
                                    ]));
                                }
                            }
                        }
                    }
                }
            });

        let has_unused = outputs.iter().skip(1).any(|o| o.remotes.is_empty());
        if has_unused && ui.small_button("Clear unused").clicked() {
            let to_clear: Vec<usize> = outputs.iter().enumerate().skip(1)
                .filter(|(_, o)| o.remotes.is_empty())
                .map(|(i, _)| i)
                .rev().collect();
            for rm_idx in to_clear {
                let fresh_outputs: Vec<OutPin> = (0..snarl.get_node(node_id).map_or(0, |n| n.outputs.len()))
                    .map(|i| snarl.out_pin(OutPinId { node: node_id, output: i }))
                    .collect();
                remove_automap_split_output(node_id, rm_idx, &fresh_outputs, snarl);
            }
        }
    });
}

fn remove_automap_split_output(node_id: NodeId, rm_idx: usize, outputs: &[OutPin], snarl: &mut Snarl<NodeData>) {
    // Never remove index 0 (the AutoMap passthrough output).
    if rm_idx == 0 { return; }
    let tail: Vec<Vec<egui_snarl::InPinId>> = outputs[rm_idx..]
        .iter().map(|o| o.remotes.clone()).collect();
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
        for remote in remotes { snarl.connect(new_out, remote); }
    }
}

// ── AutoMap Collector body ────────────────────────────────────────────────────

fn show_automap_collect_body(
    node_id: NodeId,
    inputs: &[InPin],
    ui: &mut egui::Ui,
    snarl: &mut Snarl<NodeData>,
) {
    // Individual input pin IDs stored in params["collect_input_pin_ids"] (parallel to inputs[1..]).
    let current_ids: Vec<String> = snarl.get_node(node_id)
        .and_then(|n| n.params.get("collect_input_pin_ids").and_then(|v| v.as_array()))
        .map(|a| a.iter().map(|v| v.as_str().unwrap_or("").to_string()).collect())
        .unwrap_or_default();

    let is_learning = snarl.get_node(node_id)
        .and_then(|n| n.params.get("learning").and_then(|v| v.as_bool()))
        .unwrap_or(false);

    ui.vertical(|ui| {
        ui.set_min_width(170.0);

        // Existing individual inputs with remove buttons.
        let mut to_remove: Option<usize> = None;
        for (i, pin_id) in current_ids.iter().enumerate() {
            // Resolve display name: canonical list first, otherwise show the raw key name
            // (learned keys use their egui Key Debug name as both id and display).
            let display = am_canon::ALL_PINS.iter()
                .find(|p| p.id == pin_id.as_str())
                .map(|p| p.display_name)
                .unwrap_or(pin_id.as_str());
            ui.horizontal(|ui| {
                if ui.small_button("×").clicked() { to_remove = Some(i + 1); }
                ui.label(egui::RichText::new(display).small());
            });
        }
        if let Some(rm_idx) = to_remove {
            remove_automap_collect_input(node_id, rm_idx, inputs, snarl);
        }

        ui.add_space(4.0);

        // ── Add-input dropdown (canonical ALL_PINS) ───────────────────────────
        egui::ComboBox::from_id_salt((node_id, "add_am_collect"))
            .selected_text(egui::RichText::new("+ Add input").small())
            .width(150.0)
            .show_ui(ui, |ui| {
                for ap in am_canon::ALL_PINS {
                    if current_ids.iter().any(|id| id == ap.id) { continue; }
                    if ui.selectable_label(false, egui::RichText::new(ap.display_name).small()).clicked() {
                        if let Some(node) = snarl.get_node_mut(node_id) {
                            node.inputs.push(PinDescriptor::new(ap.display_name, ap.signal_type));
                            match node.params.get_mut("collect_input_pin_ids") {
                                Some(Value::Array(ids)) => { ids.push(Value::String(ap.id.to_string())); }
                                _ => {
                                    node.params.insert("collect_input_pin_ids".to_string(), Value::Array(vec![
                                        Value::String(ap.id.to_string()),
                                    ]));
                                }
                            }
                        }
                    }
                }
            });

        // ── Learn-key (capture next keypress; works for any key egui knows) ──
        if is_learning {
            ui.label(egui::RichText::new("Press a key… (Esc cancels)").italics().small());

            let key_pressed = ui.input(|i| {
                i.events.iter().find_map(|e| {
                    if let egui::Event::Key { key, pressed: true, .. } = e {
                        Some(*key)
                    } else { None }
                })
            });

            if let Some(key) = key_pressed {
                if let Some(node) = snarl.get_node_mut(node_id) {
                    node.params.insert("learning".to_string(), Value::Bool(false));
                }
                if key != egui::Key::Escape {
                    let pin_name = format!("{key:?}");
                    let already_has = current_ids.iter().any(|id| id == &pin_name);
                    if !already_has {
                        if let Some(node) = snarl.get_node_mut(node_id) {
                            // Bool — every key is a digital signal.  Pin id == display name == egui Key Debug.
                            node.inputs.push(PinDescriptor::new(&pin_name, SignalType::Bool));
                            match node.params.get_mut("collect_input_pin_ids") {
                                Some(Value::Array(ids)) => { ids.push(Value::String(pin_name)); }
                                _ => {
                                    node.params.insert("collect_input_pin_ids".to_string(),
                                        Value::Array(vec![Value::String(pin_name)]));
                                }
                            }
                        }
                    }
                }
            }
        } else if ui.small_button("+ Learn key").clicked() {
            if let Some(node) = snarl.get_node_mut(node_id) {
                node.params.insert("learning".to_string(), Value::Bool(true));
            }
        }

        let has_unwired = inputs.iter().skip(1).any(|p| p.remotes.is_empty());
        if has_unwired && ui.small_button("Clear unused").clicked() {
            let to_clear: Vec<usize> = inputs.iter().enumerate().skip(1)
                .filter(|(_, p)| p.remotes.is_empty())
                .map(|(i, _)| i)
                .rev().collect();
            for rm_idx in to_clear {
                let fresh_inputs: Vec<InPin> = (0..snarl.get_node(node_id).map_or(0, |n| n.inputs.len()))
                    .map(|i| snarl.in_pin(InPinId { node: node_id, input: i }))
                    .collect();
                remove_automap_collect_input(node_id, rm_idx, &fresh_inputs, snarl);
            }
        }
    });
}

fn remove_automap_collect_input(node_id: NodeId, rm_idx: usize, inputs: &[InPin], snarl: &mut Snarl<NodeData>) {
    if rm_idx == 0 { return; } // Never remove the AutoMap passthrough input.
    let tail: Vec<Vec<OutPinId>> = inputs[rm_idx..]
        .iter().map(|p| p.remotes.clone()).collect();
    for i in 0..tail.len() {
        snarl.drop_inputs(InPinId { node: node_id, input: rm_idx + i });
    }
    if let Some(node) = snarl.get_node_mut(node_id) {
        node.inputs.remove(rm_idx);
        // Keep collect_input_pin_ids in sync (index rm_idx-1 because it doesn't include input[0]).
        if let Some(Value::Array(ids)) = node.params.get_mut("collect_input_pin_ids") {
            let id_idx = rm_idx - 1;
            if id_idx < ids.len() { ids.remove(id_idx); }
        }
    }
    for (shift, remotes) in tail.into_iter().enumerate().skip(1) {
        let new_in = InPinId { node: node_id, input: rm_idx + shift - 1 };
        for remote in remotes { snarl.connect(remote, new_in); }
    }
}

// ── AutoMap Fork body ─────────────────────────────────────────────────────────

fn show_automap_fork_body(
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
                node.outputs.push(PinDescriptor::new(format!("out_{next}"), SignalType::AutoMap));
            }
        }
    });
}

// ── AutoMap Selector body ─────────────────────────────────────────────────────

fn show_automap_selector_body(
    node_id: NodeId,
    inputs: &[InPin],
    ui: &mut egui::Ui,
    snarl: &mut Snarl<NodeData>,
) {
    // inputs[0] = select (fixed); inputs[1..] = in_0, in_1, ... (dynamic AutoMap)
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
                node.inputs.push(PinDescriptor::new(format!("in_{next}"), SignalType::AutoMap));
            }
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
                node.inputs.push(PinDescriptor::new(name, SignalType::Any));
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
                node.inputs.push(PinDescriptor::new(format!("in_{next}"), SignalType::Any));
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
                node.outputs.push(PinDescriptor::new(format!("out_{next}"), SignalType::Any));
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
    let resp = ui.add(egui::DragValue::new(&mut v).speed(0.01));
    if resp.changed() {
        if let Some(node) = snarl.get_node_mut(node_id) {
            if let Some(n) = Number::from_f64(v as f64) {
                node.params.insert("value".to_string(), Value::Number(n));
            }
        }
    }
    register_exposable_element(ui, node_id, "value", resp.rect);
}

fn show_switch_body(node_id: NodeId, ui: &mut egui::Ui, snarl: &mut Snarl<NodeData>) {
    let active = snarl
        .get_node(node_id)
        .and_then(|n| n.params.get("active").and_then(|v| v.as_bool()))
        .unwrap_or(false);
    let mut a = active;
    let label = if a { "ON" } else { "OFF" };
    let resp = ui.toggle_value(&mut a, label);
    if resp.changed() {
        if let Some(node) = snarl.get_node_mut(node_id) {
            node.params.insert("active".to_string(), Value::Bool(a));
        }
    }
    register_exposable_element(ui, node_id, "toggle", resp.rect);
}

/// Body for the Text/Label module: editable multiline text + font-size slider.
/// No I/O; purely visual annotation. Persists `text` and `font_size` in params.
fn show_label_body(node_id: NodeId, ui: &mut egui::Ui, snarl: &mut Snarl<NodeData>) {
    let outer = ui.allocate_ui(egui::vec2(180.0, 0.0), |ui| {
        show_label_body_sized(node_id, ui, snarl, 160.0, 0.0);
    });
    register_exposable_element(ui, node_id, "text", outer.response.rect);
}

/// Same as `show_label_body` but with explicit width / height for use when the
/// label is pinned to a sub-patch body and the user has resized its container.
/// Pass `height = 0.0` to let the text edit auto-size vertically.
fn show_label_body_sized(
    node_id: NodeId,
    ui: &mut egui::Ui,
    snarl: &mut Snarl<NodeData>,
    width: f32,
    height: f32,
) {
    let (mut text, font_size, col) = snarl.get_node(node_id).map(|n| {
        let t = n.params.get("text").and_then(|v| v.as_str()).unwrap_or("Label").to_string();
        let f = n.params.get("font_size").and_then(|v| v.as_f64()).unwrap_or(14.0) as f32;
        let c = read_label_color(n);
        (t, f, c)
    }).unwrap_or_else(|| ("Label".to_string(), 14.0, egui::Color32::from_rgb(220, 220, 220)));

    let prev_text = text.clone();

    let mut edit = egui::TextEdit::multiline(&mut text)
        .font(egui::FontId::proportional(font_size))
        .text_color(col)
        .desired_width(width.max(40.0));
    if height > 8.0 {
        // Approximate row count from chosen height + font size for a reasonable
        // initial layout; the TextEdit will still scroll if content exceeds it.
        let line_h = (font_size * 1.4).max(10.0);
        let rows = (height / line_h).floor().max(1.0) as usize;
        edit = edit.desired_rows(rows);
    } else {
        edit = edit.desired_rows(2);
    }
    ui.add(edit);

    if text != prev_text {
        if let Some(node) = snarl.get_node_mut(node_id) {
            node.params.insert("text".to_string(), Value::String(text));
        }
    }
}

/// Read the text color stored on a label node. Defaults to a soft gray when
/// the param is missing or malformed (older patches saved before color support).
fn read_label_color(n: &NodeData) -> egui::Color32 {
    match n.params.get("color").and_then(|v| v.as_array()) {
        Some(arr) if arr.len() == 4 => egui::Color32::from_rgba_unmultiplied(
            arr[0].as_u64().unwrap_or(220) as u8,
            arr[1].as_u64().unwrap_or(220) as u8,
            arr[2].as_u64().unwrap_or(220) as u8,
            arr[3].as_u64().unwrap_or(255) as u8,
        ),
        _ => egui::Color32::from_rgb(220, 220, 220),
    }
}

/// Read the SVG tint color. Default is fully transparent (alpha = 0 = no tint).
fn read_svg_tint(n: &NodeData) -> egui::Color32 {
    match n.params.get("tint").and_then(|v| v.as_array()) {
        Some(arr) if arr.len() == 4 => egui::Color32::from_rgba_unmultiplied(
            arr[0].as_u64().unwrap_or(255) as u8,
            arr[1].as_u64().unwrap_or(255) as u8,
            arr[2].as_u64().unwrap_or(255) as u8,
            arr[3].as_u64().unwrap_or(0) as u8,
        ),
        _ => egui::Color32::TRANSPARENT,
    }
}

fn show_svg_body(node_id: NodeId, ui: &mut egui::Ui, snarl: &mut Snarl<NodeData>) {
    let outer = ui.allocate_ui(egui::vec2(160.0, 0.0), |ui| {
        show_svg_body_sized(node_id, ui, snarl, egui::vec2(160.0, 160.0));
    });
    register_exposable_element(ui, node_id, "image", outer.response.rect);
}

/// Renders the loaded SVG into a resizable area. The SVG is rasterized via
/// usvg/resvg to an RGBA pixmap whose pixels we then recolor according to the
/// chosen mode (Override = blend toward chosen color; Additive = add chosen
/// color on top). The recolored pixmap is uploaded as an egui texture and
/// painted into the rect. The texture is cached in egui memory keyed by
/// (node, rev, target_size, mode, color) so resizing/recoloring force a new
/// rasterization but unchanged frames don't.
fn show_svg_body_sized(
    node_id: NodeId,
    ui: &mut egui::Ui,
    snarl: &mut Snarl<NodeData>,
    container: egui::Vec2,
) {
    let (data, rev, tint, mode) = snarl.get_node(node_id).map(|n| {
        let d = n.params.get("svg_data").and_then(|v| v.as_str()).unwrap_or("").to_string();
        let r = n.params.get("svg_rev").and_then(|v| v.as_u64()).unwrap_or(0);
        let t = read_svg_tint(n);
        let m = n.params.get("color_mode").and_then(|v| v.as_str()).unwrap_or("override").to_string();
        (d, r, t, m)
    }).unwrap_or_default();

    let target = egui::vec2(container.x.max(16.0), container.y.max(16.0));
    let (rect, _) = ui.allocate_exact_size(target, egui::Sense::hover());

    if data.is_empty() {
        let painter = ui.painter_at(rect);
        painter.rect_stroke(rect, 4.0,
            egui::Stroke::new(1.0, egui::Color32::from_gray(70)),
            egui::StrokeKind::Inside);
        painter.text(
            rect.center(),
            egui::Align2::CENTER_CENTER,
            "No SVG loaded\nclick Load… in header",
            egui::FontId::proportional(11.0),
            egui::Color32::from_gray(150),
        );
        return;
    }

    let pw = (rect.width().round() as u32).max(1);
    let ph = (rect.height().round() as u32).max(1);
    let cache_key = egui::Id::new((
        "svg_tex_cache", node_id.0, rev, pw, ph, mode.as_str(),
        tint.r(), tint.g(), tint.b(), tint.a(),
    ));
    let cached = ui.ctx().data(|d| d.get_temp::<egui::TextureHandle>(cache_key));
    let tex = match cached {
        Some(t) => t,
        None => {
            match rasterize_svg_recolored(&data, pw, ph, &mode, tint) {
                Some(image) => {
                    let handle = ui.ctx().load_texture(
                        format!("svg-{}-{}", node_id.0, rev),
                        image,
                        egui::TextureOptions::LINEAR,
                    );
                    ui.ctx().data_mut(|d| d.insert_temp(cache_key, handle.clone()));
                    handle
                }
                None => {
                    ui.painter_at(rect).text(
                        rect.center(),
                        egui::Align2::CENTER_CENTER,
                        "SVG parse failed",
                        egui::FontId::proportional(11.0),
                        egui::Color32::from_rgb(220, 80, 80),
                    );
                    return;
                }
            }
        }
    };

    ui.painter().image(
        tex.id(),
        rect,
        egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
        egui::Color32::WHITE,
    );
}

/// Rasterize an SVG to a rect of `(w, h)` and recolor the resulting pixmap
/// according to `mode`:
///   - "override": each pixel's RGB is lerped toward `tint.rgb` by `tint.a/255`,
///     preserving the SVG's own per-pixel alpha (so silhouette is kept).
///   - "additive": `tint.rgb * (tint.a / 255)` is added to each pixel's RGB
///     (clamped), again preserving the SVG's alpha.
/// Returns None if the SVG can't be parsed.
fn rasterize_svg_recolored(
    svg_text: &str,
    w: u32,
    h: u32,
    mode: &str,
    tint: egui::Color32,
) -> Option<egui::ColorImage> {
    let opt = usvg::Options::default();
    let tree = usvg::Tree::from_str(svg_text, &opt).ok()?;

    let svg_size = tree.size();
    let sx = w as f32 / svg_size.width().max(1.0);
    let sy = h as f32 / svg_size.height().max(1.0);
    let scale = sx.min(sy);
    let render_w = (svg_size.width() * scale).round().max(1.0) as u32;
    let render_h = (svg_size.height() * scale).round().max(1.0) as u32;

    let mut pixmap = tiny_skia::Pixmap::new(w, h)?;
    // Center the rendered SVG in the target rect (preserves aspect ratio).
    let off_x = ((w as f32 - render_w as f32) * 0.5).max(0.0);
    let off_y = ((h as f32 - render_h as f32) * 0.5).max(0.0);
    let transform = tiny_skia::Transform::from_translate(off_x, off_y)
        .post_scale(1.0, 1.0)
        .pre_scale(scale, scale);
    resvg::render(&tree, transform, &mut pixmap.as_mut());

    // Recolor in place. tiny_skia stores premultiplied RGBA; we work in
    // straight-alpha space then re-premultiply on write.
    let blend = (tint.a() as f32 / 255.0).clamp(0.0, 1.0);
    let tr = tint.r() as f32;
    let tg = tint.g() as f32;
    let tb = tint.b() as f32;
    let pixels = pixmap.pixels_mut();
    if blend > 0.0 {
        match mode {
            "additive" => {
                for px in pixels.iter_mut() {
                    let a = px.alpha() as f32;
                    if a == 0.0 { continue; }
                    // Un-premultiply.
                    let r = (px.red()   as f32) / a * 255.0;
                    let g = (px.green() as f32) / a * 255.0;
                    let b = (px.blue()  as f32) / a * 255.0;
                    let r2 = (r + tr * blend).min(255.0);
                    let g2 = (g + tg * blend).min(255.0);
                    let b2 = (b + tb * blend).min(255.0);
                    // Re-premultiply.
                    let af = a / 255.0;
                    *px = tiny_skia::PremultipliedColorU8::from_rgba(
                        (r2 * af).round() as u8,
                        (g2 * af).round() as u8,
                        (b2 * af).round() as u8,
                        a as u8,
                    ).unwrap_or(*px);
                }
            }
            _ => {
                // Override: lerp original RGB toward tint RGB by `blend`.
                for px in pixels.iter_mut() {
                    let a = px.alpha() as f32;
                    if a == 0.0 { continue; }
                    let r = (px.red()   as f32) / a * 255.0;
                    let g = (px.green() as f32) / a * 255.0;
                    let b = (px.blue()  as f32) / a * 255.0;
                    let r2 = r + (tr - r) * blend;
                    let g2 = g + (tg - g) * blend;
                    let b2 = b + (tb - b) * blend;
                    let af = a / 255.0;
                    *px = tiny_skia::PremultipliedColorU8::from_rgba(
                        (r2 * af).round() as u8,
                        (g2 * af).round() as u8,
                        (b2 * af).round() as u8,
                        a as u8,
                    ).unwrap_or(*px);
                }
            }
        }
    }

    // Convert tiny-skia premultiplied RGBA → egui::ColorImage (also premultiplied).
    let raw = pixmap.data();
    let pixels: Vec<egui::Color32> = raw.chunks_exact(4)
        .map(|c| egui::Color32::from_rgba_premultiplied(c[0], c[1], c[2], c[3]))
        .collect();
    Some(egui::ColorImage {
        size: [w as usize, h as usize],
        pixels,
        source_size: egui::Vec2::new(w as f32, h as f32),
    })
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

    let mut shape_rect:   Option<egui::Rect> = None;
    let mut freq_rect:    Option<egui::Rect> = None;
    let mut phase_rect:   Option<egui::Rect> = None;
    let mut preview_rect: Option<egui::Rect> = None;
    ui.vertical(|ui| {
        // Row 1: shape selector
        let r = ui.horizontal(|ui| {
            changed |= ui.selectable_value(&mut shape, "sine".into(),     egui::RichText::new("Sine").small()).changed();
            changed |= ui.selectable_value(&mut shape, "triangle".into(), egui::RichText::new("Tri").small()).changed();
            changed |= ui.selectable_value(&mut shape, "saw".into(),      egui::RichText::new("Saw").small()).changed();
            changed |= ui.selectable_value(&mut shape, "square".into(),   egui::RichText::new("Sqr").small()).changed();
        });
        shape_rect = Some(r.response.rect);

        // Row 2: frequency unit toggle + value
        let r = ui.horizontal(|ui| {
            changed |= ui.selectable_value(&mut freq_unit, "hz".into(), egui::RichText::new("Hz").small()).changed();
            changed |= ui.selectable_value(&mut freq_unit, "ms".into(), egui::RichText::new("ms").small()).changed();
            let (lo, hi, spd) = if freq_unit == "hz" { (0.01, 200.0, 0.1) } else { (1.0, 60_000.0, 10.0) };
            changed |= ui.add_enabled(!freq_wired, egui::DragValue::new(&mut freq_p).speed(spd).range(lo..=hi)).changed();
        });
        freq_rect = Some(r.response.rect);

        // Row 3: phase offset
        let r = ui.horizontal(|ui| {
            ui.label(egui::RichText::new("Phase").small().weak());
            changed |= ui.add_enabled(!phase_wired, egui::DragValue::new(&mut phase_p).speed(0.01).range(0.0..=1.0)).changed();
            // Bi/Uni toggle
            ui.separator();
            changed |= ui.selectable_value(&mut bipolar, true,  egui::RichText::new("Bi").small()).changed();
            changed |= ui.selectable_value(&mut bipolar, false, egui::RichText::new("Uni").small()).changed();
        });
        phase_rect = Some(r.response.rect);

        // Row 4: waveform preview
        let preview_size = egui::vec2(ui.available_width().max(80.0), 36.0);
        let (rect, _) = ui.allocate_exact_size(preview_size, egui::Sense::hover());
        preview_rect = Some(rect);
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
    if let Some(r) = shape_rect   { register_exposable_element(ui, node_id, "shape",   r); }
    if let Some(r) = freq_rect    { register_exposable_element(ui, node_id, "freq",    r); }
    if let Some(r) = phase_rect   { register_exposable_element(ui, node_id, "phase",   r); }
    if let Some(r) = preview_rect { register_exposable_element(ui, node_id, "preview", r); }
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

    let mut mode_rect: Option<egui::Rect> = None;
    let mut gyr_rect:  Option<egui::Rect> = None;
    let mut acc_rect:  Option<egui::Rect> = None;
    ui.vertical(|ui| {
        ui.spacing_mut().item_spacing = egui::vec2(2.0, 2.0);

        let r = ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 2.0;
            for (label, id) in [("Local", "local"), ("Player", "player"), ("World", "world"), ("Laser", "laser")] {
                changed |= ui.selectable_value(&mut mode, id.to_string(), egui::RichText::new(label).small()).changed();
            }
        });
        mode_rect = Some(r.response.rect);

        let r = ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 3.0;
            ui.label(egui::RichText::new("Gyr:").small().weak());
            for i in 0..3 {
                let (label, tip) = GYR_LABELS[i];
                changed |= ui.checkbox(&mut inv_gyro[i], egui::RichText::new(label).small())
                    .on_hover_text(tip).changed();
            }
        });
        gyr_rect = Some(r.response.rect);

        let r = ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 3.0;
            ui.label(egui::RichText::new("Acc:").small().weak());
            for i in 0..3 {
                let (label, tip) = ACC_LABELS[i];
                changed |= ui.checkbox(&mut inv_accel[i], egui::RichText::new(label).small())
                    .on_hover_text(tip).changed();
            }
        });
        acc_rect = Some(r.response.rect);

        ui.label(egui::RichText::new(format!("X:{:+.3}  Y:{:+.3}", out_x, out_y)).small().weak());
    });
    if let Some(r) = mode_rect { register_exposable_element(ui, node_id, "mode",         r); }
    if let Some(r) = gyr_rect  { register_exposable_element(ui, node_id, "gyro_invert",  r); }
    if let Some(r) = acc_rect  { register_exposable_element(ui, node_id, "accel_invert", r); }

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

    let mut mode_rect:    Option<egui::Rect> = None;
    let mut range_rect:   Option<egui::Rect> = None;
    let mut step_rect:    Option<egui::Rect> = None;
    let mut minmax_rect:  Option<egui::Rect> = None;
    ui.vertical(|ui| {
        // Row 1: counting mode
        let r = ui.horizontal(|ui| {
            changed |= ui.selectable_value(&mut mode, "loop".into(),      egui::RichText::new("Loop").small()).changed();
            changed |= ui.selectable_value(&mut mode, "limit".into(),     egui::RichText::new("Limit").small()).changed();
            changed |= ui.selectable_value(&mut mode, "bounce".into(),    egui::RichText::new("Bounce").small()).changed();
            changed |= ui.selectable_value(&mut mode, "unlimited".into(), egui::RichText::new("Unlimited").small()).changed();
        });
        mode_rect = Some(r.response.rect);

        // Row 2: output range + reset button
        let r = ui.horizontal(|ui| {
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
        range_rect = Some(r.response.rect);

        // Row 3: Step
        let r = ui.horizontal(|ui| {
            ui.label(egui::RichText::new("Step").small().weak());
            changed |= ui.add_enabled(!step_wired, egui::DragValue::new(&mut step_p).speed(0.1).range(0.001..=10000.0)).changed();
        });
        step_rect = Some(r.response.rect);

        // Row 4: Min / Max
        let r = ui.horizontal(|ui| {
            ui.label(egui::RichText::new("Min").small().weak());
            changed |= ui.add_enabled(!min_wired, egui::DragValue::new(&mut min_p).speed(0.1)).changed();
            ui.label(egui::RichText::new("Max").small().weak());
            let max_active = !max_wired && mode != "unlimited";
            changed |= ui.add_enabled(max_active, egui::DragValue::new(&mut max_p).speed(0.1)).changed();
        });
        minmax_rect = Some(r.response.rect);
    });
    if let Some(r) = mode_rect   { register_exposable_element(ui, node_id, "mode",       r); }
    if let Some(r) = range_rect  { register_exposable_element(ui, node_id, "range_mode", r); }
    if let Some(r) = step_rect   { register_exposable_element(ui, node_id, "step",       r); }
    if let Some(r) = minmax_rect { register_exposable_element(ui, node_id, "min_max",    r); }

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

    let r1 = ui.horizontal(|ui| {
        changed |= ui.selectable_value(&mut mode, "delay_true".into(),  egui::RichText::new("Delay ON").small()).changed();
        changed |= ui.selectable_value(&mut mode, "delay_false".into(), egui::RichText::new("Delay OFF").small()).changed();
    });
    let r2 = ui.horizontal(|ui| {
        let limit = if unit == "ms" { 60_000.0 } else { 10_000.0 };
        changed |= ui.add(egui::DragValue::new(&mut time).speed(1.0).range(0.0..=limit)).changed();
        changed |= ui.selectable_value(&mut unit, "ms".into(),      egui::RichText::new("ms").small()).changed();
        changed |= ui.selectable_value(&mut unit, "samples".into(), egui::RichText::new("frames").small()).changed();
    });
    register_exposable_element(ui, node_id, "mode", r1.response.rect);
    register_exposable_element(ui, node_id, "time", r2.response.rect);

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
    let (value, bipolar) = snarl.get_node(node_id).map(|n| {
        let v = n.params.get("value").and_then(|v| v.as_f64()).unwrap_or(0.0) as f32;
        let b = n.params.get("bipolar").and_then(|v| v.as_bool()).unwrap_or(false);
        (v, b)
    }).unwrap_or((0.0, false));

    let (lo, hi) = if bipolar { (-1.0f32, 1.0f32) } else { (0.0f32, 1.0f32) };
    let mut v = value.clamp(lo, hi);
    let mut bipolar = bipolar;
    let mut value_changed = false;
    let mut mode_changed = false;

    let mut knob_rect: Option<egui::Rect> = None;
    ui.vertical(|ui| {
        egui::Resize::default()
            .id_salt(("knob", node_id))
            .default_size([80.0, 80.0])
            .min_size([40.0, 30.0])
            .show(ui, |ui| {
                let available = ui.available_size();
                let aspect = available.x / available.y.max(1.0);
                let (rect, resp) = ui.allocate_exact_size(available, egui::Sense::click_and_drag());
                knob_rect = Some(rect);

                if resp.double_clicked() {
                    v = 0.0f32.clamp(lo, hi);
                    value_changed = true;
                } else if resp.dragged() {
                    let delta = resp.drag_delta();
                    let range = hi - lo;
                    let norm_delta = if aspect >= 2.0 {
                        delta.x / rect.width()
                    } else {
                        -delta.y / rect.height()
                    };
                    v = (v + norm_delta * range).clamp(lo, hi);
                    value_changed = true;
                }

                if resp.hovered() {
                    let scroll = ui.input(|i| i.raw_scroll_delta.y);
                    if scroll != 0.0 {
                        v = (v + scroll * 0.005 * (hi - lo)).clamp(lo, hi);
                        value_changed = true;
                    }
                }

                let t = (v - lo) / (hi - lo);
                let painter = ui.painter_at(rect);
                let active = resp.hovered() || resp.dragged();

                if aspect >= 2.0 {
                    draw_knob_h_fader(&painter, rect, t, bipolar, active);
                } else if aspect <= 0.5 {
                    draw_knob_v_fader(&painter, rect, t, bipolar, active);
                } else {
                    draw_knob_rotary(&painter, rect, t, bipolar, active);
                }
            });

        ui.horizontal(|ui| {
            mode_changed |= ui.selectable_value(&mut bipolar, false, egui::RichText::new("Uni").small()).changed();
            mode_changed |= ui.selectable_value(&mut bipolar, true,  egui::RichText::new("Bi").small()).changed();
            ui.add_space(4.0);
            ui.label(egui::RichText::new(format!("{:.3}", v)).small().weak().monospace());
        });
    });
    if let Some(rect) = knob_rect {
        register_exposable_element(ui, node_id, "value", rect);
    }

    if value_changed || mode_changed {
        if let Some(node) = snarl.get_node_mut(node_id) {
            if mode_changed {
                node.params.insert("bipolar".to_string(), Value::Bool(bipolar));
                let new_lo = if bipolar { -1.0f64 } else { 0.0f64 };
                let cur_v = node.params.get("value").and_then(|v| v.as_f64()).unwrap_or(0.0);
                if let Some(n) = Number::from_f64(cur_v.clamp(new_lo, 1.0)) {
                    node.params.insert("value".to_string(), Value::Number(n));
                }
            }
            if value_changed {
                if let Some(n) = Number::from_f64(v as f64) {
                    node.params.insert("value".to_string(), Value::Number(n));
                }
            }
        }
    }
}

// Knob angle mapping: 135° screen angle (lower-left / ~7 o'clock) sweeping
// 270° clockwise to ~45° (lower-right / ~5 o'clock). t=0 → min, t=1 → max.
fn knob_angle_rad(t: f32) -> f32 {
    (135.0_f32 + t * 270.0_f32).to_radians()
}

fn draw_knob_rotary(painter: &egui::Painter, rect: egui::Rect, t: f32, bipolar: bool, active: bool) {
    let center = rect.center();
    let radius = (rect.width().min(rect.height()) * 0.5 - 4.0).max(10.0);
    let track_r = radius * 0.72;
    let n = 64usize;

    painter.circle_filled(center, radius, Color32::from_gray(30));
    painter.circle_stroke(center, radius, egui::Stroke::new(1.0, Color32::from_gray(55)));

    let arc_pts = |t0: f32, t1: f32| -> Vec<egui::Pos2> {
        (0..=n).map(|i| {
            let ti = t0 + (i as f32 / n as f32) * (t1 - t0);
            let a = knob_angle_rad(ti);
            egui::pos2(center.x + track_r * a.cos(), center.y + track_r * a.sin())
        }).collect()
    };

    // Background track
    painter.add(egui::Shape::line(arc_pts(0.0, 1.0), egui::Stroke::new(3.0, Color32::from_gray(50))));

    // Value arc
    let accent = if bipolar { Color32::from_rgb(100, 150, 255) } else { Color32::from_rgb(80, 200, 120) };
    let (a0, a1) = if bipolar { if t >= 0.5 { (0.5, t) } else { (t, 0.5) } } else { (0.0, t) };
    if (a1 - a0).abs() > 0.001 {
        painter.add(egui::Shape::line(arc_pts(a0, a1), egui::Stroke::new(3.0, accent)));
    }

    // Indicator
    let va = knob_angle_rad(t);
    painter.line_segment(
        [
            egui::pos2(center.x + radius * 0.18 * va.cos(), center.y + radius * 0.18 * va.sin()),
            egui::pos2(center.x + radius * 0.62 * va.cos(), center.y + radius * 0.62 * va.sin()),
        ],
        egui::Stroke::new(2.0, if active { Color32::WHITE } else { Color32::from_gray(200) }),
    );
    painter.circle_filled(center, 2.5, Color32::from_gray(85));
}

fn draw_knob_h_fader(painter: &egui::Painter, rect: egui::Rect, t: f32, bipolar: bool, active: bool) {
    let margin = 8.0f32;
    let track_rect = egui::Rect::from_center_size(
        rect.center(),
        egui::vec2(rect.width() - 2.0 * margin, 6.0),
    );
    painter.rect_filled(track_rect, 3.0, Color32::from_gray(40));

    let handle_x = track_rect.left() + t * track_rect.width();
    let accent = if bipolar { Color32::from_rgb(100, 150, 255) } else { Color32::from_rgb(80, 200, 120) };

    let fill = if bipolar {
        let cx = track_rect.center().x;
        if handle_x >= cx {
            egui::Rect::from_min_max(egui::pos2(cx, track_rect.top()), egui::pos2(handle_x, track_rect.bottom()))
        } else {
            egui::Rect::from_min_max(egui::pos2(handle_x, track_rect.top()), egui::pos2(cx, track_rect.bottom()))
        }
    } else {
        egui::Rect::from_min_max(track_rect.min, egui::pos2(handle_x, track_rect.bottom()))
    };
    painter.rect_filled(fill, 0.0, accent);

    if bipolar {
        let cx = track_rect.center().x;
        painter.line_segment(
            [egui::pos2(cx, track_rect.top() - 2.0), egui::pos2(cx, track_rect.bottom() + 2.0)],
            egui::Stroke::new(1.0, Color32::from_gray(110)),
        );
    }

    let r = if active { 7.0 } else { 5.0 };
    painter.circle_filled(
        egui::pos2(handle_x, rect.center().y), r,
        if active { Color32::WHITE } else { Color32::from_gray(200) },
    );
}

fn draw_knob_v_fader(painter: &egui::Painter, rect: egui::Rect, t: f32, bipolar: bool, active: bool) {
    let margin = 8.0f32;
    let track_rect = egui::Rect::from_center_size(
        rect.center(),
        egui::vec2(6.0, rect.height() - 2.0 * margin),
    );
    painter.rect_filled(track_rect, 3.0, Color32::from_gray(40));

    // t=1 → top of track, t=0 → bottom
    let handle_y = track_rect.bottom() - t * track_rect.height();
    let accent = if bipolar { Color32::from_rgb(100, 150, 255) } else { Color32::from_rgb(80, 200, 120) };

    let fill = if bipolar {
        let cy = track_rect.center().y;
        if handle_y <= cy {
            egui::Rect::from_min_max(egui::pos2(track_rect.left(), handle_y), egui::pos2(track_rect.right(), cy))
        } else {
            egui::Rect::from_min_max(egui::pos2(track_rect.left(), cy), egui::pos2(track_rect.right(), handle_y))
        }
    } else {
        egui::Rect::from_min_max(egui::pos2(track_rect.left(), handle_y), track_rect.max)
    };
    painter.rect_filled(fill, 0.0, accent);

    if bipolar {
        let cy = track_rect.center().y;
        painter.line_segment(
            [egui::pos2(track_rect.left() - 2.0, cy), egui::pos2(track_rect.right() + 2.0, cy)],
            egui::Stroke::new(1.0, Color32::from_gray(110)),
        );
    }

    let r = if active { 7.0 } else { 5.0 };
    painter.circle_filled(
        egui::pos2(rect.center().x, handle_y), r,
        if active { Color32::WHITE } else { Color32::from_gray(200) },
    );
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

// ── Sub-patch body ────────────────────────────────────────────────────────────

/// Type selector body for Inlet and Outlet nodes inside a sub-patch.
/// The selected type is stored in `params["signal_type"]` and propagated to the node's pin.
fn show_inlet_outlet_body(node_id: NodeId, ui: &mut egui::Ui, snarl: &mut Snarl<NodeData>) {
    let (current, is_inlet) = snarl.get_node(node_id)
        .map(|n| {
            let t = n.params.get("signal_type")
                .and_then(|v| serde_json::from_value(v.clone()).ok())
                .unwrap_or(SignalType::Any);
            (t, n.module_id == "subpatch.inlet")
        })
        .unwrap_or((SignalType::Any, true));

    let mut t = current;
    egui::ComboBox::from_id_salt(egui::Id::new(("sp_io_type", node_id.0)))
        .selected_text(format!("{:?}", t))
        .width(74.0)
        .show_ui(ui, |ui| {
            for opt in [SignalType::Float, SignalType::Bool, SignalType::Vec2,
                        SignalType::Int, SignalType::Any, SignalType::AutoMap]
            {
                ui.selectable_value(&mut t, opt, format!("{:?}", opt));
            }
        });

    if t != current {
        if let Some(node) = snarl.get_node_mut(node_id) {
            if let Ok(v) = serde_json::to_value(t) {
                node.params.insert("signal_type".to_string(), v);
            }
            // Update the visible pin type immediately.
            if is_inlet {
                if let Some(pin) = node.outputs.get_mut(0) { pin.signal_type = t; }
            } else {
                if let Some(pin) = node.inputs.get_mut(0) { pin.signal_type = t; }
            }
        }
    }
}

// ── Sub-patch body: bare pinned UI elements ───────────────────────────────────

/// Renders pinned UI elements at their stored 2D positions, **without** any
/// surrounding container/title — each element is a free-floating widget.
/// In Layout mode (`outer_node.extra.layout_unlocked`) each element gets a
/// dashed selection outline + corner resize handle and the underlying widget
/// is disabled so the user can drag/resize without accidentally operating it.
/// In Lock mode the widget is fully interactive and no chrome is drawn.
fn show_subpatch_body(outer_id: NodeId, ui: &mut egui::Ui, snarl: &mut Snarl<NodeData>) -> bool {
    let exposed: Vec<ExposedModule> = snarl.get_node(outer_id)
        .and_then(|n| n.subpatch.as_ref())
        .map(|sp| sp.exposed_modules.clone())
        .unwrap_or_default();

    if exposed.is_empty() {
        return false;
    }

    let is_unlocked = snarl.get_node(outer_id)
        .map(|n| n.extra.layout_unlocked)
        .unwrap_or(false);

    let (snap_enabled, snap_grid) = snarl.get_node(outer_id)
        .and_then(|n| n.subpatch.as_ref())
        .map(|sp| (sp.snap_enabled, sp.snap_grid_px.max(2) as f32))
        .unwrap_or((false, 8.0));

    // Collect module info before any mutable borrow; flag stale references.
    let infos: Vec<(egui_snarl::NodeId, String, bool, String)> = exposed.iter().map(|e| {
        let inner_id = egui_snarl::NodeId(e.inner_node_id);
        let inner = snarl.get_node(outer_id)
            .and_then(|n| n.subpatch.as_ref())
            .and_then(|sp| sp.snarl.get_node(inner_id));
        (
            inner_id,
            inner.map(|n| n.module_id.clone()).unwrap_or_default(),
            inner.is_some(),
            inner.map(|n| n.display_name.clone()).unwrap_or_default(),
        )
    }).collect();

    // 2D origin — everything is positioned relative to here.
    let origin = ui.cursor().min;

    // Layout mode: faint snap grid for visual feedback. Drawn under widgets.
    if is_unlocked && snap_enabled && snap_grid >= 2.0 {
        let avail_w = ui.available_width().max(40.0);
        let avail_h = ui.available_height().max(40.0);
        let painter = ui.painter().with_clip_rect(egui::Rect::from_min_size(
            origin, egui::vec2(avail_w, avail_h),
        ));
        let stroke = egui::Stroke::new(0.5, egui::Color32::from_rgba_unmultiplied(150, 200, 255, 28));
        let mut x = 0.0f32;
        while x <= avail_w {
            let xp = origin.x + x;
            painter.line_segment(
                [egui::pos2(xp, origin.y), egui::pos2(xp, origin.y + avail_h)],
                stroke,
            );
            x += snap_grid;
        }
        let mut y = 0.0f32;
        while y <= avail_h {
            let yp = origin.y + y;
            painter.line_segment(
                [egui::pos2(origin.x, yp), egui::pos2(origin.x + avail_w, yp)],
                stroke,
            );
            y += snap_grid;
        }
    }

    let mut remove: Option<usize> = None;
    // Live target values each frame. None = no drag/resize active for this idx.
    let mut pos_targets:  Vec<Option<[f32; 2]>> = vec![None; exposed.len()];
    let mut size_targets: Vec<Option<[f32; 2]>> = vec![None; exposed.len()];

    let shift_held = ui.input(|i| i.modifiers.shift);
    const RESIZE_HANDLE: f32 = 18.0;
    const MIN_W: f32 = 32.0;
    const MIN_H: f32 = 18.0;

    // Drag origins are stashed in egui memory so successive frames can compute
    // the snapped target from origin + total drag offset, rather than
    // accumulating per-frame snapped deltas (which causes the "drunk" lag where
    // it takes lots of cursor motion to cross to the next snap line).
    let drag_pos_id  = |i: usize| egui::Id::new(("sp_drag_pos_origin",  outer_id.0, i));
    let drag_size_id = |i: usize| egui::Id::new(("sp_drag_size_origin", outer_id.0, i));

    for (idx, (exp, (inner_id, module_id, exists, inner_display_name))) in
        exposed.iter().zip(infos.iter()).enumerate()
    {
        // Auto-remove stale references (inner node deleted).
        if !exists {
            remove = Some(idx);
            continue;
        }

        let mod_pos  = origin + egui::vec2(exp.pos[0],  exp.pos[1]);
        let mod_size = egui::vec2(exp.size[0].max(MIN_W), exp.size[1].max(MIN_H));
        let element_rect = egui::Rect::from_min_size(mod_pos, mod_size);
        let inner_id_c = *inner_id;

        // Render the bare element. In Layout mode the widget is disabled so
        // clicks fall through to the drag/resize/select rectangles below.
        // Intersect (don't replace) the parent clip so painting can never escape
        // the snarl node's body area into the tab bar / device panels.
        ui.allocate_ui_at_rect(element_rect, |ui| {
            let new_clip = ui.clip_rect().intersect(element_rect);
            ui.set_clip_rect(new_clip);
            ui.add_enabled_ui(!is_unlocked, |ui| {
                let sp = snarl.get_node_mut(outer_id).and_then(|n| n.subpatch.as_mut());
                if let Some(sp) = sp {
                    render_pinned_element(
                        inner_id_c, module_id, &exp.element_id, ui, &mut sp.snarl,
                        mod_size,
                    );
                }
            });
        });

        // Layout mode: dashed outline + drag area + corner resize handle.
        // IMPORTANT: order matters — egui's `interact` API gives priority to
        // the LATER interact when rects overlap. Body interact is added first
        // (full rect), then the handle (small corner) on top, so clicks in the
        // corner go to the handle.
        if is_unlocked {
            let outline = egui::Stroke::new(1.0, egui::Color32::from_rgba_unmultiplied(150, 220, 255, 200));
            let r = element_rect;
            ui.painter().line_segment([r.left_top(),     r.right_top()],    outline);
            ui.painter().line_segment([r.right_top(),    r.right_bottom()], outline);
            ui.painter().line_segment([r.right_bottom(), r.left_bottom()],  outline);
            ui.painter().line_segment([r.left_bottom(),  r.left_top()],     outline);

            let handle_rect = egui::Rect::from_min_size(
                egui::pos2(element_rect.max.x - RESIZE_HANDLE, element_rect.max.y - RESIZE_HANDLE),
                egui::vec2(RESIZE_HANDLE, RESIZE_HANDLE),
            );

            // ── Drag (move) — body interact FIRST so the handle wins later ──
            let interact_resp = ui.interact(
                element_rect,
                egui::Id::new(("sp_layout_interact", outer_id.0, idx)),
                egui::Sense::click_and_drag(),
            );
            let pointer_in_handle = ui.ctx().input(|i| i.pointer.interact_pos())
                .map(|p| handle_rect.contains(p))
                .unwrap_or(false);

            // ── Resize handle — added AFTER body, so it wins inside its rect ──
            let handle_resp = ui.interact(
                handle_rect,
                egui::Id::new(("sp_layout_resize", outer_id.0, idx)),
                egui::Sense::click_and_drag(),
            );

            // Visual: filled-in tinted square + corner stripes for the handle.
            // The fill makes it much easier to spot/grab than just stripes.
            let fill = if handle_resp.hovered() || handle_resp.dragged() {
                egui::Color32::from_rgba_unmultiplied(180, 230, 255, 130)
            } else {
                egui::Color32::from_rgba_unmultiplied(150, 220, 255, 80)
            };
            ui.painter().rect_filled(handle_rect, 2.0, fill);
            let stroke = egui::Stroke::new(1.2, egui::Color32::from_rgb(180, 230, 255));
            for k in 1..=3 {
                let off = k as f32 * (RESIZE_HANDLE / 4.0);
                ui.painter().line_segment(
                    [egui::pos2(handle_rect.max.x - off, handle_rect.max.y),
                     egui::pos2(handle_rect.max.x,       handle_rect.max.y - off)],
                    stroke,
                );
            }

            // Origin-based resize. Capture starting size on drag start;
            // each frame, target = origin + total_offset, snapped at apply.
            if handle_resp.drag_started() {
                ui.ctx().data_mut(|d| d.insert_temp(
                    drag_size_id(idx),
                    [exp.size[0], exp.size[1], 0.0f32, 0.0f32],
                ));
            }
            if handle_resp.dragged_by(egui::PointerButton::Primary) {
                let prev = ui.ctx().data(|d| d.get_temp::<[f32;4]>(drag_size_id(idx)))
                    .unwrap_or([exp.size[0], exp.size[1], 0.0, 0.0]);
                let dd = handle_resp.drag_delta();
                let mut acc_x = prev[2] + dd.x;
                let mut acc_y = prev[3] + dd.y;
                if shift_held {
                    let aspect = (prev[0] / prev[1].max(1.0)).max(0.0001);
                    if acc_x.abs() * (1.0 / aspect) > acc_y.abs() {
                        acc_y = acc_x / aspect;
                    } else {
                        acc_x = acc_y * aspect;
                    }
                }
                ui.ctx().data_mut(|d| d.insert_temp(
                    drag_size_id(idx),
                    [prev[0], prev[1], acc_x, acc_y],
                ));
                size_targets[idx] = Some([prev[0] + acc_x, prev[1] + acc_y]);
            }
            if handle_resp.drag_stopped() {
                ui.ctx().data_mut(|d| d.remove_temp::<[f32;4]>(drag_size_id(idx)));
            }
            let menu_label = if !inner_display_name.is_empty() {
                format!("{} — {}", inner_display_name, exp.element_id)
            } else {
                exp.element_id.clone()
            };
            handle_resp.context_menu(|ui| {
                ui.label(egui::RichText::new(&menu_label).small().weak());
                ui.separator();
                if ui.button("Unpin").clicked() {
                    remove = Some(idx);
                    ui.close_menu();
                }
            });

            // Body drag — only when the pointer is NOT in the handle and the
            // handle isn't currently being dragged. egui's later-wins rule
            // already steals the click for the handle, but this guard also
            // prevents pos updates when the user grabs the corner before the
            // overlap check kicks in.
            let body_dragging = interact_resp.dragged_by(egui::PointerButton::Primary)
                && !pointer_in_handle
                && !handle_resp.dragged_by(egui::PointerButton::Primary);

            if interact_resp.drag_started() && !pointer_in_handle {
                ui.ctx().data_mut(|d| d.insert_temp(
                    drag_pos_id(idx),
                    [exp.pos[0], exp.pos[1], 0.0f32, 0.0f32],
                ));
            }
            if body_dragging {
                let prev = ui.ctx().data(|d| d.get_temp::<[f32;4]>(drag_pos_id(idx)))
                    .unwrap_or([exp.pos[0], exp.pos[1], 0.0, 0.0]);
                let dd = interact_resp.drag_delta();
                let acc_x = prev[2] + dd.x;
                let acc_y = prev[3] + dd.y;
                ui.ctx().data_mut(|d| d.insert_temp(
                    drag_pos_id(idx),
                    [prev[0], prev[1], acc_x, acc_y],
                ));
                pos_targets[idx] = Some([prev[0] + acc_x, prev[1] + acc_y]);
            }
            if interact_resp.drag_stopped() {
                ui.ctx().data_mut(|d| d.remove_temp::<[f32;4]>(drag_pos_id(idx)));
            }
            interact_resp.context_menu(|ui| {
                ui.label(egui::RichText::new(&menu_label).small().weak());
                ui.separator();
                if ui.button("Unpin").clicked() {
                    remove = Some(idx);
                    ui.close_menu();
                }
            });
        }
    }

    // Apply position / size targets. Snap is applied to the *absolute target*
    // value (origin + total drag offset), not to per-frame accumulated deltas.
    // This avoids the "drunk" lag where reaching the next snap step required
    // dragging extra distance because the stored value silently rounded down.
    let any_moved   = pos_targets.iter() .any(|t| t.is_some());
    let any_resized = size_targets.iter().any(|t| t.is_some());
    let snap = |v: f32| -> f32 {
        if snap_enabled && snap_grid > 0.5 {
            (v / snap_grid).round() * snap_grid
        } else { v }
    };
    if any_moved || any_resized {
        if let Some(node) = snarl.get_node_mut(outer_id) {
            if let Some(sp) = node.subpatch.as_mut() {
                for i in 0..sp.exposed_modules.len() {
                    if let Some(m) = sp.exposed_modules.get_mut(i) {
                        if let Some(t) = pos_targets.get(i).copied().flatten() {
                            let mut tx = t[0];
                            let mut ty = t[1];
                            if snap_enabled { tx = snap(tx); ty = snap(ty); }
                            m.pos[0] = tx.max(0.0);
                            m.pos[1] = ty.max(0.0);
                        }
                        if let Some(t) = size_targets.get(i).copied().flatten() {
                            let mut tw = t[0];
                            let mut th = t[1];
                            if snap_enabled { tw = snap(tw); th = snap(th); }
                            m.size[0] = tw.max(MIN_W);
                            m.size[1] = th.max(MIN_H);
                        }
                    }
                }
            }
        }
    }

    // Apply unpin.
    if let Some(i) = remove {
        if let Some(node) = snarl.get_node_mut(outer_id) {
            if let Some(sp) = node.subpatch.as_mut() {
                sp.exposed_modules.remove(i);
            }
            if node.subpatch.as_ref().map(|sp| sp.exposed_modules.is_empty()).unwrap_or(true) {
                node.extra.layout_unlocked = false;
            }
        }
    }

    false
}

/// Dispatches to the appropriate per-element renderer for a pinned element.
/// `element_id == "default"` renders the whole module body (back-compat path,
/// for legacy patches that pinned entire bodies); other ids render just one
/// UI element of the module sized to fit the user-chosen container.
fn render_pinned_element(
    inner_id: egui_snarl::NodeId,
    module_id: &str,
    element_id: &str,
    ui: &mut egui::Ui,
    inner_snarl: &mut Snarl<NodeData>,
    container_size: egui::Vec2,
) {
    let cap_w = container_size.x.max(20.0);
    ui.set_max_width(cap_w);

    // ── Per-element renderers ────────────────────────────────────────────────
    match (module_id, element_id) {
        // Knob slider: scaled-up slider taking the full container width.
        ("module.knob", "value") => {
            render_knob_value(inner_id, ui, inner_snarl, container_size);
            return;
        }
        // Constant: just the dragvalue, no other UI clutter.
        ("module.constant", "value") => {
            render_constant_value(inner_id, ui, inner_snarl, container_size);
            return;
        }
        // Switch: just the toggle.
        ("module.switch", "toggle") => {
            render_switch_toggle(inner_id, ui, inner_snarl, container_size);
            return;
        }
        // Text label: read-only when pinned (locked or in layout mode), so the
        // outer body never accepts text edits — those happen inside the editor.
        ("module.label", "text") => {
            render_label_text_readonly(inner_id, ui, inner_snarl, container_size);
            return;
        }
        ("module.svg", "image") => {
            show_svg_body_sized(inner_id, ui, inner_snarl, container_size);
            return;
        }
        // Gyro 3DOF — three pinable rows.
        ("processing.gyro_3dof", "mode") => {
            render_gyro_mode_row(inner_id, ui, inner_snarl, container_size);
            return;
        }
        ("processing.gyro_3dof", "gyro_invert") => {
            render_gyro_invert_row(inner_id, ui, inner_snarl, container_size);
            return;
        }
        ("processing.gyro_3dof", "accel_invert") => {
            render_accel_invert_row(inner_id, ui, inner_snarl, container_size);
            return;
        }
        // Response curve graphs (regular and Vec): render only the curve
        // canvas, no surrounding sliders.
        ("module.response_curve", "curve") => {
            render_response_curve_only(inner_id, ui, inner_snarl, container_size, false);
            return;
        }
        ("module.vec_response_curve", "curve") => {
            render_response_curve_only(inner_id, ui, inner_snarl, container_size, true);
            return;
        }
        ("module.response_curve", "scale_row") => {
            render_response_curve_scale_row(inner_id, ui, inner_snarl, container_size, false);
            return;
        }
        ("module.response_curve", "range_row") => {
            render_response_curve_range_row(inner_id, ui, inner_snarl, container_size, false);
            return;
        }
        ("module.response_curve", "grid_row") => {
            render_response_curve_grid_row(inner_id, ui, inner_snarl, container_size);
            return;
        }
        ("module.vec_response_curve", "scale_row") => {
            render_response_curve_scale_row(inner_id, ui, inner_snarl, container_size, true);
            return;
        }
        ("module.vec_response_curve", "range_row") => {
            render_response_curve_range_row(inner_id, ui, inner_snarl, container_size, true);
            return;
        }
        ("module.vec_response_curve", "grid_row") => {
            render_response_curve_grid_row(inner_id, ui, inner_snarl, container_size);
            return;
        }
        // Average / Delay / DC Filter — bare DragValue rows.
        ("module.average", "samples") => {
            render_dragvalue_param(inner_id, ui, inner_snarl, container_size,
                "Samples", "buf_size", 10.0, 1.0, 1.0..=10_000.0, None);
            return;
        }
        ("module.average", "spike_mad") => {
            render_dragvalue_param(inner_id, ui, inner_snarl, container_size,
                "Spike MAD", "spike_mad", 0.0, 0.1, 0.0..=20.0, Some(1));
            return;
        }
        ("module.delay", "ms") => {
            render_dragvalue_param(inner_id, ui, inner_snarl, container_size,
                "ms", "delay_ms", 100.0, 1.0, 0.0..=60_000.0, None);
            return;
        }
        ("module.dc_filter", "window_ms") => {
            render_dragvalue_param(inner_id, ui, inner_snarl, container_size,
                "Window ms", "window_ms", 500.0, 10.0, 10.0..=60_000.0, None);
            return;
        }
        ("module.dc_filter", "decay_ms") => {
            render_dragvalue_param(inner_id, ui, inner_snarl, container_size,
                "Decay ms", "decay_ms", 200.0, 10.0, 10.0..=60_000.0, None);
            return;
        }
        // Counter — per-row.
        ("logic.counter", "mode")       => { render_counter_mode(inner_id, ui, inner_snarl, container_size); return; }
        ("logic.counter", "range_mode") => { render_counter_range_mode(inner_id, ui, inner_snarl, container_size); return; }
        ("logic.counter", "step")       => { render_counter_step(inner_id, ui, inner_snarl, container_size); return; }
        ("logic.counter", "min_max")    => { render_counter_min_max(inner_id, ui, inner_snarl, container_size); return; }
        // Logic Delay — mode + time.
        ("logic.delay", "mode") => { render_logic_delay_mode(inner_id, ui, inner_snarl, container_size); return; }
        ("logic.delay", "time") => { render_logic_delay_time(inner_id, ui, inner_snarl, container_size); return; }
        // Oscillator — per-row + bare preview.
        ("generator.oscillator", "shape")   => { render_oscillator_shape(inner_id, ui, inner_snarl, container_size); return; }
        ("generator.oscillator", "freq")    => { render_oscillator_freq(inner_id, ui, inner_snarl, container_size);  return; }
        ("generator.oscillator", "phase")   => { render_oscillator_phase(inner_id, ui, inner_snarl, container_size); return; }
        ("generator.oscillator", "preview") => {
            render_oscillator_preview(inner_id, ui, inner_snarl, container_size);
            return;
        }
        // Readout — live value display, scaled to container.
        ("display.readout", "value") => {
            render_readout_value(inner_id, ui, inner_snarl, container_size);
            return;
        }
        // Oscilloscope — bare display + bare controls row.
        ("display.oscilloscope", "display") => {
            render_oscilloscope_display(inner_id, ui, inner_snarl, container_size);
            return;
        }
        ("display.oscilloscope", "controls") => {
            render_oscilloscope_controls(inner_id, ui, inner_snarl, container_size);
            return;
        }
        // Vectorscope — bare display.
        ("display.vectorscope", "display") => {
            render_vectorscope_display(inner_id, ui, inner_snarl, container_size);
            return;
        }
        _ => {}
    }

    // ── Legacy / unknown element_id ─────────────────────────────────────────
    // Older patches stored `element_id == "default"` (whole-body pin). The new
    // model is per-element only, so render a placeholder asking the user to
    // re-pin via Layout mode rather than displaying a misleading body crop.
    let _ = container_size;
    let _ = inner_snarl;
    let _ = inner_id;
    let _ = module_id;
    let _ = element_id;
    ui.label(egui::RichText::new("Re-pin via Layout mode").small().weak());
}

fn dispatch_pinned_body(
    inner_id: egui_snarl::NodeId,
    module_id: &str,
    ui: &mut egui::Ui,
    inner_snarl: &mut Snarl<NodeData>,
) {
    // Cap width so modules that use available_width() (oscillator, response curve)
    // don't expand to fill the 800px allocate_ui_at_rect max_rect.
    // Knob uses egui::Resize (its own stored size) and is unaffected by this.
    let cap = ui.available_width().min(450.0);
    ui.set_max_width(cap);
    match module_id {
        "module.knob"       => show_knob_body(inner_id, ui, inner_snarl),
        "module.constant"   => show_constant_body(inner_id, ui, inner_snarl),
        "module.switch"     => show_switch_body(inner_id, ui, inner_snarl),
        "module.label"      => show_label_body(inner_id, ui, inner_snarl),
        "generator.oscillator" => show_oscillator_body(inner_id, &[], ui, inner_snarl),
        "module.selector"   => show_selector_body(inner_id, &[], ui, inner_snarl),
        "module.response_curve"     => show_response_curve_body(inner_id, &[], &[], ui, inner_snarl),
        "module.vec_response_curve" => show_vec_response_curve_body(inner_id, &[], &[], ui, inner_snarl),
        "processing.gyro_3dof"      => show_gyro_3dof_body(inner_id, ui, inner_snarl),
        "logic.greater_than" | "logic.less_than" => show_or_equal_body(inner_id, ui, inner_snarl),
        "logic.delay"       => show_logic_delay_body(inner_id, ui, inner_snarl),
        "logic.counter"     => show_counter_body(inner_id, &[], ui, inner_snarl),
        "module.delay"      => show_delay_body(inner_id, &[], &[], ui, inner_snarl),
        "module.average"    => show_average_body(inner_id, &[], &[], ui, inner_snarl),
        "module.dc_filter"  => show_dc_filter_body(inner_id, &[], &[], ui, inner_snarl),
        _ => { /* no body for this module type */ }
    }
}

// ── Per-element pinned renderers ──────────────────────────────────────────────
//
// These render a single UI element of a module sized to the user's chosen
// container, with no extra controls / labels around it. They intentionally
// avoid exposing buttons that would mutate the inner module's I/O structure
// (e.g. add/remove pins, Learn, Clear unused).

fn render_knob_value(
    inner_id: NodeId,
    ui: &mut egui::Ui,
    inner_snarl: &mut Snarl<NodeData>,
    container: egui::Vec2,
) {
    let (value, bipolar) = inner_snarl.get_node(inner_id).map(|n| {
        let v = n.params.get("value").and_then(|v| v.as_f64()).unwrap_or(0.0) as f32;
        let b = n.params.get("bipolar").and_then(|v| v.as_bool()).unwrap_or(false);
        (v, b)
    }).unwrap_or((0.0, false));

    let (lo, hi) = if bipolar { (-1.0f32, 1.0f32) } else { (0.0f32, 1.0f32) };
    let mut v = value.clamp(lo, hi);

    let avail = egui::vec2(container.x.max(40.0), container.y.max(20.0));
    let aspect = avail.x / avail.y.max(1.0);
    let (rect, resp) = ui.allocate_exact_size(avail, egui::Sense::click_and_drag());

    let mut value_changed = false;
    if resp.double_clicked() {
        v = 0.0f32.clamp(lo, hi);
        value_changed = true;
    } else if resp.dragged() {
        let delta = resp.drag_delta();
        let range = hi - lo;
        let norm_delta = if aspect >= 2.0 { delta.x / rect.width() } else { -delta.y / rect.height() };
        v = (v + norm_delta * range).clamp(lo, hi);
        value_changed = true;
    }
    if resp.hovered() {
        let scroll = ui.input(|i| i.raw_scroll_delta.y);
        if scroll != 0.0 {
            v = (v + scroll * 0.005 * (hi - lo)).clamp(lo, hi);
            value_changed = true;
        }
    }

    let t = (v - lo) / (hi - lo);
    let painter = ui.painter_at(rect);
    let active = resp.hovered() || resp.dragged();
    if aspect >= 2.0 {
        draw_knob_h_fader(&painter, rect, t, bipolar, active);
    } else if aspect <= 0.5 {
        draw_knob_v_fader(&painter, rect, t, bipolar, active);
    } else {
        draw_knob_rotary(&painter, rect, t, bipolar, active);
    }

    if value_changed {
        if let Some(node) = inner_snarl.get_node_mut(inner_id) {
            if let Some(n) = Number::from_f64(v as f64) {
                node.params.insert("value".to_string(), Value::Number(n));
            }
        }
    }
}

fn render_constant_value(
    inner_id: NodeId,
    ui: &mut egui::Ui,
    inner_snarl: &mut Snarl<NodeData>,
    container: egui::Vec2,
) {
    let value = inner_snarl.get_node(inner_id)
        .and_then(|n| n.params.get("value").and_then(|v| v.as_f64()))
        .unwrap_or(0.0) as f32;
    let mut v = value;
    // Use the full container width for the dragvalue.
    ui.set_max_width(container.x);
    if ui.add_sized([container.x, 24.0], egui::DragValue::new(&mut v).speed(0.01)).changed() {
        if let Some(node) = inner_snarl.get_node_mut(inner_id) {
            if let Some(n) = Number::from_f64(v as f64) {
                node.params.insert("value".to_string(), Value::Number(n));
            }
        }
    }
}

fn render_switch_toggle(
    inner_id: NodeId,
    ui: &mut egui::Ui,
    inner_snarl: &mut Snarl<NodeData>,
    container: egui::Vec2,
) {
    let active = inner_snarl.get_node(inner_id)
        .and_then(|n| n.params.get("active").and_then(|v| v.as_bool()))
        .unwrap_or(false);
    let mut a = active;
    let label = if a { "ON" } else { "OFF" };
    let h = container.y.max(24.0);
    if ui.add_sized([container.x, h], egui::SelectableLabel::new(a, label)).clicked() {
        a = !a;
        if let Some(node) = inner_snarl.get_node_mut(inner_id) {
            node.params.insert("active".to_string(), Value::Bool(a));
        }
    }
}

/// Read-only label for the outer body. Editing happens only in the editor;
/// the body shows static text at the chosen font size.
fn render_label_text_readonly(
    inner_id: NodeId,
    ui: &mut egui::Ui,
    inner_snarl: &mut Snarl<NodeData>,
    container: egui::Vec2,
) {
    let (text, font_size, col) = inner_snarl.get_node(inner_id).map(|n| {
        let t = n.params.get("text").and_then(|v| v.as_str()).unwrap_or("Label").to_string();
        let f = n.params.get("font_size").and_then(|v| v.as_f64()).unwrap_or(14.0) as f32;
        let c = read_label_color(n);
        (t, f, c)
    }).unwrap_or_else(|| ("Label".to_string(), 14.0, egui::Color32::from_rgb(220, 220, 220)));
    ui.set_max_width(container.x);
    ui.label(egui::RichText::new(text).size(font_size).color(col));
}

// ── Gyro 3DOF row renderers ──────────────────────────────────────────────────

fn render_gyro_mode_row(inner_id: NodeId, ui: &mut egui::Ui, snarl: &mut Snarl<NodeData>, container: egui::Vec2) {
    let mut mode = snarl.get_node(inner_id)
        .and_then(|n| n.params.get("mode").and_then(|v| v.as_str()).map(|s| s.to_string()))
        .unwrap_or_else(|| "local".to_string());
    let mut changed = false;
    ui.set_max_width(container.x);
    apply_widget_scale(ui, container, egui::vec2(190.0, 22.0));
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 2.0;
        for (label, id) in [("Local", "local"), ("Player", "player"), ("World", "world"), ("Laser", "laser")] {
            changed |= ui.selectable_value(&mut mode, id.to_string(), egui::RichText::new(label)).changed();
        }
    });
    if changed {
        if let Some(node) = snarl.get_node_mut(inner_id) {
            node.params.insert("mode".into(), Value::String(mode));
        }
    }
}

fn render_gyro_invert_row(inner_id: NodeId, ui: &mut egui::Ui, snarl: &mut Snarl<NodeData>, container: egui::Vec2) {
    let (mut yaw, mut pitch, mut roll) = snarl.get_node(inner_id).map(|n| {
        (
            n.params.get("inv_yaw").and_then(|v| v.as_bool()).unwrap_or(false),
            n.params.get("inv_pitch").and_then(|v| v.as_bool()).unwrap_or(false),
            n.params.get("inv_roll").and_then(|v| v.as_bool()).unwrap_or(false),
        )
    }).unwrap_or_default();
    let mut changed = false;
    ui.set_max_width(container.x);
    apply_widget_scale(ui, container, egui::vec2(180.0, 22.0));
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 3.0;
        ui.label(egui::RichText::new("Gyr:").weak());
        changed |= ui.checkbox(&mut yaw,   egui::RichText::new("yaw")).changed();
        changed |= ui.checkbox(&mut pitch, egui::RichText::new("pitch")).changed();
        changed |= ui.checkbox(&mut roll,  egui::RichText::new("roll")).changed();
    });
    if changed {
        if let Some(node) = snarl.get_node_mut(inner_id) {
            node.params.insert("inv_yaw".into(),   Value::Bool(yaw));
            node.params.insert("inv_pitch".into(), Value::Bool(pitch));
            node.params.insert("inv_roll".into(),  Value::Bool(roll));
        }
    }
}

fn render_accel_invert_row(inner_id: NodeId, ui: &mut egui::Ui, snarl: &mut Snarl<NodeData>, container: egui::Vec2) {
    let (mut x, mut y, mut z) = snarl.get_node(inner_id).map(|n| {
        (
            n.params.get("inv_accel_x").and_then(|v| v.as_bool()).unwrap_or(false),
            n.params.get("inv_accel_y").and_then(|v| v.as_bool()).unwrap_or(false),
            n.params.get("inv_accel_z").and_then(|v| v.as_bool()).unwrap_or(false),
        )
    }).unwrap_or_default();
    let mut changed = false;
    ui.set_max_width(container.x);
    apply_widget_scale(ui, container, egui::vec2(160.0, 22.0));
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 3.0;
        ui.label(egui::RichText::new("Acc:").weak());
        changed |= ui.checkbox(&mut x, egui::RichText::new("X")).changed();
        changed |= ui.checkbox(&mut y, egui::RichText::new("Y")).changed();
        changed |= ui.checkbox(&mut z, egui::RichText::new("+Z")).changed();
    });
    if changed {
        if let Some(node) = snarl.get_node_mut(inner_id) {
            node.params.insert("inv_accel_x".into(), Value::Bool(x));
            node.params.insert("inv_accel_y".into(), Value::Bool(y));
            node.params.insert("inv_accel_z".into(), Value::Bool(z));
        }
    }
}

/// Scales the current Ui's font and interact metrics so the inner widgets
/// visually grow/shrink with the container, instead of staying tiny while only
/// the surrounding "crop area" gets bigger. Returns the chosen scale so the
/// caller can apply it to fixed-width helpers (e.g. spacing, separators).
///
/// `natural` is the size the row was authored for (typically the size captured
/// at pin time / the rect this element occupies on the original module body).
fn apply_widget_scale(ui: &mut egui::Ui, container: egui::Vec2, natural: egui::Vec2) -> f32 {
    let sx = (container.x / natural.x.max(1.0)).clamp(0.5, 4.0);
    let sy = (container.y / natural.y.max(1.0)).clamp(0.5, 4.0);
    let scale = sx.min(sy).clamp(0.5, 4.0);
    if (scale - 1.0).abs() < 0.02 { return 1.0; }

    // Scale all named text styles uniformly so labels, buttons, and DragValues
    // all grow together. Egui clones the style on edit, so this only affects
    // the current sub-Ui (the allocate_ui_at_rect closure), not the parent.
    let style = ui.style_mut();
    for (_, font_id) in style.text_styles.iter_mut() {
        font_id.size = (font_id.size * scale).max(6.0);
    }
    let sp = &mut style.spacing;
    sp.button_padding *= scale;
    sp.item_spacing   *= scale;
    sp.interact_size.y = (sp.interact_size.y * scale).max(12.0);
    sp.icon_width      = (sp.icon_width * scale).max(8.0);
    sp.icon_width_inner = (sp.icon_width_inner * scale).max(6.0);
    scale
}

// ── Average / Delay / DC Filter ───────────────────────────────────────────────

fn render_dragvalue_param(
    inner_id: NodeId,
    ui: &mut egui::Ui,
    snarl: &mut Snarl<NodeData>,
    container: egui::Vec2,
    label: &str,
    param: &str,
    default: f32,
    speed: f64,
    range: std::ops::RangeInclusive<f32>,
    max_decimals: Option<usize>,
) {
    let cur = snarl.get_node(inner_id)
        .and_then(|n| n.params.get(param).and_then(|v| v.as_f64()))
        .unwrap_or(default as f64) as f32;
    let mut v = cur;
    ui.set_max_width(container.x);
    apply_widget_scale(ui, container, egui::vec2(160.0, 22.0));
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new(label).weak());
        let mut dv = egui::DragValue::new(&mut v).speed(speed).range(range);
        if let Some(d) = max_decimals { dv = dv.max_decimals(d); }
        let label_w = ui.available_width() * 0.0; // available_width is now post-label
        let _ = label_w;
        let avail_w = ui.available_width().max(40.0);
        let h = container.y.max(ui.spacing().interact_size.y);
        if ui.add_sized([avail_w, h], dv).changed() {
            if let (Some(node), Some(n)) = (
                snarl.get_node_mut(inner_id),
                Number::from_f64(v as f64),
            ) {
                node.params.insert(param.into(), Value::Number(n));
            }
        }
    });
}

// ── Counter ───────────────────────────────────────────────────────────────────

fn render_counter_mode(inner_id: NodeId, ui: &mut egui::Ui, snarl: &mut Snarl<NodeData>, container: egui::Vec2) {
    let mut mode = snarl.get_node(inner_id)
        .and_then(|n| n.params.get("mode").and_then(|v| v.as_str()).map(|s| s.to_string()))
        .unwrap_or_else(|| "loop".to_string());
    let mut changed = false;
    ui.set_max_width(container.x);
    apply_widget_scale(ui, container, egui::vec2(220.0, 22.0));
    ui.horizontal(|ui| {
        for (lbl, id) in [("Loop", "loop"), ("Limit", "limit"), ("Bounce", "bounce"), ("Unlimited", "unlimited")] {
            changed |= ui.selectable_value(&mut mode, id.to_string(), egui::RichText::new(lbl)).changed();
        }
    });
    if changed {
        if let Some(node) = snarl.get_node_mut(inner_id) {
            node.params.insert("mode".into(), Value::String(mode));
        }
    }
}

fn render_counter_range_mode(inner_id: NodeId, ui: &mut egui::Ui, snarl: &mut Snarl<NodeData>, container: egui::Vec2) {
    let mut normalized = snarl.get_node(inner_id)
        .and_then(|n| n.params.get("normalized").and_then(|v| v.as_bool()))
        .unwrap_or(false);
    let mut changed = false;
    ui.set_max_width(container.x);
    apply_widget_scale(ui, container, egui::vec2(140.0, 22.0));
    ui.horizontal(|ui| {
        changed |= ui.selectable_value(&mut normalized, false, egui::RichText::new("Raw")).changed();
        changed |= ui.selectable_value(&mut normalized, true,  egui::RichText::new("0..1")).changed();
        if ui.small_button("↺").on_hover_text("Reset counter").clicked() {
            if let Some(node) = snarl.get_node_mut(inner_id) {
                while node.extra.aux_f32.len() < 2 { node.extra.aux_f32.push(0.0); }
                node.extra.aux_f32[0] = 0.0;
                node.extra.aux_f32[1] = 1.0;
                node.extra.aux_f32_dirty = true;
            }
        }
    });
    if changed {
        if let Some(node) = snarl.get_node_mut(inner_id) {
            node.params.insert("normalized".into(), Value::Bool(normalized));
        }
    }
}

fn render_counter_step(inner_id: NodeId, ui: &mut egui::Ui, snarl: &mut Snarl<NodeData>, container: egui::Vec2) {
    let mut step = snarl.get_node(inner_id)
        .and_then(|n| n.params.get("step_param").and_then(|v| v.as_f64()))
        .unwrap_or(1.0) as f32;
    ui.set_max_width(container.x);
    apply_widget_scale(ui, container, egui::vec2(120.0, 22.0));
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new("Step").weak());
        if ui.add(egui::DragValue::new(&mut step).speed(0.1).range(0.001..=10000.0)).changed() {
            if let (Some(node), Some(n)) = (snarl.get_node_mut(inner_id), Number::from_f64(step as f64)) {
                node.params.insert("step_param".into(), Value::Number(n));
            }
        }
    });
}

fn render_counter_min_max(inner_id: NodeId, ui: &mut egui::Ui, snarl: &mut Snarl<NodeData>, container: egui::Vec2) {
    let (mut min_p, mut max_p, mode) = snarl.get_node(inner_id).map(|n| {
        let mn = n.params.get("min_param").and_then(|v| v.as_f64()).unwrap_or(0.0) as f32;
        let mx = n.params.get("max_param").and_then(|v| v.as_f64()).unwrap_or(10.0) as f32;
        let md = n.params.get("mode").and_then(|v| v.as_str()).unwrap_or("loop").to_string();
        (mn, mx, md)
    }).unwrap_or((0.0, 10.0, "loop".to_string()));
    let mut changed = false;
    ui.set_max_width(container.x);
    apply_widget_scale(ui, container, egui::vec2(180.0, 22.0));
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new("Min").weak());
        changed |= ui.add(egui::DragValue::new(&mut min_p).speed(0.1)).changed();
        ui.label(egui::RichText::new("Max").weak());
        changed |= ui.add_enabled(mode != "unlimited", egui::DragValue::new(&mut max_p).speed(0.1)).changed();
    });
    if changed {
        if let Some(node) = snarl.get_node_mut(inner_id) {
            if let Some(n) = Number::from_f64(min_p as f64) { node.params.insert("min_param".into(), Value::Number(n)); }
            if let Some(n) = Number::from_f64(max_p as f64) { node.params.insert("max_param".into(), Value::Number(n)); }
        }
    }
}

// ── Logic Delay ───────────────────────────────────────────────────────────────

fn render_logic_delay_mode(inner_id: NodeId, ui: &mut egui::Ui, snarl: &mut Snarl<NodeData>, container: egui::Vec2) {
    let mut mode = snarl.get_node(inner_id)
        .and_then(|n| n.params.get("mode").and_then(|v| v.as_str()).map(|s| s.to_string()))
        .unwrap_or_else(|| "delay_false".to_string());
    let mut changed = false;
    ui.set_max_width(container.x);
    apply_widget_scale(ui, container, egui::vec2(180.0, 22.0));
    ui.horizontal(|ui| {
        changed |= ui.selectable_value(&mut mode, "delay_true".into(),  egui::RichText::new("Delay ON")).changed();
        changed |= ui.selectable_value(&mut mode, "delay_false".into(), egui::RichText::new("Delay OFF")).changed();
    });
    if changed {
        if let Some(node) = snarl.get_node_mut(inner_id) {
            node.params.insert("mode".into(), Value::String(mode));
        }
    }
}

fn render_logic_delay_time(inner_id: NodeId, ui: &mut egui::Ui, snarl: &mut Snarl<NodeData>, container: egui::Vec2) {
    let (mut time, mut unit) = snarl.get_node(inner_id).map(|n| {
        let t = n.params.get("time").and_then(|v| v.as_f64()).unwrap_or(100.0) as f32;
        let u = n.params.get("unit").and_then(|v| v.as_str()).unwrap_or("ms").to_string();
        (t, u)
    }).unwrap_or((100.0, "ms".to_string()));
    let mut changed = false;
    ui.set_max_width(container.x);
    apply_widget_scale(ui, container, egui::vec2(200.0, 22.0));
    ui.horizontal(|ui| {
        let limit = if unit == "ms" { 60_000.0 } else { 10_000.0 };
        changed |= ui.add(egui::DragValue::new(&mut time).speed(1.0).range(0.0..=limit)).changed();
        changed |= ui.selectable_value(&mut unit, "ms".into(),      egui::RichText::new("ms")).changed();
        changed |= ui.selectable_value(&mut unit, "samples".into(), egui::RichText::new("frames")).changed();
    });
    if changed {
        if let Some(node) = snarl.get_node_mut(inner_id) {
            node.params.insert("unit".into(), Value::String(unit));
            if let Some(n) = Number::from_f64(time as f64) { node.params.insert("time".into(), Value::Number(n)); }
        }
    }
}

// ── Oscillator ────────────────────────────────────────────────────────────────

fn render_oscillator_shape(inner_id: NodeId, ui: &mut egui::Ui, snarl: &mut Snarl<NodeData>, container: egui::Vec2) {
    let mut shape = snarl.get_node(inner_id)
        .and_then(|n| n.params.get("shape").and_then(|v| v.as_str()).map(|s| s.to_string()))
        .unwrap_or_else(|| "sine".to_string());
    let mut changed = false;
    ui.set_max_width(container.x);
    apply_widget_scale(ui, container, egui::vec2(180.0, 22.0));
    ui.horizontal(|ui| {
        changed |= ui.selectable_value(&mut shape, "sine".into(),     egui::RichText::new("Sine")).changed();
        changed |= ui.selectable_value(&mut shape, "triangle".into(), egui::RichText::new("Tri")).changed();
        changed |= ui.selectable_value(&mut shape, "saw".into(),      egui::RichText::new("Saw")).changed();
        changed |= ui.selectable_value(&mut shape, "square".into(),   egui::RichText::new("Sqr")).changed();
    });
    if changed {
        if let Some(node) = snarl.get_node_mut(inner_id) {
            node.params.insert("shape".into(), Value::String(shape));
        }
    }
}

fn render_oscillator_freq(inner_id: NodeId, ui: &mut egui::Ui, snarl: &mut Snarl<NodeData>, container: egui::Vec2) {
    let (mut freq_unit, mut freq_p) = snarl.get_node(inner_id).map(|n| {
        let u = n.params.get("freq_unit").and_then(|v| v.as_str()).unwrap_or("hz").to_string();
        let f = n.params.get("freq_param").and_then(|v| v.as_f64()).unwrap_or(1.0) as f32;
        (u, f)
    }).unwrap_or(("hz".to_string(), 1.0));
    let mut changed = false;
    ui.set_max_width(container.x);
    apply_widget_scale(ui, container, egui::vec2(180.0, 22.0));
    ui.horizontal(|ui| {
        changed |= ui.selectable_value(&mut freq_unit, "hz".into(), egui::RichText::new("Hz")).changed();
        changed |= ui.selectable_value(&mut freq_unit, "ms".into(), egui::RichText::new("ms")).changed();
        let (lo, hi, spd) = if freq_unit == "hz" { (0.01, 200.0, 0.1) } else { (1.0, 60_000.0, 10.0) };
        changed |= ui.add(egui::DragValue::new(&mut freq_p).speed(spd).range(lo..=hi)).changed();
    });
    if changed {
        if let Some(node) = snarl.get_node_mut(inner_id) {
            node.params.insert("freq_unit".into(), Value::String(freq_unit));
            if let Some(n) = Number::from_f64(freq_p as f64) { node.params.insert("freq_param".into(), Value::Number(n)); }
        }
    }
}

fn render_oscillator_phase(inner_id: NodeId, ui: &mut egui::Ui, snarl: &mut Snarl<NodeData>, container: egui::Vec2) {
    let (mut phase_p, mut bipolar) = snarl.get_node(inner_id).map(|n| {
        let p = n.params.get("phase_param").and_then(|v| v.as_f64()).unwrap_or(0.0) as f32;
        let b = n.params.get("bipolar").and_then(|v| v.as_bool()).unwrap_or(true);
        (p, b)
    }).unwrap_or((0.0, true));
    let mut changed = false;
    ui.set_max_width(container.x);
    apply_widget_scale(ui, container, egui::vec2(220.0, 22.0));
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new("Phase").weak());
        changed |= ui.add(egui::DragValue::new(&mut phase_p).speed(0.01).range(0.0..=1.0)).changed();
        ui.separator();
        changed |= ui.selectable_value(&mut bipolar, true,  egui::RichText::new("Bi")).changed();
        changed |= ui.selectable_value(&mut bipolar, false, egui::RichText::new("Uni")).changed();
    });
    if changed {
        if let Some(node) = snarl.get_node_mut(inner_id) {
            node.params.insert("bipolar".into(), Value::Bool(bipolar));
            if let Some(n) = Number::from_f64(phase_p as f64) { node.params.insert("phase_param".into(), Value::Number(n)); }
        }
    }
}

fn render_oscillator_preview(
    inner_id: NodeId,
    ui: &mut egui::Ui,
    snarl: &mut Snarl<NodeData>,
    container: egui::Vec2,
) {
    let (shape, phase_p, bipolar) = snarl.get_node(inner_id).map(|n| {
        let s = n.params.get("shape").and_then(|v| v.as_str()).unwrap_or("sine").to_string();
        let p = n.params.get("phase_param").and_then(|v| v.as_f64()).unwrap_or(0.0) as f32;
        let b = n.params.get("bipolar").and_then(|v| v.as_bool()).unwrap_or(true);
        (s, p, b)
    }).unwrap_or(("sine".to_string(), 0.0, true));

    let avail = egui::vec2(container.x.max(40.0), container.y.max(20.0));
    let (rect, _) = ui.allocate_exact_size(avail, egui::Sense::hover());
    if !ui.is_rect_visible(rect) { return; }
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 2.0, egui::Color32::from_gray(18));
    let zero_y = if bipolar { rect.center().y } else { rect.bottom() };
    painter.line_segment(
        [egui::pos2(rect.left(), zero_y), egui::pos2(rect.right(), zero_y)],
        egui::Stroke::new(0.5, egui::Color32::from_gray(55)),
    );
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

// ── Readout ───────────────────────────────────────────────────────────────────

fn render_readout_value(
    inner_id: NodeId,
    ui: &mut egui::Ui,
    snarl: &mut Snarl<NodeData>,
    container: egui::Vec2,
) {
    let sig = snarl.get_node(inner_id)
        .and_then(|n| n.extra.last_signals.first().copied().flatten());
    let text = match sig {
        Some(Signal::Float(f)) => format!("{f:.4}"),
        Some(Signal::Bool(b))  => if b { "true".into() } else { "false".into() },
        Some(Signal::Vec2(v))  => format!("({:.3}, {:.3})", v.x, v.y),
        Some(Signal::Int(i))   => format!("{i}"),
        None                   => "—".into(),
    };
    let font = container.y.clamp(10.0, 64.0) * 0.55;
    ui.add_sized(
        [container.x, container.y.max(18.0)],
        egui::Label::new(egui::RichText::new(text).monospace().size(font)),
    );
}

// ── Oscilloscope / Vectorscope ────────────────────────────────────────────────

fn render_oscilloscope_display(
    inner_id: NodeId,
    ui: &mut egui::Ui,
    snarl: &mut Snarl<NodeData>,
    container: egui::Vec2,
) {
    let (history, n_channels, win_ms, osc_scale, osc_auto, osc_uni) = snarl.get_node(inner_id).map(|n| {
        let win = n.params.get("osc_win_ms").and_then(|v| v.as_f64()).unwrap_or(200.0).clamp(10.0, 10_000.0) as f32;
        let sc  = n.params.get("osc_scale") .and_then(|v| v.as_f64()).unwrap_or(1.0).max(0.001) as f32;
        let au  = n.params.get("osc_auto")  .and_then(|v| v.as_bool()).unwrap_or(false);
        let uni = n.params.get("osc_uni")   .and_then(|v| v.as_bool()).unwrap_or(false);
        (n.extra.history.clone(), n.inputs.len().max(1), win, sc, au, uni)
    }).unwrap_or_default();

    let osc_win = (win_ms / 1000.0 * SAMPLE_RATE as f32) as usize;
    let n_total = history.len();
    let start   = n_total.saturating_sub(osc_win);
    let visible: Vec<Vec<Option<f32>>> = history.iter().skip(start).cloned().collect();
    let n = visible.len();

    let eff_scale = if osc_auto {
        let max_v = visible.iter()
            .flat_map(|s| s.iter().filter_map(|v| *v))
            .map(|v: f32| v.abs())
            .fold(0.0f32, f32::max);
        if max_v > 0.0 { max_v } else { 1.0 }
    } else { osc_scale };

    let avail = egui::vec2(container.x.max(40.0), container.y.max(24.0));
    let (rect, _) = ui.allocate_exact_size(avail, egui::Sense::hover());
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 2.0, Color32::from_gray(16));

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
    if osc_uni {
        painter.line_segment(
            [egui::pos2(rect.left(), rect.bottom()), egui::pos2(rect.right(), rect.bottom())],
            egui::Stroke::new(1.0, Color32::from_gray(55)),
        );
    }

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
    ui.ctx().request_repaint();
    let _ = inner_id;
}

/// Bare oscilloscope controls row: Win slider + Scale (with Auto fallback) +
/// Bi/Uni selector. Same controls as the editor body but as a free widget.
fn render_oscilloscope_controls(
    inner_id: NodeId,
    ui: &mut egui::Ui,
    snarl: &mut Snarl<NodeData>,
    container: egui::Vec2,
) {
    let (mut win_ms, mut sc, mut au, mut uni, eff_scale) = snarl.get_node(inner_id).map(|n| {
        let win = n.params.get("osc_win_ms").and_then(|v| v.as_f64()).unwrap_or(200.0).clamp(10.0, 10_000.0) as f32;
        let s   = n.params.get("osc_scale") .and_then(|v| v.as_f64()).unwrap_or(1.0).max(0.001) as f32;
        let a   = n.params.get("osc_auto")  .and_then(|v| v.as_bool()).unwrap_or(false);
        let u   = n.params.get("osc_uni")   .and_then(|v| v.as_bool()).unwrap_or(false);
        (win, s, a, u, s)
    }).unwrap_or((200.0, 1.0, false, false, 1.0));

    let mut changed = false;
    ui.set_max_width(container.x);
    apply_widget_scale(ui, container, egui::vec2(360.0, 22.0));
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new("Win").weak());
        changed |= ui.add(egui::Slider::new(&mut win_ms, 10.0f32..=10_000.0)
            .logarithmic(true).show_value(false)).changed();
        let lbl = if win_ms >= 1000.0 { format!("{:.1}s", win_ms / 1000.0) } else { format!("{:.0}ms", win_ms) };
        ui.label(egui::RichText::new(lbl).weak());
        ui.separator();
        ui.label(egui::RichText::new("Scale").weak());
        if au {
            ui.label(egui::RichText::new(format!("{:.3}", eff_scale)).weak());
        } else {
            changed |= ui.add(egui::DragValue::new(&mut sc).speed(0.01)
                .range(0.001f32..=100.0).max_decimals(3)).changed();
        }
        let was_au = au;
        ui.checkbox(&mut au, egui::RichText::new("Auto"));
        changed |= au != was_au;
        ui.separator();
        let was_uni = uni;
        ui.selectable_value(&mut uni, false, egui::RichText::new("Bi"));
        ui.selectable_value(&mut uni, true,  egui::RichText::new("Uni"));
        changed |= uni != was_uni;
    });
    if changed {
        if let Some(node) = snarl.get_node_mut(inner_id) {
            if let Some(n) = Number::from_f64(win_ms as f64) { node.params.insert("osc_win_ms".into(), Value::Number(n)); }
            if let Some(n) = Number::from_f64(sc as f64)     { node.params.insert("osc_scale".into(),  Value::Number(n)); }
            node.params.insert("osc_auto".into(), Value::Bool(au));
            node.params.insert("osc_uni".into(),  Value::Bool(uni));
        }
    }
}

fn render_vectorscope_display(
    inner_id: NodeId,
    ui: &mut egui::Ui,
    snarl: &mut Snarl<NodeData>,
    container: egui::Vec2,
) {
    let (history, n_channels, last_signals) = snarl.get_node(inner_id)
        .map(|n| (n.extra.history.clone(), n.inputs.len().max(1), n.extra.last_signals.clone()))
        .unwrap_or_default();

    let side = container.x.min(container.y).max(40.0);
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
        for (idx, sample) in trail.iter().enumerate() {
            let (Some(x), Some(y)) = (
                sample.get(xi).copied().flatten(),
                sample.get(yi).copied().flatten(),
            ) else { continue; };
            let px = rect.center().x + x.clamp(-1.0, 1.0) * rect.width() * 0.45;
            let py = rect.center().y - y.clamp(-1.0, 1.0) * rect.height() * 0.45;
            let alpha = ((idx as f32 / nt.max(1) as f32) * 200.0) as u8 + 35;
            painter.circle_filled(egui::pos2(px, py), 1.5,
                Color32::from_rgba_unmultiplied(col.r(), col.g(), col.b(), alpha));
        }
        if let Some(Some(Signal::Vec2(v))) = last_signals.get(ch) {
            let px = rect.center().x + v.x.clamp(-1.0, 1.0) * rect.width() * 0.45;
            let py = rect.center().y - v.y.clamp(-1.0, 1.0) * rect.height() * 0.45;
            painter.circle_filled(egui::pos2(px, py), 4.0, col);
            painter.circle_stroke(egui::pos2(px, py), 4.0,
                egui::Stroke::new(1.0, Color32::from_gray(100)));
        }
    }
    ui.ctx().request_repaint();
    let _ = inner_id;
}

/// Renders the response curve graph as a bare widget filling `container`.
/// No surrounding sliders, buttons, channel +/-, etc. — just the graph,
/// fully interactive (drag points, alt-bias, dbl-click add, right-click remove).
/// Sized exactly to the user-allocated rect from the sub-patch layout.
fn render_response_curve_only(
    inner_id: NodeId,
    ui: &mut egui::Ui,
    inner_snarl: &mut Snarl<NodeData>,
    container: egui::Vec2,
    is_vec: bool,
) {
    let avail = egui::vec2(container.x.max(20.0), container.y.max(20.0));
    let (rect, bg_resp) = ui.allocate_exact_size(avail, egui::Sense::click());
    paint_response_curve_graph(inner_id, ui, inner_snarl, rect, bg_resp, is_vec);
}

/// Bare Log-Exp slider + Abs (only for non-vec) + Snap.
fn render_response_curve_scale_row(
    inner_id: NodeId,
    ui: &mut egui::Ui,
    snarl: &mut Snarl<NodeData>,
    container: egui::Vec2,
    is_vec: bool,
) {
    let (mut sc_t, mut absolute, mut snap_on) = snarl.get_node(inner_id).map(|n| {
        let s = n.params.get("scale_t").and_then(|v| v.as_f64()).unwrap_or(0.0) as f32;
        let a = if is_vec { true } else { n.params.get("absolute").and_then(|v| v.as_bool()).unwrap_or(true) };
        let sn = n.params.get("snap").and_then(|v| v.as_bool()).unwrap_or(false);
        (s, a, sn)
    }).unwrap_or((0.0, true, false));

    ui.set_max_width(container.x);
    apply_widget_scale(ui, container, egui::vec2(220.0, 22.0));
    let mut changed = false;
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new("Log").weak());
        // Slider takes a portion of available width so it scales with the row.
        let slider_w = (ui.available_width() * 0.45).clamp(40.0, 200.0);
        let slider_h = ui.spacing().interact_size.y.min(20.0).max(10.0);
        let (slider_rect, slider_resp) =
            ui.allocate_exact_size(egui::vec2(slider_w, slider_h), egui::Sense::click_and_drag());
        if slider_resp.double_clicked() { sc_t = 0.0; changed = true; }
        else if slider_resp.dragged() {
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
            egui::pos2(knob_x, slider_rect.center().y),
            (slider_rect.height() * 0.35).max(3.0),
            if slider_resp.hovered() || slider_resp.dragged() { Color32::WHITE } else { Color32::from_gray(190) },
        );
        ui.label(egui::RichText::new("Exp").weak());
        ui.separator();
        if !is_vec {
            let was = absolute;
            ui.checkbox(&mut absolute, egui::RichText::new("Abs"));
            changed |= absolute != was;
        }
        let was = snap_on;
        ui.checkbox(&mut snap_on, egui::RichText::new("Snap"));
        changed |= snap_on != was;
    });
    if changed {
        if let Some(node) = snarl.get_node_mut(inner_id) {
            if let Some(n) = Number::from_f64(sc_t as f64) { node.params.insert("scale_t".into(), Value::Number(n)); }
            if !is_vec { node.params.insert("absolute".into(), Value::Bool(absolute)); }
            node.params.insert("snap".into(), Value::Bool(snap_on));
        }
    }
}

/// Bare In/Out range row. For non-vec curves: in_min, in_max, out_min, out_max.
/// For vec curves: in_max, out_max (vec curves are always [0,1]).
fn render_response_curve_range_row(
    inner_id: NodeId,
    ui: &mut egui::Ui,
    snarl: &mut Snarl<NodeData>,
    container: egui::Vec2,
    is_vec: bool,
) {
    ui.set_max_width(container.x);
    apply_widget_scale(ui, container, egui::vec2(220.0, 22.0));
    let mut changed = false;
    if is_vec {
        let (mut i_max, mut o_max) = snarl.get_node(inner_id).map(|n| {
            let i = n.params.get("in_max").and_then(|v| v.as_f64()).unwrap_or(1.0) as f32;
            let o = n.params.get("out_max").and_then(|v| v.as_f64()).unwrap_or(1.0) as f32;
            (i, o)
        }).unwrap_or((1.0, 1.0));
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("In max").weak());
            changed |= ui.add(egui::DragValue::new(&mut i_max).speed(0.01).max_decimals(2)).changed();
            ui.separator();
            ui.label(egui::RichText::new("Out max").weak());
            changed |= ui.add(egui::DragValue::new(&mut o_max).speed(0.01).max_decimals(2)).changed();
        });
        if changed {
            if let Some(node) = snarl.get_node_mut(inner_id) {
                if let Some(n) = Number::from_f64(i_max as f64) { node.params.insert("in_max".into(),  Value::Number(n)); }
                if let Some(n) = Number::from_f64(o_max as f64) { node.params.insert("out_max".into(), Value::Number(n)); }
            }
        }
    } else {
        let (mut i0, mut i1, mut o0, mut o1) = snarl.get_node(inner_id).map(|n| {
            let i0 = n.params.get("in_min").and_then(|v| v.as_f64()).unwrap_or(-1.0) as f32;
            let i1 = n.params.get("in_max").and_then(|v| v.as_f64()).unwrap_or( 1.0) as f32;
            let o0 = n.params.get("out_min").and_then(|v| v.as_f64()).unwrap_or(-1.0) as f32;
            let o1 = n.params.get("out_max").and_then(|v| v.as_f64()).unwrap_or( 1.0) as f32;
            (i0, i1, o0, o1)
        }).unwrap_or((-1.0, 1.0, -1.0, 1.0));
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("In").weak());
            changed |= ui.add(egui::DragValue::new(&mut i0).speed(0.01).prefix("↓").max_decimals(2)).changed();
            changed |= ui.add(egui::DragValue::new(&mut i1).speed(0.01).prefix("↑").max_decimals(2)).changed();
            ui.separator();
            ui.label(egui::RichText::new("Out").weak());
            changed |= ui.add(egui::DragValue::new(&mut o0).speed(0.01).prefix("↓").max_decimals(2)).changed();
            changed |= ui.add(egui::DragValue::new(&mut o1).speed(0.01).prefix("↑").max_decimals(2)).changed();
        });
        if changed {
            if let Some(node) = snarl.get_node_mut(inner_id) {
                for (k, v) in [
                    ("in_min", i0 as f64), ("in_max", i1 as f64),
                    ("out_min", o0 as f64), ("out_max", o1 as f64),
                ] {
                    if let Some(n) = Number::from_f64(v) { node.params.insert(k.into(), Value::Number(n)); }
                }
            }
        }
    }
}

/// Bare Grid + Trail row.
fn render_response_curve_grid_row(
    inner_id: NodeId,
    ui: &mut egui::Ui,
    snarl: &mut Snarl<NodeData>,
    container: egui::Vec2,
) {
    let (mut gx, mut gy, mut tm) = snarl.get_node(inner_id).map(|n| {
        let gx = n.params.get("grid_x").and_then(|v| v.as_i64()).unwrap_or(4) as f64;
        let gy = n.params.get("grid_y").and_then(|v| v.as_i64()).unwrap_or(4) as f64;
        let tm = n.params.get("trail_ms").and_then(|v| v.as_i64()).unwrap_or(300);
        (gx, gy, tm)
    }).unwrap_or((4.0, 4.0, 300));
    ui.set_max_width(container.x);
    apply_widget_scale(ui, container, egui::vec2(220.0, 22.0));
    let mut changed = false;
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new("Grid").weak());
        changed |= ui.add(egui::DragValue::new(&mut gx).speed(0.25)
            .range(1.0..=20.0).max_decimals(0).prefix("H ")).changed();
        changed |= ui.add(egui::DragValue::new(&mut gy).speed(0.25)
            .range(1.0..=20.0).max_decimals(0).prefix("V ")).changed();
        ui.separator();
        ui.label(egui::RichText::new("Trail").weak());
        changed |= ui.add(egui::DragValue::new(&mut tm).speed(5.0)
            .range(0i64..=1000).suffix("ms")).changed();
    });
    if changed {
        if let Some(node) = snarl.get_node_mut(inner_id) {
            node.params.insert("grid_x".into(),   serde_json::json!(gx as i64));
            node.params.insert("grid_y".into(),   serde_json::json!(gy as i64));
            node.params.insert("trail_ms".into(), serde_json::json!(tm));
        }
    }
}

/// Paints just the response-curve graph (background + grid + curve + control
/// points + bias handles + live-input trails) into `rect`, and writes back any
/// param changes made via interaction. Shared between the in-editor body
/// renderer and the bare layout-pinned renderer on the sub-patch face.
fn paint_response_curve_graph(
    node_id: NodeId,
    ui: &mut egui::Ui,
    snarl: &mut Snarl<NodeData>,
    rect: egui::Rect,
    bg_resp: egui::Response,
    is_vec: bool,
) {
    // Initialise params on first use (kept consistent with the body renderers).
    let needs_init = snarl.get_node(node_id).map(|n| !n.params.contains_key("points")).unwrap_or(false);
    if needs_init {
        if let Some(node) = snarl.get_node_mut(node_id) {
            node.params.insert("points".into(), serde_json::json!([[0.0, 0.0], [1.0, 1.0]]));
            node.params.insert("biases".into(), serde_json::json!([0.0]));
            if !is_vec {
                node.params.insert("absolute".into(), Value::Bool(true));
                node.params.insert("in_min".into(),   serde_json::json!(-1.0));
                node.params.insert("out_min".into(),  serde_json::json!(-1.0));
            }
            node.params.insert("in_max".into(),   serde_json::json!(1.0f64));
            node.params.insert("out_max".into(),  serde_json::json!(1.0f64));
            node.params.insert("grid_x".into(),   serde_json::json!(4i64));
            node.params.insert("grid_y".into(),   serde_json::json!(4i64));
            node.params.insert("snap".into(),     Value::Bool(false));
            node.params.insert("scale_t".into(),  serde_json::json!(0.0f64));
            node.params.insert("trail_ms".into(), serde_json::json!(300i64));
        }
    }

    // Read params.
    let (points, biases, absolute, in_min, in_max, grid_x, grid_y, snap, scale_t, trail_ms) = snarl
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
            let abs  = if is_vec { true } else { n.params.get("absolute").and_then(|v| v.as_bool()).unwrap_or(true) };
            let i0   = if is_vec { 0.0 } else { n.params.get("in_min") .and_then(|v| v.as_f64()).unwrap_or(-1.0) as f32 };
            let i1   = n.params.get("in_max").and_then(|v| v.as_f64()).unwrap_or(1.0) as f32;
            let gx   = n.params.get("grid_x").and_then(|v| v.as_i64()).unwrap_or(4).max(1) as usize;
            let gy   = n.params.get("grid_y").and_then(|v| v.as_i64()).unwrap_or(4).max(1) as usize;
            let sn   = n.params.get("snap").and_then(|v| v.as_bool()).unwrap_or(false);
            let sc   = n.params.get("scale_t").and_then(|v| v.as_f64()).map(|f| f as f32).unwrap_or(0.0);
            let tm   = n.params.get("trail_ms").and_then(|v| v.as_i64()).unwrap_or(300).clamp(0, 1000);
            (pts, bss, abs, i0, i1, gx, gy, sn, sc, tm)
        })
        .unwrap_or_else(|| (vec![[0.0, 0.0], [1.0, 1.0]], vec![], true, -1.0, 1.0, 4, 4, false, 0.0f32, 300));

    let n_channels = snarl.get_node(node_id)
        .map(|n| n.inputs.len().min(n.outputs.len()))
        .unwrap_or(1).max(1);
    let live_inputs: Vec<Option<f32>> = (0..n_channels)
        .map(|ch| snarl.get_node(node_id)
            .and_then(|n| n.extra.last_signals.get(ch)?.as_ref())
            .map(sig_f32))
        .collect();

    let (x_lo, x_hi): (f32, f32) = if absolute { (0.0, 1.0) } else { (-1.0, 1.0) };
    let (y_lo, y_hi): (f32, f32) = if absolute { (0.0, 1.0) } else { (-1.0, 1.0) };
    let x_range = x_hi - x_lo;
    let y_range = y_hi - y_lo;

    let mut new_points  = points.clone();
    let mut new_biases  = biases.clone();
    let mut pts_changed  = false;
    let mut bias_changed = false;

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

    let bias_id_tag = if is_vec { "vbias_h_only" } else { "bias_h_only" };
    let pt_id_tag   = if is_vec { "vcpt_only" }    else { "cpt_only" };
    let trail_id_tag = if is_vec { "vtrail_only" } else { "trail_only" };

    let alt_held = ui.input(|i| i.modifiers.alt);
    if alt_held && new_points.len() >= 2 {
        while new_biases.len() < new_points.len() - 1 { new_biases.push(0.0); }
        for seg in 0..(new_points.len() - 1) {
            let mid_x = (new_points[seg][0] + new_points[seg + 1][0]) * 0.5;
            let mid_y = sample_curve(&new_points, mid_x, &new_biases).clamp(y_lo, y_hi);
            let hpos  = c2s(mid_x, mid_y);
            let hid   = ui.id().with((bias_id_tag, node_id, seg));
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
            painter.circle_stroke(hpos, 4.0, egui::Stroke::new(1.0, Color32::from_gray(100)));
        }
    }

    let mut remove_idx: Option<usize> = None;
    for i in 0..new_points.len() {
        let [px, py] = new_points[i];
        let screen   = c2s(px, py);
        let pt_id    = ui.id().with((pt_id_tag, node_id, i));
        let pt_resp  = ui.interact(
            egui::Rect::from_center_size(screen, egui::Vec2::splat(12.0)),
            pt_id, egui::Sense::click_and_drag());

        // Origin-anchored drag: stash the point's [x, y] at drag start and the
        // running pixel offset; each frame, target = origin + accumulated_px
        // mapped to curve coords, then snapped once. Without this, snapping
        // accumulates per-frame rounding and feels "drunk".
        let origin_id = ui.id().with(("crv_pt_origin", pt_id_tag, node_id, i));
        if pt_resp.drag_started() && !alt_held {
            ui.ctx().data_mut(|d| d.insert_temp(origin_id, [px, py, 0.0f32, 0.0f32]));
        }
        if pt_resp.dragged() && !alt_held {
            let prev = ui.ctx().data(|d| d.get_temp::<[f32;4]>(origin_id))
                .unwrap_or([px, py, 0.0, 0.0]);
            let dd  = pt_resp.drag_delta();
            let acc_x_px = prev[2] + dd.x;
            let acc_y_px = prev[3] + dd.y;
            ui.ctx().data_mut(|d| d.insert_temp(origin_id, [prev[0], prev[1], acc_x_px, acc_y_px]));
            let nx_raw = prev[0] + acc_x_px * x_range / rect.width();
            let ny_raw = prev[1] - acc_y_px * y_range / rect.height();
            let lo_x   = new_points.get(i.wrapping_sub(1)).map(|p| p[0] + 0.001).unwrap_or(x_lo);
            let hi_x   = new_points.get(i + 1).map(|p| p[0] - 0.001).unwrap_or(x_hi);
            let (sx, sy) = do_snap(nx_raw, ny_raw);
            new_points[i] = [sx.clamp(lo_x, hi_x), sy.clamp(y_lo, y_hi)];
            pts_changed = true;
        }
        if pt_resp.drag_stopped() {
            ui.ctx().data_mut(|d| d.remove_temp::<[f32;4]>(origin_id));
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

    // Live-position trails.
    let abs_max   = if is_vec { in_max.abs().max(f32::EPSILON) } else {
        in_max.abs().max(in_min.abs()).max(f32::EPSILON)
    };
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
        type Trail = std::collections::VecDeque<(f32, std::time::Instant)>;
        let trail_id = ui.id().with((trail_id_tag, node_id, ch as u32));
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

    // Write back curve points / biases.
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
}

// ── Layout mode (Pin element) overlay framework ───────────────────────────────
//
// When a sub-patch's editor window is in Layout mode, body renderers register
// hit-rects for each exposable UI element via `register_exposable_element`.
// The helper draws a tinted overlay and converts a click into a pin request,
// stashed in egui memory and read by `show_subpatch_editors` after render.
//
// Module-side renderers that haven't been instrumented yet simply don't
// register anything, so they're invisible to the layout selector.

const LAYOUT_ACTIVE_KEY:  &str = "fxi_layout_active";
const LAYOUT_PENDING_KEY: &str = "fxi_layout_pending";

pub fn set_layout_mode_active(ctx: &egui::Context, active: bool) {
    ctx.data_mut(|d| d.insert_temp(egui::Id::new(LAYOUT_ACTIVE_KEY), active));
}

pub fn layout_mode_active(ctx: &egui::Context) -> bool {
    ctx.data(|d| d.get_temp::<bool>(egui::Id::new(LAYOUT_ACTIVE_KEY))).unwrap_or(false)
}

/// Pin request returned to the editor. Includes the source-rect size so the
/// new ExposedModule starts at the same dimensions the element had on the
/// original module body, instead of an arbitrary default.
pub fn take_layout_pending(ctx: &egui::Context) -> Option<(usize, String, [f32; 2])> {
    ctx.data_mut(|d| d.remove_temp::<(usize, String, [f32; 2])>(egui::Id::new(LAYOUT_PENDING_KEY)))
}

/// Draws a tinted highlight + click target on `rect`. No-op when layout mode
/// is off, so callers can sprinkle it around without conditionals. The rect's
/// size is captured into the pin request so the new pinned widget inherits
/// the source element's dimensions.
fn register_exposable_element(
    ui: &mut egui::Ui,
    node_id: NodeId,
    element_id: &str,
    rect: egui::Rect,
) {
    if !layout_mode_active(ui.ctx()) { return; }
    if rect.area() < 4.0 { return; }
    let id = egui::Id::new(("fxi_layout_elem", node_id.0, element_id));
    let resp = ui.interact(rect, id, egui::Sense::click());
    let painter = ui.painter();
    let (fill, outline) = if resp.hovered() {
        (egui::Color32::from_rgba_unmultiplied(120, 200, 255, 90),
         egui::Color32::from_rgb(180, 230, 255))
    } else {
        (egui::Color32::from_rgba_unmultiplied(80, 160, 220, 35),
         egui::Color32::from_rgb(80, 160, 220))
    };
    painter.rect_filled(rect, 4.0, fill);
    let s = egui::Stroke::new(1.5, outline);
    painter.line_segment([rect.left_top(),     rect.right_top()],    s);
    painter.line_segment([rect.right_top(),    rect.right_bottom()], s);
    painter.line_segment([rect.right_bottom(), rect.left_bottom()],  s);
    painter.line_segment([rect.left_bottom(),  rect.left_top()],     s);
    if resp.clicked() {
        let size = [rect.width().max(40.0), rect.height().max(20.0)];
        ui.ctx().data_mut(|d| {
            d.insert_temp(
                egui::Id::new(LAYOUT_PENDING_KEY),
                (node_id.0, element_id.to_string(), size),
            );
        });
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
    is_inner_canvas: bool,
) {
    let mut categories: Vec<&str> = vec![];
    for d in descriptors {
        // Inlet/Outlet nodes (category "SubPatch") are only available inside sub-patch editors.
        if !is_inner_canvas && d.category == "SubPatch" { continue; }
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
    let resp = ui.add_sized(
        [120.0, 24.0],
        egui::Label::new(egui::RichText::new(text).monospace().size(14.0)),
    );
    register_exposable_element(ui, node_id, "value", resp.rect);
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

    let mut display_rect: Option<egui::Rect> = None;
    ui.vertical(|ui| {
        egui::Resize::default()
            .id_salt(("osc", node_id))
            .default_size([240.0, 100.0])
            .min_size([60.0, 30.0])
            .show(ui, |ui| {
                let (rect, _) = ui.allocate_exact_size(ui.available_size(), egui::Sense::hover());
                display_rect = Some(rect);
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

        let controls_resp = ui.horizontal(|ui| {
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
        register_exposable_element(ui, node_id, "controls", controls_resp.response.rect);

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
    if let Some(r) = display_rect { register_exposable_element(ui, node_id, "display", r); }
}

fn show_vectorscope_body(node_id: NodeId, inputs: &[InPin], ui: &mut egui::Ui, snarl: &mut Snarl<NodeData>) {
    let (history, n_channels, last_signals) = snarl
        .get_node(node_id)
        .map(|n| (n.extra.history.clone(), n.inputs.len().max(1), n.extra.last_signals.clone()))
        .unwrap_or_default();

    let mut display_rect: Option<egui::Rect> = None;
    ui.vertical(|ui| {
        egui::Resize::default()
            .id_salt(("vs", node_id))
            .default_size([140.0, 140.0])
            .min_size([40.0, 40.0])
            .show(ui, |ui| {
                let side = ui.available_size().min_elem();
                let (rect, _) = ui.allocate_exact_size(egui::Vec2::splat(side), egui::Sense::hover());
                display_rect = Some(rect);
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
    if let Some(r) = display_rect { register_exposable_element(ui, node_id, "display", r); }
}

// ── Processing module body renderers ──────────────────────────────────────────

fn show_delay_body(node_id: NodeId, inputs: &[InPin], outputs: &[OutPin], ui: &mut egui::Ui, snarl: &mut Snarl<NodeData>) {
    let delay_ms = snarl
        .get_node(node_id)
        .and_then(|n| n.params.get("delay_ms").and_then(|v| v.as_f64()))
        .unwrap_or(100.0) as f32;
    let mut v = delay_ms;
    let r = ui.horizontal(|ui| {
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
    register_exposable_element(ui, node_id, "ms", r.response.rect);
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

    let r1 = ui.horizontal(|ui| {
        ui.label(egui::RichText::new("Samples").small());
        changed |= ui.add(egui::DragValue::new(&mut bs).speed(1.0).range(1.0..=10_000.0)).changed();
    });
    let r2 = ui.horizontal(|ui| {
        ui.label(egui::RichText::new("Spike MAD").small())
            .on_hover_text("Outlier threshold in median absolute deviations. 0 = off. Try 3.0 to start.");
        changed |= ui.add(egui::DragValue::new(&mut sm).speed(0.1).range(0.0..=20.0).max_decimals(1)).changed();
    });
    register_exposable_element(ui, node_id, "samples",   r1.response.rect);
    register_exposable_element(ui, node_id, "spike_mad", r2.response.rect);

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

    let r1 = ui.horizontal(|ui| {
        ui.label(egui::RichText::new("Window ms").small());
        changed |= ui.add(egui::DragValue::new(&mut w).speed(10.0).range(10.0..=60_000.0)).changed();
    });
    let r2 = ui.horizontal(|ui| {
        ui.label(egui::RichText::new("Decay ms").small());
        changed |= ui.add(egui::DragValue::new(&mut d).speed(10.0).range(10.0..=60_000.0)).changed();
    });
    register_exposable_element(ui, node_id, "window_ms", r1.response.rect);
    register_exposable_element(ui, node_id, "decay_ms",  r2.response.rect);

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

    let mut curve_graph_rect: Option<egui::Rect> = None;
    ui.vertical(|ui| {
        // ── Graph ─────────────────────────────────────────────────────────────
        egui::Resize::default()
            .id_salt(("crv", node_id))
            .default_size([180.0, 180.0])
            .min_size([80.0, 80.0])
            .show(ui, |ui| {
                let (rect, bg_resp) =
                    ui.allocate_exact_size(ui.available_size(), egui::Sense::click());
                curve_graph_rect = Some(rect);
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

                    let origin_id = ui.id().with(("crv_pt_origin_inline", node_id, i));
                    if pt_resp.drag_started() && !alt_held {
                        ui.ctx().data_mut(|d| d.insert_temp(origin_id, [px, py, 0.0f32, 0.0f32]));
                    }
                    if pt_resp.dragged() && !alt_held {
                        let prev = ui.ctx().data(|d| d.get_temp::<[f32;4]>(origin_id))
                            .unwrap_or([px, py, 0.0, 0.0]);
                        let dd  = pt_resp.drag_delta();
                        let acc_x_px = prev[2] + dd.x;
                        let acc_y_px = prev[3] + dd.y;
                        ui.ctx().data_mut(|d| d.insert_temp(origin_id, [prev[0], prev[1], acc_x_px, acc_y_px]));
                        let nx_raw = prev[0] + acc_x_px * x_range / rect.width();
                        let ny_raw = prev[1] - acc_y_px * y_range / rect.height();
                        let lo_x   = new_points.get(i.wrapping_sub(1)).map(|p| p[0] + 0.001).unwrap_or(x_lo);
                        let hi_x   = new_points.get(i + 1).map(|p| p[0] - 0.001).unwrap_or(x_hi);
                        let (sx, sy) = do_snap(nx_raw, ny_raw);
                        new_points[i] = [sx.clamp(lo_x, hi_x), sy.clamp(y_lo, y_hi)];
                        pts_changed = true;
                    }
                    if pt_resp.drag_stopped() {
                        ui.ctx().data_mut(|d| d.remove_temp::<[f32;4]>(origin_id));
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
        let scale_resp = ui.horizontal(|ui| {
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
        register_exposable_element(ui, node_id, "scale_row", scale_resp.response.rect);

        // Row 2: In/Out range
        let range_resp = ui.scope(|ui| {
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
        });
        register_exposable_element(ui, node_id, "range_row", range_resp.response.rect);

        // Row 3: Grid + Trail
        let grid_resp = ui.horizontal(|ui| {
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
        register_exposable_element(ui, node_id, "grid_row", grid_resp.response.rect);

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
    if let Some(rect) = curve_graph_rect {
        register_exposable_element(ui, node_id, "curve", rect);
    }
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
    let mut curve_graph_rect: Option<egui::Rect> = None;

    ui.vertical(|ui| {
        // ── Graph ─────────────────────────────────────────────────────────────
        egui::Resize::default()
            .id_salt(("vcrv", node_id))
            .default_size([180.0, 180.0])
            .min_size([80.0, 80.0])
            .show(ui, |ui| {
                let (rect, bg_resp) =
                    ui.allocate_exact_size(ui.available_size(), egui::Sense::click());
                curve_graph_rect = Some(rect);
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

                    let origin_id = ui.id().with(("vcrv_pt_origin_inline", node_id, i));
                    if pt_resp.drag_started() && !alt_held {
                        ui.ctx().data_mut(|d| d.insert_temp(origin_id, [px, py, 0.0f32, 0.0f32]));
                    }
                    if pt_resp.dragged() && !alt_held {
                        let prev = ui.ctx().data(|d| d.get_temp::<[f32;4]>(origin_id))
                            .unwrap_or([px, py, 0.0, 0.0]);
                        let dd  = pt_resp.drag_delta();
                        let acc_x_px = prev[2] + dd.x;
                        let acc_y_px = prev[3] + dd.y;
                        ui.ctx().data_mut(|d| d.insert_temp(origin_id, [prev[0], prev[1], acc_x_px, acc_y_px]));
                        let nx_raw = prev[0] + acc_x_px * x_range / rect.width();
                        let ny_raw = prev[1] - acc_y_px * y_range / rect.height();
                        let lo_x   = new_points.get(i.wrapping_sub(1)).map(|p| p[0] + 0.001).unwrap_or(x_lo);
                        let hi_x   = new_points.get(i + 1).map(|p| p[0] - 0.001).unwrap_or(x_hi);
                        let (sx, sy) = do_snap(nx_raw, ny_raw);
                        new_points[i] = [sx.clamp(lo_x, hi_x), sy.clamp(y_lo, y_hi)];
                        pts_changed = true;
                    }
                    if pt_resp.drag_stopped() {
                        ui.ctx().data_mut(|d| d.remove_temp::<[f32;4]>(origin_id));
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
        let scale_resp = ui.horizontal(|ui| {
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
        register_exposable_element(ui, node_id, "scale_row", scale_resp.response.rect);

        // Row 2: In/Out max
        let range_resp = ui.horizontal(|ui| {
            ui.label(egui::RichText::new("In max").small().weak());
            changed |= ui.add(egui::DragValue::new(&mut i1).speed(0.01).max_decimals(2)).changed();
            ui.separator();
            ui.label(egui::RichText::new("Out max").small().weak());
            changed |= ui.add(egui::DragValue::new(&mut o1).speed(0.01).max_decimals(2)).changed();
        });
        register_exposable_element(ui, node_id, "range_row", range_resp.response.rect);

        // Row 3: Grid + Trail
        let grid_resp = ui.horizontal(|ui| {
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
        register_exposable_element(ui, node_id, "grid_row", grid_resp.response.rect);

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
    if let Some(rect) = curve_graph_rect {
        register_exposable_element(ui, node_id, "curve", rect);
    }
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
