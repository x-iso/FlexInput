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
    canvas::{sample_curve, Canvas, NodeData},
    canvas::node::{ExposedModule, UiSubPatch},
    canvas::ClipboardData,
    guide_watcher::{spawn_guide_watcher, GuideWatchConfig},
    panels::{physical_devices, virtual_devices::{SharedDevicePool, VirtualDevicePanel}},
    panic_hotkey::{load_panic_shortcut, save_panic_shortcut, spawn_panic_hotkey_listener},
    pin_hotkey::spawn_pin_hotkey_listener,
    settings::{self, AppSettings, PersistedTab, PersistedWorkspace, PinShortcut},
};

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
    /// Ephemeral Easy-mode UI state for this tab. Recomputed from the
    /// canvas on activation, not serialized to `.fxp`.
    pub easy_state: EasyState,
}

/// Per-tab transient state used only when Easy mode is active. Holds the
/// preset-switch confirmation flow and a cached "current preset identity"
/// so we can detect when the in-canvas sub-patch has been tweaked away
/// from the on-disk preset. None of this is persisted.
#[derive(Default)]
pub struct EasyState {
    /// Preset chip the user is hovering, used to highlight the chip.
    pub hovered_preset: Option<std::path::PathBuf>,
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
        Self {
            title: if n == 1 { "Untitled".to_string() } else { format!("Untitled {}", n) },
            file_path: None,
            bound_exes: vec![],
            canvas: Canvas::new(),
            virtual_panel: VirtualDevicePanel::new(),
            bypassed: false,
            auto_bypass: false,
            easy_state: EasyState::default(),
        }
    }
}

/// Collect every `device_id` string referenced by a `device.sink` node in
/// the given snarl that targets a virtual device (id starts with
/// `"virtual."`). Used to drive shared-pool reconciliation and the
/// active-tab id filter.
/// ctx-data slot holding the rect the inner-shadow gradient should hug.
/// Written during the CentralPanel pass: the content area below the tab
/// strip in Easy mode, or the canvas rect in Advanced mode. Read by
/// `paint_inner_window_shadow` after the frame's panels are laid out.
const INNER_SHADOW_RECT_KEY: &str = "flexinput::inner_shadow_rect";

