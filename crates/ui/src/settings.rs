//! User-configurable application settings persisted to
//! `%APPDATA%\FlexInput\settings.json` (and `workspace.json` for the
//! opt-in tabs-on-relaunch feature).
//!
//! Mirrors the panic-hotkey pattern in [`crate::panic_hotkey`]: simple JSON
//! files, no external config crate. New fields default-in via
//! `#[serde(default)]` so older saved configs keep loading.

use egui_snarl::Snarl;

use crate::canvas::NodeData;

/// Polling / sample rate ranges exposed by the Settings UI.
pub const POLLING_HZ_MIN: u32 = 125;
pub const POLLING_HZ_MAX: u32 = 1000;
pub const POLLING_HZ_DEFAULT: u32 = 500;

pub const SAMPLE_RATE_HZ_MIN: u32 = 500;
pub const SAMPLE_RATE_HZ_MAX: u32 = 8000;
pub const SAMPLE_RATE_HZ_DEFAULT: u32 = 2000;

fn default_polling_hz() -> u32 { POLLING_HZ_DEFAULT }
fn default_sample_rate_hz() -> u32 { SAMPLE_RATE_HZ_DEFAULT }
fn default_true() -> bool { true }
fn default_deadzone() -> f32 { 0.1 }
fn default_gyro_mult() -> f32 { 1.0 }
fn default_mouse_sens() -> f32 { 1.0 }
fn default_theme() -> Theme { Theme::Dark }
fn default_contrast() -> f32 { 0.0 }
fn default_see_through_alpha() -> f32 { 0.55 }
fn default_pin_shortcut() -> PinShortcut { PinShortcut::default() }
fn default_pin_guide_double_tap() -> bool { true }
fn default_focus_flip_flop() -> bool { true }
fn default_ui_mode() -> UiMode { UiMode::Easy }

/// Keyboard shortcut for the always-on-top pin toggle. Mirrors
/// `PanicShortcut` in shape — kept in `settings.rs` so the full settings
/// blob round-trips through one serde struct. Empty `key` means "unassigned"
/// and disables the global hotkey.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PinShortcut {
    #[serde(default)] pub ctrl:  bool,
    #[serde(default)] pub shift: bool,
    #[serde(default)] pub alt:   bool,
    #[serde(default)] pub win:   bool,
    /// egui::Key Debug name. `None` = unassigned.
    #[serde(default)] pub key:   Option<String>,
}

impl Default for PinShortcut {
    fn default() -> Self {
        // Ctrl+Shift+P — "pin". Unlikely to collide with the panic chord
        // (Ctrl+Backtick) or common game bindings.
        Self { ctrl: true, shift: true, alt: false, win: false, key: Some("P".to_string()) }
    }
}

impl PinShortcut {
    pub fn label(&self) -> String {
        let mut parts: Vec<&str> = Vec::new();
        if self.ctrl  { parts.push("Ctrl"); }
        if self.shift { parts.push("Shift"); }
        if self.alt   { parts.push("Alt"); }
        if self.win   { parts.push("Win"); }
        let key = self.key.as_deref().unwrap_or("…");
        if parts.is_empty() { key.to_string() } else { format!("{}+{}", parts.join("+"), key) }
    }
}

/// App-wide visual theme. Controls the egui Visuals base + the canvas
/// node/header fill colors picked by `Canvas::new`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Theme { Dark, Light }

/// Which top-level UI is shown across all tabs. Global — the header
/// toggle flips every tab into the same view. Easy is the factory
/// default: a three-panel preset-driven UI that hides the snarl canvas.
/// Advanced reveals the underlying canvas exactly as it was before
/// Easy mode existed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub enum UiMode {
    #[default]
    Easy,
    Advanced,
}

