mod curve;
pub mod node;
mod viewer;

pub use curve::sample_curve;
pub use node::NodeData;
pub use viewer::FlexViewer;

use std::collections::{HashMap, HashSet};

use egui_snarl::{ui::SnarlStyle, InPinId, OutPinId, Snarl};
use flexinput_core::{PinDescriptor, ModuleDescriptor};
use flexinput_devices::PhysicalDevice;
use flexinput_virtual::{SinkPin, VirtualDevice};
use serde_json::Value;

#[derive(serde::Serialize, serde::Deserialize)]
struct UiPatch {
    version: u32,
    snarl: Snarl<NodeData>,
    /// IDs of virtual output devices that were active (e.g. `"virtual.xinput.0"`).
    virtual_device_ids: Vec<String>,
    /// Exe filenames that auto-switch to this tab (e.g. `["game.exe"]`).
    #[serde(default)]
    bound_exes: Vec<String>,
}

pub struct Canvas {
    pub snarl: Snarl<NodeData>,
    style: SnarlStyle,
    /// Pending wire right-click context menu: (from, to, screen position).
    wire_ctx_menu: Option<(OutPinId, InPinId, egui::Pos2)>,
    /// True on the frame the wire menu was first opened; suppresses the outside-click close check.
    wire_ctx_just_opened: bool,
    /// Active inline rename: (node id, edit buffer, popup position).
    rename_state: Option<(egui_snarl::NodeId, String, egui::Pos2)>,
}

impl Canvas {
    pub fn new() -> Self {
        let mut style = SnarlStyle::default();
        style.collapsible = Some(true);
        Canvas { snarl: Snarl::new(), style, wire_ctx_menu: None, wire_ctx_just_opened: false, rename_state: None }
    }

