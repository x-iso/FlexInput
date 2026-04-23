use std::collections::HashMap;

use eframe::egui;
use egui_snarl::{InPinId, NodeId, Snarl};
use flexinput_core::{ModuleDescriptor, Signal};
use flexinput_core::PinDescriptor;
use flexinput_devices::{init_backends, midi::cc_display_name, DeviceBackend, HidHideClient, MidiBackend, PhysicalDevice};
use flexinput_engine::Engine;
use flexinput_modules::all_modules;
use flexinput_virtual::VirtualDevice;

use crate::{
    canvas::{sample_curve, Canvas, NodeData},
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
    backends: Vec<Box<dyn DeviceBackend>>,
    midi_backend: Option<MidiBackend>,
    devices: Vec<PhysicalDevice>,
    virtual_panel: VirtualDevicePanel,
    /// Latest polled signals, cached so routing can access them.
    last_signals: HashMap<(String, String), Signal>,
    hidhide: Option<HidHideClient>,
    last_update: std::time::Instant,
    /// Manually tracked bottom panel height so resize survives egui state resets.
    bottom_panel_height: f32,
}

impl FlexInputApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        setup_fonts(&cc.egui_ctx);
        let descriptors = all_modules().into_iter().map(|r| r.descriptor).collect();
        let backends = init_backends();
        let midi_backend = Some(MidiBackend::new());
        let hidhide = HidHideClient::try_open();
        if let (Some(hh), Some(exe)) = (&hidhide, HidHideClient::current_exe_path()) {
            hh.ensure_whitelisted(&exe);
        }
        Self {
            engine: Engine::new(),
            canvas: Canvas::new(),
            descriptors,
            backends,
            midi_backend,
            devices: vec![],
            virtual_panel: VirtualDevicePanel::new(),
            last_signals: HashMap::new(),
            hidhide,
            last_update: std::time::Instant::now(),
            bottom_panel_height: 220.0,
        }
    }
}