#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct AppSettings {
    #[serde(default = "default_polling_hz")]
    pub polling_hz: u32,
    #[serde(default = "default_sample_rate_hz")]
    pub sample_rate_hz: u32,
    #[serde(default)]
    pub keep_workspace: bool,
    #[serde(default = "default_true")]
    pub device_nodes_default_collapsed: bool,
    /// Default `deadzone` param applied to newly-added device.source nodes.
    #[serde(default = "default_deadzone")]
    pub default_stick_deadzone: f32,
    /// Default `gyro_multiplier` param applied to newly-added device.source nodes.
    #[serde(default = "default_gyro_mult")]
    pub default_gyro_mult: f32,
    /// Default `mouse_sensitivity` param applied to newly-added keymouse sinks.
    #[serde(default = "default_mouse_sens")]
    pub default_mouse_sensitivity: f32,
    /// Selected app theme. Defaults to Dark to match existing visuals.
    #[serde(default = "default_theme")]
    pub theme: Theme,
    /// Contrast adjustment in -1..1 range. Positive lightens panel/node
    /// backgrounds (more contrast against text), negative darkens. 0 is
    /// neutral — current default.
    #[serde(default = "default_contrast")]
    pub contrast: f32,
    /// Background opacity when "see-through" mode is active. 0.0 = fully
    /// transparent, 1.0 = fully opaque. The translucent fill only affects
    /// panel/window backgrounds; nodes and connectors stay opaque.
    #[serde(default = "default_see_through_alpha")]
    pub see_through_alpha: f32,
    /// Persists the last see-through state so the app reopens in the same
    /// visual mode.
    #[serde(default)]
    pub see_through_active: bool,
    /// Persists the last always-on-top pin state.
    #[serde(default)]
    pub pin_active: bool,
    /// Keyboard chord that toggles the pin globally. Mirrors panic_shortcut
    /// — uses RegisterHotKey under the hood (see `pin_hotkey.rs`).
    #[serde(default = "default_pin_shortcut")]
    pub pin_shortcut: PinShortcut,
    /// If true, the controller Guide/PS button also toggles the pin.
    #[serde(default)]
    pub pin_via_guide: bool,
    /// If true, Guide-button activation requires a double-tap (within ~300ms);
    /// otherwise a single tap fires. Default true to avoid colliding with
    /// the Game Bar / Steam overlay's own Guide-button handling.
    #[serde(default = "default_pin_guide_double_tap")]
    pub pin_guide_double_tap: bool,
    /// When true, activating the pin remembers the previous foreground
    /// window and brings it back to the front; deactivating returns focus
    /// to FlexInput. Lets the user flip-flop between tweak and test.
    #[serde(default = "default_focus_flip_flop")]
    pub focus_flip_flop: bool,
    /// Optional chord button required IN ADDITION to the Guide button
    /// for pin activation via controller. Signal name like `"btn_lb"`
    /// or `"btn_back"`. Captured via AutoMap-style learn mode in
    /// Settings. None = no chord (Guide alone fires).
    #[serde(default)]
    pub pin_guide_chord: Option<String>,
    /// When true, FlexInput's own virtual ViGEm controllers are also
    /// listed in the bottom (physical) devices panel — useful for
    /// closing the loop and testing your patch against itself. Off by
    /// default; the virtual is normally already represented by its
    /// chip in the top panel.
    #[serde(default)]
    pub show_own_virtuals_as_physical: bool,
    /// What to do with the camera when a patch is loaded into a tab.
    #[serde(default)]
    pub on_patch_load: OnPatchLoad,
    /// Profiler toggle. When true the app spawns a `puffin_http` server on
    /// 127.0.0.1:8585 and flips `puffin::set_scopes_on(true)` so the macros
    /// scattered through the hot paths actually emit events. Connect from
    /// the standalone `puffin_viewer` GUI (`cargo install puffin_viewer`)
    /// to inspect a live flamegraph. NOT persisted — it's a dev-time tool
    /// and we don't want users accidentally leaving it on across sessions.
    #[serde(default, skip)]
    pub profiler_enabled: bool,
    /// Top-level UI mode. Easy = factory-default preset-driven view;
    /// Advanced = legacy snarl canvas + side panels. Global across tabs.
    #[serde(default = "default_ui_mode")]
    pub ui_mode: UiMode,
    /// Optional folder scanned for user-authored sub-patch presets
    /// (`*.fxsp`). Scanned in addition to the factory presets shipped
    /// under `app/assets/sub-patches/`.
    #[serde(default)]
    pub user_presets_folder: Option<std::path::PathBuf>,
    /// Ordered list of preset file paths the user has starred as
    /// favorites. Surfaced as the first category in the Easy mode
    /// preset dropdown. Order is user-controlled via drag handles.
    #[serde(default)]
    pub favorite_presets: Vec<std::path::PathBuf>,
}