    pub fn show(
        &mut self,
        descriptors: &[ModuleDescriptor],
        live_device_ids: &HashSet<String>,
        ui: &mut egui::Ui,
    ) {
        let ctx = ui.ctx().clone();
        let mut viewer = FlexViewer {
            descriptors,
            ctx,
            live_device_ids,
            pending_wire_menu: None,
            rename_request: None,
        };
        self.snarl.show(&mut viewer, &self.style, "flexinput_canvas", ui);

        if let Some(pending) = viewer.pending_wire_menu {
            self.wire_ctx_menu = Some(pending);
            self.wire_ctx_just_opened = true;
        }

        // ── Rename popup ──────────────────────────────────────────────────────
        if let Some(node_id) = viewer.rename_request {
            let current = self.snarl.get_node(node_id)
                .map(|n| n.display_name.clone())
                .unwrap_or_default();
            let pos = ui.ctx().input(|i| i.pointer.latest_pos().unwrap_or_default());
            self.rename_state = Some((node_id, current, pos));
        }

        let mut commit_name: Option<(egui_snarl::NodeId, String)> = None;
        let mut close_rename = false;

        if let Some((node_id, ref mut buf, pos)) = self.rename_state {
            let mut open = true;
            egui::Window::new("Rename")
                .id(egui::Id::new("rename_module_window"))
                .fixed_pos(pos)
                .resizable(false)
                .collapsible(false)
                .open(&mut open)
                .show(ui.ctx(), |ui| {
                    let resp = ui.add(
                        egui::TextEdit::singleline(buf)
                            .desired_width(200.0)
                            .hint_text("Module name"),
                    );
                    if !resp.has_focus() {
                        resp.request_focus();
                    }
                    ui.horizontal(|ui| {
                        if ui.button("OK").clicked()
                            || ui.input(|i| i.key_pressed(egui::Key::Enter))
                        {
                            commit_name = Some((node_id, buf.clone()));
                            close_rename = true;
                        }
                        if ui.button("Cancel").clicked()
                            || ui.input(|i| i.key_pressed(egui::Key::Escape))
                        {
                            close_rename = true;
                        }
                    });
                });
            if !open {
                close_rename = true;
            }
        }

        if let Some((nid, name)) = commit_name {
            if let Some(node) = self.snarl.get_node_mut(nid) {
                node.display_name = name;
            }
        }
        if close_rename {
            self.rename_state = None;
        }

        // ── Wire right-click context menu ─────────────────────────────────────
        if let Some((out_id, in_id, pos)) = self.wire_ctx_menu {
            // Read pin signal types for filtering compatible modules.
            let out_sig = self.snarl.get_node(out_id.node)
                .and_then(|n| n.outputs.get(out_id.output))
                .map(|p| p.signal_type);
            let in_sig = self.snarl.get_node(in_id.node)
                .and_then(|n| n.inputs.get(in_id.input))
                .map(|p| p.signal_type);

            // Pre-collect compatible modules grouped by category.
            let mut cats: Vec<(&str, Vec<usize>)> = vec![];
            for (i, d) in descriptors.iter().enumerate() {
                if d.inputs.is_empty() || d.outputs.is_empty() { continue; }
                let in_ok = out_sig.map_or(true, |t| d.inputs.iter().any(|p| p.signal_type.accepts(t)));
                let out_ok = in_sig.map_or(true, |t| d.outputs.iter().any(|p| t.accepts(p.signal_type)));
                if in_ok && out_ok {
                    if let Some(entry) = cats.iter_mut().find(|(c, _)| *c == d.category) {
                        entry.1.push(i);
                    } else {
                        cats.push((d.category, vec![i]));
                    }
                }
            }

            let mut close = false;
            let mut delete = false;
            let mut insert_idx: Option<usize> = None;

            let area_resp = egui::Area::new(egui::Id::new("wire_ctx_menu"))
                .order(egui::Order::Foreground)
                .fixed_pos(pos)
                .show(ui.ctx(), |ui| {
                    egui::Frame::popup(ui.style()).show(ui, |ui| {
                        ui.set_min_width(150.0);
                        if ui.button("✖ Delete wire").clicked() {
                            delete = true;
                            close = true;
                        }
                        if !cats.is_empty() {
                            ui.separator();
                            ui.label(egui::RichText::new("Insert between…").small().weak());
                            for (cat, indices) in &cats {
                                ui.menu_button(*cat, |ui| {
                                    for &i in indices {
                                        if ui.button(descriptors[i].display_name).clicked() {
                                            insert_idx = Some(i);
                                            close = true;
                                            ui.close();
                                        }
                                    }
                                });
                            }
                        }
                    });
                });

            if delete {
                self.snarl.disconnect(out_id, in_id);
            }
            if let Some(i) = insert_idx {
                insert_between(&mut self.snarl, &descriptors[i], out_id, in_id);
            }

            // Close on click outside (skip the frame the menu first appeared).
            if !self.wire_ctx_just_opened {
                let ptr = ui.input(|i| i.pointer.latest_pos().unwrap_or_default());
                let clicked = ui.input(|i| i.pointer.any_click());
                if clicked && !area_resp.response.rect.contains(ptr) {
                    close = true;
                }
                if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                    close = true;
                }
            }
            self.wire_ctx_just_opened = false;
            if close {
                self.wire_ctx_menu = None;
            }
        }
    }

    /// Add a physical device as a source node. No-op if already present.
    pub fn add_device_source(&mut self, device: &PhysicalDevice) {
        let already_present = self.snarl.nodes_ids_data().any(|(_, n)| {
            n.value.module_id == "device.source"
                && n.value.params.get("device_id").and_then(|v| v.as_str()) == Some(&device.id)
        });
        if already_present {
            return;
        }

        let outputs = device
            .outputs
            .iter()
            .map(|p| PinDescriptor::new(&p.display_name, p.signal_type))
            .collect();

        let inputs = device
            .inputs
            .iter()
            .map(|p| PinDescriptor::new(&p.display_name, p.signal_type))
            .collect();

        let mut params = HashMap::new();
        params.insert("device_id".to_string(), Value::String(device.id.clone()));
        params.insert("output_pin_ids".to_string(), Value::Array(
            device.outputs.iter().map(|p| Value::String(p.id.clone())).collect(),
        ));
        params.insert("input_pin_ids".to_string(), Value::Array(
            device.inputs.iter().map(|p| Value::String(p.id.clone())).collect(),
        ));

        let node = NodeData {
            module_id: "device.source".to_string(),
            display_name: device.display_name.clone(),
            category: "Device".to_string(),
            inputs,
            outputs,
            params,
            extra: Default::default(),
        };

        self.snarl.insert_node(egui::pos2(80.0, 80.0), node);
    }

    /// Add a physical device's input pins as a sink node (e.g. MIDI OUT port).
    /// No-op if already present (keyed by device id).
    pub fn add_physical_sink(&mut self, device: &PhysicalDevice) {
        let already_present = self.snarl.nodes_ids_data().any(|(_, n)| {
            n.value.module_id == "device.sink"
                && n.value.params.get("device_id").and_then(|v| v.as_str()) == Some(&device.id)
        });
        if already_present {
            return;
        }

        let fixed_count = device.inputs.len();
        let inputs = device.inputs.iter()
            .map(|p| PinDescriptor::new(&p.display_name, p.signal_type))
            .collect();

        let mut params = HashMap::new();
        params.insert("device_id".to_string(), Value::String(device.id.clone()));
        params.insert("fixed_input_count".to_string(), Value::Number(fixed_count.into()));
        params.insert("input_pin_ids".to_string(), Value::Array(
            device.inputs.iter().map(|p| Value::String(p.id.clone())).collect(),
        ));

        let node = NodeData {
            module_id: "device.sink".to_string(),
            display_name: device.display_name.clone(),
            category: "Device".to_string(),
            inputs,
            outputs: vec![],
            params,
            extra: Default::default(),
        };

        self.snarl.insert_node(egui::pos2(400.0, 80.0), node);
    }

    /// Add a virtual device as a sink node. No-op if already present (keyed by device id).
    pub fn add_virtual_sink(&mut self, device: &dyn VirtualDevice) {
        let already_present = self.snarl.nodes_ids_data().any(|(_, n)| {
            n.value.module_id == "device.sink"
                && n.value.params.get("device_id").and_then(|v| v.as_str()) == Some(device.id())
        });
        if already_present {
            return;
        }

        let fixed_count = device.sink_pins().len();
        let inputs = device
            .sink_pins()
            .iter()
            .map(|p: &SinkPin| PinDescriptor::new(p.display_name, p.signal_type))
            .collect();

        let mut params = HashMap::new();
        params.insert("device_id".to_string(), Value::String(device.id().to_string()));
        params.insert("fixed_input_count".to_string(), Value::Number(fixed_count.into()));
        params.insert("input_pin_ids".to_string(), Value::Array(
            device.sink_pins().iter().map(|p| Value::String(p.id.to_string())).collect(),
        ));

        let node = NodeData {
            module_id: "device.sink".to_string(),
            display_name: device.display_name().to_string(),
            category: "Device".to_string(),
            inputs,
            outputs: vec![],
            params,
            extra: Default::default(),
        };

        self.snarl.insert_node(egui::pos2(400.0, 80.0), node);
    }
}

