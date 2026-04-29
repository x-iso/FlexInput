use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, RwLock};
use std::sync::atomic::{AtomicBool, Ordering};

use eframe::egui;
use egui_snarl::{InPinId, NodeId, Snarl};
use flexinput_core::{ModuleDescriptor, Signal, SignalType};
use flexinput_core::PinDescriptor;
use flexinput_devices::{init_backends, midi::cc_display_name, DeviceBackend, HidHideClient, MidiBackend, PhysicalDevice};
use flexinput_engine::{Engine, NodeSnap, ProcessingGraph, ProcessingOutput, SinkBus, spawn_processing_thread};
use flexinput_modules::all_modules;
use flexinput_virtual::VirtualDevice;

use crate::{
    canvas::{sample_curve, Canvas, NodeData},
    panels::{physical_devices, virtual_devices::VirtualDevicePanel},
};

fn setup_fonts(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();
    #[cfg(windows)]
    {
        if let Ok(bytes) = std::fs::read(r"C:\Windows\Fonts\segoeui.ttf") {
            fonts.font_data.insert("segoe_ui".to_owned(), std::sync::Arc::new(egui::FontData::from_owned(bytes)));
            for family in fonts.families.values_mut() {
                family.push("segoe_ui".to_owned());
            }
        }
        // Segoe UI Symbol provides arrows/symbols (↶ ↷) not covered by Segoe UI.
        if let Ok(bytes) = std::fs::read(r"C:\Windows\Fonts\seguisym.ttf") {
            fonts.font_data.insert("segoe_sym".to_owned(), std::sync::Arc::new(egui::FontData::from_owned(bytes)));
            for family in fonts.families.values_mut() {
                family.push("segoe_sym".to_owned());
            }
        }
    }
    ctx.set_fonts(fonts);
}

pub struct PatchTab {
    pub title: String,
    pub file_path: Option<std::path::PathBuf>,
    /// Exe filenames that auto-switch to this tab (e.g. `["game.exe", "launcher.exe"]`).
    pub bound_exes: Vec<String>,
    pub canvas: Canvas,
    pub virtual_panel: VirtualDevicePanel,
    /// Manual bypass: stop sending output from this tab's virtual/physical sinks.
    pub bypassed: bool,
    /// Auto-bypass: suppress output whenever no bound process is in the foreground.
    pub auto_bypass: bool,
}

impl PatchTab {
    fn new_untitled(n: u32) -> Self {
        Self {
            title: if n == 1 { "Untitled".to_string() } else { format!("Untitled {}", n) },
            file_path: None,
            bound_exes: vec![],
            canvas: Canvas::new(),
            virtual_panel: VirtualDevicePanel::new(),
            bypassed: false,
            auto_bypass: false,
        }
    }
}

pub struct FlexInputApp {
    engine: Engine,
    tabs: Vec<PatchTab>,
    active_tab: usize,
    next_untitled: u32,
    descriptors: Vec<ModuleDescriptor>,
    /// MIDI backend shared with the I/O thread (UI uses it for CC learning).
    midi_backend: Arc<Mutex<Option<MidiBackend>>>,
    /// Physical device list refreshed by the I/O thread; UI reads for display.
    devices: Vec<PhysicalDevice>,
    shared_devices: Arc<RwLock<Vec<PhysicalDevice>>>,
    /// Latest raw device signals (written by I/O thread at 500 Hz); used for canvas display.
    last_signals: HashMap<(String, String), Signal>,
    eval_cache: HashMap<(NodeId, usize), Option<Signal>>,
    logo_texture: Option<egui::TextureHandle>,
    hidhide: Option<HidHideClient>,
    last_update: std::time::Instant,
    bottom_panel_height: f32,
    /// Whether to automatically switch to the tab whose bound_exe matches the foreground process.
    auto_switch: bool,
    /// Last foreground exe seen, used to avoid redundant switches.
    last_fg_exe: String,
    /// Whether the bind-to-process picker window is open.
    bind_window_open: bool,
    /// Search filter string for the bind window.
    bind_window_filter: String,
    /// Cached process list shown in the bind window.
    bind_window_procs: Vec<(String, String)>,
    /// Whether the HidHide configuration window is open.
    hidhide_window_open: bool,
    /// Search filter for the HidHide process picker.
    hidhide_filter: String,
    /// Running process list (full_path, exe_name, title) for HidHide whitelist picker.
    hidhide_proc_list: Vec<(String, String, String)>,
    /// Cached whitelist read from the HidHide driver; refreshed on window open and after edits.
    hidhide_whitelist: Vec<String>,
    // ── Processing thread shared state ────────────────────────────────────────
    proc_graph: Arc<RwLock<ProcessingGraph>>,
    proc_device_signals: Arc<RwLock<HashMap<(String, String), Signal>>>,
    proc_outputs: Arc<Mutex<ProcessingOutput>>,
    // ── I/O thread shared state ───────────────────────────────────────────────
    /// Points to the active tab's device list Arc so the I/O thread always dispatches
    /// to the right set of virtual devices without needing to know the active tab index.
    io_device_list: Arc<RwLock<Arc<Mutex<Vec<Box<dyn VirtualDevice>>>>>>,
    /// Bypass flag: when true the I/O thread calls reset_outputs() instead of flush().
    io_bypass: Arc<AtomicBool>,
}

impl FlexInputApp {
    pub fn new(cc: &eframe::CreationContext<'_>, icon_bytes: &[u8]) -> Self {
        setup_fonts(&cc.egui_ctx);
        let descriptors = all_modules().into_iter().map(|r| r.descriptor).collect();
        let backends    = init_backends();
        let midi_backend = Arc::new(Mutex::new(Some(MidiBackend::new())));
        // HidHide integration disabled pending a proper rewrite.
        let hidhide: Option<HidHideClient> = None;
        let logo_texture = eframe::icon_data::from_png_bytes(icon_bytes).ok().map(|icon| {
            let image = egui::ColorImage::from_rgba_unmultiplied(
                [icon.width as usize, icon.height as usize],
                &icon.rgba,
            );
            cc.egui_ctx.load_texture("app_logo", image, egui::TextureOptions::LINEAR)
        });

        let proc_graph          = Arc::new(RwLock::new(ProcessingGraph::default()));
        let proc_device_signals = Arc::new(RwLock::new(HashMap::<(String, String), Signal>::new()));
        let proc_outputs        = Arc::new(Mutex::new(ProcessingOutput::default()));
        let sink_bus: SinkBus   = Arc::new(RwLock::new(HashMap::new()));
        spawn_processing_thread(
            Arc::clone(&proc_graph),
            Arc::clone(&proc_device_signals),
            Arc::clone(&proc_outputs),
            Arc::clone(&sink_bus),
        );

        let tabs = vec![PatchTab::new_untitled(1)];
        let shared_devices = Arc::new(RwLock::new(Vec::<PhysicalDevice>::new()));
        let io_bypass      = Arc::new(AtomicBool::new(false));
        // Point io_device_list at the first (active) tab's device Arc.
        let io_device_list = Arc::new(RwLock::new(
            Arc::clone(&tabs[0].virtual_panel.active),
        ));

        spawn_io_thread(
            backends,
            Arc::clone(&midi_backend),
            Arc::clone(&proc_device_signals),
            Arc::clone(&sink_bus),
            Arc::clone(&io_device_list),
            Arc::clone(&io_bypass),
            Arc::clone(&shared_devices),
        );

        Self {
            engine: Engine::new(),
            tabs,
            active_tab: 0,
            next_untitled: 2,
            descriptors,
            midi_backend,
            devices: vec![],
            shared_devices,
            last_signals: HashMap::new(),
            eval_cache: HashMap::new(),
            logo_texture,
            hidhide,
            last_update: std::time::Instant::now(),
            bottom_panel_height: 220.0,
            auto_switch: false,
            last_fg_exe: String::new(),
            bind_window_open: false,
            bind_window_filter: String::new(),
            bind_window_procs: vec![],
            hidhide_window_open: false,
            hidhide_filter: String::new(),
            hidhide_proc_list: vec![],
            hidhide_whitelist: vec![],
            proc_graph,
            proc_device_signals,
            proc_outputs,
            io_device_list,
            io_bypass,
        }
    }
}