/// Camera behavior immediately after a patch is loaded into a tab.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub enum OnPatchLoad {
    /// Leave the canvas view exactly as it was — no auto-pan, no auto-zoom.
    #[default]
    Off,
    /// Pan the camera to the centroid of the loaded nodes (zoom unchanged).
    Center,
    /// Pan + scale so every node is visible with a small margin.
    ZoomToFit,
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            polling_hz: POLLING_HZ_DEFAULT,
            sample_rate_hz: SAMPLE_RATE_HZ_DEFAULT,
            keep_workspace: false,
            device_nodes_default_collapsed: true,
            default_stick_deadzone: 0.1,
            default_gyro_mult: 1.0,
            default_mouse_sensitivity: 1.0,
            theme: Theme::Dark,
            contrast: 0.0,
            see_through_alpha: 0.55,
            see_through_active: false,
            pin_active: false,
            pin_shortcut: PinShortcut::default(),
            pin_via_guide: false,
            pin_guide_double_tap: true,
            focus_flip_flop: true,
            pin_guide_chord: None,
            show_own_virtuals_as_physical: false,
            on_patch_load: OnPatchLoad::Off,
            profiler_enabled: false,
            ui_mode: UiMode::Easy,
            user_presets_folder: None,
            favorite_presets: Vec::new(),
        }
    }
}

/// Apply `settings.theme` + `settings.contrast` to the egui style. Called
/// each frame from the app loop so changes take effect immediately.
///
/// Note: theme is currently forced to Dark regardless of the persisted
/// value, because light mode isn't fully plumbed through every custom-
/// painted element. The setting is preserved on disk for a future
/// proper light-mode pass.
pub fn apply_theme_and_contrast(ctx: &egui::Context, settings: &AppSettings) {
    let _ = settings.theme; // theme picker hidden in UI; dark-only for now
    let mut style: egui::Style = (*ctx.style()).clone();
    style.visuals = egui::Visuals::dark();
    let c = settings.contrast.clamp(-1.0, 1.0);
    if c.abs() > 1e-3 {
        // Contrast pushes panel/widget fills lighter (c > 0) or darker
        // (c < 0) by up to ~30 levels of brightness. We work directly on
        // the visuals fields the panel/widget chrome reads from.
        let shift = (c * 30.0).round() as i16;
        let adjust = |col: egui::Color32| -> egui::Color32 {
            let f = |v: u8| (v as i16 + shift).clamp(0, 255) as u8;
            egui::Color32::from_rgba_unmultiplied(f(col.r()), f(col.g()), f(col.b()), col.a())
        };
        style.visuals.panel_fill            = adjust(style.visuals.panel_fill);
        style.visuals.window_fill           = adjust(style.visuals.window_fill);
        style.visuals.extreme_bg_color      = adjust(style.visuals.extreme_bg_color);
        style.visuals.faint_bg_color        = adjust(style.visuals.faint_bg_color);
        style.visuals.code_bg_color         = adjust(style.visuals.code_bg_color);
        style.visuals.widgets.noninteractive.bg_fill = adjust(style.visuals.widgets.noninteractive.bg_fill);
        style.visuals.widgets.noninteractive.weak_bg_fill = adjust(style.visuals.widgets.noninteractive.weak_bg_fill);
        style.visuals.widgets.inactive.bg_fill = adjust(style.visuals.widgets.inactive.bg_fill);
        style.visuals.widgets.inactive.weak_bg_fill = adjust(style.visuals.widgets.inactive.weak_bg_fill);
    }

    // See-through mode is NOT applied here on the global style — that
    // would tint sub-windows (Settings, Calibration, Subpatch editors)
    // and bleed into node bodies via `widgets.*` and `extreme_bg_color`.
    // Instead the canvas CentralPanel applies the alpha directly on its
    // own `egui::Frame` (see `app.rs` central-panel block), so only the
    // backdrop behind the snarl nodes goes translucent. Nothing else.
    ctx.set_style(style);
}