impl Canvas {
    /// Serialize the canvas + virtual device list to a `.fxp` file chosen by the user.
    pub fn save_patch(&self, virtual_device_ids: Vec<String>, bound_exes: Vec<String>) {
        let Some(path) = rfd::FileDialog::new()
            .add_filter("FlexInput Patch", &["fxp"])
            .set_file_name("patch.fxp")
            .save_file()
        else { return };

        let patch = UiPatch {
            version: 1,
            snarl: self.snarl.clone(),
            virtual_device_ids,
            bound_exes,
        };
        if let Ok(json) = serde_json::to_string_pretty(&patch) {
            let _ = std::fs::write(path, json);
        }
    }

    /// Open a `.fxp` file and return the loaded Canvas, virtual device IDs, bound exes, and path.
    /// Returns `None` if the user cancels or the file is invalid.
    pub fn load_patch() -> Option<(Canvas, Vec<String>, Vec<String>, std::path::PathBuf)> {
        let path = rfd::FileDialog::new()
            .add_filter("FlexInput Patch", &["fxp"])
            .pick_file()?;

        let json = std::fs::read_to_string(&path).ok()?;
        let patch: UiPatch = serde_json::from_str(&json).ok()?;

        let mut canvas = Canvas::new();
        canvas.snarl = patch.snarl;
        Some((canvas, patch.virtual_device_ids, patch.bound_exes, path))
    }
}

impl Default for Canvas {
    fn default() -> Self {
        Self::new()
    }
}

/// Disconnect a wire and insert `desc` between its endpoints, auto-connecting compatible pins.
fn insert_between(
    snarl: &mut Snarl<NodeData>,
    desc: &ModuleDescriptor,
    out_id: OutPinId,
    in_id: InPinId,
) {
    let from_pos = snarl.get_node_info(out_id.node).map(|n| n.pos).unwrap_or_default();
    let to_pos   = snarl.get_node_info(in_id.node) .map(|n| n.pos).unwrap_or_default();
    let insert_pos = egui::pos2(
        (from_pos.x + to_pos.x) * 0.5,
        (from_pos.y + to_pos.y) * 0.5,
    );

    let out_type = snarl.get_node(out_id.node)
        .and_then(|n| n.outputs.get(out_id.output))
        .map(|p| p.signal_type);
    let in_type = snarl.get_node(in_id.node)
        .and_then(|n| n.inputs.get(in_id.input))
        .map(|p| p.signal_type);

    snarl.disconnect(out_id, in_id);
    let new_id = snarl.insert_node(insert_pos, NodeData::from(desc));

    if let Some(idx) = desc.inputs.iter().position(|p| out_type.map_or(true, |t| p.signal_type.accepts(t))) {
        snarl.connect(out_id, InPinId { node: new_id, input: idx });
    }
    if let Some(idx) = desc.outputs.iter().position(|p| in_type.map_or(true, |t| t.accepts(p.signal_type))) {
        snarl.connect(OutPinId { node: new_id, output: idx }, in_id);
    }
}
