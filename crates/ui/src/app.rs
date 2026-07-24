use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, RwLock};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use eframe::egui;
use egui_snarl::{InPinId, NodeId, OutPinId, Snarl};
use flexinput_core::{ModuleDescriptor, PinDescriptor, Signal, SignalType, SubPatchPin};
use flexinput_devices::{init_backends, midi::cc_display_name, DeviceBackend, HidHideClient, MidiBackend, PhysicalDevice};
use flexinput_engine::{Engine, NodeSnap, ProcessingGraph, ProcessingOutput, SinkBus, current_sample_rate, spawn_processing_thread};
use flexinput_modules::all_modules;
use flexinput_virtual::VirtualDevice;

use crate::{
    canvas::{Canvas, NodeData},
    canvas::node::{ExposedModule, UiSubPatch},
    canvas::ClipboardData,
    guide_watcher::{spawn_guide_watcher, GuideWatchConfig},
    panels::{physical_devices, virtual_devices::{SharedDevicePool, VirtualDevicePanel}},
    panic_hotkey::{load_panic_shortcut, save_panic_shortcut, spawn_panic_hotkey_listener},
    pin_hotkey::spawn_pin_hotkey_listener,
    settings::{self, AppSettings, PersistedTab, PersistedWorkspace, PinShortcut},
};

// ── Split-out modules ─────────────────────────────────────────────────────────
//
// `app.rs` is a facade: it keeps `FlexInputApp` and its companion structs, the
// `eframe::App` impl, and re-exports everything moved into `app/` so that every
// pre-split path (`crate::app::X`) still resolves — no consumer edits.
//
// Children carry `use super::*;`, so they see these imports and the app's own
// private items exactly as they did when they lived here.
mod bind_window;
mod chrome;
mod devices_pool;
mod graph;
mod hidhide_ui;
mod nav;
mod persistence;
mod settings_window;
mod subpatch;
mod threads;

pub(crate) use chrome::*;
// Re-exported at crate level by lib.rs, so it needs to stay `pub` here — the
// glob above would otherwise narrow it to pub(crate).
pub use chrome::render_app_icon;
pub(crate) use devices_pool::*;
pub(crate) use graph::*;
pub(crate) use subpatch::*;
pub(crate) use threads::*;

/// When set, internal `request_repaint_throttled()` callers (scope
/// renderers, animation widgets, etc.) skip their repaint request,
/// letting the explicit base-rate scheduler at the end of `update()`
/// dictate the next frame. This is the ONLY way to actually enforce a
/// repaint ceiling in egui — `request_repaint_after(d)` is a "no later
/// than d" hint that any `request_repaint()` (delay 0) call will beat
/// in the same frame. Without suppressing those at source, the 5 Hz
/// background throttle and the user's 15/30/60 Hz Repaint rate setting
/// have no effect on heavy scope-laden patches.
///
/// Set at the very top of `App::update()` based on viewport focus state
/// and the current Repaint rate setting; consumed by every renderer
/// that would otherwise call `request_repaint()` unconditionally.
pub(crate) static REPAINT_SUPPRESSED: AtomicBool = AtomicBool::new(false);


/// Renderer-internal repaint request that honors `REPAINT_SUPPRESSED`.
/// Replaces bare `ctx.request_repaint()` in widget code so the
/// throttle / background-cap can actually take effect.
pub(crate) fn request_repaint_throttled(ctx: &egui::Context) {
    if !REPAINT_SUPPRESSED.load(Ordering::Relaxed) {
        ctx.request_repaint();
    }
}

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

/// Human label for a gamepad button COMBO (`["btn_lb","btn_rb"]` → "LB / L1 +
/// RB / R1"). Empty/None handled by callers.
fn pretty_chord_combo(combo: &[String]) -> String {
    combo.iter().map(|p| pretty_chord_name(p)).collect::<Vec<_>>().join(" + ")
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
    /// Ephemeral Easy-mode UI state for this tab. Recomputed from the
    /// canvas on activation, not serialized to `.fxp`.
    pub easy_state: EasyState,
    /// Stable salt keying this tab's canvas pan/zoom, kept in sync with
    /// `canvas.view_salt`. Persisted via `PersistedTab.view_salt` so each tab
    /// keeps its own independent view across tab switches and restarts.
    pub view_salt: u64,
    /// Screen-overlay layout (pinned module elements + decorations on the
    /// transparent info overlay). Persisted with the tab (workspace + .fxp).
    pub overlay: crate::canvas::OverlayLayout,
}

/// Per-tab transient state used only when Easy mode is active. Holds the
/// preset-switch confirmation flow and a cached "current preset identity"
/// so we can detect when the in-canvas sub-patch has been tweaked away
/// from the on-disk preset. None of this is persisted.
#[derive(Default)]
pub struct EasyState {
    /// Pending preset-switch confirmation. When set, the center panel
    /// shows a "discard your tweaks?" modal; on confirm the preset at
    /// this path is applied.
    pub pending_preset_switch: Option<std::path::PathBuf>,
    /// Path + canonical-JSON content hash of the preset that was last
    /// loaded into this tab's sub-patch. Set when the user picks a chip;
    /// compared against the live sub-patch hash each frame to detect
    /// dirty state.
    pub loaded_preset: Option<(std::path::PathBuf, u64)>,
}

impl PatchTab {
    fn new_untitled(n: u32) -> Self {
        let view_salt = crate::canvas::next_canvas_salt();
        let mut canvas = Canvas::new();
        canvas.set_view_salt(view_salt);
        Self {
            title: if n == 1 { "Untitled".to_string() } else { format!("Untitled {}", n) },
            file_path: None,
            bound_exes: vec![],
            canvas,
            virtual_panel: VirtualDevicePanel::new(),
            bypassed: false,
            auto_bypass: false,
            easy_state: EasyState::default(),
            view_salt,
            overlay: Default::default(),
        }
    }
}


/// Predicate: does this canvas look like an Easy-mode-compatible
/// patch? Used by File→Load Patch to auto-flip to Easy mode when a
/// `.fxsp` is opened. Requires exactly one `subpatch` node whose
/// inner UiSubPatch:
///   * has at least one AutoMap-typed inlet AND one AutoMap-typed
///     outlet (so wiring.rs::rewire can connect a device.source and
///     virtual sinks through it), and
///   * has a non-empty layout (`items` Vec) so the central panel
///     shows actual content rather than "(pick a preset to begin)".
/// Allows any number of device.source / device.sink nodes (zero in
/// the .fxsp case since we just loaded a bare sub-patch).
fn is_easy_compatible_canvas(canvas: &crate::canvas::Canvas) -> bool {
    use flexinput_core::SignalType;
    let mut subpatch_node: Option<&NodeData> = None;
    for (_, n) in canvas.snarl.nodes_ids_data() {
        match n.value.module_id.as_str() {
            "subpatch" => {
                if subpatch_node.is_some() { return false; } // more than one
                subpatch_node = Some(&n.value);
            }
            "device.source" | "device.sink" => {} // allowed
            _ => return false,                    // foreign node
        }
    }
    let Some(node) = subpatch_node else { return false; };
    let Some(sp) = node.subpatch.as_ref() else { return false; };
    let has_in  = sp.pins_in.iter().any(|p| p.signal_type == SignalType::AutoMap);
    let has_out = sp.pins_out.iter().any(|p| p.signal_type == SignalType::AutoMap);
    has_in && has_out && !sp.items.is_empty()
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
    /// Last mutation_gen of the parent canvas that this editor synced its
    /// inner snarl from. Used to skip the per-frame `*sp.snarl.clone()`
    /// pre-sync when the parent hasn't mutated. `None` means "never synced"
    /// (force a sync on the next frame).
    last_synced_parent_gen: Option<u64>,
    /// Inner canvas mutation_gen at end of the previous frame. If unchanged
    /// at end of current frame, the user did not edit anything inside this
    /// editor, so the write-back snarl clone (line ~11205) can be skipped
    /// entirely — saving a full 50-node graph clone per idle editor frame.
    last_inner_gen: Option<u64>,
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
    /// Latest raw device signals (written by I/O thread at the polling rate); used for canvas display.
    last_signals: HashMap<(String, String), Signal>,
    eval_cache: HashMap<(NodeId, usize), Option<Signal>>,
    logo_texture: Option<egui::TextureHandle>,
    /// Transient HidHide control handle — opened only while the legacy HidHide
    /// window is in use, then released. Never held persistently (that blocks the
    /// elevated helper from opening the exclusive control device).
    hidhide: Option<HidHideClient>,
    /// Whether the HidHide driver is installed (detected once at startup without
    /// holding the handle). Gates the "Hide originals" toggle.
    hidhide_installed: bool,
    /// Debounce for HidHide reconcile: the (active, sorted vid/pid targets) last
    /// sent to the helper, so we don't re-apply identical state.
    hidhide_last_active: Option<bool>,
    hidhide_last_targets: Vec<(u16, u16)>,
    /// Force a HidHide reconcile next frame (set by the toggle and at startup).
    hidhide_dirty: bool,
    /// Order-independent hash of the connected device-id set; the reconcile's
    /// snarl walk only runs when this changes (plug/unplug), the dirty flag is set,
    /// or the slow fallback interval elapses — never every frame.
    hidhide_last_device_sig: u64,
    /// Last time the reconcile walk ran (throttle for patch-wiring edits).
    hidhide_last_reconcile: std::time::Instant,
    bottom_panel_height: f32,
    /// Summed `mutation_gen` across all tabs as of the last crash-recovery
    /// snapshot write. The recovery snapshot (`recovery.json`) is rewritten
    /// only when this total changes between frames and no value gesture is in
    /// progress — i.e. once per settled edit, mirroring the undo commit-on-
    /// settle signal. Cheap idle frames never re-serialize. See
    /// `maybe_write_recovery_snapshot`.
    last_recovery_mutation_gen: u64,
    /// Whether the Virtual Devices (top) panel is collapsed (only its heading tab visible).
    virtual_panel_collapsed: bool,
    /// Whether the Physical Devices (bottom) panel is collapsed.
    physical_panel_collapsed: bool,
    /// Whether the Easy-mode left "Devices" panel is collapsed (folded to the
    /// left, leaving only its floating tab as the re-open button). Session-only,
    /// mirroring the Advanced device panels above.
    easy_left_panel_collapsed: bool,
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
    /// Resolved sink-bound feedback (rumble/lightbar) per `(device_id, pin)`,
    /// written by the processing thread. The UI reads it to RELAY live LED
    /// colours into displays (3D viewer LED strip) — never to route hardware.
    sink_bus: SinkBus,
    // ── I/O thread shared state ───────────────────────────────────────────────
    /// App-level shared pool of virtual output devices. Same instance of
    /// `virtual.xinput.0` is reused across every tab that references it.
    /// Membership is reconciled on workspace restore, patch load, and tab
    /// close (pruning).
    shared_virtual_devices: SharedDevicePool,
    /// Background worker for blocking device lifecycle ops (create / destroy /
    /// driver reinstall). Keeps the elevated-helper IPC off the UI thread so the
    /// app never freezes during device deploy/remove/install. See `device_ops`.
    device_ops: crate::device_ops::DeviceOpHandle,
    /// Device ids with an in-flight `Create` on the worker — so `reconcile`
    /// doesn't enqueue a duplicate before the first one lands in the pool.
    pending_device_ids: HashSet<String>,
    /// Device ids whose last `Create` failed — suppresses the per-frame reconcile
    /// from retrying every frame (which would spam the worker). Cleared when the
    /// device's canvas node goes away, so removing + re-adding retries.
    failed_device_ids: HashSet<String>,
    /// Set when the "Reinstall HIDMaestro drivers" button is clicked; shows the
    /// confirm dialog. Cleared on confirm/cancel.
    reinstall_confirm_open: bool,
    /// Set when the "Uninstall HIDMaestro drivers" button is clicked; shows the
    /// confirm dialog. Cleared on confirm/cancel.
    uninstall_confirm_open: bool,
    /// Last device-op error, shown briefly in Settings. Cleared on next op.
    last_device_op_error: Option<String>,
    /// One-shot: on a GPU-recovery relaunch we seed helper persistence ON so the
    /// reclaimed devices aren't wiped; we then restore the user's real setting
    /// once reclaim settles (after a grace window so reconcile has surely run),
    /// or unconditionally after a hard deadline so the temporary override can
    /// never stick (handled at the top of `update`, even while GPU-stalled).
    /// Runtime-only — never writes the saved setting. `Some((real_persist,
    /// armed_at))` until restored.
    gpu_recovery_restore_persist: Option<(bool, std::time::Instant)>,
    /// Set of virtual device IDs referenced by the *active tab's* canvas.
    /// The I/O thread routes signals only to devices whose id is in this
    /// set; devices owned by background tabs receive `reset_outputs()` each
    /// tick so they don't drive output. Rebuilt by `set_active_tab` and
    /// whenever the active tab's canvas changes.
    active_tab_device_ids: Arc<RwLock<HashSet<String>>>,
    /// Bypass flag: when true the I/O thread calls reset_outputs() instead of flush().
    io_bypass: Arc<AtomicBool>,
    /// Gamepad-UI-nav suppression: when true the I/O thread treats output like
    /// `io_bypass` (resets instead of flushing). Set each frame to
    /// `focused && any nav-enabled device active`, so mapped output is silenced
    /// while the controller drives FlexInput's own UI, and resumes the instant
    /// FlexInput loses focus. Raw input keeps publishing, so live graphs update.
    ui_nav_suppress: Arc<AtomicBool>,
    // ── MIDI watch thread shared state ────────────────────────────────────────
    /// MIDI device IDs (`midi_in:N` / `midi_out:N`) referenced by canvas
    /// device.source / device.sink nodes across all tabs. The MIDI watch
    /// thread reads this to decide which OS handles to keep open vs release.
    /// UI rebuilds it each frame from the current canvas state.
    pinned_midi_ids: Arc<RwLock<HashSet<String>>>,
    /// Set true to ask the MIDI watch thread to re-enumerate ports (manual refresh;
    /// we no longer poll periodically, to avoid disturbing the audio stack).
    midi_refresh_requested: Arc<AtomicBool>,
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
    /// Gamepad UI-navigation runtime state (per-device toggle, selection/edit
    /// level, cursor, Alt+Tab switcher). Runtime-only — not serialized.
    gamepad_nav: crate::gamepad_nav::GamepadNav,
    /// Live processing rate handed to the engine thread.
    sample_rate_hz: Arc<AtomicU32>,
    /// Live device polling rate handed to the I/O thread.
    polling_hz: Arc<AtomicU32>,
    /// Global "route every pad through SDL" switch, shared with the I/O thread
    /// so the Settings toggle re-arbitrates backends live. Mirrors
    /// `app_settings.sdl_all_pads`.
    sdl_all_pads: Arc<AtomicBool>,
    /// Per-device measured polling rates (device_id → Hz). Written by the
    /// I/O thread, read by the canvas viewer to show live per-device Hz.
    pub device_rates: flexinput_engine::DeviceRates,
    /// Per-pin time-windowed scope rings (raw values + timestamps) for the
    /// calibration window. Populated by the I/O thread at polling Hz.
    pub scope_taps: flexinput_engine::ScopeTaps,
    /// Per-device snap-back spike filter settings (enabled, sensitivity 0..100).
    /// Written by the UI (calibration window) when the user toggles or drags
    /// the slider; read by the I/O thread each tick and pushed to backends.
    pub spike_filter_settings: Arc<RwLock<HashMap<String, (bool, f32)>>>,
    /// Pending rumble-ping requests, pushed by the UI when the user clicks a
    /// device-card icon. The I/O thread drains this each tick, starts a 200 ms
    /// rumble pulse on the named physical device, and stops it when it expires.
    pub ping_requests: Arc<Mutex<Vec<String>>>,
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
    /// Raised by the overlay hotkey listener thread; the UI loop consumes
    /// it once per frame and flips the overlay's visible flag.
    overlay_toggle_requested: Arc<AtomicBool>,
    /// Live snapshot of the overlay keyboard chord shared with its hotkey
    /// listener thread. Updated whenever the user re-binds in Settings.
    overlay_shortcut_shared: Arc<RwLock<PinShortcut>>,
    /// True while the overlay shortcut button is in Learn mode in Settings.
    overlay_learning: bool,
    /// Raised by the CONFIG-overlay hotkey listener thread; consumed once per
    /// frame to flip the config overlay's visible flag (M3).
    config_overlay_toggle_requested: Arc<AtomicBool>,
    /// Live snapshot of the config-overlay chord shared with its listener thread.
    config_overlay_shortcut_shared: Arc<RwLock<PinShortcut>>,
    /// True while the config-overlay shortcut button is in Learn mode in Settings.
    config_overlay_learning: bool,
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
    /// Deferred pin-off foreground handoff. On pin-off we must yield the
    /// foreground to another window, but doing it inside the same frame as
    /// `toggle_pin` races eframe's deferred `WindowLevel::Normal` viewport
    /// command (processed by winit *after* this frame), which re-activates
    /// us — the user sees the target app blink forward then FlexInput snap
    /// back. So we stash the target here and re-assert it for a few frames
    /// once the level change has settled. `Some((hwnd_or_0, frames_left))`;
    /// hwnd 0 means "no specific target — yield to whatever's beneath us".
    pin_pending_yield: Option<(isize, u8)>,
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
    /// stop emitting events. Field exists in release too (so the struct
    /// layout doesn't drift between profiles) but is never assigned —
    /// `#[allow(dead_code)]` keeps the release warning down.
    #[allow(dead_code)]
    profiler_server: Option<puffin_http::Server>,
    /// Last bg_repaint_hz setting we logged. Debug-only — used to print
    /// the active rate exactly once per change so we can verify the
    /// slider is actually wired through to ctx.request_repaint_after.
    #[cfg(debug_assertions)]
    last_logged_repaint_hz: Option<u32>,
    /// Last (theme, contrast bits, see_through_active, alpha bits) tuple
    /// applied to the egui style. The per-frame `apply_theme_and_contrast`
    /// call short-circuits when the tuple matches the current settings —
    /// avoids walking the entire egui Visuals on every vsync just to
    /// rewrite identical values. `None` means "never applied", which
    /// forces the first frame to push the initial style.
    theme_applied_for: Option<(crate::settings::Theme, u32, bool, u32)>,
    /// True once the GPU device was lost while FlexInput was NOT the foreground
    /// window. In this state the GUI is stalled (renders nothing) so we don't
    /// thrash relaunches against a game that owns the GPU; the input/engine
    /// threads keep running. Cleared by relaunching once FlexInput returns to
    /// the foreground. See the GPU-loss block at the top of `update`.
    gpu_stalled: bool,
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
        let key_raw = self.key.as_deref().unwrap_or("…");
        let key = match key_raw { "Backtick" => "~", other => other };
        if parts.is_empty() {
            key.to_string()
        } else {
            format!("{}+{}", parts.join("+"), key)
        }
    }
}

