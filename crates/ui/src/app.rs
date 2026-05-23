use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, RwLock};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use eframe::egui;
use egui_snarl::{InPinId, NodeId, OutPinId, Snarl};
use flexinput_core::{ModuleDescriptor, PinDescriptor, Signal, SignalType, SubPatchPin};
use flexinput_devices::{init_backends, midi::cc_display_name, DeviceBackend, HidHideClient, MidiBackend, PhysicalDevice};
use flexinput_engine::{Engine, NodeSnap, ProcessingGraph, ProcessingOutput, SinkBus, spawn_processing_thread};
use flexinput_modules::all_modules;
use flexinput_virtual::VirtualDevice;

use crate::{
    canvas::{sample_curve, Canvas, NodeData},
    canvas::node::{ExposedModule, UiSubPatch},
    canvas::ClipboardData,
    guide_watcher::{spawn_guide_watcher, GuideWatchConfig},
    panels::{physical_devices, virtual_devices::{SharedDevicePool, VirtualDevicePanel}},
    panic_hotkey::{load_panic_shortcut, save_panic_shortcut, spawn_panic_hotkey_listener},
    pin_hotkey::spawn_pin_hotkey_listener,
    settings::{self, AppSettings, PersistedTab, PersistedWorkspace, PinShortcut},
};

/// Human-readable name for a chord button signal (e.g. `"btn_lb"` →
/// `"LB"`). Falls back to the raw signal name for anything we don't
/// recognise.
fn pretty_chord_name(sig: &str) -> String {
    match sig {
        "btn_south"     => "South / A".to_string(),
        "btn_east"      => "East / B".to_string(),
        "btn_west"      => "West / X".to_string(),
        "btn_north"     => "North / Y".to_string(),
        "btn_lb"        => "LB / L1".to_string(),
        "btn_rb"        => "RB / R1".to_string(),
        "btn_lt_dig"    => "LT (digital)".to_string(),
        "btn_rt_dig"    => "RT (digital)".to_string(),
        "btn_ls"        => "L-Stick click".to_string(),
        "btn_rs"        => "R-Stick click".to_string(),
        "btn_start"     => "Start / Options".to_string(),
        "btn_back"      => "Back / Share / Create".to_string(),
        "btn_touchpad"  => "Touchpad click".to_string(),
        "btn_mute"      => "Mute".to_string(),
        other           => other.to_string(),
    }
}

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
    /// Stateless panel renderer. Devices themselves live in the app-level
    /// `shared_virtual_devices` pool, not on the tab.
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

/// Collect every `device_id` string referenced by a `device.sink` node in
/// the given snarl that targets a virtual device (id starts with
/// `"virtual."`). Used to drive shared-pool reconciliation and the
/// active-tab id filter.
fn snarl_virtual_device_ids(snarl: &Snarl<NodeData>) -> Vec<String> {
    snarl
        .nodes_ids_data()
        .filter_map(|(_, n)| {
            let node = &n.value;
            if node.module_id == "device.sink" {
                node.params
                    .get("device_id")
                    .and_then(|v| v.as_str())
                    .filter(|id| id.starts_with("virtual."))
                    .map(|s| s.to_string())
            } else {
                None
            }
        })
        .collect()
}

/// Insert virtual devices into the shared pool for every id in
/// `needed_ids` that doesn't already exist. Pre-existing devices are
/// reused — never duplicated. Devices the pool has but `needed_ids`
/// doesn't list are left alone (pruning is a separate operation).
fn reconcile_shared_devices(
    pool: &mut Vec<Box<dyn VirtualDevice>>,
    needed_ids: &[String],
) {
    for id in needed_ids {
        if !pool.iter().any(|d| d.id() == id.as_str()) {
            if let Some(dev) = try_create_virtual_device(id) {
                pool.push(dev);
            }
        }
    }
}

/// Drop devices from the shared pool whose id is not referenced by any
/// open tab's canvas. Called after closing a tab. Returns the dropped ids
/// (informational).
fn prune_shared_devices(
    pool: &mut Vec<Box<dyn VirtualDevice>>,
    tabs: &[PatchTab],
) -> Vec<String> {
    let mut keep: HashSet<String> = HashSet::new();
    for tab in tabs {
        for id in snarl_virtual_device_ids(&tab.canvas.snarl) {
            keep.insert(id);
        }
    }
    let mut dropped = Vec::new();
    pool.retain(|d| {
        let id = d.id().to_string();
        if keep.contains(&id) {
            true
        } else {
            dropped.push(id);
            false
        }
    });
    dropped
}

// ── Sub-patch editor windows ──────────────────────────────────────────────────

struct SubPatchEditor {
    tab_idx: usize,
    /// NodeId of the sub-patch node in the *parent* snarl (tab canvas or a parent editor).
    node_id: NodeId,
    /// Index into sub_patch_editors of the parent editor, or None if parented to the tab canvas.
    parent_editor_idx: Option<usize>,
    canvas: Canvas,
    open: bool,
    /// Last canvas.clipboard_gen seen; used to detect a genuine user copy inside this editor.
    last_clipboard_gen: u64,
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
    /// Whether the Virtual Devices (top) panel is collapsed (only its heading tab visible).
    virtual_panel_collapsed: bool,
    /// Whether the Physical Devices (bottom) panel is collapsed.
    physical_panel_collapsed: bool,
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
    sub_patch_editors: Vec<SubPatchEditor>,
    /// NodeIds of device.source nodes whose Device Calibration window is open.
    calibration_open: std::collections::HashSet<egui_snarl::NodeId>,
    /// App-level clipboard shared across all Canvas instances (outer tabs and SubPatchEditor
    /// inner canvases). Written whenever a copy action fires in any canvas.
    /// Read when the target canvas has no local clipboard (cross-boundary paste).
    app_clipboard: Option<ClipboardData>,
    /// True when app_clipboard was last written by an inner SubPatchEditor canvas.
    /// Used to force-seed the outer canvas clipboard so inner→outer paste works
    /// even when the outer canvas already has its own stale clipboard.
    app_clipboard_from_inner: bool,
    /// Last clipboard_gen seen from the active tab's outer canvas.
    /// Used to detect genuine user copies without comparing clipboard contents.
    last_outer_clipboard_gen: u64,
    // ── Processing thread shared state ────────────────────────────────────────
    proc_graph: flexinput_engine::ArcGraph,
    proc_device_signals: flexinput_engine::ArcSignals,
    proc_outputs: Arc<Mutex<ProcessingOutput>>,
    // ── I/O thread shared state ───────────────────────────────────────────────
    /// App-level shared pool of virtual output devices. Same instance of
    /// `virtual.xinput.0` is reused across every tab that references it.
    /// Membership is reconciled on workspace restore, patch load, and tab
    /// close (pruning).
    shared_virtual_devices: SharedDevicePool,
    /// Set of virtual device IDs referenced by the *active tab's* canvas.
    /// The I/O thread routes signals only to devices whose id is in this
    /// set; devices owned by background tabs receive `reset_outputs()` each
    /// tick so they don't drive output. Rebuilt by `set_active_tab` and
    /// whenever the active tab's canvas changes.
    active_tab_device_ids: Arc<RwLock<HashSet<String>>>,
    /// Bypass flag: when true the I/O thread calls reset_outputs() instead of flush().
    io_bypass: Arc<AtomicBool>,
    // ── MIDI watch thread shared state ────────────────────────────────────────
    /// MIDI device IDs (`midi_in:N` / `midi_out:N`) referenced by canvas
    /// device.source / device.sink nodes across all tabs. The MIDI watch
    /// thread reads this to decide which OS handles to keep open vs release.
    /// UI rebuilds it each frame from the current canvas state.
    pinned_midi_ids: Arc<RwLock<HashSet<String>>>,
    /// MIDI device list maintained by the MIDI watch thread. Appended into
    /// the I/O thread's shared_devices each enum cycle so the UI sees a
    /// unified gilrs + MIDI list without ever touching the MIDI lock.
    shared_midi_devices: Arc<RwLock<Vec<PhysicalDevice>>>,
    // ── Panic mode ────────────────────────────────────────────────────────────
    /// User-configurable global shortcut to toggle all virtual output off.
    panic_shortcut: PanicShortcut,
    /// True while the panic shortcut is engaged. Forces io_bypass for the
    /// active tab regardless of normal bypass state, until the user toggles off.
    panic_active: bool,
    /// True when the shortcut button is in Learn mode (next chord captures).
    panic_learning: bool,
    /// Set by the global hotkey listener when the configured chord fires.
    /// UI consumes this each frame and toggles `panic_active`.
    panic_toggle_requested: Arc<AtomicBool>,
    /// Live snapshot of the shortcut for the global hotkey listener thread.
    /// Updated whenever the user changes the binding.
    panic_shortcut_shared: Arc<RwLock<PanicShortcut>>,
    // ── Settings ──────────────────────────────────────────────────────────────
    /// User-configurable preferences persisted to settings.json. Mutated by the
    /// Settings window UI; persistence happens on change (debounced).
    settings: AppSettings,
    /// True while the Settings window is shown.
    settings_open: bool,
    /// Set when settings have changed and need to be written out at end of frame.
    settings_dirty: bool,
    /// Live processing rate handed to the engine thread.
    sample_rate_hz: Arc<AtomicU32>,
    /// Live device polling rate handed to the I/O thread.
    polling_hz: Arc<AtomicU32>,
    /// Per-device measured polling rates (device_id → Hz). Written by the
    /// I/O thread, read by the canvas viewer to show live per-device Hz.
    pub device_rates: flexinput_engine::DeviceRates,
    /// Per-pin time-windowed scope rings (raw values + timestamps) for the
    /// calibration window. Populated by the I/O thread at polling Hz.
    pub scope_taps: flexinput_engine::ScopeTaps,
    // ── Always-on-top pin ────────────────────────────────────────────────
    /// Set by both the global keyboard hotkey thread and the Guide-button
    /// watcher thread when the user fires their configured pin toggle. The
    /// UI loop consumes it, flips `settings.pin_active`, and sends the
    /// matching `ViewportCommand::WindowLevel` so the change takes effect
    /// without waiting for the next mouse interaction.
    pin_toggle_requested: Arc<AtomicBool>,
    /// Live snapshot of the pin keyboard chord shared with the hotkey
    /// listener thread. Updated whenever the user re-binds in Settings.
    pin_shortcut_shared: Arc<RwLock<PinShortcut>>,
    /// Live snapshot of the Guide-button watcher config (enabled +
    /// double-tap mode + chord). The watcher reads this each poll
    /// iteration.
    pin_guide_cfg: Arc<RwLock<GuideWatchConfig>>,
    /// AutoMap-style chord learn: set true to ask the watcher to
    /// capture the next pressed button on any device. Watcher clears
    /// it when a capture lands.
    pin_learn_chord: Arc<AtomicBool>,
    /// Result slot for `pin_learn_chord`. Watcher writes the captured
    /// signal name here; UI consumes it on the next frame.
    pin_learned_chord: Arc<Mutex<Option<String>>>,
    /// True while the pin shortcut button is in Learn mode in Settings.
    pin_learning: bool,
    /// HWND of whatever foreground window we left when the pin was last
    /// engaged. Used by the focus flip-flop feature to restore focus to
    /// that window so the user can immediately test their changes.
    /// Stored as `isize` because `HWND` is `!Send`.
    pin_prev_foreground_hwnd: Option<isize>,
    /// Continuously-tracked HWND of the most recent non-FlexInput
    /// foreground window. Sampled each frame from
    /// `process_list::foreground_hwnd()`. Used as the flip-flop target
    /// when the user toggles the pin — by the time they click our pin
    /// button, FlexInput itself is foreground, so we need a remembered
    /// pointer to the window that was foreground *before* that.
    pin_last_external_hwnd: Option<isize>,
    /// FlexInput's own HWND (set on first `update()` call by reading
    /// `eframe::Frame::window_handle()`). Used for direct Win32
    /// operations that can't be routed through eframe — dropping
    /// topmost synchronously on pin-off, applying the click-through
    /// layered-window style, and toggling `WS_EX_TRANSPARENT`. Stored
    /// as `isize` for `Send`-ness.
    self_hwnd: Option<isize>,
    /// Running `puffin_http` server. `Some` exactly while the Profiler
    /// toggle in Settings is on. Dropping the server stops the listener
    /// thread; we also call `puffin::set_scopes_on(false)` so the macros
    /// stop emitting events.
    profiler_server: Option<puffin_http::Server>,
}

/// Keyboard-only shortcut for panic mode. Modifiers + non-modifier key.
/// Stored as serializable strings so it can be persisted to disk.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PanicShortcut {
    pub ctrl:  bool,
    pub shift: bool,
    pub alt:   bool,
    pub win:   bool,
    /// egui::Key as Debug string (e.g. "Escape", "F8"). None means "unassigned".
    pub key:   Option<String>,
}

impl Default for PanicShortcut {
    fn default() -> Self {
        // Ctrl+Backtick — `Backtick` is the egui Debug name for the
        // tilde / backtick key (US layout), unlikely to collide with shell or
        // game bindings while still being easy to mash blind.
        Self { ctrl: true, shift: false, alt: false, win: false, key: Some("Backtick".to_string()) }
    }
}

impl PanicShortcut {
    /// Human-readable label for the button face.
    pub fn label(&self) -> String {
        let mut parts: Vec<&str> = Vec::new();
        if self.ctrl  { parts.push("Ctrl"); }
        if self.shift { parts.push("Shift"); }
        if self.alt   { parts.push("Alt"); }
        if self.win   { parts.push("Win"); }
        let key = self.key.as_deref().unwrap_or("…");
        if parts.is_empty() {
            key.to_string()
        } else {
            format!("{}+{}", parts.join("+"), key)
        }
    }
}

impl FlexInputApp {
    pub fn new(cc: &eframe::CreationContext<'_>, icon_bytes: &[u8]) -> Self {
        setup_fonts(&cc.egui_ctx);
        // Install egui_extras image loaders so SVG images render inside nodes
        // and pinned sub-patch widgets. The svg feature pulls in resvg/usvg.
        egui_extras::install_image_loaders(&cc.egui_ctx);
        let descriptors = all_modules().into_iter().map(|r| r.descriptor).collect();
        let backends    = init_backends();
        let midi_backend = Arc::new(Mutex::new(Some(MidiBackend::new())));
        // HidHide integration disabled pending a proper rewrite.
        let hidhide: Option<HidHideClient> = None;
        let logo_texture = eframe::icon_data::from_png_bytes(icon_bytes).ok().map(|icon| {
            // Pre-multiply alpha so blending at small render sizes doesn't
            // produce dark fringes around the logo's edges.
            let mut premul = Vec::with_capacity(icon.rgba.len());
            for px in icon.rgba.chunks_exact(4) {
                let a = px[3] as u32;
                premul.push((px[0] as u32 * a / 255) as u8);
                premul.push((px[1] as u32 * a / 255) as u8);
                premul.push((px[2] as u32 * a / 255) as u8);
                premul.push(px[3]);
            }
            let image = egui::ColorImage::from_rgba_premultiplied(
                [icon.width as usize, icon.height as usize],
                &premul,
            );
            // Mipmaps + linear min/mag give a clean downscale from the source
            // PNG (large) to the ~20px render size in the title bar.
            let opts = egui::TextureOptions {
                magnification: egui::TextureFilter::Linear,
                minification:  egui::TextureFilter::Linear,
                wrap_mode:     egui::TextureWrapMode::ClampToEdge,
                mipmap_mode:   Some(egui::TextureFilter::Linear),
            };
            cc.egui_ctx.load_texture("app_logo", image, opts)
        });

        // ── Settings ──────────────────────────────────────────────────────
        // Loaded before threads spawn so the engine starts at the user's rate.
        let app_settings = settings::load_settings();
        let sample_rate_hz = Arc::new(AtomicU32::new(app_settings.sample_rate_hz));
        let polling_hz     = Arc::new(AtomicU32::new(app_settings.polling_hz));

        let proc_graph          = flexinput_engine::new_arc_graph();
        let proc_device_signals = flexinput_engine::new_arc_signals();
        let proc_outputs        = Arc::new(Mutex::new(ProcessingOutput::default()));
        let sink_bus: SinkBus   = Arc::new(RwLock::new(HashMap::new()));
        spawn_processing_thread(
            Arc::clone(&proc_graph),
            Arc::clone(&proc_device_signals),
            Arc::clone(&proc_outputs),
            Arc::clone(&sink_bus),
            Arc::clone(&sample_rate_hz),
        );

        // Restore workspace if the user opted in; otherwise start with one empty tab.
        let on_load_view = match app_settings.on_patch_load {
            settings::OnPatchLoad::Off       => None,
            settings::OnPatchLoad::Center    => Some(crate::canvas::PendingViewAction::Center),
            settings::OnPatchLoad::ZoomToFit => Some(crate::canvas::PendingViewAction::ZoomToFit),
        };
        let tabs = if app_settings.keep_workspace {
            match settings::load_workspace() {
                Some(ws) if !ws.tabs.is_empty() => ws.tabs.into_iter().map(|pt| {
                    let mut canvas = Canvas::new();
                    canvas.snarl = pt.snarl;
                    // Restored patches are conceptually "loaded" — honor the
                    // on-patch-load camera setting so the saved view's
                    // arbitrary pan/zoom doesn't strand the user off-canvas.
                    canvas.pending_view_action = on_load_view;
                    PatchTab {
                        title: pt.title,
                        file_path: pt.file_path,
                        bound_exes: pt.bound_exes,
                        canvas,
                        virtual_panel: VirtualDevicePanel::new(),
                        bypassed: false,
                        auto_bypass: pt.auto_bypass,
                    }
                }).collect(),
                _ => vec![PatchTab::new_untitled(1)],
            }
        } else {
            vec![PatchTab::new_untitled(1)]
        };
        let shared_devices = Arc::new(RwLock::new(Vec::<PhysicalDevice>::new()));
        let shared_midi_devices = Arc::new(RwLock::new(Vec::<PhysicalDevice>::new()));
        let pinned_midi_ids = Arc::new(RwLock::new(HashSet::<String>::new()));
        let io_bypass      = Arc::new(AtomicBool::new(false));

        // App-level shared virtual-device pool. Reconciled from every
        // restored tab's canvas so re-opening the app brings back the
        // devices each patch requires (no duplicates: a single shared
        // instance per device id).
        let shared_virtual_devices: SharedDevicePool =
            Arc::new(Mutex::new(Vec::<Box<dyn VirtualDevice>>::new()));
        {
            let mut pool = shared_virtual_devices.lock().unwrap();
            for tab in &tabs {
                let ids = snarl_virtual_device_ids(&tab.canvas.snarl);
                reconcile_shared_devices(&mut pool, &ids);
            }
        }

        // Active-tab device id filter — I/O thread only ticks devices
        // whose id is in this set. Seeded from tab 0's canvas.
        let active_tab_device_ids: Arc<RwLock<HashSet<String>>> = Arc::new(RwLock::new(
            snarl_virtual_device_ids(&tabs[0].canvas.snarl).into_iter().collect(),
        ));

        let device_rates = flexinput_engine::new_device_rates();
        let scope_taps   = flexinput_engine::new_scope_taps();
        spawn_io_thread(
            backends,
            Arc::clone(&midi_backend),
            Arc::clone(&proc_device_signals),
            Arc::clone(&sink_bus),
            Arc::clone(&shared_virtual_devices),
            Arc::clone(&active_tab_device_ids),
            Arc::clone(&io_bypass),
            Arc::clone(&shared_devices),
            Arc::clone(&shared_midi_devices),
            Arc::clone(&polling_hz),
            Arc::clone(&device_rates),
            Arc::clone(&scope_taps),
        );

        spawn_midi_watch_thread(
            Arc::clone(&midi_backend),
            Arc::clone(&pinned_midi_ids),
            Arc::clone(&shared_midi_devices),
        );

        // ── Panic-mode state ──────────────────────────────────────────────
        let panic_shortcut = load_panic_shortcut().unwrap_or_default();
        let panic_toggle_requested = Arc::new(AtomicBool::new(false));
        let panic_shortcut_shared  = Arc::new(RwLock::new(panic_shortcut.clone()));
        spawn_panic_hotkey_listener(
            Arc::clone(&panic_shortcut_shared),
            Arc::clone(&panic_toggle_requested),
        );

        // ── Always-on-top pin state ──────────────────────────────────────
        // Both the keyboard hotkey thread and the Guide-button watcher
        // raise the same `pin_toggle_requested` flag so the UI loop has a
        // single edge to consume each frame.
        let pin_toggle_requested = Arc::new(AtomicBool::new(false));
        let pin_shortcut_shared  = Arc::new(RwLock::new(app_settings.pin_shortcut.clone()));
        let pin_guide_cfg        = Arc::new(RwLock::new(GuideWatchConfig {
            enabled: app_settings.pin_via_guide,
            require_double_tap: app_settings.pin_guide_double_tap,
            chord_signal: app_settings.pin_guide_chord.clone(),
        }));
        let pin_learn_chord      = Arc::new(AtomicBool::new(false));
        let pin_learned_chord    = Arc::new(Mutex::new(None));
        spawn_pin_hotkey_listener(
            Arc::clone(&pin_shortcut_shared),
            Arc::clone(&pin_toggle_requested),
        );
        spawn_guide_watcher(
            Arc::clone(&pin_guide_cfg),
            Arc::clone(&pin_toggle_requested),
            Arc::clone(&proc_device_signals),
            Arc::clone(&pin_learn_chord),
            Arc::clone(&pin_learned_chord),
        );
        // Seed the see-through data slot so the eye button reflects the
        // persisted value on first frame.
        cc.egui_ctx.data_mut(|d| {
            d.insert_temp(
                egui::Id::new(crate::canvas::SEE_THROUGH_DATA_KEY),
                app_settings.see_through_active,
            );
        });

        // Pick `next_untitled` high enough that any restored "Untitled N" tab
        // doesn't collide with a freshly-created one.
        let next_untitled = tabs.iter()
            .filter_map(|t| t.title.strip_prefix("Untitled")
                .and_then(|rest| rest.trim().parse::<u32>().ok()
                    .or_else(|| if rest.is_empty() { Some(1) } else { None })))
            .max()
            .map(|n| n + 1)
            .unwrap_or(2);

        Self {
            engine: Engine::new(),
            tabs,
            active_tab: 0,
            next_untitled,
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
            virtual_panel_collapsed: false,
            physical_panel_collapsed: false,
            auto_switch: false,
            last_fg_exe: String::new(),
            bind_window_open: false,
            bind_window_filter: String::new(),
            bind_window_procs: vec![],
            hidhide_window_open: false,
            hidhide_filter: String::new(),
            hidhide_proc_list: vec![],
            hidhide_whitelist: vec![],
            sub_patch_editors: vec![],
            calibration_open: std::collections::HashSet::new(),
            app_clipboard: None,
            app_clipboard_from_inner: false,
            last_outer_clipboard_gen: 0,
            proc_graph,
            proc_device_signals,
            proc_outputs,
            shared_virtual_devices,
            active_tab_device_ids,
            io_bypass,
            pinned_midi_ids,
            shared_midi_devices,
            panic_shortcut: panic_shortcut.clone(),
            panic_active: false,
            panic_learning: false,
            panic_toggle_requested,
            panic_shortcut_shared,
            settings: app_settings,
            settings_open: false,
            settings_dirty: false,
            sample_rate_hz,
            polling_hz,
            device_rates,
            scope_taps,
            pin_toggle_requested,
            pin_shortcut_shared,
            pin_guide_cfg,
            pin_learn_chord,
            pin_learned_chord,
            pin_learning: false,
            pin_prev_foreground_hwnd: None,
            pin_last_external_hwnd: None,
            self_hwnd: None,
            profiler_server: None,
        }
    }
}