impl eframe::App for FlexInputApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        let dt = self.last_update.elapsed().as_secs_f32().clamp(0.001, 0.1);
        self.last_update = std::time::Instant::now();

        // Poll gamepad backends.
        self.last_signals.clear();
        self.devices.clear();
        for backend in &mut self.backends {
            for (dev, pin, sig) in backend.poll() {
                self.last_signals.insert((dev, pin), sig);
            }
            self.devices.extend(backend.enumerate());
        }

        // Poll MIDI backend.
        if let Some(midi) = &mut self.midi_backend {
            for (dev, pin, sig) in midi.poll() {
                self.last_signals.insert((dev, pin), sig);
            }
            self.devices.extend(midi.enumerate());
        }

        // Feed learned CCs into canvas nodes that have learning mode active.
        {
            let snarl = &mut self.canvas.snarl;
            let midi_opt = &mut self.midi_backend;
            if let Some(midi) = midi_opt {
                let learning: Vec<(NodeId, String)> = snarl
                    .nodes_ids_data()
                    .filter(|(_, n)| {
                        n.value.module_id == "device.source"
                            && n.value.params.get("learning").and_then(|v| v.as_bool()) == Some(true)
                            && n.value.params.get("device_id").and_then(|v| v.as_str())
                                .map(|id| id.starts_with("midi_in:"))
                                .unwrap_or(false)
                    })
                    .map(|(id, n)| {
                        let dev_id = n.value.params["device_id"].as_str().unwrap_or("").to_string();
                        (id, dev_id)
                    })
                    .collect();

                for (node_id, device_id) in learning {
                    if let Some(cc) = midi.take_learned_cc(&device_id) {
                        let already_has = snarl
                            .get_node(node_id)
                            .and_then(|n| n.params.get("output_pin_ids").and_then(|v| v.as_array()))
                            .map(|ids| ids.iter().any(|v| v.as_str() == Some(&format!("cc_{}", cc))))
                            .unwrap_or(false);
                        if !already_has {
                            if let Some(node) = snarl.get_node_mut(node_id) {
                                node.outputs.push(PinDescriptor::new(&cc_display_name(cc), flexinput_core::SignalType::Float));
                                if let Some(serde_json::Value::Array(ids)) = node.params.get_mut("output_pin_ids") {
                                    ids.push(serde_json::Value::String(format!("cc_{}", cc)));
                                }
                            }
                        }
                    }
                }
            }
        }

        self.engine.tick();

        // Update stateful processing nodes (delay, lowpass) before routing.
        update_stateful_nodes(&mut self.canvas.snarl, &self.last_signals, dt);

        // Route canvas signals → virtual devices, then flush HID reports.
        route_signals(&self.canvas.snarl, &self.last_signals, &mut self.virtual_panel.active);
        for dev in &mut self.virtual_panel.active {
            dev.flush();
        }

        // Route canvas signals → MIDI OUT sinks.
        if let Some(midi) = &mut self.midi_backend {
            route_midi_out(&self.canvas.snarl, &self.last_signals, midi);
        }

        // Update display-node signal histories so the viewer can render them.
        update_display_nodes(&mut self.canvas.snarl, &self.last_signals);

        // ── Menu bar ─────────────────────────────────────────────────────────────
        let mut do_save = false;
        let mut do_load = false;
        egui::TopBottomPanel::top("menu_bar").show(ctx, |ui| {
            egui::MenuBar::new().ui(ui, |ui| {
                ui.menu_button("File", |ui| {
                    if ui.button("Save Patch…").clicked() {
                        do_save = true;
                        ui.close();
                    }
                    if ui.button("Load Patch…").clicked() {
                        do_load = true;
                        ui.close();
                    }
                });
            });
        });

        // Handle save/load before the borrow split so self is fully available.
        if do_save {
            let vids = self.virtual_panel.active.iter().map(|d| d.id().to_string()).collect();
            self.canvas.save_patch(vids);
        }
        if do_load {
            if let Some((new_canvas, vids)) = crate::canvas::Canvas::load_patch() {
                self.canvas = new_canvas;
                self.virtual_panel.active.clear();
                for vid in &vids {
                    if let Some(dev) = try_create_virtual_device(vid) {
                        self.virtual_panel.active.push(dev);
                    }
                }
            }
        }

        // Build the set of currently-live device IDs for canvas status dots.
        let live_device_ids: std::collections::HashSet<String> =
            self.devices.iter().map(|d| d.id.clone())
                .chain(
                    self.virtual_panel.active.iter()
                        .filter(|d| d.is_connected())
                        .map(|d| d.id().to_string())
                )
                .collect();

        let devices = &self.devices;
        let hidhide = self.hidhide.as_ref();
        let bottom_panel_height = self.bottom_panel_height;
        let (virtual_panel, canvas) =
            (&mut self.virtual_panel, &mut self.canvas);

        egui::TopBottomPanel::top("virtual_devices_panel")
            .min_height(48.0)
            .resizable(true)
            .show(ctx, |ui| {
                virtual_panel.show(ui, canvas);
            });

        let bottom_resp = egui::TopBottomPanel::bottom("physical_devices_panel")
            .min_height(80.0)
            .default_height(bottom_panel_height)
            .resizable(true)
            .show(ctx, |ui| {
                physical_devices::show(ui, devices, canvas, hidhide);
            });
        self.bottom_panel_height = bottom_resp.response.rect.height();

        egui::CentralPanel::default().show(ctx, |ui| {
            crate::panels::canvas::show(canvas, &self.descriptors, &live_device_ids, ui);
        });

        // Signal routing must run every frame regardless of UI interaction.
        ctx.request_repaint();
    }
}