impl FlexInputApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        // Permanent breadcrumb: which GPU adapter + wgpu backend this instance
        // rendered on. Window-compositor symptoms (slow resize, stale frames on
        // restore) are backend/driver-specific, so the first diagnostic question
        // is always "what did wgpu pick?" — answer it in the log up front.
        if let Some(rs) = &cc.wgpu_render_state {
            let info = rs.adapter.get_info();
            eprintln!(
                "[gpu] adapter=\"{}\" backend={:?} type={:?} driver=\"{}\"",
                info.name, info.backend, info.device_type, info.driver_info
            );
            // Capture the negotiated surface format so the 3D controller
            // pipeline is built to match the render pass (else wgpu rejects
            // the draw with an incompatible-target validation error).
            crate::model::callback::set_target_format(rs.target_format);
        }
        setup_fonts(&cc.egui_ctx);
        // Install egui_extras image loaders so SVG images render inside nodes
        // and pinned sub-patch widgets. The svg feature pulls in resvg/usvg.
        egui_extras::install_image_loaders(&cc.egui_ctx);
        let descriptors = all_modules().into_iter().map(|r| r.descriptor).collect();
        let backends    = init_backends();
        let midi_backend = Arc::new(Mutex::new(Some(MidiBackend::new())));
        // Detect HidHide presence WITHOUT holding the control-device handle. A
        // persistently-held handle blocks the elevated helper from opening
        // HidHide's (exclusive) control device — observed as the helper reporting
        // "driver not present" even though detection here succeeds. So open
        // transiently, record presence, and drop the handle immediately. All WRITES
        // go through the helper; `self.hidhide` is opened on demand only while the
        // legacy HidHide window is in use (released when it closes).
        let hidhide_installed = HidHideClient::try_open().is_some();
        let hidhide: Option<HidHideClient> = None;
        // Title-bar logo tile. Loaded from a pre-baked 256px PNG (rendered
        // from icon_v2.svg at build time) rather than rasterizing the SVG
        // here — the SVG's blur/color-matrix filters take tens of seconds
        // for resvg to rasterize, which previously stalled startup. The
        // 256px source downscales crisply to the ~30px tile via mipmaps.
        let logo_texture = eframe::icon_data::from_png_bytes(APP_ICON_PNG).ok().map(|icon| {
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
                [icon.width as usize, icon.height as usize], &premul);
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
        let mut app_settings = settings::load_settings();
        // Seed the HIDMaestro helper's persistence policy *before* any device is
        // created, so the helper's first `Hello` (and its orphan-cleanup
        // decision) reflects the user's setting. Off → helper removes leftovers
        // and tears down on app death; on → devices persist for reclaim.
        //
        // GPU-recovery boot: the prior (dead-GPU) process flipped the helper to
        // persist=on and left the virtual devices alive for us to reclaim. If we
        // seeded the real setting here and it's OFF, our first Hello would run the
        // startup wipe and destroy exactly those devices before reclaim. So on a
        // recovery boot we seed persist=ON now (no wipe, reclaim succeeds) and
        // restore the user's real setting once devices are reclaimed (below).
        #[cfg(windows)]
        {
            let gpu_recovery = std::env::var(crate::GPU_RECOVERY_ENV).is_ok();
            let seed_persist = gpu_recovery || app_settings.persist_virtual_devices;
            flexinput_hidmaestro::helper::set_persist(seed_persist);
        }
        // Migrate any previously-saved arbitrary polling rate to a valid step
        // (the slider is now quantized to whole-ms periods).
        app_settings.polling_hz = settings::snap_polling_hz(app_settings.polling_hz);
        let sample_rate_hz = Arc::new(AtomicU32::new(app_settings.sample_rate_hz));
        let polling_hz     = Arc::new(AtomicU32::new(app_settings.polling_hz));
        let sdl_all_pads   = Arc::new(AtomicBool::new(app_settings.sdl_all_pads));
        // Mirror the polling rate to flexinput-virtual so HIDMaestro XInput pads
        // set their XUSB companion's pump period to match (see
        // requested_poll_interval_ms). Pushed again on every slider change.
        flexinput_virtual::set_requested_poll_hz(app_settings.polling_hz);
        // Mirror the virtual-mouse physical-suppression settings to the
        // flexinput-virtual globals the keymouse thread reads. Pushed again on
        // every Settings change (see the Settings panel handlers).
        flexinput_virtual::set_mouse_suppression_enabled(app_settings.mouse_suppression_enabled);
        flexinput_virtual::set_mouse_suppression_release_ms(app_settings.mouse_suppress_release_ms);
        // Experimental mixed-output braiding (read by both the I/O thread and the
        // keymouse thread via the shared phase clock in flexinput-virtual).
        flexinput_virtual::set_braid_enabled(app_settings.mixed_braid_enabled);
        flexinput_virtual::set_braid_rate_hz(app_settings.mixed_braid_rate_hz);
        // Register the user 3D-models directory (global setting) before any
        // viewer loads a model.
        crate::model::set_user_models_dir(
            app_settings
                .user_models_dir
                .as_deref()
                .filter(|s| !s.is_empty())
                .map(std::path::PathBuf::from),
        );

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
        let persisted_tab_to_patch = |pt: PersistedTab| -> PatchTab {
            let mut canvas = Canvas::new();
            canvas.snarl = pt.snarl;
            crate::canvas::migrate_loaded_snarl(&mut canvas.snarl);
            // Wipe any stale in-progress capture/learn state from saved
            // remapper-family nodes so a restored patch starts clean.
            clear_canvas_capture_state(&mut canvas);
            // Restored patches are conceptually "loaded" — honor the
            // on-patch-load camera setting so the saved view's
            // arbitrary pan/zoom doesn't strand the user off-canvas.
            canvas.pending_view_action = on_load_view;
            // Restore this tab's stable view salt (keys its pan/zoom). `0`
            // means a pre-existing workspace without the field — allocate a
            // fresh unique salt so old tabs don't all collapse onto one key.
            let view_salt = if pt.view_salt != 0 { pt.view_salt } else { crate::canvas::next_canvas_salt() };
            canvas.set_view_salt(view_salt);
            let easy_state = EasyState {
                loaded_preset: pt.easy_preset_path.map(|p| (p, 0)),
                ..EasyState::default()
            };
            PatchTab {
                title: pt.title,
                file_path: pt.file_path,
                bound_exes: pt.bound_exes,
                canvas,
                virtual_panel: VirtualDevicePanel::new(),
                bypassed: false,
                auto_bypass: pt.auto_bypass,
                easy_state,
                view_salt,
                overlay: pt.overlay,
            }
        };
        // A crash-recovery snapshot takes precedence over the opt-in workspace:
        // its presence means the last run did NOT exit cleanly (a GPU-loss
        // relaunch or hard crash), so restoring it is how the relaunch becomes
        // seamless. It's consumed exactly once — deleted immediately after we
        // read it — and is honored regardless of `keep_workspace`. If absent,
        // we fall back to the opt-in workspace, then to one empty tab.
        //
        // Each arm yields `(tabs, active_tab)` so we reopen on the tab the user
        // left rather than always snapping back to tab 0. The persisted
        // `active_tab` is clamped against the restored tab count below.
        let (tabs, restored_active_tab): (Vec<PatchTab>, usize) =
            if let Some(ws) = settings::load_recovery().filter(|ws| !ws.tabs.is_empty()) {
                eprintln!("Restoring {} tab(s) from crash-recovery snapshot.", ws.tabs.len());
                settings::delete_recovery();
                let active = ws.active_tab;
                (ws.tabs.into_iter().map(persisted_tab_to_patch).collect(), active)
            } else if app_settings.keep_workspace {
                match settings::load_workspace() {
                    Some(ws) if !ws.tabs.is_empty() => {
                        let active = ws.active_tab;
                        (ws.tabs.into_iter().map(persisted_tab_to_patch).collect(), active)
                    }
                    _ => (vec![PatchTab::new_untitled(1)], 0),
                }
            } else {
                (vec![PatchTab::new_untitled(1)], 0)
            };
        // `tabs` is guaranteed non-empty here (every arm yields ≥1 tab).
        let active_tab = restored_active_tab.min(tabs.len() - 1);
        let shared_devices = Arc::new(RwLock::new(Vec::<PhysicalDevice>::new()));
        let shared_midi_devices = Arc::new(RwLock::new(Vec::<PhysicalDevice>::new()));
        let pinned_midi_ids = Arc::new(RwLock::new(HashSet::<String>::new()));
        // Set by the "Refresh MIDI" button to ask the watch thread to re-enumerate
        // (we no longer poll, to avoid periodic wdmaud/audio disruption).
        let midi_refresh_requested = Arc::new(AtomicBool::new(false));
        let io_bypass      = Arc::new(AtomicBool::new(false));
        let ui_nav_suppress = Arc::new(AtomicBool::new(false));

        // App-level shared virtual-device pool. Reconciled from every
        // restored tab's canvas so re-opening the app brings back the
        // devices each patch requires (no duplicates: a single shared
        // instance per device id).
        let shared_virtual_devices: SharedDevicePool =
            Arc::new(Mutex::new(Vec::<Box<dyn VirtualDevice>>::new()));

        // Background worker for device lifecycle ops — spawned before the first
        // reconcile so even startup device creation runs off the UI thread (the
        // window appears immediately; pads pop in as the worker finishes them).
        let device_ops = crate::device_ops::spawn(cc.egui_ctx.clone());
        let mut pending_device_ids: HashSet<String> = HashSet::new();
        {
            // Enqueue (don't build inline) every device the restored tabs need.
            let mut needed: Vec<String> = Vec::new();
            for tab in &tabs {
                for id in snarl_virtual_device_ids(&tab.canvas.snarl) {
                    if !needed.contains(&id) {
                        needed.push(id);
                    }
                }
            }
            for id in needed {
                if pending_device_ids.insert(id.clone()) {
                    let _ = device_ops.tx.send(crate::device_ops::DeviceOp::Create { device_id: id });
                }
            }
        }

        // Active-tab device id filter — I/O thread only ticks devices
        // whose id is in this set. Seeded from the *restored* active tab's
        // canvas (not tab 0) so its virtual pads are live immediately on
        // launch, before any manual tab interaction.
        let active_tab_device_ids: Arc<RwLock<HashSet<String>>> = Arc::new(RwLock::new(
            snarl_virtual_device_ids(&tabs[active_tab].canvas.snarl).into_iter().collect(),
        ));

        let device_rates = flexinput_engine::new_device_rates();
        let scope_taps   = flexinput_engine::new_scope_taps();
        let spike_filter_settings: Arc<RwLock<HashMap<String, (bool, f32)>>> =
            Arc::new(RwLock::new(HashMap::new()));
        let ping_requests: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        spawn_io_thread(
            backends,
            Arc::clone(&midi_backend),
            Arc::clone(&proc_device_signals),
            Arc::clone(&sink_bus),
            Arc::clone(&shared_virtual_devices),
            Arc::clone(&active_tab_device_ids),
            Arc::clone(&io_bypass),
            Arc::clone(&ui_nav_suppress),
            Arc::clone(&shared_devices),
            Arc::clone(&shared_midi_devices),
            Arc::clone(&polling_hz),
            Arc::clone(&device_rates),
            Arc::clone(&scope_taps),
            Arc::clone(&spike_filter_settings),
            Arc::clone(&sdl_all_pads),
            Arc::clone(&ping_requests),
        );

        spawn_midi_watch_thread(
            Arc::clone(&midi_backend),
            Arc::clone(&pinned_midi_ids),
            Arc::clone(&shared_midi_devices),
            Arc::clone(&midi_refresh_requested),
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
            crate::pin_hotkey::HOTKEY_ID_PIN,
            "pin-hotkey",
            Arc::clone(&pin_shortcut_shared),
            Arc::clone(&pin_toggle_requested),
        );
        // Info-overlay visibility hotkey — same listener pattern, own id.
        let overlay_toggle_requested = Arc::new(AtomicBool::new(false));
        let overlay_shortcut_shared  = Arc::new(RwLock::new(app_settings.overlay_shortcut.clone()));
        spawn_pin_hotkey_listener(
            crate::pin_hotkey::HOTKEY_ID_OVERLAY,
            "overlay-hotkey",
            Arc::clone(&overlay_shortcut_shared),
            Arc::clone(&overlay_toggle_requested),
        );
        // Config-overlay visibility hotkey — same pattern, own id (M3).
        let config_overlay_toggle_requested = Arc::new(AtomicBool::new(false));
        let config_overlay_shortcut_shared  = Arc::new(RwLock::new(app_settings.config_overlay_shortcut.clone()));
        spawn_pin_hotkey_listener(
            crate::pin_hotkey::HOTKEY_ID_CONFIG,
            "config-overlay-hotkey",
            Arc::clone(&config_overlay_shortcut_shared),
            Arc::clone(&config_overlay_toggle_requested),
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
        // Same for the info overlay's visible flag (▣ toggle).
        if app_settings.overlay_visible {
            crate::overlay::set_overlay_visible(&cc.egui_ctx, true);
        }
        // And the config overlay's visible flag (M3).
        if app_settings.config_overlay_visible {
            crate::config_overlay::set_config_overlay_visible(&cc.egui_ctx, true);
        }

        // Pick `next_untitled` high enough that any restored "Untitled N" tab
        // doesn't collide with a freshly-created one.
        let next_untitled = tabs.iter()
            .filter_map(|t| t.title.strip_prefix("Untitled")
                .and_then(|rest| rest.trim().parse::<u32>().ok()
                    .or_else(|| if rest.is_empty() { Some(1) } else { None })))
            .max()
            .map(|n| n + 1)
            .unwrap_or(2);

        let mut app = Self {
            engine: Engine::new(),
            tabs,
            active_tab,
            next_untitled,
            descriptors,
            midi_backend,
            devices: vec![],
            shared_devices,
            last_signals: HashMap::new(),
            eval_cache: HashMap::new(),
            logo_texture,
            hidhide,
            hidhide_installed,
            hidhide_last_active: None,
            hidhide_last_targets: Vec::new(),
            hidhide_dirty: true, // reconcile once at startup (apply default-on masking)
            hidhide_last_device_sig: 0,
            hidhide_last_reconcile: std::time::Instant::now(),
            bottom_panel_height: 220.0,
            // Seeded below from the restored tabs so the first frame doesn't
            // pointlessly rewrite the recovery snapshot we may have just loaded.
            last_recovery_mutation_gen: 0,
            virtual_panel_collapsed: false,
            physical_panel_collapsed: false,
            easy_left_panel_collapsed: false,
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
            sink_bus: Arc::clone(&sink_bus),
            shared_virtual_devices,
            device_ops,
            pending_device_ids,
            failed_device_ids: HashSet::new(),
            reinstall_confirm_open: false,
            uninstall_confirm_open: false,
            last_device_op_error: None,
            gpu_recovery_restore_persist: {
                #[cfg(windows)]
                { if std::env::var(crate::GPU_RECOVERY_ENV).is_ok() {
                    Some((app_settings.persist_virtual_devices, std::time::Instant::now()))
                } else { None } }
                #[cfg(not(windows))]
                { None }
            },
            active_tab_device_ids,
            io_bypass,
            ui_nav_suppress,
            pinned_midi_ids,
            midi_refresh_requested,
            panic_shortcut: panic_shortcut.clone(),
            panic_active: false,
            panic_learning: false,
            panic_toggle_requested,
            panic_shortcut_shared,
            settings: app_settings,
            settings_open: false,
            settings_dirty: false,
            gamepad_nav: crate::gamepad_nav::GamepadNav::default(),
            sample_rate_hz,
            polling_hz,
            sdl_all_pads,
            device_rates,
            scope_taps,
            spike_filter_settings,
            ping_requests,
            pin_toggle_requested,
            pin_shortcut_shared,
            pin_guide_cfg,
            pin_learn_chord,
            pin_learned_chord,
            pin_learning: false,
            overlay_toggle_requested,
            overlay_shortcut_shared,
            overlay_learning: false,
            config_overlay_toggle_requested,
            config_overlay_shortcut_shared,
            config_overlay_learning: false,
            pin_prev_foreground_hwnd: None,
            pin_last_external_hwnd: None,
            pin_pending_yield: None,
            self_hwnd: None,
            profiler_server: None,
            #[cfg(debug_assertions)]
            last_logged_repaint_hz: None,
            theme_applied_for: None,
            // If the panic hook relaunched us because the GPU was lost while a
            // game owned it (FlexInput backgrounded), boot straight into the
            // stall so we don't render against the game-held device and loop.
            gpu_stalled: std::env::var(crate::GPU_STALL_ENV).is_ok(),
        };
        // Seed the recovery dirty-signal from the restored tabs so the first
        // frame doesn't immediately rewrite the snapshot we may have just
        // loaded (the restored tabs already carry their mutation_gen).
        app.last_recovery_mutation_gen = app.total_mutation_gen();
        app
    }
}