impl eframe::App for FlexInputApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Tell puffin a new frame began. Cheap when scopes are off (atomic
        // check); only does real work while the profiler toggle is on.
        puffin::GlobalProfiler::lock().new_frame();
        puffin::profile_function!();
        let dt = self.last_update.elapsed().as_secs_f32().clamp(0.001, 0.1);
        self.last_update = std::time::Instant::now();

        // Cache our own HWND on the first frame we have one. We need it
        // for direct Win32 work that can't go through eframe: dropping
        // our topmost synchronously, applying click-through, etc.
        #[cfg(windows)]
        if self.self_hwnd.is_none() {
            use raw_window_handle::{HasWindowHandle, RawWindowHandle};
            if let Ok(h) = _frame.window_handle() {
                if let RawWindowHandle::Win32(w) = h.as_raw() {
                    self.self_hwnd = Some(w.hwnd.get() as isize);
                }
            }
        }

        // Apply selected theme + contrast every frame so changes take
        // effect immediately when the user moves the slider in Settings.
        // Also folds in the see-through alpha when active.
        {
            puffin::profile_scope!("apply_theme_and_contrast");
            crate::settings::apply_theme_and_contrast(ctx, &self.settings);
        }

        // Restore persisted always-on-top pin state on the first frame
        // after launch. eframe only honours the runtime WindowLevel
        // command, not a builder hint, so the persisted bool needs to be
        // re-applied here. We piggy-back on `last_fg_exe.is_empty()` —
        // true exactly once at startup — to avoid storing a separate
        // "first frame" flag.
        if self.settings.pin_active && self.last_fg_exe.is_empty()
            && self.pin_prev_foreground_hwnd.is_none()
        {
            ctx.send_viewport_cmd(egui::ViewportCommand::WindowLevel(
                egui::WindowLevel::AlwaysOnTop,
            ));
        }

        // Read the latest device signals written by the I/O thread (500 Hz).
        // `load_full()` returns the current `Arc<HashMap>` — a refcount
        // bump, no map clone. Deref-cloning the map only happens at the
        // few `.last_signals = …` sites that need an owned map.
        {
            puffin::profile_scope!("read_signals_load");
            let snap = self.proc_device_signals.load_full();
            self.last_signals = (*snap).clone();
        }
        // Refresh device list from I/O thread. Both gilrs and MIDI device
        // listings are populated there, so the UI never contends with the
        // 500 Hz MIDI poll lock (which used to cause MIDI cards to flicker
        // in/out whenever the lock was held during a paint).
        {
            puffin::profile_scope!("read_devices_clone");
            self.devices = self.shared_devices.read().unwrap().clone();
        }

        // Publish the set of `midi_in:N` / `midi_out:N` IDs referenced by
        // canvas device.source / device.sink nodes (across all tabs and
        // sub-patch editors). The MIDI watch thread reads this to decide
        // which OS handles to keep open vs release: unpinned handles are
        // closed so loopMIDI can actually remove ports the user deletes.
        {
            puffin::profile_scope!("rebuild_pinned_midi");
            let mut pinned: HashSet<String> = HashSet::new();
            for tab in &self.tabs {
                for (_, n) in tab.canvas.snarl.nodes_ids_data() {
                    if n.value.module_id == "device.source" || n.value.module_id == "device.sink" {
                        if let Some(id) = n.value.params.get("device_id").and_then(|v| v.as_str()) {
                            if id.starts_with("midi_in:") || id.starts_with("midi_out:") {
                                pinned.insert(id.to_string());
                            }
                        }
                    }
                }
            }
            for ed in &self.sub_patch_editors {
                for (_, n) in ed.canvas.snarl.nodes_ids_data() {
                    if n.value.module_id == "device.source" || n.value.module_id == "device.sink" {
                        if let Some(id) = n.value.params.get("device_id").and_then(|v| v.as_str()) {
                            if id.starts_with("midi_in:") || id.starts_with("midi_out:") {
                                pinned.insert(id.to_string());
                            }
                        }
                    }
                }
            }
            *self.pinned_midi_ids.write().unwrap() = pinned;
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

        // Track the most recent external (non-FlexInput) foreground HWND
        // so the pin flip-flop has a target when the user toggles via the
        // in-app button (at which point FlexInput is foreground and a
        // fresh `foreground_hwnd()` call returns None). Cheap on Windows
        // — one user32 call per frame. Skipped while the pin is engaged
        // because FlexInput sits on top and any "external" window the
        // user clicks would itself be a flip target we don't want to
        // overwrite.
        if !self.settings.pin_active {
            if let Some(hwnd) = crate::process_list::foreground_hwnd() {
                self.pin_last_external_hwnd = Some(hwnd);
            }
        }

        // Consume any global-hotkey toggle requests from the hook thread BEFORE
        // computing effective_bypass, so the visible tab-dot flips this frame.
        if self.panic_toggle_requested.swap(false, Ordering::Relaxed) {
            self.panic_active = !self.panic_active;
        }

        // ── See-through: mirror the eye-toggle data slot into settings ────
        // The zoom-overlay button writes the new state into a temp data
        // slot (so it doesn't need a mutable reference to settings); we
        // pull that here. Writing back the slot every frame is harmless —
        // it's a `Cell`-style store, not a queue.
        {
            let key = egui::Id::new(crate::canvas::SEE_THROUGH_DATA_KEY);
            let from_slot: bool = ctx.data(|d| d.get_temp::<bool>(key))
                .unwrap_or(self.settings.see_through_active);
            if from_slot != self.settings.see_through_active {
                self.settings.see_through_active = from_slot;
                self.settings_dirty = true;
            }
        }

        // ── Pin / always-on-top toggle ────────────────────────────────────
        // The keyboard listener thread AND the Guide-button watcher share
        // the same `pin_toggle_requested` flag — we consume it once per
        // frame and re-apply the WindowLevel command so the change takes
        // effect immediately. We also handle the optional focus flip-flop
        // (capture HWND on pin-on, restore on pin-off) here, on the UI
        // thread, where the Win32 calls are safe to make.
        if self.pin_toggle_requested.swap(false, Ordering::Relaxed) {
            self.toggle_pin(ctx);
        }

        // Effective bypass: manual toggle OR (auto mode on AND auto-bypass AND bound process not in focus).
        // Panic mode forces bypass on the active tab so its bypass indicator
        // flips green→orange the moment the shortcut fires.
        let active_idx = self.active_tab;
        let panic_active_now = self.panic_active;
        let effective_bypass: Vec<bool> = self.tabs.iter().enumerate().map(|(i, tab)| {
            let auto = self.auto_switch
                && tab.auto_bypass
                && !tab.bound_exes.is_empty()
                && !tab.bound_exes.iter().any(|b| b.eq_ignore_ascii_case(&self.last_fg_exe));
            let panic = panic_active_now && i == active_idx;
            tab.bypassed || auto || panic
        }).collect();

        let canvas_has_nodes = self.tabs[self.active_tab].canvas.snarl.nodes_ids_data().next().is_some();

        // Push a fresh graph snapshot to the processing thread each frame.
        {
            puffin::profile_scope!("build_and_publish_graph");
            let (graph_snap, dirty_uids) = {
                puffin::profile_scope!("build_processing_graph");
                let snarl = &self.tabs[self.active_tab].canvas.snarl;
                build_processing_graph(snarl)
            };
            {
                puffin::profile_scope!("write_proc_graph");
                // ArcSwap publish: proc thread reads via `load()` which
                // is lock-free and only refcount-bumps the Arc handle.
                self.proc_graph.store(std::sync::Arc::new(graph_snap));
            }
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
        puffin::profile_scope!("pull_outputs_and_display");
        self.eval_cache.clear();
        if canvas_has_nodes {
            let (last_inputs_snap, last_outputs_snap, scope_batch) = {
                let mut out = self.proc_outputs.lock().unwrap();
                for (&(uid, pin), &sig) in &out.node_outputs {
                    self.eval_cache.insert((NodeId(uid), pin), sig);
                }
                let last = out.last_inputs.clone();
                let last_out = out.last_outputs.clone();
                let scopes = std::mem::take(&mut out.scope_pending);
                (last, last_out, scopes)
            };
            // Group scope samples by uid so each node receives its full batch.
            let mut scope_lookup: HashMap<usize, Vec<Vec<Option<f32>>>> = HashMap::new();
            for (uid, sample) in scope_batch {
                scope_lookup.entry(uid).or_default().push(sample);
            }
            // Walk root + recurse into subpatch inner snarls so inner display
            // nodes (oscilloscope, response_curve, gyro_3dof, …) receive their
            // visual feedback. Inner nodes are keyed by namespaced_uid; matches
            // what eval_subgraph wrote into last_inputs / scope_samples.
            apply_display_state(
                &mut self.tabs[self.active_tab].canvas.snarl,
                None,
                &last_inputs_snap,
                &last_outputs_snap,
                &scope_lookup,
            );
        }

        // Signal routing and device flushing are handled by the 500 Hz I/O thread.
        // panic_active is already folded into effective_bypass above so the
        // tab-bar indicator and the I/O thread stay in sync from the same source.
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
        let mut toggle_settings = false;
        let mut do_pin_toggle = false;
        let pin_active_now = self.settings.pin_active;
        let can_undo = self.tabs[self.active_tab].canvas.can_undo();
        let can_redo = self.tabs[self.active_tab].canvas.can_redo();
        let title_frame = egui::Frame::NONE.fill(ctx.style().visuals.panel_fill);
        egui::TopBottomPanel::top("title_bar")
            .exact_height(32.0)
            .frame(title_frame)
            .show_separator_line(false)
            .show(ctx, |ui| {
                show_title_bar(
                    ui, ctx,
                    &mut do_save, &mut do_load, &mut do_new, &mut do_close, &mut do_bind,
                    &mut do_hidhide,
                    &mut self.auto_switch,
                    &mut do_undo, &mut do_redo,
                    can_undo, can_redo,
                    &self.logo_texture,
                    &mut self.panic_shortcut,
                    &mut self.panic_active,
                    &mut self.panic_learning,
                    &self.panic_shortcut_shared,
                    &mut toggle_settings,
                    pin_active_now,
                    &mut do_pin_toggle,
                );
            });
        if toggle_settings {
            self.settings_open = !self.settings_open;
        }
        if do_pin_toggle {
            self.toggle_pin(ctx);
        }

        // ── Tab bar ───────────────────────────────────────────────────────────────
        let tab_bar_frame = egui::Frame::NONE.fill(ctx.style().visuals.widgets.noninteractive.bg_fill);
        let (tab_switch, tab_close_idx, tab_new, bypass_toggle_idx) = egui::TopBottomPanel::top("tab_bar")
            .exact_height(28.0)
            .frame(tab_bar_frame)
            .show_separator_line(false)
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

        // ── Settings window ───────────────────────────────────────────────────
        self.draw_settings_window(ctx);
        if self.settings_dirty {
            settings::save_settings(&self.settings);
            self.settings_dirty = false;
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
            // Clamp active_tab before set_active_tab reads self.tabs[self.active_tab].
            self.active_tab = self.active_tab.min(self.tabs.len() - 1);
            // Force the active-id set + I/O filter refresh even when
            // new_idx == active_tab (set_active_tab early-returns on no-op);
            // the closing tab may have referenced devices that are now
            // orphaned and we want to silence them before pruning.
            self.refresh_active_tab_device_ids();
            self.set_active_tab(new_idx);
            // Prune the shared pool: drop any device no remaining tab
            // references. The Drop impl on each VirtualDevice releases
            // the underlying OS resource (ViGEm target, enigo handles).
            {
                let mut pool = self.shared_virtual_devices.lock().unwrap();
                let _ = prune_shared_devices(&mut pool, &self.tabs);
            }
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
            // Saved patches record the device ids referenced by the active
            // tab's canvas — that's the contract `.fxp` consumers expect.
            // The shared pool may hold devices owned by other tabs that
            // shouldn't end up in this file.
            let vids: Vec<String> =
                snarl_virtual_device_ids(&self.tabs[self.active_tab].canvas.snarl);
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
                // Queue the configured on-load camera action. The canvas
                // consumes this on its next render — it can't apply now
                // because SnarlState / snarl_rect aren't in scope here.
                tab.canvas.pending_view_action = match self.settings.on_patch_load {
                    settings::OnPatchLoad::Off       => None,
                    settings::OnPatchLoad::Center    =>
                        Some(crate::canvas::PendingViewAction::Center),
                    settings::OnPatchLoad::ZoomToFit =>
                        Some(crate::canvas::PendingViewAction::ZoomToFit),
                };
                // Reconcile the shared pool: union of the saved id list
                // and the canvas-referenced ids (canvas wins on disagreement
                // because that's what the user will see). Reuses existing
                // pool entries — no duplicate instances.
                let canvas_vids = snarl_virtual_device_ids(&tab.canvas.snarl);
                let mut needed: Vec<String> = vids;
                for cv in canvas_vids {
                    if !needed.iter().any(|v| v == &cv) {
                        needed.push(cv);
                    }
                }
                {
                    let mut pool = self.shared_virtual_devices.lock().unwrap();
                    reconcile_shared_devices(&mut pool, &needed);
                }
                // Active-tab canvas changed — refresh the I/O filter so the
                // new tab's devices start receiving signals this frame.
                self.refresh_active_tab_device_ids();
                // Prune any devices the previous canvas needed but the new
                // one (and no other tab) does.
                {
                    let mut pool = self.shared_virtual_devices.lock().unwrap();
                    prune_shared_devices(&mut pool, &self.tabs);
                }
            }
        }

        // Build live device IDs for the active tab's canvas status dots.
        let live_device_ids: std::collections::HashSet<String> = {
            let active_ids: std::collections::HashSet<String> =
                self.active_tab_device_ids.read().unwrap().clone();
            let virtual_live: Vec<String> = {
                let devs = self.shared_virtual_devices.lock().unwrap();
                devs.iter()
                    .filter(|d| d.is_connected() && active_ids.contains(d.id()))
                    .map(|d| d.id().to_string())
                    .collect()
            };
            self.devices.iter().map(|d| d.id.clone())
                .chain(virtual_live)
                .collect()
        };

        // Optionally hide FlexInput's own ViGEm virtuals from the
        // physical-devices panel. The gilrs backend already dedups based
        // on SetupAPI counts, but its `phys_counts` cache is refreshed
        // every ~2s — a freshly-created virtual can therefore leak into
        // the displayed list for up to that window. Filter by kind here
        // using the shared pool as authoritative for "ours": one
        // virtual.xinput → drop one ControllerKind::XInput entry, etc.
        let devices_owned;
        let devices: &[PhysicalDevice] = if self.settings.show_own_virtuals_as_physical {
            &self.devices
        } else {
            let mut to_skip: HashMap<flexinput_devices::ControllerKind, usize> = HashMap::new();
            {
                let pool = self.shared_virtual_devices.lock().unwrap();
                for d in pool.iter() {
                    let prefix = flexinput_virtual::kind_prefix(d.id());
                    let kind = match prefix.as_str() {
                        "virtual.xinput" => Some(flexinput_devices::ControllerKind::XInput),
                        "virtual.ds4"    => Some(flexinput_devices::ControllerKind::DualShock4),
                        _ => None,
                    };
                    if let Some(k) = kind {
                        *to_skip.entry(k).or_insert(0) += 1;
                    }
                }
            }
            // Walk in reverse so we drop the *last* N entries of each
            // matching kind — gilrs lists in plug order so a real
            // controller plugged before our virtual stays visible.
            let mut keep: Vec<bool> = vec![true; self.devices.len()];
            for (k, n) in to_skip.iter() {
                let mut remaining = *n;
                for i in (0..self.devices.len()).rev() {
                    if remaining == 0 { break; }
                    if keep[i] && self.devices[i].kind == *k {
                        keep[i] = false;
                        remaining -= 1;
                    }
                }
            }
            devices_owned = self.devices.iter().enumerate()
                .filter_map(|(i, d)| if keep[i] { Some(d.clone()) } else { None })
                .collect::<Vec<_>>();
            &devices_owned
        };
        let bottom_panel_height = self.bottom_panel_height;
        // Pre-compute the set of device ids referenced by *non-active* tab
        // canvases so the panel can grey out the close (X) button on any
        // chip another tab still needs.
        let referenced_by_other_tabs: std::collections::HashSet<String> = {
            let mut s = std::collections::HashSet::new();
            for (i, t) in self.tabs.iter().enumerate() {
                if i == self.active_tab { continue; }
                for id in snarl_virtual_device_ids(&t.canvas.snarl) {
                    s.insert(id);
                }
            }
            s
        };
        let shared_pool_for_panel = Arc::clone(&self.shared_virtual_devices);
        let tab = &mut self.tabs[self.active_tab];
        let (virtual_panel, canvas) = (&mut tab.virtual_panel, &mut tab.canvas);

        // Device panels use a darker fill so they read as separate from the
        // canvas surface and the floating heading tabs visually integrate.
        let panel_frame = {
            let bg = ctx.style().visuals.panel_fill;
            let dark = crate::panels::physical_devices::darken_color(bg, 0.35);
            egui::Frame::default()
                .fill(dark)
                .inner_margin(egui::Margin::symmetric(8, 2))
        };
        let collapsed_frame = egui::Frame::default().inner_margin(egui::Margin::ZERO);

        // Animation progress: 0.0 = fully collapsed, 1.0 = fully expanded.
        // ctx.animate_bool_with_time eases between targets smoothly.
        let virt_open = ctx.animate_bool_with_time(
            egui::Id::new("virt_panel_open_anim"),
            !self.virtual_panel_collapsed,
            0.18,
        );
        let phys_open = ctx.animate_bool_with_time(
            egui::Id::new("phys_panel_open_anim"),
            !self.physical_panel_collapsed,
            0.18,
        );

        const VIRT_NATURAL_H: f32 = 36.0;
        const PHYS_NATURAL_H: f32 = 44.0;
        const COLLAPSED_H:    f32 = 1.0;
        let virt_h = COLLAPSED_H.max(VIRT_NATURAL_H * virt_open);
        let phys_h = COLLAPSED_H.max(PHYS_NATURAL_H * phys_open);

        // Build a frame that scales its vertical margins with the animation
        // progress so the panel can actually shrink to ~0 height. Once fully
        // open we use the regular frame so margins are stable for layout.
        let scaled_frame = |progress: f32| -> egui::Frame {
            let bg = ctx.style().visuals.panel_fill;
            let dark = crate::panels::physical_devices::darken_color(bg, 0.35);
            let vpad = (2.0 * progress).round() as i8;
            egui::Frame::default()
                .fill(dark)
                .inner_margin(egui::Margin { left: 8, right: 8, top: vpad, bottom: vpad })
        };
        let top_frame = if virt_open < 0.001 { collapsed_frame }
                        else if virt_open > 0.999 { panel_frame }
                        else { scaled_frame(virt_open) };
        let bot_frame = if phys_open < 0.001 { collapsed_frame }
                        else if phys_open > 0.999 { panel_frame }
                        else { scaled_frame(phys_open) };

        let default_collapsed = self.settings.device_nodes_default_collapsed;
        let device_defaults = crate::canvas::DeviceParamDefaults {
            stick_deadzone: self.settings.default_stick_deadzone,
            gyro_mult: self.settings.default_gyro_mult,
            mouse_sensitivity: self.settings.default_mouse_sensitivity,
        };

        let top_resp = egui::TopBottomPanel::top("virtual_devices_panel")
            .resizable(false)
            .exact_height(virt_h)
            .frame(top_frame)
            .show(ctx, |ui| {
                if virt_open > 0.01 {
                    virtual_panel.show(
                        ui,
                        &shared_pool_for_panel,
                        canvas,
                        default_collapsed,
                        device_defaults,
                        &|id| referenced_by_other_tabs.contains(id),
                    );
                }
            });

        let bottom_resp = egui::TopBottomPanel::bottom("physical_devices_panel")
            .resizable(false)
            .exact_height(phys_h)
            .frame(bot_frame)
            .show(ctx, |ui| {
                if phys_open > 0.01 {
                    physical_devices::show(ui, devices, canvas, default_collapsed, device_defaults);
                }
            });
        // Only record the natural height when fully expanded so the snapshot
        // isn't poisoned by the in-flight animation values.
        if phys_open > 0.99 {
            self.bottom_panel_height = bottom_resp.response.rect.height();
        }

        // Floating heading tabs hang off each panel's canvas-facing edge,
        // nudged 1px down to sit on the canvas inner border.
        let top_rect = top_resp.response.rect;
        let bottom_rect = bottom_resp.response.rect;
        let top_anchor_y = top_rect.bottom() + 5.0;
        let bottom_anchor_y = bottom_rect.top() - 1.0;
        let top_tab = crate::panels::physical_devices::draw_floating_heading(
            ctx,
            "heading_virtual_devices",
            "Virtual Devices",
            egui::pos2(top_rect.left() + 12.0, top_anchor_y),
            crate::panels::physical_devices::TabDirection::Down,
        );
        if top_tab.clicked() {
            self.virtual_panel_collapsed = !self.virtual_panel_collapsed;
        }
        let bottom_tab = crate::panels::physical_devices::draw_floating_heading(
            ctx,
            "heading_physical_devices",
            "Physical Devices",
            egui::pos2(bottom_rect.left() + 12.0, bottom_anchor_y),
            crate::panels::physical_devices::TabDirection::Up,
        );
        if bottom_tab.clicked() {
            self.physical_panel_collapsed = !self.physical_panel_collapsed;
        }

        // Seed outer canvas clipboard from inner when app_clipboard_from_inner is set
        // (user copied inside a sub-patch editor last frame) or on first use.
        if self.app_clipboard_from_inner || canvas.clipboard().is_none() {
            if let Some(ref cb) = self.app_clipboard {
                canvas.set_clipboard(cb.clone());
            }
        }
        let outer_gen_before = canvas.clipboard_gen;

        let device_rates_snap = self.device_rates.read().map(|r| r.clone()).unwrap_or_default();
        let mut calibrate_request: Option<egui_snarl::NodeId> = None;
        // Two-way bridge between settings.see_through_alpha and the
        // temp data slot that the eye-button popover slider edits.
        // If the slot value differs from settings (popover user moved
        // the slider), pull the new value into settings and mark
        // dirty. Then publish settings back to the slot so any other
        // reader (canvas bg_frame fill) sees the up-to-date value.
        {
            let alpha_id = egui::Id::new(crate::canvas::SEE_THROUGH_ALPHA_KEY);
            let from_slot: Option<f32> = ctx.data(|d| d.get_temp::<f32>(alpha_id));
            if let Some(a) = from_slot {
                if (a - self.settings.see_through_alpha).abs() > f32::EPSILON {
                    self.settings.see_through_alpha = a.clamp(0.0, 1.0);
                    self.settings_dirty = true;
                }
            }
            ctx.data_mut(|d| {
                d.insert_temp(alpha_id, self.settings.see_through_alpha);
            });
        }
        // CentralPanel handling has two modes:
        //  * Opaque: default `Frame::central_panel(&style)` — its
        //    `panel_fill` paints the whole area, and the 8 px
        //    inner_margin gives the implicit "frame surround" band
        //    around the snarl rect.
        //  * See-through: we need the inner area to actually be
        //    transparent so the snarl `bg_frame` alpha can show the
        //    desktop. We use a TRANSPARENT frame fill (so the inner
        //    area doesn't paint), but keep `inner_margin(8)` so the
        //    snarl rect lands in the same place as opaque mode. Then
        //    we explicitly paint 4 opaque bands in the 8 px surround
        //    region — this is the FRAME, promoted from
        //    accidental-byproduct of the central panel fill to a
        //    deliberate UI element.
        let style_snapshot = ctx.style();
        let see_through_on = self.settings.see_through_active;
        let central_frame = if see_through_on {
            egui::Frame::central_panel(&style_snapshot)
                .fill(egui::Color32::TRANSPARENT)
        } else {
            egui::Frame::central_panel(&style_snapshot)
        };
        egui::CentralPanel::default().frame(central_frame).show(ctx, |ui| {
            if see_through_on {
                // `ui.max_rect()` here is the central panel's content
                // area AFTER the 8 px inner_margin has been applied,
                // so the visible "frame" surround band sits in the
                // 8 px ring OUTSIDE this rect. Paint that ring on the
                // ctx's background layer painter (which spans the
                // whole window) using the panel_fill color.
                let inner = ui.max_rect();
                let outer = inner.expand(8.0);
                let p = ctx.layer_painter(egui::LayerId::background());
                let frame_color = style_snapshot.visuals.panel_fill;
                // Top
                p.rect_filled(
                    egui::Rect::from_min_max(outer.left_top(),
                        egui::pos2(outer.right(), inner.top())),
                    egui::CornerRadius::ZERO, frame_color);
                // Bottom
                p.rect_filled(
                    egui::Rect::from_min_max(
                        egui::pos2(outer.left(), inner.bottom()),
                        outer.right_bottom()),
                    egui::CornerRadius::ZERO, frame_color);
                // Left (between top and bottom bands)
                p.rect_filled(
                    egui::Rect::from_min_max(
                        egui::pos2(outer.left(), inner.top()),
                        egui::pos2(inner.left(), inner.bottom())),
                    egui::CornerRadius::ZERO, frame_color);
                // Right
                p.rect_filled(
                    egui::Rect::from_min_max(
                        egui::pos2(inner.right(), inner.top()),
                        egui::pos2(outer.right(), inner.bottom())),
                    egui::CornerRadius::ZERO, frame_color);
            }
            puffin::profile_scope!("canvas_show");
            calibrate_request = crate::panels::canvas::show(
                canvas, &self.descriptors, &live_device_ids, &self.last_signals,
                &self.panic_shortcut, devices, &device_rates_snap,
                device_defaults, ui,
            );
        });
        if let Some(node) = calibrate_request {
            self.calibration_open.insert(node);
        }
        {
            puffin::profile_scope!("calibration_show_windows");
            crate::panels::calibration::show_windows(ctx, canvas, &mut self.calibration_open, &self.last_signals, &self.scope_taps);
        }

        // Only update app_clipboard from outer canvas when the user actually copied
        // (gen advanced). Clear from_inner flag regardless so seeding doesn't repeat.
        if canvas.clipboard_gen != outer_gen_before {
            self.last_outer_clipboard_gen = canvas.clipboard_gen;
            if let Some(cb) = canvas.clipboard() {
                self.app_clipboard = Some(cb);
            }
        }
        self.app_clipboard_from_inner = false;

        // ── Sub-patch editor windows ──────────────────────────────────────────
        show_subpatch_editors(self, ctx, &live_device_ids);

        // When the patch is live (has nodes or virtual devices), paint every
        // vsync — `request_repaint()` with no delay tells eframe to paint
        // next frame; the swap-chain throttles to the monitor refresh rate,
        // so no beating with arbitrary monitor refresh rates.
        //
        // When idle (empty patch and no virtual devices), fall back to a
        // slow 100 ms tick. Always-repainting on the desktop PC produces
        // worse strobing than the conditional path on some GPU/driver
        // combos (likely a glow + DWM swap-chain interaction). Empty patch
        // is a transient state; once a node is dropped the live path kicks
        // in and rendering is smooth.
        // Repaint heuristic: live path runs whenever a virtual device is
        // referenced by the active tab's canvas. Background-tab devices
        // don't need vsync repaints here — their UI is hidden.
        let has_virtual = !self.active_tab_device_ids.read().unwrap().is_empty();
        if canvas_has_nodes || has_virtual {
            ctx.request_repaint();
        } else {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }

        // ── Window border ────────────────────────────────────────────────
        // We turned off OS decorations (`with_decorations(false)`) so
        // paint a 1 px subtle border ourselves. Skipped while
        // maximized so the window snaps cleanly to monitor edges.
        let maximized = ctx.input(|i| i.viewport().maximized.unwrap_or(false));
        if !maximized {
            let rect = ctx.input(|i| i.screen_rect());
            let painter = ctx.layer_painter(egui::LayerId::new(
                egui::Order::Foreground,
                egui::Id::new("app_window_border"),
            ));
            // Inset by half a pixel so the 1px stroke sits inside the
            // window bounds and isn't cropped by the corner region.
            let border_rect = rect.shrink(0.5);
            let stroke_color = egui::Color32::from_rgba_unmultiplied(255, 255, 255, 40);
            painter.rect_stroke(
                border_rect,
                egui::CornerRadius::ZERO,
                egui::Stroke::new(1.0, stroke_color),
                egui::StrokeKind::Inside,
            );
        }

        handle_window_resize(ctx);

        // ── Reconcile shared virtual-device pool against canvas state ────
        // Cheap; catches sink-node adds/removes that happened anywhere
        // this frame (panel "+", panel "X", canvas drop, right-click
        // delete, undo/redo, paste). Runs once per frame at the end so
        // every edit path converges on a consistent pool.
        {
            let needed: Vec<String> = self.tabs.iter()
                .flat_map(|t| snarl_virtual_device_ids(&t.canvas.snarl))
                .collect();
            let mut pool = self.shared_virtual_devices.lock().unwrap();
            reconcile_shared_devices(&mut pool, &needed);
            let _ = prune_shared_devices(&mut pool, &self.tabs);
        }
        // Refresh the I/O thread's active-tab device id filter.
        self.refresh_active_tab_device_ids();
    }

    /// Called by eframe just before the application exits. Persist workspace
    /// (if opted in) and settings here.
    fn on_exit(&mut self) {
        settings::save_settings(&self.settings);
        self.save_workspace_now();
    }

    /// Override of the default eframe clear color (which is a 70%-opaque
    /// dark gray — meant to look reasonable in non-transparent mode and to
    /// give a hint of the desktop in transparent mode). For our see-through
    /// toggle to actually go fully transparent we have to clear to RGBA 0;
    /// otherwise that opaque-by-default gray sits behind the canvas's
    /// alpha-faded `bg_frame` and you get a "more gray" effect at low alpha
    /// instead of full transparency.
    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        if self.settings.see_through_active {
            [0.0, 0.0, 0.0, 0.0]
        } else {
            // Mirror the eframe default — dark gray, ~70% alpha — so the
            // non-transparent mode looks identical to before.
            egui::Color32::from_rgba_unmultiplied(12, 12, 12, 180)
                .to_normalized_gamma_f32()
        }
    }
}

