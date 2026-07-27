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

/// The only polling rates we expose: each is exactly 1000/N for N = 1..8 ms,
/// because the HIDMaestro XUSB companion's input-pump period is whole-ms (so the
/// virtual Xbox 360 delivers XInput at exactly these rates; anything in between
/// would round to one of them on the driver side). Descending = ms ascending.
pub const POLLING_HZ_STEPS: [u32; 8] = [1000, 500, 333, 250, 200, 167, 143, 125];

/// Snap an arbitrary Hz to the nearest exposed step in [`POLLING_HZ_STEPS`].
/// Used to migrate older saved values to a valid step on load.
pub fn snap_polling_hz(hz: u32) -> u32 {
    POLLING_HZ_STEPS[polling_hz_to_index(hz)]
}

/// Index into [`POLLING_HZ_STEPS`] of the step nearest `hz`. The settings
/// slider drives this index (0..=7) rather than the raw Hz, so the handle is
/// evenly spaced and snaps to a valid step *while* dragging instead of only on
/// release. `POLLING_HZ_STEPS` is descending, so index 0 = fastest (1000 Hz).
pub fn polling_hz_to_index(hz: u32) -> usize {
    POLLING_HZ_STEPS
        .iter()
        .enumerate()
        .min_by_key(|(_, &step)| step.abs_diff(hz))
        .map(|(i, _)| i)
        .unwrap_or(0)
}

/// Hz value for a (clamped) step index. Inverse of [`polling_hz_to_index`].
pub fn polling_hz_from_index(idx: usize) -> u32 {
    POLLING_HZ_STEPS[idx.min(POLLING_HZ_STEPS.len() - 1)]
}

pub const SAMPLE_RATE_HZ_MIN: u32 = 500;
pub const SAMPLE_RATE_HZ_MAX: u32 = 8000;
pub const SAMPLE_RATE_HZ_DEFAULT: u32 = 2000;