/// Paint a pronounced inner-edge shadow: a true gradient that darkens
/// the edges of the *content* rect and fades smoothly to transparent
/// toward the center. Only painted while see-through is active — its
/// job is to ground the window dimensions against the desktop bleeding
/// through; in opaque mode the solid surfaces already read as edges.
///
/// The gradient is a real triangle mesh (an outer ring of vertices at
/// peak alpha, an inner ring at zero alpha) so the GPU interpolates the
/// alpha per-pixel — no visible stepping like a stack of strokes.
///
/// `content_rect` comes from `INNER_SHADOW_RECT_KEY`: the area below the
/// tab strip (Easy mode) or the canvas rect (Advanced mode).
fn paint_inner_window_shadow(ctx: &egui::Context) {
    use egui::epaint::{Mesh, Vertex};
    if !ctx.data(|d| d.get_temp::<bool>(
        egui::Id::new(crate::canvas::SEE_THROUGH_DATA_KEY)))
        .unwrap_or(false)
    {
        return;
    }
    let Some(rect) = ctx.data(|d| d.get_temp::<egui::Rect>(
        egui::Id::new(INNER_SHADOW_RECT_KEY)))
    else { return };
    if rect.width() <= 0.0 || rect.height() <= 0.0 { return; }

    // Pronounced: a ~28px band fading from opaque-ish black at the
    // edge to fully transparent inward.
    const BAND: f32 = 28.0;
    const PEAK_ALPHA: u8 = 130;
    let band = BAND.min(rect.width() * 0.5).min(rect.height() * 0.5);
    if band <= 0.0 { return; }

    let outer = rect;
    let inner = rect.shrink(band);
    let edge = egui::Color32::from_black_alpha(PEAK_ALPHA);
    let center = egui::Color32::TRANSPARENT;
    let uv = egui::epaint::WHITE_UV;

    let mut mesh = Mesh::default();
    // 8 vertices: 4 outer corners (edge color) then 4 inner corners
    // (transparent). The GPU interpolates alpha across each band.
    let mut push = |p: egui::Pos2, c: egui::Color32| {
        let idx = mesh.vertices.len() as u32;
        mesh.vertices.push(Vertex { pos: p, uv, color: c });
        idx
    };
    let o_tl = push(outer.left_top(), edge);
    let o_tr = push(outer.right_top(), edge);
    let o_br = push(outer.right_bottom(), edge);
    let o_bl = push(outer.left_bottom(), edge);
    let i_tl = push(inner.left_top(), center);
    let i_tr = push(inner.right_top(), center);
    let i_br = push(inner.right_bottom(), center);
    let i_bl = push(inner.left_bottom(), center);

    // Four trapezoidal bands (top, right, bottom, left), each two tris,
    // with edge verts at peak alpha and inner verts transparent.
    for &[a, b, c, d] in &[
        [o_tl, o_tr, i_tr, i_tl], // top
        [o_tr, o_br, i_br, i_tr], // right
        [o_br, o_bl, i_bl, i_br], // bottom
        [o_bl, o_tl, i_tl, i_bl], // left
    ] {
        mesh.indices.extend_from_slice(&[a, b, c, a, c, d]);
    }

    // Background order: this runs AFTER the CentralPanel pass, so within
    // the Background layer the shadow paints over the canvas / sub-patch
    // body content, but Background sits below `Middle` (floating windows)
    // and `Foreground` (menus / popups) — so the shadow stays behind all
    // of those rather than bleeding over them.
    let painter = ctx.layer_painter(egui::LayerId::new(
        egui::Order::Background,
        egui::Id::new("app_inner_shadow"),
    ));
    painter.add(mesh);
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

/// The physical-device `ControllerKind` that one of FlexInput's own virtual
/// devices enumerates as, used to filter our own pads out of the physical list
/// (and exclude them from nav). Returns `None` for backends that don't appear
/// in the gilrs physical list. Matches on the full virtual-device id (not the
/// 2-segment prefix) so the HIDMaestro variants — `virtual.hm.ds4`,
/// `virtual.hm.dualsense` — are classified correctly; the old prefix-only match
/// collapsed both to `virtual.hm` and let them leak into the physical panel.
fn own_virtual_kind(dev_id: &str) -> Option<flexinput_devices::ControllerKind> {
    use flexinput_devices::ControllerKind;
    // Strip any `.N` instance suffix by matching on the leading kind segments.
    if dev_id.starts_with("virtual.xinput") {
        Some(ControllerKind::XInput)
    } else if dev_id.starts_with("virtual.hm.dualsense") {
        Some(ControllerKind::DualSense)
    } else if dev_id.starts_with("virtual.hm.ds4") || dev_id.starts_with("virtual.ds4") {
        Some(ControllerKind::DualShock4)
    } else {
        None
    }
}

/// Insert virtual devices into the shared pool for every id in
/// `needed_ids` that doesn't already exist. Pre-existing devices are
/// reused — never duplicated. Devices the pool has but `needed_ids`
/// doesn't list are left alone (pruning is a separate operation).
/// Enqueue async `Create` ops for any needed device id that isn't already in the
/// pool and isn't already in flight. Non-blocking: the worker builds each device
/// (blocking helper IPC happens there) and the UI installs it into `pool` when
/// the result arrives (see `drain_device_op_results`). `pending` tracks in-flight
/// ids so we never enqueue a duplicate before the first lands.
fn reconcile_shared_devices(
    pool: &Mutex<Vec<Box<dyn VirtualDevice>>>,
    pending: &mut HashSet<String>,
    failed: &HashSet<String>,
    tx: &std::sync::mpsc::Sender<crate::device_ops::DeviceOp>,
    needed_ids: &[String],
) {
    let existing: HashSet<String> = {
        let pool = pool.lock().unwrap();
        pool.iter().map(|d| d.id().to_string()).collect()
    };
    for id in needed_ids {
        // Skip what's already built, in flight, or known-failed (failed ids are
        // only retried after the node is removed + re-added — see drain).
        if existing.contains(id) || pending.contains(id) || failed.contains(id) {
            continue;
        }
        // Only enqueue ids we can actually build (known kinds).
        if try_create_virtual_device_known(id) {
            pending.insert(id.clone());
            let _ = tx.send(crate::device_ops::DeviceOp::Create { device_id: id.clone() });
        }
    }
}

/// True if `id` names a known virtual device kind (mirrors the kind/instance
/// split in `try_create_virtual_device` without building anything).
fn try_create_virtual_device_known(id: &str) -> bool {
    let kind_id = match id.rfind('.') {
        Some(dot) => match id[dot + 1..].parse::<usize>() {
            Ok(_) => &id[..dot],
            Err(_) => id,
        },
        None => id,
    };
    flexinput_virtual::available_device_kinds()
        .iter()
        .any(|k| k.kind_id == kind_id)
}

/// Remove devices from the shared pool whose id is not referenced by any open
/// tab's canvas, and **return the removed `Box`es** so the caller can drop them
/// off the UI thread (a HIDMaestro device's `Drop` calls `helper::destroy`,
/// which blocks). Called after closing a tab / reloading a workspace. The pool
/// lock is acquired internally and released before returning.
fn take_unreferenced_devices(
    pool: &Mutex<Vec<Box<dyn VirtualDevice>>>,
    tabs: &[PatchTab],
) -> Vec<Box<dyn VirtualDevice>> {
    let mut keep: HashSet<String> = HashSet::new();
    for tab in tabs {
        for id in snarl_virtual_device_ids(&tab.canvas.snarl) {
            keep.insert(id);
        }
    }
    let mut pool = pool.lock().unwrap();
    let mut removed = Vec::new();
    let mut i = 0;
    while i < pool.len() {
        if keep.contains(pool[i].id()) {
            i += 1;
        } else {
            removed.push(pool.remove(i));
        }
    }
    removed
}

/// Take every device no longer referenced by the open tabs out of the pool and
/// hand it to the worker for off-thread teardown (so `helper::destroy` doesn't
/// block the UI). Also clears any matching in-flight `pending` create. No-op
/// when nothing needs removing.
fn prune_devices_async(
    pool: &Mutex<Vec<Box<dyn VirtualDevice>>>,
    pending: &mut HashSet<String>,
    tx: &std::sync::mpsc::Sender<crate::device_ops::DeviceOp>,
    tabs: &[PatchTab],
) {
    for dev in take_unreferenced_devices(pool, tabs) {
        let device_id = dev.id().to_string();
        pending.remove(&device_id);
        let _ = tx.send(crate::device_ops::DeviceOp::Destroy { device_id, device: dev });
    }
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
    /// Latest raw device signals (written by I/O thread at 500 Hz); used for canvas display.
    last_signals: HashMap<(String, String), Signal>,
    eval_cache: HashMap<(NodeId, usize), Option<Signal>>,
    logo_texture: Option<egui::TextureHandle>,
    hidhide: Option<HidHideClient>,
    last_update: std::time::Instant,
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
    /// Last device-op error, shown briefly in Settings. Cleared on next op.
    last_device_op_error: Option<String>,
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
    /// Gamepad UI-navigation runtime state (per-device toggle, selection/edit
    /// level, cursor, Alt+Tab switcher). Runtime-only — not serialized.
    gamepad_nav: crate::gamepad_nav::GamepadNav,
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
        setup_fonts(&cc.egui_ctx);
        // Install egui_extras image loaders so SVG images render inside nodes
        // and pinned sub-patch widgets. The svg feature pulls in resvg/usvg.
        egui_extras::install_image_loaders(&cc.egui_ctx);
        let descriptors = all_modules().into_iter().map(|r| r.descriptor).collect();
        let backends    = init_backends();
        let midi_backend = Arc::new(Mutex::new(Some(MidiBackend::new())));
        // HidHide integration disabled pending a proper rewrite.
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
        let app_settings = settings::load_settings();
        // Seed the HIDMaestro helper's persistence policy *before* any device is
        // created, so the helper's first `Hello` (and its orphan-cleanup
        // decision) reflects the user's setting. Off → helper removes leftovers
        // and tears down on app death; on → devices persist for reclaim.
        #[cfg(windows)]
        flexinput_hidmaestro::helper::set_persist(app_settings.persist_virtual_devices);
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
            }
        };
        // A crash-recovery snapshot takes precedence over the opt-in workspace:
        // its presence means the last run did NOT exit cleanly (a GPU-loss
        // relaunch or hard crash), so restoring it is how the relaunch becomes
        // seamless. It's consumed exactly once — deleted immediately after we
        // read it — and is honored regardless of `keep_workspace`. If absent,
        // we fall back to the opt-in workspace, then to one empty tab.
        let tabs = if let Some(ws) = settings::load_recovery().filter(|ws| !ws.tabs.is_empty()) {
            eprintln!("Restoring {} tab(s) from crash-recovery snapshot.", ws.tabs.len());
            settings::delete_recovery();
            ws.tabs.into_iter().map(persisted_tab_to_patch).collect()
        } else if app_settings.keep_workspace {
            match settings::load_workspace() {
                Some(ws) if !ws.tabs.is_empty() => {
                    ws.tabs.into_iter().map(persisted_tab_to_patch).collect()
                }
                _ => vec![PatchTab::new_untitled(1)],
            }
        } else {
            vec![PatchTab::new_untitled(1)]
        };
        let shared_devices = Arc::new(RwLock::new(Vec::<PhysicalDevice>::new()));
        let shared_midi_devices = Arc::new(RwLock::new(Vec::<PhysicalDevice>::new()));
        let pinned_midi_ids = Arc::new(RwLock::new(HashSet::<String>::new()));
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
        // whose id is in this set. Seeded from tab 0's canvas.
        let active_tab_device_ids: Arc<RwLock<HashSet<String>>> = Arc::new(RwLock::new(
            snarl_virtual_device_ids(&tabs[0].canvas.snarl).into_iter().collect(),
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
            Arc::clone(&ping_requests),
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

        let mut app = Self {
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
            // Seeded below from the restored tabs so the first frame doesn't
            // pointlessly rewrite the recovery snapshot we may have just loaded.
            last_recovery_mutation_gen: 0,
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
            device_ops,
            pending_device_ids,
            failed_device_ids: HashSet::new(),
            reinstall_confirm_open: false,
            last_device_op_error: None,
            active_tab_device_ids,
            io_bypass,
            ui_nav_suppress,
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
            gamepad_nav: crate::gamepad_nav::GamepadNav::default(),
            sample_rate_hz,
            polling_hz,
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
            pin_prev_foreground_hwnd: None,
            pin_last_external_hwnd: None,
            pin_pending_yield: None,
            self_hwnd: None,
            profiler_server: None,
            #[cfg(debug_assertions)]
            last_logged_repaint_hz: None,
            theme_applied_for: None,
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

        // ── GPU-loss recovery ────────────────────────────────────────────
        // The vendored egui-wgpu raises this flag (instead of panicking) when
        // the graphics device is lost mid-frame — a fullscreen game resetting
        // the device, a driver TDR, or VRAM exhaustion. eframe 0.33 can't
        // rebuild the device in place, so we recover by relaunching a fresh
        // process. Our latest work is already on disk via the always-on
        // recovery snapshot, so the new instance restores it seamlessly. This
        // is the graceful path; the panic hook in `app/src/main.rs` is the
        // last-ditch net for any loss we didn't convert. Checked first thing so
        // we never try to render another frame on the dead device.
        if eframe::egui_wgpu::GPU_LOST.load(std::sync::atomic::Ordering::SeqCst) {
            eprintln!("GPU device lost — saving recovery snapshot and relaunching.");
            // Force a final snapshot regardless of the dirty signal, then hand
            // off to a fresh process and exit this one.
            settings::save_recovery(&self.build_persisted_workspace());
            settings::save_settings(&self.settings);
            crate::relaunch_self_and_exit();
        }

        // ── Apply finished device-ops worker results ─────────────────────
        // Pushes freshly-built virtual devices into the shared pool (and clears
        // in-flight markers). Done before anything reads the pool this frame.
        self.drain_device_op_results();

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

        // Signal routing and device flushing are handled by the 500 Hz I/O thread.
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
        self.draw_gp_settings_panel(ctx);
        self.draw_kbm_picker(ctx);
        self.draw_press_mode_picker(ctx);
        self.draw_reinstall_confirm(ctx);
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
            if let Some(saved_path) = self.tabs[self.active_tab].canvas
                .save_patch(vids, bound, auto_bypass, preset_path)
            {
                let tab = &mut self.tabs[self.active_tab];
                tab.title = saved_path.file_stem()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_else(|| "Untitled".to_string());
                tab.file_path = Some(saved_path);
            }
        }
        if do_load {
            if let Some((new_canvas, vids, bound, auto_bypass, path, preset_path)) =
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
            if let Some(path) = rfd::FileDialog::new()
                .add_filter("FlexInput Workspace", &["fxw", "json"])
                .set_file_name("workspace.fxw")
                .save_file()
            {
                let tabs: Vec<PersistedTab> = self.tabs.iter().map(|t| PersistedTab {
                    title: t.title.clone(),
                    file_path: t.file_path.clone(),
                    bound_exes: t.bound_exes.clone(),
                    auto_bypass: t.auto_bypass,
                    snarl: t.canvas.snarl.clone(),
                    easy_preset_path: t.easy_state.loaded_preset.as_ref().map(|(p, _)| p.clone()),
                }).collect();
                let ws = PersistedWorkspace {
                    version: 1,
                    active_tab: self.active_tab,
                    tabs,
                };
                if let Err(e) = settings::save_workspace_to(&ws, &path) {
                    eprintln!("[workspace] save failed: {e}");
                }
            }
        }

        // ── Load workspace (full tab set, replacing current state) ───────────
        if do_load_workspace {
            if let Some(path) = rfd::FileDialog::new()
                .add_filter("FlexInput Workspace", &["fxw", "json"])
                .pick_file()
            {
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
        let devices_owned;
        let devices: &[PhysicalDevice] = if self.settings.show_own_virtuals_as_physical {
            &self.devices
        } else {
            let mut to_skip: HashMap<flexinput_devices::ControllerKind, usize> = HashMap::new();
            {
                let pool = self.shared_virtual_devices.lock().unwrap();
                for d in pool.iter() {
                    if let Some(k) = own_virtual_kind(d.id()) {
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
        // Device ids that are FlexInput's own virtual pads (visible in the
        // physical list via `show_own_virtuals_as_physical`). Their nav toggle is
        // grayed + forced off so navigating from our own loopback output can't
        // create a feedback loop. Computed before the panel borrows below.
        let nav_excluded_ids = self.own_virtual_device_ids();
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

        let device_defaults_for_easy = crate::canvas::DeviceParamDefaults {
            stick_deadzone: self.settings.default_stick_deadzone,
            gyro_mult: self.settings.default_gyro_mult,
            mouse_sensitivity: self.settings.default_mouse_sensitivity,
        };
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
        let device_defaults = crate::canvas::DeviceParamDefaults {
            stick_deadzone: self.settings.default_stick_deadzone,
            gyro_mult: self.settings.default_gyro_mult,
            mouse_sensitivity: self.settings.default_mouse_sensitivity,
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
                let side_w = 280.0_f32.min(total.width() * 0.5);
                let gap = 6.0_f32;
                let left_rect = egui::Rect::from_min_size(
                    total.min,
                    egui::vec2(side_w, total.height()),
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
                ui.painter().rect_filled(left_rect, 0.0, left_fill);
                ui.scope_builder(egui::UiBuilder::new().max_rect(left_rect), |ui| {
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

        // ── Sub-patch editor windows ──────────────────────────────────────────
        {
            puffin::profile_scope!("show_subpatch_editors");
            show_subpatch_editors(self, ctx, &live_device_ids);
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
enum NavWidgetKind {
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
}

/// Identifies a setting in the gamepad-native settings panel for get/set.
#[derive(Clone, Copy, PartialEq, Eq)]
enum GpSettingKey {
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
enum NavStep {
    /// Decade-quantized (ms / sample counts): step size grows by powers of ten
    /// with the value's magnitude. <10 → 1 (fine 0.1); 10–100 → 5 (fine 1);
    /// 100–1000 → 50 (fine 10); etc.
    Decade,
    /// Plain proportional (0..1-style params like phase): fraction of the value.
    Linear,
}

/// Numeric-edit descriptor for a generic nav-editable widget element.
#[derive(Clone, Copy)]
struct NavParamSpec {
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
enum NavField {
    Value { key: &'static str, lo: f32, hi: f32, default: f32, step: NavStep },
    Enum  { key: &'static str, opts: &'static [&'static str] },
    Toggle { key: &'static str },
    /// Writes two params at once from a chosen option (label, value_a, value_b)
    /// — for the gyro mode rows that set family+axis together.
    EnumPair { key_a: &'static str, key_b: &'static str,
               opts: &'static [(&'static str, &'static str, &'static str)] },
}

#[derive(Clone)]
struct NavFieldDef {
    label: &'static str,
    field: NavField,
}

/// How a gamepad-settings row is edited.
enum GpSettingKind {
    Toggle { key: GpSettingKey },
    IntSlider { lo: f32, hi: f32, step: f32, key: GpSettingKey },
    FloatSlider { lo: f32, hi: f32, step: f32, key: GpSettingKey },
    /// Discrete cycle through a fixed list of (value, label) pairs. The
    /// underlying value is stored as `f32` (matching IntSlider) but the
    /// display string comes from the label. Used for enum settings where
    /// a slider doesn't make sense (e.g. Repaint rate: Monitor / 60 / 30 / 15 Hz).
    Cycle { key: GpSettingKey, opts: &'static [(f32, &'static str)] },
    /// Gamepad shortcut chord: South closes the panel and starts a chord
    /// capture for `target` (so the user can press the combo with the panel
    /// out of the way). Displays the currently-assigned combo.
    ChordLearn { target: crate::gamepad_nav::ChordTarget },
}

/// One row in the gamepad-native settings panel.
struct GpSettingRow {
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
        if nav.is_rising("btn_lb") && self.active_tab > 0 {
            self.set_active_tab(self.active_tab - 1);
        }
        if nav.is_rising("btn_rb") && self.active_tab + 1 < self.tabs.len() {
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

        // ── Left I/O panel navigation (cursor-driven) ────────────────────────
        // When the RS/gyro cursor is over a left-panel target, South/RT acts on
        // it (select input device, toggle output, enter slider edit). While a
        // slider edit is in progress, the controller drives that value and the
        // sub-patch handling is skipped. Returns true when it consumed input.
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
                | crate::gamepad_nav::EditLevel::RemapCard)
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
                            NavWidgetKind::Curve | NavWidgetKind::Remapper
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
        }
    }

    /// Draw the mapping-card selection glow at top level using the GLOBAL rects
    /// the remapper body published last frame (`gp_nav_remap_card_rects`). Must
    /// run outside the body's child layer — painting from inside it deadlocks
    /// epaint's graphics lock.
    fn nav_draw_remap_card_glow(&self, ctx: &egui::Context, outer_id: egui_snarl::NodeId) {
        let Some(inner) = self.nav_selected_inner_node(outer_id) else { return; };
        let scope = self.nav_remap_mappings_key(outer_id);
        let cur_pass = ctx.cumulative_pass_nr();
        let accent = ctx.style().visuals.selection.stroke.color;
        let [r, g, b, _] = accent.to_array();
        let painter = ctx.layer_painter(egui::LayerId::new(
            egui::Order::Foreground, egui::Id::new(("gp_nav_remap_card_glow", outer_id.0))));
        let ring = |rect: egui::Rect, round: f32, peak: f32, max_grow: f32| {
            if !rect.is_finite() || rect.width() < 1.0 { return; }
            let n = 6;
            for i in 0..n {
                let t = (i as f32 + 1.0) / n as f32;
                let grow = t * max_grow;
                let a = (peak * (1.0 - t)).round() as u8;
                if a == 0 { continue; }
                painter.rect_stroke(rect.expand(grow), round + grow,
                    egui::Stroke::new(2.0, egui::Color32::from_rgba_unmultiplied(r, g, b, a)),
                    egui::StrokeKind::Outside);
            }
            painter.rect_stroke(rect.expand(1.0), round,
                egui::Stroke::new(2.0, accent), egui::StrokeKind::Outside);
        };

        // Card glow (selected/entered card + focused header field).
        if let Some((pass, card, field, entered)) = ctx.data(|d|
            d.get_temp::<(u64, egui::Rect, Option<egui::Rect>, bool)>(
                egui::Id::new(("gp_nav_remap_card_rects", inner.0, scope))))
        {
            if cur_pass.saturating_sub(pass) <= 2 {
                ring(card, 5.0, if entered { 130.0 } else { 90.0 }, 7.0);
                if entered {
                    if let Some(fr) = field { ring(fr, 4.0, 160.0, 6.0); }
                }
            }
        }

        // Action-button glow (Learn / Special / Add) when an action is focused.
        let act_sel: Option<usize> = ctx.data(|d|
            d.get_temp::<(u64, usize)>(egui::Id::new(("gp_nav_remap_action", inner.0, scope))))
            .filter(|(p, _)| cur_pass.saturating_sub(*p) <= 2)
            .map(|(_, i)| i)
            .filter(|i| *i != usize::MAX);
        if let Some(ai) = act_sel {
            if let Some((pass, rects)) = ctx.data(|d|
                d.get_temp::<(u64, Vec<egui::Rect>)>(egui::Id::new(("gp_nav_action_rects", inner.0, scope))))
            {
                if cur_pass.saturating_sub(pass) <= 2 {
                    if let Some(rect) = rects.get(ai) { ring(*rect, 4.0, 130.0, 6.0); }
                }
            }
        }
    }

    /// Mappings array key for the selected remapper-family node. Remapper /
    /// Map Action / Combiner use `"mappings"`; gyro lean sections use
    /// `"lean_left"` / `"lean_right"` keyed by the pinned element id.
    fn nav_remap_mappings_key(&self, outer_id: egui_snarl::NodeId) -> &'static str {
        match self.nav_selected_element(outer_id).as_ref().map(|(_, e)| e.as_str()) {
            Some("lean_left") => "lean_left",
            Some("lean_right") => "lean_right",
            _ => "mappings",
        }
    }

    /// Number of mapping cards in the selected remapper-family widget.
    fn nav_remap_card_count(&self, outer_id: egui_snarl::NodeId) -> usize {
        let key = self.nav_remap_mappings_key(outer_id);
        let Some(inner) = self.nav_selected_inner_node(outer_id) else { return 0; };
        let canvas = &self.tabs[self.active_tab].canvas;
        canvas.snarl.get_node(outer_id)
            .and_then(|n| n.subpatch.as_ref())
            .and_then(|sp| sp.snarl.get_node(inner))
            .and_then(|node| node.params.get(key).and_then(|v| v.as_array()))
            .map(|a| a.len()).unwrap_or(0)
    }

    /// Mutable access to the selected node's mappings array, runs `f` on it, and
    /// returns whether `f` reported a change (writing back only then).
    fn nav_remap_with_mappings<F>(&mut self, outer_id: egui_snarl::NodeId, f: F) -> bool
    where F: FnOnce(&mut Vec<serde_json::Value>) -> bool {
        let key = self.nav_remap_mappings_key(outer_id);
        let Some(inner) = self.nav_selected_inner_node(outer_id) else { return false; };
        let canvas = &mut self.tabs[self.active_tab].canvas;
        let Some(sp) = canvas.snarl.get_node_mut(outer_id).and_then(|n| n.subpatch.as_mut()) else { return false; };
        let Some(node) = sp.snarl.get_node_mut(inner) else { return false; };
        let mut arr = node.params.get(key).and_then(|v| v.as_array()).cloned().unwrap_or_default();
        let changed = f(&mut arr);
        if changed {
            node.params.insert(key.to_string(), serde_json::Value::Array(arr));
        }
        changed
    }

    /// Mutate card `idx` as an object map, UPGRADING a legacy `Array<String>`
    /// entry (Map Action's old format) to `{ in: [strings] }` first so the edit
    /// lands. Without this, press-mode / toggle edits on never-yet-edited Map
    /// Action cards silently no-op (the entry isn't an object). Returns whether
    /// `f` reported a change.
    fn nav_remap_card_obj_mut<F>(&mut self, outer_id: egui_snarl::NodeId, idx: usize, f: F) -> bool
    where F: FnOnce(&mut serde_json::Map<String, serde_json::Value>) -> bool {
        self.nav_remap_with_mappings(outer_id, |arr| {
            let Some(slot) = arr.get_mut(idx) else { return false; };
            // Upgrade legacy [strings] → { in: [strings] }.
            if let Some(pins) = slot.as_array().map(|a| a.clone()) {
                let mut obj = serde_json::Map::new();
                obj.insert("in".to_string(), serde_json::Value::Array(pins));
                *slot = serde_json::Value::Object(obj);
            }
            match slot.as_object_mut() {
                Some(m) => f(m),
                None => false,
            }
        })
    }

    /// Read a card's `mode` string (default "down").
    fn nav_remap_card_mode(&self, outer_id: egui_snarl::NodeId, idx: usize) -> Option<String> {
        let key = self.nav_remap_mappings_key(outer_id);
        let inner = self.nav_selected_inner_node(outer_id)?;
        let canvas = &self.tabs[self.active_tab].canvas;
        let arr = canvas.snarl.get_node(outer_id)?.subpatch.as_ref()?
            .snarl.get_node(inner)?.params.get(key)?.as_array()?;
        let m = arr.get(idx)?;
        Some(m.get("mode").and_then(|v| v.as_str()).unwrap_or("down").to_string())
    }

    /// Read a card's bool field (default false).
    fn nav_remap_card_bool(&self, outer_id: egui_snarl::NodeId, idx: usize, key: &str) -> bool {
        let mkey = self.nav_remap_mappings_key(outer_id);
        let Some(inner) = self.nav_selected_inner_node(outer_id) else { return false; };
        let canvas = &self.tabs[self.active_tab].canvas;
        canvas.snarl.get_node(outer_id).and_then(|n| n.subpatch.as_ref())
            .and_then(|sp| sp.snarl.get_node(inner))
            .and_then(|node| node.params.get(mkey).and_then(|v| v.as_array()))
            .and_then(|a| a.get(idx))
            .and_then(|m| m.get(key).and_then(|v| v.as_bool()))
            .unwrap_or(false)
    }

    fn nav_remap_set_card_bool(&mut self, outer_id: egui_snarl::NodeId, idx: usize, key: &str, val: bool) {
        let key = key.to_string();
        self.nav_remap_card_obj_mut(outer_id, idx, |m| {
            m.insert(key, serde_json::Value::Bool(val));
            true
        });
    }

    /// Delete card `idx`.
    fn nav_remap_delete_card(&mut self, outer_id: egui_snarl::NodeId, idx: usize) -> bool {
        self.nav_remap_with_mappings(outer_id, |arr| {
            if idx < arr.len() { arr.remove(idx); true } else { false }
        })
    }

    /// Reset card `idx` to the default press mode (down) + clear its params.
    fn nav_remap_reset_card(&mut self, outer_id: egui_snarl::NodeId, idx: usize) -> bool {
        self.nav_remap_card_obj_mut(outer_id, idx, |m| {
            let had = m.contains_key("mode") || m.contains_key("window_ms")
                || m.contains_key("sustain") || m.contains_key("turbo");
            m.remove("mode");
            m.remove("window_ms");
            m.remove("sustain");
            m.remove("turbo");
            had
        })
    }

    /// Press modes in popup order (analog is offered for all remapper-family +
    /// lean widgets, which are the only ones with editable cards). Shared by the
    /// inline cycle and the press-mode picker modal.
    const PRESS_MODES: &'static [&'static str] =
        &["down","short","long","double","on_press","on_release","analog"];

    /// Set card `idx`'s press mode to `mode` directly, applying the same
    /// default-param fixups the renderer's popup does.
    fn nav_remap_set_mode(&mut self, outer_id: egui_snarl::NodeId, idx: usize, mode: &str) {
        let mode = mode.to_string();
        self.nav_remap_card_obj_mut(outer_id, idx, |m| {
            if mode == "down" {
                m.remove("mode");
                m.remove("window_ms");
                m.remove("sustain");
            } else {
                m.insert("mode".into(), serde_json::Value::String(mode.clone()));
                if !m.contains_key("window_ms") {
                    m.insert("window_ms".into(), serde_json::json!(200.0));
                }
            }
            true
        });
    }

    /// Cycle card `idx`'s press mode by `dir`, applying the same default-param
    /// fixups the card renderer's popup does (down clears window_ms/sustain;
    /// other modes seed window_ms). Mirrors the analog availability rule.
    fn nav_remap_cycle_mode(&mut self, outer_id: egui_snarl::NodeId, idx: usize, dir: i32) {
        let modes = Self::PRESS_MODES;
        self.nav_remap_card_obj_mut(outer_id, idx, |m| {
            let cur = m.get("mode").and_then(|v| v.as_str()).unwrap_or("down");
            let ci = modes.iter().position(|x| *x == cur).unwrap_or(0) as i32;
            let next = modes[(ci + dir).rem_euclid(modes.len() as i32) as usize];
            if next == "down" {
                m.remove("mode");
                m.remove("window_ms");
                m.remove("sustain");
            } else {
                m.insert("mode".into(), serde_json::Value::String(next.to_string()));
                if !m.contains_key("window_ms") {
                    m.insert("window_ms".into(), serde_json::json!(200.0));
                }
            }
            true
        });
    }

    /// Nudge card `idx`'s `window_ms` by `delta`, clamped to 10..5000.
    fn nav_remap_nudge_window(&mut self, outer_id: egui_snarl::NodeId, idx: usize, delta: f32) {
        self.nav_remap_card_obj_mut(outer_id, idx, |m| {
            let cur = m.get("window_ms").and_then(|v| v.as_f64()).unwrap_or(200.0) as f32;
            let next = (cur + delta).clamp(10.0, 5000.0);
            m.insert("window_ms".into(), serde_json::json!(next as f64));
            true
        });
    }

    /// Drive a remapper-family widget the user has ENTERED (RemapScroll level):
    /// up/down moves the SELECTED CARD, North resets it (or arms Learn when the
    /// list is empty), West deletes it, South ENTERS it (`RemapCard`), LT/RT
    /// cycle the filter, East exits. The selected card is published for the body
    /// to glow + auto-scroll into view.
    fn nav_drive_remapper(
        &mut self,
        ctx: &egui::Context,
        outer_id: egui_snarl::NodeId,
        nav: &crate::gamepad_nav::NavInput,
        _dt: f32,
        step_dir: Option<crate::gamepad_nav::NavDir>,
        rt_rising: bool,
        lt_rising: bool,
    ) {
        use crate::gamepad_nav::{EditLevel, NavDir};
        let Some(inner) = self.nav_selected_inner_node(outer_id) else {
            self.gamepad_nav.edit_level = EditLevel::Widget;
            return;
        };

        // While capture is ARMED or IN PROGRESS (the user pressed Learn), the
        // nav driver goes INERT for this device so the raw button/combo reaches
        // the capture machine instead of being eaten as navigation. Capture
        // latches on release inside the body; we resume once it leaves the
        // capturing/learning phases (and the arm clears). Selection is still
        // published so the glow persists.
        // Inert ONLY while a nav-mode capture is actually ARMED. The arm now
        // persists through the whole chord (it clears on latch, not at capture
        // start), so `armed` alone covers the entire capture window. We must NOT
        // gate on phase=="capturing" alone — Map Action auto-sits in "capturing"
        // whenever wired, so doing so would block East/South forever (the user's
        // "stuck in Map Action" bug).
        // Lean sections arm via side-scoped `_lean_<side>_armed`; everything else
        // via `_nav_capture_armed`.
        let armed_key = match self.nav_lean_side(outer_id) {
            Some("left") => "_lean_left_armed",
            Some("right") => "_lean_right_armed",
            _ => "_nav_capture_armed",
        };
        let armed = self.get_subpatch_param_bool(outer_id, inner, armed_key).unwrap_or(false);
        if armed {
            self.nav_publish_remap_selection(ctx, outer_id, inner);
            ctx.request_repaint();
            return;
        }

        // East exits the widget (only when not mid-capture).
        if nav.is_rising("btn_east") {
            self.gamepad_nav.edit_level = EditLevel::Widget;
            return;
        }

        // LT/RT cycle the mapping filter.
        if rt_rising || lt_rising {
            let dir = if rt_rising { 1 } else { -1 };
            crate::canvas::viewer::nav_cycle_remapper_filter(ctx, inner.0, dir);
        }

        // Two navigation rows:
        //   • the ACTION row (Learn / Special / Add) — navigated LEFT/RIGHT,
        //     since the buttons sit side-by-side; cursor 0..n_actions.
        //   • the CARD list below it — navigated UP/DOWN; cursor n_actions..total.
        // Up from the first card lands on the action row (keeping the horizontal
        // sub-index where possible); Down from the action row lands on the first
        // card. South activates the focused item; West deletes a focused card.
        let actions = self.nav_remap_action_items(outer_id, inner);
        let n_actions = actions.len();
        let count = self.nav_remap_card_count(outer_id);
        let total = n_actions + count;
        if total == 0 { return; }
        if self.gamepad_nav.card_index >= total { self.gamepad_nav.card_index = total - 1; }

        let cur = self.gamepad_nav.card_index;
        let on_actions = cur < n_actions;
        let new_cur = match step_dir {
            Some(NavDir::Left) if on_actions => cur.saturating_sub(1),
            Some(NavDir::Right) if on_actions => (cur + 1).min(n_actions.saturating_sub(1)),
            // Down: from the action row → first card; within cards → next card.
            Some(NavDir::Down) => {
                if on_actions { n_actions.min(total - 1) }
                else { (cur + 1).min(total - 1) }
            }
            // Up: within cards → prev card; from the first card → action row.
            Some(NavDir::Up) => {
                if on_actions { cur } // already at the top row
                else if cur == n_actions { if n_actions > 0 { 0 } else { cur } }
                else { cur - 1 }
            }
            _ => cur,
        };
        self.gamepad_nav.card_index = new_cur;
        let sel = new_cur;

        if sel < n_actions {
            // An action button/dropdown is focused → South activates it.
            if nav.is_rising("btn_south") {
                let action = actions[sel];
                // Special (Remapper or Lean) opens the virtual KB/M picker
                // instead of the mouse-only ComboBox. The picker writes its chord
                // into the widget's draft param — `draft_output` for the
                // Remapper, `_lean_<side>_draft` for a Lean section.
                let lean_draft = match action {
                    "_nav_act_special_left" => Some("_lean_left_draft"),
                    "_nav_act_special_right" => Some("_lean_right_draft"),
                    _ => None,
                };
                if action == "_nav_act_special" || lean_draft.is_some() {
                    self.gamepad_nav.kbm_picker_open = true;
                    self.gamepad_nav.kbm_picker_idx = 0;
                    self.gamepad_nav.kbm_picker_node = Some(inner);
                    self.gamepad_nav.kbm_picker_outer = Some(outer_id);
                    self.gamepad_nav.kbm_picker_draft_key =
                        lean_draft.unwrap_or("draft_output").to_string();
                    self.gamepad_nav.kbm_picker_phase_key =
                        match action {
                            "_nav_act_special_left" => Some("_lean_left_phase"),
                            "_nav_act_special_right" => Some("_lean_right_phase"),
                            _ => None,
                        }.map(|s| s.to_string());
                } else {
                    self.set_subpatch_param_bool(outer_id, inner, action, true);
                }
                ctx.request_repaint();
            }
        } else {
            // A mapping card is focused.
            let card_idx = sel - n_actions;
            if nav.is_rising("btn_west") {
                let base = self.tabs[self.active_tab].canvas.snapshot_for_undo();
                if self.nav_remap_delete_card(outer_id, card_idx) {
                    self.tabs[self.active_tab].canvas.commit_undo_if_changed(base);
                }
            } else if nav.is_rising("btn_south") {
                self.gamepad_nav.edit_level = EditLevel::RemapCard;
                self.gamepad_nav.remap_card = card_idx;
                self.gamepad_nav.card_field = 0;
                self.gamepad_nav.edit_baseline = Some(Box::new(
                    self.tabs[self.active_tab].canvas.snapshot_for_undo()));
            }
        }

        self.nav_publish_remap_selection(ctx, outer_id, inner);
    }

    /// Drive the virtual KB/M picker (modal). `step_dir` is the resolved
    /// dpad/stick direction this frame. South appends the focused pin to the
    /// remapper's `draft_output`; North resets `draft_output`; East closes.
    fn drive_kbm_picker(
        &mut self,
        step_dir: Option<crate::gamepad_nav::NavDir>,
        nav: &crate::gamepad_nav::NavInput,
    ) {
        use crate::gamepad_nav::NavDir;
        use crate::kbm_picker::{clamp_index, nearest_in_dir, KBM_LAYOUT};

        // East closes the picker.
        if nav.is_rising("btn_east") {
            self.gamepad_nav.kbm_picker_open = false;
            return;
        }
        // Spatial navigation: move to the nearest cell in the pressed direction.
        // The layout has separated clusters (nav cluster + arrows + mouse to the
        // right), so we navigate by cell geometry rather than row/col stepping.
        let mut idx = clamp_index(self.gamepad_nav.kbm_picker_idx);
        idx = match step_dir {
            Some(NavDir::Left)  => nearest_in_dir(idx, -1.0, 0.0),
            Some(NavDir::Right) => nearest_in_dir(idx, 1.0, 0.0),
            Some(NavDir::Up)    => nearest_in_dir(idx, 0.0, -1.0),
            Some(NavDir::Down)  => nearest_in_dir(idx, 0.0, 1.0),
            None => idx,
        };
        self.gamepad_nav.kbm_picker_idx = idx;

        let Some(outer) = self.gamepad_nav.kbm_picker_outer else {
            self.gamepad_nav.kbm_picker_open = false; return; };
        let Some(inner) = self.gamepad_nav.kbm_picker_node else {
            self.gamepad_nav.kbm_picker_open = false; return; };
        let draft_key = self.gamepad_nav.kbm_picker_draft_key.clone();
        let phase_key = self.gamepad_nav.kbm_picker_phase_key.clone();

        // North resets the output chord.
        if nav.is_rising("btn_north") {
            self.set_subpatch_param_str_array(outer, inner, &draft_key, &[]);
            return;
        }
        // South appends the focused pin (de-duped) + flips the widget into the
        // phase that shows the draft + enables Add, WITHOUT running the gamepad
        // capture machine (so the South used to pick isn't swept into the chord).
        if nav.is_rising("btn_south") {
            let pin = KBM_LAYOUT[idx].pin.to_string();
            let mut out = self.nav_remap_draft_vec(outer, inner, &draft_key);
            if !out.iter().any(|p| *p == pin) { out.push(pin); }
            self.set_subpatch_param_str_array(outer, inner, &draft_key, &out);
            // Lean uses `_lean_<side>_phase` → "ready" (non-capturing display).
            // Remapper uses `ui_phase` → "learning" (its Add-able output state);
            // its capture machine is gated by `capture_ok` (armed&&idle), and we
            // did NOT arm here, so picking via the picker never sweeps gamepad
            // presses into the output chord.
            match phase_key.as_deref() {
                Some(pk) => self.set_subpatch_param_str(outer, inner, pk, "ready"),
                None => self.set_subpatch_param_str(outer, inner, "ui_phase", "learning"),
            }
        }
    }

    /// Drive the press-mode picker modal: up/down move the highlight, South
    /// applies the highlighted mode to the target card (and closes), East
    /// cancels.
    fn drive_press_mode_picker(
        &mut self,
        step_dir: Option<crate::gamepad_nav::NavDir>,
        nav: &crate::gamepad_nav::NavInput,
    ) {
        use crate::gamepad_nav::NavDir;
        if nav.is_rising("btn_east") {
            self.gamepad_nav.press_mode_open = false;
            return;
        }
        let n = Self::PRESS_MODES.len();
        let mut i = self.gamepad_nav.press_mode_idx.min(n - 1);
        match step_dir {
            Some(NavDir::Up)   => i = i.saturating_sub(1),
            Some(NavDir::Down) => i = (i + 1).min(n - 1),
            _ => {}
        }
        self.gamepad_nav.press_mode_idx = i;

        if nav.is_rising("btn_south") {
            if let Some(outer) = self.gamepad_nav.press_mode_outer {
                let card = self.gamepad_nav.press_mode_card;
                let mode = Self::PRESS_MODES[i];
                self.nav_remap_set_mode(outer, card, mode);
            }
            self.gamepad_nav.press_mode_open = false;
        }
    }

    /// Read a string-array draft as a Vec<String>.
    fn nav_remap_draft_vec(&self, outer: egui_snarl::NodeId, inner: egui_snarl::NodeId, key: &str)
        -> Vec<String>
    {
        let canvas = &self.tabs[self.active_tab].canvas;
        canvas.snarl.get_node(outer).and_then(|n| n.subpatch.as_ref())
            .and_then(|sp| sp.snarl.get_node(inner))
            .and_then(|node| node.params.get(key).and_then(|v| v.as_array()))
            .map(|a| a.iter().filter_map(|v| v.as_str().map(|s| s.to_string())).collect())
            .unwrap_or_default()
    }

    /// Write a string-array param on an inner sub-patch node.
    fn set_subpatch_param_str_array(&mut self, outer: egui_snarl::NodeId,
        inner: egui_snarl::NodeId, key: &str, vals: &[String])
    {
        let canvas = &mut self.tabs[self.active_tab].canvas;
        let Some(sp) = canvas.snarl.get_node_mut(outer).and_then(|n| n.subpatch.as_mut()) else { return; };
        if let Some(node) = sp.snarl.get_node_mut(inner) {
            let arr: Vec<serde_json::Value> = vals.iter()
                .map(|s| serde_json::Value::String(s.clone())).collect();
            node.params.insert(key.to_string(), serde_json::Value::Array(arr));
        }
    }

    /// Ordered list of active action-button activation keys for the current
    /// phase. MUST match, in order and presence, the buttons the body renders
    /// (and the order it publishes their rects): Learn, Clear, Special, Add —
    /// each present only when the body shows it.
    fn nav_remap_action_items(&self, outer_id: egui_snarl::NodeId, inner: egui_snarl::NodeId)
        -> Vec<&'static str>
    {
        // Gyro Lean sections use a SEPARATE state model (`_lean_<side>_phase`
        // "idle"/"learning" + `_lean_<side>_draft`), and their body consumes
        // side-scoped `_nav_act_learn_<side>` / `_nav_act_add_<side>` flags.
        if let Some(side) = self.nav_lean_side(outer_id) {
            let phase = self.nav_lean_phase(outer_id, inner, side);
            let addable = matches!(phase.as_str(), "learning" | "ready");
            let draft = self.nav_lean_draft_len(outer_id, inner, side) > 0;
            // has_draft mirrors the body: any captured/picked output OR mid-learn.
            let has_draft = draft || matches!(phase.as_str(), "learning" | "ready");
            // Visual order (must match the body + published rects):
            // Learn (always), Special (always), Clear(has_draft),
            // Add ((learning||ready) && draft non-empty).
            let mut v = vec![if side == "left" { "_nav_act_learn_left" } else { "_nav_act_learn_right" }];
            v.push(if side == "left" { "_nav_act_special_left" } else { "_nav_act_special_right" });
            if has_draft {
                v.push(if side == "left" { "_nav_act_clear_left" } else { "_nav_act_clear_right" });
            }
            if addable && draft {
                v.push(if side == "left" { "_nav_act_add_left" } else { "_nav_act_add_right" });
            }
            return v;
        }

        let mid = self.nav_selected_module_id(outer_id);
        let phase = self.nav_remap_phase(outer_id, inner);
        let in_draft = self.nav_remap_draft_len(outer_id, inner, "draft_input") > 0;
        let out_draft = self.nav_remap_draft_len(outer_id, inner, "draft_output") > 0;
        let latched = matches!(phase.as_str(), "ready_to_learn" | "learning");
        let has_draft = in_draft || out_draft || latched;
        match mid.as_deref() {
            Some("module.remapper") => {
                // Visual order (must match the body + published rects):
                // Learn, Special(latched), Clear(has_draft), Add(out_draft&&latched).
                let mut v = vec!["_nav_act_learn"];
                if latched { v.push("_nav_act_special"); }
                if has_draft { v.push("_nav_act_clear"); }
                if out_draft && latched { v.push("_nav_act_add"); }
                v
            }
            // Map Action / Combiner: Learn, Clear(has_draft), Add(in_draft).
            _ => {
                let mut v = vec!["_nav_act_learn"];
                if has_draft { v.push("_nav_act_clear"); }
                if in_draft { v.push("_nav_act_add"); }
                v
            }
        }
    }

    /// If the selected element is a gyro Lean section, return its side
    /// (`"left"`/`"right"`).
    fn nav_lean_side(&self, outer_id: egui_snarl::NodeId) -> Option<&'static str> {
        match self.nav_selected_element(outer_id).as_ref().map(|(_, e)| e.as_str()) {
            Some("lean_left") => Some("left"),
            Some("lean_right") => Some("right"),
            _ => None,
        }
    }

    /// Read a Lean section's phase param (`_lean_<side>_phase`, default "idle").
    fn nav_lean_phase(&self, outer: egui_snarl::NodeId, inner: egui_snarl::NodeId, side: &str) -> String {
        let key = if side == "left" { "_lean_left_phase" } else { "_lean_right_phase" };
        self.get_subpatch_param_str(outer, inner, key).unwrap_or_else(|| "idle".to_string())
    }

    /// Length of a Lean section's capture draft (`_lean_<side>_draft`).
    fn nav_lean_draft_len(&self, outer: egui_snarl::NodeId, inner: egui_snarl::NodeId, side: &str) -> usize {
        let key = if side == "left" { "_lean_left_draft" } else { "_lean_right_draft" };
        let canvas = &self.tabs[self.active_tab].canvas;
        canvas.snarl.get_node(outer).and_then(|n| n.subpatch.as_ref())
            .and_then(|sp| sp.snarl.get_node(inner))
            .and_then(|node| node.params.get(key).and_then(|v| v.as_array()))
            .map(|a| a.len()).unwrap_or(0)
    }

    /// Publish the RemapScroll selection (action vs card) so the body glows the
    /// focused item. Action items glow their button rect (published by the body);
    /// cards glow via the existing card-rect publish.
    fn nav_publish_remap_selection(&self, ctx: &egui::Context,
        outer_id: egui_snarl::NodeId, inner: egui_snarl::NodeId)
    {
        let n_actions = self.nav_remap_action_items(outer_id, inner).len();
        let sel = self.gamepad_nav.card_index;
        let pass = ctx.cumulative_pass_nr();
        let scope = self.nav_remap_mappings_key(outer_id);
        let entered = matches!(self.gamepad_nav.edit_level,
            crate::gamepad_nav::EditLevel::RemapCard);
        // (selected_action_index or usize::MAX, card_index or usize::MAX, entered)
        let (act_sel, card_sel) = if entered {
            (usize::MAX, self.gamepad_nav.remap_card)
        } else if sel < n_actions {
            (sel, usize::MAX)
        } else {
            (usize::MAX, sel - n_actions)
        };
        ctx.data_mut(|d| {
            d.insert_temp(egui::Id::new(("gp_nav_remap_card", inner.0, scope)),
                (pass, card_sel, entered));
            d.insert_temp(egui::Id::new(("gp_nav_remap_action", inner.0, scope)),
                (pass, act_sel));
        });
        ctx.request_repaint();
    }

    /// Read the remapper-family node's `ui_phase` (default "capturing").
    fn nav_remap_phase(&self, outer_id: egui_snarl::NodeId, inner: egui_snarl::NodeId) -> String {
        self.get_subpatch_param_str(outer_id, inner, "ui_phase")
            .unwrap_or_else(|| "capturing".to_string())
    }
    /// Length of the captured input/output drafts.
    fn nav_remap_draft_len(&self, outer_id: egui_snarl::NodeId, inner: egui_snarl::NodeId, key: &str) -> usize {
        let canvas = &self.tabs[self.active_tab].canvas;
        canvas.snarl.get_node(outer_id).and_then(|n| n.subpatch.as_ref())
            .and_then(|sp| sp.snarl.get_node(inner))
            .and_then(|node| node.params.get(key).and_then(|v| v.as_array()))
            .map(|a| a.len()).unwrap_or(0)
    }

    /// Drive header-field editing of the entered mapping card (`RemapCard`):
    /// left/right move between the fields that APPLY for the current mode
    /// (press-mode / time-gap / hold / turbo — grayed-out ones skipped), up/down
    /// or South edit the focused field, North resets the card, East exits.
    fn nav_drive_remap_card(
        &mut self,
        ctx: &egui::Context,
        outer_id: egui_snarl::NodeId,
        nav: &crate::gamepad_nav::NavInput,
        dt: f32,
        step_dir: Option<crate::gamepad_nav::NavDir>,
        rt_rising: bool,
        mag: f32,
    ) {
        use crate::gamepad_nav::{EditLevel, NavDir};
        if nav.is_rising("btn_east") {
            self.gamepad_nav.edit_level = EditLevel::RemapScroll;
            if let Some(baseline) = self.gamepad_nav.edit_baseline.take() {
                self.tabs[self.active_tab].canvas.commit_undo_if_changed(*baseline);
            }
            return;
        }
        let Some(inner) = self.nav_selected_inner_node(outer_id) else {
            self.gamepad_nav.edit_level = EditLevel::RemapScroll;
            return;
        };
        let _ = inner;
        // Clamp the entered-card index to the live card range so a stale index
        // (count shrank, e.g. a card was deleted) edits a valid card instead of
        // silently bailing to RemapScroll (the "modes/toggles don't work" bug).
        let count = self.nav_remap_card_count(outer_id);
        if count == 0 {
            self.gamepad_nav.edit_level = EditLevel::RemapScroll;
            return;
        }
        if self.gamepad_nav.remap_card >= count {
            self.gamepad_nav.remap_card = count - 1;
        }
        let idx = self.gamepad_nav.remap_card;
        // North resets the entered card's press mode + params to default.
        if nav.is_rising("btn_north") {
            self.nav_remap_reset_card(outer_id, idx);
        }
        // Header fields, in display order: 0=press-mode, 1=time-gap, 2=hold,
        // 3=turbo. Compute which apply for the current mode (mirror the card
        // renderer's gray-out rules) so we can skip the inert ones.
        let Some(mode) = self.nav_remap_card_mode(outer_id, idx) else {
            self.gamepad_nav.edit_level = EditLevel::RemapScroll;
            return;
        };
        let turbo_on = self.nav_remap_card_bool(outer_id, idx, "turbo");
        let gap_applies = matches!(mode.as_str(),
            "short"|"long"|"double"|"analog"|"on_press"|"on_release") || turbo_on;
        let hold_applies = mode == "long" || mode == "analog";
        let turbo_applies = !matches!(mode.as_str(), "short"|"double"|"on_press"|"on_release");
        // Field 0 (press-mode) always applies.
        let applies = [true, gap_applies, hold_applies, turbo_applies];

        // Left/right move to the previous/next applicable field.
        let mut field = self.gamepad_nav.card_field.min(3);
        let move_dir = match step_dir {
            Some(NavDir::Left) => -1i32,
            Some(NavDir::Right) => 1,
            _ => 0,
        };
        if move_dir != 0 {
            let mut f = field as i32;
            for _ in 0..4 {
                f = (f + move_dir).rem_euclid(4);
                if applies[f as usize] { break; }
            }
            field = f as usize;
            self.gamepad_nav.card_field = field;
        }
        // If the focused field became inert (mode changed), snap to press-mode.
        if !applies[field] { field = 0; self.gamepad_nav.card_field = 0; }

        // Edit the focused field with up/down (and South for toggles / mode).
        let edit_press = match step_dir {
            Some(NavDir::Up) => 1i32,
            Some(NavDir::Down) => -1,
            _ => 0,
        };
        let south = nav.is_rising("btn_south") || rt_rising;
        match field {
            0 => {
                // Press mode: South OPENS the press-mode picker (a modal list of
                // the available modes with glyphs + labels) so the user can see
                // what each option does. Up/down still nudge the mode inline as a
                // quick alternative.
                if south {
                    let cur = self.nav_remap_card_mode(outer_id, idx)
                        .unwrap_or_else(|| "down".to_string());
                    self.gamepad_nav.press_mode_open = true;
                    self.gamepad_nav.press_mode_card = idx;
                    self.gamepad_nav.press_mode_outer = Some(outer_id);
                    self.gamepad_nav.press_mode_idx =
                        Self::PRESS_MODES.iter().position(|m| *m == cur).unwrap_or(0);
                } else if edit_press != 0 {
                    self.nav_remap_cycle_mode(outer_id, idx, edit_press);
                }
            }
            1 => {
                // Time gap (window_ms): decade-ish nudge, 10..5000.
                let mut delta = 0.0f32;
                let fine = self.gamepad_nav.fine_increment;
                if nav.is_rising("btn_west") {
                    self.gamepad_nav.fine_increment = !self.gamepad_nav.fine_increment;
                }
                let step = if fine { 1.0 } else { 5.0 };
                // Discrete dpad step OR continuous stick — not both, so the two
                // don't double-count / fight. Up = increase for both. In this
                // codebase up-stick is +y (see gamepad_nav::stick_dir), so use
                // +lstick.y directly (the prior `-y` inverted the stick relative
                // to the dpad).
                if mag > 0.5 {
                    let accel = self.settings.cursor_accel.max(1.0);
                    let c = nav.lstick.y;
                    delta += c.signum() * c.abs().powf(accel) * step * 40.0 * dt;
                } else if edit_press != 0 {
                    delta += edit_press as f32 * step;
                }
                if delta != 0.0 {
                    self.nav_remap_nudge_window(outer_id, idx, delta);
                }
            }
            2 => {
                if south || edit_press != 0 {
                    let cur = self.nav_remap_card_bool(outer_id, idx, "sustain");
                    let next = if south { !cur } else { edit_press > 0 };
                    self.nav_remap_set_card_bool(outer_id, idx, "sustain", next);
                }
            }
            3 => {
                if south || edit_press != 0 {
                    let cur = self.nav_remap_card_bool(outer_id, idx, "turbo");
                    let next = if south { !cur } else { edit_press > 0 };
                    self.nav_remap_set_card_bool(outer_id, idx, "turbo", next);
                }
            }
            _ => {}
        }

        // Publish selected card + focused field so the body glows them.
        let pass = ctx.cumulative_pass_nr();
        let scope = self.nav_remap_mappings_key(outer_id);
        ctx.data_mut(|d| {
            d.insert_temp(egui::Id::new(("gp_nav_remap_card", inner.0, scope)), (pass, idx, true));
            d.insert_temp(egui::Id::new(("gp_nav_remap_card_field", inner.0, scope)), (pass, field as u64));
        });
        ctx.request_repaint();
    }

    /// Unified multi-field editor. Left/right move the focused field; up/down +
    /// stick-X edit the focused field (Value = nudge, Enum/EnumPair = cycle,
    /// Toggle = on South or up/down). West=fine, North=reset focused field.
    /// Publishes the focus index + a small HUD label so the user sees what's
    /// targeted (since the underlying body rows aren't gamepad-aware).
    fn nav_drive_fields(
        &mut self,
        ctx: &egui::Context,
        outer_id: egui_snarl::NodeId,
        nav: &crate::gamepad_nav::NavInput,
        dt: f32,
        step_dir: Option<crate::gamepad_nav::NavDir>,
        rt_rising: bool,
        mag: f32,
    ) {
        use crate::gamepad_nav::NavDir;
        let fields = self.nav_element_fields(outer_id);
        if fields.is_empty() { return; }
        let n = fields.len();
        self.gamepad_nav.field_index = self.gamepad_nav.field_index.min(n - 1);

        // West → fine. North → reset focused field.
        if nav.is_rising("btn_west") {
            self.gamepad_nav.fine_increment = !self.gamepad_nav.fine_increment;
        }
        let fine = self.gamepad_nav.fine_increment;

        // Left/right: move field focus (when multiple). Up/down: edit value.
        // For single-field elements, left/right also edit (no focus to move).
        let multi = n > 1;
        let mut edit_press = 0i32; // -1/+1 from dpad up/down or stick
        if let Some(dir) = step_dir {
            match dir {
                NavDir::Left if multi => {
                    self.gamepad_nav.field_index = self.gamepad_nav.field_index.saturating_sub(1);
                }
                NavDir::Right if multi => {
                    self.gamepad_nav.field_index = (self.gamepad_nav.field_index + 1).min(n - 1);
                }
                NavDir::Up   | NavDir::Right => edit_press = 1,
                NavDir::Down | NavDir::Left  => edit_press = -1,
            }
        }
        let idx = self.gamepad_nav.field_index;
        let def = fields[idx].clone();
        let Some(inner) = self.nav_selected_inner_node(outer_id) else { return; };

        // North → reset focused field.
        if nav.is_rising("btn_north") {
            match &def.field {
                NavField::Value { key, default, .. } =>
                    self.set_subpatch_param_f32(outer_id, inner, key, *default),
                NavField::Enum { key, opts } =>
                    self.set_subpatch_param_str(outer_id, inner, key, opts[0]),
                NavField::Toggle { key } =>
                    self.set_subpatch_param_bool(outer_id, inner, key, false),
                NavField::EnumPair { key_a, key_b, opts } => {
                    self.set_subpatch_param_str(outer_id, inner, key_a, opts[0].1);
                    self.set_subpatch_param_str(outer_id, inner, key_b, opts[0].2);
                }
            }
        }

        match &def.field {
            NavField::Value { key, lo, hi, default, step } => {
                let press = edit_press as f32;
                let cont = if mag > 0.5 { nav.lstick.x } else { 0.0 };
                if press != 0.0 || cont != 0.0 {
                    self.nav_adjust_field_value(outer_id, inner, *key, *lo, *hi, *default, *step,
                        press, cont, fine, dt);
                }
            }
            NavField::Enum { key, opts } => {
                // South or up/down cycles; up/right = +1, down/left = -1.
                let dir = if nav.is_rising("btn_south") || rt_rising { 1 } else { edit_press };
                if dir != 0 {
                    let cur = self.get_subpatch_param_str(outer_id, inner, key)
                        .unwrap_or_else(|| opts[0].to_string());
                    let cu = opts.iter().position(|o| *o == cur).unwrap_or(0) as i32;
                    let next = opts[(cu + dir).rem_euclid(opts.len() as i32) as usize];
                    self.set_subpatch_param_str(outer_id, inner, key, next);
                }
            }
            NavField::Toggle { key } => {
                if nav.is_rising("btn_south") || rt_rising || edit_press != 0 {
                    let cur = self.get_subpatch_param_bool(outer_id, inner, key).unwrap_or(false);
                    // up/right → on, down/left → off; South toggles.
                    let next = if nav.is_rising("btn_south") || rt_rising { !cur }
                               else { edit_press > 0 };
                    self.set_subpatch_param_bool(outer_id, inner, key, next);
                }
            }
            NavField::EnumPair { key_a, key_b, opts } => {
                let dir = if nav.is_rising("btn_south") || rt_rising { 1 } else { edit_press };
                if dir != 0 {
                    let a = self.get_subpatch_param_str(outer_id, inner, key_a).unwrap_or_default();
                    let b = self.get_subpatch_param_str(outer_id, inner, key_b).unwrap_or_default();
                    let cu = opts.iter().position(|o| o.1 == a && o.2 == b).unwrap_or(0) as i32;
                    let nx = &opts[(cu + dir).rem_euclid(opts.len() as i32) as usize];
                    self.set_subpatch_param_str(outer_id, inner, key_a, nx.1);
                    self.set_subpatch_param_str(outer_id, inner, key_b, nx.2);
                }
            }
        }

        // Publish a focus HUD near the selected item so the user sees which
        // field is targeted + its label/value.
        self.nav_publish_field_hud(ctx, outer_id, inner, &fields, idx, fine);
    }

    /// Value-field nudge: decade or linear stepping (shared with the standalone
    /// numeric widgets), per-field bounds.
    #[allow(clippy::too_many_arguments)]
    fn nav_adjust_field_value(&mut self, outer_id: egui_snarl::NodeId, inner: egui_snarl::NodeId,
        key: &str, lo: f32, hi: f32, default: f32, step: NavStep,
        press: f32, cont: f32, fine: bool, dt: f32)
    {
        let cur = self.get_subpatch_param_f32(outer_id, inner, key).unwrap_or(default);
        let press_step = match step {
            NavStep::Decade => {
                let v = (cur - lo).abs().max(1e-6);
                let decade = 10f32.powf(v.log10().floor()).max(1.0);
                let coarse = if decade <= 1.0 { 1.0 } else { decade * 0.5 };
                if fine { coarse * 0.1 } else { coarse }
            }
            NavStep::Linear => {
                let span = (hi - lo).abs().max(f32::EPSILON);
                span * if fine { 0.005 } else { 0.02 }
            }
        };
        let accel = self.settings.cursor_accel.max(1.0);
        let cont_curved = cont.signum() * cont.abs().clamp(0.0, 1.0).powf(accel);
        let cont_per_s = press_step * if fine { 8.0 } else { 25.0 };
        let mut delta = 0.0f32;
        if press != 0.0 { delta += press * press_step; }
        if cont != 0.0  { delta += cont_curved * cont_per_s * dt; }
        if delta == 0.0 { return; }
        // Whole-unit params read better rounded — but a small linear step (e.g.
        // grid 1..20 → step ~0.38) rounds back to the current value and looks
        // dead. For those, a discrete dpad/stick press must move at least one
        // whole unit in the press direction.
        let whole = matches!(key, "buf_size" | "grid_x" | "grid_y" | "trail_ms");
        let mut next = (cur + delta).clamp(lo, hi);
        if whole {
            next = next.round();
            if press != 0.0 && (next - cur.round()).abs() < 0.5 {
                next = (cur.round() + press.signum()).clamp(lo, hi);
            }
            // These renderers read the param with `as_i64()` and ignore a JSON
            // float, so whole-unit params MUST be stored as integers.
            self.set_subpatch_param_i64(outer_id, inner, key, next as i64);
            return;
        }
        self.set_subpatch_param_f32(outer_id, inner, key, next);
    }

    /// Publish the multi-field focus HUD (pass-stamped) for the renderer overlay
    /// + a foreground text label so the user sees the focused field.
    fn nav_publish_field_hud(&self, ctx: &egui::Context, outer_id: egui_snarl::NodeId,
        inner: egui_snarl::NodeId, fields: &[NavFieldDef], idx: usize, fine: bool)
    {
        let def = &fields[idx];
        // Read the focused field's current value as a string for the HUD.
        let val_str = match &def.field {
            NavField::Value { key, .. } =>
                self.get_subpatch_param_f32(outer_id, inner, key)
                    .map(|v| format!("{:.3}", v)).unwrap_or_default(),
            NavField::Enum { key, .. } | NavField::EnumPair { key_a: key, .. } =>
                self.get_subpatch_param_str(outer_id, inner, key).unwrap_or_default(),
            NavField::Toggle { key } =>
                if self.get_subpatch_param_bool(outer_id, inner, key).unwrap_or(false) { "ON".into() } else { "OFF".into() },
        };
        // Find the item's screen rect (published by render_subpatch_body) to
        // anchor the HUD just above it.
        let rects: Option<(u64, Vec<(usize, egui::Rect)>)> =
            ctx.data(|d| d.get_temp(egui::Id::new(("gp_nav_item_rects", outer_id.0))));
        let sel = {
            let canvas = &self.tabs[self.active_tab].canvas;
            canvas.snarl.get_node(outer_id).and_then(|n| n.subpatch.as_ref())
                .and_then(|sp| sp.selected_item)
        };
        let rect = rects.and_then(|(_, rs)| sel.and_then(|s| rs.iter().find(|(i,_)| *i == s).map(|(_,r)| *r)));
        let Some(rect) = rect else { return; };
        let accent = ctx.style().visuals.selection.stroke.color;

        // Per-field inner glow: if the row renderer published per-control rects
        // this frame, draw an outward bloom ring on the focused field's rect so
        // the user sees WHICH sub-control is targeted (the HUD pill names it; the
        // ring points at it). Falls back silently to pill-only when a renderer
        // hasn't been instrumented.
        let element = self.nav_selected_element(outer_id)
            .map(|(_, e)| e).unwrap_or_default();
        let field_rects: Option<(u64, Vec<egui::Rect>)> =
            ctx.data(|d| d.get_temp(egui::Id::new(("gp_nav_field_rects", inner.0, element))));
        if let Some((_, frs)) = field_rects {
            if let Some(fr) = frs.get(idx) {
                if fr.is_finite() && fr.width() > 0.5 {
                    let [r, g, b, _] = accent.to_array();
                    let p = ctx.layer_painter(egui::LayerId::new(
                        egui::Order::Foreground, egui::Id::new(("gp_nav_field_glow", outer_id.0))));
                    let rings = 6;
                    for i in 0..rings {
                        let t = (i as f32 + 1.0) / rings as f32;
                        let grow = t * 7.0;
                        let a = (150.0 * (1.0 - t)).round() as u8;
                        if a == 0 { continue; }
                        p.rect_stroke(fr.expand(grow), 5.0 + grow,
                            egui::Stroke::new(2.0, egui::Color32::from_rgba_unmultiplied(r, g, b, a)),
                            egui::StrokeKind::Outside);
                    }
                    p.rect_stroke(fr.expand(1.5), 5.0, egui::Stroke::new(2.0, accent),
                        egui::StrokeKind::Outside);
                }
            }
        }
        let painter = ctx.layer_painter(egui::LayerId::new(
            egui::Order::Foreground, egui::Id::new(("gp_nav_field_hud", outer_id.0))));
        let n = fields.len();
        let label = if n > 1 {
            format!("[{}/{}] {} = {}{}", idx + 1, n, def.label, val_str,
                if fine { "  (fine)" } else { "" })
        } else {
            format!("{} = {}{}", def.label, val_str, if fine { "  (fine)" } else { "" })
        };
        let pos = egui::pos2(rect.center().x, rect.top() - 6.0);
        // Background pill for legibility.
        let galley = painter.layout_no_wrap(label.clone(),
            egui::FontId::proportional(12.0), egui::Color32::WHITE);
        let pad = egui::vec2(6.0, 3.0);
        let bg = egui::Rect::from_center_size(
            egui::pos2(pos.x, pos.y - galley.size().y * 0.5),
            galley.size() + pad * 2.0);
        painter.rect_filled(bg, 4.0, egui::Color32::from_rgba_unmultiplied(20, 20, 24, 230));
        painter.rect_stroke(bg, 4.0, egui::Stroke::new(1.0, accent), egui::StrokeKind::Outside);
        painter.text(egui::pos2(pos.x, pos.y - galley.size().y * 0.5),
            egui::Align2::CENTER_CENTER, label, egui::FontId::proportional(12.0), egui::Color32::WHITE);
    }

    /// Read/write a String param on an inner sub-patch node.
    fn get_subpatch_param_str(&self, outer_id: egui_snarl::NodeId,
        inner: egui_snarl::NodeId, key: &str) -> Option<String>
    {
        let canvas = &self.tabs[self.active_tab].canvas;
        let sp = canvas.snarl.get_node(outer_id)?.subpatch.as_ref()?;
        sp.snarl.get_node(inner)?.params.get(key)?.as_str().map(|s| s.to_string())
    }
    fn set_subpatch_param_str(&mut self, outer_id: egui_snarl::NodeId,
        inner: egui_snarl::NodeId, key: &str, val: &str)
    {
        let canvas = &mut self.tabs[self.active_tab].canvas;
        let Some(sp) = canvas.snarl.get_node_mut(outer_id).and_then(|n| n.subpatch.as_mut()) else { return; };
        if let Some(node) = sp.snarl.get_node_mut(inner) {
            node.params.insert(key.to_string(), serde_json::Value::String(val.to_string()));
        }
    }
    fn set_subpatch_param_bool(&mut self, outer_id: egui_snarl::NodeId,
        inner: egui_snarl::NodeId, key: &str, val: bool)
    {
        let canvas = &mut self.tabs[self.active_tab].canvas;
        let Some(sp) = canvas.snarl.get_node_mut(outer_id).and_then(|n| n.subpatch.as_mut()) else { return; };
        if let Some(node) = sp.snarl.get_node_mut(inner) {
            node.params.insert(key.to_string(), serde_json::Value::Bool(val));
        }
    }
    fn get_subpatch_param_bool(&self, outer_id: egui_snarl::NodeId,
        inner: egui_snarl::NodeId, key: &str) -> Option<bool>
    {
        let canvas = &self.tabs[self.active_tab].canvas;
        let sp = canvas.snarl.get_node(outer_id)?.subpatch.as_ref()?;
        sp.snarl.get_node(inner)?.params.get(key)?.as_bool()
    }

    /// Hit-test the RS/gyro cursor against the sub-patch item screen rects
    /// published by `render_subpatch_body` last frame. Returns the index of the
    /// topmost item under the cursor (last in paint order wins), or None.
    fn nav_cursor_hit_item(&self, ctx: &egui::Context, outer_id: egui_snarl::NodeId) -> Option<usize> {
        let cursor = self.gamepad_nav.cursor_pos;
        let rects: Option<(u64, Vec<(usize, egui::Rect)>)> =
            ctx.data(|d| d.get_temp(egui::Id::new(("gp_nav_item_rects", outer_id.0))));
        let (_pass, rects) = rects?;
        // Only consider items that are actually interactable — skip text titles,
        // graphs, svgs and other decorative elements the cursor passes over.
        let canvas = &self.tabs[self.active_tab].canvas;
        let sp = canvas.snarl.get_node(outer_id)?.subpatch.as_ref()?;
        rects
            .iter()
            .rev()
            .find(|(i, r)| r.contains(cursor) && Self::sp_item_is_editable(sp, *i))
            .map(|(i, _)| *i)
    }

    /// True if sub-patch item `idx` is a gamepad-editable widget (knob/constant/
    /// dropdown/switch/curve/remapper), not a decorative element. Only the
    /// `"curve"` element of a response curve is selectable (its scale/range/grid
    /// rows are separate, non-dot elements).
    fn sp_item_is_editable(
        sp: &crate::canvas::node::UiSubPatch,
        idx: usize,
    ) -> bool {
        use crate::canvas::node::LayoutItem;
        let Some(LayoutItem::Module(m)) = sp.items.get(idx) else { return false; };
        let inner = egui_snarl::NodeId(m.inner_node_id);
        let Some(mid) = sp.snarl.get_node(inner).map(|n| n.module_id.clone()) else { return false; };
        Self::elem_is_nav_target(&mid, &m.element_id)
    }

    /// Pure (module, element) test for "the cursor can target this and the nav
    /// driver has a handler for it". Single source of truth shared by the cursor
    /// hit-test and any other place that needs targetability without selection
    /// context. Mirrors the arms of `nav_selected_kind`.
    fn elem_is_nav_target(mid: &str, elem: &str) -> bool {
        match (mid, elem) {
            // Single-actuation widgets.
            ("module.knob", _) | ("module.constant", _)
            | ("module.dropdown", _) | ("module.switch", _) => true,
            // Curves: the dot graph plus every editable option row.
            ("module.response_curve", "curve")
            | ("module.vec_response_curve", "curve")
            | ("module.twoway_response_curve", "curve") => true,
            // Remapper-family mapping widgets (filter cycle + in-body capture).
            ("module.remapper", _) | ("module.map_action", _)
            | ("module.automap_combiner", _) => true,
            ("processing.gyro_3dof", "lean_left")
            | ("processing.gyro_3dof", "lean_right") => true,
            // Everything else is a field row — targetable iff it has fields.
            _ => Self::elem_has_fields(mid, elem),
        }
    }

    /// Static mirror of `nav_element_fields`' coverage (pure (module, element)
    /// test) — used by the cursor hit-test, which has no selection context.
    /// MUST stay in sync with `nav_element_fields`.
    fn elem_has_fields(mid: &str, elem: &str) -> bool {
        matches!(
            (mid, elem),
            ("module.delay", "ms")
            | ("module.average", "samples") | ("module.average", "spike_mad")
            | ("module.dc_filter", "window_ms") | ("module.dc_filter", "decay_ms")
            | ("logic.delay", "time") | ("logic.delay", "mode")
            | ("generator.oscillator", "freq") | ("generator.oscillator", "phase")
            | ("generator.oscillator", "shape")
            | ("processing.gyro_3dof", "lean_threshold")
            | ("processing.gyro_3dof", "pointer_mode") | ("processing.gyro_3dof", "mode")
            | ("processing.gyro_3dof", "steering_mode")
            | ("processing.gyro_3dof", "steering_opts")
            | ("processing.gyro_3dof", "gyro_invert") | ("processing.gyro_3dof", "accel_invert")
            | ("logic.counter", "mode") | ("logic.counter", "range_mode")
            | ("logic.counter", "step") | ("logic.counter", "min_max")
            | ("module.selector", "mode") | ("module.selector", "range_mode")
            | ("module.selector", "step") | ("module.selector", "min_max")
            | ("module.response_curve", "scale_row") | ("module.response_curve", "range_row")
            | ("module.response_curve", "grid_row") | ("module.response_curve", "grid_options_row")
            | ("module.vec_response_curve", "scale_row") | ("module.vec_response_curve", "range_row")
            | ("module.vec_response_curve", "grid_row") | ("module.vec_response_curve", "grid_options_row")
            | ("module.twoway_response_curve", "scale_row") | ("module.twoway_response_curve", "range_row")
            | ("module.twoway_response_curve", "grid_row") | ("module.twoway_response_curve", "grid_options_row")
            | ("module.twoway_response_curve", "hyst_row") | ("module.twoway_response_curve", "interp_row")
            | ("module.twoway_response_curve", "lane_toggle")
            | ("display.oscilloscope", "controls")
        )
    }

    /// Cursor-driven navigation of the left I/O panel. Returns true when it
    /// consumed this frame's input (so sub-patch nav is skipped).
    ///
    /// - Not editing: if the cursor is visible and South/RT rises over a target,
    ///   dispatch it — select input device, toggle output sink, toggle the
    ///   digital-triggers checkbox, or (for sliders) ENTER a left-edit.
    /// - Editing a slider: stick/dpad nudges the value, West toggles fine,
    ///   North resets to default, East/LT exits (committing one undo entry).
    fn nav_drive_left_panel(
        &mut self,
        ctx: &egui::Context,
        nav: &crate::gamepad_nav::NavInput,
        dt: f32,
        rt_rising: bool,
        lt_rising: bool,
    ) -> bool {
        use crate::gamepad_nav::{self as gn, LeftNavAction, NavDir};

        // Published targets (this frame, from io_panel). Used for hover-glow and
        // to recover the editing target's rect.
        let targets: Vec<gn::LeftNavTarget> = ctx
            .data(|d| d.get_temp::<(u64, Vec<gn::LeftNavTarget>)>(gn::left_targets_id()))
            .map(|(_, t)| t)
            .unwrap_or_default();

        // Glow helper: edge ring + OUTWARD bloom only — never fills the widget
        // interior, so the slider/checkbox underneath stays fully visible while
        // editing. The bloom is concentric outside-strokes with falling alpha (a
        // true outward gradient). `editing` brightens + widens it.
        let glow = |rect: egui::Rect, editing: bool| {
            let accent = ctx.style().visuals.selection.stroke.color;
            let [r, g, b, _] = accent.to_array();
            let round = 10.0_f32;
            let painter = ctx.layer_painter(egui::LayerId::new(
                egui::Order::Foreground, egui::Id::new("gp_nav_left_glow")));
            // Outward bloom: a handful of expanding rings fading to transparent.
            let rings = 7;
            let max_grow = if editing { 9.0 } else { 6.0 };
            let peak = if editing { 150.0 } else { 90.0 };
            for i in 0..rings {
                let t = (i as f32 + 1.0) / rings as f32; // 0..1 outward
                let grow = t * max_grow;
                let a = (peak * (1.0 - t)).round() as u8;
                if a == 0 { continue; }
                painter.rect_stroke(
                    rect.expand(grow), round + grow,
                    egui::Stroke::new(2.0, egui::Color32::from_rgba_unmultiplied(r, g, b, a)),
                    egui::StrokeKind::Outside,
                );
            }
            // Crisp edge ring on the widget border.
            painter.rect_stroke(rect.expand(1.0), round,
                egui::Stroke::new(if editing { 2.0 } else { 1.25 }, accent),
                egui::StrokeKind::Outside);
        };

        // ── Editing a left-panel slider ──────────────────────────────────
        if let Some(action) = self.gamepad_nav.left_edit.clone() {
            let LeftNavAction::AdjustParam { node, key, lo, hi, step, default, log } = action
            else {
                // Non-adjust actions never enter edit; clear defensively.
                self.gamepad_nav.left_edit = None;
                return false;
            };

            // Exit (East / LT) → commit one undo entry if changed.
            if nav.is_rising("btn_east") || lt_rising {
                self.gamepad_nav.left_edit = None;
                if let Some(baseline) = self.gamepad_nav.edit_baseline.take() {
                    self.tabs[self.active_tab].canvas.commit_undo_if_changed(*baseline);
                }
                return true;
            }
            // West → fine increments. North → reset to default.
            if nav.is_rising("btn_west") {
                self.gamepad_nav.fine_increment = !self.gamepad_nav.fine_increment;
            }
            if nav.is_rising("btn_north") {
                self.set_node_param_f32(node, &key, default);
                return true;
            }

            // Directional delta: dpad step + left-stick continuous.
            let fine = self.gamepad_nav.fine_increment;
            let span = hi - lo;
            // Per-press step is a fraction of the range; fine = ¼ of that.
            let base_step = span * if fine { 0.01 } else { 0.04 };
            let mut delta = 0.0f32;
            let mut step_dir: Option<NavDir> = None;
            if nav.is_rising("dpad_right") || nav.is_rising("dpad_up") { step_dir = Some(NavDir::Right); }
            else if nav.is_rising("dpad_left") || nav.is_rising("dpad_down") { step_dir = Some(NavDir::Left); }
            if let Some(d) = step_dir {
                delta += if matches!(d, NavDir::Right) { base_step } else { -base_step };
            }
            let mag = nav.lstick.length();
            if mag > 0.5 {
                let sens = span * if fine { 0.15 } else { 0.6 };
                delta += nav.lstick.x * sens * dt;
            }
            if delta != 0.0 {
                let cur = self.get_node_param_f32(node, &key).unwrap_or(default);
                let next = if log {
                    // Multiplicative step in log space for log-scaled sliders.
                    let factor = 1.0 + (delta / span);
                    (cur * factor).clamp(lo, hi)
                } else {
                    (cur + delta).clamp(lo, hi)
                };
                self.set_node_param_f32(node, &key, next);
            }
            // Glow the editing target (look its rect up by node+key).
            if let Some(t) = targets.iter().find(|t| matches!(&t.action,
                LeftNavAction::AdjustParam { node: n, key: k, .. } if *n == node && *k == key))
            {
                glow(t.rect, true);
            }
            let _ = (rt_rising, step);
            return true;
        }

        // ── Not editing: hover-glow + act on the target under the cursor ──
        if !self.gamepad_nav.cursor_visible {
            return false;
        }
        let cursor = self.gamepad_nav.cursor_pos;
        // Hover highlight whatever target the cursor is over (even without a
        // press) so the user sees what South/RT will act on.
        if let Some(hov) = targets.iter().rev().find(|t| t.rect.contains(cursor)) {
            glow(hov.rect, false);
        }
        if !(nav.is_rising("btn_south") || rt_rising) {
            return false;
        }
        let Some(hit) = targets.iter().rev().find(|t| t.rect.contains(cursor)) else {
            return false;
        };
        match hit.action.clone() {
            LeftNavAction::SelectInput { device_id } => {
                self.nav_select_input_device(&device_id);
            }
            LeftNavAction::ToggleOutput { kind } => {
                self.nav_toggle_output_sink(&kind);
            }
            LeftNavAction::ToggleParam { node, key } => {
                let base = self.tabs[self.active_tab].canvas.snapshot_for_undo();
                let cur = self.get_node_param_bool(node, &key).unwrap_or(false);
                self.set_node_param_bool(node, &key, !cur);
                self.tabs[self.active_tab].canvas.commit_undo_if_changed(base);
            }
            action @ LeftNavAction::AdjustParam { .. } => {
                // Enter slider edit; snapshot for a single coalesced undo entry.
                self.gamepad_nav.edit_baseline = Some(Box::new(
                    self.tabs[self.active_tab].canvas.snapshot_for_undo()));
                self.gamepad_nav.fine_increment = false;
                self.gamepad_nav.left_edit = Some(action);
            }
        }
        true
    }

    /// Make `device_id` the active input source (mirrors the io_panel card
    /// click path: remove existing source nodes, add this device, rewire).
    fn nav_select_input_device(&mut self, device_id: &str) {
        let already = {
            let canvas = &self.tabs[self.active_tab].canvas;
            canvas.snarl.nodes_ids_data()
                .find(|(_, n)| n.value.module_id == "device.source")
                .and_then(|(_, n)| n.value.params.get("device_id")
                    .and_then(|v| v.as_str())) == Some(device_id)
        };
        if already { return; }
        let Some(dev) = self.devices.iter().find(|d| d.id == device_id).cloned()
        else { return; };
        let defaults = self.nav_device_defaults();
        let collapsed = self.settings.device_nodes_default_collapsed;
        let canvas = &mut self.tabs[self.active_tab].canvas;
        let to_remove: Vec<egui_snarl::NodeId> = canvas.snarl.nodes_ids_data()
            .filter(|(_, n)| n.value.module_id == "device.source")
            .map(|(id, _)| id)
            .collect();
        for id in to_remove { canvas.snarl.remove_node(id); }
        canvas.add_device_source(&dev, collapsed, defaults);
        crate::easy::layout::reposition_io_nodes(canvas);
        crate::easy::wiring::rewire(canvas);
    }

    /// Device param defaults from settings (mirrors the Easy panel's inline
    /// construction).
    fn nav_device_defaults(&self) -> crate::canvas::DeviceParamDefaults {
        crate::canvas::DeviceParamDefaults {
            stick_deadzone: self.settings.default_stick_deadzone,
            gyro_mult: self.settings.default_gyro_mult,
            mouse_sensitivity: self.settings.default_mouse_sensitivity,
        }
    }

    /// Toggle a virtual output sink on/off by kind prefix (xinput/ds4/keymouse),
    /// honoring the xinput⇄ds4 mutual exclusion the io_panel enforces.
    fn nav_toggle_output_sink(&mut self, kind: &str) {
        use flexinput_virtual::kind_prefix;
        let canvas = &mut self.tabs[self.active_tab].canvas;
        let has = canvas.snarl.nodes_ids_data().any(|(_, n)| {
            n.value.module_id == "device.sink"
                && n.value.params.get("device_id").and_then(|v| v.as_str())
                    .map(|d| kind_prefix(d) == kind).unwrap_or(false)
        });
        let remove_kind = |canvas: &mut Canvas, k: &str| {
            let ids: Vec<egui_snarl::NodeId> = canvas.snarl.nodes_ids_data()
                .filter(|(_, n)| n.value.module_id == "device.sink"
                    && n.value.params.get("device_id").and_then(|v| v.as_str())
                        .map(|d| kind_prefix(d) == k).unwrap_or(false))
                .map(|(id, _)| id).collect();
            for id in ids { canvas.snarl.remove_node(id); }
        };
        if has {
            remove_kind(canvas, kind);
        } else {
            // xinput and ds4 are mutually exclusive.
            if kind == "virtual.xinput" { remove_kind(canvas, "virtual.ds4"); }
            if kind == "virtual.ds4" { remove_kind(canvas, "virtual.xinput"); }
            let defaults = self.nav_device_defaults();
            let collapsed = self.settings.device_nodes_default_collapsed;
            let pool = std::sync::Arc::clone(&self.shared_virtual_devices);
            let canvas = &mut self.tabs[self.active_tab].canvas;
            crate::easy::io_panel::nav_ensure_sink(
                canvas, kind, &pool, collapsed, defaults);
        }
        let canvas = &mut self.tabs[self.active_tab].canvas;
        crate::easy::wiring::rewire(canvas);
    }

    /// Read an f32 param on an active-tab node (top-level snarl).
    fn get_node_param_f32(&self, node: egui_snarl::NodeId, key: &str) -> Option<f32> {
        let canvas = &self.tabs[self.active_tab].canvas;
        canvas.snarl.get_node(node)?.params.get(key)?.as_f64().map(|v| v as f32)
    }
    /// Write an f32 param on an active-tab node (top-level snarl).
    fn set_node_param_f32(&mut self, node: egui_snarl::NodeId, key: &str, val: f32) {
        let canvas = &mut self.tabs[self.active_tab].canvas;
        if let Some(n) = canvas.snarl.get_node_mut(node) {
            n.params.insert(key.to_string(), serde_json::Value::from(val as f64));
        }
    }
    fn get_node_param_bool(&self, node: egui_snarl::NodeId, key: &str) -> Option<bool> {
        let canvas = &self.tabs[self.active_tab].canvas;
        canvas.snarl.get_node(node)?.params.get(key)?.as_bool()
    }
    fn set_node_param_bool(&mut self, node: egui_snarl::NodeId, key: &str, val: bool) {
        let canvas = &mut self.tabs[self.active_tab].canvas;
        if let Some(n) = canvas.snarl.get_node_mut(node) {
            n.params.insert(key.to_string(), serde_json::Value::Bool(val));
        }
    }

    /// Resolve the inner module id of the selected sub-patch item, if it's a
    /// Module item.
    #[allow(dead_code)]
    fn nav_selected_module_id(&self, outer_id: egui_snarl::NodeId) -> Option<String> {
        let canvas = &self.tabs[self.active_tab].canvas;
        let sp = canvas.snarl.get_node(outer_id)?.subpatch.as_ref()?;
        let sel = sp.selected_item?;
        let item = sp.items.get(sel)?;
        if let crate::canvas::node::LayoutItem::Module(m) = item {
            let inner = egui_snarl::NodeId(m.inner_node_id);
            sp.snarl.get_node(inner).map(|n| n.module_id.clone())
        } else {
            None
        }
    }

    /// Resolve the inner node id of the selected sub-patch item, if it's a
    /// Module item.
    fn nav_selected_inner_node(&self, outer_id: egui_snarl::NodeId) -> Option<egui_snarl::NodeId> {
        let canvas = &self.tabs[self.active_tab].canvas;
        let sp = canvas.snarl.get_node(outer_id)?.subpatch.as_ref()?;
        let sel = sp.selected_item?;
        if let crate::canvas::node::LayoutItem::Module(m) = sp.items.get(sel)? {
            Some(egui_snarl::NodeId(m.inner_node_id))
        } else {
            None
        }
    }

    /// (module_id, element_id) of the selected sub-patch item.
    fn nav_selected_element(&self, outer_id: egui_snarl::NodeId) -> Option<(String, String)> {
        let canvas = &self.tabs[self.active_tab].canvas;
        let sp = canvas.snarl.get_node(outer_id)?.subpatch.as_ref()?;
        let sel = sp.selected_item?;
        if let crate::canvas::node::LayoutItem::Module(m) = sp.items.get(sel)? {
            let mid = sp.snarl.get_node(egui_snarl::NodeId(m.inner_node_id))?.module_id.clone();
            Some((mid, m.element_id.clone()))
        } else {
            None
        }
    }

    /// Numeric-edit descriptor for the selected element, if it edits a single
    /// scalar param: (param_key, lo, hi, step, default). `step` is the per-press
    /// nudge in param units; the continuous stick path scales off (hi-lo). This
    /// table is what lets a generic Value editor drive every numeric widget
    /// (delay, average, dc_filter, oscillator, logic thresholds, …) by its
    /// exposed `element_id`, not just knob/constant.
    fn nav_value_param(&self, outer_id: egui_snarl::NodeId) -> Option<NavParamSpec> {
        let (mid, elem) = self.nav_selected_element(outer_id)?;
        // Param keys + element_ids verified against each module body's pinned
        // dispatch (`render_dragvalue_param` / oscillator rows). Decade stepping
        // for time/sample counts (wide ranges); Linear for 0..1-ish params.
        use NavStep::*;
        let (key, lo, hi, default, step) = match (mid.as_str(), elem.as_str()) {
            ("module.delay", "ms")            => ("delay_ms",   0.0,  60_000.0, 100.0, Decade),
            ("module.average", "samples")     => ("buf_size",   1.0,  10_000.0, 10.0,  Decade),
            ("module.average", "spike_mad")   => ("spike_mad",  0.0,  20.0,     0.0,   Linear),
            ("module.dc_filter", "window_ms") => ("window_ms",  10.0, 60_000.0, 500.0, Decade),
            ("module.dc_filter", "decay_ms")  => ("decay_ms",   10.0, 60_000.0, 200.0, Decade),
            ("logic.delay", "time")           => ("time",       0.0,  60_000.0, 100.0, Decade),
            ("generator.oscillator", "freq")  => ("freq_param", 0.01, 200.0,    1.0,   Decade),
            ("generator.oscillator", "phase") => ("phase_param",0.0,  1.0,      0.0,   Linear),
            ("logic.counter", "step")         => ("step_param", 0.001,10_000.0, 1.0,   Decade),
            ("processing.gyro_3dof", "lean_threshold") => ("lean_threshold", 0.01, 4.0, 0.3, Linear),
            _ => return None,
        };
        Some(NavParamSpec { key, lo, hi, default, step })
    }

    /// Enum-cycle descriptor for a selected element backed by a String param
    /// with an ordered option set: (param_key, options). South/dpad cycles it.
    /// Superseded by the unified field editor; kept for reference.
    #[allow(dead_code)]
    fn nav_enum_spec(&self, outer_id: egui_snarl::NodeId)
        -> Option<(&'static str, &'static [&'static str])>
    {
        let (mid, elem) = self.nav_selected_element(outer_id)?;
        let d: (&'static str, &'static [&'static str]) = match (mid.as_str(), elem.as_str()) {
            ("generator.oscillator", "shape") =>
                ("shape", &["sine", "triangle", "saw", "square"]),
            ("logic.delay", "mode") =>
                ("mode", &["delay_true", "delay_false"]),
            ("logic.counter", "mode") =>
                ("mode", &["loop", "limit", "bounce", "unlimited"]),
            _ => return None,
        };
        Some(d)
    }

    /// Cycle the selected enum-string element by `dir` (+1/-1), wrapping.
    #[allow(dead_code)]
    fn nav_cycle_enum(&mut self, outer_id: egui_snarl::NodeId, dir: i32) {
        let Some((key, opts)) = self.nav_enum_spec(outer_id) else { return; };
        let Some(inner) = self.nav_selected_inner_node(outer_id) else { return; };
        let canvas = &mut self.tabs[self.active_tab].canvas;
        let Some(sp) = canvas.snarl.get_node_mut(outer_id).and_then(|n| n.subpatch.as_mut()) else { return; };
        let Some(node) = sp.snarl.get_node_mut(inner) else { return; };
        let cur = node.params.get(key).and_then(|v| v.as_str()).unwrap_or(opts[0]);
        let idx = opts.iter().position(|o| *o == cur).unwrap_or(0) as i32;
        let next = opts[(idx + dir).rem_euclid(opts.len() as i32) as usize];
        node.params.insert(key.to_string(), serde_json::Value::String(next.to_string()));
    }

    /// Master field table: every multi-control (and single-control) widget
    /// element maps to its ordered list of editable fields. The unified
    /// multi-field editor (`nav_drive_fields`) walks these. Returns empty for
    /// elements with no nav-editable fields. Verified against the pinned render
    /// functions' param keys.
    fn nav_element_fields(&self, outer_id: egui_snarl::NodeId) -> Vec<NavFieldDef> {
        use NavField::*;
        use NavStep::{Decade, Linear};
        let Some((mid, elem)) = self.nav_selected_element(outer_id) else { return vec![]; };
        // Gyro axis options (family, axis) for the pointer/steering mode rows.
        const GYRO_PTR: &[(&str, &str, &str)] = &[
            ("Pitch+Yaw", "pointer", "pitch_yaw"),
            ("Pitch+Roll", "pointer", "pitch_roll"),
            ("Player", "pointer", "player"),
            ("World", "pointer", "world"),
        ];
        const GYRO_STEER: &[(&str, &str, &str)] = &[
            ("Pitch+Yaw", "steering", "pitch_yaw"),
            ("Pitch+Roll", "steering", "pitch_roll"),
            ("Player", "steering", "player"),
            ("World", "steering", "world"),
        ];
        let v = |key, lo, hi, default, step| NavField::Value { key, lo, hi, default, step };
        macro_rules! f { ($l:expr, $field:expr) => { NavFieldDef { label: $l, field: $field } }; }
        match (mid.as_str(), elem.as_str()) {
            // ── single-field elements (also driven by the unified editor) ──
            ("module.delay", "ms")            => vec![f!("ms", v("delay_ms",0.0,60_000.0,100.0,Decade))],
            ("module.average", "samples")     => vec![f!("Samples", v("buf_size",1.0,10_000.0,10.0,Decade))],
            ("module.average", "spike_mad")   => vec![f!("Spike MAD", v("spike_mad",0.0,20.0,0.0,Linear))],
            ("module.dc_filter", "window_ms") => vec![f!("Window ms", v("window_ms",10.0,60_000.0,500.0,Decade))],
            ("module.dc_filter", "decay_ms")  => vec![f!("Decay ms", v("decay_ms",10.0,60_000.0,200.0,Decade))],
            ("logic.delay", "time")           => vec![f!("Time", v("time",0.0,60_000.0,100.0,Decade))],
            ("logic.delay", "mode")           => vec![f!("Mode", Enum{key:"mode",opts:&["delay_true","delay_false"]})],
            ("generator.oscillator", "freq")  => vec![f!("Freq", v("freq_param",0.01,200.0,1.0,Decade))],
            ("generator.oscillator", "phase") => vec![f!("Phase", v("phase_param",0.0,1.0,0.0,Linear))],
            ("generator.oscillator", "shape") => vec![f!("Shape", Enum{key:"shape",opts:&["sine","triangle","saw","square"]})],
            ("processing.gyro_3dof", "lean_threshold") => vec![f!("Lean", v("lean_threshold",0.01,4.0,0.3,Linear))],
            // ── multi-field rows ──
            ("logic.counter", "mode") => vec![f!("Mode", Enum{key:"mode",opts:&["loop","limit","bounce","unlimited"]})],
            ("logic.counter", "range_mode") => vec![f!("Normalized", Toggle{key:"normalized"})],
            ("logic.counter", "step") => vec![f!("Step", v("step_param",0.001,10_000.0,1.0,Decade))],
            ("logic.counter", "min_max") => vec![
                f!("Min", v("min_param",-1_000_000.0,1_000_000.0,0.0,Linear)),
                f!("Max", v("max_param",-1_000_000.0,1_000_000.0,10.0,Linear)),
            ],
            ("processing.gyro_3dof", "pointer_mode") | ("processing.gyro_3dof", "mode") =>
                vec![f!("Pointer", EnumPair{key_a:"family",key_b:"axis",opts:GYRO_PTR})],
            ("processing.gyro_3dof", "steering_mode") =>
                vec![f!("Steering", EnumPair{key_a:"family",key_b:"axis",opts:GYRO_STEER})],
            ("processing.gyro_3dof", "steering_opts") => vec![
                f!("excl. Y", Toggle{key:"steering_exclude_y"}),
                f!("re-center", v("recenter_strength",0.0,4.0,0.0,Linear)),
                f!("ease", v("reset_ease_in",0.0,2.0,0.25,Linear)),
            ],
            ("processing.gyro_3dof", "gyro_invert") => vec![
                f!("yaw", Toggle{key:"inv_yaw"}),
                f!("pitch", Toggle{key:"inv_pitch"}),
                f!("roll", Toggle{key:"inv_roll"}),
            ],
            ("processing.gyro_3dof", "accel_invert") => vec![
                f!("accX", Toggle{key:"inv_accel_x"}),
                f!("accY", Toggle{key:"inv_accel_y"}),
                f!("accZ", Toggle{key:"inv_accel_z"}),
            ],
            // ── curve option rows ──
            ("module.response_curve", "scale_row") | ("module.twoway_response_curve", "scale_row") => vec![
                f!("Log/Exp", v("scale_t",-1.0,1.0,0.0,Linear)),
                f!("Abs", Toggle{key:"absolute"}),
                f!("Snap", Toggle{key:"snap"}),
            ],
            ("module.vec_response_curve", "scale_row") => vec![
                f!("Log/Exp", v("scale_t",-1.0,1.0,0.0,Linear)),
                f!("Snap", Toggle{key:"snap"}),
            ],
            ("module.response_curve", "range_row") | ("module.twoway_response_curve", "range_row") => vec![
                f!("In↓", v("in_min",-100.0,100.0,-1.0,Linear)),
                f!("In↑", v("in_max",-100.0,100.0,1.0,Linear)),
                f!("Out↓", v("out_min",-100.0,100.0,-1.0,Linear)),
                f!("Out↑", v("out_max",-100.0,100.0,1.0,Linear)),
            ],
            ("module.vec_response_curve", "range_row") => vec![
                f!("In max", v("in_max",-100.0,100.0,1.0,Linear)),
                f!("Out max", v("out_max",-100.0,100.0,1.0,Linear)),
            ],
            ("module.response_curve", "grid_row") | ("module.vec_response_curve", "grid_row")
            | ("module.twoway_response_curve", "grid_row") => vec![
                f!("Grid H", v("grid_x",1.0,20.0,4.0,Linear)),
                f!("Grid V", v("grid_y",1.0,20.0,4.0,Linear)),
                f!("Trail ms", v("trail_ms",0.0,1000.0,300.0,Decade)),
            ],
            ("module.response_curve", "grid_options_row") | ("module.vec_response_curve", "grid_options_row")
            | ("module.twoway_response_curve", "grid_options_row") => vec![
                f!("Scale grid", Toggle{key:"show_scaled_grid"}),
                f!("Labels", Toggle{key:"show_grid_labels"}),
            ],
            ("module.twoway_response_curve", "hyst_row") => vec![
                f!("Hyst %", v("hysteresis_pct",0.001,10.0,0.5,Linear)),
                f!("Hyst ms", v("hysteresis_ms",0.02,50.0,20.0,Linear)),
            ],
            ("module.twoway_response_curve", "interp_row") => vec![
                f!("Interp ms", v("interp_ms",0.0,500.0,50.0,Decade)),
            ],
            ("module.twoway_response_curve", "lane_toggle") => vec![
                f!("Lane", Enum{key:"active_lane",opts:&["up","dn"]}),
            ],
            // Oscilloscope controls row: Win (log ms) / Scale / Auto / Bi-Uni.
            ("display.oscilloscope", "controls") => vec![
                f!("Win ms", v("osc_win_ms",10.0,10_000.0,200.0,Decade)),
                f!("Scale", v("osc_scale",0.001,100.0,1.0,Decade)),
                f!("Auto", Toggle{key:"osc_auto"}),
                f!("Uni", Toggle{key:"osc_uni"}),
            ],
            // Selector mirrors counter's controls.
            ("module.selector", "mode") => vec![f!("Mode", Enum{key:"mode",opts:&["loop","limit","bounce","unlimited"]})],
            ("module.selector", "range_mode") => vec![f!("Normalized", Toggle{key:"normalized"})],
            ("module.selector", "step") => vec![f!("Step", v("step_param",0.001,10_000.0,1.0,Decade))],
            ("module.selector", "min_max") => vec![
                f!("Min", v("min_param",-1_000_000.0,1_000_000.0,0.0,Linear)),
                f!("Max", v("max_param",-1_000_000.0,1_000_000.0,10.0,Linear)),
            ],
            _ => vec![],
        }
    }

    /// Kind of gamepad interaction the selected widget supports.
    fn nav_selected_kind(&self, outer_id: egui_snarl::NodeId) -> NavWidgetKind {
        // The selected item's element_id: a curve module can be pinned as the
        // dot-graph ("curve") or as separate scale/range/grid rows — only the
        // graph element supports dot editing.
        let elem = {
            let canvas = &self.tabs[self.active_tab].canvas;
            canvas.snarl.get_node(outer_id)
                .and_then(|n| n.subpatch.as_ref())
                .and_then(|sp| sp.selected_item.and_then(|i| sp.items.get(i)))
                .and_then(|it| match it {
                    crate::canvas::node::LayoutItem::Module(m) => Some(m.element_id.clone()),
                    _ => None,
                })
        };
        match self.nav_selected_module_id(outer_id).as_deref() {
            Some("module.knob") | Some("module.constant") => NavWidgetKind::Value,
            Some("module.dropdown") => NavWidgetKind::Dropdown,
            Some("module.switch") => NavWidgetKind::Toggle,
            Some("module.response_curve")
            | Some("module.vec_response_curve")
            | Some("module.twoway_response_curve")
                if elem.as_deref() == Some("curve") => NavWidgetKind::Curve,
            Some("module.remapper") | Some("module.map_action")
            | Some("module.automap_combiner") => NavWidgetKind::Remapper,
            // Gyro lean sections are remapper-family mapping rows (Learn/capture +
            // filter), unlike gyro's other elements which are plain field rows.
            Some("processing.gyro_3dof")
                if matches!(elem.as_deref(), Some("lean_left") | Some("lean_right"))
                => NavWidgetKind::Remapper,
            // Everything else with a field definition (single- or multi-control)
            // routes through the unified multi-field editor.
            _ if !self.nav_element_fields(outer_id).is_empty() => NavWidgetKind::MultiField,
            _ => NavWidgetKind::None,
        }
    }

    /// Adjust the selected widget by a directional `delta`. For value widgets
    /// (knob/constant) this is a continuous nudge; for dropdowns it cycles the
    /// selection by sign(delta).
    fn nav_adjust_selected(&mut self, outer_id: egui_snarl::NodeId, delta: f32) {
        // `delta` arrives normalized to a 0..1-style range by the caller. For
        // generic params we rescale it to the param's own (hi-lo) span so the
        // feel is consistent regardless of units (ms, samples, Hz, …).
        let generic = self.nav_value_param(outer_id);
        let Some(inner) = self.nav_selected_inner_node(outer_id) else { return; };
        if let Some(spec) = generic {
            let span = (spec.hi - spec.lo).abs().max(f32::EPSILON);
            let cur = self.get_subpatch_param_f32(outer_id, inner, spec.key).unwrap_or(spec.default);
            let next = (cur + delta * span).clamp(spec.lo, spec.hi);
            self.set_subpatch_param_f32(outer_id, inner, spec.key, next);
            return;
        }
        let canvas = &mut self.tabs[self.active_tab].canvas;
        let Some(sp) = canvas.snarl.get_node_mut(outer_id).and_then(|n| n.subpatch.as_mut()) else { return; };
        let Some(node) = sp.snarl.get_node_mut(inner) else { return; };
        match node.module_id.as_str() {
            "module.knob" => {
                let bipolar = node.params.get("bipolar").and_then(|v| v.as_bool()).unwrap_or(false);
                let (lo, hi) = if bipolar { (-1.0f32, 1.0f32) } else { (0.0f32, 1.0f32) };
                let cur = node.params.get("value").and_then(|v| v.as_f64()).unwrap_or(0.0) as f32;
                let next = (cur + delta).clamp(lo, hi);
                node.params.insert("value".to_string(), serde_json::Value::from(next as f64));
            }
            "module.constant" => {
                // Constants are unbounded; scale the nudge a bit larger.
                let cur = node.params.get("value").and_then(|v| v.as_f64()).unwrap_or(0.0) as f32;
                node.params.insert("value".to_string(),
                    serde_json::Value::from((cur + delta) as f64));
            }
            _ => {}
        }
    }

    /// Proportional (log-ish) adjust for a generic numeric widget. `press` is a
    /// per-frame discrete impulse (±1 from dpad rising), `cont` is the stick X
    /// (-1..1), `fine` halves the rates. The step scales with the value's own
    /// magnitude so wide ranges (0..60000) are usable: ~6%/press coarse,
    /// ~1.5%/press fine; continuous ~120%/s coarse, ~25%/s fine at full stick.
    /// A range-derived floor lets the value climb off exactly 0.
    #[allow(dead_code)]
    fn nav_adjust_generic(&mut self, outer_id: egui_snarl::NodeId,
        press: f32, cont: f32, fine: bool, dt: f32)
    {
        let Some(spec) = self.nav_value_param(outer_id) else { return; };
        let Some(inner) = self.nav_selected_inner_node(outer_id) else { return; };
        let cur = self.get_subpatch_param_f32(outer_id, inner, spec.key).unwrap_or(spec.default);
        let (lo, hi) = (spec.lo, spec.hi);

        // Per-press discrete step.
        let press_step = match spec.step {
            // Decade: step = magnitude's decade (1/10/100/…) × {1 coarse, 0.1 fine}.
            // <10 → 1 (fine 0.1); 10–100 → 5? — user wants 5 in the 10–100 band.
            // Use: decade d = 10^floor(log10(v)); coarse = d (with a 5× bump in
            // the [1,10)·d upper half? no — keep simple decade), fine = d/10.
            NavStep::Decade => {
                // Band by the value's decade: 1,10,100,… Coarse step per band:
                //   <10 → 1   |  10–100 → 5  |  100–1000 → 50  |  1000+ → 500 …
                // i.e. 1 in the first band, half-decade above. Fine = coarse/10
                // (so <10 fine = 0.1 ms sub-ms, 10–100 fine = 0.5, etc.).
                let v = (cur - lo).abs().max(1e-6);
                let decade = 10f32.powf(v.log10().floor()).max(1.0); // 1,10,100,…
                let coarse = if decade <= 1.0 { 1.0 } else { decade * 0.5 };
                if fine { coarse * 0.1 } else { coarse }
            }
            // Linear (phase etc.): fixed fraction of the 0..1-ish range.
            NavStep::Linear => {
                let span = (hi - lo).abs().max(f32::EPSILON);
                span * if fine { 0.005 } else { 0.02 }
            }
        };

        // Continuous stick step: an accelerated curve on |cont| (gentle low,
        // fast at full deflection), scaled to the same step magnitude per second.
        let accel = self.settings.cursor_accel.max(1.0);
        let cont_curved = cont.signum() * cont.abs().clamp(0.0, 1.0).powf(accel);
        // At full deflection, ~ (press_step × steps_per_sec). Coarse ≈ 25/s of
        // the press step, fine ≈ 8/s.
        let cont_per_s = press_step * if fine { 8.0 } else { 25.0 };

        let mut delta = 0.0f32;
        if press != 0.0 { delta += press * press_step; }
        if cont != 0.0  { delta += cont_curved * cont_per_s * dt; }
        if delta == 0.0 { return; }

        let next = (cur + delta).clamp(lo, hi);
        // Decade params that represent whole units (samples) read better rounded;
        // ms/Hz tolerate fractions. Round sample counts to integers.
        let next = if spec.key == "buf_size" { next.round() } else { next };
        self.set_subpatch_param_f32(outer_id, inner, spec.key, next);
    }

    /// Read/write an f32 param on an INNER sub-patch node (inside outer_id's
    /// sub-patch snarl).
    fn get_subpatch_param_f32(&self, outer_id: egui_snarl::NodeId,
        inner: egui_snarl::NodeId, key: &str) -> Option<f32>
    {
        let canvas = &self.tabs[self.active_tab].canvas;
        let sp = canvas.snarl.get_node(outer_id)?.subpatch.as_ref()?;
        sp.snarl.get_node(inner)?.params.get(key)?.as_f64().map(|v| v as f32)
    }
    fn set_subpatch_param_f32(&mut self, outer_id: egui_snarl::NodeId,
        inner: egui_snarl::NodeId, key: &str, val: f32)
    {
        let canvas = &mut self.tabs[self.active_tab].canvas;
        let Some(sp) = canvas.snarl.get_node_mut(outer_id).and_then(|n| n.subpatch.as_mut()) else { return; };
        if let Some(node) = sp.snarl.get_node_mut(inner) {
            node.params.insert(key.to_string(), serde_json::Value::from(val as f64));
        }
    }
    /// Store an integer-valued param as a JSON integer (renderers that read with
    /// `as_i64()` ignore a JSON float).
    fn set_subpatch_param_i64(&mut self, outer_id: egui_snarl::NodeId,
        inner: egui_snarl::NodeId, key: &str, val: i64)
    {
        let canvas = &mut self.tabs[self.active_tab].canvas;
        let Some(sp) = canvas.snarl.get_node_mut(outer_id).and_then(|n| n.subpatch.as_mut()) else { return; };
        if let Some(node) = sp.snarl.get_node_mut(inner) {
            node.params.insert(key.to_string(), serde_json::Value::from(val));
        }
    }

    /// Open or close the selected dropdown's pinned popup (the real options
    /// list), matching the click-to-toggle popup the renderer manages in egui
    /// memory under `("dropdown_pinned_popup", inner_id.0)`.
    fn nav_set_dropdown_popup(&self, ctx: &egui::Context, outer_id: egui_snarl::NodeId, open: bool) {
        if !matches!(self.nav_selected_module_id(outer_id).as_deref(), Some("module.dropdown")) {
            return;
        }
        let Some(inner) = self.nav_selected_inner_node(outer_id) else { return; };
        let popup_id = egui::Id::new(("dropdown_pinned_popup", inner.0));
        ctx.memory_mut(|m| {
            let is_open = m.is_popup_open(popup_id);
            if open && !is_open { m.open_popup(popup_id); }
            else if !open && is_open { m.close_popup(popup_id); }
        });
    }

    /// Cycle the selected dropdown by `dir` (+1/-1), wrapping. No-op otherwise.
    fn nav_cycle_dropdown(&mut self, outer_id: egui_snarl::NodeId, dir: i32) {
        let Some(inner) = self.nav_selected_inner_node(outer_id) else { return; };
        let canvas = &mut self.tabs[self.active_tab].canvas;
        let Some(sp) = canvas.snarl.get_node_mut(outer_id).and_then(|n| n.subpatch.as_mut()) else { return; };
        let Some(node) = sp.snarl.get_node_mut(inner) else { return; };
        if node.module_id != "module.dropdown" { return; }
        let n = node.params.get("options").and_then(|v| v.as_array()).map(|a| a.len()).unwrap_or(0);
        if n == 0 { return; }
        let cur = node.params.get("selected_index").and_then(|v| v.as_u64()).unwrap_or(0) as i32;
        let next = (cur + dir).rem_euclid(n as i32);
        node.params.insert("selected_index".to_string(), serde_json::Value::from(next as u64));
    }

    /// Toggle the selected switch's `active` state. Returns true if a switch was
    /// toggled (so the caller can record undo).
    fn nav_toggle_switch(&mut self, outer_id: egui_snarl::NodeId) -> bool {
        let Some(inner) = self.nav_selected_inner_node(outer_id) else { return false; };
        let canvas = &mut self.tabs[self.active_tab].canvas;
        let Some(sp) = canvas.snarl.get_node_mut(outer_id).and_then(|n| n.subpatch.as_mut()) else { return false; };
        let Some(node) = sp.snarl.get_node_mut(inner) else { return false; };
        if node.module_id != "module.switch" { return false; }
        // Current state must come from the engine's last emitted value when
        // present (it reconciles UI clicks with direct/latch inputs); the
        // persisted `active` is only a fallback. Toggling the stale param alone
        // gets overwritten by `last_out` next frame, so we also bump the
        // `ui_toggle_seq` the engine watches — exactly like a mouse click
        // (`switch_handle_click`).
        let cur = match node.extra.last_out.first() {
            Some(Some(Signal::Bool(b))) => *b,
            _ => node.params.get("active").and_then(|v| v.as_bool()).unwrap_or(false),
        };
        let seq = node.params.get("ui_toggle_seq").and_then(|v| v.as_u64()).unwrap_or(0);
        node.params.insert("ui_toggle_seq".to_string(), serde_json::Value::from(seq.wrapping_add(1)));
        node.params.insert("active".to_string(), serde_json::Value::Bool(!cur));
        true
    }

    /// Read/parse the selected curve node's `points` (Vec<[f32;2]>), if the
    /// selection is a response-curve module. Returns (inner_node_id, points).
    /// Param keys (points, biases) for the curve's currently-edited lane. The
    /// two-way curve has an up lane (`points`) and a down lane (`points_dn`),
    /// switched by its `active_lane` param; the driver edits whichever is active
    /// so it matches the lane shown (and glowed) in the body. Other curves only
    /// have `points`.
    fn nav_curve_keys(&self, outer_id: egui_snarl::NodeId, inner: egui_snarl::NodeId)
        -> (&'static str, &'static str)
    {
        let canvas = &self.tabs[self.active_tab].canvas;
        let lane_dn = canvas.snarl.get_node(outer_id)
            .and_then(|n| n.subpatch.as_ref())
            .and_then(|sp| sp.snarl.get_node(inner))
            .filter(|node| node.module_id == "module.twoway_response_curve")
            .and_then(|node| node.params.get("active_lane").and_then(|v| v.as_str()))
            == Some("dn");
        if lane_dn { ("points_dn", "biases_dn") } else { ("points", "biases") }
    }

    fn nav_curve_points(&self, outer_id: egui_snarl::NodeId)
        -> Option<(egui_snarl::NodeId, Vec<[f32; 2]>)>
    {
        let inner = self.nav_selected_inner_node(outer_id)?;
        let canvas = &self.tabs[self.active_tab].canvas;
        let sp = canvas.snarl.get_node(outer_id)?.subpatch.as_ref()?;
        let node = sp.snarl.get_node(inner)?;
        if !matches!(node.module_id.as_str(),
            "module.response_curve" | "module.vec_response_curve" | "module.twoway_response_curve")
        { return None; }
        let (pts_key, _) = self.nav_curve_keys(outer_id, inner);
        let pts: Vec<[f32; 2]> = node.params.get(pts_key)?.as_array()?
            .iter().filter_map(|p| {
                let a = p.as_array()?;
                Some([a.get(0)?.as_f64()? as f32, a.get(1)?.as_f64()? as f32])
            }).collect();
        if pts.len() < 2 { return None; }
        Some((inner, pts))
    }

    /// Write a points Vec back to a curve node (keeps `biases` length in sync
    /// at one-per-segment, padding/truncating with 0.0).
    fn nav_curve_write_points(&mut self, inner: egui_snarl::NodeId,
        outer_id: egui_snarl::NodeId, pts: &[[f32; 2]])
    {
        let (pts_key, bias_key) = self.nav_curve_keys(outer_id, inner);
        let canvas = &mut self.tabs[self.active_tab].canvas;
        let Some(sp) = canvas.snarl.get_node_mut(outer_id).and_then(|n| n.subpatch.as_mut())
        else { return; };
        let Some(node) = sp.snarl.get_node_mut(inner) else { return; };
        let arr: Vec<serde_json::Value> = pts.iter()
            .map(|p| serde_json::json!([p[0] as f64, p[1] as f64])).collect();
        node.params.insert(pts_key.into(), serde_json::Value::Array(arr));
        // biases: one per segment (points-1).
        let want = pts.len().saturating_sub(1);
        let mut biases: Vec<f64> = node.params.get(bias_key)
            .and_then(|v| v.as_array())
            .map(|a| a.iter().filter_map(|b| b.as_f64()).collect())
            .unwrap_or_default();
        biases.resize(want, 0.0);
        node.params.insert(bias_key.into(),
            serde_json::Value::Array(biases.into_iter().map(serde_json::Value::from).collect()));
    }

    /// Graph X/Y span for a curve (absolute curves: 0..1; bipolar: -1..1).
    /// Derived from the published geometry bounds.
    fn nav_curve_bounds(&self, ctx: &egui::Context, inner: egui_snarl::NodeId)
        -> (f32, f32, f32, f32)
    {
        self.nav_curve_geom(ctx, inner)
            .map(|(_, xl, xh, yl, yh)| (xl, xh, yl, yh))
            .unwrap_or((0.0, 1.0, 0.0, 1.0))
    }

    /// Convert the cursor's screen pos to graph coords, if the cursor is over
    /// this curve's graph rect.
    fn nav_curve_cursor_graph(&self, ctx: &egui::Context, inner: egui_snarl::NodeId)
        -> Option<[f32; 2]>
    {
        let (rect, x_lo, x_hi, y_lo, y_hi) = self.nav_curve_geom(ctx, inner)?;
        let p = self.gamepad_nav.cursor_pos;
        if !rect.contains(p) { return None; }
        Some([
            x_lo + (p.x - rect.left()) / rect.width() * (x_hi - x_lo),
            y_lo + (rect.bottom() - p.y) / rect.height() * (y_hi - y_lo),
        ])
    }

    /// Index of the curve dot nearest the cursor (when over the graph), else None.
    fn nav_curve_dot_near_cursor(&self, ctx: &egui::Context,
        outer_id: egui_snarl::NodeId, inner: egui_snarl::NodeId) -> Option<usize>
    {
        let g = self.nav_curve_cursor_graph(ctx, inner)?;
        let (_, pts) = self.nav_curve_points(outer_id)?;
        let (x_lo, x_hi, y_lo, y_hi) = self.nav_curve_bounds(ctx, inner);
        let sx = (x_hi - x_lo).abs().max(f32::EPSILON);
        let sy = (y_hi - y_lo).abs().max(f32::EPSILON);
        pts.iter().enumerate()
            .min_by(|(_, a), (_, b)| {
                let da = ((a[0]-g[0])/sx).powi(2) + ((a[1]-g[1])/sy).powi(2);
                let db = ((b[0]-g[0])/sx).powi(2) + ((b[1]-g[1])/sy).powi(2);
                da.partial_cmp(&db).unwrap()
            })
            .map(|(i, _)| i)
    }

    /// Add a dot at the cursor's graph position (when the cursor is over the
    /// graph). Returns the inserted index, or None if the cursor isn't on it.
    fn nav_curve_add_at_cursor(&mut self, ctx: &egui::Context,
        outer_id: egui_snarl::NodeId, inner: egui_snarl::NodeId) -> Option<usize>
    {
        let g = self.nav_curve_cursor_graph(ctx, inner)?;
        let (x_lo, x_hi, y_lo, y_hi) = self.nav_curve_bounds(ctx, inner);
        let (_, mut pts) = self.nav_curve_points(outer_id)?;
        let gx = g[0].clamp(x_lo, x_hi);
        let gy = g[1].clamp(y_lo, y_hi);
        let idx = pts.partition_point(|p| p[0] < gx);
        pts.insert(idx, [gx, gy]);
        self.nav_curve_write_points(inner, outer_id, &pts);
        Some(idx)
    }

    /// Delete a specific dot index (guards endpoints / min 2 points).
    fn nav_curve_delete_index(&mut self, outer_id: egui_snarl::NodeId, idx: usize) -> bool {
        let Some((inner, mut pts)) = self.nav_curve_points(outer_id) else { return false; };
        if pts.len() <= 2 { return false; }
        // Keep the two endpoints; only interior dots are deletable.
        if idx == 0 || idx >= pts.len() - 1 { return false; }
        pts.remove(idx);
        self.nav_curve_write_points(inner, outer_id, &pts);
        true
    }

    /// Move dot `i` by (dx, dy) in graph space. Endpoints keep their fixed X
    /// (only Y moves); interior dots clamp X between neighbors.
    fn nav_curve_move_dot(&mut self, outer_id: egui_snarl::NodeId, i: usize, dx: f32, dy: f32) {
        let Some((inner, mut pts)) = self.nav_curve_points(outer_id) else { return; };
        if i >= pts.len() { return; }
        // Bounds from the node: absolute → 0..1 ; bipolar → -1..1. Infer from
        // the existing endpoints' x.
        let x_lo = pts.first().map(|p| p[0]).unwrap_or(0.0);
        let x_hi = pts.last().map(|p| p[0]).unwrap_or(1.0);
        let (y_lo, y_hi) = if x_lo < 0.0 { (-1.0, 1.0) } else { (0.0, 1.0) };
        let is_end = i == 0 || i == pts.len() - 1;
        let new_x = if is_end {
            pts[i][0] // endpoints fixed in X
        } else {
            let lo = pts[i - 1][0] + 0.001;
            let hi = pts[i + 1][0] - 0.001;
            (pts[i][0] + dx).clamp(lo, hi)
        };
        let new_y = (pts[i][1] + dy).clamp(y_lo, y_hi);
        pts[i] = [new_x, new_y];
        self.nav_curve_write_points(inner, outer_id, &pts);
    }

    /// Adjust the bias (curvature) of the segment to the RIGHT of dot `i` by
    /// `db`, clamped to [-1, 1]. (Biases are one-per-segment.)
    fn nav_curve_adjust_bias(&mut self, outer_id: egui_snarl::NodeId, i: usize, db: f32) {
        let Some(inner) = self.nav_selected_inner_node(outer_id) else { return; };
        let (pts_key, bias_key) = self.nav_curve_keys(outer_id, inner);
        let canvas = &mut self.tabs[self.active_tab].canvas;
        let Some(sp) = canvas.snarl.get_node_mut(outer_id).and_then(|n| n.subpatch.as_mut()) else { return; };
        let Some(node) = sp.snarl.get_node_mut(inner) else { return; };
        let n_pts = node.params.get(pts_key).and_then(|v| v.as_array()).map(|a| a.len()).unwrap_or(0);
        if n_pts < 2 { return; }
        let seg = i.min(n_pts - 2); // segment index to the right of dot i
        let mut biases: Vec<f64> = node.params.get(bias_key)
            .and_then(|v| v.as_array())
            .map(|a| a.iter().filter_map(|b| b.as_f64()).collect())
            .unwrap_or_default();
        biases.resize(n_pts - 1, 0.0);
        biases[seg] = (biases[seg] as f32 + db).clamp(-1.0, 1.0) as f64;
        node.params.insert(bias_key.into(),
            serde_json::Value::Array(biases.into_iter().map(serde_json::Value::from).collect()));
    }

    /// Read the curve geometry the curve body published last frame:
    /// (graph rect, x_lo, x_hi, y_lo, y_hi). Lets the driver map graph↔screen.
    fn nav_curve_geom(&self, ctx: &egui::Context, inner: egui_snarl::NodeId)
        -> Option<(egui::Rect, f32, f32, f32, f32)>
    {
        let g: (u64, egui::Rect, f32, f32, f32, f32) =
            ctx.data(|d| d.get_temp(egui::Id::new(("gp_nav_curve_geom", inner.0))))?;
        Some((g.1, g.2, g.3, g.4, g.5))
    }

    /// Publish the highlighted dot + editing flag so the curve body rings it.
    fn nav_publish_curve_sel(&self, ctx: &egui::Context, inner: egui_snarl::NodeId, editing: bool) {
        let pass = ctx.cumulative_pass_nr();
        let idx = self.gamepad_nav.curve_dot;
        ctx.data_mut(|d| d.insert_temp(
            egui::Id::new(("gp_nav_curve_sel", inner.0)), (pass, idx, editing)));
    }

    /// `CurveDots` level: dpad/LS highlights a dot, RT adds (at cursor if
    /// visible else largest gap), LT deletes (nearest cursor / highlighted),
    /// South enters dot move, East exits the curve.
    fn nav_drive_curve_dots(
        &mut self,
        ctx: &egui::Context,
        outer_id: egui_snarl::NodeId,
        nav: &crate::gamepad_nav::NavInput,
        step_dir: Option<crate::gamepad_nav::NavDir>,
        rt_rising: bool,
        lt_rising: bool,
    ) {
        use crate::gamepad_nav::{EditLevel, NavDir};
        let Some((inner, pts)) = self.nav_curve_points(outer_id) else {
            // Not a curve any more — bail to widget level.
            self.gamepad_nav.edit_level = EditLevel::Widget;
            return;
        };
        // Clamp highlight to valid range.
        self.gamepad_nav.curve_dot = self.gamepad_nav.curve_dot.min(pts.len() - 1);

        // East → exit the curve (commit one undo entry for the whole session).
        if nav.is_rising("btn_east") {
            self.gamepad_nav.edit_level = EditLevel::Widget;
            if let Some(b) = self.gamepad_nav.edit_baseline.take() {
                self.tabs[self.active_tab].canvas.commit_undo_if_changed(*b);
            }
            return;
        }

        // dpad/LS left-right steps the highlight between dots.
        match step_dir {
            Some(NavDir::Left) => {
                self.gamepad_nav.curve_dot = self.gamepad_nav.curve_dot.saturating_sub(1);
            }
            Some(NavDir::Right) => {
                self.gamepad_nav.curve_dot = (self.gamepad_nav.curve_dot + 1).min(pts.len() - 1);
            }
            _ => {}
        }

        // RT/LT are CURSOR-DRIVEN: they require the RS/gyro cursor to be over
        // the graph. RT adds a dot at the cursor's graph position; LT deletes the
        // dot nearest the cursor. When the cursor isn't over the graph (e.g. the
        // user is stepping dots with the dpad), the triggers do nothing — so the
        // action is always exactly where the cursor points.
        let cursor_on_graph =
            self.gamepad_nav.cursor_visible
            && self.nav_curve_cursor_graph(ctx, inner).is_some();
        if rt_rising && cursor_on_graph {
            let base = self.tabs[self.active_tab].canvas.snapshot_for_undo();
            if let Some(idx) = self.nav_curve_add_at_cursor(ctx, outer_id, inner) {
                self.gamepad_nav.curve_dot = idx;
                self.tabs[self.active_tab].canvas.commit_undo_if_changed(base);
            }
        }
        if lt_rising && cursor_on_graph {
            if let Some(target) = self.nav_curve_dot_near_cursor(ctx, outer_id, inner) {
                let base = self.tabs[self.active_tab].canvas.snapshot_for_undo();
                if self.nav_curve_delete_index(outer_id, target) {
                    self.gamepad_nav.curve_dot = self.gamepad_nav.curve_dot.min(
                        self.nav_curve_points(outer_id).map(|(_, p)| p.len().saturating_sub(1)).unwrap_or(0));
                    self.tabs[self.active_tab].canvas.commit_undo_if_changed(base);
                }
            }
        }

        // South → grab the highlighted dot under the cursor first (if visible).
        if nav.is_rising("btn_south") {
            if self.gamepad_nav.cursor_visible {
                if let Some(idx) = self.nav_curve_dot_near_cursor(ctx, outer_id, inner) {
                    self.gamepad_nav.curve_dot = idx;
                }
            }
            self.gamepad_nav.edit_level = EditLevel::CurveDot;
            self.gamepad_nav.fine_increment = false;
        }

        self.nav_publish_curve_sel(ctx, inner, false);
    }

    /// `CurveDot` level: dpad/LS moves the highlighted dot in X/Y; hold-North
    /// edits the segment curvature (bias); East/South returns to dot nav.
    fn nav_drive_curve_dot(
        &mut self,
        ctx: &egui::Context,
        outer_id: egui_snarl::NodeId,
        nav: &crate::gamepad_nav::NavInput,
        dt: f32,
        step_dir: Option<crate::gamepad_nav::NavDir>,
        rt_rising: bool,
        lt_rising: bool,
    ) {
        use crate::gamepad_nav::EditLevel;
        let _ = (rt_rising, lt_rising, step_dir);
        let Some((inner, pts)) = self.nav_curve_points(outer_id) else {
            self.gamepad_nav.edit_level = EditLevel::Widget;
            return;
        };
        let i = self.gamepad_nav.curve_dot.min(pts.len() - 1);

        // East / South → back to dot navigation.
        if nav.is_rising("btn_east") || nav.is_rising("btn_south") {
            self.gamepad_nav.edit_level = EditLevel::CurveDots;
            self.nav_publish_curve_sel(ctx, inner, false);
            return;
        }
        // West → toggle fine increments.
        if nav.is_rising("btn_west") {
            self.gamepad_nav.fine_increment = !self.gamepad_nav.fine_increment;
        }

        let fine = self.gamepad_nav.fine_increment;
        let mag = nav.lstick.length();

        // Hold North → adjust segment curvature (bias) instead of moving the
        // dot. Bias spans only [-1, 1], so rates are deliberately gentle — a
        // full-deflection hold takes several seconds to cross the range, and
        // fine is much slower again for precise shaping.
        if nav.pressed.contains("btn_north") {
            let mut db = 0.0f32;
            let s = if fine { 0.003 } else { 0.012 }; // per dpad press
            // Discrete: dpad rising edges only (stick is the continuous path).
            if nav.is_rising("dpad_right") || nav.is_rising("dpad_up") { db += s; }
            if nav.is_rising("dpad_left") || nav.is_rising("dpad_down") { db -= s; }
            if mag > 0.3 {
                // Continuous: ~0.15/s coarse, ~0.03/s fine at full deflection.
                let rate = if fine { 0.03 } else { 0.15 };
                db += nav.lstick.y * rate * dt;
            }
            if db != 0.0 { self.nav_curve_adjust_bias(outer_id, i, db); }
            self.nav_publish_curve_sel(ctx, inner, true);
            // Tell the body to show its bias (curvature) handles this frame.
            let pass = ctx.cumulative_pass_nr();
            ctx.data_mut(|d| d.insert_temp(
                egui::Id::new(("gp_nav_curve_bias", inner.0)), pass));
            return;
        }

        // Move the dot in X/Y. dpad = one discrete step (rising edge only);
        // LS = continuous. Using dpad rising avoids double-applying when the
        // stick auto-repeat would also fill `step_dir`.
        let _ = step_dir;
        let step = if fine { 0.0015 } else { 0.015 };
        let mut dx = 0.0f32;
        let mut dy = 0.0f32;
        if nav.is_rising("dpad_left") { dx -= step; }
        if nav.is_rising("dpad_right") { dx += step; }
        if nav.is_rising("dpad_up") { dy += step; }
        if nav.is_rising("dpad_down") { dy -= step; }
        if mag > 0.08 {
            // Accelerated stick response (gentle first half, fast toward full
            // deflection) — speed scales with |axis|^accel per axis, matching the
            // cursor feel. Coarse top ≈0.5/s, fine top ≈0.07/s (graph units/s).
            let accel = self.settings.cursor_accel.max(1.0);
            let top = if fine { 0.07 } else { 0.5 };
            let curve = |a: f32| -> f32 {
                a.signum() * a.abs().clamp(0.0, 1.0).powf(accel) * top * dt
            };
            dx += curve(nav.lstick.x);
            dy += curve(nav.lstick.y); // +y up
        }
        if dx != 0.0 || dy != 0.0 {
            self.nav_curve_move_dot(outer_id, i, dx, dy);
        }
        self.nav_publish_curve_sel(ctx, inner, true);
    }

    /// Reset the selected widget's value to its default (0.0 for knob/constant;
    /// the descriptor default for generic numeric widgets).
    fn nav_reset_selected(&mut self, outer_id: egui_snarl::NodeId) {
        if let Some(spec) = self.nav_value_param(outer_id) {
            if let Some(inner) = self.nav_selected_inner_node(outer_id) {
                self.set_subpatch_param_f32(outer_id, inner, spec.key, spec.default);
            }
            return;
        }
        let Some(inner) = self.nav_selected_inner_node(outer_id) else { return; };
        let canvas = &mut self.tabs[self.active_tab].canvas;
        let Some(sp) = canvas.snarl.get_node_mut(outer_id).and_then(|n| n.subpatch.as_mut()) else { return; };
        let Some(node) = sp.snarl.get_node_mut(inner) else { return; };
        if matches!(node.module_id.as_str(), "module.knob" | "module.constant") {
            node.params.insert("value".to_string(), serde_json::Value::from(0.0f64));
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
        fired
    }

    /// Non-nav-only shortcut-chord detection: scan every eligible gamepad
    /// (non-MIDI, not our own loopback virtual) for the assigned see-through /
    /// panic combos and fire once per full press. Only called when FlexInput is
    /// focused and `gamepad_chords_nav_only` is false.
    fn check_shortcut_chords_global(&mut self, ctx: &egui::Context) {
        if !ctx.input(|i| i.focused) { return; }
        // Nothing to do if no combos are assigned.
        if self.settings.seethrough_chord.is_none() && self.settings.panic_chord.is_none() {
            self.gamepad_nav.seethrough_chord_down = false;
            self.gamepad_nav.panic_chord_down = false;
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
        let screen = ctx.screen_rect();
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

        let screen = ctx.screen_rect();
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

    /// Render the Settings modal. Reads/writes `self.settings`, mirrors live
    /// values into the engine/I-O atomics, and flips `settings_dirty` so the
    /// outer update loop persists settings.json at end of frame.
    /// Ordered list of gamepad-navigable settings rows. Keeps the panel and the
    /// driver in lock-step (same indices). Each row is a kind + label; the
    /// numeric kinds carry their range/step so the driver can nudge them.
    fn gp_settings_rows(&self) -> Vec<GpSettingRow> {
        use GpSettingKind::*;
        vec![
            GpSettingRow { label: "Max polling rate".into(),
                kind: IntSlider { lo: settings::POLLING_HZ_MIN as f32,
                    hi: settings::POLLING_HZ_MAX as f32, step: 50.0,
                    key: GpSettingKey::PollingHz }, suffix: " Hz" },
            GpSettingRow { label: "Processing sample rate".into(),
                kind: IntSlider { lo: settings::SAMPLE_RATE_HZ_MIN as f32,
                    hi: settings::SAMPLE_RATE_HZ_MAX as f32, step: 50.0,
                    key: GpSettingKey::SampleRateHz }, suffix: " Hz" },
            GpSettingRow { label: "Background repaint rate".into(),
                kind: IntSlider { lo: settings::BG_REPAINT_HZ_MIN as f32,
                    hi: settings::BG_REPAINT_HZ_MAX as f32, step: 1.0,
                    key: GpSettingKey::BgRepaintHz }, suffix: " Hz" },
            GpSettingRow { label: "Gamepad UI nav by default".into(),
                kind: Toggle { key: GpSettingKey::NavDefault }, suffix: "" },
            GpSettingRow { label: "Cursor max speed".into(),
                kind: IntSlider { lo: 1000.0, hi: 30000.0, step: 250.0,
                    key: GpSettingKey::CursorMaxSpeed }, suffix: " px/s" },
            GpSettingRow { label: "Cursor acceleration".into(),
                kind: FloatSlider { lo: 1.0, hi: 4.0, step: 0.05,
                    key: GpSettingKey::CursorAccel }, suffix: "" },
            GpSettingRow { label: "Contrast".into(),
                kind: FloatSlider { lo: -1.0, hi: 1.0, step: 0.05,
                    key: GpSettingKey::Contrast }, suffix: "" },
            GpSettingRow { label: "Keep tabs on launch".into(),
                kind: Toggle { key: GpSettingKey::KeepWorkspace }, suffix: "" },
            GpSettingRow { label: "Collapse new device nodes".into(),
                kind: Toggle { key: GpSettingKey::CollapseNodes }, suffix: "" },
            GpSettingRow { label: "Show own virtuals as physical".into(),
                kind: Toggle { key: GpSettingKey::ShowVirtuals }, suffix: "" },
            GpSettingRow { label: "Default deadzone".into(),
                kind: FloatSlider { lo: 0.0, hi: 0.5, step: 0.005,
                    key: GpSettingKey::DefDeadzone }, suffix: "" },
            GpSettingRow { label: "Default gyro ×".into(),
                kind: FloatSlider { lo: 0.1, hi: 50.0, step: 0.05,
                    key: GpSettingKey::DefGyroMult }, suffix: "" },
            GpSettingRow { label: "Default mouse speed".into(),
                kind: FloatSlider { lo: 0.0, hi: 3000.0, step: 1.0,
                    key: GpSettingKey::DefMouseSens }, suffix: "" },
            GpSettingRow { label: "Shortcuts: nav-only".into(),
                kind: Toggle { key: GpSettingKey::ChordsNavOnly }, suffix: "" },
            GpSettingRow { label: "Shortcut: See-through".into(),
                kind: ChordLearn { target: crate::gamepad_nav::ChordTarget::SeeThrough }, suffix: "" },
            GpSettingRow { label: "Shortcut: Panic".into(),
                kind: ChordLearn { target: crate::gamepad_nav::ChordTarget::Panic }, suffix: "" },
        ]
    }

    /// Current numeric value of a settings key (bools as 0/1).
    fn gp_setting_value(&self, key: GpSettingKey) -> f32 {
        use GpSettingKey::*;
        match key {
            PollingHz => self.settings.polling_hz as f32,
            SampleRateHz => self.settings.sample_rate_hz as f32,
            BgRepaintHz => self.settings.bg_repaint_hz as f32,
            NavDefault => self.settings.gamepad_ui_nav_default as i32 as f32,
            CursorMaxSpeed => self.settings.cursor_max_speed,
            CursorAccel => self.settings.cursor_accel,
            Contrast => self.settings.contrast,
            KeepWorkspace => self.settings.keep_workspace as i32 as f32,
            CollapseNodes => self.settings.device_nodes_default_collapsed as i32 as f32,
            ShowVirtuals => self.settings.show_own_virtuals_as_physical as i32 as f32,
            DefDeadzone => self.settings.default_stick_deadzone,
            DefGyroMult => self.settings.default_gyro_mult,
            DefMouseSens => self.settings.default_mouse_sensitivity,
            ChordsNavOnly => self.settings.gamepad_chords_nav_only as i32 as f32,
        }
    }

    /// Write a numeric value to a settings key (bools from !=0), pushing any
    /// live-thread side effects, and mark settings dirty.
    fn gp_setting_set(&mut self, key: GpSettingKey, val: f32) {
        use GpSettingKey::*;
        match key {
            PollingHz => {
                let v = (val.round() as u32)
                    .clamp(settings::POLLING_HZ_MIN, settings::POLLING_HZ_MAX);
                self.settings.polling_hz = v;
                self.polling_hz.store(v, Ordering::Relaxed);
            }
            SampleRateHz => {
                let v = (val.round() as u32)
                    .clamp(settings::SAMPLE_RATE_HZ_MIN, settings::SAMPLE_RATE_HZ_MAX);
                self.settings.sample_rate_hz = v;
                self.sample_rate_hz.store(v, Ordering::Relaxed);
            }
            BgRepaintHz => {
                let v = (val.round() as u32)
                    .clamp(settings::BG_REPAINT_HZ_MIN, settings::BG_REPAINT_HZ_MAX);
                self.settings.bg_repaint_hz = v;
            }
            NavDefault => self.settings.gamepad_ui_nav_default = val != 0.0,
            CursorMaxSpeed => self.settings.cursor_max_speed = val.clamp(1000.0, 30000.0),
            CursorAccel => self.settings.cursor_accel = val.clamp(1.0, 4.0),
            Contrast => self.settings.contrast = val.clamp(-1.0, 1.0),
            KeepWorkspace => {
                self.settings.keep_workspace = val != 0.0;
                if !self.settings.keep_workspace { settings::delete_workspace(); }
            }
            CollapseNodes => self.settings.device_nodes_default_collapsed = val != 0.0,
            ShowVirtuals => self.settings.show_own_virtuals_as_physical = val != 0.0,
            DefDeadzone => self.settings.default_stick_deadzone = val.clamp(0.0, 0.5),
            DefGyroMult => self.settings.default_gyro_mult = val.clamp(0.1, 50.0),
            DefMouseSens => self.settings.default_mouse_sensitivity = val.clamp(0.0, 3000.0),
            ChordsNavOnly => self.settings.gamepad_chords_nav_only = val != 0.0,
        }
        self.settings_dirty = true;
    }

    /// Drive the gamepad settings panel (modal). dpad/stick up/down moves the
    /// highlighted row; South toggles a bool or enters/exits numeric edit; while
    /// editing, left/right nudges the value. East closes (or exits edit). West
    /// = fine step. North = (numeric) reset is not wired — values have explicit
    /// ranges; skip.
    fn nav_drive_gp_settings(
        &mut self,
        nav: &crate::gamepad_nav::NavInput,
        rt_rising: bool,
        lt_rising: bool,
    ) {
        use crate::gamepad_nav::{self as gn, NavDir};
        let rows = self.gp_settings_rows();
        if rows.is_empty() { return; }
        let editing = self.gamepad_nav.settings_editing;

        // ── Shortcut-chord capture (panel stays open, mirrors the widget Learn
        // flow exactly) ─────────────────────────────────────────────────────
        // When a ChordLearn row is "learning", the panel is listening: we wait
        // for the device to go idle ONCE (so the South press that started the
        // capture isn't swept in), then accumulate every held button (ANY pin
        // is bindable — North included), and the moment everything releases we
        // latch the combo into the target setting and exit capture, leaving the
        // panel open on the same row. East aborts capture (back), no binding
        // written. This is identical to how a widget's Learn captures input.
        if self.gamepad_nav.chord_learn.is_some() {
            self.drive_gp_chord_capture(nav);
            return;
        }

        // Directional intent (dpad discrete + fresh stick deflection).
        let mut dir: Option<NavDir> = None;
        if nav.is_rising("dpad_up") { dir = Some(NavDir::Up); }
        else if nav.is_rising("dpad_down") { dir = Some(NavDir::Down); }
        else if nav.is_rising("dpad_left") { dir = Some(NavDir::Left); }
        else if nav.is_rising("dpad_right") { dir = Some(NavDir::Right); }
        if dir.is_none() {
            if let Some(sd) = gn::stick_dir(nav.lstick) {
                if self.gamepad_nav.repeat_dir != Some(sd) {
                    self.gamepad_nav.repeat_dir = Some(sd);
                    dir = Some(sd);
                }
            } else {
                self.gamepad_nav.repeat_dir = None;
            }
        }

        if !editing {
            // Row navigation.
            match dir {
                Some(NavDir::Up) => {
                    self.gamepad_nav.settings_index =
                        self.gamepad_nav.settings_index.saturating_sub(1);
                }
                Some(NavDir::Down) => {
                    self.gamepad_nav.settings_index =
                        (self.gamepad_nav.settings_index + 1).min(rows.len() - 1);
                }
                _ => {}
            }
            let idx = self.gamepad_nav.settings_index.min(rows.len() - 1);
            let row = &rows[idx];
            // South / RT → toggle bool, enter numeric edit, or start a chord
            // capture (which closes the panel so the user can press the combo).
            if nav.is_rising("btn_south") || rt_rising {
                match &row.kind {
                    GpSettingKind::Toggle { key } => {
                        let cur = self.gp_setting_value(*key);
                        self.gp_setting_set(*key, if cur != 0.0 { 0.0 } else { 1.0 });
                    }
                    GpSettingKind::ChordLearn { target } => {
                        // Start listening — panel STAYS open and shows the
                        // listening state on this row. Capture runs in
                        // `drive_gp_chord_capture` (early-returned above while
                        // learning). Arm-idle = false so the South that started
                        // this isn't swept into the combo.
                        self.gamepad_nav.chord_learn = Some(*target);
                        self.gamepad_nav.chord_draft.clear();
                        self.gamepad_nav.chord_arm_idle = false;
                    }
                    _ => { self.gamepad_nav.settings_editing = true; }
                }
            }
            // North → clear the assigned binding on a ChordLearn row.
            if nav.is_rising("btn_north") {
                if let GpSettingKind::ChordLearn { target } = &row.kind {
                    use crate::gamepad_nav::ChordTarget;
                    match target {
                        ChordTarget::SeeThrough => self.settings.seethrough_chord = None,
                        ChordTarget::Panic      => self.settings.panic_chord = None,
                    }
                    self.settings_dirty = true;
                }
            }
            // East / LT → close panel.
            if nav.is_rising("btn_east") || lt_rising {
                self.gamepad_nav.settings_open = false;
            }
        } else {
            let idx = self.gamepad_nav.settings_index.min(rows.len() - 1);
            let row = &rows[idx];
            let fine = nav.pressed.contains("btn_west");
            // Cycle gets its own handler — left/right step by one option,
            // wrapping. No fine step / stick deflection (it's a discrete
            // choice). Done early so the slider math below doesn't fire.
            if let GpSettingKind::Cycle { key, opts } = &row.kind {
                let cur = self.gp_setting_value(*key);
                let cur_idx = opts.iter().position(|(v, _)| (v - cur).abs() < 0.5)
                    .unwrap_or(0) as i32;
                let n = opts.len() as i32;
                let new_idx = match dir {
                    Some(NavDir::Right) | Some(NavDir::Up) => (cur_idx + 1).rem_euclid(n),
                    Some(NavDir::Left)  | Some(NavDir::Down) => (cur_idx - 1).rem_euclid(n),
                    _ => cur_idx,
                };
                if new_idx != cur_idx {
                    self.gp_setting_set(*key, opts[new_idx as usize].0);
                }
                return;
            }
            // Left/right (dpad or stick) nudges the value.
            let (lo, hi, step, key) = match &row.kind {
                GpSettingKind::IntSlider { lo, hi, step, key }
                | GpSettingKind::FloatSlider { lo, hi, step, key } => (*lo, *hi, *step, *key),
                GpSettingKind::Toggle { .. } | GpSettingKind::ChordLearn { .. } | GpSettingKind::Cycle { .. } => {
                    self.gamepad_nav.settings_editing = false; return;
                }
            };
            let mut delta = 0.0f32;
            let s = step * if fine { 0.25 } else { 1.0 };
            match dir {
                Some(NavDir::Right) | Some(NavDir::Up) => delta += s,
                Some(NavDir::Left) | Some(NavDir::Down) => delta -= s,
                _ => {}
            }
            let mag = nav.lstick.length();
            if mag > 0.5 {
                let span = hi - lo;
                delta += nav.lstick.x * span * if fine { 0.15 } else { 0.6 }
                    * 0.016; // ~per-frame scale
            }
            if delta != 0.0 {
                let cur = self.gp_setting_value(key);
                self.gp_setting_set(key, (cur + delta).clamp(lo, hi));
            }
            // South / East / RT / LT → leave edit (back to row nav).
            if nav.is_rising("btn_south") || nav.is_rising("btn_east")
                || rt_rising || lt_rising
            {
                self.gamepad_nav.settings_editing = false;
            }
        }
    }

    /// Render the gamepad-native settings panel (driven by `nav_drive_gp_settings`).
    /// A self-contained modal mirroring the gamepad-relevant subset of global
    /// settings, navigable purely by controller (the real Settings window can't
    /// be). Display-only here — all mutation happens in the driver.
    fn draw_gp_settings_panel(&mut self, ctx: &egui::Context) {
        if !self.gamepad_nav.settings_open { return; }
        let rows = self.gp_settings_rows();
        let sel = self.gamepad_nav.settings_index.min(rows.len().saturating_sub(1));
        let editing = self.gamepad_nav.settings_editing;
        let accent = ctx.style().visuals.selection.stroke.color;
        // Skin for combo glyphs: the active nav device's, else Xbox.
        let glyph_skin = self.gamepad_nav.active_dev.as_deref()
            .map(crate::canvas::remapper_icons::skin_from_device_id)
            .unwrap_or(crate::canvas::remapper_icons::Skin::Xbox);

        egui::Window::new("🎮 Settings")
            .id(egui::Id::new("gp_settings_panel"))
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
            .default_width(380.0)
            .show(ctx, |ui| {
                if self.gamepad_nav.chord_learn.is_some() {
                    ui.label(egui::RichText::new(
                        "Listening… hold a 2+ button combo and release to bind   \
                         (East alone: cancel)")
                        .small().color(egui::Color32::from_rgb(230, 185, 95)));
                } else {
                    ui.label(egui::RichText::new(
                        "D-pad/stick: move   South: edit/toggle   ←/→: adjust   \
                         West: fine   North: clear shortcut   East: close")
                        .small().color(egui::Color32::from_gray(150)));
                }
                ui.add_space(6.0);
                ui.separator();
                ui.add_space(4.0);
                for (i, row) in rows.iter().enumerate() {
                    let is_sel = i == sel;

                    // Is this row currently capturing a shortcut combo?
                    let learning_row = matches!(&row.kind, GpSettingKind::ChordLearn { target }
                        if self.gamepad_nav.chord_learn == Some(*target));

                    // For an idle ChordLearn row with a stored binding we draw
                    // the combo as glyph icons (trim + tooltip). Otherwise the
                    // value is a plain right-aligned string.
                    let mut combo_icons: Option<Vec<String>> = None;
                    let val_str = match &row.kind {
                        GpSettingKind::Toggle { key } =>
                            if self.gp_setting_value(*key) != 0.0 { "ON".to_string() } else { "OFF".to_string() },
                        GpSettingKind::IntSlider { key, .. } =>
                            format!("{}{}", self.gp_setting_value(*key).round() as i64, row.suffix),
                        GpSettingKind::FloatSlider { key, .. } =>
                            format!("{:.2}{}", self.gp_setting_value(*key), row.suffix),
                        GpSettingKind::Cycle { key, opts } => {
                            let v = self.gp_setting_value(*key);
                            opts.iter().find(|(val, _)| (val - v).abs() < 0.5)
                                .map(|(_, label)| (*label).to_string())
                                .unwrap_or_else(|| format!("?{:.0}", v))
                        }
                        GpSettingKind::ChordLearn { target } => {
                            use crate::gamepad_nav::ChordTarget;
                            if self.gamepad_nav.chord_learn == Some(*target) {
                                // Learning: live listening state. Show the combo
                                // captured so far as icons too.
                                if self.gamepad_nav.chord_draft.is_empty() {
                                    "◉ Listening…".to_string()
                                } else {
                                    combo_icons = Some(self.gamepad_nav.chord_draft.clone());
                                    String::new()
                                }
                            } else {
                                let assigned = match target {
                                    ChordTarget::SeeThrough => self.settings.seethrough_chord.as_ref(),
                                    ChordTarget::Panic => self.settings.panic_chord.as_ref(),
                                };
                                match assigned {
                                    Some(c) if !c.is_empty() => { combo_icons = Some(c.clone()); String::new() }
                                    _ => "(none)".to_string(),
                                }
                            }
                        }
                    };

                    let (rect, resp) = ui.allocate_exact_size(
                        egui::vec2(ui.available_width(), 24.0), egui::Sense::hover());
                    let painter = ui.painter();
                    if learning_row {
                        // Warm "listening" highlight, distinct from the cool
                        // selection accent, so it's obvious the panel is capturing.
                        let warm = egui::Color32::from_rgb(220, 170, 80);
                        let [r, g, b, _] = warm.to_array();
                        painter.rect_filled(rect, 5.0,
                            egui::Color32::from_rgba_unmultiplied(r, g, b, 60));
                        painter.rect_stroke(rect, 5.0,
                            egui::Stroke::new(2.0, warm), egui::StrokeKind::Inside);
                    } else if is_sel {
                        let bright = editing && !matches!(row.kind, GpSettingKind::Toggle { .. });
                        let [r, g, b, _] = accent.to_array();
                        painter.rect_filled(rect, 5.0,
                            egui::Color32::from_rgba_unmultiplied(r, g, b,
                                if bright { 70 } else { 40 }));
                        painter.rect_stroke(rect, 5.0,
                            egui::Stroke::new(if bright { 2.0 } else { 1.0 }, accent),
                            egui::StrokeKind::Inside);
                    }
                    // Row label (left). Measure its width so combo glyphs know
                    // how far left they may extend before crowding it.
                    let label_galley = painter.layout_no_wrap(
                        row.label.clone(), egui::FontId::proportional(13.0),
                        ui.visuals().text_color());
                    let label_right = rect.left() + 10.0 + label_galley.size().x;
                    painter.galley(
                        egui::pos2(rect.left() + 10.0, rect.center().y - label_galley.size().y * 0.5),
                        label_galley, ui.visuals().text_color());

                    if let Some(pins) = combo_icons {
                        // Draw the combo as glyph icons right-to-left, with "+"
                        // separators. If they would crowd the label, trim the
                        // overflow (leftmost icons) and prefix a "…"; the full
                        // combo is always available in a hover tooltip.
                        const G: f32 = 18.0;       // glyph size
                        const SEP: f32 = 9.0;      // width budget for a "+"
                        const PAD: f32 = 16.0;     // min gap from the label text
                        let min_x = label_right + PAD;
                        let mut x = rect.right() - 10.0;
                        let cy = rect.center().y;
                        let icon_col = if learning_row { egui::Color32::from_rgb(230, 185, 95) }
                            else if is_sel { accent } else { egui::Color32::from_gray(210) };
                        let mut trimmed = false;
                        // Walk pins from last to first, placing each icon to the
                        // left of the previous, stopping when we run out of room.
                        for (j, pin) in pins.iter().enumerate().rev() {
                            // Reserve room for a leading "…" if there are still
                            // earlier pins we might not fit.
                            let need_ellipsis = j > 0;
                            let reserve = if need_ellipsis { SEP } else { 0.0 };
                            if x - G < min_x + reserve {
                                trimmed = true;
                                break;
                            }
                            let icon_rect = egui::Rect::from_min_size(
                                egui::pos2(x - G, cy - G * 0.5), egui::vec2(G, G));
                            if let Some(tex) = self.gp_legend_glyph(ctx, glyph_skin, pin) {
                                painter.image(tex.id(), icon_rect,
                                    egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                                    egui::Color32::WHITE);
                            } else {
                                painter.text(icon_rect.center(), egui::Align2::CENTER_CENTER,
                                    gp_pin_token(pin), egui::FontId::proportional(11.0), icon_col);
                            }
                            x -= G;
                            if j > 0 {
                                x -= SEP;
                                painter.text(egui::pos2(x + SEP * 0.5, cy),
                                    egui::Align2::CENTER_CENTER, "+",
                                    egui::FontId::proportional(12.0), icon_col);
                            }
                        }
                        if trimmed {
                            painter.text(egui::pos2(x, cy), egui::Align2::RIGHT_CENTER, "…",
                                egui::FontId::proportional(13.0), icon_col);
                        }
                        // Full combo tooltip (always, since even untrimmed icons
                        // can be ambiguous).
                        resp.on_hover_text(pretty_chord_combo(&pins));
                    } else {
                        painter.text(
                            egui::pos2(rect.right() - 10.0, rect.center().y),
                            egui::Align2::RIGHT_CENTER, &val_str,
                            egui::FontId::proportional(13.0),
                            if learning_row { egui::Color32::from_rgb(230, 185, 95) }
                            else if is_sel { accent }
                            else { ui.visuals().weak_text_color() });
                    }
                    ui.add_space(2.0);
                }
            });
        ctx.request_repaint();
    }

    /// Render the virtual KB/M picker modal: a keyboard-ish grid of KBM icons
    /// with the focused cell highlighted, the current output chord shown above,
    /// and control hints. Input is handled in `drive_kbm_picker`; this is
    /// display-only. Rendered top-level (not in a sublayer), so painting here is
    /// safe.
    fn draw_kbm_picker(&mut self, ctx: &egui::Context) {
        if !self.gamepad_nav.kbm_picker_open { return; }
        use crate::kbm_picker::{clamp_index, layout_extent, KBM_LAYOUT};
        let sel = clamp_index(self.gamepad_nav.kbm_picker_idx);
        let accent = ctx.style().visuals.selection.stroke.color;

        // Current output chord for the header preview (read from whichever draft
        // param this picker session targets).
        let dk = self.gamepad_nav.kbm_picker_draft_key.clone();
        let chord: Vec<String> = match (self.gamepad_nav.kbm_picker_outer,
                                        self.gamepad_nav.kbm_picker_node) {
            (Some(o), Some(i)) => self.nav_remap_draft_vec(o, i, &dk),
            _ => Vec::new(),
        };

        const UNIT: f32 = 30.0; // px per grid unit
        const GAP: f32 = 3.0;   // gap between adjacent keys
        let (ext_x, ext_y) = layout_extent();
        let board_w = ext_x * (UNIT + GAP);
        let board_h = ext_y * (UNIT + GAP);
        let skin = crate::canvas::remapper_icons::Skin::Kbm;

        egui::Window::new("⌨ KB/M picker")
            .id(egui::Id::new("gp_kbm_picker"))
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
            .show(ctx, |ui| {
                ui.label(egui::RichText::new(
                    "LS/D-pad: move   South: add   North: clear   East: done")
                    .small().color(egui::Color32::from_gray(150)));
                ui.add_space(4.0);
                // Output chord preview.
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new("Output:").small().weak());
                    if chord.is_empty() {
                        ui.label(egui::RichText::new("(none)").small().italics().weak());
                    } else {
                        for (i, pin) in chord.iter().enumerate() {
                            if i > 0 { ui.label(egui::RichText::new("+").strong()); }
                            ui.label(egui::RichText::new(kbm_pin_label(pin)).strong());
                        }
                    }
                });
                ui.add_space(6.0);
                ui.separator();
                ui.add_space(6.0);

                // Absolute-positioned keyboard: allocate one canvas sized to the
                // layout extent, then place each cell at its (x,y)*unit origin so
                // the nav cluster + arrows + mouse sit in their own clusters to
                // the right of the main block.
                let (canvas, _) = ui.allocate_exact_size(
                    egui::vec2(board_w, board_h), egui::Sense::hover());
                let painter = ui.painter_at(canvas);
                for (i, cell) in KBM_LAYOUT.iter().enumerate() {
                    let min = canvas.min + egui::vec2(
                        cell.x * (UNIT + GAP), cell.y * (UNIT + GAP));
                    let size = egui::vec2(
                        cell.width * UNIT + (cell.width - 1.0) * GAP, UNIT);
                    let rect = egui::Rect::from_min_size(min, size);
                    let focused = i == sel;
                    // Cell background + focus highlight.
                    let bg = if focused {
                        let [rr, gg, bb, _] = accent.to_array();
                        egui::Color32::from_rgba_unmultiplied(rr, gg, bb, 60)
                    } else {
                        egui::Color32::from_gray(40)
                    };
                    painter.rect_filled(rect, 4.0, bg);
                    if focused {
                        painter.rect_stroke(rect, 4.0,
                            egui::Stroke::new(2.0, accent), egui::StrokeKind::Outside);
                    }
                    // Icon (or text fallback), centered.
                    if let Some(tex) = kbm_cell_texture(ctx, skin, cell.pin) {
                        let s = (UNIT - 6.0).min(size.x - 6.0).max(8.0);
                        let img_rect = egui::Rect::from_center_size(
                            rect.center(), egui::vec2(s, s));
                        painter.image(tex.id(), img_rect,
                            egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                            egui::Color32::WHITE);
                    } else {
                        painter.text(rect.center(), egui::Align2::CENTER_CENTER,
                            kbm_pin_label(cell.pin),
                            egui::FontId::proportional(11.0),
                            ui.visuals().text_color());
                    }
                }
            });
        ctx.request_repaint();
    }

    /// Modal press-mode picker: a vertical list of the press modes (glyph +
    /// label + short description) with the current/highlighted one accented.
    /// Opened from a mapping card's press-mode field; input handled in
    /// `drive_press_mode_picker`.
    fn draw_press_mode_picker(&mut self, ctx: &egui::Context) {
        if !self.gamepad_nav.press_mode_open { return; }
        let sel = self.gamepad_nav.press_mode_idx.min(Self::PRESS_MODES.len() - 1);
        // Current mode on the target card (to mark the active row).
        let cur_mode = self.gamepad_nav.press_mode_outer.map(|o|
            self.nav_remap_card_mode(o, self.gamepad_nav.press_mode_card)
                .unwrap_or_else(|| "down".to_string()))
            .unwrap_or_else(|| "down".to_string());
        let accent = ctx.style().visuals.selection.stroke.color;

        egui::Window::new("Press mode")
            .id(egui::Id::new("gp_press_mode_picker"))
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
            .show(ctx, |ui| {
                ui.label(egui::RichText::new(
                    "LS/D-pad: move   South: apply   East: cancel")
                    .small().color(egui::Color32::from_gray(150)));
                ui.add_space(6.0);
                for (i, mode) in Self::PRESS_MODES.iter().enumerate() {
                    let glyph = crate::canvas::viewer::remapper_press_mode_glyph(mode);
                    let label = crate::canvas::viewer::remapper_press_mode_label(mode);
                    let focused = i == sel;
                    let is_cur = *mode == cur_mode;
                    let (rect, _) = ui.allocate_exact_size(
                        egui::vec2(220.0, 26.0), egui::Sense::hover());
                    let painter = ui.painter();
                    if focused {
                        let [r, g, b, _] = accent.to_array();
                        painter.rect_filled(rect, 4.0,
                            egui::Color32::from_rgba_unmultiplied(r, g, b, 55));
                        painter.rect_stroke(rect, 4.0,
                            egui::Stroke::new(1.5, accent), egui::StrokeKind::Inside);
                    }
                    painter.text(rect.left_center() + egui::vec2(10.0, 0.0),
                        egui::Align2::LEFT_CENTER, glyph,
                        egui::FontId::proportional(16.0), ui.visuals().text_color());
                    painter.text(rect.left_center() + egui::vec2(34.0, 0.0),
                        egui::Align2::LEFT_CENTER, label,
                        egui::FontId::proportional(13.0), ui.visuals().text_color());
                    if is_cur {
                        painter.text(rect.right_center() - egui::vec2(8.0, 0.0),
                            egui::Align2::RIGHT_CENTER, "●",
                            egui::FontId::proportional(10.0), accent);
                    }
                }
            });
        ctx.request_repaint();
    }

    /// Context-sensitive button legend for the current gamepad-nav state.
    /// Returns ordered `(glyphs, label)` hints, where `glyphs` is one or more
    /// gamepad pin ids drawn side-by-side before the label (e.g. LS + D-pad for
    /// "Navigate", LB + RB for "Tab"). Directional helpers (`hint_move`,
    /// `hint_horiz`, `hint_vert`) bundle the stick + matching D-pad glyphs.
    fn gp_legend_hints(&self) -> Vec<(Vec<&'static str>, &'static str)> {
        use crate::gamepad_nav::EditLevel;

        // Stick + D-pad bundles so both navigation methods are advertised.
        // `_move` uses the all-direction glyphs; `_horiz`/`_vert` use the
        // axis-specific (both-arrows) glyphs.
        let hint_move  = || vec!["left_stick", "dpad"];                       // any direction
        let hint_horiz = || vec!["left_stick_horizontal", "dpad_horizontal"]; // left/right axis
        let hint_vert  = || vec!["left_stick_vertical", "dpad_vertical"];     // up/down axis

        // Modal contexts take priority over the sub-patch edit level.
        if self.gamepad_nav.kbm_picker_open {
            return vec![
                (hint_move(), "Move"),
                (vec!["btn_south"], "Add key"),
                (vec!["btn_north"], "Clear chord"),
                (vec!["btn_east"], "Done"),
            ];
        }
        if self.gamepad_nav.settings_open {
            // While a shortcut row is learning, the panel is listening for a
            // combo — show the capture hints (release to bind, East to abort).
            if self.gamepad_nav.chord_learn.is_some() {
                return vec![
                    (vec![], "Hold a 2+ button combo, release to bind"),
                    (vec!["btn_east"], "Press alone: cancel"),
                ];
            }
            return vec![
                (hint_vert(), "Move"),
                (vec!["btn_south"], if self.gamepad_nav.settings_editing { "Apply" } else { "Edit" }),
                (hint_horiz(), "Adjust"),
                (vec!["btn_west"], "Fine"),
                (vec!["btn_north"], "Clear shortcut"),
                (vec!["btn_east"], "Close"),
            ];
        }
        if self.gamepad_nav.alt_tab_active {
            return vec![
                (vec!["right_stick"], "Switch window"),
                (vec!["btn_back"], "Release to commit"),
            ];
        }
        if self.gamepad_nav.preset_nav_open {
            return vec![
                (hint_move(), "Move"),
                (vec!["btn_south"], "Apply preset"),
                (vec!["btn_start"], "Close"),
            ];
        }
        if self.gamepad_nav.press_mode_open {
            return vec![
                (hint_vert(), "Move"),
                (vec!["btn_south"], "Apply"),
                (vec!["btn_east"], "Cancel"),
            ];
        }
        if self.gamepad_nav.left_edit.is_some() {
            return vec![
                (hint_horiz(), "Adjust"),
                (vec!["btn_west"], "Fine"),
                (vec!["btn_north"], "Reset"),
                (vec!["btn_east"], "Done"),
            ];
        }

        match self.gamepad_nav.edit_level {
            EditLevel::Widget => vec![
                (hint_move(), "Navigate"),
                (vec!["right_stick"], "Cursor"),
                (vec!["btn_south", "right_trigger"], "Select / Edit"),
                (vec!["btn_lb", "btn_rb"], "Tab"),
                (vec!["btn_start"], "Presets"),
                (vec!["btn_start"], "Hold: Settings"),
                (vec!["btn_back"], "Alt-Tab"),
                (vec!["btn_ls"], "Undo"),
                (vec!["btn_rs"], "Redo"),
            ],
            EditLevel::Editing => {
                // Row-type (multi-field) widgets split the axes: horizontal =
                // select field, vertical = adjust value. Single-value widgets
                // (knob / constant) adjust on any direction.
                let multi = self.nav_active_outer_id()
                    .map(|o| matches!(self.nav_selected_kind(o),
                        NavWidgetKind::MultiField))
                    .unwrap_or(false);
                if multi {
                    vec![
                        (hint_horiz(), "Select field"),
                        (hint_vert(), "Adjust"),
                        (vec!["btn_south"], "Confirm"),
                        (vec!["btn_west"], "Fine"),
                        (vec!["btn_north"], "Reset"),
                        (vec!["btn_east"], "Back"),
                    ]
                } else {
                    vec![
                        (hint_move(), "Adjust"),
                        (vec!["btn_south"], "Confirm"),
                        (vec!["btn_west"], "Fine"),
                        (vec!["btn_north"], "Reset"),
                        (vec!["btn_east"], "Back"),
                    ]
                }
            }
            EditLevel::CurveDots => vec![
                (hint_move(), "Pick dot"),
                (vec!["btn_south"], "Edit dot"),
                (vec!["right_trigger"], "Add dot"),
                (vec!["left_trigger"], "Delete dot"),
                (vec!["btn_east"], "Back"),
            ],
            EditLevel::CurveDot => vec![
                (hint_move(), "Move dot"),
                (vec!["btn_west"], "Fine"),
                (vec!["btn_east"], "Back"),
            ],
            EditLevel::RemapScroll => vec![
                (hint_move(), "Navigate"),
                (vec!["btn_south"], "Select / Enter"),
                (vec!["btn_north"], "Reset card"),
                (vec!["btn_west"], "Delete card"),
                (vec!["left_trigger", "right_trigger"], "Filter"),
                (vec!["btn_east"], "Back"),
            ],
            EditLevel::RemapCard => vec![
                (hint_horiz(), "Field"),
                (hint_vert(), "Adjust"),
                (vec!["btn_south"], "Toggle / Open"),
                (vec!["btn_north"], "Reset card"),
                (vec!["btn_east"], "Back"),
            ],
        }
    }

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
        use std::collections::HashMap;
        let mut to_skip: HashMap<flexinput_devices::ControllerKind, usize> = HashMap::new();
        {
            let pool = self.shared_virtual_devices.lock().unwrap();
            for d in pool.iter() {
                if let Some(k) = own_virtual_kind(d.id()) { *to_skip.entry(k).or_insert(0) += 1; }
            }
        }
        let mut owned = std::collections::HashSet::new();
        for (k, n) in to_skip.iter() {
            let mut remaining = *n;
            for i in (0..self.devices.len()).rev() {
                if remaining == 0 { break; }
                if self.devices[i].kind == *k && !owned.contains(&self.devices[i].id) {
                    owned.insert(self.devices[i].id.clone());
                    remaining -= 1;
                }
            }
        }
        owned
    }

    /// Resolve the active Easy sub-patch outer node id (the `subpatch` node in
    /// the active tab), if any. Used by the legend to inspect the selected
    /// widget kind.
    fn nav_active_outer_id(&self) -> Option<egui_snarl::NodeId> {
        self.tabs.get(self.active_tab)?.canvas.snarl
            .nodes_ids_data()
            .find(|(_, n)| n.value.module_id == "subpatch")
            .map(|(id, _)| id)
    }

    /// One gamepad-shortcut chord row: a label, a Learn button that captures a
    /// button combo (sets `gamepad_nav.chord_learn`), and a clear (✕). Shared by
    /// the desktop Settings window and the gamepad-native settings panel.
    /// Returns true if the setting changed (so the caller marks dirty).
    fn gamepad_shortcut_row(&mut self, ui: &mut egui::Ui, label: &str,
        target: crate::gamepad_nav::ChordTarget) -> bool
    {
        use crate::gamepad_nav::ChordTarget;
        let mut changed = false;
        let learning = self.gamepad_nav.chord_learn == Some(target);
        // Snapshot the assigned combo + presence as owned values so no borrow of
        // self.settings is held across the closure (which mutates self).
        let (assigned_label, has_assigned) = {
            let assigned: Option<&Vec<String>> = match target {
                ChordTarget::SeeThrough => self.settings.seethrough_chord.as_ref(),
                ChordTarget::Panic      => self.settings.panic_chord.as_ref(),
            };
            (assigned.map(|c| pretty_chord_combo(c)), assigned.is_some())
        };
        let face = if learning {
            "Hold 2+ buttons, release…".to_string()
        } else {
            assigned_label.unwrap_or_else(|| "(none)".to_string())
        };
        ui.horizontal(|ui| {
            ui.label(format!("{label}:"));
            let mut btn = egui::Button::new(egui::RichText::new(face).size(12.0));
            if learning {
                btn = btn.fill(egui::Color32::from_rgb(80, 60, 30))
                    .stroke(egui::Stroke::new(1.0, egui::Color32::from_rgb(200, 160, 80)));
            }
            if ui.add(btn).on_hover_text(
                "Click, then hold a 2+ button gamepad combo and release it to bind.\n\
                 Captured while held; latches when you let go. \
                 (Press East alone to cancel.)").clicked()
            {
                // Toggle learn for this target; clear any in-flight draft and
                // require a full release before accumulating.
                self.gamepad_nav.chord_learn = if learning { None } else { Some(target) };
                self.gamepad_nav.chord_draft.clear();
                self.gamepad_nav.chord_arm_idle = false;
            }
            if has_assigned
                && ui.small_button("✕").on_hover_text("Clear shortcut").clicked()
            {
                match target {
                    ChordTarget::SeeThrough => self.settings.seethrough_chord = None,
                    ChordTarget::Panic      => self.settings.panic_chord = None,
                }
                if learning { self.gamepad_nav.chord_learn = None; }
                changed = true;
            }
        });
        changed
    }

    /// Bottom legend bar listing the active gamepad's button actions for the
    /// current nav context. Visible only while a nav-enabled gamepad drives the
    /// UI (`active_dev` set this frame by `run_gamepad_nav`).
    fn draw_gp_legend_bar(&self, ctx: &egui::Context) {
        let Some(dev) = self.gamepad_nav.active_dev.clone() else { return; };
        let skin = crate::canvas::remapper_icons::skin_from_device_id(&dev);
        let hints = self.gp_legend_hints();
        if hints.is_empty() { return; }

        egui::TopBottomPanel::bottom("gp_legend_bar")
            .resizable(false)
            .show_separator_line(true)
            .frame(egui::Frame::default()
                .fill(ctx.style().visuals.panel_fill)
                .inner_margin(egui::Margin::symmetric(10, 5)))
            .show(ctx, |ui| {
                // Wrap so a long hint set folds onto a second line instead of
                // overflowing the window width on narrow displays.
                ui.horizontal_wrapped(|ui| {
                    ui.spacing_mut().item_spacing.x = 4.0;
                    const GLYPH: f32 = 18.0;
                    for (i, (pins, label)) in hints.iter().enumerate() {
                        if i > 0 {
                            ui.add_space(3.0);
                            ui.separator();
                            ui.add_space(3.0);
                        }
                        // One or more glyphs (e.g. LS + D-pad, LB + RB) shown
                        // side-by-side before the shared label.
                        for (j, pin) in pins.iter().enumerate() {
                            if j > 0 {
                                ui.label(egui::RichText::new("/").size(11.0).weak());
                            }
                            if let Some(tex) = self.gp_legend_glyph(ctx, skin, pin) {
                                ui.add(egui::Image::new((tex.id(), egui::vec2(GLYPH, GLYPH))));
                            } else {
                                // No glyph under this skin — short textual token.
                                ui.label(egui::RichText::new(gp_pin_token(pin)).strong().size(12.0));
                            }
                        }
                        ui.add_space(2.0);
                        ui.label(egui::RichText::new(*label).size(12.0));
                    }
                });
            });
    }

    /// Cached glyph texture for a gamepad button pin under a skin (white-bg-free
    /// SVG rendered with native colors). Cached per (skin, pin) on ctx temp data.
    fn gp_legend_glyph(&self, ctx: &egui::Context,
        skin: crate::canvas::remapper_icons::Skin, pin: &str)
        -> Option<egui::TextureHandle>
    {
        let key = egui::Id::new(("gp_legend_glyph", skin.as_str(), pin));
        if let Some(t) = ctx.data(|d| d.get_temp::<egui::TextureHandle>(key)) {
            return Some(t);
        }
        let bytes = crate::canvas::remapper_icons::pin_svg(skin, pin)?;
        let svg = std::str::from_utf8(bytes).ok()?;
        // Native colors (transparent tint → recolor pass skipped).
        let img = crate::canvas::viewer::rasterize_svg_recolored(
            svg, 36, 36, "override", egui::Color32::TRANSPARENT)?;
        let t = ctx.load_texture(format!("gp_legend_{}_{}", skin.as_str(), pin),
            img, egui::TextureOptions::LINEAR);
        ctx.data_mut(|d| d.insert_temp(key, t.clone()));
        Some(t)
    }

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

                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    ui.label("Background repaint rate")
                        .on_hover_text("Repaint rate applied when the window is minimized or another app has focus. The focused window always paints at vsync regardless.");
                    let resp = ui.add(egui::Slider::new(
                        &mut self.settings.bg_repaint_hz,
                        settings::BG_REPAINT_HZ_MIN..=settings::BG_REPAINT_HZ_MAX,
                    ).suffix(" Hz"));
                    if resp.changed() {
                        dirty = true;
                    }
                });
                ui.label(egui::RichText::new(
                    "How often the UI repaints while FlexInput sits in the background. Lower = less CPU while you play a game, higher = smoother glanceable visuals. Has no effect when the window is focused."
                ).small().color(egui::Color32::from_gray(140)));

                ui.add_space(10.0);
                ui.separator();
                ui.add_space(6.0);

                // ── Gamepad UI navigation ──────────────────────────────
                ui.label(egui::RichText::new("Gamepad UI navigation").strong());
                ui.add_space(4.0);
                if ui.checkbox(&mut self.settings.gamepad_ui_nav_default,
                    "Enable gamepad UI navigation by default")
                    .on_hover_text(
                        "When on, newly-seen gamepads start in UI-navigation mode: while \
                         FlexInput holds focus the controller drives FlexInput's own UI and \
                         its mapped output is suppressed. Alt-tab away and mappings resume \
                         automatically. Toggle per-device on each gamepad card."
                    )
                    .changed()
                {
                    dirty = true;
                }

                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    ui.label("Cursor max speed")
                        .on_hover_text("Top speed of the right-stick/gyro nav cursor at full deflection (px/s). The actual speed follows the acceleration curve below up to this cap.");
                    let resp = ui.add(egui::Slider::new(
                        &mut self.settings.cursor_max_speed, 1000.0..=30000.0)
                        .suffix(" px/s"));
                    if resp.changed() { dirty = true; }
                });
                ui.horizontal(|ui| {
                    ui.label("Cursor acceleration")
                        .on_hover_text("How the cursor speed ramps with stick deflection. 1.0 = linear; higher = slower start and a faster top end (deflection raised to this power).");
                    let mut a = self.settings.cursor_accel;
                    let resp = ui.add(egui::Slider::new(&mut a, 1.0..=4.0)
                        .fixed_decimals(2));
                    if resp.double_clicked() { a = 2.0; }
                    if (a - self.settings.cursor_accel).abs() > f32::EPSILON {
                        self.settings.cursor_accel = a.clamp(1.0, 4.0);
                        dirty = true;
                    }
                });

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

                // ── Gamepad shortcuts ───────────────────────────────────
                // Assign a gamepad button combo to toggle see-through / panic.
                // Learned by clicking Learn then pressing+releasing a combo.
                ui.label(egui::RichText::new("Gamepad shortcuts").strong());
                ui.add_space(4.0);
                if self.gamepad_shortcut_row(ui, "See-through", crate::gamepad_nav::ChordTarget::SeeThrough) {
                    dirty = true;
                }
                if self.gamepad_shortcut_row(ui, "Panic mode", crate::gamepad_nav::ChordTarget::Panic) {
                    dirty = true;
                }
                ui.add_space(2.0);
                if ui.checkbox(&mut self.settings.gamepad_chords_nav_only,
                    "Only when in gamepad navigation mode")
                    .on_hover_text(
                        "On: these combos fire only while the driving gamepad is in UI-navigation mode \
                         (so the same buttons stay free for in-game mappings otherwise).\n\
                         Off: they fire from any connected gamepad whenever FlexInput is focused.")
                    .changed()
                {
                    dirty = true;
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
                if ui.checkbox(
                    &mut self.settings.persist_virtual_devices,
                    "Keep virtual controllers alive after FlexInput closes",
                ).changed() {
                    dirty = true;
                    #[cfg(windows)]
                    flexinput_hidmaestro::helper::set_persist(self.settings.persist_virtual_devices);
                }
                ui.label(egui::RichText::new(
                    "Off by default: virtual pads are removed when the app closes or crashes. \
                     Turn on so a running game keeps its gamepad across an app restart or update \
                     \u{2014} FlexInput reclaims the existing device on next launch. (HIDMaestro only.)",
                ).small().color(egui::Color32::from_gray(140)));

                ui.add_space(6.0);
                if ui.button("Reinstall HIDMaestro drivers").clicked() {
                    self.reinstall_confirm_open = true;
                }
                ui.label(egui::RichText::new(
                    "Removes and reinstalls the driver, then re-deploys the virtual controllers \
                     on your canvas. Prompts for admin once. Use this if virtual DS4/DualSense \
                     stop working after a Windows or app update.",
                ).small().color(egui::Color32::from_gray(140)));
                if let Some(err) = &self.last_device_op_error {
                    ui.label(egui::RichText::new(format!("Last device error: {err}"))
                        .small().color(egui::Color32::from_rgb(220, 120, 120)));
                }

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

                // ── Easy mode ───────────────────────────────────────────
                ui.label(egui::RichText::new("Easy mode").strong());
                ui.add_space(4.0);
                ui.label(egui::RichText::new(
                    "Folder scanned for user-authored .fxsp sub-patch presets, in addition to the factory presets shipped under app/assets/sub-patches/."
                ).small().color(egui::Color32::from_gray(140)));
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    ui.label("User presets folder:");
                    let label = self.settings.user_presets_folder.as_ref()
                        .map(|p| p.display().to_string())
                        .unwrap_or_else(|| "(none)".into());
                    ui.monospace(label);
                    if ui.button("Browse…").clicked() {
                        if let Some(p) = rfd::FileDialog::new().pick_folder() {
                            self.settings.user_presets_folder = Some(p);
                            dirty = true;
                        }
                    }
                    if self.settings.user_presets_folder.is_some()
                        && ui.button("Clear").clicked()
                    {
                        self.settings.user_presets_folder = None;
                        dirty = true;
                    }
                });

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

                // ── Profiler (dev tool, debug builds only) ──────────────
                // Toggle flips `puffin::set_scopes_on()` and starts/stops
                // a `puffin_http` server on 127.0.0.1:8585 so the
                // standalone `puffin_viewer` GUI can connect for a live
                // flamegraph. Not persisted — resets to off on every
                // launch. Hidden from release builds entirely so the
                // shipped UI doesn't expose an internal dev tool.
                #[cfg(debug_assertions)]
                {
                    ui.add_space(10.0);
                    ui.separator();
                    ui.add_space(6.0);

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
                }

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
    /// Snapshot the full tab/canvas state into a `PersistedWorkspace`. Shared
    /// by the opt-in workspace save and the always-on crash-recovery save so
    /// both serialize identical state.
    fn build_persisted_workspace(&self) -> PersistedWorkspace {
        let tabs: Vec<PersistedTab> = self.tabs.iter().map(|t| PersistedTab {
            title: t.title.clone(),
            file_path: t.file_path.clone(),
            bound_exes: t.bound_exes.clone(),
            auto_bypass: t.auto_bypass,
            snarl: t.canvas.snarl.clone(),
            easy_preset_path: t.easy_state.loaded_preset.as_ref().map(|(p, _)| p.clone()),
        }).collect();
        PersistedWorkspace {
            version: 1,
            active_tab: self.active_tab,
            tabs,
        }
    }

    fn save_workspace_now(&self) {
        if !self.settings.keep_workspace { return; }
        settings::save_workspace(&self.build_persisted_workspace());
    }

    /// Sum `mutation_gen` over all tabs. Used as the cheap dirty signal for the
    /// crash-recovery autosave — it advances on every snarl mutation (any
    /// push_undo / push_snapshot / undo / redo), so a change between frames
    /// means persistent state changed.
    fn total_mutation_gen(&self) -> u64 {
        self.tabs.iter().map(|t| t.canvas.mutation_gen).fold(0u64, u64::wrapping_add)
    }

    /// Write the crash-recovery snapshot if (and only if) a settled edit
    /// happened since the last write. Called once per frame from `update`.
    /// Independent of `keep_workspace`: even a user who never opted into tab
    /// persistence must not lose work to a GPU-loss relaunch. The write is
    /// atomic (temp + rename) so the panic-hook / relaunch path can never read
    /// a half-written file.
    fn maybe_write_recovery_snapshot(&mut self) {
        let gen = self.total_mutation_gen();
        if gen == self.last_recovery_mutation_gen {
            return;
        }
        self.last_recovery_mutation_gen = gen;
        settings::save_recovery(&self.build_persisted_workspace());
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
    // Gamepad-UI-nav suppression — treated identically to `io_bypass`.
    ui_nav_suppress: Arc<AtomicBool>,
    shared_devices: Arc<RwLock<Vec<PhysicalDevice>>>,
    shared_midi_devices: Arc<RwLock<Vec<PhysicalDevice>>>,
    polling_hz: Arc<AtomicU32>,
    device_rates: flexinput_engine::DeviceRates,
    scope_taps: flexinput_engine::ScopeTaps,
    spike_filter_settings: Arc<RwLock<HashMap<String, (bool, f32)>>>,
    ping_requests: Arc<Mutex<Vec<String>>>,
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

                // Input must win over UI rendering. This thread polls physical
                // inputs and flushes the virtual-device outputs — the hard
                // real-time leg of the input→output path. Pin it above the UI
                // and render threads so a busy frame can never delay an input
                // flush. TIME_CRITICAL (not just ABOVE_NORMAL) because the loop
                // is a tight bounded poll-and-sleep: it yields the CPU every
                // iteration via `thread::sleep`, so it can't starve other
                // threads, but while runnable it should preempt them.
                use windows_sys::Win32::System::Threading::{
                    GetCurrentThread, SetThreadPriority, THREAD_PRIORITY_TIME_CRITICAL,
                };
                let ok = SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_TIME_CRITICAL);
                eprintln!("[device-io] SetThreadPriority(TIME_CRITICAL) -> {} (nonzero == ok)", ok);
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

            // Active rumble-ping pulses: device_id → instant the pulse should stop.
            // Started when the UI pushes a ping request; cleared once expired (sending
            // a single rumble-off so the motors don't latch).
            let mut ping_until: HashMap<String, Instant> = HashMap::new();
            const PING_RUMBLE_MS: u64 = 200;

            loop {
                puffin::GlobalProfiler::lock().new_frame();
                puffin::profile_scope!("io_thread_iter");
                let t0 = Instant::now();
                // Re-read polling rate each iteration so live retunes apply.
                let hz = polling_hz.load(Ordering::Relaxed).clamp(60, 4000);
                let interval = Duration::from_nanos(1_000_000_000 / hz as u64);

                // Push per-device snap-back spike-filter settings to backends.
                // Cheap: each backend's `set_spike_filter` early-returns when
                // the value is unchanged. We do this BEFORE polling so the
                // filter applies to this iteration's samples.
                {
                    puffin::profile_scope!("push_spike_filter");
                    let settings = spike_filter_settings.read().unwrap();
                    for (dev_id, (on, sens)) in settings.iter() {
                        for backend in &mut backends {
                            backend.set_spike_filter(dev_id, *on, *sens);
                        }
                    }
                }

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
                //
                // Skip the write-lock acquisition entirely when no taped pin
                // names appear in this iteration's signals — the common case
                // when no gyro-capable device is connected. Saves a contended
                // RwLock write per loop iteration (500 Hz) on idle setups.
                let has_taped_pin = signals.keys()
                    .any(|(_, pin)| flexinput_engine::SCOPE_TAP_PINS.iter().any(|p| *p == pin.as_str()));
                if has_taped_pin {
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
                let bypass = io_bypass.load(Ordering::Relaxed)
                    || ui_nav_suppress.load(Ordering::Relaxed);
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
                    }

                    // Poll rumble/feedback signals back from active-tab devices —
                    // ALWAYS, even under bypass. Bypass suppresses *outgoing*
                    // mapped input to the virtual pad; it must NOT stop us
                    // draining *incoming* rumble/FFB the game sends back. The
                    // HIDMaestro backend decodes rumble *inside* poll_outputs (it
                    // drains the SHM output ring), so gating poll_outputs on
                    // !bypass meant a game's rumble never reached the physical pad
                    // whenever FlexInput was unfocused (bypass true). The ViGEm
                    // backends update rumble on a separate notification thread, so
                    // they were unaffected — which is why XInput rumble worked but
                    // HIDMaestro's didn't. Background-tab devices are still
                    // skipped so their feedback can't route into the wrong graph.
                    let mut virt_sigs: Vec<((String, String), Signal)> = Vec::new();
                    for dev in devs.iter_mut() {
                        let id = dev.id().to_string();
                        if !active_ids.contains(&id) { continue; }
                        for (pin_id, sig) in dev.poll_outputs() {
                            virt_sigs.push(((id.clone(), pin_id.to_string()), sig));
                        }
                    }
                    if !virt_sigs.is_empty() {
                        // ArcSwap is publish-only — load, clone, merge, store.
                        let cur = proc_device_signals.load_full();
                        let mut merged: HashMap<(String, String), Signal> = (*cur).clone();
                        for (k, v) in virt_sigs { merged.insert(k, v); }
                        proc_device_signals.store(std::sync::Arc::new(merged));
                    }
                }

                // Physical device outputs (rumble, lightbar) to gilrs pads run
                // regardless of bypass: these carry *incoming* feedback the game
                // sends back to a virtual pad (rumble/FFB), routed to a physical
                // pad via AutoMap — not FlexInput's own mapped input, which is
                // what bypass suppresses. Gating these on !bypass meant a game's
                // rumble never reached the physical pad whenever FlexInput was
                // unfocused (bypass stale-true). HD-rumble amplitude pins get a
                // default frequency injected below so they're audible on Switch
                // Pro (whose HD motors need a non-zero frequency).
                for ((device_id, pin_id), &signal) in &sink_outputs {
                    if device_id.starts_with("gilrs:") {
                        for backend in &mut backends {
                            backend.send(device_id, pin_id, signal);
                        }
                    }
                }
                // Default HD-rumble frequency: when AutoMap feedback drives an
                // hd_*_amp pin (amplitude only) without a paired hd_*_freq, the
                // Switch Pro stays silent (its voice-coil needs a frequency). If
                // a side's amp is non-zero and no explicit freq was routed this
                // tick, inject ~320 Hz (0.6) so the rumble is audible — matching
                // what the manual ping pulse does.
                for (amp_pin, freq_pin) in [("hd_l_amp", "hd_l_freq"), ("hd_r_amp", "hd_r_freq")] {
                    for ((device_id, pin_id), &signal) in &sink_outputs {
                        if !device_id.starts_with("gilrs:") || pin_id != amp_pin { continue; }
                        let amp = signal.as_float();
                        let has_freq = sink_outputs
                            .get(&(device_id.clone(), freq_pin.to_string()))
                            .map(|s| s.as_float() > 0.0)
                            .unwrap_or(false);
                        if amp > 0.01 && !has_freq {
                            for backend in &mut backends {
                                backend.send(device_id, freq_pin, Signal::Float(0.6));
                            }
                        }
                    }
                }
                if !bypass {
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

                // ── Rumble ping ──────────────────────────────────────────────
                // Diagnostic pulse so the user can confirm which physical pad a
                // card maps to. Runs regardless of bypass — it's a deliberate,
                // user-initiated action, not patch output. New requests start a
                // 200 ms pulse; we drive both motors while active and send a
                // single rumble-off when the deadline passes.
                {
                    let now = Instant::now();
                    if let Ok(mut reqs) = ping_requests.try_lock() {
                        for dev_id in reqs.drain(..) {
                            ping_until.insert(dev_id, now + Duration::from_millis(PING_RUMBLE_MS));
                        }
                    }
                    if !ping_until.is_empty() {
                        let mut expired: Vec<String> = Vec::new();
                        for (dev_id, deadline) in ping_until.iter() {
                            let active = now < *deadline;
                            let amp = if active { Signal::Float(1.0) } else { Signal::Float(0.0) };
                            // Drive every rumble pin family so the ping works
                            // regardless of controller type — each backend ignores
                            // pins it doesn't recognise:
                            //   rumble_strong/weak → XInput motors, DS4/DualSense
                            //                        classic motors
                            //   hd_l_amp/hd_r_amp  → Switch Pro HD rumble (needs a
                            //                        non-zero freq to be audible, so
                            //                        we set ~320 Hz too)
                            let freq = if active { Signal::Float(0.6) } else { Signal::Float(0.0) };
                            for backend in &mut backends {
                                backend.send(dev_id, "rumble_strong", amp);
                                backend.send(dev_id, "rumble_weak", amp);
                                backend.send(dev_id, "hd_l_amp", amp);
                                backend.send(dev_id, "hd_r_amp", amp);
                                backend.send(dev_id, "hd_l_freq", freq);
                                backend.send(dev_id, "hd_r_freq", freq);
                            }
                            if !active { expired.push(dev_id.clone()); }
                        }
                        for dev_id in expired { ping_until.remove(&dev_id); }
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
        "logic.has_changed" | "logic.delay" | "logic.counter" | "generator.oscillator" | "generator.envelope" | "module.delay" | "processing.gyro_3dof" => {
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
    scope_lookup: &mut HashMap<usize, Vec<Vec<Option<f32>>>>,
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
            // Switch: the engine reconciles UI clicks + direct/latch inputs
            // and emits the resulting Bool as output[0]. Mirror that back into
            // `params["active"]` so the UI body reads a value that's already
            // in sync with the wires next frame.
            if node.module_id == "module.switch" {
                if let Some(Some(flexinput_core::Signal::Bool(b))) = node.extra.last_out.first() {
                    let cur = node.params.get("active").and_then(|v| v.as_bool()).unwrap_or(false);
                    if cur != *b {
                        node.params.insert("active".to_string(), serde_json::Value::Bool(*b));
                    }
                }
            }
            // Scope samples: move (drain) the per-uid bucket out of
            // scope_lookup instead of cloning each `Vec<Option<f32>>`.
            // Each sample becomes a single push_back into the history
            // ring with no intermediate copy.
            if let Some(samples) = scope_lookup.remove(&uid) {
                let is_trigscope = node.module_id == "display.trigscope";
                if is_trigscope {
                    let win_samples = {
                        let win_ms = node.params.get("ts_win_ms").and_then(|v| v.as_f64()).unwrap_or(200.0).clamp(10.0, 10_000.0) as f32;
                        (win_ms / 1000.0 * current_sample_rate() as f32) as usize
                    };
                    for s in samples {
                        let trig_val = s.first().copied().flatten().unwrap_or(0.0);
                        let rising = node.extra.trig_prev <= 0.0 && trig_val > 0.0;
                        node.extra.trig_prev = trig_val;
                        if rising && !node.extra.trig_armed {
                            node.extra.trig_armed = true;
                            node.extra.trig_acc.clear();
                        }
                        if node.extra.trig_armed {
                            node.extra.trig_acc.push(s);
                            if node.extra.trig_acc.len() >= win_samples {
                                node.extra.trig_capture = Some(std::mem::take(&mut node.extra.trig_acc));
                                node.extra.trig_armed = false;
                            }
                        }
                    }
                } else {
                    let h = &mut node.extra.history;
                    for s in samples {
                        if h.len() >= HISTORY_LEN { h.pop_front(); }
                        h.push_back(s);
                    }
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

/// True when `id` names a real I/O device (physical pad, MIDI port, or virtual
/// sink) rather than a synthetic AutoMap-bus key (`collector:`, `remap:`,
/// `forksel:`, `combiner:`, `lean:`). Used to decide when to fall back to the
/// underlying physical device for feedback (reverse) routing.
fn is_real_device_id(id: &str) -> bool {
    id.starts_with("gilrs:")
        || id.starts_with("midi_in:")
        || id.starts_with("midi_out:")
        || id.starts_with("virtual.")
}

/// Public helper for the viewer: resolve an AutoMap chain back to the
/// originating physical device id (or a sensible fallback) for UI capture.
/// Returns `Some(device_id)` when resolved, or `None` when not wired.
pub fn find_automap_device_id_for_viewer(
    snarl: &Snarl<NodeData>,
    src: OutPinId,
    parent: Option<&crate::canvas::viewer::AutomapGlowParent<'_>>,
) -> Option<String> {
    // Mirror of `find_automap_device_rec` but accepting the viewer's
    // `AutomapGlowParent` chain so the UI can resolve AutoMap origins when
    // rendering inner canvases. Returns (dev_id, pins, fallback) and we
    // surface the fallback or dev_id to the caller.
    fn rec(
        snarl: &Snarl<NodeData>,
        src: OutPinId,
        parents: Option<&crate::canvas::viewer::AutomapGlowParent<'_>>,
    ) -> Option<(String, Vec<String>, Option<String>)> {
        let node = snarl.get_node(src.node)?;
        if node.module_id == "device.source" {
            let dev_id = node.params.get("device_id")?.as_str()?.to_string();
            let pin_ids: Vec<String> = node.params.get("output_pin_ids")?.as_array()?
                .iter().map(|v| v.as_str().unwrap_or("").to_string()).collect();
            return Some((dev_id, pin_ids, None));
        }
        if node.module_id == "module.automap_split" || node.module_id == "module.feedback_control" {
            let am_idx = node.inputs.iter().position(|p| p.signal_type == SignalType::AutoMap)?;
            let in_pin = snarl.in_pin(InPinId { node: src.node, input: am_idx });
            let upstream = *in_pin.remotes.first()?;
            return rec(snarl, upstream, parents);
        }
        if node.module_id == "module.automap_fork" || node.module_id == "module.automap_selector" {
            let node_uid = match parents {
                None => src.node.0,
                Some(p) => flexinput_engine::namespaced_uid(fold_outer_uid_app(p), src.node.0),
            };
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
                .and_then(|s| rec(snarl, s, parents).map(|(id, _, _)| id));
            let collector_uid = match parents {
                None => src.node.0,
                Some(p) => flexinput_engine::namespaced_uid(fold_outer_uid_app(p), src.node.0),
            };
            let collector_id = format!("collector:{}", collector_uid);
            let canonical_pins: Vec<String> = flexinput_core::automap::ALL_PINS
                .iter().map(|p| p.id.to_string()).collect();
            return Some((collector_id, canonical_pins, upstream_dev_id));
        }
        if node.module_id == "module.automap_combiner" {
            let combiner_uid = match parents {
                None => src.node.0,
                Some(p) => flexinput_engine::namespaced_uid(fold_outer_uid_app(p), src.node.0),
            };
            let combiner_id = format!("combiner:{}", combiner_uid);
            let canonical_pins: Vec<String> = flexinput_core::automap::ALL_PINS
                .iter().map(|p| p.id.to_string()).collect();
            let upstream_dev_id = (0..node.inputs.len())
                .find_map(|i| {
                    if node.inputs[i].signal_type != SignalType::AutoMap { return None; }
                    let in_pin = snarl.in_pin(InPinId { node: src.node, input: i });
                    let &s = in_pin.remotes.first()?;
                    rec(snarl, s, parents).map(|(id, _, fallback)| fallback.unwrap_or(id))
                });
            return Some((combiner_id, canonical_pins, upstream_dev_id));
        }
        if node.module_id == "module.remapper" {
            let upstream_dev_id = node.inputs.iter()
                .position(|p| p.signal_type == SignalType::AutoMap)
                .and_then(|am_idx| {
                    let in_pin = snarl.in_pin(InPinId { node: src.node, input: am_idx });
                    in_pin.remotes.first().copied()
                })
                .and_then(|s| rec(snarl, s, parents).map(|(id, _, _)| id));
            let remap_uid = match parents {
                None => src.node.0,
                Some(p) => flexinput_engine::namespaced_uid(fold_outer_uid_app(p), src.node.0),
            };
            let remap_id = format!("remap:{}", remap_uid);
            let canonical_pins: Vec<String> = flexinput_core::automap::ALL_PINS
                .iter().map(|p| p.id.to_string()).collect();
            return Some((remap_id, canonical_pins, upstream_dev_id));
        }
        if node.module_id == "processing.gyro_3dof" {
            let pin_type = node.outputs.get(src.output).map(|p| p.signal_type);
            if pin_type != Some(SignalType::AutoMap) { return None; }
            let lean_uid = match parents {
                None => src.node.0,
                Some(p) => flexinput_engine::namespaced_uid(fold_outer_uid_app(p), src.node.0),
            };
            let lean_id = format!("lean:{}", lean_uid);
            let canonical_pins: Vec<String> = flexinput_core::automap::ALL_PINS
                .iter().map(|p| p.id.to_string()).collect();
            return Some((lean_id, canonical_pins, None));
        }
        if node.module_id == "subpatch" {
            let sp = node.subpatch.as_ref()?;
            let outlet_id: NodeId = sp.snarl.nodes_ids_data()
                .find(|(_, n)| n.value.module_id == "subpatch.outlet"
                    && n.value.params.get("pin_index").and_then(|v| v.as_u64())
                        == Some(src.output as u64))
                .map(|(id, _)| id)?;
            let outlet_in = sp.snarl.in_pin(InPinId { node: outlet_id, input: 0 });
            let inner_upstream = *outlet_in.remotes.first()?;
            let frame = crate::canvas::viewer::AutomapGlowParent { snarl, subpatch_node_id: src.node, prev: parents };
            return rec(&sp.snarl, inner_upstream, Some(&frame));
        }
        if node.module_id == "subpatch.inlet" {
            let pin_idx = node.params.get("pin_index").and_then(|v| v.as_u64())? as usize;
            let p = parents?;
            let outer_in = p.snarl.in_pin(InPinId { node: p.subpatch_node_id, input: pin_idx });
            let upstream = *outer_in.remotes.first()?;
            return rec(p.snarl, upstream, p.prev);
        }
        None
    }
    rec(snarl, src, parent).map(|(dev, _pins, fallback)| fallback.unwrap_or(dev))
}

// Helper to reconstruct the fold_outer_uid value for the viewer parent chain.
fn fold_outer_uid_app(p: &crate::canvas::viewer::AutomapGlowParent<'_>) -> usize {
    match p.prev {
        None => p.subpatch_node_id.0,
        Some(prev) => flexinput_engine::namespaced_uid(fold_outer_uid_app(prev), p.subpatch_node_id.0),
    }
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
    if node.module_id == "module.automap_split" || node.module_id == "module.feedback_control" {
        // Both pass the AutoMap bus through on output 0 from their AutoMap input.
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
    if node.module_id == "processing.gyro_3dof" {
        // Lean dispatch publishes per-pin signals into collector_sigs under
        // `lean:{uid}`. Only the `Map` AutoMap output pin (the last output)
        // resolves to this collector — wires from the other outputs (Out,
        // X, Y, Lean, Lean!) are typed Float / Vec2 / Bool and shouldn't
        // hit this path, but we guard on signal type just in case.
        let pin_type = node.outputs.get(src.output).map(|p| p.signal_type);
        if pin_type != Some(SignalType::AutoMap) { return None; }
        // No upstream fallback — the 3DOF module's Device input feeds gyro
        // data, not a passthrough for other gamepad pins.
        let lean_uid = match parents {
            None => src.node.0,
            Some(p) => flexinput_engine::namespaced_uid(fold_outer_uid(p), src.node.0),
        };
        let lean_id = format!("lean:{}", lean_uid);
        let canonical_pins: Vec<String> = flexinput_core::automap::ALL_PINS
            .iter().map(|p| p.id.to_string()).collect();
        return Some((lean_id, canonical_pins, None));
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

/// Downstream sibling of [`find_automap_device_rec`]: follow an AutoMap bus
/// FORWARD from an output pin to the destination `device.sink`'s device_id,
/// crossing sub-patch boundaries (out through outlets, in through inlets).
/// Returns the first `device.sink` reached. Used by the Feedback Control node to
/// locate the virtual destination whose rumble/light request its outlets tap.
fn find_automap_dest_sink_rec(
    snarl: &Snarl<NodeData>,
    dst: InPinId,
    parents: Option<&AutomapParent<'_>>,
    depth: u32,
) -> Option<String> {
    if depth > 64 { return None; }
    let node = snarl.get_node(dst.node)?;
    // Destination device sink — the end of the line.
    if node.module_id == "device.sink" {
        return node.params.get("device_id").and_then(|v| v.as_str()).map(|s| s.to_string());
    }
    // Outlet: pop OUT to the parent snarl, continue from the subpatch node's
    // matching output pin.
    if node.module_id == "subpatch.outlet" {
        let pin_idx = node.params.get("pin_index").and_then(|v| v.as_u64())? as usize;
        let parent = parents?;
        let out_pin = parent.snarl.out_pin(OutPinId { node: parent.subpatch_id, output: pin_idx });
        for &downstream in &out_pin.remotes {
            if let Some(d) = find_automap_dest_sink_rec(parent.snarl, downstream, parent.prev, depth + 1) {
                return Some(d);
            }
        }
        return None;
    }
    // Wire entered a subpatch via input pin `dst.input`. Pop IN: find the inlet
    // with the matching pin_index and continue from its output downstream.
    if node.module_id == "subpatch" {
        let sp = node.subpatch.as_ref()?;
        let inlet_id: NodeId = sp.snarl.nodes_ids_data()
            .find(|(_, n)| n.value.module_id == "subpatch.inlet"
                && n.value.params.get("pin_index").and_then(|v| v.as_u64())
                    == Some(dst.input as u64))
            .map(|(id, _)| id)?;
        let frame = AutomapParent { snarl, subpatch_id: dst.node, prev: parents };
        let inlet_out = sp.snarl.out_pin(OutPinId { node: inlet_id, output: 0 });
        for &downstream in &inlet_out.remotes {
            if let Some(d) = find_automap_dest_sink_rec(&sp.snarl, downstream, Some(&frame), depth + 1) {
                return Some(d);
            }
        }
        return None;
    }
    // Pass-through AutoMap modules forward the bus on their AutoMap output 0
    // (Splitter, Collector, Fork, Selector, Combiner, Remapper, Feedback Control).
    // Follow output pin 0's downstream remotes.
    let out_pin = snarl.out_pin(OutPinId { node: dst.node, output: 0 });
    for &downstream in &out_pin.remotes {
        if let Some(d) = find_automap_dest_sink_rec(snarl, downstream, parents, depth + 1) {
            return Some(d);
        }
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

    // Pre-pass: physical device ids whose source node enabled the digital→analog
    // trigger bridge (or that are digital-only pads, where it's always on). A sink
    // only honours the bridge when its upstream source is in this set.
    let mut digital_trigger_devs: HashSet<String> = HashSet::new();
    for (_id, node) in &node_list {
        if node.module_id != "device.source" { continue; }
        let Some(dev_id) = node.params.get("device_id").and_then(|v| v.as_str()) else { continue; };
        let opted_in = node.params.get("digital_triggers").and_then(|v| v.as_bool()).unwrap_or(false);
        let digital_only = dev_id.strip_prefix("gilrs:")
            .and_then(|r| r.split(':').next()) == Some("switch_pro");
        if opted_in || digital_only {
            digital_trigger_devs.insert(dev_id.to_string());
        }
    }

    // Pre-pass: collect, for each physical device_id used as an AutoMap source,
    // the list of virtual sink device_ids that auto-map from it. Used to wire
    // feedback signals (rumble, lightbar) backward along AutoMap connections.
    let mut feedback_map: HashMap<String, Vec<String>> = HashMap::new();
    for (node_id, node) in &node_list {
        let is_sink = node.module_id == "device.sink"
            || (node.module_id == "device.source" && !node.inputs.is_empty());
        if !is_sink { continue; }
        // Find this sink's AutoMap source device_id (if wired).
        //
        // When the wire passes through an AutoMap module (Collector, Fork,
        // Selector, Remapper, Combiner, 3DOF), `find_automap_device_rec`
        // returns a SYNTHETIC key (`collector:{uid}`, `remap:{uid}`, …) as the
        // first element and the real upstream physical device as the fallback
        // (third element). Feedback flows back to the *physical* device, so the
        // map must be keyed by the physical id — fall back to it whenever the
        // resolved id isn't itself a real device id. Without this, routing a pad
        // through a sub-patch full of AutoMap modules silently drops rumble.
        let automap_src_dev = (0..node.inputs.len()).find_map(|i| {
            if node.inputs.get(i).map(|p| p.signal_type) != Some(SignalType::AutoMap) {
                return None;
            }
            let pin = snarl.in_pin(InPinId { node: *node_id, input: i });
            let &src = pin.remotes.first()?;
            find_automap_device_rec(snarl, src, parents).map(|(d, _, fallback)| {
                if is_real_device_id(&d) { d } else { fallback.unwrap_or(d) }
            })
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

            // Digital→analog trigger bridge: enabled when the upstream PHYSICAL
            // source opted in (or is digital-only). The upstream physical id is the
            // fallback dev when routed through a collector, else the automap source
            // id when it's itself a real device.
            let upstream_phys = automap_fallback_dev.clone().or_else(|| {
                automap_source.as_ref().map(|(d, _)| d.clone())
                    .filter(|d| is_real_device_id(d))
            });
            let digital_trigger_bridge = upstream_phys
                .map(|d| digital_trigger_devs.contains(&d))
                .unwrap_or(false);

            Some(SinkTarget { device_id: sink_dev_id, pin_ids, multi_sources, automap_source, automap_fallback_dev, feedback_sources, is_self_sink: false, digital_trigger_bridge })
        } else {
            None
        };

        // For modules that read device signals by name, inject the originating device_id.
        let mut params = node.params.clone();
        if matches!(node.module_id.as_str(),
            "processing.gyro_3dof" | "module.automap_split"
            | "module.automap_fork" | "module.automap_selector"
            | "module.remapper" | "module.map_action"
            | "module.automap_collect")
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
                        // combiner ("combiner:"), remapper ("remap:"), and gyro lean ("lean:").
                        if dev_id.starts_with("collector:")
                            || dev_id.starts_with("forksel:")
                            || dev_id.starts_with("combiner:")
                            || dev_id.starts_with("remap:")
                            || dev_id.starts_with("lean:")
                        {
                            params.insert("_automap_collector_id".to_string(),
                                serde_json::Value::String(dev_id));
                        }
                    }
                }
            }
            // Selector: inject parallel dev / collector strings per port so
            // eval can read overrides from upstream Remapper/Collector/etc.
            // before falling back to raw device samples. Mirrors Combiner.
            if node.module_id == "module.automap_selector" {
                let mut extra_devs: Vec<serde_json::Value> = Vec::new();
                let mut extra_collectors: Vec<serde_json::Value> = Vec::new();
                for i in 1..node.inputs.len() {
                    let pin = snarl.in_pin(InPinId { node: *node_id, input: i });
                    let resolved = pin.remotes.first()
                        .and_then(|&src| find_automap_device_rec(snarl, src, parents));
                    let (dev_str, coll_str) = match resolved {
                        Some((dev_id, _, fallback)) => {
                            let is_collector = dev_id.starts_with("collector:")
                                || dev_id.starts_with("forksel:")
                                || dev_id.starts_with("remap:")
                                || dev_id.starts_with("combiner:")
                                || dev_id.starts_with("lean:");
                            let dev = fallback.unwrap_or_else(|| if is_collector { String::new() } else { dev_id.clone() });
                            let coll = if is_collector { dev_id } else { String::new() };
                            (dev, coll)
                        }
                        None => (String::new(), String::new()),
                    };
                    extra_devs.push(serde_json::Value::String(dev_str));
                    extra_collectors.push(serde_json::Value::String(coll_str));
                }
                params.insert("_automap_input_devs".to_string(), serde_json::Value::Array(extra_devs));
                params.insert("_automap_input_collectors".to_string(), serde_json::Value::Array(extra_collectors));
            }
        }
        // Note: we intentionally do NOT mutate the source `snarl` here.
        // The injected `_automap_*` values are stored in the local `params`
        // and carried forward into the returned `NodeSnap.params`, which
        // the UI body renderers read at runtime. Mutating `snarl` would
        // require a mutable borrow of `snarl` which is not available here.
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
                            || dev_id.starts_with("combiner:")
                            || dev_id.starts_with("lean:");
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

        // Feedback Control: stamp the resolved physical source (inlet injection
        // target) and virtual destination (outlet tap source), plus the fixed
        // inlet/outlet pin-id lists so eval can key collector_sigs / dev_sigs.
        if node.module_id == "module.feedback_control" {
            // Inlet injection target: upstream physical pad on AutoMap input 0.
            let src_dev = {
                let pin = snarl.in_pin(InPinId { node: *node_id, input: 0 });
                pin.remotes.first()
                    .and_then(|&src| find_automap_device_rec(snarl, src, parents))
                    .map(|(dev_id, _, fallback)| {
                        if is_real_device_id(&dev_id) { dev_id } else { fallback.unwrap_or(dev_id) }
                    })
                    .filter(|d| is_real_device_id(d))
            };
            if let Some(d) = src_dev {
                params.insert("_fb_source_dev".to_string(), serde_json::Value::String(d));
            }
            // Outlet tap source: downstream virtual destination on AutoMap output 0.
            let dest_dev = {
                let out_pin = snarl.out_pin(OutPinId { node: *node_id, output: 0 });
                out_pin.remotes.iter().find_map(|&downstream| {
                    find_automap_dest_sink_rec(snarl, downstream, parents, 0)
                        .filter(|d| d.starts_with("virtual."))
                })
            };
            if let Some(d) = dest_dev {
                params.insert("_fb_dest_dev".to_string(), serde_json::Value::String(d));
            }
            // Fixed inlet/outlet pin-id lists (parallel to inputs[1..] / outputs[1..]).
            let inlet_ids: Vec<serde_json::Value> = flexinput_core::automap::FEEDBACK_INLET_PINS
                .iter().map(|p| serde_json::Value::String(p.id.to_string())).collect();
            let outlet_ids: Vec<serde_json::Value> = flexinput_core::automap::FEEDBACK_OUTLET_PINS
                .iter().map(|p| serde_json::Value::String(p.id.to_string())).collect();
            params.insert("_fb_inlet_ids".to_string(), serde_json::Value::Array(inlet_ids));
            params.insert("_fb_outlet_ids".to_string(), serde_json::Value::Array(outlet_ids));
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
    // Helper: parse an _automap_*_id-style string (e.g. "remap:42", "collector:7",
    // "forksel:3:0", "combiner:9") to the UID of the publishing node. Returns
    // None for empty strings and unrecognised prefixes.
    let uid_from_collector_id = |s: &str| -> Option<usize> {
        let stripped = s.strip_prefix("collector:")
            .or_else(|| s.strip_prefix("combiner:"))
            .or_else(|| s.strip_prefix("remap:"))
            .or_else(|| s.strip_prefix("lean:"))
            .or_else(|| s.strip_prefix("forksel:").and_then(|t| t.split(':').next()))?;
        stripped.parse::<usize>().ok()
    };
    // Inside a sub-patch, `find_automap_device_rec` returns NAMESPACED uids
    // (folded via `namespaced_uid(outer_chain, node.uid)`) so they match the
    // keys the engine's subgraph eval publishes to. But `snap.node_uid` in
    // the inner snap list is the RAW uid (= NodeId.0), since the inner
    // build hasn't applied namespacing to its own snaps. Compare against
    // the raw-uid equivalent: strip the outer chain.
    let outer_chain_uid: Option<usize> = parents.map(fold_outer_uid);
    let match_inner_uid = |target_uid: usize, snap_uid: usize| -> bool {
        if snap_uid == target_uid { return true; }
        if let Some(outer) = outer_chain_uid {
            if flexinput_engine::namespaced_uid(outer, snap_uid) == target_uid {
                return true;
            }
        }
        false
    };
    for (idx, snap) in snaps.iter().enumerate() {
        // Regular nodes: single-source inputs.
        for &(src_idx, _) in snap.input_sources.iter().flatten() {
            dependents[src_idx].push(idx);
            in_degree[idx] += 1;
        }
        // AutoMap-consuming non-sinks (Combiner, Selector, Fork): the
        // `input_sources` chain only reaches the *immediate* upstream node, but
        // these consumers read from `collector_sigs` keyed by the originating
        // collector/remapper UID found by `find_automap_device_rec`. That UID
        // may belong to a node several hops upstream (e.g. through a Splitter)
        // or even inside a sub-patch — in either case the topo edge from the
        // immediate predecessor is not enough to guarantee the collector
        // publishes before this node reads. Add explicit deps so the
        // Remapper / Collector / Combiner / Fork that backs each AutoMap input
        // is scheduled before this consumer.
        let is_am_consumer = matches!(snap.module_id.as_str(),
            "module.automap_combiner"
            | "module.automap_selector"
            | "module.automap_fork"
            | "module.automap_split"
            | "module.automap_collect");
        if is_am_consumer {
            let mut seen: HashSet<usize> = HashSet::new();
            // Combiner: per-port collector IDs are pre-baked in
            // `_automap_input_collectors`. Use them directly to avoid a second
            // call into `find_automap_device_rec`.
            let collector_ids: Vec<String> = snap.params.get("_automap_input_collectors")
                .and_then(|v| v.as_array())
                .map(|a| a.iter().map(|v| v.as_str().unwrap_or("").to_string()).collect())
                .unwrap_or_default();
            for cid in &collector_ids {
                if let Some(uid) = uid_from_collector_id(cid) {
                    if let Some(am_idx) = snaps.iter().position(|s| match_inner_uid(uid, s.node_uid)) {
                        if am_idx != idx && seen.insert(am_idx) {
                            dependents[am_idx].push(idx);
                            in_degree[idx] += 1;
                        }
                    }
                }
            }
            // Fallback: walk every AutoMap input pin and trace through. Catches
            // Selector / Fork / Split (which don't populate
            // `_automap_input_collectors`) and any case where the param array
            // is missing or stale.
            let (outer_node_id, outer_node) = node_list[idx];
            for i in 0..outer_node.inputs.len() {
                if outer_node.inputs.get(i).map(|p| p.signal_type) != Some(SignalType::AutoMap) {
                    continue;
                }
                let pin = snarl.in_pin(InPinId { node: outer_node_id, input: i });
                let Some(&src) = pin.remotes.first() else { continue };
                let Some((am_dev_id, _, _)) = find_automap_device_rec(snarl, src, parents) else { continue };
                let Some(uid) = uid_from_collector_id(&am_dev_id) else { continue };
                let Some(am_idx) = snaps.iter().position(|s| match_inner_uid(uid, s.node_uid)) else { continue };
                if am_idx != idx && seen.insert(am_idx) {
                    dependents[am_idx].push(idx);
                    in_degree[idx] += 1;
                }
            }
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
                        if let Some(am_idx) = snaps.iter().position(|s| match_inner_uid(uid, s.node_uid)) {
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
    "display.trigscope",
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
/// Actions a single tab-bar frame can request. Bundled into a struct
/// (rather than a wide tuple) now that the File menu / Auto-switch
/// toggle live here alongside the tab list.
struct TabBarActions {
    switch_to: Option<usize>,
    close_idx: Option<usize>,
    new_tab: bool,
    bypass_toggle: Option<usize>,
    do_save: bool,
    do_load: bool,
    do_save_workspace: bool,
    do_load_workspace: bool,
    do_bind: bool,
    do_close: bool,
}

fn show_tab_bar(
    ui: &mut egui::Ui,
    tabs: &[PatchTab],
    active_tab: usize,
    effective_bypass: &[bool],
    auto_switch: &mut bool,
) -> TabBarActions {
    let mut switch_to: Option<usize> = None;
    let mut close_idx: Option<usize> = None;
    let mut new_tab = false;
    let mut bypass_toggle: Option<usize> = None;
    let mut do_save = false;
    let mut do_load = false;
    let mut do_save_workspace = false;
    let mut do_load_workspace = false;
    let mut do_bind = false;
    let mut do_close = false;

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

    // The bar splits into two regions on one horizontal row:
    //   1. A PINNED left cluster (File menu + Auto toggle + divider) that
    //      never scrolls, sitting on a solid #1B1B1B "Side Shadow"
    //      backdrop.
    //   2. A horizontal ScrollArea holding only the tabs + "+" button.
    // Tabs scrolling toward the divider fade into the Side Shadow; a
    // right-edge fade appears when more tabs overflow off-screen.
    let panel_outer = ui.max_rect();
    // #1B1B1B from the mockup's Side Shadow gradient.
    let shadow_solid = egui::Color32::from_rgb(27, 27, 27);

    // `tabs_left` is filled in after the pinned cluster lays out; we need
    // it both for the ScrollArea origin and the shadow geometry.
    let mut tabs_left = panel_outer.left();

    // Use horizontal_centered so EVERY item (the short File/Auto widgets
    // AND the full-height tab rects) is vertically centered against the
    // full row height — plain `horizontal()` top-aligns items placed
    // before the tall tabs, which is what made File/Auto ride high.
    ui.horizontal_centered(|ui| {
        // Reserve a paint slot for the Side Shadow backdrop NOW, so it
        // renders UNDER the File/Auto widgets that follow (same layer,
        // earlier z). We fill it once `tabs_left` is known below.
        let shadow_idx = ui.painter().add(egui::Shape::Noop);

        // ── 1. Pinned File menu + Auto-switch cluster ──────────────────
        ui.add_space(8.0);
        ui.menu_button("File", |ui| {
            if ui.button("New").clicked()                       { new_tab = true; ui.close(); }
            if ui.button("Save Patch…").clicked()               { do_save  = true; ui.close(); }
            if ui.button("Load Patch…").clicked()               { do_load  = true; ui.close(); }
            ui.separator();
            if ui.button("Save Workspace…").clicked()           { do_save_workspace = true; ui.close(); }
            if ui.button("Load Workspace…").clicked()           { do_load_workspace = true; ui.close(); }
            ui.separator();
            if ui.button("Bind Tab to Process…").clicked()      { do_bind  = true; ui.close(); }
            ui.separator();
            if ui.button("Close Tab").clicked()                 { do_close = true; ui.close(); }
        });

        ui.add_space(6.0);

        // Auto-switch toggle button. Rendered as a single selectable
        // widget: "Auto" text + a filled circle (same style as the tab
        // activity/bypass dot) that follows the current text color. Using
        // a constant-size painted dot — rather than swapping ●/○ glyphs —
        // keeps the button width fixed so toggling never shifts the row.
        let auto_hover = if *auto_switch {
            "Auto-switch ON — tabs switch when a bound process gains focus"
        } else {
            "Auto-switch OFF — tab switching is manual"
        };
        {
            let font_id = egui::TextStyle::Button.resolve(ui.style());
            let galley = ui.painter().layout_no_wrap(
                "Auto".to_owned(), font_id, egui::Color32::PLACEHOLDER);
            let pad = ui.spacing().button_padding;
            let dot_d = 8.0_f32;          // dot diameter slot
            let gap = 5.0_f32;            // text → dot gap
            let content_w = galley.size().x + gap + dot_d;
            // Height matches a normal button (text + vertical padding) so
            // the Auto pill is the same height as the File button; the
            // surrounding horizontal_centered layout vertically centers it
            // in the tab row. (Using the full row height here made the
            // selected-background pill taller than File.)
            let size = egui::vec2(
                content_w + pad.x * 2.0,
                galley.size().y + pad.y * 2.0,
            );
            let (rect, resp) = ui.allocate_exact_size(size, egui::Sense::click());
            let vis = ui.style().interact_selectable(&resp, *auto_switch);
            // Selectable background (matches egui's SelectableLabel).
            if *auto_switch || resp.hovered() {
                ui.painter().rect(
                    rect, vis.corner_radius, vis.weak_bg_fill,
                    egui::Stroke::NONE, egui::StrokeKind::Inside);
            }
            let text_color = vis.text_color();
            let text_pos = egui::pos2(rect.left() + pad.x, rect.center().y - galley.size().y / 2.0);
            ui.painter().galley(text_pos, galley.clone(), text_color);
            let dot_cx = rect.left() + pad.x + galley.size().x + gap + dot_d / 2.0;
            ui.painter().circle(
                egui::pos2(dot_cx, rect.center().y),
                4.0, text_color, egui::Stroke::new(1.2, text_color));
            if resp.on_hover_text(auto_hover).clicked() {
                *auto_switch = !*auto_switch;
            }
        }

        // Divider between the pinned cluster and the scrolling tabs.
        ui.add_space(8.0);
        let (sep_rect, _) = ui.allocate_exact_size(egui::vec2(1.0, h), egui::Sense::hover());
        let inset = 7.0_f32;
        let x = sep_rect.center().x;
        ui.painter().line_segment(
            [egui::pos2(x, sep_rect.top() + inset),
             egui::pos2(x, sep_rect.bottom() - inset)],
            egui::Stroke::new(1.0, sep_color),
        );
        ui.add_space(2.0);

        // Left edge of the scrolling tab region — the Side Shadow fade is
        // anchored here so tabs dissolve as they slide under the pinned
        // cluster.
        tabs_left = ui.cursor().left();

        // Fill the reserved slot: solid #1B1B1B backdrop covering the
        // pinned cluster (panel left → tabs_left). Because the slot was
        // reserved before File/Auto, this paints UNDER them. Stop 1 px
        // short of the bottom so the tab-bar / content border line stays
        // visible (otherwise the dark block overlaps it).
        ui.painter().set(
            shadow_idx,
            egui::Shape::rect_filled(
                egui::Rect::from_min_max(
                    panel_outer.left_top(),
                    egui::pos2(tabs_left, panel_outer.bottom() - 1.0),
                ),
                egui::CornerRadius::ZERO,
                shadow_solid,
            ),
        );

        // ── 2. Scrolling tab strip (tabs + "+") ────────────────────────
        let scroll_out = egui::ScrollArea::horizontal()
            .id_salt("tab_scroll")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.horizontal_centered(|ui| {
                ui.spacing_mut().item_spacing = egui::Vec2::ZERO;
                // Initial left padding so the first tab sits clear of the
                // Side Shadow fade when unscrolled. This padding scrolls
                // away with the content, letting tabs slide under the
                // fade once the user scrolls.
                ui.add_space(50.0);

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

        // ── Side Shadow fades ──────────────────────────────────────────
        // Left: #1B1B1B → transparent fade starting at the cluster
        // boundary (the soft edge of the solid backdrop), so tabs
        // dissolve as they scroll under File/Auto. Right: same fade,
        // only when more tabs overflow off-screen.
        paint_tab_scroll_shadows(ui.ctx(), &scroll_out, panel_outer, tabs_left, shadow_solid);
    });

    TabBarActions {
        switch_to, close_idx, new_tab, bypass_toggle,
        do_save, do_load, do_save_workspace, do_load_workspace,
        do_bind, do_close,
    }
}

/// Paint the tab-strip Side Shadow fades, matching the mockup's
/// `linear-gradient(90deg, #1B1B1B 70%, transparent 100%)`.
///
/// LEFT: a `#1B1B1B → transparent` fade beginning at `tabs_left` — the
/// soft right edge of the solid backdrop behind File/Auto — so tabs
/// dissolve into it as they scroll under the pinned cluster. Always
/// drawn. RIGHT: the mirror fade, only when more tabs overflow.
fn paint_tab_scroll_shadows<R>(
    ctx: &egui::Context,
    out: &egui::scroll_area::ScrollAreaOutput<R>,
    panel_outer: egui::Rect,
    tabs_left: f32,
    shadow_solid: egui::Color32,
) {
    const FADE_W: f32 = 42.0; // ~30% of the mockup's 141px Side Shadow
    let inner = out.inner_rect;
    let offset_x = out.state.offset.x;
    let max_scroll = (out.content_size.x - inner.width()).max(0.0);
    let can_scroll_right = offset_x < max_scroll - 0.5;

    // Render the fade on Background so floating windows (which default to
    // Order::Middle) sit on top of it instead of being overlaid by it.
    let layer = egui::LayerId::new(
        egui::Order::Background,
        egui::Id::new(("tab_edge_fade", out.id)),
    );
    let mut painter = ctx.layer_painter(layer);
    painter.set_clip_rect(panel_outer);

    let clear = egui::Color32::TRANSPARENT;
    let top = panel_outer.top();
    // Stop 1 px above the bottom so the tab-bar / content border line
    // stays visible under the fade.
    let bot = panel_outer.bottom() - 1.0;
    // Opaque lead-in that overlaps the base-layer backdrop (which ends at
    // tabs_left), so a tab scrolled into this zone is fully hidden and
    // there's no seam where the backdrop and fade meet.
    const OVERLAP: f32 = 10.0;

    // Opaque #1B1B1B lead-in covering [tabs_left - OVERLAP, tabs_left].
    painter.rect_filled(
        egui::Rect::from_min_max(
            egui::pos2(tabs_left - OVERLAP, top),
            egui::pos2(tabs_left, bot)),
        egui::CornerRadius::ZERO,
        shadow_solid,
    );
    // Fade #1B1B1B → transparent over [tabs_left, tabs_left + FADE_W].
    paint_tab_gradient_quad(&painter,
        egui::pos2(tabs_left, top), egui::pos2(tabs_left + FADE_W, bot),
        shadow_solid, clear, clear, shadow_solid);

    // Right fade — transparent → solid #1B1B1B at the right edge, only
    // when tabs overflow off-screen there.
    if can_scroll_right {
        let x0 = panel_outer.right() - FADE_W;
        let x1 = panel_outer.right();
        paint_tab_gradient_quad(&painter,
            egui::pos2(x0, top), egui::pos2(x1, bot),
            clear, shadow_solid, shadow_solid, clear);
    }
}

/// Two-triangle horizontal gradient quad. Corner colors map tl, tr, br,
/// bl. (Local copy mirroring the physical-devices panel helper to keep
/// the tab-bar self-contained.)
fn paint_tab_gradient_quad(
    painter: &egui::Painter,
    tl: egui::Pos2,
    br: egui::Pos2,
    c_tl: egui::Color32, c_tr: egui::Color32, c_br: egui::Color32, c_bl: egui::Color32,
) {
    use egui::epaint::{Mesh, Vertex};
    let mut mesh = Mesh::default();
    let uv = egui::epaint::WHITE_UV;
    let tr = egui::pos2(br.x, tl.y);
    let bl = egui::pos2(tl.x, br.y);
    let i = mesh.vertices.len() as u32;
    mesh.vertices.push(Vertex { pos: tl, uv, color: c_tl });
    mesh.vertices.push(Vertex { pos: tr, uv, color: c_tr });
    mesh.vertices.push(Vertex { pos: br, uv, color: c_br });
    mesh.vertices.push(Vertex { pos: bl, uv, color: c_bl });
    mesh.indices.extend_from_slice(&[i, i+1, i+2, i, i+2, i+3]);
    painter.add(mesh);
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
    do_save_workspace: &mut bool,
    do_load_workspace: &mut bool,
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
    ui_mode: settings::UiMode,
    do_set_mode: &mut Option<settings::UiMode>,
) {
    let bar = ui.max_rect();
    let h = bar.height();
    let btn_w = 46.0_f32;
    let ctrl_w = btn_w * 3.0;
    // Wide enough for the Wide mode pill (~204 px) + dividers +
    // undo/redo + pin without forcing the Short variant; capped so the
    // cluster never crowds the centered FlexInput title on narrow
    // windows (the pill auto-falls to its Short variant when squeezed).
    let left_w = 380.0_f32.min(bar.width() * 0.42);

    // Full-bar drag sensing (placed first so interactive widgets above take priority).
    let drag = ui.interact(bar, ui.id().with("tb_drag"), egui::Sense::click_and_drag());

    // The File menu / Auto-switch toggle moved down into the tab bar
    // (see `show_tab_bar`); these params are still threaded through for
    // potential future use but are no longer surfaced here.
    let _ = (do_save, do_load, do_save_workspace, do_load_workspace,
             do_new, do_close, do_bind, do_hidhide, auto_switch);

    // ── Left cluster: Mode pill → undo/redo → pin ──────────────────────────
    // The Easy/Advanced mode pill now anchors at the FAR LEFT of the
    // title bar (where File/Auto used to live), followed by the
    // undo/redo buttons and the pin toggle. Matches the new mockup.
    let left_rect = egui::Rect::from_min_size(bar.min, egui::vec2(left_w, h));
    ui.scope_builder(egui::UiBuilder::new().max_rect(left_rect), |ui| {
        ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
            ui.add_space(8.0);

            // ── Mode pill (Easy / Advanced) ────────────────────────────
            // Adaptive: pick the widest variant that fits the slack in
            // the left cluster. The pill is SVG-rendered with manual
            // rect math, so allocate a slot in the left-to-right flow
            // and hand that rect to `render_mode_pill`.
            let pill_h = (h - 4.0).max(20.0);
            let wide_w  = pill_size_px(MODE_WHOLE_PILL_SVG, pill_h).0;
            let short_w = pill_size_px(MODE_SHORT_PILL_SVG, pill_h).0;
            // Reserve room for undo/redo + pin + dividers after the pill
            // (~120 px); if the window is narrow, fall back to the short
            // pill so those controls never get clipped.
            let avail = ui.available_width();
            let (pill_w, variant) = if avail - wide_w >= 120.0 {
                (wide_w, ModePillVariant::Wide)
            } else {
                (short_w, ModePillVariant::Short)
            };
            let (pill_rect, _) = ui.allocate_exact_size(
                egui::vec2(pill_w, pill_h), egui::Sense::hover());
            render_mode_pill(ui, pill_rect, variant, ui_mode, do_set_mode);

            // ── Divider before undo/redo ───────────────────────────────
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

            // ── See-through eye toggle ──────────────────────────────────
            // Shares the same ctx-data slots the (legacy) zoom-overlay
            // eye used, so see-through state + opacity stay in sync no
            // matter which mode the user is in. Click toggles; hover
            // pops out a vertical opacity slider.
            ui.add_space(4.0);
            render_eye_toggle(ui, h);
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

    // ── Center: FlexInput title (matches Figma `title` group) ─────────────
    // Layout (Figma group 122×38, scaled to the title-bar height):
    //   • Dark rounded "TitleBG" pill (#1B1B1B) behind logo + text;
    //     gains a white outline on hover.
    //   • Square logo tile (rasterized from icon_v2.svg — the dark
    //     rounded square with the Fi glyph) overhanging the pill's left
    //     edge, sized to the full bar height so it pokes slightly above
    //     and below the pill.
    //   • "FlexInput" text (16 px in Figma) just right of the tile.
    // The whole group is clickable → opens Settings.
    let mid = bar.center();
    let base_color = egui::Color32::WHITE; // Figma title text is pure white

    // Figma proportions: logo 38, pill 32 tall, text 16 px.
    // Scale to the bar: tile = bar height, pill a touch shorter — then a
    // uniform 0.9 nudge so the whole group reads a touch smaller than the
    // exact mockup measurements (matches the intended visual weight).
    const TITLE_SCALE: f32 = 0.9;
    let tile = ((h - 2.0) * TITLE_SCALE).max(24.0 * TITLE_SCALE);  // logo tile side (overhangs pill)
    let pill_h = ((h - 6.0) * TITLE_SCALE).max(20.0 * TITLE_SCALE); // dark pill height
    let text_px = 16.0_f32 * TITLE_SCALE;
    let logo_text_gap = 8.0_f32 * TITLE_SCALE;     // gap between tile and text
    let text_pad_right = 12.0_f32 * TITLE_SCALE;   // pill padding after the text

    let font_id = egui::FontId::proportional(text_px);
    let galley = ui.painter().layout_no_wrap("FlexInput".to_string(), font_id, base_color);
    let text_size = galley.size();

    // The pill spans from a few px inside the tile's left to the right of
    // the text; the tile overhangs the pill's left like in the mock.
    let tile_overhang = 4.0_f32 * TITLE_SCALE; // how far the tile pokes past the pill left
    let group_w = tile + logo_text_gap + text_size.x + text_pad_right;
    let group_left = mid.x - group_w / 2.0;

    let tile_rect = egui::Rect::from_min_size(
        egui::pos2(group_left, mid.y - tile / 2.0),
        egui::vec2(tile, tile),
    );
    let pill_left = tile_rect.left() + tile_overhang;
    let pill_rect = egui::Rect::from_min_max(
        egui::pos2(pill_left, mid.y - pill_h / 2.0),
        egui::pos2(group_left + group_w, mid.y + pill_h / 2.0),
    );
    let text_left = tile_rect.right() + logo_text_gap;

    // Hit rect covers the whole group (tile + pill).
    let hit_rect = tile_rect.union(pill_rect);
    let logo_resp = ui.interact(hit_rect, ui.id().with("logo_settings"), egui::Sense::click());
    if logo_resp.clicked() {
        *toggle_settings = true;
    }
    if logo_resp.hovered() {
        ctx.set_cursor_icon(egui::CursorIcon::PointingHand);
    }

    // Dark pill background (#1B1B1B), brightening slightly on hover with
    // an outline around the whole pill as the hover affordance.
    let pill_fill = if logo_resp.hovered() {
        egui::Color32::from_rgb(38, 38, 38)
    } else {
        egui::Color32::from_rgb(27, 27, 27) // #1B1B1B
    };
    let pill_stroke = if logo_resp.hovered() {
        egui::Stroke::new(1.0, egui::Color32::from_white_alpha(110))
    } else {
        egui::Stroke::NONE
    };
    ui.painter().rect(
        pill_rect,
        egui::CornerRadius::same(6),
        pill_fill,
        pill_stroke,
        egui::StrokeKind::Inside,
    );

    // Logo tile + text.
    let painter = ui.painter();
    if let Some(tex) = logo {
        let uv = egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0));
        // Snap to physical pixels so the bitmap downscale samples on-pixel
        // (otherwise LINEAR sampling smears the icon's alpha edges).
        let ppp = ctx.pixels_per_point();
        let snap = |v: f32| (v * ppp).round() / ppp;
        let logo_rect = egui::Rect::from_min_max(
            egui::pos2(snap(tile_rect.left()), snap(tile_rect.top())),
            egui::pos2(snap(tile_rect.right()), snap(tile_rect.bottom())),
        );
        painter.image(tex.id(), logo_rect, uv, egui::Color32::WHITE);
    }
    painter.galley(egui::pos2(text_left, mid.y - text_size.y / 2.0), galley, base_color);

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

// ── Mode pill (Easy / Advanced) ──────────────────────────────────────────────
//
// SVG-driven segmented control. Two presentation variants:
//
//   `Wide`  — `mode_whole_pill.svg` (outer pill with "Mode:" label
//             baked in) + `easy_mode.svg` or `advanced_mode.svg`
//             overlaid right-anchored on top of the pill. Both chip
//             SVGs are a single graphic containing BOTH halves with
//             the slanted divider already drawn; the active half is
//             tinted, the inactive half is the muted variant. We
//             just split the rendered image rectangle into two click
//             zones (Easy on left, Advanced on right).
//
//   `Short` — `mode_short_pill.svg` (outer pill, no "Mode:" label) +
//             same chip SVG overlay. Used when there isn't enough
//             room between the File-menu cluster and the centered
//             FlexInput title to fit the wide variant.

/// App logo (the dark rounded "Fi" tile), pre-baked to a 256px PNG from
/// `icon_v2.svg`. Used for both the title-bar logo and the OS
/// window/taskbar icon. Baked rather than rasterized at runtime because
/// the SVG's blur/color-matrix filters take ~45s for resvg to render at
/// 256px — that delay was stalling app startup. Re-bake whenever the
/// source SVG changes (render_app_icon → save_png at 256).
pub(crate) const APP_ICON_PNG: &[u8] = include_bytes!(
    "../../../app/assets/icon_v2_256.png");

const MODE_WHOLE_PILL_SVG: &[u8] = include_bytes!(
    "../../../app/assets/mode_whole_pill.svg");
const MODE_SHORT_PILL_SVG: &[u8] = include_bytes!(
    "../../../app/assets/mode_short_pill.svg");
const MODE_EASY_SVG: &[u8] = include_bytes!(
    "../../../app/assets/easy_mode.svg");
const MODE_ADV_SVG:  &[u8] = include_bytes!(
    "../../../app/assets/advanced_mode.svg");

/// Decode the pre-baked app-logo PNG ([`APP_ICON_PNG`], 256px) into an
/// [`egui::IconData`] for the OS window / taskbar icon. Decoding a PNG is
/// instant, unlike rasterizing the filter-heavy source SVG. Returns
/// `None` if the bundled PNG fails to decode.
pub fn render_app_icon() -> Option<egui::IconData> {
    let icon = eframe::icon_data::from_png_bytes(APP_ICON_PNG).ok()?;
    Some(egui::IconData { rgba: icon.rgba, width: icon.width, height: icon.height })
}

// Render height in logical pixels; SVGs are 28/30 px tall by design,
// 22 is a comfortable shrunk title-bar size.
const MODE_PILL_RENDER_H: f32 = 22.0;

#[derive(Clone, Copy, Debug)]
enum ModePillVariant { Wide, Short }

/// See-through eye toggle for the title bar. Click flips see-through on/off;
/// hover pops out a vertical opacity slider below the button. Reads & writes
/// the same ctx-data slots (`SEE_THROUGH_DATA_KEY` / `SEE_THROUGH_ALPHA_KEY`)
/// that `FlexInputApp::update` mirrors into `settings`, so it works from
/// either Easy or Advanced mode without threading state through.
fn render_eye_toggle(ui: &mut egui::Ui, bar_h: f32) {
    let see_through_id = egui::Id::new(crate::canvas::SEE_THROUGH_DATA_KEY);
    let see_through_on: bool = ui.ctx().data(|d| d.get_temp::<bool>(see_through_id))
        .unwrap_or(false);

    let eye_label = egui::RichText::new("👁").size(14.0);
    let eye_btn = egui::SelectableLabel::new(see_through_on, eye_label);
    let eye_resp = ui.add_sized(egui::vec2(26.0, (bar_h - 6.0).max(18.0)), eye_btn);
    let hover = if see_through_on {
        "See-through: ON — click to make app fully opaque.\nHover to adjust opacity."
    } else {
        "See-through: OFF — click to make app translucent.\nHover to adjust opacity."
    };
    let eye_resp = eye_resp.on_hover_text(hover);
    if eye_resp.clicked() {
        ui.ctx().data_mut(|d| d.insert_temp(see_through_id, !see_through_on));
    }

    // Opacity popover BELOW the button (title bar is at the top of the
    // window, so the slider drops down). Same grace-timer pattern as the
    // legacy zoom-overlay version so the cursor can travel from the eye
    // to the slider without the popup closing mid-traversal.
    let popup_id = ui.id().with("titlebar_see_through_popup");
    let last_hover_id = popup_id.with("last_hover");
    const POPUP_GRACE: std::time::Duration = std::time::Duration::from_millis(2500);
    let now = std::time::Instant::now();
    let last_hover: Option<std::time::Instant> =
        ui.ctx().data(|d| d.get_temp::<std::time::Instant>(last_hover_id));
    if eye_resp.hovered() {
        ui.ctx().data_mut(|d| d.insert_temp(last_hover_id, now));
    }
    let popup_visible = eye_resp.hovered()
        || last_hover.map(|t| now.duration_since(t) < POPUP_GRACE).unwrap_or(false);
    if popup_visible {
        let alpha_id = egui::Id::new(crate::canvas::SEE_THROUGH_ALPHA_KEY);
        let mut alpha: f32 = ui.ctx().data(|d| d.get_temp::<f32>(alpha_id))
            .unwrap_or(0.55);
        let popup_area = egui::Area::new(popup_id)
            .order(egui::Order::Foreground)
            .fixed_pos(egui::pos2(
                eye_resp.rect.center().x - 28.0,
                eye_resp.rect.bottom() + 4.0,
            ))
            .interactable(true);
        ui.ctx().request_repaint_after(std::time::Duration::from_millis(100));
        let popup_resp = popup_area.show(ui.ctx(), |ui| {
            let bg = ui.visuals().window_fill();
            egui::Frame::default()
                .fill(egui::Color32::from_rgba_unmultiplied(bg.r(), bg.g(), bg.b(), 240))
                .stroke(egui::Stroke::new(1.0,
                    ui.visuals().widgets.noninteractive.bg_stroke.color))
                .corner_radius(6.0)
                .inner_margin(egui::Margin::same(6))
                .show(ui, |ui| {
                    ui.vertical_centered(|ui| {
                        ui.label(egui::RichText::new(format!("{:.0}%", alpha * 100.0)).small());
                        let resp = ui.add_sized(
                            egui::vec2(40.0, 96.0),
                            egui::Slider::new(&mut alpha, 0.0_f32..=1.0)
                                .vertical().show_value(false),
                        );
                        if resp.changed() {
                            ui.ctx().data_mut(|d|
                                d.insert_temp(alpha_id, alpha.clamp(0.0, 1.0)));
                        }
                    });
                }).response
        }).response;
        if eye_resp.hovered() || popup_resp.hovered() {
            ui.ctx().data_mut(|d| d.insert_temp(last_hover_id, now));
        }
    }
}

/// Compute the on-screen width that a pill SVG occupies when rendered
/// at `render_h` logical pixels, preserving the SVG's intrinsic aspect.
fn pill_size_px(svg_bytes: &[u8], render_h: f32) -> (f32, f32) {
    // Cheap aspect probe: parse `viewBox` or width/height from the
    // SVG header without going through usvg. Falls back to 1:1.
    let text = std::str::from_utf8(svg_bytes).unwrap_or("");
    let aspect = parse_svg_aspect(text).unwrap_or(1.0);
    (render_h * aspect, render_h)
}

fn parse_svg_aspect(text: &str) -> Option<f32> {
    // Look for the `viewBox="0 0 W H"` attribute first; fall back to
    // separate width / height attributes if the viewBox is missing.
    if let Some(vb_start) = text.find("viewBox=\"") {
        let s = &text[vb_start + 9..];
        if let Some(end) = s.find('"') {
            let parts: Vec<&str> = s[..end].split_whitespace().collect();
            if parts.len() == 4 {
                let w: f32 = parts[2].parse().ok()?;
                let h: f32 = parts[3].parse().ok()?;
                if h > 0.0 { return Some(w / h); }
            }
        }
    }
    let w = parse_svg_attr(text, "width")?;
    let h = parse_svg_attr(text, "height")?;
    if h > 0.0 { Some(w / h) } else { None }
}

fn parse_svg_attr(text: &str, name: &str) -> Option<f32> {
    let key = format!("{}=\"", name);
    let i = text.find(&key)?;
    let s = &text[i + key.len()..];
    let end = s.find('"')?;
    s[..end].parse().ok()
}

/// Rasterize an SVG to a cached non-square texture at exactly
/// (w_px, h_px) DEVICE pixels. Reuses the recolored rasterizer; since
/// w_px : h_px already matches the SVG aspect, no letterboxing occurs.
fn mode_pill_texture(
    ui: &egui::Ui,
    bytes: &'static [u8],
    w_px: u32,
    h_px: u32,
) -> Option<egui::TextureHandle> {
    let cache_key = egui::Id::new(("mode_pill_tex", bytes.as_ptr() as usize, w_px, h_px));
    if let Some(h) = ui.ctx().data(|d| d.get_temp::<egui::TextureHandle>(cache_key)) {
        return Some(h);
    }
    let text = std::str::from_utf8(bytes).ok()?;
    let img = crate::canvas::viewer::rasterize_svg_recolored(
        text, w_px, h_px, "override", egui::Color32::TRANSPARENT)?;
    let handle = ui.ctx().load_texture(
        format!("mode_pill_{:p}_{}x{}", bytes.as_ptr(), w_px, h_px),
        img,
        egui::TextureOptions::LINEAR,
    );
    ui.ctx().data_mut(|d| d.insert_temp(cache_key, handle.clone()));
    Some(handle)
}

fn paint_pill_svg(ui: &mut egui::Ui, bytes: &'static [u8], rect: egui::Rect) {
    let ppp = ui.ctx().pixels_per_point();
    // Snap the destination rect to physical pixel boundaries so the
    // texture lands at integer texel positions — otherwise LINEAR
    // sampling blends adjacent pixels and softens every edge,
    // making the rasterized SVG text look blurry.
    let snap = |v: f32| (v * ppp).round() / ppp;
    let rect = egui::Rect::from_min_max(
        egui::pos2(snap(rect.left()),  snap(rect.top())),
        egui::pos2(snap(rect.right()), snap(rect.bottom())),
    );
    let w_px = ((rect.width())  * ppp).round() as u32;
    let h_px = ((rect.height()) * ppp).round() as u32;
    if let Some(tex) = mode_pill_texture(ui, bytes, w_px, h_px) {
        let uv = egui::Rect::from_min_max(
            egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0));
        ui.painter().image(tex.id(), rect, uv, egui::Color32::WHITE);
    }
}

fn render_mode_pill(
    ui: &mut egui::Ui,
    rect: egui::Rect,
    variant: ModePillVariant,
    mode: settings::UiMode,
    do_set_mode: &mut Option<settings::UiMode>,
) {
    // 1. Outer pill background (with or without "Mode:" label).
    let bg_svg = match variant {
        ModePillVariant::Wide  => MODE_WHOLE_PILL_SVG,
        ModePillVariant::Short => MODE_SHORT_PILL_SVG,
    };
    paint_pill_svg(ui, bg_svg, rect);

    // 2. Chip overlay (Easy/Adv combined) right-anchored on the pill.
    // The chip SVG already contains both halves AND the slanted
    // divider; only the colors differ between easy_mode.svg and
    // advanced_mode.svg.
    //
    // Height note: the pill SVGs are 30 px tall (28 inner + 1 px
    // stroke on each side); the chip SVGs are 28 px tall with no
    // stroke. To keep the chip INSIDE the pill's outline, render it
    // at a shrunk height matching the pill's inner content area.
    let chip_svg = match mode {
        settings::UiMode::Easy     => MODE_EASY_SVG,
        settings::UiMode::Advanced => MODE_ADV_SVG,
    };
    let pill_inset = 1.0_f32; // matches the 1 px stroke on the pill SVGs
    let chip_h = (rect.height() - 2.0 * pill_inset).max(1.0);
    let (chip_w, _) = pill_size_px(chip_svg, chip_h);
    let chip_rect = egui::Rect::from_min_size(
        egui::pos2(rect.right() - chip_w - pill_inset,
                   rect.top() + pill_inset),
        egui::vec2(chip_w, chip_h),
    );
    paint_pill_svg(ui, chip_svg, chip_rect);

    // 3. Click zones — split the chip rect down the middle. The SVGs
    // were authored so the two halves are roughly equal-width either
    // side of the slash, so a 50/50 split on the rect is close enough
    // for hit-testing without parsing the slash geometry.
    let mid_x = chip_rect.center().x;
    let easy_zone = egui::Rect::from_min_max(
        chip_rect.left_top(),
        egui::pos2(mid_x, chip_rect.bottom()),
    );
    let adv_zone = egui::Rect::from_min_max(
        egui::pos2(mid_x, chip_rect.top()),
        chip_rect.right_bottom(),
    );
    let easy_resp = ui.interact(easy_zone,
        ui.id().with("mode_pill_easy"), egui::Sense::click());
    let adv_resp = ui.interact(adv_zone,
        ui.id().with("mode_pill_adv"), egui::Sense::click());
    if easy_resp.clicked() { *do_set_mode = Some(settings::UiMode::Easy); }
    if adv_resp.clicked()  { *do_set_mode = Some(settings::UiMode::Advanced); }
    if easy_resp.hovered() || adv_resp.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
}

// ── Sub-patch editor windows ──────────────────────────────────────────────────

// Auto-placement step for newly-pinned modules (viewer PINNED_MOD_H + gap).
const PINNED_STEP: f32 = 108.0;
const PINNED_PAD: f32  = 4.0;

/// On-disk representation of a single sub-patch (.fxsp). Distinct from the
/// top-level patch format (.fxp) so the save/load dialog filters cleanly.
#[derive(serde::Serialize, serde::Deserialize)]
pub(crate) struct SubPatchFile {
    pub(crate) version: u32,
    pub(crate) sub_patch: UiSubPatch,
}

/// Short human label for a KB/M pin (fallback when no icon, or for the chord
/// preview). Strips the `key_`/`mouse_`/`scroll_` prefix and prettifies a few.
fn kbm_pin_label(pin: &str) -> String {
    match pin {
        "mouse_left" => "LMB".into(),
        "mouse_right" => "RMB".into(),
        "mouse_middle" => "MMB".into(),
        "mouse_back" => "MB4".into(),
        "mouse_forward" => "MB5".into(),
        "scroll_up" => "Scroll↑".into(),
        "scroll_down" => "Scroll↓".into(),
        "key_pageup" => "PgUp".into(),
        "key_pagedown" => "PgDn".into(),
        "key_insert" => "Ins".into(),
        "key_delete" => "Del".into(),
        "key_printscreen" => "PrtSc".into(),
        "key_pause" => "Pause".into(),
        "key_space" => "Space".into(),
        "key_enter" => "Enter".into(),
        "key_backspace" => "Bksp".into(),
        "key_escape" => "Esc".into(),
        "key_capslock" => "Caps".into(),
        "key_backtick" => "`".into(),
        "key_arrowup" => "↑".into(),
        "key_arrowdown" => "↓".into(),
        "key_arrowleft" => "←".into(),
        "key_arrowright" => "→".into(),
        _ => {
            let s = pin.strip_prefix("key_").or_else(|| pin.strip_prefix("mouse_"))
                .unwrap_or(pin);
            let s = s.strip_prefix("num").unwrap_or(s);
            let mut cs = s.chars();
            cs.next().map(|f| f.to_uppercase().collect::<String>() + cs.as_str())
                .unwrap_or_else(|| s.to_string())
        }
    }
}

/// Short textual token for a gamepad pin used in the legend bar when no glyph
/// is available under the active skin (sticks/triggers don't always have a
/// directional SVG). Keeps the hint readable as a fallback.
fn gp_pin_token(pin: &str) -> &'static str {
    match pin {
        "left_stick" | "left_stick_left" | "left_stick_up"
            | "left_stick_horizontal" | "left_stick_vertical" => "LS",
        "right_stick" => "RS",
        "dpad_up" | "dpad_left" | "dpad_down" | "dpad_right"
            | "dpad" | "dpad_horizontal" | "dpad_vertical" => "Dpad",
        "btn_south" => "A", "btn_east" => "B", "btn_west" => "X", "btn_north" => "Y",
        "btn_lb" => "LB", "btn_rb" => "RB",
        "left_trigger" => "LT", "right_trigger" => "RT",
        "btn_ls" => "LS▾", "btn_rs" => "RS▾",
        "btn_start" => "Start", "btn_back" => "Back",
        _ => "•",
    }
}

/// Rasterize a KB/M pin's SVG icon to a cached texture (white, transparent bg).
fn kbm_cell_texture(ctx: &egui::Context, skin: crate::canvas::remapper_icons::Skin, pin: &str)
    -> Option<egui::TextureHandle>
{
    let bytes = crate::canvas::remapper_icons::pin_svg(skin, pin)?;
    let size_px = (26.0 * ctx.pixels_per_point()).round() as u32;
    let cache_key = egui::Id::new(("kbm_picker_icon", bytes.as_ptr() as usize, size_px));
    if let Some(tex) = ctx.data(|d| d.get_temp::<egui::TextureHandle>(cache_key)) {
        return Some(tex);
    }
    let text = std::str::from_utf8(bytes).ok()?;
    let img = crate::canvas::viewer::rasterize_svg_recolored(
        text, size_px, size_px, "override", egui::Color32::TRANSPARENT)?;
    let handle = ctx.load_texture(
        format!("kbm_picker_icon_{:p}", bytes.as_ptr()), img, egui::TextureOptions::LINEAR);
    ctx.data_mut(|d| d.insert_temp(cache_key, handle.clone()));
    Some(handle)
}

/// Clear the transient capture/learn state on every remapper-family inner node
/// of a sub-patch. These params (`ui_phase`, `draft_input`, `draft_output`,
/// `_pressed_prev`, `_nav_capture_armed`, `_nav_arm_idle`, `_tp_click_mode`,
/// `_tp_zones`) describe an in-progress capture gesture, not saved configuration
/// — they must never carry across a patch open or app launch (otherwise a
/// half-captured chord from a previous session blocks starting a fresh Learn).
/// The committed `mappings` array is left untouched.
pub(crate) fn clear_transient_capture_state(sp: &mut UiSubPatch) {
    const TRANSIENT: &[&str] = &[
        "ui_phase", "draft_input", "draft_output", "_pressed_prev",
        "_nav_capture_armed", "_nav_arm_idle", "_nav_act_learn",
        "_nav_act_special", "_nav_act_add", "_nav_act_clear",
        "_tp_click_mode", "_tp_zones",
        // Gyro Lean per-side capture transients.
        "_lean_left_phase", "_lean_left_draft", "_lean_left_pressed_prev",
        "_lean_left_armed", "_lean_left_arm_idle",
        "_nav_act_learn_left", "_nav_act_special_left", "_nav_act_add_left",
        "_nav_act_clear_left",
        "_lean_right_phase", "_lean_right_draft", "_lean_right_pressed_prev",
        "_lean_right_armed", "_lean_right_arm_idle",
        "_nav_act_learn_right", "_nav_act_special_right", "_nav_act_add_right",
        "_nav_act_clear_right",
    ];
    for (_, node_ref) in sp.snarl.nodes_ids_data_mut() {
        let node = &mut node_ref.value;
        if matches!(node.module_id.as_str(),
            "module.remapper" | "module.map_action" | "module.automap_combiner"
            | "processing.gyro_3dof")
        {
            for k in TRANSIENT { node.params.remove(*k); }
        }
    }
}

/// Clear transient capture state across every sub-patch in a canvas (the outer
/// snarl's nodes that carry a `.subpatch`). Used on patch/workspace load.
pub(crate) fn clear_canvas_capture_state(canvas: &mut crate::canvas::Canvas) {
    for (_, node_ref) in canvas.snarl.nodes_ids_data_mut() {
        if let Some(sp) = node_ref.value.subpatch.as_mut() {
            clear_transient_capture_state(sp);
        }
    }
}

pub(crate) fn save_subpatch_file(sp: &UiSubPatch) -> Option<std::path::PathBuf> {
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

pub(crate) fn load_subpatch_file() -> Option<UiSubPatch> {
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
    let active = app.active_tab;
    // Fast path: no editors and no pending open request → nothing to do.
    // Called every frame from update(), so skip all per-frame work in the
    // common case (empty workspace / no editor open).
    if app.sub_patch_editors.is_empty() && app.tabs[active].canvas.pending_edit_subpatch.is_none() {
        return;
    }
    // Snapshot once so the inner-canvas show call can borrow this immutably while
    // `app` is borrowed mutably for sub-patch editor state.
    // NOTE: `live_signals` is NOT cloned here — `app.last_signals` is already
    // a fresh per-frame clone from `proc_device_signals` (see update()), and
    // we borrow it inside the closure. Saves one HashMap clone per editor
    // frame at large patches.
    let panic_shortcut = app.panic_shortcut.clone();
    let device_defaults_inner = crate::canvas::DeviceParamDefaults {
        stick_deadzone: app.settings.default_stick_deadzone,
        gyro_mult: app.settings.default_gyro_mult,
        mouse_sensitivity: app.settings.default_mouse_sensitivity,
    };

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
                last_synced_parent_gen: None,
                last_inner_gen: None,
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
            puffin::profile_scope!("editor_presync");
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
                .map(|sp| sp.iter_module_pins().map(|(_, m)| m.inner_node_id).collect())
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

        puffin::profile_scope!("editor_viewport_show");
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
                    puffin::profile_scope!("editor_inner_canvas_show");
                    // Borrow `app.last_signals` directly (no clone). For
                    // device_rates we hold a short-lived read guard across
                    // the show call so it borrows the underlying map rather
                    // than cloning it. Canvas::show only reads, never
                    // touches the RwLock itself, so no deadlock risk.
                    let empty = std::collections::HashMap::new();
                    let guard = app.device_rates.read();
                    let device_rates_ref: &std::collections::HashMap<String, u32> =
                        match &guard { Ok(r) => r, Err(_) => &empty };
                    let _ = inner_canvas.show(
                        descriptors, live_device_ids, &app.last_signals,
                        &panic_shortcut, devices, device_rates_ref,
                        device_defaults_inner, ui, automap_parent, None,
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
                crate::canvas::migrate_loaded_snarl(&mut inner_canvas.snarl);
            }
        }

        // Handle pin/unpin.
        if let Some((inner_id, element_id, src_size)) = inner_canvas.pending_expose_module.take() {
            let any_existing = match parent_editor_idx {
                None    => app.tabs[active].canvas.snarl.get_node(node_id),
                Some(p) => app.sub_patch_editors[p].canvas.snarl.get_node(node_id),
            }.and_then(|n| n.subpatch.as_ref())
             .map(|sp| sp.has_module_pin_for(inner_id.0))
             .unwrap_or(false);
            let unpin = any_existing && element_id == "default";
            if unpin {
                let node_opt = match parent_editor_idx {
                    None    => app.tabs[active].canvas.snarl.get_node_mut(node_id),
                    Some(p) => app.sub_patch_editors[p].canvas.snarl.get_node_mut(node_id),
                };
                if let Some(node) = node_opt {
                    if let Some(sp) = node.subpatch.as_mut() {
                        sp.remove_module_pins_for(inner_id.0);
                    }
                    if node.subpatch.as_ref().map(|sp| sp.is_layout_empty()).unwrap_or(true) {
                        node.extra.layout_unlocked = false;
                    }
                }
            } else {
                let init_size = if src_size[0] >= 1.0 && src_size[1] >= 1.0 { src_size } else { [220.0, 100.0] };
                let next_y = match parent_editor_idx {
                    None    => app.tabs[active].canvas.snarl.get_node(node_id),
                    Some(p) => app.sub_patch_editors[p].canvas.snarl.get_node(node_id),
                }.and_then(|n| n.subpatch.as_ref())
                 .map(|sp| sp.module_pins_bottom_y())
                 .unwrap_or(0.0);
                let node_opt = match parent_editor_idx {
                    None    => app.tabs[active].canvas.snarl.get_node_mut(node_id),
                    Some(p) => app.sub_patch_editors[p].canvas.snarl.get_node_mut(node_id),
                };
                if let Some(node) = node_opt {
                    if let Some(sp) = node.subpatch.as_mut() {
                        sp.push_module_pin(ExposedModule {
                            inner_node_id: inner_id.0,
                            element_id,
                            pos: [PINNED_PAD, next_y + PINNED_PAD],
                            size: init_size,
                            text_override: None,
                            switch_override: None,
                            graph_override: None,
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
        // Gated on inner canvas mutation: if the editor didn't mutate
        // anything this frame (no user input, no structural change),
        // there's nothing new to write back — the parent's sp.snarl
        // already matches what we'd write. Skip the full snarl clone
        // entirely on idle frames. `last_inner_gen` is bumped any time
        // the inner canvas mutates (push_undo/push_snapshot/undo/redo).
        let cur_inner_gen = app.sub_patch_editors[i].canvas.mutation_gen;
        let prev_inner_gen = app.sub_patch_editors[i].last_inner_gen;
        if prev_inner_gen != Some(cur_inner_gen) {
            puffin::profile_scope!("editor_writeback");
            let inner_snarl = app.sub_patch_editors[i].canvas.snarl.clone();
            let node_opt = match parent_editor_idx {
                None    => app.tabs[active].canvas.snarl.get_node_mut(node_id),
                Some(p) => app.sub_patch_editors[p].canvas.snarl.get_node_mut(node_id),
            };
            if let Some(node) = node_opt {
                if let Some(sp) = node.subpatch.as_mut() { *sp.snarl = inner_snarl; }
            }
            app.sub_patch_editors[i].last_inner_gen = Some(cur_inner_gen);
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
                last_synced_parent_gen: None,
                last_inner_gen: None,
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