impl FlexInputApp {
    /// Switch the active tab and refresh the I/O thread's device id filter.
    /// Devices in the shared pool that the new tab doesn't reference are
    /// silenced (`reset_outputs()`) immediately so a drifting axis from
    /// the old tab doesn't leak into the new tab's idle frame.
    fn set_active_tab(&mut self, idx: usize) {
        if idx == self.active_tab { return; }
        self.active_tab = idx;
        self.refresh_active_tab_device_ids();
        // Silence everything not referenced by the new active tab. The I/O
        // thread will keep silencing them each tick; this immediate pass
        // matters because the I/O thread runs at 500 Hz and the UI thread
        // controls flush ordering on tab switch.
        let active_ids = self.active_tab_device_ids.read().unwrap().clone();
        let mut devs = self.shared_virtual_devices.lock().unwrap();
        for dev in devs.iter_mut() {
            if !active_ids.contains(dev.id()) {
                dev.reset_outputs();
            }
        }
    }

    /// Rebuild `active_tab_device_ids` from the current active tab's
    /// canvas. Cheap; call whenever the canvas content changes in a way
    /// that adds/removes a device.sink node.
    fn refresh_active_tab_device_ids(&self) {
        let ids: HashSet<String> = snarl_virtual_device_ids(
            &self.tabs[self.active_tab].canvas.snarl,
        ).into_iter().collect();
        *self.active_tab_device_ids.write().unwrap() = ids;
    }

    /// Flip the always-on-top pin state. Sends the matching `WindowLevel`
    /// viewport command, persists to settings, and runs the optional focus
    /// flip-flop:
    ///   * On pin-on: capture the previous foreground HWND (if any) so we
    ///     can restore it on pin-off. After applying always-on-top the
    ///     window itself is now foreground — but it took a frame to get
    ///     there, so we read the foreground BEFORE the command lands.
    ///   * On pin-off: bring the previously-captured HWND back to the
    ///     front (best-effort — Windows may block this if too much time
    ///     has passed; see `process_list::bring_hwnd_to_front`).
    fn toggle_pin(&mut self, ctx: &egui::Context) {
        let new_state = !self.settings.pin_active;
        if new_state && self.settings.focus_flip_flop {
            // Use the continuously-tracked last-external HWND rather than
            // a fresh `foreground_hwnd()` call: by the time the user
            // clicks the pin button (or the hotkey thread sets the toggle
            // flag and we get scheduled), FlexInput itself is usually
            // foreground, so a fresh read would return None.
            self.pin_prev_foreground_hwnd = self.pin_last_external_hwnd;
        }
        self.settings.pin_active = new_state;
        self.settings_dirty = true;
        let level = if new_state {
            egui::WindowLevel::AlwaysOnTop
        } else {
            egui::WindowLevel::Normal
        };
        ctx.send_viewport_cmd(egui::ViewportCommand::WindowLevel(level));
        // Pin-off: drop our topmost synchronously via Win32. eframe
        // defers the `WindowLevel::Normal` command into winit's event
        // loop, so if we relied on it alone we'd still be in the
        // topmost band when bring_hwnd_to_front runs. Direct
        // SetWindowPos(self, HWND_NOTOPMOST) takes effect immediately.
        if !new_state {
            if let Some(hwnd) = self.self_hwnd {
                crate::process_list::drop_topmost(hwnd);
            }
            if self.settings.focus_flip_flop {
                if let Some(hwnd) = self.pin_prev_foreground_hwnd.take() {
                    let _ = crate::process_list::bring_hwnd_to_front(hwnd);
                }
            }
        }
    }