impl eframe::App for FlexInputApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        let dt = self.last_update.elapsed().as_secs_f32().clamp(0.001, 0.1);
        self.last_update = std::time::Instant::now();

        // Read the latest device signals written by the I/O thread (500 Hz).
        self.last_signals = self.proc_device_signals.read().unwrap().clone();
        // Refresh device list from I/O thread and append live MIDI devices.
        self.devices = self.shared_devices.read().unwrap().clone();
        if let Ok(mut midi_g) = self.midi_backend.try_lock() {
            if let Some(m) = midi_g.as_mut() {
                self.devices.extend(m.enumerate());
            }
        }

        // Feed learned CCs into the active tab's canvas nodes.
        {
            let snarl = &mut self.tabs[self.active_tab].canvas.snarl;
            if let Ok(mut midi_g) = self.midi_backend.try_lock() {
            if let Some(midi) = midi_g.as_mut() {
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
            }} // close if let Some(midi) + if let Ok(midi_g)
        }

        self.engine.tick();

        // Update foreground tracking for auto-switch (auto-bypass is gated on auto_switch too).
        if self.auto_switch {
            if let Some(fg_exe) = crate::process_list::foreground_exe() {
                if fg_exe != self.last_fg_exe {
                    self.last_fg_exe = fg_exe.clone();
                }
                if let Some(idx) = self.tabs.iter().position(|t| {
                    t.bound_exes.iter().any(|b| b.eq_ignore_ascii_case(&self.last_fg_exe))
                }) {
                    self.set_active_tab(idx);
                }
            }
        }

        // Effective bypass: manual toggle OR (auto mode on AND auto-bypass AND bound process not in focus).
        let effective_bypass: Vec<bool> = self.tabs.iter().map(|tab| {
            tab.bypassed || (
                self.auto_switch
                    && tab.auto_bypass
                    && !tab.bound_exes.is_empty()
                    && !tab.bound_exes.iter().any(|b| b.eq_ignore_ascii_case(&self.last_fg_exe))
            )
        }).collect();

        let canvas_has_nodes = self.tabs[self.active_tab].canvas.snarl.nodes_ids_data().next().is_some();

        // Push a fresh graph snapshot to the processing thread each frame.
        {
            let (graph_snap, dirty_uids) = {
                let snarl = &self.tabs[self.active_tab].canvas.snarl;
                build_processing_graph(snarl)
            };
            *self.proc_graph.write().unwrap() = graph_snap;
            if !dirty_uids.is_empty() {
                let snarl = &mut self.tabs[self.active_tab].canvas.snarl;
                for (id, node_ref) in snarl.nodes_ids_data_mut() {
                    if dirty_uids.contains(&id.0) {
                        node_ref.value.extra.aux_f32_dirty = false;
                    }
                }
            }
        }

        // Pull outputs from the processing thread: pre-populate eval_cache, sync display state.
        self.eval_cache.clear();
        if canvas_has_nodes {
            let uid_map: HashMap<usize, NodeId> = self.tabs[self.active_tab].canvas.snarl
                .nodes_ids_data().map(|(id, _)| (id.0, id)).collect();

            let scope_batch = {
                let mut out = self.proc_outputs.lock().unwrap();
                for (&(uid, pin), &sig) in &out.node_outputs {
                    self.eval_cache.insert((NodeId(uid), pin), sig);
                }
                for (&uid, sigs) in &out.last_inputs {
                    if let Some(&nid) = uid_map.get(&uid) {
                        if let Some(node) = self.tabs[self.active_tab].canvas.snarl.get_node_mut(nid) {
                            node.extra.last_signals = sigs.clone();
                        }
                    }
                }
                std::mem::take(&mut out.scope_pending)
            };
            for (uid, sample) in scope_batch {
                if let Some(&nid) = uid_map.get(&uid) {
                    if let Some(node) = self.tabs[self.active_tab].canvas.snarl.get_node_mut(nid) {
                        let h = &mut node.extra.history;
                        if h.len() >= HISTORY_LEN { h.pop_front(); }
                        h.push_back(sample);
                    }
                }
            }
        }

        // Signal routing and device flushing are handled by the 500 Hz I/O thread.
        // Just keep the bypass flag in sync so the I/O thread knows whether to reset outputs.
        self.io_bypass.store(effective_bypass[self.active_tab], Ordering::Relaxed);

        // ── Custom title bar ──────────────────────────────────────────────────────
        let mut do_save = false;
        let mut do_load = false;
        let mut do_new  = false;
        let mut do_close = false;
        let mut do_bind  = false;
        let mut do_hidhide = false;
        let mut do_undo = false;
        let mut do_redo = false;
        let can_undo = self.tabs[self.active_tab].canvas.can_undo();
        let can_redo = self.tabs[self.active_tab].canvas.can_redo();
        let title_frame = egui::Frame::NONE.fill(ctx.style().visuals.panel_fill);
        egui::TopBottomPanel::top("title_bar")
            .exact_height(32.0)
            .frame(title_frame)
            .show(ctx, |ui| {
                show_title_bar(
                    ui, ctx,
                    &mut do_save, &mut do_load, &mut do_new, &mut do_close, &mut do_bind,
                    &mut do_hidhide,
                    &mut self.auto_switch,
                    &mut do_undo, &mut do_redo,
                    can_undo, can_redo,
                    &self.logo_texture,
                );
            });

        // ── Tab bar ───────────────────────────────────────────────────────────────
        let tab_bar_frame = egui::Frame::NONE.fill(ctx.style().visuals.widgets.noninteractive.bg_fill);
        let (tab_switch, tab_close_idx, tab_new, bypass_toggle_idx) = egui::TopBottomPanel::top("tab_bar")
            .exact_height(28.0)
            .frame(tab_bar_frame)
            .show(ctx, |ui| show_tab_bar(ui, &self.tabs, self.active_tab, &effective_bypass))
            .inner;
        do_new = do_new || tab_new;
        if let Some(idx) = bypass_toggle_idx {
            if idx < self.tabs.len() {
                // Any manual bypass action disengages auto mode.
                self.auto_switch = false;
                if effective_bypass[idx] {
                    // Turn off bypass: clear manual first; if only auto-bypass was active, disable it.
                    if self.tabs[idx].bypassed {
                        self.tabs[idx].bypassed = false;
                    } else {
                        self.tabs[idx].auto_bypass = false;
                    }
                } else {
                    self.tabs[idx].bypassed = true;
                }
            }
        }

        // Open the bind-to-process picker.
        if do_bind {
            self.bind_window_open = true;
            self.bind_window_filter.clear();
            self.bind_window_procs = crate::process_list::enumerate_windows();
        }

        // Open the HidHide configuration window.
        if do_hidhide {
            self.hidhide_window_open = true;
            self.hidhide_filter.clear();
            self.hidhide_proc_list = crate::process_list::enumerate_processes_full();
            if let Some(hh) = &self.hidhide {
                self.hidhide_whitelist = hh.whitelist();
            }
        }

        // ── Bind-to-process window ────────────────────────────────────────────────
        if self.bind_window_open {
            let mut open = true;
            let active_idx = self.active_tab;
            let tab_title = self.tabs[active_idx].title.clone();

            egui::Window::new(format!("Bind \"{tab_title}\" to process"))
                .id(egui::Id::new("bind_proc_window"))
                .collapsible(false)
                .resizable(true)
                .default_size([380.0, 440.0])
                .max_size(egui::vec2(480.0, 640.0))
                .open(&mut open)
                .show(ctx, |ui| {
                    // ── Current bindings as removable chips ───────────────────
                    let bound_exes = self.tabs[active_idx].bound_exes.clone();
                    if bound_exes.is_empty() {
                        ui.weak("No bindings — click a process below to add one.");
                    } else {
                        ui.horizontal_wrapped(|ui| {
                            let mut remove_idx: Option<usize> = None;
                            for (i, exe) in bound_exes.iter().enumerate() {
                                egui::Frame::default()
                                    .inner_margin(egui::Margin::symmetric(6, 2))
                                    .corner_radius(8.0)
                                    .fill(ui.visuals().widgets.inactive.bg_fill)
                                    .show(ui, |ui| {
                                        ui.horizontal(|ui| {
                                            ui.spacing_mut().item_spacing.x = 4.0;
                                            ui.label(exe.as_str());
                                            let (rect, resp) = ui.allocate_exact_size(
                                                egui::vec2(14.0, 14.0), egui::Sense::click());
                                            if resp.hovered() {
                                                ui.painter().circle_filled(
                                                    rect.center(), 7.0,
                                                    ui.visuals().widgets.hovered.bg_fill);
                                            }
                                            let c = rect.center();
                                            let d = 3.2_f32;
                                            let s = egui::Stroke::new(1.2, ui.visuals().text_color());
                                            ui.painter().line_segment(
                                                [egui::pos2(c.x-d, c.y-d), egui::pos2(c.x+d, c.y+d)], s);
                                            ui.painter().line_segment(
                                                [egui::pos2(c.x+d, c.y-d), egui::pos2(c.x-d, c.y+d)], s);
                                            if resp.clicked() { remove_idx = Some(i); }
                                        });
                                    });
                            }
                            if let Some(i) = remove_idx {
                                self.tabs[active_idx].bound_exes.remove(i);
                            }
                        });
                    }
                    ui.add_space(4.0);
                    ui.checkbox(
                        &mut self.tabs[active_idx].auto_bypass,
                        "Auto-bypass when bound process is not in focus",
                    );
                    ui.separator();

                    // ── Filter + refresh ──────────────────────────────────────
                    ui.horizontal(|ui| {
                        ui.label("Filter:");
                        ui.add(egui::TextEdit::singleline(&mut self.bind_window_filter)
                            .desired_width(ui.available_width() - 64.0));
                        if ui.button("Refresh").clicked() {
                            self.bind_window_procs = crate::process_list::enumerate_windows();
                        }
                    });
                    ui.add_space(4.0);

                    // ── Process list ──────────────────────────────────────────
                    // Clicking a row toggles the binding (adds or removes).
                    let filter = self.bind_window_filter.to_lowercase();
                    let row_h = 38.0_f32;
                    egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
                        ui.set_min_width(0.0);
                        let mut toggle_exe: Option<String> = None;
                        for (exe, title) in &self.bind_window_procs {
                            if !filter.is_empty()
                                && !exe.to_lowercase().contains(&filter)
                                && !title.to_lowercase().contains(&filter)
                            {
                                continue;
                            }
                            let is_bound = self.tabs[active_idx].bound_exes.iter()
                                .any(|b| b.eq_ignore_ascii_case(exe));

                            let (rect, resp) = ui.allocate_exact_size(
                                egui::vec2(ui.available_width(), row_h),
                                egui::Sense::click(),
                            );
                            let fill = if is_bound {
                                ui.visuals().selection.bg_fill.gamma_multiply(0.5)
                            } else if resp.hovered() {
                                ui.visuals().widgets.hovered.bg_fill
                            } else {
                                egui::Color32::TRANSPARENT
                            };
                            if fill != egui::Color32::TRANSPARENT {
                                ui.painter().rect_filled(rect, egui::CornerRadius::ZERO, fill);
                            }
                            // Checkmark for bound entries
                            let text_x = if is_bound {
                                let c = egui::pos2(rect.left() + 12.0, rect.center().y);
                                let s = egui::Stroke::new(1.5, ui.visuals().selection.stroke.color);
                                ui.painter().line_segment(
                                    [egui::pos2(c.x - 4.0, c.y), egui::pos2(c.x - 1.0, c.y + 3.5)], s);
                                ui.painter().line_segment(
                                    [egui::pos2(c.x - 1.0, c.y + 3.5), egui::pos2(c.x + 5.0, c.y - 4.0)], s);
                                rect.left() + 24.0
                            } else {
                                rect.left() + 8.0
                            };
                            let top = egui::pos2(text_x, rect.top() + 5.0);
                            let bot = egui::pos2(text_x, rect.top() + 22.0);
                            ui.painter().text(top, egui::Align2::LEFT_TOP,
                                title, egui::FontId::proportional(13.0), ui.visuals().text_color());
                            ui.painter().text(bot, egui::Align2::LEFT_TOP,
                                exe, egui::FontId::proportional(11.0), ui.visuals().weak_text_color());

                            if resp.clicked() { toggle_exe = Some(exe.clone()); }
                        }
                        if let Some(exe) = toggle_exe {
                            let tab = &mut self.tabs[active_idx];
                            if let Some(pos) = tab.bound_exes.iter().position(|b| b.eq_ignore_ascii_case(&exe)) {
                                tab.bound_exes.remove(pos);
                            } else {
                                tab.bound_exes.push(exe);
                            }
                        }
                        if self.bind_window_procs.is_empty() {
                            ui.weak("No windows found.");
                        }
                    });
                });

            if !open {
                self.bind_window_open = false;
            }
        }

        // ── HidHide configuration window ──────────────────────────────────────────
        if self.hidhide_window_open {
            let mut open = true;

            // Collect deferred mutations; applied after the closure to avoid borrow conflicts.
            let mut toggle_to_active: Option<bool> = None;
            let mut remove_idx: Option<usize> = None;
            let mut add_path: Option<String> = None;

            egui::Window::new("HidHide")
                .id(egui::Id::new("hidhide_window"))
                .collapsible(false)
                .resizable(true)
                .default_size([420.0, 500.0])
                .max_size(egui::vec2(600.0, 700.0))
                .open(&mut open)
                .show(ctx, |ui| {
                    if self.hidhide.is_none() {
                        ui.add_space(8.0);
                        ui.label("HidHide driver not found.");
                        ui.add_space(4.0);
                        ui.weak("Install HidHide to enable per-process device hiding.");
                        return;
                    }

                    // ── Active toggle ─────────────────────────────────────────
                    let hh_ref = self.hidhide.as_ref().unwrap();
                    let is_active = hh_ref.is_active();
                    let act_label = if is_active { "Active ●" } else { "Active ○" };
                    let act_hover = if is_active {
                        "HidHide is active — listed devices are hidden from non-whitelisted apps"
                    } else {
                        "HidHide is inactive — all devices are visible to all apps"
                    };
                    let act_resp = ui.add(egui::Button::new(act_label).selected(is_active))
                        .on_hover_text(act_hover);
                    if act_resp.clicked() {
                        toggle_to_active = Some(!is_active);
                    }

                    // ── Last-operation status (diagnostic) ────────────────────
                    let last_status = ui.ctx().memory(|m| {
                        m.data.get_temp::<String>(egui::Id::new("hidhide_last_status"))
                    });
                    if let Some(status) = last_status {
                        ui.add_space(4.0);
                        ui.label(
                            egui::RichText::new(format!("Last hide/show: {}", status))
                                .small().monospace()
                        );
                    }

                    ui.separator();

                    // ── Whitelist chips ───────────────────────────────────────
                    ui.label("Whitelisted Applications");
                    ui.add_space(4.0);

                    let own_exe_upper = HidHideClient::current_exe_path()
                        .map(|p| p.to_uppercase())
                        .unwrap_or_default();

                    if self.hidhide_whitelist.is_empty() {
                        ui.weak("No processes whitelisted.");
                    } else {
                        ui.horizontal_wrapped(|ui| {
                            for (i, path) in self.hidhide_whitelist.iter().enumerate() {
                                let is_own = path.to_uppercase() == own_exe_upper;
                                let chip_label = std::path::Path::new(path)
                                    .file_name()
                                    .map(|n| n.to_string_lossy().into_owned())
                                    .unwrap_or_else(|| path.clone());

                                egui::Frame::default()
                                    .inner_margin(egui::Margin::symmetric(6, 2))
                                    .corner_radius(8.0)
                                    .fill(ui.visuals().widgets.inactive.bg_fill)
                                    .show(ui, |ui| {
                                        ui.horizontal(|ui| {
                                            ui.spacing_mut().item_spacing.x = 4.0;
                                            if is_own {
                                                ui.label("\u{1F512}"); // 🔒
                                            }
                                            ui.label(&chip_label).on_hover_text(path.as_str());
                                            if !is_own {
                                                let (rect, resp) = ui.allocate_exact_size(
                                                    egui::vec2(14.0, 14.0),
                                                    egui::Sense::click(),
                                                );
                                                if resp.hovered() {
                                                    ui.painter().circle_filled(
                                                        rect.center(), 7.0,
                                                        ui.visuals().widgets.hovered.bg_fill,
                                                    );
                                                }
                                                let c = rect.center();
                                                let d = 3.2_f32;
                                                let s = egui::Stroke::new(1.2, ui.visuals().text_color());
                                                ui.painter().line_segment(
                                                    [egui::pos2(c.x - d, c.y - d), egui::pos2(c.x + d, c.y + d)], s);
                                                ui.painter().line_segment(
                                                    [egui::pos2(c.x + d, c.y - d), egui::pos2(c.x - d, c.y + d)], s);
                                                if resp.clicked() {
                                                    remove_idx = Some(i);
                                                }
                                            }
                                        });
                                    });
                            }
                        });
                    }

                    ui.separator();

                    // ── Blacklist (hidden devices) ────────────────────────────
                    ui.label("Hidden Devices (Blacklist)");
                    ui.add_space(4.0);
                    let blacklist = hh_ref.blacklist();
                    if blacklist.is_empty() {
                        ui.weak("No devices hidden.");
                    } else {
                        egui::ScrollArea::vertical()
                            .id_salt("hidhide_blacklist_scroll")
                            .max_height(120.0)
                            .show(ui, |ui| {
                                for path in &blacklist {
                                    ui.label(
                                        egui::RichText::new(path).small().monospace()
                                    );
                                }
                            });
                    }

                    ui.add_space(4.0);
                    ui.horizontal(|ui| {
                        if ui.button("Test write")
                            .on_hover_text("Writes a fixed test string to the blacklist. \
                                Bypasses device detection — purely tests IOCTL_SET_BLACKLIST.")
                            .clicked()
                        {
                            let status = hh_ref.set_hidden("FLEXINPUT_TEST_PATH_ABCD", true);
                            ui.ctx().memory_mut(|m| {
                                m.data.insert_temp(
                                    egui::Id::new("hidhide_last_status"),
                                    status,
                                );
                            });
                        }
                        if ui.button("Test remove")
                            .on_hover_text("Removes the test path from the blacklist.")
                            .clicked()
                        {
                            let status = hh_ref.set_hidden("FLEXINPUT_TEST_PATH_ABCD", false);
                            ui.ctx().memory_mut(|m| {
                                m.data.insert_temp(
                                    egui::Id::new("hidhide_last_status"),
                                    status,
                                );
                            });
                        }
                    });

                    ui.separator();

                    // ── Add from running processes ────────────────────────────
                    ui.label("Add from running applications:");
                    ui.add_space(4.0);

                    ui.horizontal(|ui| {
                        ui.label("Filter:");
                        ui.add(egui::TextEdit::singleline(&mut self.hidhide_filter)
                            .desired_width(ui.available_width() - 64.0));
                        if ui.button("Refresh").clicked() {
                            self.hidhide_proc_list = crate::process_list::enumerate_processes_full();
                        }
                    });
                    ui.add_space(4.0);

                    if self.hidhide_proc_list.is_empty() {
                        ui.weak("Click Refresh to load running processes.");
                    } else {
                        let filter = self.hidhide_filter.to_lowercase();
                        let wl_upper: Vec<String> = self.hidhide_whitelist
                            .iter().map(|s| s.to_uppercase()).collect();
                        let row_h = 38.0_f32;

                        egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
                            ui.set_min_width(0.0);
                            for (full_path, exe_name, title) in &self.hidhide_proc_list {
                                if !filter.is_empty()
                                    && !exe_name.to_lowercase().contains(&filter)
                                    && !title.to_lowercase().contains(&filter)
                                {
                                    continue;
                                }

                                let is_listed = wl_upper.contains(&full_path.to_uppercase());

                                let (rect, resp) = ui.allocate_exact_size(
                                    egui::vec2(ui.available_width(), row_h),
                                    egui::Sense::click(),
                                );
                                let fill = if is_listed {
                                    ui.visuals().selection.bg_fill.gamma_multiply(0.5)
                                } else if resp.hovered() {
                                    ui.visuals().widgets.hovered.bg_fill
                                } else {
                                    egui::Color32::TRANSPARENT
                                };
                                if fill != egui::Color32::TRANSPARENT {
                                    ui.painter().rect_filled(rect, egui::CornerRadius::ZERO, fill);
                                }

                                let text_x = if is_listed {
                                    let c = egui::pos2(rect.left() + 12.0, rect.center().y);
                                    let s = egui::Stroke::new(1.5, ui.visuals().selection.stroke.color);
                                    ui.painter().line_segment(
                                        [egui::pos2(c.x - 4.0, c.y), egui::pos2(c.x - 1.0, c.y + 3.5)], s);
                                    ui.painter().line_segment(
                                        [egui::pos2(c.x - 1.0, c.y + 3.5), egui::pos2(c.x + 5.0, c.y - 4.0)], s);
                                    rect.left() + 24.0
                                } else {
                                    rect.left() + 8.0
                                };

                                let top = egui::pos2(text_x, rect.top() + 5.0);
                                let bot = egui::pos2(text_x, rect.top() + 22.0);
                                ui.painter().text(top, egui::Align2::LEFT_TOP,
                                    title, egui::FontId::proportional(13.0),
                                    ui.visuals().text_color());
                                ui.painter().text(bot, egui::Align2::LEFT_TOP,
                                    exe_name, egui::FontId::proportional(11.0),
                                    ui.visuals().weak_text_color());

                                if resp.clicked() {
                                    add_path = Some(full_path.clone());
                                }
                            }
                        });
                    }
                });

            // Apply deferred mutations (after closure — avoids borrow conflicts).
            if let Some(hh) = &self.hidhide {
                if let Some(active) = toggle_to_active {
                    hh.set_active(active);
                }
                if let Some(i) = remove_idx {
                    self.hidhide_whitelist.remove(i);
                    hh.set_whitelist(&self.hidhide_whitelist.clone());
                }
                if let Some(path) = add_path {
                    let upper = path.to_uppercase();
                    let own_upper = HidHideClient::current_exe_path()
                        .map(|p| p.to_uppercase())
                        .unwrap_or_default();
                    if self.hidhide_whitelist.iter().any(|s| s.to_uppercase() == upper) {
                        // Toggle off (remove), but never remove FlexInput itself.
                        if upper != own_upper {
                            self.hidhide_whitelist.retain(|s| s.to_uppercase() != upper);
                            hh.set_whitelist(&self.hidhide_whitelist.clone());
                        }
                    } else {
                        self.hidhide_whitelist.push(path);
                        hh.set_whitelist(&self.hidhide_whitelist.clone());
                    }
                }
            }

            if !open {
                self.hidhide_window_open = false;
            }
        }

        // Close a specific tab from the tab bar X button.
        let close_idx = tab_close_idx.or(if do_close { Some(self.active_tab) } else { None });
        if let Some(idx) = close_idx {
            if self.tabs.len() == 1 {
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                return;
            }
            self.tabs.remove(idx);
            let new_idx = self.active_tab.saturating_sub(if self.active_tab > idx { 1 } else { 0 })
                .min(self.tabs.len() - 1);
            self.set_active_tab(new_idx);
        }

        // Switch active tab — manual tab click disengages auto mode.
        if let Some(idx) = tab_switch {
            if idx < self.tabs.len() {
                self.set_active_tab(idx);
                self.auto_switch = false;
            }
        }

        // New tab.
        if do_new {
            let n = self.next_untitled;
            self.next_untitled += 1;
            self.tabs.push(PatchTab::new_untitled(n));
            let new_idx = self.tabs.len() - 1;
            self.set_active_tab(new_idx);
        }

        // Undo / Redo from title bar buttons.
        if do_undo { self.tabs[self.active_tab].canvas.undo(); }
        if do_redo { self.tabs[self.active_tab].canvas.redo(); }

        // Save / Load operate on the active tab.
        if do_save {
            let vids = {
                let devs = self.tabs[self.active_tab].virtual_panel.active.lock().unwrap();
                devs.iter().map(|d| d.id().to_string()).collect()
            };
            let bound = self.tabs[self.active_tab].bound_exes.clone();
            let auto_bypass = self.tabs[self.active_tab].auto_bypass;
            if let Some(saved_path) = self.tabs[self.active_tab].canvas.save_patch(vids, bound, auto_bypass) {
                let tab = &mut self.tabs[self.active_tab];
                tab.title = saved_path.file_stem()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_else(|| "Untitled".to_string());
                tab.file_path = Some(saved_path);
            }
        }
        if do_load {
            if let Some((new_canvas, vids, bound, auto_bypass, path)) = crate::canvas::Canvas::load_patch() {
                let tab = &mut self.tabs[self.active_tab];
                tab.canvas = new_canvas;
                tab.title = path.file_stem()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_else(|| "Untitled".to_string());
                tab.file_path = Some(path);
                tab.bound_exes = bound;
                tab.auto_bypass = auto_bypass;
                {
                    let mut devs = tab.virtual_panel.active.lock().unwrap();
                    devs.clear();
                    for vid in &vids {
                        if let Some(dev) = try_create_virtual_device(vid) {
                            devs.push(dev);
                        }
                    }
                }
            }
        }

        // Build live device IDs for the active tab's canvas status dots.
        let live_device_ids: std::collections::HashSet<String> = {
            let virtual_live: Vec<String> = {
                let devs = self.tabs[self.active_tab].virtual_panel.active.lock().unwrap();
                devs.iter().filter(|d| d.is_connected()).map(|d| d.id().to_string()).collect()
            };
            self.devices.iter().map(|d| d.id.clone())
                .chain(virtual_live)
                .collect()
        };

        let devices = &self.devices;
        let hidhide = self.hidhide.as_ref();
        let bottom_panel_height = self.bottom_panel_height;
        let tab = &mut self.tabs[self.active_tab];
        let (virtual_panel, canvas) = (&mut tab.virtual_panel, &mut tab.canvas);

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

        let has_virtual = {
            let devs = self.tabs[self.active_tab].virtual_panel.active.lock().unwrap();
            !devs.is_empty()
        };
        let repaint_after = if canvas_has_nodes || has_virtual {
            std::time::Duration::from_millis(8)
        } else {
            std::time::Duration::from_millis(100)
        };
        ctx.request_repaint_after(repaint_after);

        handle_window_resize(ctx);
    }
}