/// Recreate a virtual device from its saved ID string (e.g. `"virtual.xinput.0"`).
fn try_create_virtual_device(id: &str) -> Option<Box<dyn flexinput_virtual::VirtualDevice>> {
    let dot = id.rfind('.')?;
    let kind_id = &id[..dot];
    let instance: usize = id[dot + 1..].parse().ok()?;
    Some(flexinput_virtual::create_device(kind_id, instance))
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

fn route_midi_out(
    snarl: &Snarl<NodeData>,
    dev_sigs: &HashMap<(String, String), Signal>,
    midi: &mut flexinput_devices::MidiBackend,
) {
    let mut routes: Vec<(String, String, Signal)> = vec![];

    for (node_id, node_ref) in snarl.nodes_ids_data() {
        let node = &node_ref.value;
        if node.module_id != "device.sink" { continue; }

        let sink_id = match node.params.get("device_id").and_then(|v| v.as_str()) {
            Some(s) => s.to_string(),
            None => continue,
        };
        if !sink_id.starts_with("midi_out:") { continue; }

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
        midi.send(&device_id, &pin_id, signal);
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
        // Stateful modules: output computed by update_stateful_nodes() each frame.
        "module.delay" | "module.lowpass" => {
            node.extra.last_signals.first().copied().flatten()
        }
        "module.response_curve" => {
            if out_idx >= 3 { return None; }
            let x        = get_f(inputs, out_idx, 0.0);
            let pts      = curve_points_from_params(node);
            let biases   = biases_from_params(node);
            let absolute = node.params.get("absolute") .and_then(|v| v.as_bool()).unwrap_or(true);
            let in_max   = node.params.get("in_max")  .and_then(|v| v.as_f64()).unwrap_or(1.0)  as f32;
            let in_min   = node.params.get("in_min")  .and_then(|v| v.as_f64()).unwrap_or(-1.0) as f32;
            let out_max  = node.params.get("out_max") .and_then(|v| v.as_f64()).unwrap_or(1.0)  as f32;
            let out_min  = node.params.get("out_min") .and_then(|v| v.as_f64()).unwrap_or(-1.0) as f32;
            let in_scale = node.params.get("in_scale").and_then(|v| v.as_i64()).unwrap_or(0);
            Some(Signal::Float(apply_curve(x, &pts, &biases, absolute, in_min, in_max, out_min, out_max, in_scale)))
        }
        _ => None,
    }
}

// ── Stateful node pre-pass ────────────────────────────────────────────────────

const STATEFUL_IDS: &[&str] = &["module.delay", "module.lowpass", "module.response_curve"];

fn update_stateful_nodes(
    snarl: &mut Snarl<NodeData>,
    dev_sigs: &HashMap<(String, String), Signal>,
    dt: f32,
) {
    let node_ids: Vec<NodeId> = snarl
        .nodes_ids_data()
        .filter(|(_, n)| STATEFUL_IDS.contains(&n.value.module_id.as_str()))
        .map(|(id, _)| id)
        .collect();

    for node_id in node_ids {
        let n_inputs = snarl.get_node(node_id).map(|n| n.inputs.len()).unwrap_or(0);
        let inputs: Vec<Option<Signal>> = (0..n_inputs)
            .map(|i| {
                let pin = snarl.in_pin(InPinId { node: node_id, input: i });
                pin.remotes.first().and_then(|&src| {
                    eval_output(snarl, src.node, src.output, dev_sigs, 0)
                })
            })
            .collect();

        if let Some(node) = snarl.get_node_mut(node_id) {
            match node.module_id.as_str() {
                "module.delay" => {
                    let out = compute_delay(&inputs, &mut node.extra, &node.params);
                    node.extra.last_signals = vec![out];
                }
                "module.lowpass" => {
                    let out = compute_lowpass(&inputs, &mut node.extra, &node.params, dt);
                    node.extra.last_signals = vec![out];
                }
                // Cache raw inputs so the body renderer can draw the current-position dot.
                "module.response_curve" => {
                    node.extra.last_signals = inputs;
                }
                _ => {}
            }
        }
    }
}

fn compute_delay(
    inputs: &[Option<Signal>],
    extra: &mut crate::canvas::node::NodeExtra,
    params: &HashMap<String, serde_json::Value>,
) -> Option<Signal> {
    let v = sig_to_f32(inputs.first().copied().flatten())?;
    let delay_secs = params
        .get("delay_ms")
        .and_then(|v| v.as_f64())
        .unwrap_or(100.0)
        .clamp(0.0, 60_000.0) as f32 / 1000.0;

    let now = std::time::Instant::now();
    extra.delay_buf.push_back((now, v));

    // Find the most-recent sample that is at least delay_secs old.
    let mut output = extra.delay_buf.front().map(|(_, v)| *v); // pre-fill
    for (ts, val) in extra.delay_buf.iter() {
        if now.duration_since(*ts).as_secs_f32() >= delay_secs {
            output = Some(*val);
        } else {
            break;
        }
    }

    // Trim samples more than delay + 1 s old (keep at least 2 for continuity).
    let max_age = delay_secs + 1.0;
    while extra.delay_buf.len() > 2 {
        let oldest_age = now
            .duration_since(extra.delay_buf.front().unwrap().0)
            .as_secs_f32();
        if oldest_age > max_age {
            extra.delay_buf.pop_front();
        } else {
            break;
        }
    }

    output.map(Signal::Float)
}

fn compute_lowpass(
    inputs: &[Option<Signal>],
    extra: &mut crate::canvas::node::NodeExtra,
    params: &HashMap<String, serde_json::Value>,
    dt: f32,
) -> Option<Signal> {
    let x = sig_to_f32(inputs.first().copied().flatten())? as f64;

    let cutoff = params
        .get("cutoff_hz")
        .and_then(|v| v.as_f64())
        .unwrap_or(10.0)
        .clamp(0.1, 1000.0);
    // Clamp Q ≤ 1/√2 to guarantee no resonance / amplification.
    let q = params
        .get("q")
        .and_then(|v| v.as_f64())
        .unwrap_or(0.707)
        .clamp(0.1, std::f64::consts::FRAC_1_SQRT_2);
    let fs = (1.0 / dt as f64).clamp(10.0, 10_000.0);

    // 2nd-order Butterworth lowpass (Audio EQ Cookbook).
    let w0 = 2.0 * std::f64::consts::PI * cutoff / fs;
    let alpha = w0.sin() / (2.0 * q);
    let cos_w0 = w0.cos();
    let a0 = 1.0 + alpha;
    let b0 = (1.0 - cos_w0) * 0.5 / a0;
    let b1 = (1.0 - cos_w0) / a0;
    let b2 = b0;
    let a1 = -2.0 * cos_w0 / a0;
    let a2 = (1.0 - alpha) / a0;

    let [x1, x2, y1, y2] = extra.filter_state;
    let y = b0 * x + b1 * x1 + b2 * x2 - a1 * y1 - a2 * y2;
    extra.filter_state = [x, x1, y, y1];

    Some(Signal::Float(y as f32))
}

fn biases_from_params(node: &NodeData) -> Vec<f32> {
    node.params
        .get("biases")
        .and_then(|v| v.as_array())
        .map(|arr| arr.iter().filter_map(|b| b.as_f64().map(|f| f as f32)).collect())
        .unwrap_or_default()
}

fn curve_points_from_params(node: &NodeData) -> Vec<[f32; 2]> {
    let absolute = node.params.get("absolute").and_then(|v| v.as_bool()).unwrap_or(true);
    node.params
        .get("points")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|pt| {
                    let a = pt.as_array()?;
                    Some([a.get(0)?.as_f64()? as f32, a.get(1)?.as_f64()? as f32])
                })
                .collect()
        })
        .unwrap_or_else(|| {
            if absolute { vec![[0.0, 0.0], [1.0, 1.0]] }
            else        { vec![[-1.0, -1.0], [1.0, 1.0]] }
        })
}