impl eframe::App for FlexInputApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Tell puffin a new frame began. Cheap when scopes are off (atomic
        // check); only does real work while the profiler toggle is on.
        puffin::GlobalProfiler::lock().new_frame();
        puffin::profile_function!();

        // Publish this frame's macro-port table so mapping-card chips, the
        // KB/M picker, and Touch Zones' analog-output checks can resolve
        // "macro:{id}" pins to names/icons/types anywhere without threading
        // the table through signatures.
        crate::macro_icons::publish_registry(
            std::sync::Arc::new(self.macro_display_entries()));

        // ── GPU-recovery persist safety net ──────────────────────────────
        // On a GPU-loss relaunch the helper's persistence was forced ON so the
        // devices kept alive across the relaunch aren't wiped before reclaim.
        // This is a TEMPORARY RUNTIME override (`set_persist`) — it never writes
        // the saved `persist_virtual_devices` setting. Hand the helper back the
        // user's REAL setting as soon as reclaim settles, OR — as a hard backstop
        // — after a bounded deadline even if it never settles (failed reclaim,
        // empty-but-pending, or a stall/crash boot). Runs FIRST, ahead of the
        // stall early-return below, so a stalled child still reverts instead of
        // stranding persist=on and leaking the virtual devices on close. It only
        // ever restores to the user's setting; it does not change it.
        #[cfg(windows)]
        if let Some((real_persist, armed_at)) = self.gpu_recovery_restore_persist {
            let elapsed = armed_at.elapsed();
            // Grace before deciding "nothing to reclaim" (startup reconcile must
            // have enqueued its reclaim creates first); hard deadline is the
            // backstop so the override can never stick.
            const GRACE: std::time::Duration = std::time::Duration::from_secs(3);
            const HARD_DEADLINE: std::time::Duration = std::time::Duration::from_secs(15);
            let settled = elapsed >= GRACE && {
                let pool_has = !self.shared_virtual_devices.lock().unwrap().is_empty();
                pool_has || self.pending_device_ids.is_empty()
            };
            if settled || elapsed >= HARD_DEADLINE {
                flexinput_hidmaestro::helper::set_persist(real_persist);
                self.gpu_recovery_restore_persist = None;
                eprintln!(
                    "[gpu-recovery] restored persist={real_persist} ({})",
                    if settled { "reclaim settled" } else { "hard deadline" }
                );
            }
        }

        // ── GPU-loss recovery ────────────────────────────────────────────
        // The vendored egui-wgpu raises this flag (instead of panicking) when
        // the graphics device is lost mid-frame — a fullscreen game resetting
        // the device, a driver TDR, or VRAM exhaustion. eframe 0.33 can't
        // rebuild the device in place, so the ultimate fix is to relaunch a
        // fresh process; the recovery snapshot makes that seamless.
        //
        // BUT relaunching *while a game owns the GPU* is futile and harmful:
        // the game holds the device (fullscreen flip / exclusive), so every
        // fresh process loses it again immediately and we thrash — the UI only
        // becomes usable once the game exits, and each teardown briefly
        // interrupts input forwarding. The input + engine pipeline runs on
        // independent threads (device-io / processing) that DON'T need the GPU,
        // so when FlexInput isn't the foreground window we instead STALL the
        // GUI: render nothing, keep those threads (and virtual devices / rumble
        // forwarding) alive, and defer the relaunch until FlexInput is
        // foreground again (user alt-tabbed back, or the game exited). GUI
        // latency while backgrounded doesn't matter; uninterrupted input does.
        if self.gpu_stalled {
            // Already stalled. If we're back in foreground, rebuild the UI now;
            // otherwise keep idling (don't render — see below).
            let foreground = crate::process_list::foreground_exe().is_none();
            // The device may re-latch the flag on each polled present; clear it
            // so a future genuine loss is still observable.
            eframe::egui_wgpu::GPU_LOST.store(false, std::sync::atomic::Ordering::SeqCst);
            if foreground {
                eprintln!("GPU stall: FlexInput foreground again — relaunching to rebuild UI.");
                settings::save_recovery(&self.build_persisted_workspace());
                settings::save_settings(&self.settings);
                crate::relaunch_self_and_exit();
            }
            // Stay stalled: poll a few times a second for foreground return.
            // Returning here builds no UI this frame, so egui emits no shapes
            // and no texture deltas — the present is a safe no-op on the dead
            // device (the buffer-staging guards skip; the texture path isn't
            // reached because nothing changed).
            ctx.request_repaint_after(std::time::Duration::from_millis(250));
            return;
        }
        if eframe::egui_wgpu::GPU_LOST.load(std::sync::atomic::Ordering::SeqCst) {
            let foreground = crate::process_list::foreground_exe().is_none();
            if foreground {
                // We're the focused window — the user is looking at the UI, so
                // rebuild it immediately via relaunch.
                eprintln!("GPU device lost (foreground) — saving recovery snapshot and relaunching.");
                settings::save_recovery(&self.build_persisted_workspace());
                settings::save_settings(&self.settings);
                crate::relaunch_self_and_exit();
            }
            // A game (or other app) owns the GPU. Enter the stall instead of
            // relaunching into a loop. Persist a snapshot once so a hard crash
            // during the stall still recovers, then idle the GUI.
            eprintln!("GPU device lost (backgrounded) — stalling GUI; input/engine keep running.");
            settings::save_recovery(&self.build_persisted_workspace());
            settings::save_settings(&self.settings);
            eframe::egui_wgpu::GPU_LOST.store(false, std::sync::atomic::Ordering::SeqCst);
            self.gpu_stalled = true;
            ctx.request_repaint_after(std::time::Duration::from_millis(250));
            return;
        }

        // ── Apply finished device-ops worker results ─────────────────────
        // Pushes freshly-built virtual devices into the shared pool (and clears
        // in-flight markers). Done before anything reads the pool this frame.
        self.drain_device_op_results();

        // Keep the I/O thread's active-tab device-id filter current every frame.
        // This set gates which virtual devices have their feedback (rumble/FFB)
        // routed into the graph. It was previously refreshed only on canvas
        // events, so for a device acting purely as a feedback SOURCE the set
        // could stay stale and its rumble never reached the physical pad. The
        // refresh is change-guarded internally (cheap when nothing changed), so
        // a per-frame call doesn't contend with the I/O thread in the steady
        // state.
        self.refresh_active_tab_device_ids();

        // ── Visibility / focus state ────────────────────────────────────
        // Tracked here, consumed by the repaint-scheduling code at the
        // tail of update(). When minimized OR unfocused, we cap the
        // base repaint rate at 5 Hz regardless of the user's Settings →
        // Repaint rate, but we still run the full UI pass — animations
        // tick, gamepad-nav cursor moves, the focus-flip handoff to a
        // virtual output runs, etc. Skipping the pass entirely broke
        // gamepad-nav → virtual-output transitions.
        let vp_minimized = ctx.input(|i| i.viewport().minimized.unwrap_or(false));
        let vp_focused   = ctx.input(|i| i.viewport().focused.unwrap_or(true));
        let bg_throttle  = vp_minimized || !vp_focused;
        // Renderer-internal `request_repaint_throttled()` calls (scope
        // tracers, marquee animations, glow effects, etc.) skip when
        // this flag is set. We ONLY suppress while the window is in the
        // background — when focused, the user wants smooth visuals and
        // every animation gets vsync. When backgrounded, the user's
        // bg_repaint_hz dictates the rate and inner repaint requests
        // are short-circuited at the source.
        REPAINT_SUPPRESSED.store(bg_throttle, Ordering::Relaxed);

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

        // Apply selected theme + contrast only when the relevant
        // settings actually changed (plus once at startup before any
        // user input). The function walks the egui style and replaces
        // colors/strokes — cheap individually but wasted on every
        // vsync frame when nothing has moved. `theme_applied_for`
        // remembers the (theme, contrast, see_through_active, alpha)
        // tuple we last pushed into the context; matching values skip
        // the re-apply entirely.
        {
            puffin::profile_scope!("apply_theme_and_contrast");
            let key = (
                self.settings.theme,
                self.settings.contrast.to_bits(),
                self.settings.see_through_active,
                self.settings.see_through_alpha.to_bits(),
            );
            if self.theme_applied_for != Some(key) {
                crate::settings::apply_theme_and_contrast(ctx, &self.settings);
                self.theme_applied_for = Some(key);
            }
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

        // Read the latest device signals written by the I/O thread (polling rate).
        // `load_full()` returns the current `Arc<HashMap>` — a refcount
        // bump, no map clone. Deref-cloning the map only happens at the
        // few `.last_signals = …` sites that need an owned map.
        {
            puffin::profile_scope!("read_signals_load");
            let snap = self.proc_device_signals.load_full();
            self.last_signals = (*snap).clone();
        }
        // Merge live LED/lightbar feedback into the signal map so displays can
        // relay it (the 3D viewer's LED strip). These are sink-bound OUTPUT
        // pins ("lightbar_*"), so they never collide with input pin names.
        {
            let bus = self.sink_bus.read().unwrap();
            for ((dev, pin), sig) in bus.iter() {
                if pin.starts_with("lightbar_") {
                    self.last_signals.insert((dev.clone(), pin.clone()), *sig);
                }
            }
        }
        // Refresh device list from I/O thread. Both gilrs and MIDI device
        // listings are populated there, so the UI never contends with the
        // I/O-rate MIDI poll lock (which used to cause MIDI cards to flicker
        // in/out whenever the lock was held during a paint).
        {
            puffin::profile_scope!("read_devices_clone");
            self.devices = self.shared_devices.read().unwrap().clone();
        }

        // Keep HidHide masking in sync with the live patch + device set. Cheap and
        // debounced (only acts when the remapped-physical set actually changes).
        self.reconcile_hidhide();

        // Dispatch XInput player-slot requests from the slot circles. Each request
        // is routed to the helper's ordered-reorder engine, which places the named
        // device at the exact clicked slot (displacing peers as needed, with a
        // crash-safe watchdog). The card key identifies the device:
        //   * "output"        — the Easy-mode card → resolve the active virtual id,
        //   * "virtual.*"      — a Virtual Xbox sink node (key IS its device_id),
        //   * "gilrs:xinput:*" — a physical Xbox source node (key IS its device_id;
        //                        we look up its VID/PID so the helper can find the
        //                        physical XUSB devnode).
        // Runs off the UI thread (blocking, multi-second helper IPC).
        #[cfg(windows)]
        for (card, slot) in crate::easy::io_panel::drain_xinput_slot_requests() {
            // Resolve (device_id, vid, pid) for the request.
            let resolved: Option<(String, Option<u16>, Option<u16>)> =
                if card == crate::easy::io_panel::XINPUT_CARD_OUTPUT {
                    self.active_virtual_xinput_device_id().map(|id| (id, None, None))
                } else if card.starts_with("gilrs:") || card.starts_with("sdl:") {
                    // Physical source — find its VID/PID from the live device list.
                    let vp = self.devices.iter()
                        .find(|d| d.id == card)
                        .map(|d| (d.vid, d.pid))
                        .unwrap_or((None, None));
                    Some((card.clone(), vp.0, vp.1))
                } else {
                    // Virtual sink node — the card key is the device_id itself.
                    Some((card.clone(), None, None))
                };
            match resolved {
                Some((dev_id, vid, pid)) => {
                    // Optimistically record the assignment so the "this device's
                    // slot" glow is correct immediately (and stays correct with
                    // multiple XInput devices present, which we can't otherwise
                    // correlate). The engine places the device at this slot.
                    crate::easy::io_panel::record_xinput_slot_assignment(&dev_id, slot);
                    std::thread::spawn(move || {
                        match flexinput_hidmaestro::helper::assign_xinput_slot(&dev_id, vid, pid, slot) {
                            Ok(()) => eprintln!("[xinput-slot] assigned {dev_id} to slot {slot}"),
                            Err(e) => eprintln!("[xinput-slot] assign {dev_id} → slot {slot} failed: {e}"),
                        }
                    });
                }
                None => eprintln!("[xinput-slot] no XInput device resolved for card '{card}'"),
            }
        }

        // Gamepad UI navigation: consume the active nav device's input and
        // drive FlexInput's own UI. Must run after `last_signals` is refreshed
        // (above) and before the Easy panel renders so selection/edit changes
        // show this frame. Also publishes the `ui_nav_suppress` flag.
        {
            puffin::profile_scope!("gamepad_nav");
            self.run_gamepad_nav(ctx);
        }

        // Desktop Settings window shortcut-chord capture: when a learn is in
        // flight but the gamepad settings panel is NOT open, the capture was
        // started from the real Settings window (mouse). Pump it from any
        // connected gamepad so the user can still press a controller combo
        // there. The gamepad-panel path captures inside nav_drive_gp_settings.
        if self.gamepad_nav.chord_learn.is_some() && !self.gamepad_nav.settings_open {
            self.drive_chord_learn_desktop();
        }

        // Global (non-nav-only) shortcut-chord toggles: when the user has opted
        // out of nav-only, the see-through / panic combos fire from ANY connected
        // gamepad whenever FlexInput is focused — independent of nav mode. The
        // nav-only path lives inside run_gamepad_nav (driving device only).
        if !self.settings.gamepad_chords_nav_only && self.gamepad_nav.chord_learn.is_none() {
            self.check_shortcut_chords_global(ctx);
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

        // Overlay visibility persists the same way (titlebar ▣ / hotkey /
        // chord all write the ctx slot; we mirror it into settings here).
        {
            let from_slot = crate::overlay::overlay_visible(ctx);
            if from_slot != self.settings.overlay_visible {
                self.settings.overlay_visible = from_slot;
                self.settings_dirty = true;
            }
        }
        // Config overlay visibility persists the same way (M3).
        {
            let from_slot = crate::config_overlay::config_overlay_visible(ctx);
            if from_slot != self.settings.config_overlay_visible {
                self.settings.config_overlay_visible = from_slot;
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

        // ── Overlay visibility toggle (global hotkey) ─────────────────────
        // The keyboard listener thread raises the flag; flip the ctx slot
        // here on the UI thread (show_overlay reads it later this frame).
        if self.overlay_toggle_requested.swap(false, Ordering::Relaxed) {
            crate::overlay::set_overlay_visible(ctx, !crate::overlay::overlay_visible(ctx));
        }

        // ── Config overlay visibility toggle (global hotkey, M3) ──────────
        if self.config_overlay_toggle_requested.swap(false, Ordering::Relaxed) {
            let on = crate::config_overlay::config_overlay_visible(ctx);
            crate::config_overlay::set_config_overlay_visible(ctx, !on);
        }

        // Deferred pin-off foreground handoff. Scheduled by `toggle_pin` on
        // pin-off as `(target, delay_frames)`. eframe processes the
        // `WindowLevel::Normal` command in winit ~1 frame after we issue it,
        // and that re-activates us — so if we yielded foreground inline the
        // target app would blink forward then FlexInput would snap back. We
        // instead wait out the delay (requesting repaints so idle frames
        // still tick) and do the handoff ONCE, after the level change has
        // settled. Doing it once avoids spamming the synthetic ALT keystroke
        // `bring_hwnd_to_front` uses to defeat focus-stealing prevention.
        if let Some((hwnd, delay)) = self.pin_pending_yield {
            if delay > 0 {
                self.pin_pending_yield = Some((hwnd, delay - 1));
                ctx.request_repaint();
            } else {
                if hwnd != 0 {
                    let _ = crate::process_list::bring_hwnd_to_front(hwnd);
                } else if let Some(me) = self.self_hwnd {
                    crate::process_list::yield_foreground_below(me);
                }
                self.pin_pending_yield = None;
            }
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
                let defaults = self.nav_device_defaults();
                let snarl = &self.tabs[self.active_tab].canvas.snarl;
                build_processing_graph(snarl, defaults)
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
        //
        // `try_lock` instead of `lock` — the proc thread holds the same
        // mutex while writing each catchup batch; if we'd block here a
        // slow paint frame snowballs into mutex-stall amplification.
        // Skipping the drain costs one frame of display staleness
        // (≤16 ms, imperceptible) and the data arrives on the next
        // frame instead.
        //
        // IMPORTANT: this profile_scope! is inside an explicit block.
        // A bare `profile_scope!(...)` at function-body scope binds the
        // RAII guard to function exit, so the "pull_outputs_and_display"
        // timing would include canvas_show and everything else below
        // — a misleading number we had to debug once already.
        {
        puffin::profile_scope!("pull_outputs_and_display");
        self.eval_cache.clear();
        if canvas_has_nodes {
            let drained = {
                puffin::profile_scope!("drain_proc_outputs");
                match self.proc_outputs.try_lock() {
                    Ok(mut out) => {
                        for (&(uid, pin), &sig) in &out.node_outputs {
                            self.eval_cache.insert((NodeId(uid), pin), sig);
                        }
                        let last     = std::mem::take(&mut out.last_inputs);
                        let last_out = std::mem::take(&mut out.last_outputs);
                        let scopes   = std::mem::take(&mut out.scope_pending);
                        Some((last, last_out, scopes))
                    }
                    Err(_) => None,
                }
            };
            if let Some((last_inputs_snap, last_outputs_snap, scope_batch)) = drained {
                let scope_count = scope_batch.len();
                let li_count = last_inputs_snap.len();
                let lo_count = last_outputs_snap.len();
                puffin::profile_scope!("display_state_sizes",
                    format!("scope={} li={} lo={}", scope_count, li_count, lo_count));
                let mut scope_lookup: HashMap<usize, Vec<Vec<Option<f32>>>> = {
                    puffin::profile_scope!("scope_lookup_build");
                    let mut m: HashMap<usize, Vec<Vec<Option<f32>>>> = HashMap::new();
                    for (uid, sample) in scope_batch {
                        m.entry(uid).or_default().push(sample);
                    }
                    m
                };
                {
                    puffin::profile_scope!("apply_display_state");
                    apply_display_state(
                        &mut self.tabs[self.active_tab].canvas.snarl,
                        None,
                        &last_inputs_snap,
                        &last_outputs_snap,
                        &mut scope_lookup,
                    );
                }
            }
        }
        } // end pull_outputs_and_display scope

        // Signal routing and device flushing are handled by the I/O thread.
        // panic_active is already folded into effective_bypass above so the
        // tab-bar indicator and the I/O thread stay in sync from the same source.
        self.io_bypass.store(effective_bypass[self.active_tab], Ordering::Relaxed);

        // ── Custom title bar ──────────────────────────────────────────────────────
        let mut do_save = false;
        let mut do_load = false;
        let mut do_save_workspace = false;
        let mut do_load_workspace = false;
        let mut do_new  = false;
        let mut do_close = false;
        let mut do_bind  = false;
        let mut do_hidhide = false;
        let mut do_undo = false;
        let mut do_redo = false;
        let mut toggle_settings = false;
        let mut do_pin_toggle = false;
        let mut do_set_mode: Option<settings::UiMode> = None;
        let pin_active_now = self.settings.pin_active;
        let ui_mode_now = self.settings.ui_mode;
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
                    &mut do_save, &mut do_load,
                    &mut do_save_workspace, &mut do_load_workspace,
                    &mut do_new, &mut do_close, &mut do_bind,
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
                    ui_mode_now,
                    &mut do_set_mode,
                );
            });
        if toggle_settings {
            self.settings_open = !self.settings_open;
        }
        if do_pin_toggle {
            self.toggle_pin(ctx);
        }
        if let Some(new_mode) = do_set_mode {
            if self.settings.ui_mode != new_mode {
                self.settings.ui_mode = new_mode;
                settings::save_settings(&self.settings);
            }
        }

        // ── Tab bar ───────────────────────────────────────────────────────────────
        let tab_bar_frame = egui::Frame::NONE.fill(ctx.style().visuals.widgets.noninteractive.bg_fill);
        let tab_actions = egui::TopBottomPanel::top("tab_bar")
            .exact_height(32.0)
            .frame(tab_bar_frame)
            .show_separator_line(false)
            .show(ctx, |ui| show_tab_bar(ui, &self.tabs, self.active_tab, &effective_bypass, &mut self.auto_switch))
            .inner;
        let TabBarActions {
            switch_to: tab_switch,
            close_idx: tab_close_idx,
            new_tab: tab_new,
            bypass_toggle: bypass_toggle_idx,
            do_save: tab_save,
            do_load: tab_load,
            do_save_workspace: tab_save_ws,
            do_load_workspace: tab_load_ws,
            do_bind: tab_bind,
            do_close: tab_close,
        } = tab_actions;
        do_new   = do_new   || tab_new;
        do_save  = do_save  || tab_save;
        do_load  = do_load  || tab_load;
        do_save_workspace = do_save_workspace || tab_save_ws;
        do_load_workspace = do_load_workspace || tab_load_ws;
        do_bind  = do_bind  || tab_bind;
        do_close = do_close || tab_close;
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
            // Open a transient handle for the window's lifetime (released when it
            // closes — see the release guard before the window render).
            self.hidhide = HidHideClient::try_open();
            if let Some(hh) = &self.hidhide {
                self.hidhide_whitelist = hh.whitelist();
            }
        }

        // ── Bind-to-process window ────────────────────────────────────────────────
        self.draw_bind_window(ctx);

        // Release the transient HidHide handle whenever the window isn't open —
        // holding it would block the elevated helper from opening the control device.
        if !self.hidhide_window_open && self.hidhide.is_some() {
            self.hidhide = None;
        }

        // ── HidHide configuration window ──────────────────────────────────────────
        self.draw_hidhide_window(ctx);

        // ── Settings window ───────────────────────────────────────────────────
        self.draw_settings_window(ctx);
        self.draw_gp_settings_panel(ctx);
        // Auto-close the picker when a Touch Zones assignment finishes: the card
        // renderer commits the mapping (or Cancel fires) by resetting the node's
        // `_tz_phase` out of "captured". Watching that here means the picker
        // closes on Add without a viewer→app close bridge. Only applies to the
        // TZ picker variant; the Remapper's picker is closed by its own Done.
        if self.gamepad_nav.kbm_picker_open && self.gamepad_nav.kbm_picker_touch_zones {
            let path = self.gamepad_nav.kbm_picker_path.clone();
            if let Some(inner) = self.gamepad_nav.kbm_picker_node {
                let phase = self.picker_target_param_str(&path, inner, "_tz_phase");
                if phase.as_deref() != Some("captured") {
                    self.gamepad_nav.kbm_picker_open = false;
                    self.gamepad_nav.kbm_picker_viewport = None;
                }
            }
        }

        // KB/M picker: drawn here only when the main window owns the session.
        // Editor-owned sessions render inside their own viewport (see
        // show_subpatch_editors); if the owning editor has closed, fall back to
        // the main window so the modal can't become unreachable.
        if let Some(vp) = self.gamepad_nav.kbm_picker_viewport {
            let owner_open = self.sub_patch_editors.iter().any(|e| egui::ViewportId::from_hash_of(
                ("subpatch_editor", e.tab_idx, e.node_id.0)) == vp);
            if !owner_open { self.gamepad_nav.kbm_picker_viewport = None; }
        }
        if self.gamepad_nav.kbm_picker_viewport.is_none() {
            self.draw_kbm_picker(ctx);
        }
        self.draw_press_mode_picker(ctx);
        self.draw_reinstall_confirm(ctx);
        self.draw_uninstall_confirm(ctx);
        // Modal device-op overlay — painted last so it sits above everything and
        // swallows input while a create/remove/reinstall is in flight.
        self.draw_device_op_overlay(ctx);
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
            // references. Teardown runs on the worker (off the UI thread) since
            // a HIDMaestro device's Drop calls the blocking helper destroy.
            prune_devices_async(
                &self.shared_virtual_devices,
                &mut self.pending_device_ids,
                &self.device_ops.tx,
                &self.tabs,
            );
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

        // Keyboard Undo / Redo in Easy mode. Advanced mode handles Ctrl+Z /
        // Ctrl+Shift+Z inside `Canvas::show()`, but that path doesn't run in
        // Easy mode (the central panel returns early before `canvas::show`), so
        // the shortcut would otherwise be dead there even though pinned-widget
        // edits are now undoable. Skip when a text field holds focus so typing
        // (pinned Label text, preset rename) isn't hijacked.
        if self.settings.ui_mode == settings::UiMode::Easy && !ctx.wants_keyboard_input() {
            let (kz, ksz) = ctx.input(|i| {
                let z = i.events.iter().any(|e| matches!(e,
                    egui::Event::Key { key: egui::Key::Z, pressed: true, modifiers, .. }
                    if modifiers.ctrl && !modifiers.shift));
                let sz = i.events.iter().any(|e| matches!(e,
                    egui::Event::Key { key: egui::Key::Z, pressed: true, modifiers, .. }
                    if modifiers.ctrl && modifiers.shift));
                (z, sz)
            });
            if kz { do_undo = true; }
            if ksz { do_redo = true; }
        }

        // Undo / Redo from title bar buttons (and the Easy-mode keyboard path).
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
            let preset_path = self.tabs[self.active_tab]
                .easy_state.loaded_preset.as_ref().map(|(p, _)| p.clone());
            let overlay = self.tabs[self.active_tab].overlay.clone();
            if let Some(saved_path) = self.tabs[self.active_tab].canvas
                .save_patch(vids, bound, auto_bypass, preset_path, overlay)
            {
                let tab = &mut self.tabs[self.active_tab];
                tab.title = saved_path.file_stem()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_else(|| "Untitled".to_string());
                tab.file_path = Some(saved_path);
            }
        }
        if do_load {
            if let Some((new_canvas, vids, bound, auto_bypass, path, preset_path, overlay)) =
                crate::canvas::Canvas::load_patch()
            {
                // If the loaded file was a .fxsp wrapped into an
                // Easy-shaped canvas (single subpatch node) AND the
                // sub-patch declares the AutoMap inlet/outlet pair
                // plus has a non-empty layout, flip to Easy mode so
                // the user lands in the preset-driven UI directly.
                let loaded_was_fxsp = path.extension()
                    .and_then(|s| s.to_str())
                    .map(|s| s.eq_ignore_ascii_case("fxsp"))
                    .unwrap_or(false);
                if loaded_was_fxsp && is_easy_compatible_canvas(&new_canvas)
                    && self.settings.ui_mode != settings::UiMode::Easy
                {
                    self.settings.ui_mode = settings::UiMode::Easy;
                    settings::save_settings(&self.settings);
                }
                let tab = &mut self.tabs[self.active_tab];
                tab.canvas = new_canvas;
                clear_canvas_capture_state(&mut tab.canvas);
                tab.title = path.file_stem()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_else(|| "Untitled".to_string());
                tab.file_path = Some(path);
                tab.bound_exes = bound;
                tab.auto_bypass = auto_bypass;
                tab.overlay = overlay;
                // Restore Easy-mode preset link: rederive content hash
                // from the live sub-patch. If the saved path is gone,
                // EasyState will fall back to hash-matching against the
                // current preset index (see center_panel::restore_link).
                tab.easy_state.loaded_preset = preset_path.map(|p| (p, 0));
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
                reconcile_shared_devices(
                    &self.shared_virtual_devices,
                    &mut self.pending_device_ids,
                    &self.failed_device_ids,
                    &self.device_ops.tx,
                    &needed,
                );
                // Active-tab canvas changed — refresh the I/O filter so the
                // new tab's devices start receiving signals this frame.
                self.refresh_active_tab_device_ids();
                // Prune any devices the previous canvas needed but the new
                // one (and no other tab) does.
                prune_devices_async(
                    &self.shared_virtual_devices,
                    &mut self.pending_device_ids,
                    &self.device_ops.tx,
                    &self.tabs,
                );
            }
        }

        // ── Save workspace (full tab set) ────────────────────────────────────
        // Mirrors the auto-persisted `workspace.json` path but lets the user
        // pick the destination — handy for A/B perf comparisons by quickly
        // swapping between an empty workspace and a loaded one without
        // restarting the app.
        if do_save_workspace {
            if let Some(path) = crate::overlay::with_overlay_not_topmost(|| {
                rfd::FileDialog::new()
                    .add_filter("FlexInput Workspace", &["fxw", "json"])
                    .set_file_name("workspace.fxw")
                    .save_file()
            }) {
                let ws = self.build_persisted_workspace();
                if let Err(e) = settings::save_workspace_to(&ws, &path) {
                    eprintln!("[workspace] save failed: {e}");
                }
            }
        }

        // ── Load workspace (full tab set, replacing current state) ───────────
        if do_load_workspace {
            if let Some(path) = crate::overlay::with_overlay_not_topmost(|| {
                rfd::FileDialog::new()
                    .add_filter("FlexInput Workspace", &["fxw", "json"])
                    .pick_file()
            }) {
                if let Some(ws) = settings::load_workspace_from(&path) {
                    let on_load_view = match self.settings.on_patch_load {
                        settings::OnPatchLoad::Off       => None,
                        settings::OnPatchLoad::Center    => Some(crate::canvas::PendingViewAction::Center),
                        settings::OnPatchLoad::ZoomToFit => Some(crate::canvas::PendingViewAction::ZoomToFit),
                    };
                    let new_tabs: Vec<PatchTab> = ws.tabs.into_iter().map(|pt| {
                        let mut canvas = Canvas::new();
                        canvas.snarl = pt.snarl;
                        crate::canvas::migrate_loaded_snarl(&mut canvas.snarl);
                        clear_canvas_capture_state(&mut canvas);
                        canvas.pending_view_action = on_load_view;
                        // Keep each tab's view independent; allocate a fresh
                        // salt for legacy (`0`) workspaces. See startup loader.
                        let view_salt = if pt.view_salt != 0 { pt.view_salt } else { crate::canvas::next_canvas_salt() };
                        canvas.set_view_salt(view_salt);
                        let easy_state = EasyState {
                            loaded_preset: pt.easy_preset_path.map(|p| (p, 0)),
                            ..EasyState::default()
                        };
                        PatchTab {
                            title: pt.title,
                            file_path: pt.file_path,
                            bound_exes: pt.bound_exes,
                            canvas,
                            virtual_panel: VirtualDevicePanel::new(),
                            bypassed: false,
                            auto_bypass: pt.auto_bypass,
                            easy_state,
                            view_salt,
                            overlay: pt.overlay,
                        }
                    }).collect();
                    if !new_tabs.is_empty() {
                        self.tabs = new_tabs;
                        self.active_tab = ws.active_tab.min(self.tabs.len() - 1);
                        // Rebuild the shared virtual-device pool from the new
                        // tab set; prune what's no longer needed. Both go through
                        // the worker so the reload never blocks on helper IPC.
                        let mut needed: Vec<String> = Vec::new();
                        for tab in &self.tabs {
                            for v in snarl_virtual_device_ids(&tab.canvas.snarl) {
                                if !needed.iter().any(|x| x == &v) { needed.push(v); }
                            }
                        }
                        reconcile_shared_devices(
                            &self.shared_virtual_devices,
                            &mut self.pending_device_ids,
                            &self.failed_device_ids,
                            &self.device_ops.tx,
                            &needed,
                        );
                        prune_devices_async(
                            &self.shared_virtual_devices,
                            &mut self.pending_device_ids,
                            &self.device_ops.tx,
                            &self.tabs,
                        );
                        self.refresh_active_tab_device_ids();
                    }
                } else {
                    eprintln!("[workspace] load failed: file missing or invalid JSON");
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
        // Device ids that are FlexInput's own virtual pads. Two-tier: HIDMaestro
        // (PS) pads via the path-classified `:v` gilrs id, plus ViGEm
        // (xinput/ds4) pads via the pool-count fallback (no HID path) — see
        // `own_virtual_device_ids`. Used both to filter the physical list (below)
        // and to gray out their nav toggle (loopback feedback guard).
        let nav_excluded_ids = self.own_virtual_device_ids();
        let devices_owned;
        let devices: &[PhysicalDevice] = if self.settings.show_own_virtuals_as_physical {
            &self.devices
        } else {
            // Drop FlexInput's OWN emulated devices so they don't clutter the
            // physical list. A real controller is never dropped: PS pads are
            // path-classified (not plug-order), and the ViGEm fallback only
            // claims as many as we actually created.
            devices_owned = self.devices.iter()
                .filter(|d| !nav_excluded_ids.contains(&d.id))
                .cloned()
                .collect::<Vec<_>>();
            &devices_owned
        };
        // Belt-and-suspenders: ensure any excluded device is OFF in the nav map
        // (covers the case where the global default flipped it on before the
        // device was recognized as a loopback virtual).
        for id in &nav_excluded_ids {
            if let Some(v) = self.gamepad_nav.mode.get_mut(id) { *v = false; }
        }
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
        let ping_requests_for_panel = Arc::clone(&self.ping_requests);
        let easy_mode = self.settings.ui_mode == settings::UiMode::Easy;

        // Gamepad-nav legend bar — declared before the physical-devices panel so
        // it docks outermost/lowest at the very bottom. Drawn here (before the
        // `&mut self.gamepad_nav` disjoint borrow below) as a read-only `&self`
        // call. Visible only while a nav-enabled gamepad drives the UI.
        self.draw_gp_legend_bar(ctx);

        let device_defaults_for_easy = self.nav_device_defaults();
        let user_presets_folder = self.settings.user_presets_folder.clone();
        // Snapshots needed for the inner sub-patch render in Easy
        // center panel (mirrors what show_subpatch_editors passes to
        // inner_canvas.show in the Sub-Patch editor window).
        let descriptors_for_easy = self.descriptors.clone();
        let live_signals_for_easy = self.last_signals.clone();
        let panic_shortcut_for_easy = self.panic_shortcut.clone();
        // Pull favorites out so easy can mutate; we'll write back +
        // persist if the user starred or reordered something.
        let mut favorites_for_easy = self.settings.favorite_presets.clone();
        let favorites_before = favorites_for_easy.clone();
        let nav_mode_default = self.settings.gamepad_ui_nav_default;
        // Disjoint borrow: `tab` below borrows only `self.tabs`, so this
        // independent borrow of `self.gamepad_nav` is allowed. io_panel takes
        // `&mut .mode`; center_panel takes the whole `&mut gamepad_nav` for
        // preset-dropdown navigation.
        let gamepad_nav = &mut self.gamepad_nav;
        let tab = &mut self.tabs[self.active_tab];
        let (virtual_panel, canvas, easy_state) =
            (&mut tab.virtual_panel, &mut tab.canvas, &mut tab.easy_state);

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
        // Built inline (not via `nav_device_defaults`) because a whole-`self`
        // method call can't coexist with the `&mut self.gamepad_nav` above;
        // field reads on `self.settings` can.
        let device_defaults = crate::canvas::DeviceParamDefaults {
            stick_deadzone: self.settings.default_stick_deadzone,
            gyro_mult: self.settings.default_gyro_mult,
            mouse_sensitivity: self.settings.default_mouse_sensitivity,
            rumble_floor: self.settings.default_rumble_floor,
            rumble_max: self.settings.default_rumble_max,
            rumble_exp: self.settings.default_rumble_exp,
        };

        // Both side panels are declared unconditionally so egui's
        // remembered panel stack ordering is stable across Easy ↔
        // Advanced toggles. In Easy mode the panel bodies are no-ops
        // (height collapses to a zero-content frame) and the floating
        // heading tabs are hidden.
        let top_resp = egui::TopBottomPanel::top("virtual_devices_panel")
            .resizable(false)
            .exact_height(if easy_mode { 0.0 } else { virt_h })
            .frame(if easy_mode { collapsed_frame } else { top_frame })
            .show(ctx, |ui| {
                if !easy_mode && virt_open > 0.01 {
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
            .exact_height(if easy_mode { 0.0 } else { phys_h })
            .frame(if easy_mode { collapsed_frame } else { bot_frame })
            .show(ctx, |ui| {
                if !easy_mode && phys_open > 0.01 {
                    physical_devices::show(ui, devices, canvas, default_collapsed, device_defaults);
                }
            });
        if !easy_mode && phys_open > 0.99 {
            self.bottom_panel_height = bottom_resp.response.rect.height();
        }

        if !easy_mode {
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
            if easy_mode {
                // Two-pane Easy layout built INSIDE the CentralPanel
                // (rather than using SidePanels) so the egui panel
                // topology — and the layer ordering the snarl canvas
                // relies on for set_sublayer — stays identical to
                // Advanced mode. Allocating fixed-width child UIs
                // sidesteps the "Background vs Middle" sublayer panic
                // that toggling SidePanel registrations triggers.
                //
                // Left panel: input/output picker (scrollable gamepad
                // list on top, output section on bottom). Central:
                // sub-patch preset picker + body.
                let total = ui.available_rect_before_wrap();
                // `total` is inset from the window edge by the central
                // panel's inner margin (~8px). The visible window edge
                // sits in that surround ring. Compute the full content
                // bounds (ring included) so (a) the surround doesn't go
                // fully transparent in see-through mode, and (b) the
                // inner shadow hugs the real window edge with no offset.
                let margin = style_snapshot.spacing.window_margin.left as f32;
                let outer_total = total.expand(margin.max(8.0));
                // Stash the FULL content area (everything below the tab
                // strip, out to the window edge) so the post-frame inner-
                // shadow pass hugs the real edge rather than the margin-
                // inset content rect.
                ctx.data_mut(|d| d.insert_temp(
                    egui::Id::new(INNER_SHADOW_RECT_KEY), outer_total));
                // Fixed left-panel width so cards / chips don't restyle
                // as the user resizes the window. Only shrinks if the
                // window itself is narrower than this baseline.
                let side_w_full = 280.0_f32.min(total.width() * 0.5);
                // Collapse animation: 1.0 = fully open, 0.0 = folded away left.
                // `side_w` is the on-screen SLICE width; the panel itself keeps its
                // full width and SLIDES left (translated + clipped) so its contents
                // move out of frame rather than being squeezed into nothing.
                let left_open = ctx.animate_bool_with_time(
                    egui::Id::new("easy_left_open_anim"),
                    !self.easy_left_panel_collapsed,
                    0.18,
                );
                let side_w = side_w_full * left_open;
                let left_visible = left_open > 0.01;
                let gap = 6.0_f32 * left_open;
                // Visible slice (also the clip + dark-fill + tab-anchor rect).
                let left_rect = egui::Rect::from_min_size(
                    total.min,
                    egui::vec2(side_w, total.height()),
                );
                // Full-width panel, translated left so its RIGHT edge lands on the
                // slice's right edge; the part left of `total.min.x` is clipped off
                // frame. At open=1 it coincides with `left_rect`.
                let panel_full_rect = egui::Rect::from_min_size(
                    egui::pos2(total.min.x + side_w - side_w_full, total.min.y),
                    egui::vec2(side_w_full, total.height()),
                );
                let center_rect = egui::Rect::from_min_size(
                    egui::pos2(total.min.x + side_w + gap, total.min.y),
                    egui::vec2((total.width() - side_w - gap).max(0.0), total.height()),
                );
                // See-through handling for Easy mode. In opaque mode the
                // left panel uses its solid dark fill and the central
                // area inherits the (opaque) CentralPanel frame. In
                // see-through mode BOTH backgrounds get the user-chosen
                // alpha so the desktop bleeds through evenly — the
                // central frame fill was set TRANSPARENT above, so we
                // paint the alpha-faded central fill ourselves here,
                // mirroring how the Advanced canvas fades its bg_frame.
                let st_alpha: f32 = ctx.data(|d|
                    d.get_temp::<f32>(egui::Id::new(crate::canvas::SEE_THROUGH_ALPHA_KEY))
                ).unwrap_or(1.0);
                let left_fill = if see_through_on {
                    let a = (st_alpha.clamp(0.0, 1.0) * 255.0).round() as u8;
                    egui::Color32::from_rgba_unmultiplied(0x1a, 0x1a, 0x1a, a)
                } else {
                    egui::Color32::from_rgb(0x1a, 0x1a, 0x1a)
                };
                if see_through_on {
                    // Fill the FULL surround ring (out to the window edge)
                    // with the alpha-faded panel fill first, so the
                    // central-panel inner margin doesn't read as a fully-
                    // transparent border. The per-panel fills below paint
                    // on top of this, matching opacity throughout.
                    let base = style_snapshot.visuals.extreme_bg_color;
                    let a = (st_alpha.clamp(0.0, 1.0) * 255.0).round() as u8;
                    ui.painter().rect_filled(
                        outer_total, 0.0,
                        egui::Color32::from_rgba_unmultiplied(
                            base.r(), base.g(), base.b(), a),
                    );
                    // Central area: alpha-faded panel fill so the
                    // sub-patch body floats over a translucent surface
                    // instead of a fully-transparent void.
                    ui.painter().rect_filled(
                        center_rect, 0.0,
                        egui::Color32::from_rgba_unmultiplied(
                            base.r(), base.g(), base.b(), a),
                    );
                }
                // Darker panel background fill so the left panel
                // visually groups itself apart from the central canvas.
                // Skipped once folded away (zero width) so nothing renders.
                if left_visible {
                    ui.painter().rect_filled(left_rect, 0.0, left_fill);
                    // Lay the panel out at FULL width in `panel_full_rect` (so cards
                    // never reflow), but clip to the visible slice — the content
                    // then slides left out of frame as the panel folds.
                    ui.scope_builder(egui::UiBuilder::new().max_rect(panel_full_rect), |ui| {
                        ui.set_clip_rect(left_rect);
                        puffin::profile_scope!("easy_io_panel_show");
                        crate::easy::io_panel::show(
                            ui,
                            devices,
                            canvas,
                            &shared_pool_for_panel,
                            default_collapsed,
                            device_defaults_for_easy,
                            &mut calibrate_request,
                            &device_rates_snap,
                            &ping_requests_for_panel,
                            &mut gamepad_nav.mode,
                            nav_mode_default,
                            &nav_excluded_ids,
                        );
                    });
                }

                // Floating "Devices" tab: hangs off the left panel's right edge
                // onto the preset bar, aligned with the preset-dropdown row. Rides
                // the panel as it collapses and pins at the window's top-left as
                // the re-open button. Publish its right edge so the center panel
                // clamps the "Preset:" label off it.
                let (tab_resp, tab_rect) = crate::panels::physical_devices::draw_devices_tab(
                    ctx,
                    "easy_devices_tab",
                    "Devices",
                    left_rect.right(),
                    // Match the preset pill's top: center content top + add_space(6)
                    // + the (34-row − 30-pill)/2 centering inset.
                    total.min.y + 8.0,
                );
                if tab_resp.clicked() {
                    self.easy_left_panel_collapsed = !self.easy_left_panel_collapsed;
                }
                ctx.data_mut(|d| d.insert_temp(
                    egui::Id::new("easy_devices_label_right_x"), tab_rect.right()));

                ui.scope_builder(egui::UiBuilder::new().max_rect(center_rect), |ui| {
                    puffin::profile_scope!("easy_center_panel_show");
                    crate::easy::center_panel::show(
                        ui,
                        canvas,
                        easy_state,
                        user_presets_folder.as_deref(),
                        device_defaults_for_easy,
                        &descriptors_for_easy,
                        devices,
                        &live_device_ids,
                        &live_signals_for_easy,
                        &panic_shortcut_for_easy,
                        &device_rates_snap,
                        &mut favorites_for_easy,
                        gamepad_nav,
                    );
                    // Keep the Easy I/O nodes arranged against the sub-patch using
                    // its REAL rendered size (just measured by the canvas show
                    // above via final_node_rect). Re-running each frame lets a
                    // freshly-deployed node converge to its measured size, and
                    // reflows automatically when the Layout editor resizes the
                    // sub-patch. Easy-mode only (this branch); Advanced never runs
                    // it, so manual node drags there are preserved.
                    crate::easy::layout::reposition_io_nodes_with_ctx(canvas, Some(ui.ctx()));
                });
                if favorites_for_easy != favorites_before {
                    self.settings.favorite_presets = favorites_for_easy;
                    settings::save_settings(&self.settings);
                }
                return;
            }
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
            // Stash the canvas content area so the post-frame inner-
            // shadow pass scopes its gradient to the canvas rather than
            // the whole window (Advanced mode).
            ctx.data_mut(|d| d.insert_temp(
                egui::Id::new(INNER_SHADOW_RECT_KEY), ui.max_rect()));
            puffin::profile_scope!("canvas_show");
            calibrate_request = crate::panels::canvas::show(
                canvas, &self.descriptors, &live_device_ids, &self.last_signals,
                &self.panic_shortcut, devices, &device_rates_snap,
                device_defaults, ui, &ping_requests_for_panel,
            );
        });
        if let Some(node) = calibrate_request {
            self.calibration_open.insert(node);
        }
        {
            puffin::profile_scope!("calibration_show_windows");
            // Hands-free gyro auto-calibration watcher — runs whether or not
            // a calibration window is open.
            crate::panels::calibration::auto_cal_tick(ctx, canvas, &self.last_signals);
            crate::panels::calibration::show_windows(ctx, canvas, &mut self.calibration_open, &self.last_signals, &self.scope_taps, &self.spike_filter_settings);
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

        // A Special… button on a top-level (or main-canvas sub-patch) Remapper/
        // Lean body requested the shared picker. The request carries its own
        // `outer` addressing; editor-viewport requests are handled inside
        // show_subpatch_editors. Deferred to here so the `canvas` borrow above
        // has ended before this `&mut self` call.
        if let Some(req) = crate::canvas::viewer::take_special_picker_request(ctx) {
            self.open_special_picker(req, None);
        }

        // Overlay pick: a click on an exposable element of the MAIN canvas
        // (armed amber by the overlay's "Add element"). Drained here — before
        // the sub-patch editors run — so an editor drain can't misattribute
        // a top-level click. Path `[]` = tab canvas.
        if crate::canvas::viewer::overlay_pick_active(ctx) {
            if let Some((inner_uid, eid, size)) =
                crate::canvas::viewer::take_overlay_pick_pending(ctx)
            {
                crate::canvas::viewer::put_overlay_pick_result(ctx, vec![], inner_uid, eid, size);
            }
            // Esc in the main window cancels the pick back to overlay edit.
            if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
                crate::canvas::viewer::set_overlay_pick_active(ctx, false);
            }
        }

        // ── Sub-patch editor windows ──────────────────────────────────────────
        {
            puffin::profile_scope!("show_subpatch_editors");
            show_subpatch_editors(self, ctx, &live_device_ids);
        }

        // ── Info overlay ──────────────────────────────────────────────────────
        // Transparent click-through viewport; paces its own repaint, so it
        // stays smooth even when the bg-throttle branch below asks for a
        // slower cadence (egui keeps the earliest requested deadline).
        {
            puffin::profile_scope!("show_overlay");
            crate::overlay::show_overlay(self, ctx);
        }

        // ── Virtual Menu overlay ──────────────────────────────────────────────
        // A SECOND transparent viewport, summoned by open menus (eval-side
        // truth) or a menu's position-edit mode; fully independent of the info
        // overlay's visibility.
        {
            puffin::profile_scope!("show_menu_overlay");
            crate::menu_overlay::show_menu_overlay(self, ctx);
        }

        // ── Config overlay ────────────────────────────────────────────────────
        // A THIRD transparent viewport (M3): shortcut-summoned, interactive over
        // its panel + click-through elsewhere, for live parameter tweaking.
        {
            puffin::profile_scope!("show_config_overlay");
            crate::config_overlay::show_config_overlay(self, ctx);
        }

        // Repaint scheduling:
        //   Focused → vsync (or 100 ms fallback for empty patch). The user
        //       is actively using the app; smoothness wins. Animated
        //       widgets (scopes, glow) can request vsync freely — they
        //       go through request_repaint_throttled() but suppression
        //       is off when focused so the request lands.
        //   Background → user's chosen bg_repaint_hz (clamped 1..30).
        //       REPAINT_SUPPRESSED is true above, so widget-internal
        //       repaints are short-circuited, and the only request that
        //       lands is this one, which dictates the rate.
        let has_virtual = !self.active_tab_device_ids.read().unwrap().is_empty();
        let bg_hz = self.settings.bg_repaint_hz
            .clamp(settings::BG_REPAINT_HZ_MIN, settings::BG_REPAINT_HZ_MAX);
        let bg_interval_ms = 1000_u64 / bg_hz as u64;
        if bg_throttle {
            ctx.request_repaint_after(std::time::Duration::from_millis(bg_interval_ms));
        } else if canvas_has_nodes || has_virtual {
            ctx.request_repaint();
        } else {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }
        // Sanity log when the setting changes — eyeballing whether the
        // slider is actually wired through. Throttle to one print per change.
        #[cfg(debug_assertions)]
        {
            if self.last_logged_repaint_hz != Some(bg_hz) {
                eprintln!("[settings] bg_repaint_hz = {} (interval = {} ms)",
                    bg_hz, bg_interval_ms);
                self.last_logged_repaint_hz = Some(bg_hz);
            }
        }

        // ── Gamepad-nav overlays: cursor ─────────────────────────────────
        self.draw_nav_cursor(ctx);

        // ── Inner edge shadow ────────────────────────────────────────────
        // A pronounced gradient darkening at all four window edges,
        // fading to transparent toward the center. Grounds the
        // window's dimensions — especially useful in see-through mode
        // where the minimalist surfaces can blur into the desktop, but
        // it's a subtle depth cue in opaque mode too. Painted on a
        // foreground layer UNDER the 1px border so the border still
        // reads as the crisp outer edge.
        paint_inner_window_shadow(ctx);

        // ── Window border ────────────────────────────────────────────────
        // We turned off OS decorations (`with_decorations(false)`) so
        // paint a 1 px subtle border ourselves. Skipped while
        // maximized so the window snaps cleanly to monitor edges.
        let maximized = ctx.input(|i| i.viewport().maximized.unwrap_or(false));
        if !maximized {
            let rect = ctx.content_rect();
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
        // every edit path converges on a consistent pool. Both reconcile and
        // prune enqueue work on the device-ops worker — never blocking IPC on
        // the UI thread. `pending_device_ids` keeps the per-frame reconcile from
        // re-enqueueing a create that's still in flight; prune only fires for
        // devices actually present in the pool, so it can't double-send either.
        {
            let needed: Vec<String> = self.tabs.iter()
                .flat_map(|t| snarl_virtual_device_ids(&t.canvas.snarl))
                .collect();
            // A failed id whose node was removed should be retryable on re-add:
            // drop it from the failed set once it's no longer referenced.
            self.failed_device_ids.retain(|id| needed.iter().any(|n| n == id));
            reconcile_shared_devices(
                &self.shared_virtual_devices,
                &mut self.pending_device_ids,
                &self.failed_device_ids,
                &self.device_ops.tx,
                &needed,
            );
            prune_devices_async(
                &self.shared_virtual_devices,
                &mut self.pending_device_ids,
                &self.device_ops.tx,
                &self.tabs,
            );
        }
        // Refresh the I/O thread's active-tab device id filter.
        self.refresh_active_tab_device_ids();

        // Crash-recovery autosave. Runs last, after every edit path has
        // converged for the frame, and no-ops unless a settled edit changed
        // persistent state since the previous write (see the method docs).
        self.maybe_write_recovery_snapshot();
    }

    /// Called by eframe just before the application exits. Persist workspace
    /// (if opted in) and settings here.
    fn on_exit(&mut self) {
        settings::save_settings(&self.settings);
        self.save_workspace_now();
        // Clean exit — discard the crash-recovery snapshot so the next launch
        // starts fresh. It only survives an *abnormal* exit (GPU-loss relaunch
        // or hard crash), which is exactly when we want to restore from it.
        settings::delete_recovery();

        // "Keep virtual controllers alive": before the device pool is dropped,
        // tell each device to relinquish its OS node so Drop won't tear it down.
        // Without this, HidMaestroDevice::drop unconditionally calls
        // helper::destroy on a clean exit — removing the very nodes the persist
        // setting promises to keep for reclaim next launch. The helper's own
        // parent-death/exit teardown already respects persist; this closes the
        // app-side Drop path that bypassed it.
        #[cfg(windows)]
        if self.settings.persist_virtual_devices {
            if let Ok(mut pool) = self.shared_virtual_devices.lock() {
                for dev in pool.iter_mut() {
                    dev.persist_on_drop();
                }
            }
        }
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

/// What kind of gamepad interaction the selected sub-patch widget supports.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum NavWidgetKind {
    /// Not interactable (label, graph, svg, …).
    None,
    /// Knob / constant — South enters value-edit, dpad/stick adjusts.
    Value,
    /// Dropdown — South enters, dpad/stick cycles options.
    Dropdown,
    /// Switch — South toggles directly (no edit mode).
    Toggle,
    /// Response curve — RT adds a dot, LT deletes a dot (direct, no edit mode).
    Curve,
    /// Remapper / Map Action — LT/RT cycle the mapping filter (direct).
    Remapper,
    /// Multi-control row — South enters; left/right pick a field; up/down/stick
    /// edit it. Covers gyro rows, curve option rows, counter min/max, etc. Also
    /// used for single-field generic widgets (one field).
    MultiField,
    /// Touch Zones pad (the pinned "field" element) — South enters line editing
    /// (`TzLines`): cycle/grab/move/recenter dividers, add/remove in mapping mode.
    TouchZones,
    /// Touch Zones mapping CARDS widget — South enters the zone-tab + Learn/
    /// Assign/Add flow (`TzCards`).
    TouchZoneCards,
}

/// Identifies a setting in the gamepad-native settings panel for get/set.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum GpSettingKey {
    PollingHz,
    SampleRateHz,
    BgRepaintHz,
    NavDefault,
    CursorMaxSpeed,
    CursorAccel,
    Contrast,
    KeepWorkspace,
    CollapseNodes,
    ShowVirtuals,
    DefDeadzone,
    DefGyroMult,
    DefMouseSens,
    ChordsNavOnly,
}

/// Stepping model for a generic numeric nav param.
#[derive(Clone, Copy, PartialEq)]
pub(crate) enum NavStep {
    /// Decade-quantized (ms / sample counts): step size grows by powers of ten
    /// with the value's magnitude. <10 → 1 (fine 0.1); 10–100 → 5 (fine 1);
    /// 100–1000 → 50 (fine 10); etc.
    Decade,
    /// Plain proportional (0..1-style params like phase): fraction of the value.
    Linear,
}

/// Numeric-edit descriptor for a generic nav-editable widget element.
#[derive(Clone, Copy)]
pub(crate) struct NavParamSpec {
    key: &'static str,
    lo: f32,
    hi: f32,
    default: f32,
    step: NavStep,
}

/// One editable field inside a (possibly multi-control) widget element. Pinned
/// rows often bundle several controls (gyro steering opts, curve range, counter
/// min/max, invert checkboxes, …). The nav editor focuses one field at a time
/// (left/right between fields) and edits it (up/down or stick). `label` shows in
/// the focus HUD.
#[derive(Clone)]
pub(crate) enum NavField {
    Value { key: &'static str, lo: f32, hi: f32, default: f32, step: NavStep },
    Enum  { key: &'static str, opts: &'static [&'static str] },
    Toggle { key: &'static str },
    /// Writes two params at once from a chosen option (label, value_a, value_b)
    /// — for the gyro mode rows that set family+axis together.
    EnumPair { key_a: &'static str, key_b: &'static str,
               opts: &'static [(&'static str, &'static str, &'static str)] },
}

#[derive(Clone)]
pub(crate) struct NavFieldDef {
    label: &'static str,
    field: NavField,
}

/// How a gamepad-settings row is edited.
pub(crate) enum GpSettingKind {
    Toggle { key: GpSettingKey },
    IntSlider { lo: f32, hi: f32, step: f32, key: GpSettingKey },
    FloatSlider { lo: f32, hi: f32, step: f32, key: GpSettingKey },
    /// Discrete cycle through a fixed list of (value, label) pairs. The
    /// underlying value is stored as `f32` (matching IntSlider) but the
    /// display string comes from the label. Used for enum settings where
    /// a slider doesn't make sense (e.g. Repaint rate: Monitor / 60 / 30 / 15 Hz).
    /// RESERVED: no settings row constructs this yet (the planned "Repaint
    /// rate" row was never added), but the nav/step/format handlers below
    /// fully support it — construct a row with it and it works.
    #[allow(dead_code)]
    Cycle { key: GpSettingKey, opts: &'static [(f32, &'static str)] },
    /// Gamepad shortcut chord: South closes the panel and starts a chord
    /// capture for `target` (so the user can press the combo with the panel
    /// out of the way). Displays the currently-assigned combo.
    ChordLearn { target: crate::gamepad_nav::ChordTarget },
}

/// One row in the gamepad-native settings panel.
pub(crate) struct GpSettingRow {
    label: String,
    kind: GpSettingKind,
    suffix: &'static str,
}

impl FlexInputApp {
    /// Per-frame gamepad UI-navigation driver. Reads the active nav device's
    /// signals, drives FlexInput's own UI (selection / edit / tabs / menus /
    /// cursor / Alt+Tab), and publishes the output-suppression flag.
    ///
    /// Runs only in Easy mode. Output suppression is gated on FlexInput holding
    /// window focus, so alt-tabbing to a game restores normal mappings.
    fn run_gamepad_nav(&mut self, ctx: &egui::Context) {
        use crate::gamepad_nav as gn;

        let raw_focused = ctx.input(|i| i.focused);
        let easy_mode = self.settings.ui_mode == settings::UiMode::Easy;
        let nav_default = self.settings.gamepad_ui_nav_default;

        // The driver also runs when the app is pinned on-top (Windows may report
        // it unfocused even though it's visible and the user is driving it) and
        // while the Alt+Tab switcher is engaged (OS Alt+Tab steals focus the
        // moment Alt presses, but we must keep holding Alt until Select is
        // released).
        //
        // NOTE: see-through is a purely visual backdrop-alpha effect — it does
        // NOT make the window click-through at the Win32 level (no
        // WS_EX_TRANSPARENT is ever applied). An earlier gate disabled nav
        // whenever pin + see-through were both on, treating see-through as a
        // pass-through intent that was never implemented. That left the window
        // pinned, on top, and visible but with nav dead. Removed.
        let focused = raw_focused || self.settings.pin_active || self.gamepad_nav.alt_tab_active;

        // Determine the active nav device. ALL nav-enabled physical gamepads can
        // drive the UI simultaneously by a last-active-input-takes-over rule:
        // each frame we pick the device showing fresh activity (a button press or
        // a stick/trigger deflection past the deadzone — gyro is EXCLUDED since
        // it's effectively always active), and keep driving the previously-active
        // device when nothing is moving (so edge detection stays stable). Devices
        // excluded: MIDI, and FlexInput's own loopback virtuals (feedback loop).
        let mut active_dev: Option<String> = None;
        let mut active_input: Option<gn::NavInput> = None;
        if focused && easy_mode {
            // FlexInput's own loopback virtuals — excluded from nav to avoid a
            // feedback loop driving the UI from our own output.
            let nav_excluded_ids = self.own_virtual_device_ids();
            // Eligible = nav-enabled, non-MIDI, not our own virtual loopback.
            let eligible: Vec<String> = self.devices.iter()
                .filter(|d| !matches!(d.kind,
                    flexinput_devices::ControllerKind::MidiIn
                    | flexinput_devices::ControllerKind::MidiOut))
                .filter(|d| !nav_excluded_ids.contains(&d.id))
                .filter(|d| *self.gamepad_nav.mode.entry(d.id.clone()).or_insert(nav_default))
                .map(|d| d.id.clone())
                .collect();

            // Find the device with fresh activity this frame (rising button OR
            // stick/trigger past deadzone). Prefer one that ISN'T the current
            // sticky device only when it's actually active, so the last input
            // wins; otherwise keep the sticky one.
            const STICK_DZ: f32 = 0.35;
            const TRIG_DZ: f32 = 0.5;
            let prev_pressed = self.gamepad_nav.prev_pressed.clone();
            let mut newly_active: Option<String> = None;
            for id in &eligible {
                let nav = gn::read_nav_input(&self.last_signals, id, &prev_pressed);
                let rising = nav.pressed.iter().any(|p| !prev_pressed.contains(p));
                let moved = nav.lstick.length() > STICK_DZ
                    || nav.rstick.length() > STICK_DZ
                    || nav.lt > TRIG_DZ || nav.rt > TRIG_DZ;
                if rising || moved { newly_active = Some(id.clone()); break; }
            }

            // Resolve the driving device: a newly-active one wins; else keep the
            // sticky device if still eligible; else seed from the tab's source or
            // the first eligible device.
            let sticky_ok = self.gamepad_nav.active_dev.as_ref()
                .map(|d| eligible.contains(d)).unwrap_or(false);
            let chosen = newly_active
                .or_else(|| if sticky_ok { self.gamepad_nav.active_dev.clone() } else { None })
                .or_else(|| {
                    // Seed: the active tab's source device if it's eligible…
                    let src = self.tabs[self.active_tab].canvas.snarl
                        .nodes_ids_data()
                        .find(|(_, n)| n.value.module_id == "device.source")
                        .and_then(|(_, n)| n.value.params.get("device_id")
                            .and_then(|v| v.as_str()).map(|s| s.to_string()));
                    match src {
                        Some(s) if eligible.contains(&s) => Some(s),
                        _ => eligible.first().cloned(),
                    }
                });

            if let Some(dev) = chosen {
                let nav = gn::read_nav_input(&self.last_signals, &dev, &self.gamepad_nav.prev_pressed);
                active_dev = Some(dev);
                active_input = Some(nav);
            }
        }

        // Suppression + capture-hold key on the resolved active nav device (the
        // physical source). Suppress mapped output while focused + nav-enabled
        // so the controller drives only the UI.
        let any_nav_enabled = active_dev.is_some();
        self.ui_nav_suppress.store(any_nav_enabled, Ordering::Relaxed);
        if let Some(dev) = &active_dev {
            let pass = ctx.cumulative_pass_nr();
            ctx.data_mut(|data| {
                data.insert_temp(egui::Id::new(("gp_nav_active", dev.clone())), pass);
                // Global flag: nav owns the sub-patch selection this frame, so
                // the body renderer must NOT clear `selected_item` (it normally
                // wipes selection every frame when not in layout-edit mode).
                data.insert_temp(egui::Id::new("gp_nav_owns_selection"), pass);
            });
        }

        let (dev_id, mut nav) = match (active_dev, active_input) {
            (Some(d), Some(n)) => (d, n),
            _ => {
                // No active device this frame. Still age out the cursor and
                // release a stuck Alt if the switcher was somehow left on.
                if self.gamepad_nav.alt_tab_active && !focused {
                    self.alt_tab_release();
                }
                if self.gamepad_nav.cursor_visible
                    && self.gamepad_nav.cursor_last_move.elapsed().as_secs_f32() > 3.0
                {
                    self.gamepad_nav.cursor_visible = false;
                }
                // No nav-enabled device at all → clear stale edge state.
                self.gamepad_nav.prev_pressed.clear();
                self.gamepad_nav.active_dev = None;
                return;
            }
        };
        // When the driving device CHANGES (another pad took over), suppress this
        // frame's rising edges so the new pad's already-held buttons don't fire
        // an action on the switch frame — the user must release+repress to act.
        // (The stick/move paths still respond immediately, which is fine.)
        let device_changed = self.gamepad_nav.active_dev.as_deref() != Some(dev_id.as_str());
        if device_changed {
            nav.rising.clear();
            self.gamepad_nav.prev_pressed = nav.pressed.clone();
        }
        // Record the active nav device so the bottom legend bar can render with
        // the right button-glyph skin this frame.
        self.gamepad_nav.active_dev = Some(dev_id.clone());

        // ── Touch Zones gamepad-learn: hold nav inert ───────────────────────
        // While a 🎮 gamepad-learn is armed on a Touch Zones card (the user is
        // demonstrating a gamepad button as the mapping OUTPUT), the raw button
        // must reach that capture, not drive navigation. The cards renderer
        // publishes a fresh pass number while armed; go inert but keep edge state
        // current so nav resumes cleanly the moment the capture latches.
        if let Some(pass) = ctx.data(|d| d.get_temp::<u64>(egui::Id::new("fxi_tz_gp_learn"))) {
            if ctx.cumulative_pass_nr().saturating_sub(pass) <= 2 {
                self.gamepad_nav.prev_pressed = nav.pressed.clone();
                self.gamepad_nav.prev_lt = nav.lt > 0.5;
                self.gamepad_nav.prev_rt = nav.rt > 0.5;
                ctx.request_repaint();
                return;
            }
        }

        let dt = ctx
            .input(|i| i.stable_dt)
            .clamp(0.001, 0.1);

        // ── Shortcut-chord toggles (see-through / panic) ─────────────────────
        // When nav-only: fire from the driving nav device here. When NOT nav-
        // only, a global scan in update() handles it (so the combo works even
        // when this device's nav toggle is off), so skip here to avoid double-
        // firing.
        if self.settings.gamepad_chords_nav_only
            && self.process_shortcut_chords(ctx, &nav)
        {
            // A shortcut combo fired this frame — consume input so its buttons
            // don't also drive navigation (e.g. Back in the combo ≠ Alt-Tab).
            self.gamepad_nav.prev_pressed = nav.pressed.clone();
            ctx.request_repaint();
            return;
        }

        // ── Alt+Tab window switcher (Select held) ───────────────────────────
        // While engaged, the controller is dedicated to the switcher.
        if self.gamepad_nav.alt_tab_active {
            self.drive_alt_tab(&dev_id, &nav);
            self.gamepad_nav.prev_pressed = nav.pressed.clone();
            ctx.request_repaint();
            return;
        }

        // ── Select / minus → Alt+Tab window switcher (hold to keep switching) ─
        // Engaged immediately on press; holds Alt while Select is held, releases
        // (commits) on release. No File menu — egui menus can't be gamepad-
        // navigated, so Select is dedicated to the OS switcher.
        if nav.is_rising("btn_back") {
            self.enter_alt_tab(&dev_id, ctx);
            self.gamepad_nav.prev_pressed = nav.pressed.clone();
            ctx.request_repaint();
            return;
        }

        // ── Start: tap = preset dropdown, hold (>250ms) = gamepad Settings ───
        if nav.is_rising("btn_start") {
            self.gamepad_nav.start_down_at = Some(std::time::Instant::now());
            self.gamepad_nav.start_hold_fired = false;
        }
        if nav.pressed.contains("btn_start") {
            if let Some(t) = self.gamepad_nav.start_down_at {
                if !self.gamepad_nav.start_hold_fired && t.elapsed().as_millis() >= 250 {
                    // Open the gamepad-native settings panel (the real Settings
                    // window can't be gamepad-navigated). Toggle so a second
                    // hold closes it.
                    self.gamepad_nav.settings_open = !self.gamepad_nav.settings_open;
                    if self.gamepad_nav.settings_open {
                        self.gamepad_nav.settings_index = 0;
                        self.gamepad_nav.settings_editing = false;
                    }
                    self.gamepad_nav.start_hold_fired = true;
                }
            }
        } else if let Some(t) = self.gamepad_nav.start_down_at.take() {
            // Released before the hold threshold → tap → toggle preset dropdown.
            if t.elapsed().as_millis() < 250 && !self.gamepad_nav.start_hold_fired {
                self.gamepad_nav.preset_nav_open = !self.gamepad_nav.preset_nav_open;
            }
        }

        // ── Gamepad settings panel (modal: captures dpad/South/East/West) ────
        if self.gamepad_nav.settings_open {
            let rt_rising = nav.rt > 0.5 && !self.gamepad_nav.prev_rt;
            let lt_rising = nav.lt > 0.5 && !self.gamepad_nav.prev_lt;
            self.nav_drive_gp_settings(&nav, rt_rising, lt_rising);
            self.gamepad_nav.prev_pressed = nav.pressed.clone();
            self.gamepad_nav.prev_rt = nav.rt > 0.5;
            self.gamepad_nav.prev_lt = nav.lt > 0.5;
            ctx.request_repaint();
            return;
        }

        // ── Virtual KB/M picker (modal: captures dpad/LS/South/North/East) ────
        if self.gamepad_nav.kbm_picker_open {
            let mut step_dir: Option<gn::NavDir> = None;
            if nav.is_rising("dpad_up") { step_dir = Some(gn::NavDir::Up); }
            else if nav.is_rising("dpad_down") { step_dir = Some(gn::NavDir::Down); }
            else if nav.is_rising("dpad_left") { step_dir = Some(gn::NavDir::Left); }
            else if nav.is_rising("dpad_right") { step_dir = Some(gn::NavDir::Right); }
            if step_dir.is_none() {
                if let Some(d) = gn::stick_dir(nav.lstick) {
                    if self.gamepad_nav.repeat_dir != Some(d) {
                        self.gamepad_nav.repeat_dir = Some(d);
                        step_dir = Some(d);
                    }
                } else {
                    self.gamepad_nav.repeat_dir = None;
                }
            }
            self.drive_kbm_picker(step_dir, &nav);
            self.gamepad_nav.prev_pressed = nav.pressed.clone();
            ctx.request_repaint();
            return;
        }

        // ── Press-mode picker (modal: up/down move, South apply, East cancel) ─
        if self.gamepad_nav.press_mode_open {
            let mut step_dir: Option<gn::NavDir> = None;
            if nav.is_rising("dpad_up") { step_dir = Some(gn::NavDir::Up); }
            else if nav.is_rising("dpad_down") { step_dir = Some(gn::NavDir::Down); }
            if step_dir.is_none() {
                if let Some(d) = gn::stick_dir(nav.lstick) {
                    if matches!(d, gn::NavDir::Up | gn::NavDir::Down)
                        && self.gamepad_nav.repeat_dir != Some(d)
                    {
                        self.gamepad_nav.repeat_dir = Some(d);
                        step_dir = Some(d);
                    } else if !matches!(d, gn::NavDir::Up | gn::NavDir::Down) {
                        self.gamepad_nav.repeat_dir = None;
                    }
                } else {
                    self.gamepad_nav.repeat_dir = None;
                }
            }
            self.drive_press_mode_picker(step_dir, &nav);
            self.gamepad_nav.prev_pressed = nav.pressed.clone();
            ctx.request_repaint();
            return;
        }

        // ── Preset dropdown navigation (modal: captures dpad/South/East) ──────
        // When open, the controller drives the preset list (rendered + applied
        // by the Easy center panel, which owns the preset index/apply logic).
        if self.gamepad_nav.preset_nav_open {
            self.gamepad_nav.preset_nav_step = 0;
            if nav.is_rising("dpad_up") { self.gamepad_nav.preset_nav_step = -1; }
            if nav.is_rising("dpad_down") { self.gamepad_nav.preset_nav_step = 1; }
            if let Some(d) = gn::stick_dir(nav.lstick) {
                // Discrete step per fresh deflection (no auto-repeat for a list).
                if self.gamepad_nav.repeat_dir != Some(d) {
                    self.gamepad_nav.repeat_dir = Some(d);
                    match d {
                        gn::NavDir::Up => self.gamepad_nav.preset_nav_step = -1,
                        gn::NavDir::Down => self.gamepad_nav.preset_nav_step = 1,
                        _ => {}
                    }
                }
            } else {
                self.gamepad_nav.repeat_dir = None;
            }
            if nav.is_rising("btn_south") || (nav.rt > 0.5 && !self.gamepad_nav.prev_rt) {
                self.gamepad_nav.preset_nav_select = true;
            }
            if nav.is_rising("btn_east") {
                self.gamepad_nav.preset_nav_open = false;
            }
            self.gamepad_nav.prev_pressed = nav.pressed.clone();
            self.gamepad_nav.prev_rt = nav.rt > 0.5;
            self.gamepad_nav.prev_lt = nav.lt > 0.5;
            ctx.request_repaint();
            return;
        }

        // ── LB/RB: switch tabs ───────────────────────────────────────────────
        // Reserved while editing Touch Zones: line editing uses the bumpers to
        // switch the focused PAD, and the cards widget uses them to cycle zones,
        // so they must not also flip tabs.
        let tz_editing = matches!(self.gamepad_nav.edit_level,
            crate::gamepad_nav::EditLevel::TzLines | crate::gamepad_nav::EditLevel::TzGrab
            | crate::gamepad_nav::EditLevel::TzCards);
        if !tz_editing && nav.is_rising("btn_lb") && self.active_tab > 0 {
            self.set_active_tab(self.active_tab - 1);
        }
        if !tz_editing && nav.is_rising("btn_rb") && self.active_tab + 1 < self.tabs.len() {
            self.set_active_tab(self.active_tab + 1);
        }

        // ── Undo / redo ──────────────────────────────────────────────────────
        if nav.is_rising("btn_ls") {
            let canvas = &mut self.tabs[self.active_tab].canvas;
            if canvas.can_undo() {
                canvas.undo();
            }
        }
        if nav.is_rising("btn_rs") {
            let canvas = &mut self.tabs[self.active_tab].canvas;
            if canvas.can_redo() {
                canvas.redo();
            }
        }

        // ── Cursor (right stick + gyro) ──────────────────────────────────────
        self.update_nav_cursor(ctx, &nav, dt);

        // ── Analog trigger context (default: RT = confirm/enter, LT = back) ──
        // Widget-specific trigger contexts (curve dot add/delete, remapper
        // filter cycle) are a planned follow-up; the default confirm/back
        // mapping below composes with the edit-level handling.
        let rt_rising = nav.rt > 0.5 && !self.gamepad_nav.prev_rt;
        let lt_rising = nav.lt > 0.5 && !self.gamepad_nav.prev_lt;
        self.gamepad_nav.prev_rt = nav.rt > 0.5;
        self.gamepad_nav.prev_lt = nav.lt > 0.5;

        // ── North: toggle the Easy left "Devices" panel (top nav level only) ──
        // North is otherwise unused at the base Widget level, so it doubles as the
        // panel Show/Hide. Suppressed inside slider edits / deeper edit levels
        // (where North is Reset/Recenter). Consumes the frame.
        if matches!(self.gamepad_nav.edit_level, gn::EditLevel::Widget)
            && self.gamepad_nav.left_edit.is_none()
            && nav.is_rising("btn_north")
        {
            self.easy_left_panel_collapsed = !self.easy_left_panel_collapsed;
            if self.easy_left_panel_collapsed {
                self.gamepad_nav.left_selected = None;
            }
            self.gamepad_nav.prev_pressed = nav.pressed.clone();
            ctx.request_repaint();
            return;
        }

        // ── Left I/O panel navigation (cursor + LS/D-pad) ────────────────────
        // The RS/gyro cursor and an LS/D-pad selection both point at left-panel
        // targets; South/RT acts on whichever is active (select input device,
        // toggle output, enter slider edit). While focus lives in the panel or a
        // slider edit is in progress, sub-patch handling is skipped. Returns true
        // when it consumed input.
        if self.nav_drive_left_panel(ctx, &nav, dt, rt_rising, lt_rising) {
            self.gamepad_nav.prev_pressed = nav.pressed.clone();
            ctx.request_repaint();
            return;
        }

        // ── Selection + edit on the active tab's sub-patch ───────────────────
        let outer_id = {
            let canvas = &self.tabs[self.active_tab].canvas;
            canvas
                .snarl
                .nodes_ids_data()
                .find(|(_, n)| n.value.module_id == "subpatch")
                .map(|(id, _)| id)
        };
        if let Some(outer_id) = outer_id {
            self.nav_drive_subpatch(ctx, outer_id, &nav, dt, rt_rising, lt_rising);
            // Publish glow state for the subpatch renderer: presence of the key
            // means "draw the nav selection glow"; value = is-editing.
            let editing = matches!(
                self.gamepad_nav.edit_level,
                crate::gamepad_nav::EditLevel::Editing
                    | crate::gamepad_nav::EditLevel::CurveDots
                    | crate::gamepad_nav::EditLevel::CurveDot
                    | crate::gamepad_nav::EditLevel::RemapScroll
                    | crate::gamepad_nav::EditLevel::TzCards
            );
            let pass = ctx.cumulative_pass_nr();
            ctx.data_mut(|d| {
                d.insert_temp(egui::Id::new(("gp_nav_glow", outer_id.0)), (pass, editing))
            });
            // Draw the mapping-card glow at top level (NOT inside the remapper
            // body's child layer — that deadlocks epaint). Uses global rects the
            // body published last frame.
            if matches!(self.gamepad_nav.edit_level,
                crate::gamepad_nav::EditLevel::RemapScroll
                | crate::gamepad_nav::EditLevel::RemapCard
                | crate::gamepad_nav::EditLevel::TzCards)
            {
                self.nav_draw_remap_card_glow(ctx, outer_id);
            }
        }

        self.gamepad_nav.prev_pressed = nav.pressed.clone();
        ctx.request_repaint();
    }

    /// Selection + value-edit handling within the sub-patch identified by
    /// `outer_id`. Splits into widget-level (move selection) and editing-level
    /// (adjust the focused control).
    fn nav_drive_subpatch(
        &mut self,
        ctx: &egui::Context,
        outer_id: egui_snarl::NodeId,
        nav: &crate::gamepad_nav::NavInput,
        dt: f32,
        rt_rising: bool,
        lt_rising: bool,
    ) {
        use crate::gamepad_nav::{self as gn, EditLevel, NavDir};

        // Resolve directional intent: dpad = discrete; stick = auto-repeat.
        let mut step_dir: Option<NavDir> = None;
        if nav.is_rising("dpad_up") {
            step_dir = Some(NavDir::Up);
        } else if nav.is_rising("dpad_down") {
            step_dir = Some(NavDir::Down);
        } else if nav.is_rising("dpad_left") {
            step_dir = Some(NavDir::Left);
        } else if nav.is_rising("dpad_right") {
            step_dir = Some(NavDir::Right);
        }

        // Left-stick auto-repeat. Magnitude scales speed.
        let stick = gn::stick_dir(nav.lstick);
        let mag = nav.lstick.length();
        if let Some(sd) = stick {
            if self.gamepad_nav.repeat_dir != Some(sd) {
                self.gamepad_nav.repeat_dir = Some(sd);
                self.gamepad_nav.repeat_accum = 1.0; // immediate first step
            }
            let rate = 6.0 + ((mag - 0.5) / 0.5).clamp(0.0, 1.0) * 12.0;
            self.gamepad_nav.repeat_accum += dt * rate;
            if self.gamepad_nav.repeat_accum >= 1.0 {
                self.gamepad_nav.repeat_accum -= 1.0;
                if step_dir.is_none() {
                    step_dir = Some(sd);
                }
            }
        } else {
            self.gamepad_nav.repeat_dir = None;
            self.gamepad_nav.repeat_accum = 0.0;
        }

        match self.gamepad_nav.edit_level {
            EditLevel::Widget => {
                // Move selection spatially.
                if let Some(dir) = step_dir {
                    // Seamless cross LEFT into the Devices panel: when already at
                    // the left-most sub-patch widget (a real selection with no
                    // left neighbour) and the panel is visible, hand focus to the
                    // panel instead of staying put. Consumes this frame; the panel
                    // takes over next frame.
                    if matches!(dir, NavDir::Left) && !self.easy_left_panel_collapsed {
                        let at_left_edge = {
                            let canvas = &self.tabs[self.active_tab].canvas;
                            canvas.snarl.get_node(outer_id)
                                .and_then(|n| n.subpatch.as_ref())
                                .map(|sp| sp.selected_item.is_some()
                                    && gn::nearest_in_dir(&sp.items, sp.selected_item, NavDir::Left).is_none())
                                .unwrap_or(false)
                        };
                        if at_left_edge {
                            let targets: Vec<gn::LeftNavTarget> = ctx
                                .data(|d| d.get_temp::<(u64, Vec<gn::LeftNavTarget>)>(
                                    gn::left_targets_id()))
                                .map(|(_, t)| t)
                                .unwrap_or_default();
                            if let Some(idx) =
                                gn::nearest_target_rect_in_dir(&targets, None, NavDir::Left)
                            {
                                self.gamepad_nav.left_selected = Some(idx);
                                self.gamepad_nav.repeat_dir = None;
                                self.gamepad_nav.repeat_accum = 0.0;
                                return;
                            }
                        }
                    }
                    let canvas = &mut self.tabs[self.active_tab].canvas;
                    if let Some(sp) = canvas
                        .snarl
                        .get_node_mut(outer_id)
                        .and_then(|n| n.subpatch.as_mut())
                    {
                        if let Some(next) = gn::nearest_in_dir(&sp.items, sp.selected_item, dir) {
                            sp.selected_item = Some(next);
                            sp.selected_items = vec![next];
                        } else if sp.selected_item.is_none() {
                            // Seed selection at the first navigable item.
                            if let Some(seed) = gn::nearest_in_dir(&sp.items, None, dir) {
                                sp.selected_item = Some(seed);
                                sp.selected_items = vec![seed];
                            }
                        }
                    }
                }
                // If the RS/gyro cursor is visible, RT/South first MOVES the
                // selection to whatever widget the cursor is over (the cursor's
                // whole point: visually pick the target). Only then do we act on
                // it. Falls through to act on the existing selection when the
                // cursor isn't over any item.
                if (nav.is_rising("btn_south") || rt_rising)
                    && self.gamepad_nav.cursor_visible
                {
                    if let Some(hit) = self.nav_cursor_hit_item(ctx, outer_id) {
                        let canvas = &mut self.tabs[self.active_tab].canvas;
                        if let Some(sp) = canvas
                            .snarl
                            .get_node_mut(outer_id)
                            .and_then(|n| n.subpatch.as_mut())
                        {
                            sp.selected_item = Some(hit);
                            sp.selected_items = vec![hit];
                        }
                    }
                }
                let kind = self.nav_selected_kind(outer_id);
                // Response curve: South ENTERS the curve (dot navigation). Once
                // inside, LT/RT add/remove dots and South again edits a dot.
                if matches!(kind, NavWidgetKind::Curve) {
                    if nav.is_rising("btn_south") || rt_rising {
                        self.gamepad_nav.edit_level = EditLevel::CurveDots;
                        self.gamepad_nav.curve_return_level = EditLevel::Widget;
                        self.gamepad_nav.curve_dot = 0;
                        self.gamepad_nav.edit_baseline = Some(Box::new(
                            self.tabs[self.active_tab].canvas.snapshot_for_undo()));
                    }
                    let _ = lt_rising;
                } else if matches!(kind, NavWidgetKind::Remapper) {
                    // Remapper / Map Action / Combiner / Lean: South ENTERS the
                    // widget (scroll mode — up/down scrolls the mapping list,
                    // North arms Learn/capture, LT/RT cycle the filter, East
                    // exits). RT/LT also cycle the filter directly at widget
                    // level for quick access without entering.
                    if nav.is_rising("btn_south") {
                        self.gamepad_nav.edit_level = EditLevel::RemapScroll;
                    } else if rt_rising || lt_rising {
                        if let Some(inner) = self.nav_selected_inner_node(outer_id) {
                            let dir = if rt_rising { 1 } else { -1 };
                            crate::canvas::viewer::nav_cycle_remapper_filter(ctx, inner.0, dir);
                        }
                    }
                } else if matches!(kind, NavWidgetKind::TouchZones) {
                    // Touch Zones pad: South ENTERS line editing (TzLines). Seed
                    // the focused line at the first interior divider of the pad
                    // and snapshot for one coalesced undo entry across the edit.
                    if nav.is_rising("btn_south") {
                        self.nav_tz_enter(outer_id);
                    }
                } else if matches!(kind, NavWidgetKind::TouchZoneCards) {
                    // Touch Zones cards: South ENTERS the zone-tab + Learn flow.
                    if nav.is_rising("btn_south") {
                        self.gamepad_nav.edit_level = EditLevel::TzCards;
                    }
                } else {
                    // South / RT → act on the selected widget by kind.
                    if nav.is_rising("btn_south") || rt_rising {
                        match kind {
                            NavWidgetKind::Toggle => {
                                // Switch: toggle immediately, one undo entry.
                                let base = self.tabs[self.active_tab].canvas.snapshot_for_undo();
                                if self.nav_toggle_switch(outer_id) {
                                    self.tabs[self.active_tab].canvas.commit_undo_if_changed(base);
                                }
                            }
                            NavWidgetKind::Value | NavWidgetKind::Dropdown
                            | NavWidgetKind::MultiField => {
                                self.gamepad_nav.edit_level = EditLevel::Editing;
                                self.gamepad_nav.fine_increment = false;
                                self.gamepad_nav.field_index = 0;
                                // Snapshot for a single coalesced undo entry on exit.
                                self.gamepad_nav.edit_baseline = Some(Box::new(
                                    self.tabs[self.active_tab].canvas.snapshot_for_undo()));
                                // Dropdown: open its real popup so all options are
                                // visible while cycling (the pinned combo is a
                                // custom button with an egui-memory popup flag).
                                if matches!(kind, NavWidgetKind::Dropdown) {
                                    self.nav_set_dropdown_popup(ctx, outer_id, true);
                                }
                            }
                            // Curve / Remapper / TouchZones(+Cards) are handled by
                            // the dedicated branches above; unreachable here.
                            NavWidgetKind::Curve | NavWidgetKind::Remapper
                            | NavWidgetKind::TouchZones | NavWidgetKind::TouchZoneCards
                            | NavWidgetKind::None => {}
                        }
                    }
                    let _ = lt_rising; // no back action at widget level
                }
            }
            EditLevel::Editing => {
                let kind = self.nav_selected_kind(outer_id);
                // East / LT / back → exit to widget level, committing the edit
                // as one undo entry if anything actually changed.
                if nav.is_rising("btn_east") || lt_rising {
                    self.gamepad_nav.edit_level = EditLevel::Widget;
                    self.nav_set_dropdown_popup(ctx, outer_id, false);
                    if let Some(baseline) = self.gamepad_nav.edit_baseline.take() {
                        self.tabs[self.active_tab].canvas.commit_undo_if_changed(*baseline);
                    }
                } else if matches!(kind, NavWidgetKind::MultiField) {
                    // Unified multi-field editor: left/right pick a field, up/down
                    // & stick edit it, West=fine, North=reset, South=cycle/toggle.
                    self.nav_drive_fields(ctx, outer_id, nav, dt, step_dir, rt_rising, mag);
                } else if matches!(kind, NavWidgetKind::Dropdown) {
                    // Real Dropdown (dynamic options param): up/down or South/RT
                    // cycles; South/RT also confirm-exits to widget level.
                    if let Some(dir) = step_dir {
                        let d = match dir {
                            NavDir::Down | NavDir::Right => 1,
                            NavDir::Up | NavDir::Left => -1,
                        };
                        self.nav_cycle_dropdown(outer_id, d);
                    }
                    if nav.is_rising("btn_south") || rt_rising {
                        self.gamepad_nav.edit_level = EditLevel::Widget;
                        self.nav_set_dropdown_popup(ctx, outer_id, false);
                        if let Some(baseline) = self.gamepad_nav.edit_baseline.take() {
                            self.tabs[self.active_tab].canvas.commit_undo_if_changed(*baseline);
                        }
                    }
                } else {
                    let _ = rt_rising;
                    // West → toggle fine increments (knob/constant).
                    if nav.is_rising("btn_west") {
                        self.gamepad_nav.fine_increment = !self.gamepad_nav.fine_increment;
                    }
                    // North → reset to default.
                    if nav.is_rising("btn_north") {
                        self.nav_reset_selected(outer_id);
                    }
                    if matches!(kind, NavWidgetKind::Value) {
                        // Knob/constant: linear normalized nudge.
                        let fine = self.gamepad_nav.fine_increment;
                        let base_step = if fine { 0.005 } else { 0.02 };
                        let mut delta = 0.0f32;
                        if let Some(dir) = step_dir {
                            let s = match dir {
                                NavDir::Right | NavDir::Up => 1.0,
                                NavDir::Left | NavDir::Down => -1.0,
                            };
                            delta += s * base_step;
                        }
                        if mag > 0.5 {
                            let sens = if fine { 0.15 } else { 0.6 };
                            delta += nav.lstick.x * sens * dt;
                        }
                        if delta != 0.0 {
                            self.nav_adjust_selected(outer_id, delta);
                        }
                    }
                }
            }
            EditLevel::CurveDots => {
                self.nav_drive_curve_dots(ctx, outer_id, nav, step_dir, rt_rising, lt_rising);
            }
            EditLevel::CurveDot => {
                self.nav_drive_curve_dot(ctx, outer_id, nav, dt, step_dir, rt_rising, lt_rising);
            }
            EditLevel::RemapScroll => {
                self.nav_drive_remapper(ctx, outer_id, nav, dt, step_dir, rt_rising, lt_rising);
            }
            EditLevel::RemapCard => {
                self.nav_drive_remap_card(ctx, outer_id, nav, dt, step_dir, rt_rising, mag);
            }
            EditLevel::TzLines | EditLevel::TzGrab => {
                self.nav_drive_touch_zones(ctx, outer_id, nav, dt, step_dir, rt_rising, lt_rising, mag);
            }
            EditLevel::TzCards => {
                self.nav_drive_tz_cards(ctx, outer_id, nav, step_dir, rt_rising, lt_rising);
            }
        }
    }










    /// Enter the Alt+Tab window switcher: hold Alt and tap Tab once. Keeps
    /// Alt held while Select stays down; releasing Select commits.
    fn enter_alt_tab(&mut self, _dev_id: &str, ctx: &egui::Context) {
        // Alt-tabbing away from a pinned FlexInput is self-defeating: it
        // would stay always-on-top over whatever window the user switches
        // to. Un-pin as part of entering the switcher. We don't run the full
        // `toggle_pin` foreground-yield here — Alt+Tab itself decides the new
        // foreground — we just clear the always-on-top level and topmost band.
        if self.settings.pin_active {
            self.settings.pin_active = false;
            self.settings_dirty = true;
            ctx.send_viewport_cmd(
                egui::ViewportCommand::WindowLevel(egui::WindowLevel::Normal));
            if let Some(hwnd) = self.self_hwnd {
                crate::process_list::drop_topmost(hwnd);
            }
        }
        #[cfg(windows)]
        {
            let tapper = self
                .gamepad_nav
                .key_tapper
                .get_or_insert_with(flexinput_virtual::windows::UiKeyTapper::new);
            tapper.alt_down();
            tapper.tap("tab");
        }
        self.gamepad_nav.alt_tab_active = true;
        self.gamepad_nav.rs_arrow_armed = true;
    }

    /// Drive the active Alt+Tab switcher: while Select is held, right-stick
    /// flicks tap arrow keys (one per flick). Releasing Select releases Alt.
    fn drive_alt_tab(&mut self, _dev_id: &str, nav: &crate::gamepad_nav::NavInput) {
        // Released Select → commit.
        if !nav.pressed.contains("btn_back") {
            self.alt_tab_release();
            return;
        }
        #[cfg(windows)]
        {
            let rs = nav.rstick;
            let mag = rs.length();
            if self.gamepad_nav.rs_arrow_armed && mag > 0.6 {
                let dir = if rs.x.abs() >= rs.y.abs() {
                    if rs.x >= 0.0 { "arrowright" } else { "arrowleft" }
                } else {
                    // Screen Y grows downward; stick +y is up.
                    if rs.y >= 0.0 { "arrowup" } else { "arrowdown" }
                };
                if let Some(tapper) = &mut self.gamepad_nav.key_tapper {
                    tapper.tap(dir);
                }
                self.gamepad_nav.rs_arrow_armed = false;
            } else if mag < 0.3 {
                self.gamepad_nav.rs_arrow_armed = true;
            }
        }
    }

    /// Release a held Alt (ends the switcher) and clear switcher state.
    fn alt_tab_release(&mut self) {
        #[cfg(windows)]
        if let Some(tapper) = &mut self.gamepad_nav.key_tapper {
            tapper.alt_up();
        }
        self.gamepad_nav.alt_tab_active = false;
        self.gamepad_nav.rs_arrow_armed = true;
    }

    /// Pins acceptable in a shortcut chord: every bool button on the pad
    /// (face / shoulder / digital-trigger / stick-click / dpad / system). No
    /// pin is excluded — North is just as bindable as any other.
    const CHORD_PINS: &[&str] = &[
        "btn_south", "btn_east", "btn_west", "btn_north",
        "btn_lb", "btn_rb", "btn_lt_dig", "btn_rt_dig",
        "btn_ls", "btn_rs", "btn_start", "btn_back",
        "btn_guide", "btn_touchpad", "btn_mute",
        "dpad_up", "dpad_down", "dpad_left", "dpad_right",
    ];

    /// Shortcut-chord capture for the DESKTOP Settings window (mouse-started
    /// learn, gamepad panel closed). Aggregates held chord pins across all
    /// connected non-MIDI gamepads (no nav-mode requirement), arm-idles once,
    /// and latches the combo on full release. No button is excluded.
    fn drive_chord_learn_desktop(&mut self) {
        let excluded = self.own_virtual_device_ids();
        let mut held: Vec<String> = Vec::new();
        for d in self.devices.iter().filter(|d| !matches!(d.kind,
            flexinput_devices::ControllerKind::MidiIn
            | flexinput_devices::ControllerKind::MidiOut))
            .filter(|d| !excluded.contains(&d.id))
        {
            for pin in Self::CHORD_PINS {
                let down = self.last_signals
                    .get(&(d.id.clone(), pin.to_string()))
                    .map(|s| s.as_bool()).unwrap_or(false);
                if down && !held.iter().any(|q| q == pin) {
                    held.push(pin.to_string());
                }
            }
        }
        let held_any = !held.is_empty();
        if !self.gamepad_nav.chord_arm_idle {
            if !held_any { self.gamepad_nav.chord_arm_idle = true; }
            return;
        }
        for pin in &held {
            if !self.gamepad_nav.chord_draft.iter().any(|q| q == pin) {
                self.gamepad_nav.chord_draft.push(pin.clone());
            }
        }
        if !held_any && !self.gamepad_nav.chord_draft.is_empty() {
            let draft = std::mem::take(&mut self.gamepad_nav.chord_draft);
            // Same rule as the panel: combos only (>= 2). East-alone cancels;
            // any other single button is rejected (keep listening).
            if draft.len() < 2 {
                self.gamepad_nav.chord_arm_idle = false;
                if draft == ["btn_east"] {
                    self.gamepad_nav.chord_learn = None;
                }
                return;
            }
            self.commit_chord(draft);
        }
    }

    /// Shortcut-chord capture, driven from inside the gamepad settings panel
    /// while a ChordLearn row is learning. Mirrors the widget Learn flow: the
    /// panel stays open and shows the listening state, we wait for the device
    /// to go idle ONCE (arm-idle) so the South that started the capture isn't
    /// swept in, accumulate every held button while the user presses the combo,
    /// and LATCH into the target setting the moment everything releases (after
    /// at least one button was held). East aborts with no binding written.
    fn drive_gp_chord_capture(&mut self, nav: &crate::gamepad_nav::NavInput) {
        let held: Vec<String> = Self::CHORD_PINS.iter()
            .filter(|p| nav.pressed.contains(**p))
            .map(|p| p.to_string())
            .collect();
        let held_any = !held.is_empty();

        // Arm-idle: don't accumulate until the device has gone fully idle once
        // since learn started — otherwise the (still-held) South that opened the
        // capture lands in the combo.
        if !self.gamepad_nav.chord_arm_idle {
            if !held_any { self.gamepad_nav.chord_arm_idle = true; }
            return;
        }

        // Accumulate the held chord (union over the press) — East included, so a
        // combo CAN contain East as long as it is pressed together with at least
        // one other button.
        for pin in &held {
            if !self.gamepad_nav.chord_draft.iter().any(|q| q == pin) {
                self.gamepad_nav.chord_draft.push(pin.clone());
            }
        }

        // On full release, interpret the captured draft.
        if !held_any && !self.gamepad_nav.chord_draft.is_empty() {
            let draft = std::mem::take(&mut self.gamepad_nav.chord_draft);
            // Shortcuts MUST be combos (>= 2 buttons). A single button never
            // latches: East-alone cancels the capture (back); any other single
            // button is simply rejected and we keep listening.
            if draft.len() < 2 {
                self.gamepad_nav.chord_arm_idle = false;
                if draft == ["btn_east"] {
                    self.gamepad_nav.chord_learn = None; // cancel
                }
                return;
            }
            self.commit_chord(draft);
        }
    }

    /// Latch a captured combo into the active `chord_learn` target and exit
    /// capture. Shared by the gamepad-panel and desktop-window capture paths.
    fn commit_chord(&mut self, chord: Vec<String>) {
        use crate::gamepad_nav::ChordTarget;
        self.gamepad_nav.chord_arm_idle = false;
        match self.gamepad_nav.chord_learn.take() {
            Some(ChordTarget::SeeThrough) => self.settings.seethrough_chord = Some(chord),
            Some(ChordTarget::Panic)      => self.settings.panic_chord = Some(chord),
            Some(ChordTarget::Overlay)    => self.settings.overlay_chord = Some(chord),
            None => {}
        }
        self.settings_dirty = true;
    }

    /// Detect the assigned see-through / panic gamepad combos in `nav` and fire
    /// the toggle once per full press (rising edge of "all combo buttons held").
    /// Respects `gamepad_chords_nav_only`: when set, only fires while the driving
    /// device is actually in nav mode (which it is here — this runs inside the
    /// nav driver after the device resolved); when unset it still requires the
    /// driver to be active, but that's the same code path. The nav-only flag
    /// therefore gates whether we evaluate at all vs. always (see caller note).
    fn process_shortcut_chords(&mut self, ctx: &egui::Context, nav: &crate::gamepad_nav::NavInput) -> bool {
        let chord_held = |chord: &Option<Vec<String>>| -> bool {
            match chord {
                Some(c) if !c.is_empty() => c.iter().all(|p| nav.pressed.contains(p)),
                _ => false,
            }
        };
        let mut fired = false;
        // See-through.
        let st_now = chord_held(&self.settings.seethrough_chord);
        if st_now && !self.gamepad_nav.seethrough_chord_down {
            let next = !self.settings.see_through_active;
            self.settings.see_through_active = next;
            self.settings_dirty = true;
            // Mirror into the temp slot the eye-toggle uses so the canvas frame
            // picks it up this frame (update() also syncs settings↔slot).
            ctx.data_mut(|d| d.insert_temp(
                egui::Id::new(crate::canvas::SEE_THROUGH_DATA_KEY), next));
            fired = true;
        }
        self.gamepad_nav.seethrough_chord_down = st_now;
        // Panic.
        let pn_now = chord_held(&self.settings.panic_chord);
        if pn_now && !self.gamepad_nav.panic_chord_down {
            self.panic_active = !self.panic_active;
            fired = true;
        }
        self.gamepad_nav.panic_chord_down = pn_now;
        // Overlay.
        let ov_now = chord_held(&self.settings.overlay_chord);
        if ov_now && !self.gamepad_nav.overlay_chord_down {
            crate::overlay::set_overlay_visible(ctx, !crate::overlay::overlay_visible(ctx));
            fired = true;
        }
        self.gamepad_nav.overlay_chord_down = ov_now;
        fired
    }

    /// Non-nav-only shortcut-chord detection: scan every eligible gamepad
    /// (non-MIDI, not our own loopback virtual) for the assigned see-through /
    /// panic combos and fire once per full press. Only called when FlexInput is
    /// focused and `gamepad_chords_nav_only` is false.
    fn check_shortcut_chords_global(&mut self, ctx: &egui::Context) {
        if !ctx.input(|i| i.focused) { return; }
        // Nothing to do if no combos are assigned.
        if self.settings.seethrough_chord.is_none()
            && self.settings.panic_chord.is_none()
            && self.settings.overlay_chord.is_none()
        {
            self.gamepad_nav.seethrough_chord_down = false;
            self.gamepad_nav.panic_chord_down = false;
            self.gamepad_nav.overlay_chord_down = false;
            return;
        }
        let excluded = self.own_virtual_device_ids();
        let any_holds = |chord: &Option<Vec<String>>| -> bool {
            let Some(c) = chord else { return false; };
            if c.is_empty() { return false; }
            self.devices.iter()
                .filter(|d| !matches!(d.kind,
                    flexinput_devices::ControllerKind::MidiIn
                    | flexinput_devices::ControllerKind::MidiOut))
                .filter(|d| !excluded.contains(&d.id))
                .any(|d| c.iter().all(|pin| {
                    self.last_signals.get(&(d.id.clone(), pin.clone()))
                        .map(|s| s.as_bool()).unwrap_or(false)
                }))
        };
        let st_now = any_holds(&self.settings.seethrough_chord);
        if st_now && !self.gamepad_nav.seethrough_chord_down {
            let next = !self.settings.see_through_active;
            self.settings.see_through_active = next;
            self.settings_dirty = true;
            ctx.data_mut(|d| d.insert_temp(
                egui::Id::new(crate::canvas::SEE_THROUGH_DATA_KEY), next));
        }
        self.gamepad_nav.seethrough_chord_down = st_now;
        let pn_now = any_holds(&self.settings.panic_chord);
        if pn_now && !self.gamepad_nav.panic_chord_down {
            self.panic_active = !self.panic_active;
        }
        self.gamepad_nav.panic_chord_down = pn_now;
        let ov_now = any_holds(&self.settings.overlay_chord);
        if ov_now && !self.gamepad_nav.overlay_chord_down {
            crate::overlay::set_overlay_visible(ctx, !crate::overlay::overlay_visible(ctx));
        }
        self.gamepad_nav.overlay_chord_down = ov_now;
    }

    /// Update the right-stick + gyro cursor overlay position/visibility.
    fn update_nav_cursor(
        &mut self,
        ctx: &egui::Context,
        nav: &crate::gamepad_nav::NavInput,
        dt: f32,
    ) {
        // Accelerated response: slow start, ramps up hard in the back half.
        // speed = RS_MAX * deflection^RS_EXP with RS_EXP > 1 (a genuinely
        // accelerating curve). Both are user-tunable in Settings; defaults are
        // sized so ~30% deflection ≈ the OLD top speed (~900 px/s):
        //   18250 * 0.30^2.5 ≈ 900.
        let rs_max = self.settings.cursor_max_speed;
        let rs_exp = self.settings.cursor_accel;
        // Gyro values are normalized dps/2000 (GYRO_REF_DPS), so a ~200°/s turn
        // is ≈0.1. Scale so that maps to a gentle ~150 px/s fine nudge.
        const GYRO_FINE: f32 = 3000.0; // px/s per normalized-gyro unit, while visible
        let screen = ctx.content_rect();
        let gn = &mut self.gamepad_nav;
        if !gn.cursor_visible {
            // Seed at screen center on first appearance.
            gn.cursor_pos = screen.center();
        }
        let rs = nav.rstick;
        let mag = rs.length();
        if mag > 0.08 {
            // Direction from the stick, speed from the accelerated curve on the
            // magnitude (slow start, fast late when rs_exp > 1).
            let speed = rs_max * mag.clamp(0.0, 1.0).powf(rs_exp);
            let dir = rs / mag; // unit vector
            // Stick +y is up; screen y grows down → negate y.
            gn.cursor_pos += egui::vec2(dir.x, -dir.y) * speed * dt;
            gn.cursor_visible = true;
            gn.cursor_last_move = std::time::Instant::now();
        }
        if gn.cursor_visible {
            // Gyro fine movement (pitch = vertical, yaw = horizontal).
            gn.cursor_pos += egui::vec2(nav.gyro_yaw, -nav.gyro_pitch) * GYRO_FINE * dt;
            gn.cursor_pos.x = gn.cursor_pos.x.clamp(screen.min.x, screen.max.x);
            gn.cursor_pos.y = gn.cursor_pos.y.clamp(screen.min.y, screen.max.y);
            if gn.cursor_last_move.elapsed().as_secs_f32() > 3.0 {
                gn.cursor_visible = false;
            }
        }
    }

    /// Draw the right-stick/gyro cursor overlay when visible.
    fn draw_nav_cursor(&mut self, ctx: &egui::Context) {
        if !self.gamepad_nav.cursor_visible {
            return;
        }
        // Lazily rasterize + upload the circular target SVG, tinted with the
        // selection accent.
        if self.gamepad_nav.cursor_tex.is_none() {
            const CURSOR_SVG: &str =
                include_str!("../../../app/assets/flair_circle_target_с.svg");
            let accent = ctx.style().visuals.selection.stroke.color;
            if let Some(img) = crate::canvas::viewer::rasterize_svg_recolored(
                CURSOR_SVG, 128, 128, "override", accent,
            ) {
                let tex = ctx.load_texture("gp_nav_cursor", img, egui::TextureOptions::LINEAR);
                self.gamepad_nav.cursor_tex = Some(tex);
            }
        }
        let Some(tex) = &self.gamepad_nav.cursor_tex else { return; };
        let painter = ctx.layer_painter(egui::LayerId::new(
            egui::Order::Foreground,
            egui::Id::new("gp_nav_cursor_layer"),
        ));
        let size = egui::vec2(56.0, 56.0);
        let rect = egui::Rect::from_center_size(self.gamepad_nav.cursor_pos, size);
        painter.image(
            tex.id(),
            rect,
            egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
            egui::Color32::WHITE,
        );
        ctx.request_repaint();
    }

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
        // matters because the I/O thread runs at the polling rate (default
        // 500 Hz) and the UI thread controls flush ordering on tab switch.
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
        // Write only when the set actually changed. This is called once per
        // frame (so a feedback-source device's id is reliably present for the
        // I/O thread's output routing — it used to be refreshed only on canvas
        // events, leaving the set stale so HIDMaestro rumble never routed to the
        // physical pad). The change-guard keeps the steady state to a cheap
        // read-lock + compare, avoiding write-lock contention with the I/O
        // thread that reads this set every tick.
        if *self.active_tab_device_ids.read().unwrap() == ids {
            return;
        }
        *self.active_tab_device_ids.write().unwrap() = ids;
    }

    /// Drain completed device-ops worker results and apply them to the shared
    /// pool. Called once at the top of `update()`. Devices only enter the pool
    /// here — after they're fully built — which is what keeps the I/O thread from
    /// ever `flush()`ing a half-initialized section (the startup-race wedge).
    fn drain_device_op_results(&mut self) {
        use crate::device_ops::DeviceOpResult;
        // Collect first so we don't hold the pool lock across the channel drain.
        let results: Vec<DeviceOpResult> = self.device_ops.rx.try_iter().collect();
        if results.is_empty() {
            return;
        }
        let mut pool = self.shared_virtual_devices.lock().unwrap();
        for res in results {
            match res {
                DeviceOpResult::Created { device } => {
                    let id = device.id().to_string();
                    self.pending_device_ids.remove(&id);
                    // Guard against a duplicate (e.g. two enqueues raced): replace
                    // any existing entry with the same id rather than doubling up.
                    if !pool.iter().any(|d| d.id() == id) {
                        pool.push(device);
                    }
                }
                DeviceOpResult::Removed { device_id } => {
                    self.pending_device_ids.remove(&device_id);
                }
                DeviceOpResult::Reinstalled { devices, errors } => {
                    for device in devices {
                        let id = device.id().to_string();
                        self.pending_device_ids.remove(&id);
                        if !pool.iter().any(|d| d.id() == id) {
                            pool.push(device);
                        }
                    }
                    if !errors.is_empty() {
                        let msg = errors.join("; ");
                        eprintln!("[device-ops] reinstall completed with issues: {msg}");
                        self.last_device_op_error = Some(msg);
                    } else {
                        self.last_device_op_error = None;
                    }
                }
                DeviceOpResult::Uninstalled { errors } => {
                    if !errors.is_empty() {
                        let msg = errors.join("; ");
                        eprintln!("[device-ops] uninstall completed with issues: {msg}");
                        self.last_device_op_error = Some(msg);
                    } else {
                        self.last_device_op_error = None;
                    }
                }
                DeviceOpResult::Failed { device_id, error } => {
                    self.pending_device_ids.remove(&device_id);
                    // Don't auto-retry a failed build every frame; require a
                    // remove+re-add (or driver reinstall) to try again.
                    self.failed_device_ids.insert(device_id.clone());
                    eprintln!("[device-ops] create '{device_id}' failed: {error}");
                    self.last_device_op_error = Some(error);
                }
            }
        }
    }

    /// Paint the full-window modal overlay while a device op is in flight. Dims
    /// the background, swallows all input (so no conflicting action can start),
    /// and shows a spinner + the worker's current label/detail. No-op when idle.
    fn draw_device_op_overlay(&self, ctx: &egui::Context) {
        let progress = match self.device_ops.progress.lock() {
            Ok(g) => g.clone(),
            Err(_) => None,
        };
        let Some(progress) = progress else { return };

        let screen = ctx.content_rect();
        egui::Area::new(egui::Id::new("device_op_modal"))
            .order(egui::Order::Foreground)
            .fixed_pos(screen.min)
            .show(ctx, |ui| {
                // Cover the whole window: paint a dim scrim and consume input so
                // nothing underneath is interactable.
                let resp = ui.allocate_rect(screen, egui::Sense::click_and_drag());
                ui.painter().rect_filled(
                    screen,
                    egui::CornerRadius::ZERO,
                    egui::Color32::from_black_alpha(160),
                );
                // Centered card.
                let painter = ui.painter();
                let card_w = 320.0_f32.min(screen.width() - 40.0);
                let card_h = 110.0;
                let card = egui::Rect::from_center_size(
                    screen.center(),
                    egui::vec2(card_w, card_h),
                );
                painter.rect_filled(card, egui::CornerRadius::same(10), egui::Color32::from_gray(32));
                painter.rect_stroke(
                    card,
                    egui::CornerRadius::same(10),
                    egui::Stroke::new(1.0, egui::Color32::from_gray(70)),
                    egui::StrokeKind::Inside,
                );
                // Spinner + text laid out inside the card.
                let mut child = ui.new_child(
                    egui::UiBuilder::new()
                        .max_rect(card.shrink(16.0))
                        .layout(egui::Layout::top_down(egui::Align::Center)),
                );
                child.add_space(4.0);
                child.add(egui::Spinner::new().size(22.0));
                child.add_space(8.0);
                child.label(egui::RichText::new(&progress.label).strong());
                if let Some(detail) = &progress.detail {
                    child.label(egui::RichText::new(detail).small().color(egui::Color32::from_gray(170)));
                }
                // Keep the spinner animating while visible.
                ui.ctx().request_repaint();
                let _ = resp;
            });
    }

    /// The "Reinstall HIDMaestro drivers" confirm dialog. On confirm, collects
    /// the HIDMaestro virtual devices currently on any canvas, removes them from
    /// the pool, and enqueues a `Reinstall` op carrying them (the worker drops
    /// them off-thread, reinstalls, and rebuilds them).
    fn draw_reinstall_confirm(&mut self, ctx: &egui::Context) {
        if !self.reinstall_confirm_open {
            return;
        }
        let mut do_reinstall = false;
        let mut cancel = false;
        egui::Window::new("Reinstall HIDMaestro drivers")
            .id(egui::Id::new("reinstall_hm_confirm"))
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
            .show(ctx, |ui| {
                ui.label(
                    "This removes and reinstalls the HIDMaestro driver, then re-deploys the \
                     virtual controllers on your canvas.",
                );
                ui.add_space(6.0);
                ui.label(
                    egui::RichText::new(
                        "Virtual controllers disconnect briefly — a running game may lose input \
                         for a few seconds. You'll be prompted for admin once.",
                    )
                    .small()
                    .color(egui::Color32::from_gray(170)),
                );
                ui.add_space(12.0);
                ui.horizontal(|ui| {
                    if ui.button("Reinstall").clicked() {
                        do_reinstall = true;
                    }
                    if ui.button("Cancel").clicked() {
                        cancel = true;
                    }
                });
            });

        if do_reinstall {
            self.reinstall_confirm_open = false;
            self.start_driver_reinstall();
        } else if cancel {
            self.reinstall_confirm_open = false;
        }
    }

    /// Collect the HIDMaestro virtual devices on every open canvas, pull them out
    /// of the shared pool, and enqueue a `Reinstall` op. The worker tears them
    /// down, reinstalls the driver, and rebuilds them; results land back through
    /// `drain_device_op_results`.
    fn start_driver_reinstall(&mut self) {
        // All HIDMaestro device ids referenced anywhere (dedup).
        let mut device_ids: Vec<String> = Vec::new();
        for tab in &self.tabs {
            for id in snarl_virtual_device_ids(&tab.canvas.snarl) {
                if id.starts_with("virtual.hm.") && !device_ids.contains(&id) {
                    device_ids.push(id);
                }
            }
        }
        // Remove those devices from the pool now (so the I/O thread stops driving
        // them) and move them into the op for off-thread teardown.
        let current: Vec<Box<dyn VirtualDevice>> = {
            let mut pool = self.shared_virtual_devices.lock().unwrap();
            let mut taken = Vec::new();
            let mut i = 0;
            while i < pool.len() {
                if device_ids.iter().any(|id| id == pool[i].id()) {
                    taken.push(pool.remove(i));
                } else {
                    i += 1;
                }
            }
            taken
        };
        for id in &device_ids {
            self.pending_device_ids.insert(id.clone());
            self.failed_device_ids.remove(id); // give them a fresh chance
        }
        let _ = self.device_ops.tx.send(crate::device_ops::DeviceOp::Reinstall {
            device_ids,
            current,
        });
    }

    /// The "Uninstall HIDMaestro drivers" confirm dialog. On confirm, tears down
    /// every HIDMaestro virtual device and removes the driver package — gamepads
    /// stop working until reinstalled.
    fn draw_uninstall_confirm(&mut self, ctx: &egui::Context) {
        if !self.uninstall_confirm_open {
            return;
        }
        let mut do_uninstall = false;
        let mut cancel = false;
        egui::Window::new("Uninstall HIDMaestro drivers")
            .id(egui::Id::new("uninstall_hm_confirm"))
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
            .show(ctx, |ui| {
                ui.label(
                    "This removes the HIDMaestro driver and all of its virtual controllers \
                     (DualShock 4 / DualSense / Xbox 360).",
                );
                ui.add_space(6.0);
                ui.label(
                    egui::RichText::new(
                        "Those controllers will stop working until you add one again (which \
                         reinstalls the driver). You'll be prompted for admin once.",
                    )
                    .small()
                    .color(egui::Color32::from_gray(170)),
                );
                ui.add_space(12.0);
                ui.horizontal(|ui| {
                    if ui.button("Uninstall").clicked() {
                        do_uninstall = true;
                    }
                    if ui.button("Cancel").clicked() {
                        cancel = true;
                    }
                });
            });

        if do_uninstall {
            self.uninstall_confirm_open = false;
            self.start_driver_uninstall();
        } else if cancel {
            self.uninstall_confirm_open = false;
        }
    }

    /// Collect the HIDMaestro virtual devices on every open canvas, pull them out
    /// of the shared pool, and enqueue an `Uninstall` op. The worker tears them
    /// down then removes the driver package; nothing is rebuilt.
    fn start_driver_uninstall(&mut self) {
        let mut device_ids: Vec<String> = Vec::new();
        for tab in &self.tabs {
            for id in snarl_virtual_device_ids(&tab.canvas.snarl) {
                if id.starts_with("virtual.hm.") && !device_ids.contains(&id) {
                    device_ids.push(id);
                }
            }
        }
        // Remove them from the pool now and move them into the op for off-thread
        // teardown (their Drop releases the device nodes before driver removal).
        let current: Vec<Box<dyn VirtualDevice>> = {
            let mut pool = self.shared_virtual_devices.lock().unwrap();
            let mut taken = Vec::new();
            let mut i = 0;
            while i < pool.len() {
                if device_ids.iter().any(|id| id == pool[i].id()) {
                    taken.push(pool.remove(i));
                } else {
                    i += 1;
                }
            }
            taken
        };
        // These ids are intentionally NOT marked pending — we are NOT rebuilding
        // them. They stay as canvas nodes (showing "installs driver" again) so the
        // user can re-add later.
        for id in &device_ids {
            self.pending_device_ids.remove(id);
        }
        let _ = self.device_ops.tx.send(crate::device_ops::DeviceOp::Uninstall { current });
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
        // Pin-on: AlwaysOnTop raises z-order but does NOT activate the
        // window, so Windows can leave us topmost-but-unfocused — keyboard
        // input and gamepad-nav focus paths then misbehave (the window is
        // pinned and on top yet acts unfocused). Activate ourselves so the
        // OS actually reports focus. (bring_hwnd_to_front also re-raises
        // z-order, harmless while we're already topmost.)
        if new_state {
            if let Some(hwnd) = self.self_hwnd {
                let _ = crate::process_list::bring_hwnd_to_front(hwnd);
            }
        }
        // Pin-off: drop our topmost synchronously via Win32. eframe
        // defers the `WindowLevel::Normal` command into winit's event
        // loop, so if we relied on it alone we'd still be in the
        // topmost band when bring_hwnd_to_front runs. Direct
        // SetWindowPos(self, HWND_NOTOPMOST) takes effect immediately.
        if !new_state {
            if let Some(hwnd) = self.self_hwnd {
                crate::process_list::drop_topmost(hwnd);
            }
            // Because pin-ON now actively makes FlexInput the foreground
            // window (see above), clearing topmost is not enough to drop us
            // from the front on pin-OFF: Windows keeps the active window
            // visually frontmost until some other window is activated, so
            // we'd stay stuck on top until the user clicked away and back.
            // We must therefore yield foreground to another window.
            //
            //  * flip-flop ON  → return focus to the specific window that was
            //    foreground when we pinned (the tweak-and-resume workflow).
            //  * flip-flop OFF → hand foreground to the last external window
            //    we saw so we fall out of the front naturally.
            //
            // Crucially this is DEFERRED, not done inline: eframe processes
            // the `WindowLevel::Normal` command above in winit *after* this
            // frame, and that re-activates us — doing the handoff now would
            // be immediately undone (target app blinks forward, FlexInput
            // snaps back). We stash the target and re-assert it over the next
            // few frames in `update()` once the level change has settled.
            let yield_to = if self.settings.focus_flip_flop {
                self.pin_prev_foreground_hwnd.take()
            } else {
                self.pin_last_external_hwnd
            };
            // hwnd 0 sentinel = no specific target; yield to whatever's below.
            self.pin_pending_yield = Some((yield_to.unwrap_or(0), 5));
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




    /// Device ids in `self.devices` that are FlexInput's OWN virtual output
    /// pads looping back through the OS as physical gamepads. These must be
    /// excluded from UI navigation (driving the UI from a device that mirrors
    /// our own output would create a feedback loop / double inputs). Uses the
    /// same reverse-walk match as the physical-list filter: for each virtual we
    /// emit, mark the *last* gilrs entry of that ControllerKind as ours (gilrs
    /// lists in plug order, so a real pad plugged before our virtual stays
    /// real). Returns ids regardless of the `show_own_virtuals_as_physical`
    /// setting — the exclusion applies whenever such a device is visible.
    fn own_virtual_device_ids(&self) -> std::collections::HashSet<String> {
        use std::collections::{HashMap, HashSet};
        let mut owned: HashSet<String> = HashSet::new();

        // Tier 1 — HIDMaestro (PS-family) pads are tagged with a `v`-prefixed
        // gilrs instance (`gilrs:dualsense:v0`) by the path-based classifier in
        // the devices backend. Identify them directly: no plug-order guessing,
        // and a real same-VID/PID controller is never mistaken for ours.
        for d in &self.devices {
            if is_own_virtual_gilrs_id(&d.id) {
                owned.insert(d.id.clone());
            }
        }

        // Tier 2 — ViGEmBus pads (`virtual.xinput`, `virtual.ds4`) have NO HID
        // instance path (they're bus-enumerated XUSB/DS4 targets), so the path
        // classifier can't see them. Fall back to the pool-count match the app
        // used before the classifier existed: for each ViGEm kind in our shared
        // pool, mark that many physical devices of the matching kind as ours,
        // walking from the end (virtuals enumerate after reals). Scoped to ViGEm
        // kinds and skips anything Tier 1 already claimed, so it never disturbs
        // the robust PS-family decision.
        let mut to_skip: HashMap<flexinput_devices::ControllerKind, usize> = HashMap::new();
        {
            let pool = self.shared_virtual_devices.lock().unwrap();
            for d in pool.iter() {
                let kind = match flexinput_virtual::kind_prefix(d.id()).as_str() {
                    "virtual.xinput" => Some(flexinput_devices::ControllerKind::XInput),
                    "virtual.ds4"    => Some(flexinput_devices::ControllerKind::DualShock4),
                    _ => None, // virtual.hm.* handled by Tier 1
                };
                if let Some(k) = kind { *to_skip.entry(k).or_insert(0) += 1; }
            }
        }
        for (k, n) in to_skip.iter() {
            let mut remaining = *n;
            for i in (0..self.devices.len()).rev() {
                if remaining == 0 { break; }
                let d = &self.devices[i];
                if d.kind == *k && !owned.contains(&d.id) {
                    owned.insert(d.id.clone());
                    remaining -= 1;
                }
            }
        }
        owned
    }



}



// ── Display node history update ───────────────────────────────────────────────

const HISTORY_LEN: usize = 20000;






#[cfg(test)]
mod macro_ordering_tests {
    use super::*;

    fn bare_node(module_id: &str) -> NodeData {
        NodeData {
            module_id: module_id.to_string(),
            display_name: module_id.to_string(),
            category: String::new(),
            inputs: vec![],
            outputs: vec![],
            params: HashMap::new(),
            subpatch: None,
            extra: Default::default(),
        }
    }

    // A Macro node has no wired inputs (in-degree 0), so without the explicit
    // publisher → macro edges the topo sort would evaluate it BEFORE the
    // Remapper that publishes its values — one tick of stale macro state.
    // Insertion order deliberately puts the macro node first to prove the
    // edge, not luck, produces the ordering. The sub-patch variant guards the
    // recursive containment scan on both sides.
    #[test]
    fn macro_nodes_evaluate_after_mapping_publishers() {
        let mut snarl: Snarl<NodeData> = Snarl::new();
        let mut mac = bare_node("module.macro");
        mac.outputs = vec![PinDescriptor::new("Ping", SignalType::Bool)];
        mac.params.insert("output_pin_ids".into(),
            serde_json::json!(["macro:aabbccdd"]));
        snarl.insert_node(egui::pos2(0.0, 0.0), mac);
        snarl.insert_node(egui::pos2(100.0, 0.0), bare_node("module.remapper"));

        let (graph, _) = build_processing_graph(&snarl, Default::default());
        let pos = |mid: &str| graph.nodes.iter().position(|s| s.module_id == mid).unwrap();
        assert!(
            pos("module.remapper") < pos("module.macro"),
            "remapper must evaluate before the macro node it feeds"
        );

        // Same guarantee when the macro node lives inside a sub-patch.
        let mut snarl: Snarl<NodeData> = Snarl::new();
        let mut sp_node = bare_node("subpatch");
        let mut sp = UiSubPatch::default();
        sp.snarl.insert_node(egui::pos2(0.0, 0.0), bare_node("module.macro"));
        sp_node.subpatch = Some(Box::new(sp));
        snarl.insert_node(egui::pos2(0.0, 0.0), sp_node);
        snarl.insert_node(egui::pos2(100.0, 0.0), bare_node("processing.gyro_3dof"));

        let (graph, _) = build_processing_graph(&snarl, Default::default());
        let pos = |mid: &str| graph.nodes.iter().position(|s| s.module_id == mid).unwrap();
        assert!(
            pos("processing.gyro_3dof") < pos("subpatch"),
            "lean publisher must evaluate before the sub-patch holding a macro node"
        );
    }
}