    /// Toggle our pseudo-maximize state. Avoids the WS_POPUP overshoot
    /// (~7-8 px gap on the taskbar edge of vertical-taskbar setups) by
    /// fitting the outer rect to the monitor work area directly instead
    /// of calling `ViewportCommand::Maximized`. The OS therefore never
    /// thinks we're maximized; we manage the toggle ourselves.
    // (Removed: pseudo_maximize. We now use the OS `Maximized` viewport
    // command, which gives correct native behavior — Aero Snap, drag-
    // from-top, restore on drag-from-titlebar — at the cost of a 7-8 px
    // overshoot on the taskbar edge for borderless windows. That can
    // be fixed properly later by subclassing the window proc and
    // handling WM_GETMINMAXINFO to clamp to work area.)

    /// Render the Settings modal. Reads/writes `self.settings`, mirrors live
    /// values into the engine/I-O atomics, and flips `settings_dirty` so the
    /// outer update loop persists settings.json at end of frame.
    fn draw_settings_window(&mut self, ctx: &egui::Context) {
        if !self.settings_open { return; }
        let mut open = true;
        let mut dirty = false;
        let mut save_workspace = false;

        egui::Window::new("Settings")
            .id(egui::Id::new("settings_window"))
            .collapsible(false)
            .resizable(true)
            .default_size([460.0, 520.0])
            .max_size(egui::vec2(560.0, 720.0))
            .open(&mut open)
            .show(ctx, |ui| {
                ui.vertical_centered(|ui| {
                    ui.heading("FlexInput");
                    ui.label(egui::RichText::new(format!("v{}", env!("CARGO_PKG_VERSION")))
                        .small().color(egui::Color32::from_gray(170)));
                });
                ui.add_space(8.0);
                // Settings content grew tall enough to fall off screen on
                // common 1080p / smaller displays — wrap in a scroll area
                // so users can reach the bottom sections (Workspace,
                // Device defaults, Links, Credits) without resizing the
                // window manually.
                egui::ScrollArea::vertical().show(ui, |ui| {
                ui.separator();
                ui.add_space(6.0);

                // ── Performance ─────────────────────────────────────────
                ui.label(egui::RichText::new("Performance").strong());
                ui.add_space(4.0);

                ui.horizontal(|ui| {
                    ui.label("Max polling rate")
                        .on_hover_text("Upper bound for the I/O loop. Actual per-device rate depends on the device — see the live Hz on each device's header in the canvas.");
                    let resp = ui.add(egui::Slider::new(
                        &mut self.settings.polling_hz,
                        settings::POLLING_HZ_MIN..=settings::POLLING_HZ_MAX,
                    ).suffix(" Hz"));
                    if resp.changed() {
                        self.polling_hz.store(self.settings.polling_hz, Ordering::Relaxed);
                        dirty = true;
                    }
                });
                ui.label(egui::RichText::new(
                    "How often the I/O thread polls gamepads and MIDI devices."
                ).small().color(egui::Color32::from_gray(140)));

                ui.add_space(8.0);

                ui.horizontal(|ui| {
                    ui.label("Processing sample rate");
                    let resp = ui.add(egui::Slider::new(
                        &mut self.settings.sample_rate_hz,
                        settings::SAMPLE_RATE_HZ_MIN..=settings::SAMPLE_RATE_HZ_MAX,
                    ).suffix(" Hz"));
                    if resp.changed() {
                        self.sample_rate_hz.store(self.settings.sample_rate_hz, Ordering::Relaxed);
                        dirty = true;
                    }
                });
                ui.label(egui::RichText::new(
                    "Engine tick rate. Higher = lower latency, more CPU."
                ).small().color(egui::Color32::from_gray(140)));

                ui.add_space(10.0);
                ui.separator();
                ui.add_space(6.0);

                // ── Appearance ──────────────────────────────────────────
                ui.label(egui::RichText::new("Appearance").strong());
                ui.add_space(4.0);
                // Theme switching is not yet plumbed through every custom-
                // painted UI element (canvas viewer overlays, calibration
                // scope backgrounds, response-curve grid, etc.), so the
                // app is dark-only for now. The contrast slider still
                // works on the dark theme.
                ui.horizontal(|ui| {
                    ui.label("Contrast:");
                    let mut c = self.settings.contrast;
                    let resp = ui.add(egui::Slider::new(&mut c, -1.0_f32..=1.0)
                        .show_value(false)
                        .clamping(egui::SliderClamping::Always));
                    if resp.double_clicked() { c = 0.0; }
                    ui.add(egui::DragValue::new(&mut c)
                        .speed(0.01)
                        .range(-1.0_f32..=1.0)
                        .fixed_decimals(2));
                    if (c - self.settings.contrast).abs() > f32::EPSILON {
                        self.settings.contrast = c.clamp(-1.0, 1.0);
                        dirty = true;
                    }
                });
                ui.label(egui::RichText::new(
                    "Adjusts panel/widget background lightness. Negative = darker, positive = lighter. Double-click the slider to reset."
                ).small().color(egui::Color32::from_gray(140)));

                // See-through opacity is set via the popover slider that
                // appears when you hover the eye icon next to the zoom
                // controls — no longer surfaced here to keep Settings
                // focused on durable preferences.

                ui.add_space(10.0);
                ui.separator();
                ui.add_space(6.0);

                // ── Always-on-top pin ──────────────────────────────────
                ui.label(egui::RichText::new("Always-on-top pin").strong());
                ui.add_space(4.0);

                // Shortcut binder (mirrors the panic-mode binder pattern).
                ui.horizontal(|ui| {
                    ui.label("Pin shortcut:");
                    let btn_text = if self.pin_learning {
                        "Press chord…".to_string()
                    } else {
                        self.settings.pin_shortcut.label()
                    };
                    let mut btn = egui::Button::new(egui::RichText::new(btn_text).size(12.0));
                    if self.pin_learning {
                        btn = btn.fill(egui::Color32::from_rgb(80, 60, 30))
                                 .stroke(egui::Stroke::new(1.0, egui::Color32::from_rgb(200, 160, 80)));
                    }
                    let resp = ui.add(btn).on_hover_text(
                        if self.pin_learning {
                            "Press the new shortcut (modifier + key).\nClick again to cancel."
                        } else {
                            "Click to re-bind. Press the shortcut anywhere on the system to toggle the pin."
                        }
                    );
                    if resp.clicked() {
                        self.pin_learning = !self.pin_learning;
                    }
                });
                if self.pin_learning {
                    let pressed: Option<egui::Key> = ctx.input(|i| {
                        i.events.iter().find_map(|e| match e {
                            egui::Event::Key { key, pressed: true, repeat: false, .. } => Some(*key),
                            _ => None,
                        })
                    });
                    if let Some(key) = pressed {
                        let m = ctx.input(|i| i.modifiers);
                        let key_name = format!("{:?}", key);
                        self.settings.pin_shortcut = settings::PinShortcut {
                            ctrl:  m.ctrl,
                            shift: m.shift,
                            alt:   m.alt,
                            win:   m.command && !m.ctrl,
                            key:   Some(key_name),
                        };
                        if let Ok(mut s) = self.pin_shortcut_shared.write() {
                            *s = self.settings.pin_shortcut.clone();
                        }
                        self.pin_learning = false;
                        dirty = true;
                    }
                }

                ui.add_space(4.0);
                if ui.checkbox(&mut self.settings.pin_via_guide,
                    "Also toggle pin with controller Guide / PS / Home button")
                    .on_hover_text(
                        "Watches every connected gamepad for a Guide-button press.\n\
                         Standard XInput on Windows does NOT expose the Guide bit on Xbox \
                         controllers — it works for DualSense via HID, virtual ViGEm pads, \
                         and most non-Microsoft controllers."
                    )
                    .changed()
                {
                    dirty = true;
                }
                ui.add_enabled_ui(self.settings.pin_via_guide, |ui| {
                    if ui.checkbox(&mut self.settings.pin_guide_double_tap,
                        "    Require double-tap")
                        .on_hover_text(
                            "Recommended: dodges collisions with Steam / Game Bar's own \
                             single-press Guide-button handling.\nTwo taps within ~300 ms."
                        )
                        .changed()
                    {
                        dirty = true;
                    }

                    // ── Chord button (AutoMap-style learn) ──────────
                    // Optional additional button that must be held with
                    // Guide for the activation to fire. Click "Learn"
                    // and press any button on the controller to bind.
                    ui.horizontal(|ui| {
                        ui.label("    Chord button:");
                        let learning = self.pin_learn_chord.load(Ordering::Relaxed);
                        let face = if learning {
                            "Press a button…".to_string()
                        } else {
                            self.settings.pin_guide_chord
                                .as_deref()
                                .map(pretty_chord_name)
                                .unwrap_or_else(|| "(none)".to_string())
                        };
                        let mut btn = egui::Button::new(
                            egui::RichText::new(face).size(12.0));
                        if learning {
                            btn = btn.fill(egui::Color32::from_rgb(80, 60, 30))
                                .stroke(egui::Stroke::new(1.0,
                                    egui::Color32::from_rgb(200, 160, 80)));
                        }
                        let resp = ui.add(btn).on_hover_text(
                            "Optional button that must be held WITH the Guide press\n\
                             for the pin to toggle. Useful to dodge Steam / Game Bar.\n\
                             Click to (re)bind; press any controller button to capture.");
                        if resp.clicked() {
                            // Toggle learn mode. Clear any prior result.
                            let new_state = !learning;
                            self.pin_learn_chord.store(new_state, Ordering::Relaxed);
                            if let Ok(mut g) = self.pin_learned_chord.lock() {
                                *g = None;
                            }
                        }
                        // Clear button to drop the chord requirement.
                        if self.settings.pin_guide_chord.is_some()
                            && ui.small_button("✕")
                                .on_hover_text("Clear chord — Guide alone fires")
                                .clicked()
                        {
                            self.settings.pin_guide_chord = None;
                            dirty = true;
                        }
                    });

                    // Consume any newly-learned chord this frame.
                    let learned: Option<String> = self.pin_learned_chord
                        .lock().ok().and_then(|mut g| g.take());
                    if let Some(name) = learned {
                        self.settings.pin_guide_chord = Some(name);
                        dirty = true;
                    }
                });

                ui.add_space(6.0);
                if ui.checkbox(&mut self.settings.focus_flip_flop,
                    "Flip-flop focus on pin toggle")
                    .on_hover_text(
                        "Pin ON: remember the previously-focused window.\n\
                         Pin OFF: return focus to it.\n\
                         Lets you press the shortcut, tweak, press again, and instantly \
                         resume testing the target app."
                    )
                    .changed()
                {
                    dirty = true;
                }


                // Push live state changes to the watcher thread whenever
                // the user toggles the Guide options.
                if let Ok(mut cfg) = self.pin_guide_cfg.write() {
                    cfg.enabled = self.settings.pin_via_guide;
                    cfg.require_double_tap = self.settings.pin_guide_double_tap;
                    cfg.chord_signal = self.settings.pin_guide_chord.clone();
                }

                ui.add_space(10.0);
                ui.separator();
                ui.add_space(6.0);

                // ── Workspace ───────────────────────────────────────────
                ui.label(egui::RichText::new("Workspace").strong());
                ui.add_space(4.0);
                let resp = ui.checkbox(&mut self.settings.keep_workspace,
                    "Keep open tabs on next launch");
                if resp.changed() {
                    dirty = true;
                    if self.settings.keep_workspace {
                        save_workspace = true;
                    } else {
                        settings::delete_workspace();
                    }
                }
                ui.label(egui::RichText::new(
                    "When enabled, the current tabs (including unsaved patches) are restored on the next launch."
                ).small().color(egui::Color32::from_gray(140)));

                ui.add_space(6.0);
                if ui.checkbox(&mut self.settings.device_nodes_default_collapsed,
                    "Collapse new device nodes by default").changed()
                {
                    dirty = true;
                }
                ui.label(egui::RichText::new(
                    "New physical / virtual device nodes spawn collapsed. The header (icon, status, Auto-Map) stays visible."
                ).small().color(egui::Color32::from_gray(140)));

                ui.add_space(6.0);
                if ui.checkbox(
                    &mut self.settings.show_own_virtuals_as_physical,
                    "Show FlexInput's own virtual controllers in the physical-devices panel",
                ).changed() {
                    dirty = true;
                }
                ui.label(egui::RichText::new(
                    "Off by default. Turn on to test patches against your own virtual output (loopback).",
                ).small().color(egui::Color32::from_gray(140)));

                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    ui.label("On patch load:");
                    let cur = self.settings.on_patch_load;
                    let label = match cur {
                        settings::OnPatchLoad::Off         => "Do nothing",
                        settings::OnPatchLoad::Center      => "Center on patch",
                        settings::OnPatchLoad::ZoomToFit   => "Zoom to fit",
                    };
                    egui::ComboBox::from_id_salt("on_patch_load_combo")
                        .selected_text(label)
                        .show_ui(ui, |ui| {
                            let mut sel = cur;
                            ui.selectable_value(&mut sel, settings::OnPatchLoad::Off,
                                "Do nothing");
                            ui.selectable_value(&mut sel, settings::OnPatchLoad::Center,
                                "Center on patch");
                            ui.selectable_value(&mut sel, settings::OnPatchLoad::ZoomToFit,
                                "Zoom to fit");
                            if sel != cur {
                                self.settings.on_patch_load = sel;
                                dirty = true;
                            }
                        });
                });
                ui.label(egui::RichText::new(
                    "Behavior when a .fxp is loaded into a tab. 'Off' preserves the current view."
                ).small().color(egui::Color32::from_gray(140)));

                ui.add_space(10.0);
                ui.separator();
                ui.add_space(6.0);

                // ── Device defaults ─────────────────────────────────────
                // Per-device sliders in the node header are seeded from these
                // when a node is first added. Editing them later updates only
                // the affected node, not these defaults.
                ui.label(egui::RichText::new("Device defaults").strong());
                ui.add_space(4.0);
                ui.label(egui::RichText::new(
                    "Applied to newly-added device nodes. Existing nodes keep their own values."
                ).small().color(egui::Color32::from_gray(140)));
                ui.add_space(4.0);

                ui.label(egui::RichText::new("Double-click the slider track to reset to factory default. Double-click the value to type a number.")
                    .small().italics().color(egui::Color32::from_gray(120)));
                ui.add_space(4.0);

                // egui Slider widgets only sense drag, so Response::double_clicked()
                // never fires on the track. Overlay a click-sense interact on the
                // same rect with a derived id and read the double-click from there.
                fn track_dbl(ui: &egui::Ui, r: &egui::Response) -> bool {
                    let id = r.id.with("__dblclick_overlay");
                    ui.interact(r.rect, id, egui::Sense::click()).double_clicked()
                }

                egui::Grid::new("device-defaults-grid")
                    .num_columns(3)
                    .spacing([10.0, 6.0])
                    .show(ui, |ui| {
                        ui.label("Default Stick deadzone");
                        let r = ui.add(egui::Slider::new(&mut self.settings.default_stick_deadzone, 0.0_f32..=0.5)
                            .show_value(false)
                            .clamping(egui::SliderClamping::Always));
                        if r.changed() { dirty = true; }
                        if track_dbl(ui, &r) {
                            self.settings.default_stick_deadzone = 0.1;
                            dirty = true;
                        }
                        if ui.add(egui::DragValue::new(&mut self.settings.default_stick_deadzone)
                            .speed(0.005)
                            .range(0.0_f32..=0.5)
                            .fixed_decimals(2))
                            .changed() { dirty = true; }
                        ui.end_row();

                        ui.label("Default Gyro ×");
                        let r = ui.add(egui::Slider::new(&mut self.settings.default_gyro_mult, 0.1_f32..=50.0)
                            .logarithmic(true)
                            .show_value(false)
                            .clamping(egui::SliderClamping::Always));
                        if r.changed() { dirty = true; }
                        if track_dbl(ui, &r) {
                            self.settings.default_gyro_mult = 1.0;
                            dirty = true;
                        }
                        if ui.add(egui::DragValue::new(&mut self.settings.default_gyro_mult)
                            .speed(0.05)
                            .range(0.1_f32..=50.0)
                            .fixed_decimals(2))
                            .changed() { dirty = true; }
                        ui.end_row();

                        ui.label("Default Mouse ×");
                        let r = ui.add(egui::Slider::new(&mut self.settings.default_mouse_sensitivity, 0.0_f32..=3000.0)
                            .logarithmic(true)
                            .smallest_positive(0.01)
                            .show_value(false)
                            .clamping(egui::SliderClamping::Always));
                        if r.changed() { dirty = true; }
                        if track_dbl(ui, &r) {
                            self.settings.default_mouse_sensitivity = 1.0;
                            dirty = true;
                        }
                        if ui.add(egui::DragValue::new(&mut self.settings.default_mouse_sensitivity)
                            .speed(0.5)
                            .range(0.0_f32..=3000.0)
                            .fixed_decimals(2))
                            .changed() { dirty = true; }
                        ui.end_row();
                    });

                ui.add_space(10.0);
                ui.separator();
                ui.add_space(6.0);

                // ── Profiler (dev tool) ─────────────────────────────────
                // Toggle here flips `puffin::set_scopes_on()` and starts/
                // stops a `puffin_http` server on 127.0.0.1:8585. Connect
                // from the standalone `puffin_viewer` GUI to see a live
                // flamegraph of FlexInput's threads. Not persisted —
                // resets to off on every launch.
                ui.label(egui::RichText::new("Profiler").strong());
                ui.add_space(4.0);
                let mut prof = self.settings.profiler_enabled;
                if ui.checkbox(&mut prof, "Enable puffin profiler (127.0.0.1:8585)").changed() {
                    self.settings.profiler_enabled = prof;
                    if prof {
                        match puffin_http::Server::new("127.0.0.1:8585") {
                            Ok(server) => {
                                puffin::set_scopes_on(true);
                                self.profiler_server = Some(server);
                                eprintln!("[profiler] listening on 127.0.0.1:8585 — connect with `puffin_viewer --url 127.0.0.1:8585`");
                            }
                            Err(e) => {
                                eprintln!("[profiler] failed to start server: {e}");
                                self.settings.profiler_enabled = false;
                            }
                        }
                    } else {
                        puffin::set_scopes_on(false);
                        self.profiler_server = None;
                        eprintln!("[profiler] stopped");
                    }
                }
                ui.label(egui::RichText::new(
                    "Install once: `cargo install puffin_viewer`.\n\
                     Then run `puffin_viewer --url 127.0.0.1:8585` with this toggle on."
                ).small().weak());

                ui.add_space(10.0);
                ui.separator();
                ui.add_space(6.0);

                // ── Links ───────────────────────────────────────────────
                ui.label(egui::RichText::new("Links").strong());
                ui.add_space(4.0);
                ui.hyperlink_to("FlexInput repository",      "https://github.com/x-iso/FlexInput");
                ui.hyperlink_to("ViGEm Bus — latest release","https://github.com/nefarius/ViGEmBus/releases/latest");
                ui.hyperlink_to("HidHide — latest release",  "https://github.com/nefarius/HidHide/releases/latest");

                ui.add_space(10.0);
                ui.separator();
                ui.add_space(6.0);

                // ── Credits ─────────────────────────────────────────────
                ui.label(egui::RichText::new("Credits").strong());
                ui.add_space(4.0);
                ui.label(egui::RichText::new(
                    "Built with egui, eframe, egui-snarl, egui_extras, gilrs, midir, rfd, serde, ViGEmBus, HidHide."
                ).small());
                ui.horizontal_wrapped(|ui| {
                    ui.label(egui::RichText::new("Input prompt SVG icons by Kenney —").small());
                    ui.hyperlink_to(
                        egui::RichText::new("kenney.nl/assets/input-prompts").small(),
                        "https://kenney.nl/assets/input-prompts",
                    );
                    ui.label(egui::RichText::new("(CC0).").small());
                });
                }); // ScrollArea
            });

        if dirty { self.settings_dirty = true; }
        if save_workspace { self.save_workspace_now(); }
        if !open { self.settings_open = false; }
    }

    /// Serialize the current tab set to workspace.json. No-op if the user
    /// has not opted in to workspace persistence.
    fn save_workspace_now(&self) {
        if !self.settings.keep_workspace { return; }
        let tabs: Vec<PersistedTab> = self.tabs.iter().map(|t| PersistedTab {
            title: t.title.clone(),
            file_path: t.file_path.clone(),
            bound_exes: t.bound_exes.clone(),
            auto_bypass: t.auto_bypass,
            snarl: t.canvas.snarl.clone(),
        }).collect();
        let ws = PersistedWorkspace {
            version: 1,
            active_tab: self.active_tab,
            tabs,
        };
        settings::save_workspace(&ws);
    }
}