fn appdata_dir() -> Option<std::path::PathBuf> {
    let appdata = std::env::var_os("APPDATA")?;
    let mut p = std::path::PathBuf::from(appdata);
    p.push("FlexInput");
    let _ = std::fs::create_dir_all(&p);
    Some(p)
}

fn settings_path() -> Option<std::path::PathBuf> {
    let mut p = appdata_dir()?;
    p.push("settings.json");
    Some(p)
}

fn workspace_path() -> Option<std::path::PathBuf> {
    let mut p = appdata_dir()?;
    p.push("workspace.json");
    Some(p)
}

pub fn load_settings() -> AppSettings {
    let Some(p) = settings_path() else { return AppSettings::default(); };
    let Ok(bytes) = std::fs::read(&p) else { return AppSettings::default(); };
    serde_json::from_slice(&bytes).unwrap_or_default()
}

pub fn save_settings(s: &AppSettings) {
    let Some(p) = settings_path() else { return; };
    if let Ok(json) = serde_json::to_vec_pretty(s) {
        let _ = std::fs::write(&p, json);
    }
}

// ── Workspace (opt-in tab persistence) ───────────────────────────────────────

#[derive(serde::Serialize, serde::Deserialize)]
pub struct PersistedTab {
    pub title: String,
    #[serde(default)]
    pub file_path: Option<std::path::PathBuf>,
    #[serde(default)]
    pub bound_exes: Vec<String>,
    #[serde(default)]
    pub auto_bypass: bool,
    pub snarl: Snarl<NodeData>,
    /// Path of the .fxsp preset most recently loaded into this tab's
    /// Easy-mode sub-patch. Mirrors `UiPatch.easy_preset_path` so a
    /// workspace round-trip restores the preset link.
    #[serde(default)]
    pub easy_preset_path: Option<std::path::PathBuf>,
}

#[derive(serde::Serialize, serde::Deserialize)]
pub struct PersistedWorkspace {
    pub version: u32,
    pub active_tab: usize,
    pub tabs: Vec<PersistedTab>,
}

pub fn save_workspace(ws: &PersistedWorkspace) {
    let Some(p) = workspace_path() else { return; };
    if let Ok(json) = serde_json::to_vec_pretty(ws) {
        let _ = std::fs::write(&p, json);
    }
}

pub fn load_workspace() -> Option<PersistedWorkspace> {
    let p = workspace_path()?;
    let bytes = std::fs::read(&p).ok()?;
    serde_json::from_slice(&bytes).ok()
}

pub fn delete_workspace() {
    if let Some(p) = workspace_path() {
        let _ = std::fs::remove_file(&p);
    }
}

/// Write a workspace snapshot to an arbitrary path. Used by the File menu's
/// "Save Workspace…" item so the user can keep named workspace bundles
/// (e.g. for A/B perf comparisons) separate from the auto-persisted
/// `workspace.json`.
pub fn save_workspace_to(ws: &PersistedWorkspace, path: &std::path::Path) -> std::io::Result<()> {
    let json = serde_json::to_vec_pretty(ws)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;
    std::fs::write(path, json)
}

/// Read a workspace snapshot from an arbitrary path. Returns None if the
/// file is missing or unparseable.
pub fn load_workspace_from(path: &std::path::Path) -> Option<PersistedWorkspace> {
    let bytes = std::fs::read(path).ok()?;
    serde_json::from_slice(&bytes).ok()
}