impl FlexInputApp {
    /// Switch the active tab and update the I/O thread's device list pointer.
    fn set_active_tab(&mut self, idx: usize) {
        if idx == self.active_tab { return; }
        self.active_tab = idx;
        *self.io_device_list.write().unwrap() =
            Arc::clone(&self.tabs[idx].virtual_panel.active);
    }
}

// ── 500 Hz device I/O thread ──────────────────────────────────────────────────

fn spawn_io_thread(
    mut backends: Vec<Box<dyn DeviceBackend>>,
    midi: Arc<Mutex<Option<MidiBackend>>>,
    proc_device_signals: Arc<RwLock<HashMap<(String, String), Signal>>>,
    sink_bus: SinkBus,
    io_device_list: Arc<RwLock<Arc<Mutex<Vec<Box<dyn VirtualDevice>>>>>>,
    io_bypass: Arc<AtomicBool>,
    shared_devices: Arc<RwLock<Vec<PhysicalDevice>>>,
) {
    use std::time::{Duration, Instant};

    std::thread::Builder::new()
        .name("device-io-500hz".into())
        .spawn(move || {
            let interval = Duration::from_nanos(1_000_000_000 / 500);
            let mut last_enum = Instant::now() - Duration::from_secs(10);
            let mut last_midi_out: HashMap<(String, String), Signal> = HashMap::new();

            loop {
                let t0 = Instant::now();

                // ── Poll physical inputs ──────────────────────────────────────
                let mut signals: HashMap<(String, String), Signal> = HashMap::new();
                for backend in &mut backends {
                    for (dev, pin, sig) in backend.poll() {
                        signals.insert((dev, pin), sig);
                    }
                }
                if let Ok(mut mg) = midi.try_lock() {
                    if let Some(m) = mg.as_mut() {
                        for (dev, pin, sig) in m.poll() {
                            signals.insert((dev, pin), sig);
                        }
                    }
                }
                *proc_device_signals.write().unwrap() = signals;

                // ── Enumerate devices periodically ────────────────────────────
                if last_enum.elapsed() > Duration::from_secs(2) {
                    let mut devs: Vec<PhysicalDevice> = Vec::new();
                    for backend in &mut backends {
                        devs.extend(backend.enumerate());
                    }
                    *shared_devices.write().unwrap() = devs;
                    last_enum = Instant::now();
                }

                // ── Get latest sink outputs from processing thread ─────────────
                // Uses a separate RwLock so this read never contends on proc_outputs.
                let sink_outputs: HashMap<(String, String), Signal> =
                    sink_bus.read().unwrap().clone();

                // ── Drive virtual & physical devices ──────────────────────────
                let bypass = io_bypass.load(Ordering::Relaxed);
                let device_arc = io_device_list.read().unwrap().clone();
                {
                    let mut devs = device_arc.lock().unwrap();
                    if bypass {
                        for dev in devs.iter_mut() { dev.reset_outputs(); }
                    } else {
                        for ((device_id, pin_id), &signal) in &sink_outputs {
                            if let Some(dev) = devs.iter_mut().find(|d| d.id() == device_id) {
                                dev.send(pin_id, signal);
                            }
                        }
                        for dev in devs.iter_mut() { dev.flush(); }
                    }
                }

                if !bypass {
                    // Physical device outputs (rumble, lightbar).
                    for ((device_id, pin_id), &signal) in &sink_outputs {
                        if device_id.starts_with("gilrs:") {
                            for backend in &mut backends {
                                backend.send(device_id, pin_id, signal);
                            }
                        }
                    }
                    // MIDI output — only send on change to avoid flooding the bus.
                    if let Ok(mut mg) = midi.try_lock() {
                        if let Some(m) = mg.as_mut() {
                            for ((device_id, pin_id), &signal) in &sink_outputs {
                                if device_id.starts_with("midi_out:") {
                                    let key = (device_id.clone(), pin_id.clone());
                                    if last_midi_out.get(&key) != Some(&signal) {
                                        m.send(device_id, pin_id, signal);
                                        last_midi_out.insert(key, signal);
                                    }
                                }
                            }
                        }
                    }
                }

                let elapsed = t0.elapsed();
                if elapsed < interval {
                    std::thread::sleep(interval - elapsed);
                }
            }
        })
        .expect("failed to spawn device I/O thread");
}