// ── 500 Hz device I/O thread ──────────────────────────────────────────────────

fn spawn_io_thread(
    mut backends: Vec<Box<dyn DeviceBackend>>,
    midi: Arc<Mutex<Option<MidiBackend>>>,
    proc_device_signals: flexinput_engine::ArcSignals,
    sink_bus: SinkBus,
    // App-level shared pool of virtual output devices. Membership is
    // managed by the UI thread (reconcile on patch load, prune on tab
    // close); the I/O thread only reads it.
    shared_virtual_devices: SharedDevicePool,
    // IDs referenced by the active tab's canvas. Devices in the pool
    // whose id is NOT in this set are silenced (`reset_outputs()`)
    // every tick — background tabs don't drive output.
    active_tab_device_ids: Arc<RwLock<HashSet<String>>>,
    io_bypass: Arc<AtomicBool>,
    shared_devices: Arc<RwLock<Vec<PhysicalDevice>>>,
    shared_midi_devices: Arc<RwLock<Vec<PhysicalDevice>>>,
    polling_hz: Arc<AtomicU32>,
    device_rates: flexinput_engine::DeviceRates,
    scope_taps: flexinput_engine::ScopeTaps,
) {
    use std::time::{Duration, Instant};

    std::thread::Builder::new()
        .name("device-io".into())
        .spawn(move || {
            // Bump the Windows system timer resolution to 1 ms so
            // `thread::sleep(Duration::from_millis(1))` actually sleeps
            // ~1 ms instead of the default ~15.6 ms. Without this, the
            // requested polling rate is capped at ~64 Hz regardless of
            // setting. Process-wide effect; matches what game-input
            // libraries do internally.
            #[cfg(windows)]
            unsafe {
                let r = windows_sys::Win32::Media::timeBeginPeriod(1);
                eprintln!("[device-io] timeBeginPeriod(1) -> {} (0 == TIMERR_NOERROR)", r);
            }
            let mut last_enum = Instant::now() - Duration::from_secs(10);
            let mut last_midi_out: HashMap<(String, String), Signal> = HashMap::new();
            // Measured I/O rate EMA. Updated each iteration; published via
            // `flexinput_engine::set_io_rate` so the UI can show the actual
            // poll rate (separate from the engine's sample rate).
            let mut last_loop_t = Instant::now();
            let mut measured_hz_ema: f32 = 0.0;
            // Per-device event accumulator. We sample the device backends'
            // raw event counts and convert to Hz on a fixed 500 ms cadence
            // (smooths short-term spikes while still feeling "live").
            let mut dev_event_acc: HashMap<String, u32> = HashMap::new();
            let mut dev_rate_ema: HashMap<String, f32> = HashMap::new();
            let mut last_rate_publish = Instant::now();

            loop {
                puffin::GlobalProfiler::lock().new_frame();
                puffin::profile_scope!("io_thread_iter");
                let t0 = Instant::now();
                // Re-read polling rate each iteration so live retunes apply.
                let hz = polling_hz.load(Ordering::Relaxed).clamp(60, 4000);
                let interval = Duration::from_nanos(1_000_000_000 / hz as u64);

                // ── Poll physical inputs ──────────────────────────────────────
                let mut signals: HashMap<(String, String), Signal> = HashMap::new();
                {
                    puffin::profile_scope!("backends_poll");
                    for backend in &mut backends {
                        for (dev, pin, sig) in backend.poll() {
                            signals.insert((dev, pin), sig);
                        }
                        for (dev, n) in backend.take_event_counts() {
                            *dev_event_acc.entry(dev).or_insert(0) += n;
                        }
                    }
                }
                {
                    puffin::profile_scope!("midi_poll");
                    if let Ok(mut mg) = midi.try_lock() {
                        if let Some(m) = mg.as_mut() {
                            for (dev, pin, sig) in m.poll() {
                                signals.insert((dev, pin), sig);
                            }
                            for (dev, n) in m.take_event_counts() {
                                *dev_event_acc.entry(dev).or_insert(0) += n;
                            }
                        }
                    }
                }
                // Tap gyro/accel samples into the per-pin scope rings so the
                // calibration window can render at true polling Hz rather than
                // UI repaint Hz. We do this BEFORE moving `signals` into the
                // shared map.
                {
                    puffin::profile_scope!("scope_taps_write");
                    let now = Instant::now();
                    let mut taps = scope_taps.write().unwrap();
                    let retain = Duration::from_millis(flexinput_engine::SCOPE_TAP_RETAIN_MS);
                    for ((dev, pin), sig) in &signals {
                        if !flexinput_engine::SCOPE_TAP_PINS.iter().any(|p| *p == pin.as_str()) {
                            continue;
                        }
                        let v = match sig {
                            Signal::Float(f) => *f,
                            Signal::Bool(b)  => if *b { 1.0 } else { 0.0 },
                            _ => continue,
                        };
                        let ring = taps.entry((dev.clone(), pin.clone()))
                            .or_insert_with(flexinput_engine::ScopeTapRing::new);
                        ring.push_back((now, v));
                        while let Some(&(t, _)) = ring.front() {
                            if now.duration_since(t) > retain
                                || ring.len() > flexinput_engine::SCOPE_TAP_MAX_LEN
                            {
                                ring.pop_front();
                            } else {
                                break;
                            }
                        }
                    }
                }

                {
                    puffin::profile_scope!("publish_signals");
                    // ArcSwap publish — consumers (proc thread, UI) read
                    // via `load_full()`, a refcount bump rather than a
                    // map clone under a RwLock.
                    proc_device_signals.store(std::sync::Arc::new(signals));
                }

                // ── Enumerate gilrs devices periodically ──────────────────────
                // MIDI enumeration is handled by spawn_midi_watch_thread() so
                // the slow Win32 MIDI calls (60–70 ms with loopMIDI loaded)
                // don't stall this 500 Hz I/O loop.
                if last_enum.elapsed() > Duration::from_secs(2) {
                    puffin::profile_scope!("enumerate_devices");
                    let mut devs: Vec<PhysicalDevice> = Vec::new();
                    for backend in &mut backends {
                        devs.extend(backend.enumerate());
                    }
                    // Append MIDI device list maintained by the MIDI watch thread.
                    devs.extend(shared_midi_devices.read().unwrap().iter().cloned());
                    *shared_devices.write().unwrap() = devs;
                    last_enum = Instant::now();
                }

                // ── Get latest sink outputs from processing thread ─────────────
                // Uses a separate RwLock so this read never contends on proc_outputs.
                let sink_outputs: HashMap<(String, String), Signal> = {
                    puffin::profile_scope!("read_sink_bus");
                    sink_bus.read().unwrap().clone()
                };

                // ── Drive virtual & physical devices ──────────────────────────
                // Shared pool holds ALL virtual devices across every open
                // tab. The active-tab id filter decides which devices
                // actually route signals this tick; devices outside the
                // filter receive `reset_outputs()` so a background tab's
                // device idles instead of holding its last state.
                let bypass = io_bypass.load(Ordering::Relaxed);
                let active_ids = active_tab_device_ids.read().unwrap().clone();
                {
                    puffin::profile_scope!("route_virtual_devices");
                    let mut devs = shared_virtual_devices.lock().unwrap();
                    if bypass {
                        for dev in devs.iter_mut() { dev.reset_outputs(); }
                    } else {
                        // Silence devices not referenced by the active tab.
                        for dev in devs.iter_mut() {
                            if !active_ids.contains(dev.id()) {
                                dev.reset_outputs();
                            }
                        }
                        // Route signals to active-tab devices only.
                        for ((device_id, pin_id), &signal) in &sink_outputs {
                            if !active_ids.contains(device_id) { continue; }
                            if let Some(dev) = devs.iter_mut().find(|d| d.id() == device_id) {
                                dev.send(pin_id, signal);
                            }
                        }
                        // Flush every device (silenced ones still need a
                        // flush to commit their zeroed state to the OS).
                        for dev in devs.iter_mut() { dev.flush(); }

                        // Poll rumble/feedback signals back only from active-tab
                        // devices — background-tab feedback would route into the
                        // wrong graph.
                        let mut virt_sigs: Vec<((String, String), Signal)> = Vec::new();
                        for dev in devs.iter_mut() {
                            let id = dev.id().to_string();
                            if !active_ids.contains(&id) { continue; }
                            for (pin_id, sig) in dev.poll_outputs() {
                                virt_sigs.push(((id.clone(), pin_id.to_string()), sig));
                            }
                        }
                        if !virt_sigs.is_empty() {
                            // ArcSwap is publish-only — to merge into the
                            // currently-published map we load it, clone
                            // into an owned mutable copy, apply the merge,
                            // and store the result. Cost = one map clone
                            // (was already paid before by the RwLock
                            // write path, which serialized vs readers).
                            let cur = proc_device_signals.load_full();
                            let mut merged: HashMap<(String, String), Signal> = (*cur).clone();
                            for (k, v) in virt_sigs { merged.insert(k, v); }
                            proc_device_signals.store(std::sync::Arc::new(merged));
                        }
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

                // Measured per-loop Hz via inter-iteration interval. EMA
                // smooths it so the UI label doesn't strobe at frame rate.
                let now = Instant::now();
                let dt = now.duration_since(last_loop_t).as_secs_f32().max(1e-4);
                last_loop_t = now;
                let inst_hz = 1.0 / dt;
                // ~1 s time constant at 500 Hz loop = alpha ≈ 0.002
                let alpha = 0.02_f32;
                if measured_hz_ema == 0.0 {
                    measured_hz_ema = inst_hz;
                } else {
                    measured_hz_ema = measured_hz_ema * (1.0 - alpha) + inst_hz * alpha;
                }
                flexinput_engine::set_io_rate(measured_hz_ema.round() as u32);

                // Publish per-device rates every ~150 ms. Hz is computed from
                // raw event counts accumulated since the last publish, EMA-
                // smoothed across publishes for stability.
                let rate_dt = last_rate_publish.elapsed();
                if rate_dt >= Duration::from_millis(150) {
                    let rate_dt_s = rate_dt.as_secs_f32().max(1e-3);
                    // Compute new per-device instantaneous Hz, lerp into EMA.
                    let alpha = 0.6_f32;
                    let seen_devs: Vec<String> = dev_event_acc.keys().cloned().collect();
                    for dev in &seen_devs {
                        let count = dev_event_acc.get(dev).copied().unwrap_or(0) as f32;
                        let inst = count / rate_dt_s;
                        let prev = dev_rate_ema.get(dev).copied().unwrap_or(0.0);
                        let new = prev * (1.0 - alpha) + inst * alpha;
                        dev_rate_ema.insert(dev.clone(), new);
                    }
                    // Devices without recent events decay toward zero.
                    let known: Vec<String> = dev_rate_ema.keys().cloned().collect();
                    for dev in known {
                        if !dev_event_acc.contains_key(&dev) {
                            let prev = dev_rate_ema.get(&dev).copied().unwrap_or(0.0);
                            let new = prev * (1.0 - alpha);
                            if new < 0.5 {
                                dev_rate_ema.remove(&dev);
                            } else {
                                dev_rate_ema.insert(dev, new);
                            }
                        }
                    }
                    dev_event_acc.clear();
                    last_rate_publish = Instant::now();

                    // Publish to shared map.
                    if let Ok(mut rates) = device_rates.write() {
                        rates.clear();
                        for (dev, hz) in &dev_rate_ema {
                            rates.insert(dev.clone(), hz.round() as u32);
                        }
                    }
                }
            }
        })
        .expect("failed to spawn device I/O thread");
}

// ── MIDI watch thread ────────────────────────────────────────────────────────
//
// Runs the (slow, Windows-blocking) MIDI port enumeration off the 500 Hz I/O
// loop so it never stalls device polling. Cycle every 2 s:
//
//  1. Read pinned_midi_ids (set of midi_in:N / midi_out:N the canvas uses).
//  2. Lock MidiBackend briefly to drop any open OS handles that aren't
//     pinned — this lets the Windows MIDI subsystem report removed ports as
//     gone (otherwise an open handle keeps loopMIDI ports alive even after
//     the user deletes them in loopMIDI's UI).
//  3. Without the lock, call list_live_ports() — the slow Win32 call.
//  4. Lock MidiBackend again briefly to apply the diff (open connections for
//     pinned ports that came back, drop entries for vanished ports) and
//     rebuild the shared MIDI device list for the UI panel.
fn spawn_midi_watch_thread(
    midi: Arc<Mutex<Option<MidiBackend>>>,
    pinned_midi_ids: Arc<RwLock<HashSet<String>>>,
    shared_midi_devices: Arc<RwLock<Vec<PhysicalDevice>>>,
) {
    use std::time::Duration;
    std::thread::Builder::new()
        .name("midi-watch".into())
        .spawn(move || {
            loop {
                std::thread::sleep(Duration::from_secs(2));

                // 1+2: release non-canvas-pinned handles so the OS can free them.
                {
                    let pinned = pinned_midi_ids.read().unwrap().clone();
                    if let Ok(mut mg) = midi.lock() {
                        if let Some(m) = mg.as_mut() {
                            m.release_unpinned(&pinned);
                        }
                    }
                }

                // 3: slow enum without the lock.
                let (live_in, live_out) = MidiBackend::list_live_ports();

                // 4: apply diff + publish device list for the UI.
                if let Ok(mut mg) = midi.lock() {
                    if let Some(m) = mg.as_mut() {
                        m.apply_port_diff(&live_in, &live_out);
                        let devs = m.enumerate();
                        *shared_midi_devices.write().unwrap() = devs;
                    }
                }
            }
        })
        .expect("failed to spawn MIDI watch thread");
}

/// Recreate a virtual device from its saved ID string.
/// Handles both `"virtual.xinput"` (instance 0, no suffix) and `"virtual.xinput.1"` (instance N).
fn try_create_virtual_device(id: &str) -> Option<Box<dyn flexinput_virtual::VirtualDevice>> {
    let (kind_id, instance) = match id.rfind('.') {
        Some(dot) => match id[dot + 1..].parse::<usize>() {
            Ok(n) => (&id[..dot], n),
            Err(_) => (id, 0),  // no numeric suffix → instance 0 (e.g. "virtual.xinput")
        },
        None => (id, 0),
    };
    let known = flexinput_virtual::available_device_kinds()
        .iter()
        .any(|k| k.kind_id == kind_id);
    if !known { return None; }
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

/// A linked-list frame used by `find_automap_device` to track the outer snarl(s)
/// when descending into nested subpatches. An inlet in an inner snarl uses the top
/// frame to pop back to the outer snarl and continue tracing the AutoMap wire chain.
struct AutomapParent<'a> {
    snarl: &'a Snarl<NodeData>,
    /// NodeId of the subpatch node in `snarl` that we descended through.
    subpatch_id: NodeId,
    prev: Option<&'a AutomapParent<'a>>,
}

/// Reconstructs the eval-side `outer_uid` value at the depth of `p` by folding
/// `namespaced_uid` over the subpatch-id chain from root to `p`. Matches the
/// chain that `eval_subgraph` builds as it descends recursively.
fn fold_outer_uid(p: &AutomapParent<'_>) -> usize {
    match p.prev {
        None => p.subpatch_id.0,
        Some(prev) => flexinput_engine::namespaced_uid(fold_outer_uid(prev), p.subpatch_id.0),
    }
}

/// Walks `snarl` and (recursively) any subpatch inner snarls, populating
/// `extra.last_signals` and `extra.history` from the latest eval results.
/// `parent_uid` is `None` at the root, `Some(ns_uid)` when recursing into a
/// subpatch — matching the `outer_uid` that `eval_subgraph` used so inner
/// nodes look up their own samples.
fn apply_display_state(
    snarl: &mut Snarl<NodeData>,
    parent_uid: Option<usize>,
    last_inputs: &HashMap<usize, Vec<Option<Signal>>>,
    last_outputs: &HashMap<usize, Vec<Option<Signal>>>,
    scope_lookup: &HashMap<usize, Vec<Vec<Option<f32>>>>,
) {
    let ids: Vec<NodeId> = snarl.nodes_ids_data().map(|(id, _)| id).collect();
    for id in ids {
        let uid = match parent_uid {
            None => id.0,
            Some(p) => flexinput_engine::namespaced_uid(p, id.0),
        };
        if let Some(node) = snarl.get_node_mut(id) {
            if let Some(sigs) = last_inputs.get(&uid) {
                node.extra.last_signals = sigs.clone();
            }
            if let Some(outs) = last_outputs.get(&uid) {
                node.extra.last_out = outs.clone();
            }
            if let Some(samples) = scope_lookup.get(&uid) {
                let h = &mut node.extra.history;
                for s in samples {
                    if h.len() >= HISTORY_LEN { h.pop_front(); }
                    h.push_back(s.clone());
                }
            }
        }
        // Recurse into subpatch inner snarl.
        let child_uid = uid;
        let is_subpatch = snarl.get_node(id).map(|n| n.module_id == "subpatch").unwrap_or(false);
        if is_subpatch {
            if let Some(node) = snarl.get_node_mut(id) {
                if let Some(sp) = node.subpatch.as_mut() {
                    apply_display_state(&mut sp.snarl, Some(child_uid), last_inputs, last_outputs, scope_lookup);
                }
            }
        }
    }
}

/// Walk an AutoMap wire chain from `src` back to the originating device.source.
/// Returns (source_dev_id, source_pin_ids, fallback_dev_id).
/// - For device.source: (real_dev_id, output_pins, None).
/// - For automap_split: transparent passthrough (result of upstream).
/// - For automap_collect: ("collector:{uid}", canonical_pins, Some(upstream_real_dev_id)).
/// - For subpatch: descends into the inner snarl through the matching outlet.
/// - For subpatch.inlet (only reached during inner traversal): pops back to outer snarl.
fn find_automap_device(snarl: &Snarl<NodeData>, src: OutPinId) -> Option<(String, Vec<String>, Option<String>)> {
    find_automap_device_rec(snarl, src, None)
}

fn find_automap_device_rec(
    snarl: &Snarl<NodeData>,
    src: OutPinId,
    parents: Option<&AutomapParent<'_>>,
) -> Option<(String, Vec<String>, Option<String>)> {
    let node = snarl.get_node(src.node)?;
    if node.module_id == "device.source" {
        let dev_id = node.params.get("device_id")?.as_str()?.to_string();
        let pin_ids: Vec<String> = node.params.get("output_pin_ids")?.as_array()?
            .iter().map(|v| v.as_str().unwrap_or("").to_string()).collect();
        return Some((dev_id, pin_ids, None));
    }
    if node.module_id == "module.automap_split" {
        let am_idx = node.inputs.iter().position(|p| p.signal_type == SignalType::AutoMap)?;
        let in_pin = snarl.in_pin(InPinId { node: src.node, input: am_idx });
        let upstream = *in_pin.remotes.first()?;
        return find_automap_device_rec(snarl, upstream, parents);
    }
    // Fork and Selector act as gating collectors: they inject signals into collector_sigs
    // only on the active output/input, so non-active paths produce silence.
    // No fallback device — the collector key alone controls what the sink sees.
    if node.module_id == "module.automap_fork"
        || node.module_id == "module.automap_selector"
    {
        let node_uid = match parents {
            None => src.node.0,
            Some(p) => flexinput_engine::namespaced_uid(fold_outer_uid(p), src.node.0),
        };
        // Encode which output pin the sink is downstream of so eval can gate per-output.
        let collector_id = format!("forksel:{}:{}", node_uid, src.output);
        let canonical_pins: Vec<String> = flexinput_core::automap::ALL_PINS
            .iter().map(|p| p.id.to_string()).collect();
        return Some((collector_id, canonical_pins, None));
    }
    if node.module_id == "module.automap_collect" {
        let upstream_dev_id = node.inputs.iter()
            .position(|p| p.signal_type == SignalType::AutoMap)
            .and_then(|am_idx| {
                let in_pin = snarl.in_pin(InPinId { node: src.node, input: am_idx });
                in_pin.remotes.first().copied()
            })
            .and_then(|s| find_automap_device_rec(snarl, s, parents).map(|(id, _, _)| id));
        // The collector ID must match the key the eval thread uses when injecting
        // signals: root-level collectors use NodeId.0, subpatch-nested collectors
        // use namespaced_uid folded through the parent chain.
        let collector_uid = match parents {
            None => src.node.0,
            Some(p) => flexinput_engine::namespaced_uid(fold_outer_uid(p), src.node.0),
        };
        let collector_id = format!("collector:{}", collector_uid);
        let canonical_pins: Vec<String> = flexinput_core::automap::ALL_PINS
            .iter().map(|p| p.id.to_string()).collect();
        return Some((collector_id, canonical_pins, upstream_dev_id));
    }
    if node.module_id == "module.automap_combiner" {
        // Combiner is a virtual bus: per-pin priority merge of its N AutoMap
        // inputs, written into collector_sigs under "combiner:{uid}". Downstream
        // consumers read it the same way they read any other collector.
        let combiner_uid = match parents {
            None => src.node.0,
            Some(p) => flexinput_engine::namespaced_uid(fold_outer_uid(p), src.node.0),
        };
        let combiner_id = format!("combiner:{}", combiner_uid);
        let canonical_pins: Vec<String> = flexinput_core::automap::ALL_PINS
            .iter().map(|p| p.id.to_string()).collect();
        // Use the first connected input's underlying physical device as the
        // fallback so haptic-feedback reverse-routing has something to bind
        // (matches Collector's behaviour).
        let upstream_dev_id = (0..node.inputs.len())
            .find_map(|i| {
                if node.inputs[i].signal_type != SignalType::AutoMap { return None; }
                let in_pin = snarl.in_pin(InPinId { node: src.node, input: i });
                let &s = in_pin.remotes.first()?;
                find_automap_device_rec(snarl, s, parents).map(|(id, _, fallback)| {
                    fallback.unwrap_or(id)
                })
            });
        return Some((combiner_id, canonical_pins, upstream_dev_id));
    }
    if node.module_id == "module.remapper" {
        // Acts as a collector: publishes per-pin signals (pass-through + mapping
        // overrides) into collector_sigs under a `remap:{uid}` key. Downstream
        // sinks find these the same way they find collector / forksel signals.
        let upstream_dev_id = node.inputs.iter()
            .position(|p| p.signal_type == SignalType::AutoMap)
            .and_then(|am_idx| {
                let in_pin = snarl.in_pin(InPinId { node: src.node, input: am_idx });
                in_pin.remotes.first().copied()
            })
            .and_then(|s| find_automap_device_rec(snarl, s, parents).map(|(id, _, _)| id));
        let remap_uid = match parents {
            None => src.node.0,
            Some(p) => flexinput_engine::namespaced_uid(fold_outer_uid(p), src.node.0),
        };
        let remap_id = format!("remap:{}", remap_uid);
        let canonical_pins: Vec<String> = flexinput_core::automap::ALL_PINS
            .iter().map(|p| p.id.to_string()).collect();
        return Some((remap_id, canonical_pins, upstream_dev_id));
    }
    if node.module_id == "subpatch" {
        // Wire enters the subpatch via output pin `src.output`. Find the outlet
        // inside whose pin_index matches, and continue tracing from its input.
        let sp = node.subpatch.as_ref()?;
        let outlet_id: NodeId = sp.snarl.nodes_ids_data()
            .find(|(_, n)| n.value.module_id == "subpatch.outlet"
                && n.value.params.get("pin_index").and_then(|v| v.as_u64())
                    == Some(src.output as u64))
            .map(|(id, _)| id)?;
        let outlet_in = sp.snarl.in_pin(InPinId { node: outlet_id, input: 0 });
        let inner_upstream = *outlet_in.remotes.first()?;
        let frame = AutomapParent { snarl, subpatch_id: src.node, prev: parents };
        return find_automap_device_rec(&sp.snarl, inner_upstream, Some(&frame));
    }
    if node.module_id == "subpatch.inlet" {
        // Inner trace reached an inlet — pop back to the outer snarl and follow
        // the outer subpatch's matching input pin upstream.
        let pin_idx = node.params.get("pin_index").and_then(|v| v.as_u64())? as usize;
        let parent = parents?;
        let outer_in = parent.snarl.in_pin(InPinId { node: parent.subpatch_id, input: pin_idx });
        let upstream = *outer_in.remotes.first()?;
        return find_automap_device_rec(parent.snarl, upstream, parent.prev);
    }
    None
}

/// Builds a topologically-sorted [`ProcessingGraph`] from the current Snarl state.
/// Also returns the UIDs of any counter nodes whose reset was just requested
/// (caller must clear the `aux_f32_dirty` flag on those nodes after writing the snapshot).
fn build_processing_graph(snarl: &Snarl<NodeData>) -> (ProcessingGraph, Vec<usize>) {
    build_processing_graph_rec(snarl, None)
}

fn build_processing_graph_rec(
    snarl: &Snarl<NodeData>,
    parents: Option<&AutomapParent<'_>>,
) -> (ProcessingGraph, Vec<usize>) {
    use std::collections::{HashSet, VecDeque};
    use flexinput_engine::graph::{InlineSubgraph, SinkTarget};

    // Collect ALL nodes (including device.sink — they're evaluated last).
    let node_list: Vec<(NodeId, &NodeData)> = snarl.nodes_ids_data()
        .map(|(id, n)| (id, &n.value))
        .collect();

    let id_to_orig: HashMap<NodeId, usize> = node_list.iter()
        .enumerate()
        .map(|(i, (id, _))| (*id, i))
        .collect();

    let mut dirty_uids: Vec<usize> = Vec::new();

    // Pre-pass: collect, for each physical device_id used as an AutoMap source,
    // the list of virtual sink device_ids that auto-map from it. Used to wire
    // feedback signals (rumble, lightbar) backward along AutoMap connections.
    let mut feedback_map: HashMap<String, Vec<String>> = HashMap::new();
    for (node_id, node) in &node_list {
        let is_sink = node.module_id == "device.sink"
            || (node.module_id == "device.source" && !node.inputs.is_empty());
        if !is_sink { continue; }
        // Find this sink's AutoMap source device_id (if wired).
        let automap_src_dev = (0..node.inputs.len()).find_map(|i| {
            if node.inputs.get(i).map(|p| p.signal_type) != Some(SignalType::AutoMap) {
                return None;
            }
            let pin = snarl.in_pin(InPinId { node: *node_id, input: i });
            let &src = pin.remotes.first()?;
            find_automap_device_rec(snarl, src, parents).map(|(d, _, _)| d)
        });
        let Some(src_dev) = automap_src_dev else { continue; };
        let sink_dev = node.params.get("device_id").and_then(|v| v.as_str()).unwrap_or("");
        // Only track virtual sinks (their feedback flows back to physical sources).
        if sink_dev.starts_with("virtual.") {
            feedback_map.entry(src_dev).or_default().push(sink_dev.to_string());
        }
    }

    let mut snaps: Vec<NodeSnap> = node_list.iter().map(|(node_id, node)| {
        let is_sink = node.module_id == "device.sink"
            || (node.module_id == "device.source" && !node.inputs.is_empty());

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

            // AutoMap: trace the AutoMap wire chain to find the originating device.source.
            let automap_result = (0..node.inputs.len()).find_map(|i| {
                if node.inputs.get(i).map(|p| p.signal_type) != Some(SignalType::AutoMap) {
                    return None;
                }
                let pin = snarl.in_pin(InPinId { node: *node_id, input: i });
                let &src = pin.remotes.first()?;
                find_automap_device_rec(snarl, src, parents)
            });
            let (automap_source, automap_fallback_dev) = match automap_result {
                Some((dev_id, pins, fallback)) => (Some((dev_id, pins)), fallback),
                None => (None, None),
            };

            // Feedback sources: virtual sinks that auto-map FROM this physical device.
            // Their output signals (rumble, lightbar) flow back to this sink's haptic inputs.
            let feedback_sources = feedback_map.get(&sink_dev_id).cloned().unwrap_or_default();

            Some(SinkTarget { device_id: sink_dev_id, pin_ids, multi_sources, automap_source, automap_fallback_dev, feedback_sources, is_self_sink: false })
        } else {
            None
        };

        // For modules that read device signals by name, inject the originating device_id.
        let mut params = node.params.clone();
        if matches!(node.module_id.as_str(),
            "processing.gyro_3dof" | "module.automap_split"
            | "module.automap_fork" | "module.automap_selector"
            | "module.remapper" | "module.map_action")
        {
            let automap_idx = node.inputs.iter().position(|p| p.signal_type == SignalType::AutoMap);
            if let Some(idx) = automap_idx {
                let pin = snarl.in_pin(InPinId { node: *node_id, input: idx });
                if let Some(&src) = pin.remotes.first() {
                    if let Some((dev_id, _, fallback)) = find_automap_device_rec(snarl, src, parents) {
                        // _automap_device_id = real physical device (fallback when upstream is a collector/forksel).
                        let real_id = fallback.unwrap_or_else(|| dev_id.clone());
                        params.insert("_automap_device_id".to_string(), serde_json::Value::String(real_id));
                        // _automap_collector_id = virtual collector key to read from collector_sigs first.
                        // Covers automap_collect ("collector:"), fork/selector ("forksel:"),
                        // combiner ("combiner:"), and remapper ("remap:").
                        if dev_id.starts_with("collector:")
                            || dev_id.starts_with("forksel:")
                            || dev_id.starts_with("combiner:")
                            || dev_id.starts_with("remap:")
                        {
                            params.insert("_automap_collector_id".to_string(),
                                serde_json::Value::String(dev_id));
                        }
                    }
                }
            }
            // Selector: also inject device_id for each additional AutoMap input (in_1, in_2, ...).
            if node.module_id == "module.automap_selector" {
                let mut extra_devs: Vec<serde_json::Value> = Vec::new();
                for i in 1..node.inputs.len() {
                    let pin = snarl.in_pin(InPinId { node: *node_id, input: i });
                    let dev_str = pin.remotes.first()
                        .and_then(|&src| find_automap_device_rec(snarl, src, parents))
                        .map(|(dev_id, _, fallback)| fallback.unwrap_or(dev_id))
                        .unwrap_or_default();
                    extra_devs.push(serde_json::Value::String(dev_str));
                }
                params.insert("_automap_input_devs".to_string(), serde_json::Value::Array(extra_devs));
            }
        }
        // Combiner: all inputs are equal AutoMap buses (no select pin). Record
        // dev_id AND collector_id for each port so eval can read collector
        // overrides (Remapper / Collector / Selector / Fork) before falling
        // back to raw device samples. Parallel arrays indexed by port.
        if node.module_id == "module.automap_combiner" {
            let mut devs: Vec<serde_json::Value> = Vec::new();
            let mut collectors: Vec<serde_json::Value> = Vec::new();
            for i in 0..node.inputs.len() {
                let pin = snarl.in_pin(InPinId { node: *node_id, input: i });
                let resolved = pin.remotes.first()
                    .and_then(|&src| find_automap_device_rec(snarl, src, parents));
                let (dev_str, coll_str) = match resolved {
                    Some((dev_id, _, fallback)) => {
                        let is_collector = dev_id.starts_with("collector:")
                            || dev_id.starts_with("forksel:")
                            || dev_id.starts_with("remap:")
                            || dev_id.starts_with("combiner:");
                        let dev = fallback.unwrap_or_else(|| if is_collector { String::new() } else { dev_id.clone() });
                        let coll = if is_collector { dev_id } else { String::new() };
                        (dev, coll)
                    }
                    None => (String::new(), String::new()),
                };
                devs.push(serde_json::Value::String(dev_str));
                collectors.push(serde_json::Value::String(coll_str));
            }
            params.insert("_automap_input_devs".to_string(), serde_json::Value::Array(devs));
            params.insert("_automap_input_collectors".to_string(), serde_json::Value::Array(collectors));
        }
        // For automap_collect: forward the stable pin-ID list so eval.rs can key collector_sigs.
        // IDs are stored separately in collect_input_pin_ids (parallel to inputs[1..]).
        if node.module_id == "module.automap_collect" {
            let collect_ids = node.params.get("collect_input_pin_ids")
                .and_then(|v| v.as_array()).cloned().unwrap_or_default();
            params.insert("_collect_pin_ids".to_string(), serde_json::Value::Array(collect_ids));
        }

        // For subpatch nodes: recursively build the inner graph and locate outlet nodes.
        // The inner build receives a parent frame so any AutoMap traces from inner
        // Splitter / Collector nodes can pop back out through the inlets.
        let inline_subgraph = if node.module_id == "subpatch" {
            node.subpatch.as_ref().map(|sp| {
                let inner_frame = AutomapParent { snarl, subpatch_id: *node_id, prev: parents };
                let (inner_graph, _) = build_processing_graph_rec(&sp.snarl, Some(&inner_frame));
                let n_out = sp.pins_out.len();
                let mut outlet_locs: Vec<Option<(usize, usize)>> = vec![None; n_out];
                for (flat_idx, inner_snap) in inner_graph.nodes.iter().enumerate() {
                    if inner_snap.module_id == "subpatch.outlet" {
                        let pin_idx = inner_snap.params.get("pin_index")
                            .and_then(|v| v.as_u64()).unwrap_or(0) as usize;
                        if pin_idx < n_out {
                            outlet_locs[pin_idx] = Some((flat_idx, 0));
                        }
                    }
                }
                Box::new(InlineSubgraph { graph: inner_graph, outlet_locs })
            })
        } else {
            None
        };

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
            inline_subgraph,
        }
    }).collect();

    // Topological sort (Kahn's algorithm).
    // Sink nodes are leaves (no node depends on them), so they naturally end up last.
    //
    // A device.source node with feedback inputs is both source and sink in one
    // physical node. Its sink-half (multi_sources) can legitimately receive a
    // wire that traces back to its own source-half — directly, or through a
    // Splitter/Math chain. That looks like a graph cycle but isn't a real
    // data-flow cycle: hardware reads happen at frame start (dev_sigs) and
    // hardware writes happen at frame end (sink_outputs), with no within-frame
    // dependency between them. We solve this by suppressing the sink-half's
    // incoming edges in the topo sort (so the source-half sorts early as a
    // pure leaf, releasing downstream consumers in Kahn), and eval runs a
    // second pass over self-sinks' multi_sources after the main loop, by which
    // time every upstream `computed[idx]` slot is filled.
    let n = snaps.len();
    let is_source_self_sink: Vec<bool> = snaps.iter().enumerate().map(|(idx, snap)| {
        if snap.module_id != "device.source" { return false; }
        let Some(ref st) = snap.sink_target else { return false; };
        // Direct self-wire: any multi_source pointing back to this node.
        if st.multi_sources.iter().any(|srcs| srcs.iter().any(|&(s, _)| s == idx)) {
            return true;
        }
        // Indirect self-wire: BFS over input_sources of upstream nodes — does any
        // path through the regular signal graph loop back to this node?
        let mut visited: HashSet<usize> = HashSet::new();
        let mut stack: Vec<usize> = st.multi_sources.iter()
            .flat_map(|srcs| srcs.iter().map(|&(s, _)| s))
            .collect();
        while let Some(cur) = stack.pop() {
            if cur == idx { return true; }
            if !visited.insert(cur) { continue; }
            if let Some(up) = snaps.get(cur) {
                for &(s, _) in up.input_sources.iter().flatten() {
                    stack.push(s);
                }
            }
        }
        false
    }).collect();

    // Propagate the detection back into each SinkTarget so eval can drive its
    // post-pass for these nodes.
    for (i, snap) in snaps.iter_mut().enumerate() {
        if is_source_self_sink[i] {
            if let Some(ref mut st) = snap.sink_target {
                st.is_self_sink = true;
            }
        }
    }

    let mut in_degree = vec![0usize; n];
    let mut dependents: Vec<Vec<usize>> = vec![vec![]; n];
    for (idx, snap) in snaps.iter().enumerate() {
        // Regular nodes: single-source inputs.
        for &(src_idx, _) in snap.input_sources.iter().flatten() {
            dependents[src_idx].push(idx);
            in_degree[idx] += 1;
        }
        // Sink nodes: multi-source inputs (deduplicated per source node to avoid double-counting).
        // Skip for device.source self-sinks: their sink-half is handled in a
        // post-pass during eval, so we don't add the cycle-inducing incoming edges here.
        if !is_source_self_sink[idx] {
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
            // If the AutoMap source is a Collector / Fork / Selector / Combiner / Remapper,
            // add it as a dependency so it is evaluated before this sink (ensuring
            // collector_sigs is populated).
            if let Some((ref am_dev_id, _)) = st.automap_source {
                // "collector:{uid}" → automap_collect node
                // "forksel:{uid}:{out}" → automap_fork or automap_selector node
                // "combiner:{uid}" → automap_combiner node
                // "remap:{uid}" → remapper node
                let uid_str = am_dev_id.strip_prefix("collector:")
                    .or_else(|| am_dev_id.strip_prefix("combiner:"))
                    .or_else(|| am_dev_id.strip_prefix("remap:"))
                    .or_else(|| am_dev_id.strip_prefix("forksel:").and_then(|s| s.split(':').next()));
                if let Some(uid_str) = uid_str {
                    if let Ok(uid) = uid_str.parse::<usize>() {
                        if let Some(am_idx) = snaps.iter().position(|s| s.node_uid == uid) {
                            if seen.insert(am_idx) {
                                dependents[am_idx].push(idx);
                                in_degree[idx] += 1;
                            }
                        }
                    }
                }
            }
            // Also depend on the immediate outer-snarl source of every AutoMap
            // input pin. Catches subpatches whose inner graph contains a Collector
            // (whose namespaced UID isn't in `snaps`) — depending on the outer
            // subpatch node still guarantees its inner eval runs and populates
            // collector_sigs before this sink reads it.
            let (outer_node_id, outer_node) = node_list[idx];
            for i in 0..outer_node.inputs.len() {
                if outer_node.inputs.get(i).map(|p| p.signal_type) != Some(SignalType::AutoMap) {
                    continue;
                }
                let pin = snarl.in_pin(InPinId { node: outer_node_id, input: i });
                if let Some(&src) = pin.remotes.first() {
                    if let Some(&src_idx) = id_to_orig.get(&src.node) {
                        if seen.insert(src_idx) {
                            dependents[src_idx].push(idx);
                            in_degree[idx] += 1;
                        }
                    }
                }
            }
        }
        } // end !is_source_self_sink guard
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
    let text_color  = ui.visuals().text_color();
    let hover_fill  = ui.visuals().widgets.hovered.bg_fill;
    let sep_color   = ui.visuals().widgets.noninteractive.bg_stroke.color;
    // Darker than the panel background so the selected tab visibly recedes.
    let panel_fill  = ui.visuals().window_fill();
    let darken = |c: egui::Color32, n: i16| {
        let f = |v: u8| (v as i16 - n).clamp(0, 255) as u8;
        egui::Color32::from_rgb(f(c.r()), f(c.g()), f(c.b()))
    };
    let active_fill = darken(panel_fill, 22);
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

                    // Background. Active tab is darker than the tab-bar
                    // panel and has rounded top corners so it visually
                    // sits forward from the bar like a file-folder tab.
                    if is_active {
                        let radius = egui::CornerRadius { nw: 6, ne: 6, sw: 0, se: 0 };
                        ui.painter().rect_filled(tab_rect, radius, active_fill);
                    } else if tab_resp.hovered() {
                        let radius = egui::CornerRadius { nw: 6, ne: 6, sw: 0, se: 0 };
                        ui.painter().rect_filled(tab_rect, radius, hover_fill);
                    }
                    let _ = sep_color; // kept for the close-X hover bg
                    let _ = panel_fill;

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

// (Removed: rounded-corner HRGN logic. SetWindowRgn interacted badly
// with WS_EX_LAYERED + pseudo-maximize, producing NC chrome strobing.
// Window stays rectangular; the painted 1 px border delineates the
// edge.)

// ── Custom title bar ──────────────────────────────────────────────────────────

fn handle_window_resize(ctx: &egui::Context) {
    // Skip edge-resize hit-testing when OS-maximized.
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
    panic_shortcut: &mut PanicShortcut,
    panic_active: &mut bool,
    panic_learning: &mut bool,
    panic_shortcut_shared: &Arc<RwLock<PanicShortcut>>,
    toggle_settings: &mut bool,
    pin_active: bool,
    do_pin_toggle: &mut bool,
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
            // Short vertical divider that doesn't extend to the panel edges.
            let (sep_rect, _) = ui.allocate_exact_size(egui::vec2(1.0, h), egui::Sense::hover());
            let inset = 8.0_f32;
            let top = sep_rect.top() + inset;
            let bottom = sep_rect.bottom() - inset;
            let x = sep_rect.center().x;
            ui.painter().line_segment(
                [egui::pos2(x, top), egui::pos2(x, bottom)],
                egui::Stroke::new(1.0, ui.visuals().widgets.noninteractive.bg_stroke.color),
            );
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

            // ── Divider before pin ─────────────────────────────────────
            ui.add_space(6.0);
            let (sep_rect, _) = ui.allocate_exact_size(egui::vec2(1.0, h), egui::Sense::hover());
            let inset = 8.0_f32;
            let x = sep_rect.center().x;
            ui.painter().line_segment(
                [egui::pos2(x, sep_rect.top() + inset),
                 egui::pos2(x, sep_rect.bottom() - inset)],
                egui::Stroke::new(1.0, ui.visuals().widgets.noninteractive.bg_stroke.color),
            );
            ui.add_space(4.0);

            // ── Pin (always-on-top) ───────────────────────────────────
            // SelectableLabel so the active state gets the standard egui
            // highlight, matching the see-through eye toggle's look.
            let pin_label = egui::RichText::new("📌").size(13.0);
            let pin_resp = ui.add(egui::SelectableLabel::new(pin_active, pin_label));
            let hover = if pin_active {
                "Pinned: window stays on top of all others.\nClick to unpin."
            } else {
                "Pin window to stay on top of all others.\nConfigure global hotkey / Guide-button trigger in Settings."
            };
            if pin_resp.on_hover_text(hover).clicked() {
                *do_pin_toggle = true;
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
                let back  = egui::Rect::from_min_size(egui::pos2(c.x - 1.5, c.y - 5.5), egui::vec2(9.0, 8.0));
                let front = egui::Rect::from_min_size(egui::pos2(c.x - 5.0, c.y - 2.0), egui::vec2(9.0, 8.0));
                ui.painter().rect_filled(
                    egui::Rect::from_min_size(front.min, egui::vec2(3.0, 2.0)),
                    egui::CornerRadius::ZERO,
                    ui.visuals().panel_fill,
                );
                draw_rect_stroke(ui.painter(), back, s);
                draw_rect_stroke(ui.painter(), front, s);
            } else {
                draw_rect_stroke(ui.painter(), egui::Rect::from_center_size(c, egui::vec2(11.0, 9.0)), s);
            }
            if resp.clicked() {
                ctx.send_viewport_cmd(egui::ViewportCommand::Maximized(!maximized));
            }

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

    // ── Panic-mode strip (anchored to the left of the window controls) ─────
    {
        // Reserve a generous slice immediately left of the window-controls.
        // The strip lays out right-to-left so the rightmost edge is always
        // pinned to ctrl_rect.left() regardless of shortcut-label length.
        const PANIC_STRIP_W: f32 = 260.0;
        let panic_rect = egui::Rect::from_min_size(
            egui::pos2(ctrl_rect.left() - PANIC_STRIP_W, bar.top()),
            egui::vec2(PANIC_STRIP_W, h),
        );
        ui.scope_builder(egui::UiBuilder::new().max_rect(panic_rect), |ui| {
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.add_space(8.0);

                // Shortcut button. While learning, label is "Press chord…".
                let btn_text = if *panic_learning {
                    "Press chord…".to_string()
                } else {
                    panic_shortcut.label()
                };
                let mut btn = egui::Button::new(egui::RichText::new(btn_text).size(12.0));
                if *panic_active {
                    btn = btn.fill(egui::Color32::from_rgb(196, 43, 28))
                             .stroke(egui::Stroke::new(1.0, egui::Color32::from_rgb(220, 80, 60)));
                } else if *panic_learning {
                    btn = btn.fill(egui::Color32::from_rgb(80, 60, 30))
                             .stroke(egui::Stroke::new(1.0, egui::Color32::from_rgb(200, 160, 80)));
                }
                let resp = ui.add(btn).on_hover_text(
                    if *panic_active {
                        "Panic mode ENGAGED — virtual output is suppressed.\nPress the shortcut again to release."
                    } else if *panic_learning {
                        "Press the new shortcut (modifier + key).\nClick again to cancel."
                    } else {
                        "Click to re-bind the shortcut.\nPress the shortcut anywhere on the system to toggle Panic mode."
                    }
                );

                // Click toggles Learn mode (start re-binding, or cancel). To
                // engage / disengage Panic mode, the user presses the shortcut
                // — the title-bar button itself is purely a re-bind control.
                if resp.clicked() {
                    *panic_learning = !*panic_learning;
                }

                // While in learn mode, watch every input event for the next chord.
                if *panic_learning {
                    let pressed: Option<egui::Key> = ctx.input(|i| {
                        i.events.iter().find_map(|e| match e {
                            egui::Event::Key { key, pressed: true, repeat: false, .. } => Some(*key),
                            _ => None,
                        })
                    });
                    if let Some(key) = pressed {
                        // Skip pure-modifier-only events (egui doesn't emit Shift/Ctrl/Alt/Win
                        // as Key here; modifiers come through i.modifiers on the next key).
                        let m = ctx.input(|i| i.modifiers);
                        let key_name = format!("{:?}", key);
                        *panic_shortcut = PanicShortcut {
                            ctrl:  m.ctrl,
                            shift: m.shift,
                            alt:   m.alt,
                            win:   m.command && !m.ctrl,
                            key:   Some(key_name),
                        };
                        save_panic_shortcut(panic_shortcut);
                        if let Ok(mut s) = panic_shortcut_shared.write() {
                            *s = panic_shortcut.clone();
                        }
                        *panic_learning = false;
                    }
                }

                ui.add_space(6.0);
                ui.label(egui::RichText::new("Panic mode:").size(12.0).weak());
            });
        });
    }

    // ── Center: logo + app name (clickable → Settings) ────────────────────
    let mid = bar.center();
    let base_color = ui.visuals().text_color();
    let font_id = egui::FontId::proportional(14.0);
    let galley = ui.painter().layout_no_wrap("FlexInput".to_string(), font_id, base_color);
    let text_size = galley.size();

    let logo_w = if logo.is_some() { 20.0 + 6.0 } else { 0.0 };
    let total_w = logo_w + text_size.x;
    let start_x = mid.x - total_w / 2.0;

    // Allocate the hit rect BEFORE painting so it wins over the title-bar
    // drag interaction allocated at the top of this function.
    let hit_rect = egui::Rect::from_center_size(
        egui::pos2(mid.x, mid.y),
        egui::vec2(total_w + 16.0, h - 6.0),
    );
    let logo_resp = ui.interact(hit_rect, ui.id().with("logo_settings"), egui::Sense::click());
    if logo_resp.clicked() {
        *toggle_settings = true;
    }
    if logo_resp.hovered() {
        ctx.set_cursor_icon(egui::CursorIcon::PointingHand);
    }

    // Subtle lighter fill always (reads against the dark title bar where a
    // dark overlay would be invisible); outline reveals on hover.
    let bg_fill = egui::Color32::from_white_alpha(if logo_resp.hovered() { 28 } else { 14 });
    let bg_stroke = if logo_resp.hovered() {
        egui::Stroke::new(1.0, egui::Color32::from_white_alpha(90))
    } else {
        egui::Stroke::NONE
    };
    ui.painter().rect(
        hit_rect,
        egui::CornerRadius::same(6),
        bg_fill,
        bg_stroke,
        egui::StrokeKind::Inside,
    );

    let text_color = if logo_resp.hovered() { egui::Color32::WHITE } else { base_color };
    let logo_tint  = if logo_resp.hovered() {
        egui::Color32::WHITE
    } else {
        egui::Color32::from_rgba_premultiplied(220, 220, 220, 220)
    };

    let painter = ui.painter();
    if let Some(tex) = logo {
        let uv = egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0));
        let logo_rect = egui::Rect::from_center_size(egui::pos2(start_x + 10.0, mid.y), egui::vec2(20.0, 20.0));
        painter.image(tex.id(), logo_rect, uv, logo_tint);
        painter.galley(egui::pos2(start_x + 20.0 + 6.0, mid.y - text_size.y / 2.0), galley, text_color);
    } else {
        painter.galley(egui::pos2(start_x, mid.y - text_size.y / 2.0), galley, text_color);
    }

    // Fire StartDrag on mouse-press (not drag_started) to avoid the
    // egui ~6 px threshold lag before the OS drag-move loop takes
    // over. Win32 itself decides click vs drag based on actual cursor
    // travel, so this is safe — single clicks still register, and
    // double-clicks still fire on the second press.
    if drag.is_pointer_button_down_on()
        && ctx.input(|i| i.pointer.primary_pressed())
    {
        ctx.send_viewport_cmd(egui::ViewportCommand::StartDrag);
    }
    if drag.double_clicked() {
        let maximized = ctx.input(|i| i.viewport().maximized.unwrap_or(false));
        ctx.send_viewport_cmd(egui::ViewportCommand::Maximized(!maximized));
    }
}