fn default_polling_hz() -> u32 { POLLING_HZ_DEFAULT }
fn default_sample_rate_hz() -> u32 { SAMPLE_RATE_HZ_DEFAULT }
fn default_true() -> bool { true }
fn default_deadzone() -> f32 { 0.1 }
fn default_gyro_mult() -> f32 { 1.0 }
// 100, not 1: the sink multiplies raw per-tick deltas, and at ×1 the cursor
// barely moves — first-run users read that as "mouse output doesn't work".
fn default_mouse_sens() -> f32 { 100.0 }
// Neutral rumble forwarding: full 0..1 band, linear curve — game rumble
// passes through unshaped until the user dials in a preference.
fn default_rumble_floor() -> f32 { crate::canvas::header_controls::RUMBLE_DEF_FLOOR }
fn default_rumble_max() -> f32 { crate::canvas::header_controls::RUMBLE_DEF_MAX }
fn default_rumble_exp() -> f32 { crate::canvas::header_controls::RUMBLE_DEF_EXP }
fn default_theme() -> Theme { Theme::Dark }
fn default_contrast() -> f32 { 0.0 }
fn default_see_through_alpha() -> f32 { 0.55 }
fn default_pin_shortcut() -> PinShortcut { PinShortcut::default() }
fn default_cursor_max_speed() -> f32 { 4000.0 }
fn default_cursor_accel() -> f32 { 2.0 }
fn default_pin_guide_double_tap() -> bool { true }
fn default_config_via_guide() -> bool { true }
fn default_config_guide_double_tap() -> bool { true }
fn default_chord_mode() -> String { "down".to_string() }
fn default_chord_gap_ms() -> f32 { 200.0 }
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
        let key_raw = self.key.as_deref().unwrap_or("…");
        let key = match key_raw { "Backtick" => "~", other => other };
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
    /// Optional user 3D-controller-models directory. Same folder structure as
    /// the bundled `app/assets/models` (`<Name>/info.txt` + `.obj` files +
    /// optional `colors.fxcol`); a same-named folder overrides the bundled
    /// model. `None`/empty = bundled models only.
    #[serde(default)]
    pub user_models_dir: Option<String>,
    #[serde(default = "default_polling_hz")]
    pub polling_hz: u32,
    #[serde(default = "default_sample_rate_hz")]
    pub sample_rate_hz: u32,
    #[serde(default = "default_true")]
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
    /// Default rumble-forwarding shape for virtual pad sinks whose node params
    /// don't override it: output band `floor..max` plus response exponent.
    /// Neutral out of the box (full band, linear curve); the node widget's
    /// double-click reset also returns to these values.
    #[serde(default = "default_rumble_floor")]
    pub default_rumble_floor: f32,
    #[serde(default = "default_rumble_max")]
    pub default_rumble_max: f32,
    #[serde(default = "default_rumble_exp")]
    pub default_rumble_exp: f32,
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
    /// Legacy: the controller Guide/PS button once toggled the pin. Superseded
    /// by `config_via_guide` (the Guide button now summons the config overlay by
    /// default). Kept for serde back-compat; no longer wired.
    #[serde(default)]
    pub pin_via_guide: bool,
    /// Legacy: the controller Guide button once summoned the config overlay via
    /// a dedicated background watcher. Superseded by the user-assignable
    /// `config_overlay_chord` (detected globally with a press mode). Kept for
    /// serde back-compat; no longer wired.
    #[serde(default = "default_config_via_guide")]
    pub config_via_guide: bool,
    /// Legacy companion to `config_via_guide` (double-tap requirement). No longer
    /// wired — the config chord carries its own press mode now.
    #[serde(default = "default_config_guide_double_tap")]
    pub config_guide_double_tap: bool,
    /// Legacy companion to `config_via_guide` (optional held chord button). No
    /// longer wired — the config chord is a full combo now.
    #[serde(default)]
    pub config_guide_chord: Option<String>,
    /// Default nav-mode state for newly-seen gamepads. Per-device runtime
    /// overrides live in `FlexInputApp::gamepad_nav.mode` (not persisted).
    /// When on, the controller drives FlexInput's own UI (with mapped output
    /// suppressed) while FlexInput holds focus.
    #[serde(default)]
    pub gamepad_ui_nav_default: bool,
    /// Top speed (px/s at full right-stick deflection) of the gamepad-nav
    /// cursor. The actual speed follows an accelerated curve up to this cap.
    #[serde(default = "default_cursor_max_speed")]
    pub cursor_max_speed: f32,
    /// Acceleration exponent for the cursor speed curve: speed scales with
    /// deflection^cursor_accel. >1 = slow start, fast late; 1 = linear.
    #[serde(default = "default_cursor_accel")]
    pub cursor_accel: f32,
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
    /// When true, HIDMaestro virtual controllers persist in the system after
    /// FlexInput closes (or crashes), and are reclaimed/reused on the next
    /// launch instead of re-created. Desirable when a game is running and must
    /// not lose its gamepad across an app restart/update. When false (default),
    /// virtual devices are removed on app close/crash and any orphans are
    /// cleaned up on startup. Only affects the HIDMaestro backend; ViGEm pads
    /// are always transient.
    #[serde(default)]
    pub persist_virtual_devices: bool,
    /// Master switch for HidHide masking. When on, FlexInput hides every physical
    /// HID controller currently remapped to a virtual output, so games see only the
    /// virtual pad (not the original). `None` = auto = **on when the HidHide driver
    /// is installed** (the requested default), off otherwise; `Some(x)` is an
    /// explicit user choice. Only masks HID-class pads (DS4 / DualSense / Switch) —
    /// the XInput/XUSB face of Xbox controllers can't be hidden. The elevated helper
    /// applies it and always clears it on app exit, so a closed app never leaves
    /// controllers hidden. Resolve the effective value with
    /// `hide_originals.unwrap_or(hidhide_installed)`.
    #[serde(default)]
    pub hide_originals: Option<bool>,
    /// Render backend selection, applied at startup in `app/src/main.rs`
    /// (changing it requires an app restart). Auto = Vulkan except when the
    /// machine's GPU is AMD on Windows, where the Vulkan swapchain stalls for
    /// seconds on resize/restore-from-minimize (groundtruthed on Win11 26H1 +
    /// Radeon, 2026-07) — those get OpenGL. The `WGPU_BACKEND` env var
    /// overrides this setting (dev escape hatch).
    #[serde(default)]
    pub renderer: RendererChoice,
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
    /// Optional gamepad button combo (canonical pin ids, e.g. `["btn_lb",
    /// "btn_rb", "btn_back"]`) that toggles SEE-THROUGH mode. Learned via a
    /// chord-capture button in Settings. None = unassigned.
    #[serde(default)]
    pub seethrough_chord: Option<Vec<String>>,
    /// Optional gamepad button combo that toggles PANIC mode. None = unassigned.
    #[serde(default)]
    pub panic_chord: Option<Vec<String>>,
    /// Optional gamepad button combo that toggles the info OVERLAY.
    /// None = unassigned.
    #[serde(default)]
    pub overlay_chord: Option<Vec<String>>,
    /// Optional gamepad button combo that toggles the PIN (always-on-top). The
    /// pin's old Guide binding moved here so the user picks their own button /
    /// combo. Focus-gated like the other chords. None = unassigned.
    #[serde(default)]
    pub pin_chord: Option<Vec<String>>,
    /// Optional gamepad button combo that toggles the CONFIG overlay. Detected
    /// globally by the background watcher (fires even while a game holds focus),
    /// so it's the primary controller path to summon the overlay mid-play.
    /// None = unassigned.
    #[serde(default)]
    pub config_overlay_chord: Option<Vec<String>>,
    /// Press mode for each gamepad shortcut chord above (`"down"` = on press,
    /// `"long"` = hold for the gap, `"double"` = double-tap within the gap).
    /// Mirrors the remapper card press modes; see `gamepad_nav::chord_fire`.
    #[serde(default = "default_chord_mode")]
    pub seethrough_chord_mode: String,
    #[serde(default = "default_chord_mode")]
    pub panic_chord_mode: String,
    #[serde(default = "default_chord_mode")]
    pub overlay_chord_mode: String,
    #[serde(default = "default_chord_mode")]
    pub pin_chord_mode: String,
    #[serde(default = "default_chord_mode")]
    pub config_overlay_chord_mode: String,
    /// Time gap (ms) each shortcut chord's press mode reads: the hold duration
    /// for `long`, the max inter-tap gap for `double`. Ignored by `down`.
    #[serde(default = "default_chord_gap_ms")]
    pub seethrough_chord_gap_ms: f32,
    #[serde(default = "default_chord_gap_ms")]
    pub panic_chord_gap_ms: f32,
    #[serde(default = "default_chord_gap_ms")]
    pub overlay_chord_gap_ms: f32,
    #[serde(default = "default_chord_gap_ms")]
    pub pin_chord_gap_ms: f32,
    #[serde(default = "default_chord_gap_ms")]
    pub config_overlay_chord_gap_ms: f32,
    /// Whether the info overlay was visible on exit — restored on launch
    /// (mirrors `see_through_active`; the live state lives in a ctx temp
    /// slot that `update()` syncs back here).
    #[serde(default)]
    pub overlay_visible: bool,
    /// Global keyboard chord toggling the info overlay's visibility. Mirrors
    /// `pin_shortcut` — RegisterHotKey under the hood (see `pin_hotkey.rs`).
    #[serde(default = "default_overlay_shortcut")]
    pub overlay_shortcut: PinShortcut,
    /// Global keyboard chord toggling the info overlay's EDIT mode (the
    /// layout-editing toolbar). Own RegisterHotKey id (`HOTKEY_ID_OVERLAY_EDIT`);
    /// mirrors `overlay_shortcut`. Turning edit on also makes the overlay visible.
    #[serde(default = "default_edit_overlay_shortcut")]
    pub edit_overlay_shortcut: PinShortcut,
    /// Repaint rate of the info overlay while it's visible. Deliberately
    /// separate from `bg_repaint_hz`: the overlay animates on top of a game,
    /// where the low background cadence would look terrible. Range
    /// OVERLAY_FPS_MIN..=MAX.
    #[serde(default = "default_overlay_fps")]
    pub overlay_fps: u32,
    /// Whether the CONFIG overlay was visible on exit — restored on launch
    /// (mirrors `overlay_visible`; live state lives in a ctx temp slot synced
    /// back by `update()`).
    #[serde(default)]
    pub config_overlay_visible: bool,
    /// Global keyboard chord toggling the CONFIG overlay's visibility. Own
    /// RegisterHotKey id (`HOTKEY_ID_CONFIG`); shares the overlay frame rate.
    #[serde(default = "default_config_overlay_shortcut")]
    pub config_overlay_shortcut: PinShortcut,
    /// Config overlay: when true, the focused pin's input passes through to the
    /// game the whole time it's focused (navigating drives the game). When false
    /// (default), input only passes while a pin is actually BEING TWEAKED
    /// (gamepad-editing or mouse-dragging) — so plain navigation is fully
    /// suppressed. Toggled from the overlay top bar.
    #[serde(default)]
    pub config_overlay_passthrough_default: bool,
    /// When true, the see-through / panic gamepad combos above only fire while
    /// the driving gamepad is in UI-navigation mode (so the same buttons stay
    /// free for in-game mappings otherwise). When false, they fire whenever
    /// FlexInput is focused and a nav-eligible gamepad is connected.
    #[serde(default = "default_true")]
    pub gamepad_chords_nav_only: bool,
    /// Repaint rate applied while the window is unfocused / minimized.
    /// Focused window always paints at vsync. Range BG_REPAINT_HZ_MIN..=MAX.
    #[serde(default = "default_bg_repaint_hz")]
    pub bg_repaint_hz: u32,
    /// Master switch for the Virtual Keyboard & Mouse "physical mouse
    /// suppression" heuristic: when on, the virtual mouse briefly yields if it
    /// sees the real cursor move on its own, so a stick-driven cursor doesn't
    /// fight a physical mouse on the desktop. Default on. Note: suppression is
    /// ALWAYS forced off in "mixed mode" (a virtual gamepad active alongside the
    /// keyboard/mouse) regardless of this setting, since the heuristic misfires
    /// in games that warp the cursor. See `flexinput_virtual` suppression globals.
    #[serde(default = "default_true")]
    pub mouse_suppression_enabled: bool,
    /// How long (ms) a detected physical-mouse move blocks the virtual mouse.
    /// Lower = faster recovery after a stray cursor event. Range 50..=2000.
    #[serde(default = "default_mouse_suppress_release_ms")]
    pub mouse_suppress_release_ms: u32,
    /// EXPERIMENTAL: braid virtual gamepad vs keyboard/mouse output — phase-
    /// offset WHEN each side's packet lands (gamepad slot / mouse slot per
    /// period) instead of muting either, to probe games whose input arbiter
    /// behaves differently under simultaneous mixed output. Neither stream is
    /// zeroed, so an idle mouse never chops the pad. Off by default. See the I/O
    /// thread's `route_virtual_devices` block + flexinput-virtual braid clock.
    #[serde(default)]
    pub mixed_braid_enabled: bool,
    /// Braid pacing as a per-lane submit rate (Hz). `0` = real-time (alternate
    /// as fast as the output threads tick; packets still never coincide). Other
    /// values throttle each lane: 500 / 250 / 125 Hz. See `BRAID_RATE_STEPS`.
    #[serde(default = "default_mixed_braid_rate_hz")]
    pub mixed_braid_rate_hz: u32,
    /// Route EVERY controller through the SDL backend instead of the native
    /// gilrs/raw-HID paths. A diagnostic switch: it lets a pad with a native
    /// parser be read via SDL and compared against its native behaviour, and
    /// surfaces SDL-only devices uniformly. Changes device IDs (`gilrs:…` →
    /// `sdl:…`), so wiring doesn't follow the toggle. Off by default.
    #[serde(default)]
    pub sdl_all_pads: bool,
}

