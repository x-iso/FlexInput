use std::collections::HashMap;

use eframe::egui;
use egui_snarl::{InPinId, NodeId, Snarl};
use flexinput_core::{ModuleDescriptor, Signal};
use flexinput_devices::{init_backend, DeviceBackend, PhysicalDevice};
use flexinput_engine::Engine;
use flexinput_modules::all_modules;
use flexinput_virtual::VirtualDevice;

use crate::{
    canvas::{Canvas, NodeData},
    panels::{physical_devices, virtual_devices::VirtualDevicePanel},
};

fn setup_fonts(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();
    #[cfg(windows)]
    if let Ok(bytes) = std::fs::read(r"C:\Windows\Fonts\segoeui.ttf") {
        fonts.font_data.insert("segoe_ui".to_owned(), std::sync::Arc::new(egui::FontData::from_owned(bytes)));
        for family in fonts.families.values_mut() {
            family.push("segoe_ui".to_owned());
        }
    }
    ctx.set_fonts(fonts);
}

pub struct FlexInputApp {
    engine: Engine,
    canvas: Canvas,
    descriptors: Vec<ModuleDescriptor>,
    device_backend: Option<Box<dyn DeviceBackend>>,
    devices: Vec<PhysicalDevice>,
    virtual_panel: VirtualDevicePanel,
    /// Latest polled signals, cached so routing can access them.
    last_signals: HashMap<(String, String), Signal>,
}

impl FlexInputApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        setup_fonts(&cc.egui_ctx);
        let descriptors = all_modules().into_iter().map(|r| r.descriptor).collect();
        let device_backend = init_backend();
        Self {
            engine: Engine::new(),
            canvas: Canvas::new(),
            descriptors,
            device_backend,
            devices: vec![],
            virtual_panel: VirtualDevicePanel::new(),
            last_signals: HashMap::new(),
        }
    }
}

impl eframe::App for FlexInputApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if let Some(backend) = &mut self.device_backend {
            self.devices = backend.enumerate();
            let signals = backend.poll();
            self.last_signals = signals
                .into_iter()
                .map(|(dev, pin, sig)| ((dev, pin), sig))
                .collect();
        }

        self.engine.tick();

        // Route canvas signals → virtual devices, then flush HID reports.
        route_signals(&self.canvas.snarl, &self.last_signals, &mut self.virtual_panel.active);
        for dev in &mut self.virtual_panel.active {
            dev.flush();
        }

        let (virtual_panel, canvas) = (&mut self.virtual_panel, &mut self.canvas);
        egui::TopBottomPanel::top("virtual_devices_panel")
            .min_height(48.0)
            .resizable(true)
            .show(ctx, |ui| {
                virtual_panel.show(ui, canvas);
            });

        egui::TopBottomPanel::bottom("physical_devices_panel")
            .min_height(80.0)
            .default_height(220.0)
            .resizable(true)
            .show(ctx, |ui| {
                physical_devices::show(ui, &self.devices, canvas);
                // Claim leftover space so the panel doesn't collapse when
                // collapsible headers are closed.
                ui.allocate_space(ui.available_size());
            });

        egui::CentralPanel::default().show(ctx, |ui| {
            crate::panels::canvas::show(&mut self.canvas, &self.descriptors, ui);
        });
    }
}

// ── Signal routing ────────────────────────────────────────────────────────────

fn route_signals(
    snarl: &Snarl<NodeData>,
    dev_sigs: &HashMap<(String, String), Signal>,
    active: &mut Vec<Box<dyn VirtualDevice>>,
) {
    let mut routes: Vec<(String, String, Signal)> = vec![];

    for (node_id, node_ref) in snarl.nodes_ids_data() {
        let node = &node_ref.value;
        if node.module_id != "device.sink" {
            continue;
        }

        let sink_id = match node.params.get("device_id").and_then(|v| v.as_str()) {
            Some(s) => s.to_string(),
            None => continue,
        };

        let pin_ids: Vec<String> = node.params
            .get("input_pin_ids")
            .and_then(|v| v.as_array())
            .map(|a| a.iter().map(|v| v.as_str().unwrap_or("").to_string()).collect())
            .unwrap_or_default();

        for in_idx in 0..node.inputs.len() {
            let dst_pin = match pin_ids.get(in_idx).filter(|s| !s.is_empty()) {
                Some(s) => s.clone(),
                None => continue,
            };

            let in_pin = snarl.in_pin(InPinId { node: node_id, input: in_idx });
            for &src in &in_pin.remotes {
                if let Some(sig) = eval_output(snarl, src.node, src.output, dev_sigs, 0) {
                    routes.push((sink_id.clone(), dst_pin.clone(), sig));
                }
            }
        }
    }

    for (device_id, pin_id, signal) in routes {
        if let Some(dev) = active.iter_mut().find(|d| d.id() == device_id) {
            dev.send(&pin_id, signal);
        }
    }
}