/// Recreate a virtual device from its saved ID string (e.g. `"virtual.xinput.0"`).
fn try_create_virtual_device(id: &str) -> Option<Box<dyn flexinput_virtual::VirtualDevice>> {
    let dot = id.rfind('.')?;
    let kind_id = &id[..dot];
    let instance: usize = id[dot + 1..].parse().ok()?;
    Some(flexinput_virtual::create_device(kind_id, instance))
}

// ── Signal routing ────────────────────────────────────────────────────────────

/// Combine two signals of the same type: Bool=OR, numeric/Vec2=sum.
fn combine_signals(a: Signal, b: Signal) -> Signal {
    match (a, b) {
        (Signal::Bool(x),  Signal::Bool(y))  => Signal::Bool(x || y),
        (Signal::Float(x), Signal::Float(y)) => Signal::Float(x + y),
        (Signal::Vec2(x),  Signal::Vec2(y))  => Signal::Vec2(x + y),
        (Signal::Int(x),   Signal::Int(y))   => Signal::Int(x + y),
        (_, b) => b,
    }
}

fn route_signals(
    snarl: &Snarl<NodeData>,
    dev_sigs: &HashMap<(String, String), Signal>,
    active: &mut Vec<Box<dyn VirtualDevice>>,
    backends: &mut Vec<Box<dyn DeviceBackend>>,
    cache: &mut HashMap<(NodeId, usize), Option<Signal>>,
) {
    // (device_id, pin_id) -> combined signal; multiple wires combine via combine_signals.
    let mut route_map: HashMap<(String, String), Signal> = HashMap::new();

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

        // Track sink pin IDs that have any direct wire connected; these take
        // priority over anything the auto-map bus would supply for the same pin.
        // Wired = a connection exists in the graph, regardless of whether a signal
        // is currently arriving (avoids auto-map filling pins mid-connection).
        let mut directly_wired: HashSet<String> = HashSet::new();

        // ── Normal (non-AutoMap) pins ────────────────────────────────────────
        for in_idx in 0..node.inputs.len() {
            if node.inputs[in_idx].signal_type == SignalType::AutoMap {
                continue;
            }
            let dst_pin = match pin_ids.get(in_idx).filter(|s| !s.is_empty()) {
                Some(s) => s.clone(),
                None => continue,
            };
            let dst_stype = node.inputs[in_idx].signal_type;
            let in_pin = snarl.in_pin(InPinId { node: node_id, input: in_idx });
            if !in_pin.remotes.is_empty() {
                directly_wired.insert(dst_pin.clone());
            }
            for &src in &in_pin.remotes {
                if let Some(sig) = eval_output(snarl, src.node, src.output, dev_sigs, 0, cache) {
                    let coerced = if dst_stype == SignalType::Any {
                        sig
                    } else {
                        match sig.coerce_to(dst_stype) {
                            Some(s) => s,
                            None => continue,
                        }
                    };
                    let key = (sink_id.clone(), dst_pin.clone());
                    route_map.entry(key).and_modify(|e| *e = combine_signals(*e, coerced)).or_insert(coerced);
                }
            }
        }

        // ── AutoMap bus pins ─────────────────────────────────────────────────
        for in_idx in 0..node.inputs.len() {
            if node.inputs[in_idx].signal_type != SignalType::AutoMap {
                continue;
            }
            let in_pin = snarl.in_pin(InPinId { node: node_id, input: in_idx });
            for &src_out in &in_pin.remotes {
                let src_node = match snarl.get_node(src_out.node) {
                    Some(n) => n,
                    None => continue,
                };
                if src_node.module_id != "device.source" {
                    continue;
                }
                let src_dev_id = match src_node.params.get("device_id").and_then(|v| v.as_str()) {
                    Some(s) => s.to_string(),
                    None => continue,
                };

                // Collect source output pin IDs/types, skipping the automap port itself.
                let src_entries: Vec<(String, SignalType)> = src_node.params
                    .get("output_pin_ids")
                    .and_then(|v| v.as_array())
                    .map(|ids| {
                        ids.iter().enumerate().filter_map(|(i, v)| {
                            let pid = v.as_str()?;
                            if pid.is_empty() { return None; }
                            let stype = src_node.outputs.get(i)?.signal_type;
                            if stype == SignalType::AutoMap { return None; }
                            Some((pid.to_string(), stype))
                        }).collect()
                    })
                    .unwrap_or_default();

                // Collect sink input pin IDs/types, skipping the automap port itself.
                let dst_entries: Vec<(String, SignalType)> = pin_ids.iter().enumerate()
                    .filter_map(|(i, id)| {
                        if id.is_empty() { return None; }
                        let stype = node.inputs.get(i)?.signal_type;
                        if stype == SignalType::AutoMap { return None; }
                        Some((id.clone(), stype))
                    })
                    .collect();

                let src_ids: Vec<&str> = src_entries.iter().map(|(s, _)| s.as_str()).collect();
                let dst_ids: Vec<&str> = dst_entries.iter().map(|(s, _)| s.as_str()).collect();

                for (mapped_src, mapped_dst) in
                    flexinput_core::automap::resolve_mapping(&src_ids, &dst_ids)
                {
                    // Direct wire on this sink pin takes priority.
                    if directly_wired.contains(mapped_dst) {
                        continue;
                    }
                    let dst_stype = dst_entries.iter()
                        .find(|(id, _)| id.as_str() == mapped_dst)
                        .map(|(_, t)| *t)
                        .unwrap_or(SignalType::Float);

                    if let Some(&raw) = dev_sigs.get(&(src_dev_id.clone(), mapped_src.to_string())) {
                        if let Some(coerced) = raw.coerce_to(dst_stype) {
                            let key = (sink_id.clone(), mapped_dst.to_string());
                            route_map.entry(key).and_modify(|e| *e = combine_signals(*e, coerced)).or_insert(coerced);
                        }
                    }
                }
            }
        }

        // When both a Vec2 stick pin and its individual axis pins are present in
        // route_map for the same device they write to the same hardware registers
        // and fight. Resolve per direct-wire priority: axes win when directly
        // wired and Vec2 is not; Vec2 wins in all other cases (including all-automap).
        const STICK_GROUPS: &[(&str, &[&str])] = &[
            ("left_stick",  &["left_stick_x", "left_stick_y"]),
            ("right_stick", &["right_stick_x", "right_stick_y"]),
            ("dpad",        &["dpad_x", "dpad_y"]),
        ];
        for &(vec2_pin, axis_pins) in STICK_GROUPS {
            let has_vec2     = route_map.contains_key(&(sink_id.clone(), vec2_pin.to_string()));
            let has_any_axis = axis_pins.iter().any(|p| route_map.contains_key(&(sink_id.clone(), p.to_string())));
            if !has_vec2 || !has_any_axis { continue; }
            let vec2_direct     = directly_wired.contains(vec2_pin);
            let any_axis_direct = axis_pins.iter().any(|p| directly_wired.contains(*p));
            if any_axis_direct && !vec2_direct {
                route_map.remove(&(sink_id.clone(), vec2_pin.to_string()));
            } else {
                for &axis_pin in axis_pins {
                    route_map.remove(&(sink_id.clone(), axis_pin.to_string()));
                }
            }
        }
    }

    for ((device_id, pin_id), signal) in route_map {
        if let Some(dev) = active.iter_mut().find(|d| d.id() == device_id) {
            dev.send(&pin_id, signal);
        } else if device_id.starts_with("gilrs:") {
            // Physical-device sink (rumble / lightbar / future haptics).
            // We dispatch to every backend and let each one filter on the id;
            // currently only GilrsBackend recognises `gilrs:N`.
            for backend in backends.iter_mut() {
                backend.send(&device_id, &pin_id, signal);
            }
        }
    }
}