fn apply_curve(
    x: f32,
    pts: &[[f32; 2]],
    biases: &[f32],
    absolute: bool,
    in_min: f32, in_max: f32,
    out_min: f32, out_max: f32,
    in_scale: i64,
) -> f32 {
    if absolute {
        let sign      = if x < 0.0 { -1.0f32 } else { 1.0 };
        let abs_max   = in_max.abs().max(in_min.abs()).max(f32::EPSILON);
        let abs_norm  = (x.abs() / abs_max).clamp(0.0, 1.0);
        let scaled    = curve_scale(abs_norm, in_scale);
        let curve_y   = sample_curve(pts, scaled, biases).clamp(0.0, 1.0);
        let out_y     = curve_scale_inv(curve_y, in_scale);
        let out_scale = out_max.abs().max(out_min.abs());
        sign * out_y * out_scale
    } else {
        let in_range  = (in_max - in_min).abs().max(f32::EPSILON);
        let out_range = out_max - out_min;
        let norm      = ((x - in_min) / in_range * 2.0 - 1.0).clamp(-1.0, 1.0);
        let sign      = if norm < 0.0 { -1.0f32 } else { 1.0 };
        let scaled    = sign * curve_scale(norm.abs(), in_scale);
        let curve_y   = sample_curve(pts, scaled, biases);
        let sign_out  = if curve_y < 0.0 { -1.0f32 } else { 1.0 };
        let out_y     = sign_out * curve_scale_inv(curve_y.abs(), in_scale);
        out_min + (out_y.clamp(-1.0, 1.0) + 1.0) * 0.5 * out_range
    }
}