/// Recursively evaluates the signal at a node's output pin.
fn eval_output(
    snarl: &Snarl<NodeData>,
    node_id: NodeId,
    out_idx: usize,
    dev_sigs: &HashMap<(String, String), Signal>,
    depth: u8,
) -> Option<Signal> {
    if depth > 16 {
        return None; // prevent infinite recursion in cyclic graphs
    }

    let node = snarl.get_node(node_id)?;

    match node.module_id.as_str() {
        "device.source" => {
            let dev_id = node.params.get("device_id")?.as_str()?;
            let ids = node.params.get("output_pin_ids")?.as_array()?;
            let pin_id = ids.get(out_idx)?.as_str()?;
            dev_sigs.get(&(dev_id.to_string(), pin_id.to_string())).copied()
        }
        "module.constant" | "module.knob" => {
            node.params.get("value")
                .and_then(|v| v.as_f64())
                .map(|f| Signal::Float(f as f32))
        }
        "module.switch" => {
            node.params.get("active")
                .and_then(|v| v.as_bool())
                .map(Signal::Bool)
        }
        id => {
            // Resolve all inputs recursively, then apply module logic inline.
            let inputs: Vec<Option<Signal>> = (0..node.inputs.len())
                .map(|i| {
                    let p = snarl.in_pin(InPinId { node: node_id, input: i });
                    p.remotes.first().and_then(|&src| {
                        eval_output(snarl, src.node, src.output, dev_sigs, depth + 1)
                    })
                })
                .collect();
            eval_module(id, out_idx, &inputs, node)
        }
    }
}

fn get_f(inputs: &[Option<Signal>], i: usize, default: f32) -> f32 {
    inputs.get(i).and_then(|s| *s).and_then(|s| {
        if let Signal::Float(f) = s { Some(f) } else { None }
    }).unwrap_or(default)
}

fn get_b(inputs: &[Option<Signal>], i: usize, default: bool) -> bool {
    inputs.get(i).and_then(|s| *s).and_then(|s| {
        if let Signal::Bool(b) = s { Some(b) } else { None }
    }).unwrap_or(default)
}

/// Evaluates a pure module given its resolved inputs; also reads param defaults.
fn eval_module(id: &str, out_idx: usize, inputs: &[Option<Signal>], node: &NodeData) -> Option<Signal> {
    // For optional inputs, fall back to node params if no wire connected.
    let param_f = |name: &str, default: f32| -> f32 {
        node.params.get(name).and_then(|v| v.as_f64()).map(|f| f as f32).unwrap_or(default)
    };

    match id {
        "math.add"       => Some(Signal::Float(get_f(inputs, 0, 0.0) + get_f(inputs, 1, 0.0))),
        "math.subtract"  => Some(Signal::Float(get_f(inputs, 0, 0.0) - get_f(inputs, 1, 0.0))),
        "math.multiply"  => Some(Signal::Float(get_f(inputs, 0, 0.0) * get_f(inputs, 1, 1.0))),
        "math.divide"    => {
            let b = get_f(inputs, 1, 1.0);
            Some(Signal::Float(if b == 0.0 { 0.0 } else { get_f(inputs, 0, 0.0) / b }))
        }
        "math.abs"       => Some(Signal::Float(get_f(inputs, 0, 0.0).abs())),
        "math.negate"    => Some(Signal::Float(-get_f(inputs, 0, 0.0))),
        "math.clamp"     => {
            let v   = get_f(inputs, 0, 0.0);
            let min = if inputs.get(1).and_then(|s| *s).is_some() { get_f(inputs, 1, -1.0) } else { param_f("min", -1.0) };
            let max = if inputs.get(2).and_then(|s| *s).is_some() { get_f(inputs, 2,  1.0) } else { param_f("max",  1.0) };
            Some(Signal::Float(v.clamp(min, max)))
        }
        "math.map_range" => {
            let v       = get_f(inputs, 0, 0.0);
            let in_min  = if inputs.get(1).and_then(|s| *s).is_some() { get_f(inputs, 1, -1.0) } else { param_f("in_min",  -1.0) };
            let in_max  = if inputs.get(2).and_then(|s| *s).is_some() { get_f(inputs, 2,  1.0) } else { param_f("in_max",   1.0) };
            let out_min = if inputs.get(3).and_then(|s| *s).is_some() { get_f(inputs, 3, -1.0) } else { param_f("out_min", -1.0) };
            let out_max = if inputs.get(4).and_then(|s| *s).is_some() { get_f(inputs, 4,  1.0) } else { param_f("out_max",  1.0) };
            let t = if (in_max - in_min).abs() < f32::EPSILON { 0.0 }
                    else { (v - in_min) / (in_max - in_min) };
            Some(Signal::Float(out_min + t * (out_max - out_min)))
        }
        "logic.and"      => Some(Signal::Bool(get_b(inputs, 0, false) && get_b(inputs, 1, false))),
        "logic.or"       => Some(Signal::Bool(get_b(inputs, 0, false) || get_b(inputs, 1, false))),
        "logic.not"      => Some(Signal::Bool(!get_b(inputs, 0, false))),
        "logic.xor"      => Some(Signal::Bool(get_b(inputs, 0, false) ^ get_b(inputs, 1, false))),
        "module.selector" => {
            if out_idx == 0 {
                if get_b(inputs, 0, false) {
                    inputs.get(2).and_then(|s| *s)
                } else {
                    inputs.get(1).and_then(|s| *s)
                }
            } else {
                None
            }
        }
        _ => None,
    }
}