fn route_midi_out(
    snarl: &Snarl<NodeData>,
    dev_sigs: &HashMap<(String, String), Signal>,
    midi: &mut flexinput_devices::MidiBackend,
    cache: &mut HashMap<(NodeId, usize), Option<Signal>>,
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
                if let Some(sig) = eval_output(snarl, src.node, src.output, dev_sigs, 0, cache) {
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
    cache: &mut HashMap<(NodeId, usize), Option<Signal>>,
) -> Option<Signal> {
    if depth > 16 {
        return None; // prevent infinite recursion in cyclic graphs
    }

    let key = (node_id, out_idx);
    if let Some(&cached) = cache.get(&key) {
        return cached;
    }

    let node = snarl.get_node(node_id)?;

    let result = match node.module_id.as_str() {
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
            let n_inputs = node.inputs.len();
            let mut inputs = Vec::with_capacity(n_inputs);
            for i in 0..n_inputs {
                let p = snarl.in_pin(InPinId { node: node_id, input: i });
                let sig = p.remotes.first().and_then(|&src| {
                    eval_output(snarl, src.node, src.output, dev_sigs, depth + 1, cache)
                });
                inputs.push(sig);
            }
            let node = snarl.get_node(node_id)?;
            eval_module(id, out_idx, &inputs, node)
        }
    };

    cache.insert(key, result);
    result
}

fn get_f(inputs: &[Option<Signal>], i: usize, default: f32) -> f32 {
    inputs.get(i).and_then(|s| *s)
        .map(|s| s.as_float())
        .unwrap_or(default)
}

fn get_b(inputs: &[Option<Signal>], i: usize, default: bool) -> bool {
    inputs.get(i).and_then(|s| *s)
        .map(|s| s.as_bool())
        .unwrap_or(default)
}