// ── Sub-patch editor windows ──────────────────────────────────────────────────

// Auto-placement step for newly-pinned modules (viewer PINNED_MOD_H + gap).
const PINNED_STEP: f32 = 108.0;
const PINNED_PAD: f32  = 4.0;

/// On-disk representation of a single sub-patch (.fxsp). Distinct from the
/// top-level patch format (.fxp) so the save/load dialog filters cleanly.
#[derive(serde::Serialize, serde::Deserialize)]
struct SubPatchFile {
    version: u32,
    sub_patch: UiSubPatch,
}

fn save_subpatch_file(sp: &UiSubPatch) -> Option<std::path::PathBuf> {
    let default_name = if sp.display_name.is_empty() {
        "sub-patch.fxsp".to_string()
    } else {
        format!("{}.fxsp", sp.display_name)
    };
    let path = rfd::FileDialog::new()
        .add_filter("FlexInput Sub-Patch", &["fxsp"])
        .set_file_name(default_name)
        .save_file()?;
    let file = SubPatchFile { version: 1, sub_patch: sp.clone() };
    if let Ok(json) = serde_json::to_string_pretty(&file) {
        let _ = std::fs::write(&path, json);
    }
    Some(path)
}

fn load_subpatch_file() -> Option<UiSubPatch> {
    let path = rfd::FileDialog::new()
        .add_filter("FlexInput Sub-Patch", &["fxsp"])
        .pick_file()?;
    let json = std::fs::read_to_string(&path).ok()?;
    let file: SubPatchFile = serde_json::from_str(&json).ok()?;
    Some(file.sub_patch)
}