pub const MOUSE_SUPPRESS_RELEASE_MS_MIN: u32 = 50;
pub const MOUSE_SUPPRESS_RELEASE_MS_MAX: u32 = 2000;
pub const MOUSE_SUPPRESS_RELEASE_MS_DEFAULT: u32 = 500;
fn default_mouse_suppress_release_ms() -> u32 { MOUSE_SUPPRESS_RELEASE_MS_DEFAULT }

/// Braid pacing steps shown by the slider, left→right. `0` is the "Real-time"
/// step (fastest, lowest latency — limited only by the polling/mouse rate); the
/// rest are explicit per-lane submit rates. ~10 ms latency is the practical
/// playability limit, so the slowest step is 125 Hz (8 ms cycle).
pub const BRAID_RATE_STEPS: [u32; 4] = [0, 500, 250, 125];
pub const MIXED_BRAID_RATE_HZ_DEFAULT: u32 = 0; // real-time
fn default_mixed_braid_rate_hz() -> u32 { MIXED_BRAID_RATE_HZ_DEFAULT }

/// Index into [`BRAID_RATE_STEPS`] of the step nearest `hz` (0 matches the
/// real-time step exactly). Used to drive the stepped slider.
pub fn braid_rate_to_index(hz: u32) -> usize {
    BRAID_RATE_STEPS
        .iter()
        .position(|&s| s == hz)
        .unwrap_or(0)
}