/// Evaluates a pure module given its resolved inputs; also reads param defaults.
fn eval_module(id: &str, out_idx: usize, inputs: &[Option<Signal>], node: &NodeData) -> Option<Signal> {
    // For optional inputs, fall back to node params if no wire connected.
    let param_f = |name: &str, default: f32| -> f32 {
        node.params.get(name).and_then(|v| v.as_f64()).map(|f| f as f32).unwrap_or(default)
    };

    match id {
        "math.add" => {
            Some(Signal::Float((0..inputs.len()).map(|i| get_f(inputs, i, 0.0)).sum()))
        }
        "math.subtract" => {
            let first = get_f(inputs, 0, 0.0);
            let rest: f32 = (1..inputs.len()).map(|i| get_f(inputs, i, 0.0)).sum();
            Some(Signal::Float(first - rest))
        }
        "math.multiply" => {
            let first = get_f(inputs, 0, 0.0);
            let rest: f32 = (1..inputs.len()).map(|i| get_f(inputs, i, 1.0)).product();
            Some(Signal::Float(first * rest))
        }
        "math.divide" => {
            let mut v = get_f(inputs, 0, 0.0);
            for i in 1..inputs.len() {
                let d = get_f(inputs, i, 1.0);
                v = if d == 0.0 { 0.0 } else { v / d };
            }
            Some(Signal::Float(v))
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
        "logic.equal"     => Some(Signal::Bool(get_f(inputs, 0, 0.0) == get_f(inputs, 1, 0.0))),
        "logic.not_equal" => Some(Signal::Bool(get_f(inputs, 0, 0.0) != get_f(inputs, 1, 0.0))),
        "logic.greater_than" => {
            let a = get_f(inputs, 0, 0.0);
            let b = get_f(inputs, 1, 0.0);
            let or_eq = node.params.get("or_equal").and_then(|v| v.as_bool()).unwrap_or(false);
            Some(Signal::Bool(if or_eq { a >= b } else { a > b }))
        }
        "logic.less_than" => {
            let a = get_f(inputs, 0, 0.0);
            let b = get_f(inputs, 1, 0.0);
            let or_eq = node.params.get("or_equal").and_then(|v| v.as_bool()).unwrap_or(false);
            Some(Signal::Bool(if or_eq { a <= b } else { a < b }))
        }
        "module.selector" => {
            if out_idx == 0 {
                let n_inputs = inputs.len().saturating_sub(1);
                let sel = get_f(inputs, 0, 0.0);
                let interp = node.params.get("interpolate").and_then(|v| v.as_bool()).unwrap_or(false);
                if interp && n_inputs >= 2 {
                    let pos = sel.clamp(0.0, 1.0) * (n_inputs - 1) as f32;
                    let lo = pos.floor() as usize;
                    let hi = (lo + 1).min(n_inputs - 1);
                    let t = pos.fract();
                    let lo_v = inputs.get(lo + 1).and_then(|s| *s).map(|s| s.as_float()).unwrap_or(0.0);
                    let hi_v = inputs.get(hi + 1).and_then(|s| *s).map(|s| s.as_float()).unwrap_or(0.0);
                    Some(Signal::Float(lo_v * (1.0 - t) + hi_v * t))
                } else {
                    let n = n_inputs as f32;
                    let idx = (sel.clamp(0.0, 1.0) * n).floor() as usize;
                    let idx = idx.min(n_inputs.saturating_sub(1));
                    inputs.get(idx + 1).and_then(|s| *s)
                }
            } else {
                None
            }
        }
        "module.split" => {
            let n = node.outputs.len();
            let sel = get_f(inputs, 0, 0.0);
            let val = get_f(inputs, 1, 0.0);
            let interp = node.params.get("interpolate").and_then(|v| v.as_bool()).unwrap_or(false);
            if interp && n >= 2 {
                let pos = sel.clamp(0.0, 1.0) * (n - 1) as f32;
                let lo = pos.floor() as usize;
                let hi = (lo + 1).min(n - 1);
                let t = pos.fract();
                let lo_w = 1.0 - t;
                let hi_w = t;
                if out_idx == lo && lo == hi {
                    Some(Signal::Float(val))
                } else if out_idx == lo {
                    Some(Signal::Float(val * lo_w))
                } else if out_idx == hi {
                    Some(Signal::Float(val * hi_w))
                } else {
                    Some(Signal::Float(0.0))
                }
            } else {
                let idx = (sel.clamp(0.0, 1.0) * n as f32).floor() as usize;
                let idx = idx.min(n.saturating_sub(1));
                if out_idx == idx { Some(Signal::Float(val)) } else { Some(Signal::Float(0.0)) }
            }
        }
        // Stateful modules: output computed by update_stateful_nodes() each frame.
        "logic.has_changed" | "logic.delay" | "logic.counter" | "generator.oscillator" | "module.delay" | "processing.gyro_3dof" => {
            node.extra.last_signals.get(out_idx).copied().flatten()
        }
        "module.average" | "module.dc_filter" => {
            node.extra.last_signals.get(out_idx).copied().flatten()
        }
        "module.response_curve" => {
            if out_idx >= node.outputs.len() { return None; }
            let x        = get_f(inputs, out_idx, 0.0);
            let pts      = curve_points_from_params(node);
            let biases   = flexinput_engine::biases_from_params(&node.params);
            let absolute = node.params.get("absolute").and_then(|v| v.as_bool()).unwrap_or(true);
            let in_max   = node.params.get("in_max") .and_then(|v| v.as_f64()).unwrap_or(1.0)  as f32;
            let in_min   = node.params.get("in_min") .and_then(|v| v.as_f64()).unwrap_or(-1.0) as f32;
            let out_max  = node.params.get("out_max").and_then(|v| v.as_f64()).unwrap_or(1.0)  as f32;
            let out_min  = node.params.get("out_min").and_then(|v| v.as_f64()).unwrap_or(-1.0) as f32;
            Some(Signal::Float(apply_curve(x, &pts, &biases, absolute, in_min, in_max, out_min, out_max, read_scale_t(node))))
        }
        "module.vec_response_curve" => {
            if out_idx >= node.outputs.len() { return None; }
            let vec = match inputs.get(out_idx).and_then(|s| *s) {
                Some(Signal::Vec2(v)) => v,
                _ => return Some(Signal::Vec2(glam::Vec2::ZERO)),
            };
            let mag = vec.length();
            if mag < f32::EPSILON {
                return Some(Signal::Vec2(glam::Vec2::ZERO));
            }
            let pts     = curve_points_from_params(node);
            let biases  = flexinput_engine::biases_from_params(&node.params);
            let in_max  = node.params.get("in_max") .and_then(|v| v.as_f64()).unwrap_or(1.0) as f32;
            let out_max = node.params.get("out_max").and_then(|v| v.as_f64()).unwrap_or(1.0) as f32;
            let out_mag = apply_curve(mag, &pts, &biases, true, 0.0, in_max, 0.0, out_max, read_scale_t(node));
            Some(Signal::Vec2(vec / mag * out_mag))
        }
        "module.vec_to_axis" => {
            let vec = match inputs.first().and_then(|s| *s) {
                Some(Signal::Vec2(v)) => v,
                _ => glam::Vec2::ZERO,
            };
            match out_idx {
                0 => Some(Signal::Float(vec.x)),
                1 => Some(Signal::Float(vec.y)),
                _ => None,
            }
        }
        "module.axis_to_vec" => {
            if out_idx != 0 { return None; }
            let x = match inputs.first().and_then(|s| *s) {
                Some(Signal::Float(f)) => f,
                _ => 0.0,
            };
            let y = match inputs.get(1).and_then(|s| *s) {
                Some(Signal::Float(f)) => f,
                _ => 0.0,
            };
            Some(Signal::Vec2(glam::Vec2::new(x, y)))
        }
        _ => None,
    }
}

// ── Processing-thread graph snapshot builder ──────────────────────────────────

/// Builds a topologically-sorted [`ProcessingGraph`] from the current Snarl state.
/// Also returns the UIDs of any counter nodes whose reset was just requested
/// (caller must clear the `aux_f32_dirty` flag on those nodes after writing the snapshot).
fn build_processing_graph(snarl: &Snarl<NodeData>) -> (ProcessingGraph, Vec<usize>) {
    use std::collections::{HashSet, VecDeque};
    use flexinput_engine::graph::SinkTarget;

    // Collect ALL nodes (including device.sink — they're evaluated last).
    let node_list: Vec<(NodeId, &NodeData)> = snarl.nodes_ids_data()
        .map(|(id, n)| (id, &n.value))
        .collect();

    let id_to_orig: HashMap<NodeId, usize> = node_list.iter()
        .enumerate()
        .map(|(i, (id, _))| (*id, i))
        .collect();

    let mut dirty_uids: Vec<usize> = Vec::new();

    let snaps: Vec<NodeSnap> = node_list.iter().map(|(node_id, node)| {
        let is_sink = node.module_id == "device.sink";

        // Non-sink: single (first) source per input pin, for the existing eval path.
        let input_sources = if !is_sink {
            (0..node.inputs.len())
                .map(|i| {
                    let pin = snarl.in_pin(InPinId { node: *node_id, input: i });
                    pin.remotes.first().and_then(|&src| {
                        id_to_orig.get(&src.node).map(|&idx| (idx, src.output))
                    })
                })
                .collect()
        } else {
            vec![] // sink nodes use sink_target.multi_sources
        };

        let device_id = node.params.get("device_id")
            .and_then(|v| v.as_str()).map(|s| s.to_string());
        let output_pin_ids = node.params.get("output_pin_ids")
            .and_then(|v| v.as_array())
            .map(|a| a.iter().map(|v| v.as_str().unwrap_or("").to_string()).collect())
            .unwrap_or_default();

        let aux_f32_override = if node.extra.aux_f32_dirty {
            dirty_uids.push(node_id.0);
            Some(node.extra.aux_f32.clone())
        } else {
            None
        };

        // For device.sink: build the full routing metadata.
        let sink_target = if is_sink {
            let sink_dev_id = device_id.clone().unwrap_or_default();
            let pin_ids: Vec<String> = node.params
                .get("input_pin_ids")
                .and_then(|v| v.as_array())
                .map(|a| a.iter().map(|v| v.as_str().unwrap_or("").to_string()).collect())
                .unwrap_or_default();

            // For each direct-wire input: collect ALL remotes (multi-source, combined additively).
            let multi_sources: Vec<Vec<(usize, usize)>> = (0..node.inputs.len())
                .map(|i| {
                    if node.inputs.get(i).map(|p| p.signal_type) == Some(SignalType::AutoMap) {
                        return vec![];
                    }
                    let pin = snarl.in_pin(InPinId { node: *node_id, input: i });
                    pin.remotes.iter()
                        .filter_map(|&src| id_to_orig.get(&src.node).map(|&idx| (idx, src.output)))
                        .collect()
                })
                .collect();

            // AutoMap: find the AutoMap input pin and its connected source.
            let automap_source = (0..node.inputs.len()).find_map(|i| {
                if node.inputs.get(i).map(|p| p.signal_type) != Some(SignalType::AutoMap) {
                    return None;
                }
                let pin = snarl.in_pin(InPinId { node: *node_id, input: i });
                let src = pin.remotes.first()?;
                let src_node = snarl.get_node(src.node)?;
                let src_dev_id = src_node.params.get("device_id")?.as_str()?.to_string();
                let src_pins: Vec<String> = src_node.params
                    .get("output_pin_ids")?.as_array()?
                    .iter().map(|v| v.as_str().unwrap_or("").to_string()).collect();
                Some((src_dev_id, src_pins))
            });

            Some(SinkTarget { device_id: sink_dev_id, pin_ids, multi_sources, automap_source })
        } else {
            None
        };

        // For modules that read device signals by name (gyro automap), inject the connected
        // source device ID so the compute function can look signals up in dev_sigs directly.
        let mut params = node.params.clone();
        if node.module_id == "processing.gyro_3dof" {
            let automap_idx = node.inputs.iter().position(|p| p.signal_type == SignalType::AutoMap);
            if let Some(idx) = automap_idx {
                let pin = snarl.in_pin(InPinId { node: *node_id, input: idx });
                if let Some(&src) = pin.remotes.first() {
                    if let Some(src_node) = snarl.get_node(src.node) {
                        if src_node.module_id == "device.source" {
                            if let Some(dev_id) = src_node.params.get("device_id").and_then(|v| v.as_str()) {
                                params.insert("_automap_device_id".to_string(), serde_json::Value::String(dev_id.to_string()));
                            }
                        }
                    }
                }
            }
        }

        NodeSnap {
            node_uid: node_id.0,
            module_id: node.module_id.clone(),
            params,
            n_outputs: node.outputs.len(),
            input_sources,
            device_id,
            output_pin_ids,
            aux_f32_override,
            sink_target,
        }
    }).collect();

    // Topological sort (Kahn's algorithm).
    // Sink nodes are leaves (no node depends on them), so they naturally end up last.
    let n = snaps.len();
    let mut in_degree = vec![0usize; n];
    let mut dependents: Vec<Vec<usize>> = vec![vec![]; n];
    for (idx, snap) in snaps.iter().enumerate() {
        // Regular nodes: single-source inputs.
        for &(src_idx, _) in snap.input_sources.iter().flatten() {
            dependents[src_idx].push(idx);
            in_degree[idx] += 1;
        }
        // Sink nodes: multi-source inputs (deduplicated per source node to avoid double-counting).
        if let Some(ref st) = snap.sink_target {
            let mut seen: HashSet<usize> = HashSet::new();
            for sources in &st.multi_sources {
                for &(src_idx, _) in sources {
                    if seen.insert(src_idx) {
                        dependents[src_idx].push(idx);
                        in_degree[idx] += 1;
                    }
                }
            }
        }
    }
    let mut queue: VecDeque<usize> = (0..n).filter(|&i| in_degree[i] == 0).collect();
    let mut sorted: Vec<usize> = Vec::with_capacity(n);
    while let Some(idx) = queue.pop_front() {
        sorted.push(idx);
        for &dep in &dependents[idx] {
            in_degree[dep] -= 1;
            if in_degree[dep] == 0 { queue.push_back(dep); }
        }
    }
    // Append any remaining nodes (cycles — shouldn't happen in practice).
    for i in 0..n { if !sorted.contains(&i) { sorted.push(i); } }

    // Remap indices from original order → sorted order.
    let mut orig_to_sorted = vec![0usize; n];
    for (new_idx, &orig) in sorted.iter().enumerate() { orig_to_sorted[orig] = new_idx; }

    let nodes = sorted.iter().map(|&orig| {
        let mut snap = snaps[orig].clone();
        // Remap single-source inputs.
        for src in snap.input_sources.iter_mut().flatten() { src.0 = orig_to_sorted[src.0]; }
        // Remap multi-source inputs for sink nodes.
        if let Some(ref mut st) = snap.sink_target {
            for sources in &mut st.multi_sources {
                for src in sources.iter_mut() { src.0 = orig_to_sorted[src.0]; }
            }
        }
        snap
    }).collect();

    (ProcessingGraph { nodes }, dirty_uids)
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
    scale_t: f32,
) -> f32 {
    if absolute {
        let sign      = if x < 0.0 { -1.0f32 } else { 1.0 };
        let abs_max   = in_max.abs().max(in_min.abs()).max(f32::EPSILON);
        let abs_norm  = (x.abs() / abs_max).clamp(0.0, 1.0);
        let scaled    = curve_scale(abs_norm, scale_t);
        let curve_y   = sample_curve(pts, scaled, biases).clamp(0.0, 1.0);
        let out_y     = curve_scale_inv(curve_y, scale_t);
        let out_scale = out_max.abs().max(out_min.abs());
        sign * out_y * out_scale
    } else {
        let in_range  = (in_max - in_min).abs().max(f32::EPSILON);
        let out_range = out_max - out_min;
        let norm      = ((x - in_min) / in_range * 2.0 - 1.0).clamp(-1.0, 1.0);
        let sign      = if norm < 0.0 { -1.0f32 } else { 1.0 };
        let scaled    = sign * curve_scale(norm.abs(), scale_t);
        let curve_y   = sample_curve(pts, scaled, biases);
        let sign_out  = if curve_y < 0.0 { -1.0f32 } else { 1.0 };
        let out_y     = sign_out * curve_scale_inv(curve_y.abs(), scale_t);
        out_min + (out_y.clamp(-1.0, 1.0) + 1.0) * 0.5 * out_range
    }
}

/// Maps x ∈ [0,1] → [0,1] continuously. t=0 → linear; t<0 → log-like; t>0 → exp-like.
/// Power law p = 2^(t*3): at t=±1, p=8 or 1/8 — far more extreme than the old log/exp modes.
fn curve_scale(x: f32, t: f32) -> f32 {
    if t.abs() < 1e-4 { return x; }
    x.clamp(0.0, 1.0).powf(2.0f32.powf(t * 3.0))
}

fn curve_scale_inv(y: f32, t: f32) -> f32 {
    if t.abs() < 1e-4 { return y; }
    y.clamp(0.0, 1.0).powf(1.0 / 2.0f32.powf(t * 3.0))
}

fn read_scale_t(node: &NodeData) -> f32 {
    node.params.get("scale_t")
        .and_then(|v| v.as_f64())
        .map(|f| f as f32)
        .unwrap_or_else(|| match node.params.get("in_scale").and_then(|v| v.as_i64()).unwrap_or(0) {
            1 => -0.5,
            2 =>  0.5,
            _ =>  0.0,
        })
}

// ── Display node history update ───────────────────────────────────────────────

const DISPLAY_IDS: &[&str] = &[
    "display.readout",
    "display.oscilloscope",
    "display.vectorscope",
];
const HISTORY_LEN: usize = 20000;

fn update_display_nodes(
    snarl: &mut Snarl<NodeData>,
    dev_sigs: &HashMap<(String, String), Signal>,
    cache: &mut HashMap<(NodeId, usize), Option<Signal>>,
) {
    let node_ids: Vec<NodeId> = snarl
        .nodes_ids_data()
        .filter(|(_, n)| DISPLAY_IDS.contains(&n.value.module_id.as_str()))
        .map(|(id, _)| id)
        .collect();

    for node_id in node_ids {
        let (n_inputs, module_id) = snarl.get_node(node_id)
            .map(|n| (n.inputs.len(), n.module_id.clone()))
            .unwrap_or_default();
        let mut vals = Vec::with_capacity(n_inputs);
        for i in 0..n_inputs {
            let pin = snarl.in_pin(InPinId { node: node_id, input: i });
            let sig = pin.remotes.first().and_then(|&src| {
                eval_output(snarl, src.node, src.output, dev_sigs, 0, cache)
            });
            vals.push(sig);
        }

        if let Some(node) = snarl.get_node_mut(node_id) {
            // Store for readout body rendering
            node.extra.last_signals = vals.clone();

            // Append one sample to the history ring buffer.
            // Vectorscope channels are Vec2: flatten each into [x, y] pairs.
            let sample: Vec<Option<f32>> = if module_id == "display.vectorscope" {
                vals.iter().flat_map(|sig| match sig {
                    Some(Signal::Vec2(v)) => [Some(v.x), Some(v.y)],
                    _ => [None, None],
                }).collect()
            } else {
                (0..vals.len())
                    .map(|i| sig_to_f32(vals.get(i).copied().flatten()))
                    .collect()
            };
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

// ── Tab bar ───────────────────────────────────────────────────────────────────

/// Returns (switch_to_idx, close_tab_idx, new_tab_requested, bypass_toggle_idx).
fn show_tab_bar(
    ui: &mut egui::Ui,
    tabs: &[PatchTab],
    active_tab: usize,
    effective_bypass: &[bool],
) -> (Option<usize>, Option<usize>, bool, Option<usize>) {
    let mut switch_to: Option<usize> = None;
    let mut close_idx: Option<usize> = None;
    let mut new_tab = false;
    let mut bypass_toggle: Option<usize> = None;

    let h = ui.available_height();
    let accent      = ui.visuals().selection.bg_fill;
    let text_color  = ui.visuals().text_color();
    let hover_fill  = ui.visuals().widgets.hovered.bg_fill;
    let sep_color   = ui.visuals().widgets.noninteractive.bg_stroke.color;
    let active_fill = ui.visuals().window_fill();
    let font_id     = egui::FontId::proportional(13.0);

    egui::ScrollArea::horizontal()
        .id_salt("tab_scroll")
        .auto_shrink([false, false])
        .show(ui, |ui| {
            ui.spacing_mut().item_spacing = egui::Vec2::ZERO;
            ui.horizontal(|ui| {
                ui.add_space(4.0);

                for (i, tab) in tabs.iter().enumerate() {
                    let is_active = i == active_tab;
                    let is_bypassed = effective_bypass.get(i).copied().unwrap_or(false);

                    let galley = ui.painter().layout_no_wrap(tab.title.clone(), font_id.clone(), text_color);
                    let label_w = galley.size().x;
                    // layout: left(8) + label + buffer(4) + bypass(14) + gap(6) + close(14) + right(8)
                    let tab_w = (label_w + 54.0).max(90.0);

                    let (tab_rect, tab_resp) = ui.allocate_exact_size(
                        egui::vec2(tab_w, h),
                        egui::Sense::click(),
                    );

                    // Background
                    if is_active {
                        ui.painter().rect_filled(tab_rect, egui::CornerRadius::ZERO, active_fill);
                        // Bottom accent line
                        ui.painter().line_segment(
                            [tab_rect.left_bottom(), tab_rect.right_bottom()],
                            egui::Stroke::new(2.0, accent),
                        );
                        // Side borders to frame the active tab
                        let border = egui::Stroke::new(1.0, sep_color);
                        ui.painter().line_segment([tab_rect.left_top(), tab_rect.left_bottom()], border);
                        ui.painter().line_segment([tab_rect.right_top(), tab_rect.right_bottom()], border);
                    } else if tab_resp.hovered() {
                        ui.painter().rect_filled(tab_rect, egui::CornerRadius::ZERO, hover_fill);
                    }

                    // Label (left-padded, vertically centered)
                    let label_x = tab_rect.left() + 8.0;
                    let label_y = tab_rect.center().y - galley.size().y / 2.0;
                    ui.painter().galley(egui::pos2(label_x, label_y), galley, text_color);

                    // Close X button
                    let x_size = 14.0_f32;
                    let x_center = egui::pos2(tab_rect.right() - 8.0 - x_size / 2.0, tab_rect.center().y);
                    let x_rect = egui::Rect::from_center_size(x_center, egui::vec2(x_size, x_size));
                    let x_resp = ui.interact(x_rect, ui.id().with(("tab_x", i)), egui::Sense::click());
                    if x_resp.hovered() {
                        ui.painter().circle_filled(x_rect.center(), x_size / 2.0 + 1.0, sep_color);
                    }
                    let c = x_rect.center();
                    let d = 3.2_f32;
                    let xs = egui::Stroke::new(1.2, text_color);
                    ui.painter().line_segment([egui::pos2(c.x - d, c.y - d), egui::pos2(c.x + d, c.y + d)], xs);
                    ui.painter().line_segment([egui::pos2(c.x + d, c.y - d), egui::pos2(c.x - d, c.y + d)], xs);

                    // Bypass toggle button (circle, left of X)
                    let bp_cx = x_center.x - x_size / 2.0 - 6.0 - 7.0; // right - 35
                    let bp_center = egui::pos2(bp_cx, tab_rect.center().y);
                    let bp_hit = egui::Rect::from_center_size(bp_center, egui::vec2(14.0, 14.0));
                    let bp_resp = ui.interact(bp_hit, ui.id().with(("tab_bp", i)), egui::Sense::click());
                    // Active tab: green (on) or amber (bypassed).
                    // Inactive tabs: amber if bypassed, invisible otherwise — showing green
                    // would wrongly imply background tabs are actively routing.
                    let dot_color = if is_bypassed {
                        egui::Color32::from_rgb(220, 140, 40) // amber = bypassed
                    } else if is_active {
                        egui::Color32::from_rgb(60, 180, 60)  // green = active (only on active tab)
                    } else {
                        egui::Color32::TRANSPARENT
                    };
                    let (bp_fill, bp_stroke_color) = (dot_color, dot_color);
                    ui.painter().circle(bp_center, 4.0, bp_fill, egui::Stroke::new(1.2, bp_stroke_color));
                    if bp_resp.clicked() {
                        bypass_toggle = Some(i);
                    }

                    if x_resp.clicked() {
                        close_idx = Some(i);
                    } else if tab_resp.clicked() {
                        switch_to = Some(i);
                    }

                    // Vertical separator between non-active adjacent tabs
                    if i + 1 < tabs.len() && !is_active && (i + 1) != active_tab {
                        let sx = tab_rect.right();
                        ui.painter().line_segment(
                            [egui::pos2(sx, tab_rect.top() + 5.0), egui::pos2(sx, tab_rect.bottom() - 5.0)],
                            egui::Stroke::new(1.0, sep_color),
                        );
                    }
                }

                // "+" new tab button
                let (plus_rect, plus_resp) = ui.allocate_exact_size(egui::vec2(32.0, h), egui::Sense::click());
                if plus_resp.hovered() {
                    ui.painter().rect_filled(plus_rect, egui::CornerRadius::ZERO, hover_fill);
                }
                let c = plus_rect.center();
                let ps = egui::Stroke::new(1.5, text_color);
                ui.painter().line_segment([egui::pos2(c.x - 5.0, c.y), egui::pos2(c.x + 5.0, c.y)], ps);
                ui.painter().line_segment([egui::pos2(c.x, c.y - 5.0), egui::pos2(c.x, c.y + 5.0)], ps);
                if plus_resp.clicked() {
                    new_tab = true;
                }
            });
        });

    (switch_to, close_idx, new_tab, bypass_toggle)
}

// ── Custom title bar ──────────────────────────────────────────────────────────

fn handle_window_resize(ctx: &egui::Context) {
    let maximized = ctx.input(|i| i.viewport().maximized.unwrap_or(false));
    if maximized { return; }

    let screen = ctx.viewport_rect();
    let (pointer_pos, primary_pressed) = ctx.input(|i| (i.pointer.hover_pos(), i.pointer.primary_pressed()));
    let Some(pos) = pointer_pos else { return };

    const BORDER: f32 = 6.0;
    let on_l = pos.x < screen.left()   + BORDER;
    let on_r = pos.x > screen.right()  - BORDER;
    let on_t = pos.y < screen.top()    + BORDER;
    let on_b = pos.y > screen.bottom() - BORDER;

    let dir = match (on_l, on_r, on_t, on_b) {
        (true,  false, true,  false) => Some(egui::ResizeDirection::NorthWest),
        (false, true,  true,  false) => Some(egui::ResizeDirection::NorthEast),
        (true,  false, false, true ) => Some(egui::ResizeDirection::SouthWest),
        (false, true,  false, true ) => Some(egui::ResizeDirection::SouthEast),
        (true,  false, false, false) => Some(egui::ResizeDirection::West),
        (false, true,  false, false) => Some(egui::ResizeDirection::East),
        (false, false, true,  false) => Some(egui::ResizeDirection::North),
        (false, false, false, true ) => Some(egui::ResizeDirection::South),
        _ => None,
    };

    if let Some(dir) = dir {
        let cursor = match dir {
            egui::ResizeDirection::North     => egui::CursorIcon::ResizeNorth,
            egui::ResizeDirection::South     => egui::CursorIcon::ResizeSouth,
            egui::ResizeDirection::East      => egui::CursorIcon::ResizeEast,
            egui::ResizeDirection::West      => egui::CursorIcon::ResizeWest,
            egui::ResizeDirection::NorthEast => egui::CursorIcon::ResizeNorthEast,
            egui::ResizeDirection::NorthWest => egui::CursorIcon::ResizeNorthWest,
            egui::ResizeDirection::SouthEast => egui::CursorIcon::ResizeSouthEast,
            egui::ResizeDirection::SouthWest => egui::CursorIcon::ResizeSouthWest,
        };
        ctx.set_cursor_icon(cursor);
        if primary_pressed {
            ctx.send_viewport_cmd(egui::ViewportCommand::BeginResize(dir));
        }
    }
}

fn draw_rect_stroke(painter: &egui::Painter, rect: egui::Rect, stroke: egui::Stroke) {
    let tl = rect.left_top();
    let tr = rect.right_top();
    let br = rect.right_bottom();
    let bl = rect.left_bottom();
    painter.line_segment([tl, tr], stroke);
    painter.line_segment([tr, br], stroke);
    painter.line_segment([br, bl], stroke);
    painter.line_segment([bl, tl], stroke);
}

fn show_title_bar(
    ui: &mut egui::Ui,
    ctx: &egui::Context,
    do_save: &mut bool,
    do_load: &mut bool,
    do_new: &mut bool,
    do_close: &mut bool,
    do_bind: &mut bool,
    do_hidhide: &mut bool,
    auto_switch: &mut bool,
    do_undo: &mut bool,
    do_redo: &mut bool,
    can_undo: bool,
    can_redo: bool,
    logo: &Option<egui::TextureHandle>,
) {
    let bar = ui.max_rect();
    let h = bar.height();
    let btn_w = 46.0_f32;
    let ctrl_w = btn_w * 3.0;
    let left_w = 300.0_f32;

    // Full-bar drag sensing (placed first so interactive widgets above take priority).
    let drag = ui.interact(bar, ui.id().with("tb_drag"), egui::Sense::click_and_drag());

    // ── Left: File menu ────────────────────────────────────────────────────
    let left_rect = egui::Rect::from_min_size(bar.min, egui::vec2(left_w, h));
    ui.scope_builder(egui::UiBuilder::new().max_rect(left_rect), |ui| {
        ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
            ui.add_space(8.0);
            ui.menu_button("File", |ui| {
                if ui.button("New").clicked()                       { *do_new   = true; ui.close(); }
                if ui.button("Save Patch…").clicked()               { *do_save  = true; ui.close(); }
                if ui.button("Load Patch…").clicked()               { *do_load  = true; ui.close(); }
                ui.separator();
                if ui.button("Bind Tab to Process…").clicked()      { *do_bind  = true; ui.close(); }
                ui.separator();
                if ui.button("Close Tab").clicked()                 { *do_close = true; ui.close(); }
            });

            ui.add_space(6.0);

            // Auto-switch toggle button
            let auto_label = if *auto_switch { "Auto ●" } else { "Auto ○" };
            let hover_text = if *auto_switch {
                "Auto-switch ON — tabs switch when a bound process gains focus"
            } else {
                "Auto-switch OFF — tab switching is manual"
            };
            if ui.selectable_label(*auto_switch, auto_label)
                .on_hover_text(hover_text)
                .clicked()
            {
                *auto_switch = !*auto_switch;
            }

            ui.add_space(6.0);
            ui.separator();
            ui.add_space(4.0);

            // Undo / Redo buttons
            if ui.add_enabled(can_undo, egui::Button::new("↶").small())
                .on_hover_text("Undo (Ctrl+Z)")
                .clicked()
            {
                *do_undo = true;
            }
            if ui.add_enabled(can_redo, egui::Button::new("↷").small())
                .on_hover_text("Redo (Ctrl+Shift+Z)")
                .clicked()
            {
                *do_redo = true;
            }
        });
    });

    // ── Right: window control buttons (painter-drawn icons) ───────────────
    let ctrl_rect = egui::Rect::from_min_size(
        egui::pos2(bar.right() - ctrl_w, bar.top()),
        egui::vec2(ctrl_w, h),
    );
    ui.scope_builder(egui::UiBuilder::new().max_rect(ctrl_rect), |ui| {
        ui.spacing_mut().item_spacing = egui::Vec2::ZERO;
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let icon_color = ui.visuals().text_color();
            let hover_fill = ui.visuals().widgets.hovered.bg_fill;

            // ── Close ──────────────────────────────────────────────────────
            let (rect, resp) = ui.allocate_exact_size(egui::vec2(btn_w, h), egui::Sense::click());
            let close_color = if resp.hovered() {
                ui.painter().rect_filled(rect, egui::CornerRadius::ZERO, egui::Color32::from_rgb(196, 43, 28));
                egui::Color32::WHITE
            } else {
                icon_color
            };
            let c = rect.center();
            let d = 5.0_f32;
            let s = egui::Stroke::new(1.5, close_color);
            ui.painter().line_segment([egui::pos2(c.x - d, c.y - d), egui::pos2(c.x + d, c.y + d)], s);
            ui.painter().line_segment([egui::pos2(c.x + d, c.y - d), egui::pos2(c.x - d, c.y + d)], s);
            if resp.clicked() { ctx.send_viewport_cmd(egui::ViewportCommand::Close); }

            // ── Maximize / Restore ─────────────────────────────────────────
            let maximized = ctx.input(|i| i.viewport().maximized.unwrap_or(false));
            let (rect, resp) = ui.allocate_exact_size(egui::vec2(btn_w, h), egui::Sense::click());
            if resp.hovered() { ui.painter().rect_filled(rect, egui::CornerRadius::ZERO, hover_fill); }
            let c = rect.center();
            let s = egui::Stroke::new(1.5, icon_color);
            if maximized {
                // Restore: two overlapping squares
                let back  = egui::Rect::from_min_size(egui::pos2(c.x - 1.5, c.y - 5.5), egui::vec2(9.0, 8.0));
                let front = egui::Rect::from_min_size(egui::pos2(c.x - 5.0, c.y - 2.0), egui::vec2(9.0, 8.0));
                // Erase the portion of back behind front's top-left so it looks layered.
                ui.painter().rect_filled(
                    egui::Rect::from_min_size(front.min, egui::vec2(3.0, 2.0)),
                    egui::CornerRadius::ZERO,
                    ui.visuals().panel_fill,
                );
                draw_rect_stroke(ui.painter(), back, s);
                draw_rect_stroke(ui.painter(), front, s);
            } else {
                // Maximize: single square
                draw_rect_stroke(ui.painter(), egui::Rect::from_center_size(c, egui::vec2(11.0, 9.0)), s);
            }
            if resp.clicked() { ctx.send_viewport_cmd(egui::ViewportCommand::Maximized(!maximized)); }

            // ── Minimize ───────────────────────────────────────────────────
            let (rect, resp) = ui.allocate_exact_size(egui::vec2(btn_w, h), egui::Sense::click());
            if resp.hovered() { ui.painter().rect_filled(rect, egui::CornerRadius::ZERO, hover_fill); }
            let c = rect.center();
            ui.painter().line_segment(
                [egui::pos2(c.x - 5.5, c.y + 2.0), egui::pos2(c.x + 5.5, c.y + 2.0)],
                egui::Stroke::new(1.5, icon_color),
            );
            if resp.clicked() { ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(true)); }
        });
    });

    // ── Center: logo + app name ────────────────────────────────────────────
    let mid = bar.center();
    let text_color = ui.visuals().text_color();
    let font_id = egui::FontId::proportional(14.0);
    let galley = ui.painter().layout_no_wrap("FlexInput".to_string(), font_id, text_color);
    let text_size = galley.size();

    let logo_w = if logo.is_some() { 20.0 + 6.0 } else { 0.0 };
    let total_w = logo_w + text_size.x;
    let start_x = mid.x - total_w / 2.0;

    let painter = ui.painter();
    if let Some(tex) = logo {
        let uv = egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0));
        let logo_rect = egui::Rect::from_center_size(egui::pos2(start_x + 10.0, mid.y), egui::vec2(20.0, 20.0));
        painter.image(tex.id(), logo_rect, uv, egui::Color32::WHITE);
        painter.galley(egui::pos2(start_x + 20.0 + 6.0, mid.y - text_size.y / 2.0), galley, text_color);
    } else {
        painter.galley(egui::pos2(start_x, mid.y - text_size.y / 2.0), galley, text_color);
    }

    // Process drag / double-click after interactive widgets.
    if drag.drag_started() {
        ctx.send_viewport_cmd(egui::ViewportCommand::StartDrag);
    }
    if drag.double_clicked() {
        let max = ctx.input(|i| i.viewport().maximized.unwrap_or(false));
        ctx.send_viewport_cmd(egui::ViewportCommand::Maximized(!max));
    }
}