fn curve_scale(x: f32, mode: i64) -> f32 {
    use std::f32::consts::E;
    match mode {
        1 => (1.0 + x * (E - 1.0)).ln(),
        2 => (x.exp() - 1.0) / (E - 1.0),
        _ => x,
    }
}

fn curve_scale_inv(y: f32, mode: i64) -> f32 {
    use std::f32::consts::E;
    match mode {
        1 => (y.exp() - 1.0) / (E - 1.0),
        2 => (1.0 + y * (E - 1.0)).ln(),
        _ => y,
    }
}

// ── Display node history update ───────────────────────────────────────────────

const DISPLAY_IDS: &[&str] = &[
    "display.readout",
    "display.oscilloscope",
    "display.vectorscope",
];
const HISTORY_LEN: usize = 512;

fn update_display_nodes(
    snarl: &mut Snarl<NodeData>,
    dev_sigs: &HashMap<(String, String), Signal>,
) {
    let node_ids: Vec<NodeId> = snarl
        .nodes_ids_data()
        .filter(|(_, n)| DISPLAY_IDS.contains(&n.value.module_id.as_str()))
        .map(|(id, _)| id)
        .collect();

    for node_id in node_ids {
        let n_inputs = snarl.get_node(node_id).map(|n| n.inputs.len()).unwrap_or(0);

        let vals: Vec<Option<Signal>> = (0..n_inputs)
            .map(|i| {
                let pin = snarl.in_pin(InPinId { node: node_id, input: i });
                pin.remotes.first().and_then(|&src| {
                    eval_output(snarl, src.node, src.output, dev_sigs, 0)
                })
            })
            .collect();

        if let Some(node) = snarl.get_node_mut(node_id) {
            // Store for readout body rendering
            node.extra.last_signals = vals.clone();

            // Append one sample to the history ring buffer
            let sample = [
                sig_to_f32(vals.get(0).copied().flatten()),
                sig_to_f32(vals.get(1).copied().flatten()),
                sig_to_f32(vals.get(2).copied().flatten()),
                sig_to_f32(vals.get(3).copied().flatten()),
            ];
            if node.extra.history.len() >= HISTORY_LEN {
                node.extra.history.pop_front();
            }
            node.extra.history.push_back(sample);
        }
    }
}

fn sig_to_f32(s: Option<Signal>) -> Option<f32> {
    match s {
        Some(Signal::Float(f)) => Some(f),
        Some(Signal::Bool(b))  => Some(if b { 1.0 } else { 0.0 }),
        Some(Signal::Vec2(v))  => Some(v.length()),
        Some(Signal::Int(i))   => Some(i as f32),
        None => None,
    }
}