fn show_subpatch_editors(
    app: &mut FlexInputApp,
    ctx: &egui::Context,
    live_device_ids: &std::collections::HashSet<String>,
) {
    // Snapshot once so the inner-canvas show call can borrow this immutably while
    // `app` is borrowed mutably for sub-patch editor state.
    let live_signals = app.last_signals.clone();
    let panic_shortcut = app.panic_shortcut.clone();
    let device_rates_inner = app.device_rates.read().map(|r| r.clone()).unwrap_or_default();
    let device_defaults_inner = crate::canvas::DeviceParamDefaults {
        stick_deadzone: app.settings.default_stick_deadzone,
        gyro_mult: app.settings.default_gyro_mult,
        mouse_sensitivity: app.settings.default_mouse_sensitivity,
    };
    let active = app.active_tab;

    // Open new editors requested this frame by the canvas viewer.
    let pending = app.tabs[active].canvas.pending_edit_subpatch.take();
    if let Some(node_id) = pending {
        let already_open = app.sub_patch_editors.iter()
            .any(|e| e.tab_idx == active && e.node_id == node_id);
        if !already_open {
            let inner_snarl = app.tabs[active].canvas.snarl
                .get_node(node_id)
                .and_then(|n| n.subpatch.as_ref())
                .map(|sp| *sp.snarl.clone())
                .unwrap_or_else(egui_snarl::Snarl::new);
            let mut editor_canvas = Canvas::new();
            editor_canvas.snarl = inner_snarl;
            editor_canvas.is_inner = true;
            app.sub_patch_editors.push(SubPatchEditor {
                tab_idx: active,
                node_id,
                parent_editor_idx: None,
                canvas: editor_canvas,
                open: true,
                last_clipboard_gen: 0,
            });
        }
    }

    // Render each open editor window.
    let mut to_close: Vec<usize> = Vec::new();
    // Collect nested-open requests outside the loop to avoid borrow issues.
    let mut pending_nested: Vec<(usize, NodeId)> = Vec::new(); // (parent_editor_idx, child_node_id)

    // Process in reverse index order so nested (child) editors write-back their
    // inner snarl into the parent editor's canvas BEFORE the parent's iteration
    // reads and propagates it upward. Without this, a child's changes (e.g. pin)
    // would be clobbered by the parent's earlier write-back to the tab canvas.
    for i in (0..app.sub_patch_editors.len()).rev() {
        if app.sub_patch_editors[i].tab_idx != active { continue; }
        let node_id = app.sub_patch_editors[i].node_id;
        let parent_editor_idx = app.sub_patch_editors[i].parent_editor_idx;

        // Close editor if the sub-patch node was deleted.
        // For nested editors, check the parent editor's canvas; for top-level, check tab canvas.
        let node_exists = match parent_editor_idx {
            None => app.tabs[active].canvas.snarl
                .get_node(node_id).map(|n| n.module_id == "subpatch").unwrap_or(false),
            Some(p) => app.sub_patch_editors[p].canvas.snarl
                .get_node(node_id).map(|n| n.module_id == "subpatch").unwrap_or(false),
        };
        if !node_exists {
            to_close.push(i);
            continue;
        }

        // Window title: resolve from parent snarl.
        let display_name = {
            let node_opt = match parent_editor_idx {
                None    => app.tabs[active].canvas.snarl.get_node(node_id),
                Some(p) => app.sub_patch_editors[p].canvas.snarl.get_node(node_id),
            };
            node_opt.map(|n| {
                if !n.display_name.is_empty() { n.display_name.clone() }
                else { n.subpatch.as_ref().map(|sp| sp.display_name.clone()).unwrap_or_else(|| "Sub-patch".to_string()) }
            }).unwrap_or_else(|| "Sub-patch".to_string())
        };

        // Extract inner canvas.
        let mut inner_canvas = std::mem::replace(&mut app.sub_patch_editors[i].canvas, Canvas::new());
        let mut open = app.sub_patch_editors[i].open;

        let descriptors: &[ModuleDescriptor] = &app.descriptors;
        let devices: &[flexinput_devices::PhysicalDevice] = &app.devices;

        // Pre-sync: pull pinned-body param changes from the parent snarl so that
        // sliders/knobs on the sub-patch body remain interactive.
        // Skip when a child editor is open: the child will write-back into this
        // editor's canvas this same frame (reverse loop order), and pre-sync would
        // overwrite those changes with the stale parent state.
        let has_active_child = app.sub_patch_editors.iter().enumerate().any(|(j, e)| {
            j != i && e.tab_idx == active && e.parent_editor_idx == Some(i)
        });
        if !has_active_child {
            let outer_inner = match parent_editor_idx {
                None    => app.tabs[active].canvas.snarl.get_node(node_id),
                Some(p) => app.sub_patch_editors[p].canvas.snarl.get_node(node_id),
            }.and_then(|n| n.subpatch.as_ref()).map(|sp| *sp.snarl.clone());
            if let Some(snarl) = outer_inner { inner_canvas.snarl = snarl; }
        }

        // Pinned IDs from parent.
        inner_canvas.pinned_inner_ids = {
            let node_opt = match parent_editor_idx {
                None    => app.tabs[active].canvas.snarl.get_node(node_id),
                Some(p) => app.sub_patch_editors[p].canvas.snarl.get_node(node_id),
            };
            node_opt.and_then(|n| n.subpatch.as_ref())
                .map(|sp| sp.exposed_modules.iter().map(|m| m.inner_node_id).collect())
                .unwrap_or_default()
        };

        let mut save_clicked = false;
        let mut load_clicked = false;
        let mut close_self = false; // for "← Back" button on nested editors

        let outer_layout_mode = match parent_editor_idx {
            None    => app.tabs[active].canvas.snarl.get_node(node_id),
            Some(p) => app.sub_patch_editors[p].canvas.snarl.get_node(node_id),
        }.map(|n| n.extra.layout_unlocked).unwrap_or(false);

        // Seed cross-boundary clipboard (outer→inner direction) so the user can
        // paste nodes copied from the outer canvas into this editor.
        // Only seed when the canvas has no clipboard yet (first frame after open).
        // Subsequent frames: the canvas retains its clipboard across mem::replace.
        if inner_canvas.clipboard().is_none() {
            if let Some(ref cb) = app.app_clipboard.clone() {
                inner_canvas.set_clipboard(cb.clone());
            }
        }
        // Snapshot gen before show() so we can detect a real user copy after.
        let gen_before = inner_canvas.clipboard_gen;

        let viewport_id = egui::ViewportId::from_hash_of(("subpatch_editor", active, node_id.0));
        // Build breadcrumb: "Parent Name > This Name" for nested editors.
        let window_title = if let Some(p) = parent_editor_idx {
            let parent_name = app.sub_patch_editors[p].canvas.snarl
                .get_node(app.sub_patch_editors[p].node_id)
                .map(|n| n.display_name.clone())
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| "Sub-patch".to_string());
            format!("✦ {} › {}", parent_name, display_name)
        } else {
            format!("✦ {}", display_name)
        };

        ctx.show_viewport_immediate(
            viewport_id,
            egui::ViewportBuilder::default()
                .with_title(window_title)
                .with_inner_size([720.0, 540.0])
                .with_min_inner_size([400.0, 300.0]),
            |vctx, _class| {
                if vctx.input(|i| i.viewport().close_requested()) {
                    open = false;
                }
                crate::canvas::viewer::set_layout_mode_active(vctx, outer_layout_mode);

                egui::TopBottomPanel::top("subpatch_editor_header").show(vctx, |ui| {
                    ui.horizontal(|ui| {
                        // "← Back" for nested editors — closes this level.
                        if parent_editor_idx.is_some()
                            && ui.small_button("← Back").on_hover_text("Close this sub-patch and return to parent").clicked()
                        {
                            close_self = true;
                        }
                        if ui.small_button("💾 Save…")
                            .on_hover_text("Save this sub-patch to a .fxsp file")
                            .clicked() { save_clicked = true; }
                        if ui.small_button("📂 Load…")
                            .on_hover_text("Replace this sub-patch's contents from a .fxsp file")
                            .clicked() { load_clicked = true; }
                        if outer_layout_mode {
                            ui.separator();
                            ui.label(egui::RichText::new("LAYOUT MODE — click highlighted elements to pin")
                                .small().color(egui::Color32::from_rgb(150, 220, 255)));
                        }
                    });
                });
                // Build a one-level AutoMap glow parent frame so the inner
                // canvas can resolve inlet AutoMap activity through the
                // sub-patch boundary. Deeper chains (grandparents) degrade
                // gracefully — the walk just bottoms out one frame earlier.
                let automap_parent = match parent_editor_idx {
                    None => Some(crate::canvas::viewer::AutomapGlowParent {
                        snarl: &app.tabs[active].canvas.snarl,
                        subpatch_node_id: node_id,
                        prev: None,
                    }),
                    Some(p) => Some(crate::canvas::viewer::AutomapGlowParent {
                        snarl: &app.sub_patch_editors[p].canvas.snarl,
                        subpatch_node_id: node_id,
                        prev: None,
                    }),
                };
                egui::CentralPanel::default().show(vctx, |ui| {
                    let _ = inner_canvas.show(
                        descriptors, live_device_ids, &live_signals,
                        &panic_shortcut, devices, &device_rates_inner,
                        device_defaults_inner, ui, automap_parent,
                    );
                });

                if let Some((inner_uid, eid, size)) = crate::canvas::viewer::take_layout_pending(vctx) {
                    inner_canvas.pending_expose_module = Some((NodeId(inner_uid), eid, size));
                }
                crate::canvas::viewer::set_layout_mode_active(vctx, false);
            },
        );

        if close_self { open = false; }

        // Collect nested edit request before putting inner_canvas back.
        if let Some(child_id) = inner_canvas.pending_edit_subpatch.take() {
            pending_nested.push((i, child_id));
        }

        // Sync clipboard upward only when the user actually copied inside this editor.
        // clipboard_gen is incremented by copy_selected(); comparing before/after show()
        // is exact regardless of node count, content, or number of copies in a row.
        let user_copied = inner_canvas.clipboard_gen != gen_before;
        if user_copied {
            app.sub_patch_editors[i].last_clipboard_gen = inner_canvas.clipboard_gen;
            if let Some(cb) = inner_canvas.clipboard() {
                app.app_clipboard = Some(cb);
                app.app_clipboard_from_inner = true;
            }
        }

        if save_clicked {
            let sp_opt = match parent_editor_idx {
                None    => app.tabs[active].canvas.snarl.get_node(node_id),
                Some(p) => app.sub_patch_editors[p].canvas.snarl.get_node(node_id),
            }.and_then(|n| n.subpatch.as_ref());
            if let Some(sp) = sp_opt { let _ = save_subpatch_file(sp); }
        }
        if load_clicked {
            if let Some(loaded) = load_subpatch_file() {
                let node_opt = match parent_editor_idx {
                    None    => app.tabs[active].canvas.snarl.get_node_mut(node_id),
                    Some(p) => app.sub_patch_editors[p].canvas.snarl.get_node_mut(node_id),
                };
                if let Some(node) = node_opt {
                    if node.subpatch.is_some() {
                        node.display_name = loaded.display_name.clone();
                        node.subpatch = Some(Box::new(loaded.clone()));
                        node.extra.layout_unlocked = false;
                    }
                }
                inner_canvas.snarl = *loaded.snarl;
            }
        }

        // Handle pin/unpin.
        if let Some((inner_id, element_id, src_size)) = inner_canvas.pending_expose_module.take() {
            let any_existing = match parent_editor_idx {
                None    => app.tabs[active].canvas.snarl.get_node(node_id),
                Some(p) => app.sub_patch_editors[p].canvas.snarl.get_node(node_id),
            }.and_then(|n| n.subpatch.as_ref())
             .map(|sp| sp.exposed_modules.iter().any(|m| m.inner_node_id == inner_id.0))
             .unwrap_or(false);
            let unpin = any_existing && element_id == "default";
            if unpin {
                let node_opt = match parent_editor_idx {
                    None    => app.tabs[active].canvas.snarl.get_node_mut(node_id),
                    Some(p) => app.sub_patch_editors[p].canvas.snarl.get_node_mut(node_id),
                };
                if let Some(node) = node_opt {
                    if let Some(sp) = node.subpatch.as_mut() {
                        sp.exposed_modules.retain(|m| m.inner_node_id != inner_id.0);
                    }
                    if node.subpatch.as_ref().map(|sp| sp.exposed_modules.is_empty()).unwrap_or(true) {
                        node.extra.layout_unlocked = false;
                    }
                }
            } else {
                let init_size = if src_size[0] >= 1.0 && src_size[1] >= 1.0 { src_size } else { [220.0, 100.0] };
                let next_y = match parent_editor_idx {
                    None    => app.tabs[active].canvas.snarl.get_node(node_id),
                    Some(p) => app.sub_patch_editors[p].canvas.snarl.get_node(node_id),
                }.and_then(|n| n.subpatch.as_ref())
                 .map(|sp| sp.exposed_modules.iter().map(|m| m.pos[1] + m.size[1]).fold(0.0f32, f32::max))
                 .unwrap_or(0.0);
                let node_opt = match parent_editor_idx {
                    None    => app.tabs[active].canvas.snarl.get_node_mut(node_id),
                    Some(p) => app.sub_patch_editors[p].canvas.snarl.get_node_mut(node_id),
                };
                if let Some(node) = node_opt {
                    if let Some(sp) = node.subpatch.as_mut() {
                        sp.exposed_modules.push(ExposedModule {
                            inner_node_id: inner_id.0,
                            element_id,
                            pos: [PINNED_PAD, next_y + PINNED_PAD],
                            size: init_size,
                        });
                    }
                }
            }
        }

        app.sub_patch_editors[i].open = open;
        app.sub_patch_editors[i].canvas = inner_canvas;

        // Port sync: always against parent snarl.
        match parent_editor_idx {
            None => {
                let (editors, tabs) = (&mut app.sub_patch_editors, &mut app.tabs);
                sync_inner_canvas_ports(&mut tabs[active].canvas.snarl, node_id, &mut editors[i].canvas);
            }
            Some(p) => {
                // Split borrow: editors[p] and editors[i] are different indices.
                let (left, right) = app.sub_patch_editors.split_at_mut(p.max(i));
                let (parent_ed, child_ed) = if p < i {
                    (&mut left[p], &mut right[i - p - 1])
                } else {
                    (&mut right[p - i - 1], &mut left[i])
                };
                sync_inner_canvas_ports(&mut parent_ed.canvas.snarl, node_id, &mut child_ed.canvas);
            }
        }

        // Write-back inner snarl to parent node.
        let inner_snarl = app.sub_patch_editors[i].canvas.snarl.clone();
        let node_opt = match parent_editor_idx {
            None    => app.tabs[active].canvas.snarl.get_node_mut(node_id),
            Some(p) => app.sub_patch_editors[p].canvas.snarl.get_node_mut(node_id),
        };
        if let Some(node) = node_opt {
            if let Some(sp) = node.subpatch.as_mut() { *sp.snarl = inner_snarl; }
        }

        if !open { to_close.push(i); }
    }

    // Open any nested editors collected during rendering.
    for (parent_idx, child_node_id) in pending_nested {
        let already_open = app.sub_patch_editors.iter()
            .any(|e| e.tab_idx == active && e.node_id == child_node_id && e.parent_editor_idx == Some(parent_idx));
        if !already_open {
            let inner_snarl = app.sub_patch_editors[parent_idx].canvas.snarl
                .get_node(child_node_id)
                .and_then(|n| n.subpatch.as_ref())
                .map(|sp| *sp.snarl.clone())
                .unwrap_or_else(egui_snarl::Snarl::new);
            let mut editor_canvas = Canvas::new();
            editor_canvas.snarl = inner_snarl;
            editor_canvas.is_inner = true;
            app.sub_patch_editors.push(SubPatchEditor {
                tab_idx: active,
                node_id: child_node_id,
                parent_editor_idx: Some(parent_idx),
                canvas: editor_canvas,
                open: true,
                last_clipboard_gen: 0,
            });
        }
    }

    for i in to_close.into_iter().rev() {
        app.sub_patch_editors.remove(i);
    }
}