/// Label for a braid step value: "Real-time" for 0, else "<hz> Hz".
pub fn braid_rate_label(hz: u32) -> String {
    if hz == 0 { "Real-time".to_string() } else { format!("{hz} Hz") }
}

/// Render backend selection (see `AppSettings::renderer`). Read once at
/// startup before the window exists; changing it requires a restart.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RendererChoice {
    /// Vulkan, except AMD GPUs on Windows get OpenGL (slow Vulkan swapchain
    /// reconfigure: multi-second resize/restore stalls).
    #[default]
    Auto,
    /// Force Vulkan.
    Vulkan,
    /// Force OpenGL.
    OpenGl,
    /// Force DirectX 12 (Windows only). Presents through a DirectComposition
    /// swapchain, the only Windows path whose per-pixel window alpha works on
    /// AMD — their Vulkan win32 surface reports COMPOSITE_ALPHA_OPAQUE only,
    /// so see-through mode can never work there (NVIDIA Vulkan only works by
    /// driver quirk).
    Dx12,
}

impl RendererChoice {
    pub fn label(self) -> &'static str {
        match self {
            Self::Auto => "Auto",
            Self::Vulkan => "Vulkan",
            Self::OpenGl => "OpenGL",
            Self::Dx12 => "DirectX 12",
        }
    }
}

