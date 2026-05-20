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

/// App-wide visual theme. Controls the egui Visuals base + the canvas
/// node/header fill colors picked by `Canvas::new`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Theme { Dark, Light }

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