/// Derives the outer sub-patch node's input/output port list from inlet/outlet
/// nodes in the inner canvas. Called every frame while the editor window is open.
///
/// New inlet/outlet nodes that lack a `pin_index` param are auto-assigned the
/// next available index. Existing indices are stable — they don't reorder when
/// nodes are moved. Port names come from the node's `display_name`; types come
/// from `params["signal_type"]` set via the node's body type-selector.
fn sync_inner_canvas_ports(
    outer_snarl: &mut egui_snarl::Snarl<NodeData>,
    node_id: NodeId,
    inner_canvas: &mut Canvas,
) {
    // ── Migrate legacy outlets: strip the obsolete output pin (and any wires) ─
    // Older builds gave subpatch.outlet a redundant "out" output pin inside the
    // sub-patch. The current descriptor has no output — values forward straight
    // to the outer meta-module. Drop stray output pins and any wires from them
    // so loaded patches converge on the new shape.
    let stale_outlet_ids: Vec<NodeId> = inner_canvas.snarl.nodes_ids_data()
        .filter(|(_, n)| n.value.module_id == "subpatch.outlet" && !n.value.outputs.is_empty())
        .map(|(id, _)| id)
        .collect();
    for id in stale_outlet_ids {
        let stale_wires: Vec<(OutPinId, InPinId)> = inner_canvas.snarl.wires()
            .filter(|(out, _)| out.node == id)
            .collect();
        for (out, inp) in stale_wires {
            inner_canvas.snarl.disconnect(out, inp);
        }
        if let Some(node) = inner_canvas.snarl.get_node_mut(id) {
            node.outputs.clear();
        }
    }

    // ── Auto-assign pin_index to newly-added inlet / outlet nodes ─────────────

    for role in ["subpatch.inlet", "subpatch.outlet"] {
        let max_idx: usize = inner_canvas.snarl.nodes_ids_data()
            .filter(|(_, n)| n.value.module_id == role)
            .filter_map(|(_, n)| n.value.params.get("pin_index").and_then(|v| v.as_u64()))
            .max()
            .map(|m| m as usize + 1)
            .unwrap_or(0);

        let unindexed: Vec<NodeId> = inner_canvas.snarl.nodes_ids_data()
            .filter(|(_, n)| n.value.module_id == role
                && !n.value.params.contains_key("pin_index"))
            .map(|(id, _)| id)
            .collect();

        for (offset, id) in unindexed.iter().enumerate() {
            if let Some(node) = inner_canvas.snarl.get_node_mut(*id) {
                let idx = max_idx + offset;
                node.params.insert("pin_index".into(),
                    serde_json::Value::Number(idx.into()));
            }
        }
    }

    // ── Sync inlet/outlet pin descriptors to match declared type and name ─────

    for (_, node) in inner_canvas.snarl.nodes_ids_data_mut() {
        let t: SignalType = node.value.params.get("signal_type")
            .and_then(|v| serde_json::from_value(v.clone()).ok())
            .unwrap_or(SignalType::Any);
        let name = node.value.display_name.clone();
        match node.value.module_id.as_str() {
            "subpatch.inlet" => {
                if let Some(pin) = node.value.outputs.get_mut(0) {
                    pin.signal_type = t;
                    pin.name = name;
                }
            }
            "subpatch.outlet" => {
                if let Some(pin) = node.value.inputs.get_mut(0) {
                    pin.signal_type = t;
                    pin.name = name;
                }
            }
            _ => {}
        }
    }

    // ── Rebuild outer node's ports from sorted inlet / outlet lists ───────────

    let mut inlets: Vec<(usize, String, SignalType)> = inner_canvas.snarl.nodes_ids_data()
        .filter(|(_, n)| n.value.module_id == "subpatch.inlet")
        .filter_map(|(_, n)| {
            let idx = n.value.params.get("pin_index")?.as_u64()? as usize;
            let t   = n.value.params.get("signal_type")
                .and_then(|v| serde_json::from_value(v.clone()).ok())
                .unwrap_or(SignalType::Any);
            Some((idx, n.value.display_name.clone(), t))
        })
        .collect();
    inlets.sort_by_key(|(i, _, _)| *i);

    let mut outlets: Vec<(usize, String, SignalType)> = inner_canvas.snarl.nodes_ids_data()
        .filter(|(_, n)| n.value.module_id == "subpatch.outlet")
        .filter_map(|(_, n)| {
            let idx = n.value.params.get("pin_index")?.as_u64()? as usize;
            let t   = n.value.params.get("signal_type")
                .and_then(|v| serde_json::from_value(v.clone()).ok())
                .unwrap_or(SignalType::Any);
            Some((idx, n.value.display_name.clone(), t))
        })
        .collect();
    outlets.sort_by_key(|(i, _, _)| *i);

    if let Some(outer_node) = outer_snarl.get_node_mut(node_id) {
        outer_node.inputs = inlets.iter()
            .map(|(_, name, t)| PinDescriptor::new(name.as_str(), *t))
            .collect();
        outer_node.outputs = outlets.iter()
            .map(|(_, name, t)| PinDescriptor::new(name.as_str(), *t))
            .collect();
        if let Some(sp) = outer_node.subpatch.as_mut() {
            sp.pins_in = inlets.iter()
                .map(|(_, name, t)| SubPatchPin { name: name.clone(), signal_type: *t })
                .collect();
            sp.pins_out = outlets.iter()
                .map(|(_, name, t)| SubPatchPin { name: name.clone(), signal_type: *t })
                .collect();
        }
    }
}