/// The renderer choice alone, for `app/src/main.rs` to pick wgpu backends
/// before the window exists (the full settings load happens again later in
/// `FlexInputApp::new`).
pub fn startup_renderer_choice() -> RendererChoice {
    load_settings().renderer
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

/// Repaint rate (Hz) applied ONLY while the window is unfocused or
/// minimized. The focused window always paints at vsync — the user is
/// actively tweaking and visual smoothness matters there. Background
/// rate caps the wasted GPU/CPU when the user is playing a game or
/// using another app while FlexInput sits in the tray.
///
/// Range 1–30 Hz. 10 Hz default: smooth enough to glance at, low
/// enough to drop CPU meaningfully.
pub const BG_REPAINT_HZ_MIN: u32 = 1;
pub const BG_REPAINT_HZ_MAX: u32 = 30;
pub const BG_REPAINT_HZ_DEFAULT: u32 = 10;
fn default_bg_repaint_hz() -> u32 { BG_REPAINT_HZ_DEFAULT }

/// Info-overlay repaint rate bounds. The floor keeps signal glow readable;
/// the ceiling matches common high-refresh monitors without letting the
/// setting turn into a busy-loop.
pub const OVERLAY_FPS_MIN: u32 = 10;
pub const OVERLAY_FPS_MAX: u32 = 144;
pub const OVERLAY_FPS_DEFAULT: u32 = 60;
fn default_overlay_fps() -> u32 { OVERLAY_FPS_DEFAULT }
fn default_overlay_shortcut() -> PinShortcut {
    // Ctrl+Shift+O — "overlay"; clear of the pin (Ctrl+Shift+P) and panic
    // (Ctrl+Backtick) defaults.
    PinShortcut { ctrl: true, shift: true, alt: false, win: false, key: Some("O".to_string()) }
}
fn default_config_overlay_shortcut() -> PinShortcut {
    // Ctrl+Shift+C — "config"; clear of pin/overlay/panic defaults.
    PinShortcut { ctrl: true, shift: true, alt: false, win: false, key: Some("C".to_string()) }
}
fn default_edit_overlay_shortcut() -> PinShortcut {
    // Ctrl+Shift+E — "edit"; clear of the other overlay/pin/panic defaults.
    PinShortcut { ctrl: true, shift: true, alt: false, win: false, key: Some("E".to_string()) }
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            user_models_dir: None,
            polling_hz: POLLING_HZ_DEFAULT,
            sample_rate_hz: SAMPLE_RATE_HZ_DEFAULT,
            keep_workspace: true,
            device_nodes_default_collapsed: true,
            default_stick_deadzone: 0.1,
            default_gyro_mult: 1.0,
            default_mouse_sensitivity: 100.0,
            default_rumble_floor: default_rumble_floor(),
            default_rumble_max: default_rumble_max(),
            default_rumble_exp: default_rumble_exp(),
            theme: Theme::Dark,
            contrast: 0.0,
            see_through_alpha: 0.55,
            see_through_active: false,
            pin_active: false,
            pin_shortcut: PinShortcut::default(),
            pin_via_guide: false,
            config_via_guide: true,
            config_guide_double_tap: true,
            config_guide_chord: None,
            gamepad_ui_nav_default: false,
            cursor_max_speed: 4000.0,
            cursor_accel: 2.0,
            pin_guide_double_tap: true,
            focus_flip_flop: true,
            pin_guide_chord: None,
            show_own_virtuals_as_physical: false,
            persist_virtual_devices: false,
            hide_originals: None,
            renderer: RendererChoice::Auto,
            on_patch_load: OnPatchLoad::Off,
            profiler_enabled: false,
            ui_mode: UiMode::Easy,
            user_presets_folder: None,
            favorite_presets: Vec::new(),
            seethrough_chord: None,
            panic_chord: None,
            overlay_chord: None,
            pin_chord: None,
            config_overlay_chord: None,
            seethrough_chord_mode: default_chord_mode(),
            panic_chord_mode: default_chord_mode(),
            overlay_chord_mode: default_chord_mode(),
            pin_chord_mode: default_chord_mode(),
            config_overlay_chord_mode: default_chord_mode(),
            seethrough_chord_gap_ms: default_chord_gap_ms(),
            panic_chord_gap_ms: default_chord_gap_ms(),
            overlay_chord_gap_ms: default_chord_gap_ms(),
            pin_chord_gap_ms: default_chord_gap_ms(),
            config_overlay_chord_gap_ms: default_chord_gap_ms(),
            overlay_visible: false,
            overlay_shortcut: default_overlay_shortcut(),
            edit_overlay_shortcut: default_edit_overlay_shortcut(),
            overlay_fps: OVERLAY_FPS_DEFAULT,
            config_overlay_visible: false,
            config_overlay_shortcut: default_config_overlay_shortcut(),
            config_overlay_passthrough_default: false,
            gamepad_chords_nav_only: true,
            bg_repaint_hz: BG_REPAINT_HZ_DEFAULT,
            mouse_suppression_enabled: true,
            mouse_suppress_release_ms: MOUSE_SUPPRESS_RELEASE_MS_DEFAULT,
            mixed_braid_enabled: false,
            mixed_braid_rate_hz: MIXED_BRAID_RATE_HZ_DEFAULT,
            sdl_all_pads: false,
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


pub fn appdata_dir() -> Option<std::path::PathBuf> {
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

fn render_attempt_path() -> Option<std::path::PathBuf> {
    let mut p = appdata_dir()?;
    p.push("render_attempt");
    Some(p)
}

/// Read the persisted renderer-cascade attempt index (see `app/src/main.rs`
/// `auto_cascade`). `None` if the file is absent/unreadable. Only consulted on a
/// GPU-recovery relaunch; a normal launch ignores and clears it, so the cascade
/// always restarts from the preferred backend on a fresh start.
pub fn read_render_attempt() -> Option<usize> {
    let p = render_attempt_path()?;
    std::fs::read_to_string(&p).ok()?.trim().parse().ok()
}

/// Persist the renderer-cascade attempt index so the next GPU-recovery relaunch
/// knows which backend to try. Best-effort.
pub fn write_render_attempt(n: usize) {
    if let Some(p) = render_attempt_path() {
        let _ = std::fs::write(p, n.to_string());
    }
}

/// Remove the renderer-cascade marker (cascade solved / reset to the preferred
/// backend). Best-effort.
pub fn clear_render_attempt() {
    if let Some(p) = render_attempt_path() {
        let _ = std::fs::remove_file(p);
    }
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
    /// Stable salt that keys this tab's canvas pan/zoom (see
    /// `Canvas::set_view_salt`). Persisted so the view stays tied to the same
    /// tab across restarts and never cross-contaminates other tabs. `0` (the
    /// `serde` default for pre-existing workspaces) means "unassigned" — the
    /// loader allocates a fresh unique salt instead.
    #[serde(default)]
    pub view_salt: u64,
    /// Screen-overlay layout (pinned module elements + decorations on the
    /// transparent info overlay). Default-empty so pre-overlay workspaces
    /// keep loading.
    #[serde(default)]
    pub overlay: crate::canvas::OverlayLayout,
    /// Config-overlay layout (curated editable tweak-pins, M3). Default-empty
    /// so pre-config workspaces keep loading.
    #[serde(default)]
    pub config: crate::canvas::OverlayLayout,
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

// ── Crash-recovery snapshot (always-on, separate from opt-in workspace) ───────
//
// Distinct from `workspace.json` on purpose:
//   • workspace.json is the *opt-in* "reopen my tabs next launch" feature,
//     gated on `keep_workspace`. A user who never enabled it still must not
//     lose work to a GPU-loss relaunch.
//   • recovery.json is written continuously (autosave-on-settle) regardless of
//     that setting, consumed exactly once on the next boot, then deleted. It
//     only ever survives if the process did not exit normally — a normal exit
//     clears it. See `FlexInputApp::write_recovery_snapshot` /
//     `take_recovery_workspace` and `app/src/main.rs`'s relaunch path.

fn recovery_path() -> Option<std::path::PathBuf> {
    let mut p = appdata_dir()?;
    p.push("recovery.json");
    Some(p)
}

/// Atomically write the crash-recovery snapshot: serialize to a sibling temp
/// file, then rename over `recovery.json`. The rename is atomic on the same
/// volume, so a crash (or GPU-loss relaunch) mid-write can never leave a
/// half-written file for the next boot to choke on.
pub fn save_recovery(ws: &PersistedWorkspace) {
    let Some(dst) = recovery_path() else { return; };
    let Ok(json) = serde_json::to_vec_pretty(ws) else { return; };
    let tmp = dst.with_extension("json.tmp");
    if std::fs::write(&tmp, &json).is_err() {
        return;
    }
    // rename() replaces the destination atomically on Windows (ReplaceFile
    // semantics) and POSIX. If it fails, drop the temp so we don't litter.
    if std::fs::rename(&tmp, &dst).is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
}

/// Load the crash-recovery snapshot if one exists (i.e. the last run did not
/// exit cleanly). Returns None when absent or unparseable.
pub fn load_recovery() -> Option<PersistedWorkspace> {
    let p = recovery_path()?;
    let bytes = std::fs::read(&p).ok()?;
    serde_json::from_slice(&bytes).ok()
}

/// Delete the crash-recovery snapshot. Called on clean exit and immediately
/// after a recovery snapshot has been consumed on boot.
pub fn delete_recovery() {
    if let Some(p) = recovery_path() {
        let _ = std::fs::remove_file(&p);
    }
}

#[cfg(test)]
mod polling_step_tests {
    use super::*;

    #[test]
    fn every_step_round_trips_through_index() {
        for (i, &hz) in POLLING_HZ_STEPS.iter().enumerate() {
            assert_eq!(polling_hz_to_index(hz), i, "hz {hz} should map to index {i}");
            assert_eq!(polling_hz_from_index(i), hz);
            assert_eq!(snap_polling_hz(hz), hz, "an exact step must snap to itself");
        }
    }

    #[test]
    fn arbitrary_values_snap_to_nearest_step() {
        // Between 250 and 333 -> nearer 250 (idx 3) below the midpoint (~291).
        assert_eq!(snap_polling_hz(280), 250);
        // Just above the 333/500 midpoint (~416) -> 500.
        assert_eq!(snap_polling_hz(450), 500);
        // A legacy saved value of 300 -> nearest step.
        assert_eq!(snap_polling_hz(300), 333);
        // Out-of-range clamps to the ends.
        assert_eq!(snap_polling_hz(50), 125);
        assert_eq!(snap_polling_hz(5000), 1000);
    }

    #[test]
    fn from_index_clamps_out_of_bounds() {
        assert_eq!(polling_hz_from_index(99), 125); // last step
    }
}
