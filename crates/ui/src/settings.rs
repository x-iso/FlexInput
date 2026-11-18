//! User-configurable application settings persisted to `settings.json` in the
//! per-user data folder (`%APPDATA%\FlexInput`, `~/.config/FlexInput` on Linux;
//! see [`flexinput_core::paths`]), and `workspace.json` for the opt-in
//! tabs-on-relaunch feature.
//!
//! Mirrors the panic-hotkey pattern in [`crate::panic_hotkey`]: simple JSON
//! files, no external config crate. New fields default-in via
//! `#[serde(default)]` so older saved configs keep loading.

use std::collections::HashMap;

use egui_snarl::Snarl;

use crate::canvas::NodeData;

/// Default polling rate. The valid rates are [`POLLING_HZ_STEPS`].
pub const POLLING_HZ_DEFAULT: u32 = 500;

/// The only polling rates we expose: 2000 Hz (0.5 ms), then exactly 1000/N for
/// N = 1..8 ms. Whole-ms periods were chosen when FlexInput's rebuilt XUSB
/// companion pumped XInput on a whole-ms timer. The stock companion instead
/// reports each frame as it's written, so the virtual Xbox 360 follows the I/O
/// rate directly; the steps stay so saved settings remain valid. Descending = ms
/// ascending. The output rate ([`AppSettings::output_hz`]) uses the same steps.
pub const POLLING_HZ_STEPS: [u32; 9] = [2000, 1000, 500, 333, 250, 200, 167, 143, 125];

/// [`AppSettings::output_hz`] value meaning "report once per poll" (the
/// default, and the only behavior before the output rate was its own setting).
pub const OUTPUT_HZ_SAME_AS_POLLING: u32 = 0;

/// A saved output rate made valid: "same as polling", or snapped to a step.
pub fn snap_output_hz(hz: u32) -> u32 {
    if hz == OUTPUT_HZ_SAME_AS_POLLING { hz } else { snap_polling_hz(hz) }
}

/// The reports-per-frame choices for display-synced output
/// ([`AppSettings::output_display_sync`]).
pub const OUTPUT_SYNC_PER_FRAME: [u32; 4] = [1, 2, 4, 8];

/// The latest a frame's first report may sit, as a share of the frame.
pub const OUTPUT_SYNC_PHASE_MAX: f32 = 0.95;

/// A saved reports-per-frame made valid: the nearest choice.
pub fn snap_output_sync_per_frame(n: u32) -> u32 {
    *OUTPUT_SYNC_PER_FRAME.iter().min_by_key(|&&c| c.abs_diff(n)).unwrap_or(&4)
}

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

/// UI scale steps exposed by the Settings slider (fraction of the OS scale,
/// 1.0 = 100%). Finer near 100% where small nudges matter, coarser at the ends.
/// The slider drives the step INDEX, so the handle snaps while dragging.
pub const UI_SCALE_STEPS: [f32; 16] = [
    0.5, 0.6, 0.7, 0.75, 0.8, 0.9, 1.0, 1.1, 1.25, 1.5, 1.75, 2.0, 2.25, 2.5, 2.75, 3.0,
];
pub const UI_SCALE_DEFAULT: f32 = 1.0;
fn default_ui_scale() -> f32 { UI_SCALE_DEFAULT }

/// Index into [`UI_SCALE_STEPS`] of the step nearest `scale`.
pub fn ui_scale_to_index(scale: f32) -> usize {
    UI_SCALE_STEPS
        .iter()
        .enumerate()
        .min_by(|(_, a), (_, b)| (*a - scale).abs().total_cmp(&(*b - scale).abs()))
        .map(|(i, _)| i)
        .unwrap_or(0)
}

/// Scale value for a (clamped) step index. Inverse of [`ui_scale_to_index`].
pub fn ui_scale_from_index(idx: usize) -> f32 {
    UI_SCALE_STEPS[idx.min(UI_SCALE_STEPS.len() - 1)]
}

/// Snap an arbitrary (e.g. hand-edited) scale to the nearest exposed step.
pub fn snap_ui_scale(scale: f32) -> f32 {
    if !scale.is_finite() { return UI_SCALE_DEFAULT; }
    ui_scale_from_index(ui_scale_to_index(scale))
}

/// The lowest polling step: a slower engine would skip input.
pub const SAMPLE_RATE_HZ_MIN: u32 = 125;
pub const SAMPLE_RATE_HZ_MAX: u32 = 8000;
pub const SAMPLE_RATE_HZ_DEFAULT: u32 = 2000;

/// How the processing sample rate is decided.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub enum SampleRateMode {
    /// [`AppSettings::sample_rate_hz`], as set.
    #[default]
    Manual,
    /// One tick per output: the least CPU.
    SameAsOutput,
    /// Two ticks per output: timers and PWM get twice the resolution for twice
    /// the CPU, and patches measure nearly the same as at any higher rate.
    TwiceOutput,
}

fn default_polling_hz() -> u32 { POLLING_HZ_DEFAULT }
fn default_sample_rate_hz() -> u32 { SAMPLE_RATE_HZ_DEFAULT }
// Four a frame: a report crossing into the next frame moves a quarter of a
// frame's motion, where at one it would move all of it.
fn default_output_sync_per_frame() -> u32 { 4 }
fn default_output_sync_phase() -> f32 { 0.5 }
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
/// Factory contrast: a little lighter than egui's stock dark panels, which
/// reads better against the text.
pub const CONTRAST_DEFAULT: f32 = 0.5;
fn default_contrast() -> f32 { CONTRAST_DEFAULT }
/// egui's dark-theme selection blue.
pub const ACCENT_DEFAULT: [u8; 3] = [0, 92, 128];
fn default_accent() -> [u8; 3] { ACCENT_DEFAULT }
/// A cool slate, for a tint that is turned up without a colour picked first.
pub const GRAY_TINT_DEFAULT: [u8; 3] = [110, 130, 170];
fn default_gray_tint() -> [u8; 3] { GRAY_TINT_DEFAULT }
/// The control font's size: the normal font at 15 points, as wide as the pills' 12-point
/// console font was.
pub const CONTROL_FONT_SIZE_DEFAULT: f32 = 15.0;
/// The control font's sizes on offer. Under 11 it can't be read on a pill; over 18 it no
/// longer fits a pill's height (it is drawn at most `CONTROL_FONT_MAX_PX`).
pub const CONTROL_FONT_SIZE_MIN: f32 = 11.0;
pub const CONTROL_FONT_SIZE_MAX: f32 = 18.0;
fn default_control_font_size() -> f32 { CONTROL_FONT_SIZE_DEFAULT }
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
/// Factory gamepad shortcuts: Home + a D-pad direction. Home is a leader, so a
/// lone Home press still reaches the game as a tap (see `guide_watcher`).
fn home_chord(dpad: &str) -> Option<Vec<String>> {
    Some(vec!["btn_guide".to_string(), dpad.to_string()])
}
fn default_overlay_chord() -> Option<Vec<String>> { home_chord("dpad_up") }
fn default_pin_chord() -> Option<Vec<String>> { home_chord("dpad_down") }
fn default_config_overlay_chord() -> Option<Vec<String>> { home_chord("dpad_right") }

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

impl AppSettings {
    /// The rate outputs actually go out at. Synced to the display, the rate
    /// that stands for: the reports a frame at the display's refresh rate (its
    /// maximum, under VRR).
    pub fn effective_output_hz(&self) -> u32 {
        if let Some(s) = self.output_sync() {
            return flexinput_virtual::display_clock::output_hz(s.per_frame);
        }
        if self.output_hz == OUTPUT_HZ_SAME_AS_POLLING { self.polling_hz } else { self.output_hz }
    }

    /// Display-synced output as the output threads take it; `None` while it's
    /// off or the platform has no display clock.
    pub fn output_sync(&self) -> Option<flexinput_virtual::display_clock::OutputSync> {
        (self.output_display_sync && flexinput_virtual::display_clock::SUPPORTED).then(|| {
            flexinput_virtual::display_clock::OutputSync {
                per_frame: snap_output_sync_per_frame(self.output_sync_per_frame),
                phase: self.output_sync_phase.clamp(0.0, OUTPUT_SYNC_PHASE_MAX),
                follow_vrr: self.output_sync_follow_vrr,
            }
        })
    }

    /// The rate the engine actually ticks at.
    pub fn effective_sample_rate_hz(&self) -> u32 {
        match self.sample_rate_mode {
            SampleRateMode::Manual => self.sample_rate_hz,
            SampleRateMode::SameAsOutput => self.effective_output_hz(),
            SampleRateMode::TwiceOutput => self.effective_output_hz() * 2,
        }.clamp(SAMPLE_RATE_HZ_MIN, SAMPLE_RATE_HZ_MAX)
    }

    /// The manual slider's floor: the polling rate, or lower if a saved rate
    /// already is, so that opening Settings never changes it.
    pub fn sample_rate_floor_hz(&self) -> u32 {
        self.polling_hz.min(self.sample_rate_hz).max(SAMPLE_RATE_HZ_MIN)
    }
}

#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct AppSettings {
    /// Optional user 3D-controller-models directory. Same folder structure as
    /// the bundled `app/assets/models` (`<Name>/info.txt` + `.obj` files +
    /// optional `colors.fxcol`); a same-named folder overrides the bundled
    /// model. `None`/empty = bundled models only.
    #[serde(default)]
    pub user_models_dir: Option<String>,
    /// Where Bluetooth Classic link keys are stored.
    ///
    /// ⭐ Worth pointing at a cloud-synced folder. A link key is the bond
    /// between a controller and the DONGLE — carrying the dongle to another PC
    /// carries the pairing with it, except that both ends must hold the key and
    /// the host's copy is this file. Synced, it follows the dongle everywhere
    /// and there is nothing to copy by hand.
    ///
    /// `None`/empty = beside the executable's working directory.
    #[serde(default)]
    pub bt_key_dir: Option<String>,
    #[serde(default = "default_polling_hz")]
    pub polling_hz: u32,
    /// How often the virtual devices report to games, separate from how often
    /// controllers are polled: below it saves work, above it gives virtual
    /// output finer steps than the controllers' own rate (output can't change
    /// faster than the processing sample rate, though). One of
    /// [`POLLING_HZ_STEPS`], or [`OUTPUT_HZ_SAME_AS_POLLING`] (default).
    #[serde(default)]
    pub output_hz: u32,
    /// Report on a grid locked to the display's frames instead of at
    /// `output_hz`: `output_sync_per_frame` reports a frame, the first
    /// `output_sync_phase` of a frame after the frame's vblank. See
    /// `flexinput_virtual::display_clock`.
    #[serde(default)]
    pub output_display_sync: bool,
    /// One of [`OUTPUT_SYNC_PER_FRAME`].
    #[serde(default = "default_output_sync_per_frame")]
    pub output_sync_per_frame: u32,
    /// 0 ..= [`OUTPUT_SYNC_PHASE_MAX`].
    #[serde(default = "default_output_sync_phase")]
    pub output_sync_phase: f32,
    /// Under VRR, follow the frames as they come. Off, report at the display's
    /// maximum rate as if it were fixed.
    #[serde(default = "default_true")]
    pub output_sync_follow_vrr: bool,
    #[serde(default = "default_sample_rate_hz")]
    pub sample_rate_hz: u32,
    #[serde(default)]
    pub sample_rate_mode: SampleRateMode,
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
    /// egui's stock dark theme; the default is [`CONTRAST_DEFAULT`] (0.5).
    #[serde(default = "default_contrast")]
    pub contrast: f32,
    /// The accent: selections, lit switches, the pills' label cells. egui's dark blue
    /// until changed.
    #[serde(default = "default_accent")]
    pub accent_color: [u8; 3],
    /// A colour the greys of panels and widgets lean toward, by [`Self::gray_tint_amount`]
    /// (0 = the plain greys).
    #[serde(default = "default_gray_tint")]
    pub gray_tint: [u8; 3],
    #[serde(default)]
    pub gray_tint_amount: f32,
    /// The font of the controls - value pills, segments, word switches, and every button and
    /// dropdown: console (monospace) or normal (the default).
    #[serde(default)]
    pub control_font_mono: bool,
    #[serde(default)]
    pub control_font_bold: bool,
    /// Its size, in the normal font's points: the other faces are drawn as wide as the normal
    /// font would be at this size (`simple_bodies::calibrate_control_fonts`).
    #[serde(default = "default_control_font_size")]
    pub control_font_size: f32,
    /// Whole-UI scale on top of the OS display scale (egui zoom factor), one of
    /// [`UI_SCALE_STEPS`]. Fonts re-rasterize at the resulting pixels-per-point,
    /// so text stays sharp at any step. Applied at startup and on the Settings
    /// window's Apply button — never live while the slider is dragged.
    #[serde(default = "default_ui_scale")]
    pub ui_scale: f32,
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
    /// suppressed) while FlexInput holds focus. On by default.
    #[serde(default = "default_true")]
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
    /// Keep a remapped controller hidden while it's momentarily absent from
    /// enumeration (Bluetooth dropout, reconnect-while-blacklisted) instead of
    /// un-hiding it. Off = legacy behaviour: the mask is recomputed purely from
    /// the devices present *right now*, so a pad that blips out is unhidden
    /// system-wide until it returns — which exposes it to a running game and
    /// makes the game's input arbiter flip between the physical pad and the
    /// virtual one. On = a pad still wired in the patch keeps its mask through
    /// the gap; the mask is still dropped when the wiring goes away, when
    /// `hide_originals` is turned off, and always on app exit (the helper
    /// clears everything at teardown), so nothing stays hidden after a close.
    /// On by default.
    #[serde(default = "default_true")]
    pub hidhide_sticky: bool,
    /// Which hidden controllers stay hidden after FlexInput exits. The helper
    /// otherwise clears its masking at teardown.
    #[serde(default)]
    pub hidhide_keep_on_exit: HidHideKeepOnExit,
    /// Per-device manual HidHide choice from the eye button on a device card,
    /// keyed by device id: `true` = hidden, `false` = visible. A device with no
    /// entry follows `hide_originals` (hidden while remapped to a virtual
    /// output). Right-clicking the eye removes the entry.
    #[serde(default)]
    pub hidhide_overrides: HashMap<String, bool>,
    /// HID instance ids the helper was told to leave hidden when FlexInput
    /// exits. Sent back as `release` on the next session's first apply, so an
    /// entry that is no longer wanted gets unhidden instead of being mistaken
    /// for a third-party one.
    #[serde(default)]
    pub hidhide_kept_ids: Vec<String>,
    /// Per-device default lightbar colour (RGB), set with the swatch on a
    /// device card and keyed by device id. Sent to the pad whenever no game or
    /// module drives its lightbar; a device with no entry keeps its own colour.
    #[serde(default)]
    pub led_defaults: HashMap<String, [u8; 3]>,
    /// Learned layouts of controllers no mapping database knows (the
    /// `sdl:raw:` devices), keyed by model (`raw_pad::profile_key`), not by
    /// device id — a layout taught once follows the pad across reconnects,
    /// tabs and presets. Mirrored for the I/O thread, whose SDL backend
    /// applies it.
    #[serde(default)]
    pub raw_pad_profiles: flexinput_core::raw_pad::RawPadProfiles,
    /// Render backend selection, applied at startup in `app/src/main.rs`
    /// (changing it requires an app restart). Auto = DirectX 12 on Windows
    /// (falling back to Vulkan, then OpenGL, if it fails to start) — Vulkan's
    /// Windows drivers proved unreliable across versions and devices; elsewhere
    /// wgpu's primary backends. The `WGPU_BACKEND` env var overrides this
    /// setting (dev escape hatch).
    #[serde(default)]
    pub renderer: RendererChoice,
    /// What to do with the camera when a patch is loaded into a tab.
    #[serde(default)]
    pub on_patch_load: OnPatchLoad,
    /// The canvas's layout grid: snapping modules to blocks, routing wires
    /// through the gaps between them.
    #[serde(default)]
    pub canvas_grid: CanvasGrid,
    /// Profiler toggle. When true the app spawns a `puffin_http` server on
    /// 127.0.0.1:8585 and flips `puffin::set_scopes_on(true)` so the macros
    /// scattered through the hot paths actually emit events. Connect from
    /// the standalone `puffin_viewer` GUI (`cargo install puffin_viewer`)
    /// to inspect a live flamegraph. NOT persisted — it's a dev-time tool
    /// and we don't want users accidentally leaving it on across sessions.
    /// Only read by the debug-only Developer section, so release builds
    /// see it as dead — `#[allow(dead_code)]` keeps that warning down.
    #[serde(default, skip)]
    #[allow(dead_code)]
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
    /// None = unassigned. Default Home + D-pad Up.
    #[serde(default = "default_overlay_chord")]
    pub overlay_chord: Option<Vec<String>>,
    /// Optional gamepad button combo that toggles the PIN (always-on-top). The
    /// pin's old Guide binding moved here so the user picks their own button /
    /// combo. Focus-gated like the other chords. None = unassigned. Default
    /// Home + D-pad Down.
    #[serde(default = "default_pin_chord")]
    pub pin_chord: Option<Vec<String>>,
    /// Optional gamepad button combo that toggles the CONFIG overlay. Detected
    /// globally by the background watcher (fires even while a game holds focus),
    /// so it's the primary controller path to summon the overlay mid-play.
    /// None = unassigned. Default Home + D-pad Right.
    #[serde(default = "default_config_overlay_chord")]
    pub config_overlay_chord: Option<Vec<String>>,
    /// Optional gamepad button combo that toggles the PHASE tuner over the game
    /// (display-synced output's Phase in frame). Global, like the config
    /// overlay's. None = unassigned (the default).
    #[serde(default)]
    pub phase_overlay_chord: Option<Vec<String>>,
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
    #[serde(default = "default_chord_mode")]
    pub phase_overlay_chord_mode: String,
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
    #[serde(default = "default_chord_gap_ms")]
    pub phase_overlay_chord_gap_ms: f32,
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
    /// The monitor the overlays cover (and a calibration's screenshot is taken of), by
    /// device name (`DISPLAY2`); `None` = the primary. Picked in the header.
    #[serde(default)]
    pub overlay_monitor: Option<String>,
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
    /// When true, a Home/Guide/PS press that no gamepad shortcut used is
    /// swallowed. When false (default), it's held back while it might still
    /// become a shortcut and replayed to the game as a tap on release.
    #[serde(default)]
    pub gamepad_home_exclusive: bool,
    /// Repaint rate applied while the window is unfocused / minimized.
    /// Focused window always paints at vsync. Range BG_REPAINT_HZ_MIN..=MAX.
    #[serde(default = "default_bg_repaint_hz")]
    pub bg_repaint_hz: u32,
    /// Stop drawing windows that aren't focused and dim them, leaving the last
    /// frame on screen. Input processing isn't affected.
    #[serde(default)]
    pub freeze_background: bool,
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

    /// Persisted main-window geometry (logical points at UI scale 100% — the
    /// units the window is created in at startup, before `ui_scale` takes
    /// effect), restored on the next launch so the window reopens where it
    /// was instead of cascading down-right each time. `window_pos` is the outer
    /// (top-left) position; `window_size` is the inner size. `None` until the
    /// window has been shown once. While maximized, pos/size keep the last
    /// *restored-down* geometry (so un-maximizing returns there) and
    /// `window_maximized` is set.
    #[serde(default)]
    pub window_pos: Option<[f32; 2]>,
    #[serde(default)]
    pub window_size: Option<[f32; 2]>,
    #[serde(default)]
    pub window_maximized: bool,
}

/// Startup window geometry resolved from persisted settings, for the app
/// binary's `main` to feed into the `ViewportBuilder`. Position is dropped when
/// it would land off every current monitor (see [`crate::onscreen_position`]).
#[derive(Clone, Copy, Debug)]
pub struct WindowGeometry {
    pub pos: Option<[f32; 2]>,
    pub size: Option<[f32; 2]>,
    pub maximized: bool,
}

/// Load the persisted window geometry for `main` to apply before creating the
/// window. Returns `None` when nothing was saved yet (first ever launch).
pub fn startup_window_geometry() -> WindowGeometry {
    let s = load_settings();
    let pos = s.window_pos.and_then(crate::onscreen_position);
    WindowGeometry {
        pos,
        size: s.window_size,
        maximized: s.window_maximized,
    }
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
    /// DirectX 12 on Windows, then Vulkan, then OpenGL if a backend fails to
    /// start (see `auto_cascade` in app/src/main.rs).
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

/// Which HidHide-masked controllers stay hidden once FlexInput exits
/// (see `AppSettings::hidhide_keep_on_exit`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HidHideKeepOnExit {
    /// Unhide everything FlexInput hid.
    Never,
    /// Keep only controllers hidden with the eye button; unhide the rest.
    #[default]
    ManualOnly,
    /// Keep every controller FlexInput hid.
    Always,
}

impl HidHideKeepOnExit {
    pub const ALL: [Self; 3] = [Self::Never, Self::ManualOnly, Self::Always];

    pub fn label(self) -> &'static str {
        match self {
            Self::Never => "Never",
            Self::ManualOnly => "Only manually hidden",
            Self::Always => "Always",
        }
    }

    /// Whether a controller hidden right now stays hidden after exit.
    pub fn keeps(self, manual: bool) -> bool {
        match self {
            Self::Never => false,
            Self::ManualOnly => manual,
            Self::Always => true,
        }
    }
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

/// The canvas's layout grid. Modules sit on a grid of square blocks with their
/// edges on block centers, so there's always at least a block's gap between
/// two, and wires run along the block lines down the middle of those gaps.
/// Edited from the canvas's zoom toolbar.
#[derive(Clone, Copy, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct CanvasGrid {
    /// Draw the grid on the canvas.
    pub show: bool,
    /// How it's drawn: lines, dashed lines, or crosses on the block corners.
    pub style: egui_snarl::ui::GridStyle,
    /// Every this many blocks a line is a little brighter (0: none).
    pub accent_every: u32,
    /// How bright the grid is drawn, times its usual faintness (1 is usual);
    /// low-contrast themes need it brighter to see it at all.
    pub brightness: f32,
    /// Snap modules to the grid as they're dragged (Alt drags freely), and
    /// size them in whole blocks.
    pub snap: bool,
    /// Push modules out of the way of one dropped onto them or grown into them.
    pub push: bool,
    /// How wires are drawn: straight over the modules, around them, or along
    /// the grid's lines. (A new field, not the old `route_wires` flag, so a
    /// settings file from before still loads: the flag is ignored.)
    pub wires: egui_snarl::ui::WireRouting,
    /// How much the wires of the hovered or selected module stand out (0 to 1):
    /// they thicken a little and the rest of the patch dims. 0 is off.
    pub focus: f32,
    /// How far wires going around modules keep from them, in canvas points.
    pub clearance: f32,
    /// The range activity glow is shown in (0 to 1): the least any activity
    /// glows, and the most full activity does.
    pub glow_min: f32,
    pub glow_max: f32,
    /// What makes an Auto-Map bus glow.
    pub automap_glow: AutomapGlow,
}

/// What makes an Auto-Map bus (and its wires) glow: which of the source
/// device's readings count as activity.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum AutomapGlow {
    /// Any reading away from zero.
    Any,
    /// Readings moving away from where they rest: an accelerometer reading
    /// gravity, or a value parked somewhere, stops counting once it settles.
    Changes,
    /// Only readings that mean something right now: a touch position counts
    /// only while there's a touch.
    #[default]
    Live,
}

impl Default for CanvasGrid {
    fn default() -> Self {
        Self {
            show: true,
            style: egui_snarl::ui::GridStyle::Crosses,
            accent_every: 0,
            brightness: 1.0,
            snap: true,
            push: true,
            wires: egui_snarl::ui::WireRouting::Around,
            focus: 0.5,
            clearance: 12.0,
            glow_min: 0.0,
            glow_max: 1.0,
            automap_glow: AutomapGlow::Live,
        }
    }
}

impl CanvasGrid {
    /// The spacings the grid menu offers for the brighter line: 0 for none, which is
    /// what ships, and odd numbers of blocks above it. An even step puts the bright lines
    /// through a module four blocks across; an odd one puts them either side of it, which is
    /// what makes the grid read as the thing the modules sit on.
    pub(crate) const ACCENT_CHOICES: &[u32] = &[0, 3, 5, 7, 9];

    /// The one block size modules are laid out in, in canvas points: modules
    /// are designed in multiples of it, so it isn't a setting.
    ///
    /// Half of what it was. The finer step is what makes a column of port rows come out
    /// even: a row is 28 points (a 25-point strip plus the pin column's 3-point spacing), so
    /// a row is exactly one block and the empty band under the last strip is the same size at
    /// every port count - measured, 26 points at three ports and at seven, where the 56-point
    /// block alternated between 26 and 55 depending on whether the port count was odd. It
    /// buys granularity, not size: the floors and defaults that were stated in old blocks are
    /// now stated in twice as many of these, so a module that started two blocks square still
    /// starts 112 points square. Nothing is brighter by default; where a patch wants a
    /// brighter line, the choices are odd numbers of blocks, so it brackets a four-block
    /// module rather than cutting through it.
    pub const BLOCK: f32 = 28.0;

    /// The snarl style's grid for these settings, `None` when it does nothing.
    pub fn layout(self) -> Option<egui_snarl::ui::GridLayout> {
        let routed = self.wires != egui_snarl::ui::WireRouting::Direct;
        (self.show || self.snap || routed).then(|| egui_snarl::ui::GridLayout {
            block: Self::BLOCK,
            show: self.show,
            style: self.style,
            accent_every: self.accent_every,
            brightness: self.brightness.clamp(0.0, 8.0),
            snap: self.snap,
            push: self.push && self.snap,
            wires: self.wires,
            clearance: self.clearance.max(0.0),
        })
    }
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
            bt_key_dir: None,
            polling_hz: POLLING_HZ_DEFAULT,
            output_hz: OUTPUT_HZ_SAME_AS_POLLING,
            output_display_sync: false,
            output_sync_per_frame: default_output_sync_per_frame(),
            output_sync_phase: default_output_sync_phase(),
            output_sync_follow_vrr: true,
            sample_rate_hz: SAMPLE_RATE_HZ_DEFAULT,
            sample_rate_mode: SampleRateMode::Manual,
            keep_workspace: true,
            device_nodes_default_collapsed: true,
            default_stick_deadzone: 0.1,
            default_gyro_mult: 1.0,
            default_mouse_sensitivity: 100.0,
            default_rumble_floor: default_rumble_floor(),
            default_rumble_max: default_rumble_max(),
            default_rumble_exp: default_rumble_exp(),
            theme: Theme::Dark,
            contrast: CONTRAST_DEFAULT,
            accent_color: ACCENT_DEFAULT,
            gray_tint: GRAY_TINT_DEFAULT,
            gray_tint_amount: 0.0,
            control_font_mono: false,
            control_font_bold: false,
            control_font_size: CONTROL_FONT_SIZE_DEFAULT,
            ui_scale: UI_SCALE_DEFAULT,
            see_through_alpha: 0.55,
            see_through_active: false,
            pin_active: false,
            pin_shortcut: PinShortcut::default(),
            pin_via_guide: false,
            config_via_guide: true,
            config_guide_double_tap: true,
            config_guide_chord: None,
            gamepad_ui_nav_default: true,
            cursor_max_speed: 4000.0,
            cursor_accel: 2.0,
            pin_guide_double_tap: true,
            focus_flip_flop: true,
            pin_guide_chord: None,
            show_own_virtuals_as_physical: false,
            persist_virtual_devices: false,
            hide_originals: None,
            hidhide_sticky: true,
            hidhide_keep_on_exit: HidHideKeepOnExit::default(),
            hidhide_overrides: HashMap::new(),
            hidhide_kept_ids: Vec::new(),
            led_defaults: HashMap::new(),
            raw_pad_profiles: HashMap::new(),
            renderer: RendererChoice::Auto,
            on_patch_load: OnPatchLoad::Off,
            canvas_grid: CanvasGrid::default(),
            profiler_enabled: false,
            ui_mode: UiMode::Easy,
            user_presets_folder: None,
            favorite_presets: Vec::new(),
            seethrough_chord: None,
            panic_chord: None,
            overlay_chord: default_overlay_chord(),
            pin_chord: default_pin_chord(),
            config_overlay_chord: default_config_overlay_chord(),
            phase_overlay_chord: None,
            seethrough_chord_mode: default_chord_mode(),
            panic_chord_mode: default_chord_mode(),
            overlay_chord_mode: default_chord_mode(),
            pin_chord_mode: default_chord_mode(),
            config_overlay_chord_mode: default_chord_mode(),
            phase_overlay_chord_mode: default_chord_mode(),
            seethrough_chord_gap_ms: default_chord_gap_ms(),
            panic_chord_gap_ms: default_chord_gap_ms(),
            overlay_chord_gap_ms: default_chord_gap_ms(),
            pin_chord_gap_ms: default_chord_gap_ms(),
            config_overlay_chord_gap_ms: default_chord_gap_ms(),
            phase_overlay_chord_gap_ms: default_chord_gap_ms(),
            overlay_visible: false,
            overlay_shortcut: default_overlay_shortcut(),
            edit_overlay_shortcut: default_edit_overlay_shortcut(),
            overlay_fps: OVERLAY_FPS_DEFAULT,
            overlay_monitor: None,
            config_overlay_visible: false,
            config_overlay_shortcut: default_config_overlay_shortcut(),
            config_overlay_passthrough_default: false,
            gamepad_chords_nav_only: true,
            gamepad_home_exclusive: false,
            bg_repaint_hz: BG_REPAINT_HZ_DEFAULT,
            freeze_background: false,
            mouse_suppression_enabled: true,
            mouse_suppress_release_ms: MOUSE_SUPPRESS_RELEASE_MS_DEFAULT,
            mixed_braid_enabled: false,
            mixed_braid_rate_hz: MIXED_BRAID_RATE_HZ_DEFAULT,
            sdl_all_pads: false,
            window_pos: None,
            window_size: None,
            window_maximized: false,
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
    apply_gray_tint(&mut style.visuals, settings.gray_tint, settings.gray_tint_amount);
    // The accent: what is selected, lit or on.
    let [r, g, b] = settings.accent_color;
    style.visuals.selection.bg_fill = egui::Color32::from_rgb(r, g, b);
    // The controls' font: the pills read it from `simple_bodies`, and every egui button and
    // dropdown from the Button text style - so none is left in the font it had.
    let size = settings.control_font_size.clamp(CONTROL_FONT_SIZE_MIN, CONTROL_FONT_SIZE_MAX);
    crate::canvas::viewer::simple_bodies::set_control_font(settings.control_font_mono, settings.control_font_bold, size);
    style.text_styles.insert(
        egui::TextStyle::Button,
        crate::canvas::viewer::simple_bodies::control_font_at(size.min(crate::canvas::viewer::simple_bodies::CONTROL_FONT_MAX_PX)),
    );
    // And every other text style in it, at egui's sizes scaled by the setting: labels,
    // hints and headings were left in egui's font. (Monospace stays: it is for code.)
    for (ts, base) in egui::Style::default().text_styles {
        if base.family == egui::FontFamily::Proportional && ts != egui::TextStyle::Button {
            style.text_styles.insert(ts, crate::canvas::viewer::simple_bodies::ui_font(base.size));
        }
    }

    // See-through mode is NOT applied here on the global style — that
    // would tint sub-windows (Settings, Calibration, Subpatch editors)
    // and bleed into node bodies via `widgets.*` and `extreme_bg_color`.
    // Instead the canvas CentralPanel applies the alpha directly on its
    // own `egui::Frame` (see `app.rs` central-panel block), so only the
    // backdrop behind the snarl nodes goes translucent. Nothing else.
    ctx.set_style(style);
}


pub(crate) use crate::canvas::viewer::simple_bodies::{ui_font, ui_size};

/// The grey tint's per-channel factors, as [`apply_gray_tint`] last set them (1, 1, 1 =
/// none), for the greys painted by hand rather than taken from the style: [`tinted`].
static TINT_K: [std::sync::atomic::AtomicU32; 3] = [
    std::sync::atomic::AtomicU32::new(0x3F80_0000),
    std::sync::atomic::AtomicU32::new(0x3F80_0000),
    std::sync::atomic::AtomicU32::new(0x3F80_0000),
];

/// Each channel's factor for a tint `amount` (0..1) of `tint`: the channel over the tint's
/// brightness, so a grey keeps its lightness and takes the tint's hue.
fn tint_factors(tint: [u8; 3], amount: f32) -> [f32; 3] {
    let amount = amount.clamp(0.0, 1.0);
    let luma = (0.299 * tint[0] as f32 + 0.587 * tint[1] as f32 + 0.114 * tint[2] as f32).max(1.0);
    tint.map(|c| 1.0 + (c as f32 / luma - 1.0) * amount)
}

fn tint_with(col: egui::Color32, k: [f32; 3]) -> egui::Color32 {
    let f = |c: u8, k: f32| (c as f32 * k).round().clamp(0.0, 255.0) as u8;
    egui::Color32::from_rgba_unmultiplied(f(col.r(), k[0]), f(col.g(), k[1]), f(col.b(), k[2]), col.a())
}

/// `col` under the grey tint (Settings → Display), for a grey a painter picks itself - a
/// node's border, a graph's backdrop, a hint's text. The style's own greys take it in
/// [`apply_gray_tint`]; these didn't, and stood out grey among the tinted ones.
pub(crate) fn tinted(col: egui::Color32) -> egui::Color32 {
    let k = TINT_K.each_ref().map(|a| f32::from_bits(a.load(std::sync::atomic::Ordering::Relaxed)));
    if k == [1.0; 3] {
        col
    } else {
        tint_with(col, k)
    }
}

/// A grey of lightness `l`, under the grey tint.
pub(crate) fn tinted_gray(l: u8) -> egui::Color32 {
    tinted(egui::Color32::from_gray(l))
}

/// Lean the greys of panels, widgets and text toward `tint` by `amount` (0..1), keeping their
/// lightness, and remember it for the greys painted by hand ([`tinted`]).
pub(crate) fn apply_gray_tint(v: &mut egui::Visuals, tint: [u8; 3], amount: f32) {
    let k = if amount < 1e-3 { [1.0; 3] } else { tint_factors(tint, amount) };
    for (a, k) in TINT_K.iter().zip(k) {
        a.store(k.to_bits(), std::sync::atomic::Ordering::Relaxed);
    }
    if k == [1.0; 3] {
        return;
    }
    let t = |col: egui::Color32| tint_with(col, k);
    v.panel_fill = t(v.panel_fill);
    v.window_fill = t(v.window_fill);
    v.extreme_bg_color = t(v.extreme_bg_color);
    v.faint_bg_color = t(v.faint_bg_color);
    v.code_bg_color = t(v.code_bg_color);
    v.window_stroke.color = t(v.window_stroke.color);
    v.window_shadow.color = t(v.window_shadow.color);
    v.popup_shadow.color = t(v.popup_shadow.color);
    for w in [&mut v.widgets.noninteractive, &mut v.widgets.inactive, &mut v.widgets.hovered, &mut v.widgets.active, &mut v.widgets.open] {
        w.bg_fill = t(w.bg_fill);
        w.weak_bg_fill = t(w.weak_bg_fill);
        w.bg_stroke.color = t(w.bg_stroke.color);
        // The text greys too: weak and normal text are greys as much as a panel is.
        w.fg_stroke.color = t(w.fg_stroke.color);
    }
}

/// What [`apply_theme_and_contrast`] reads, as one number: the app re-applies the style
/// when it changes.
pub(crate) fn theme_key(s: &AppSettings) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    (s.theme as u8).hash(&mut h);
    s.contrast.to_bits().hash(&mut h);
    s.see_through_active.hash(&mut h);
    s.see_through_alpha.to_bits().hash(&mut h);
    s.accent_color.hash(&mut h);
    s.gray_tint.hash(&mut h);
    s.gray_tint_amount.to_bits().hash(&mut h);
    s.control_font_mono.hash(&mut h);
    s.control_font_bold.hash(&mut h);
    s.control_font_size.to_bits().hash(&mut h);
    h.finish()
}

fn settings_path() -> Option<std::path::PathBuf> {
    let mut p = flexinput_core::paths::app_data_dir()?;
    p.push("settings.json");
    Some(p)
}

fn render_attempt_path() -> Option<std::path::PathBuf> {
    let mut p = flexinput_core::paths::app_data_dir()?;
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
    let mut p = flexinput_core::paths::app_data_dir()?;
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
    /// The calibration of pads taken off the tab (`canvas::DeviceMemory`).
    #[serde(default, skip_serializing_if = "crate::canvas::DeviceMemory::is_empty")]
    pub device_memory: crate::canvas::DeviceMemory,
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
    parse_workspace_resilient(&bytes, Some(&p))
}

/// Parse a workspace, isolating per-tab failures. A tab whose schema no longer
/// deserializes (e.g. a field removed across versions) is BLANKED — its content
/// is dropped but its slot, title, and bindings are kept — so one incompatible
/// patch never discards the whole workspace. Before returning any such degraded
/// result the ORIGINAL bytes are copied to a `.corrupt-<ts>.bak` sibling so
/// nothing is lost to the subsequent autosave. Returns `None` only when the
/// bytes are not valid JSON at all (still backed up first).
fn parse_workspace_resilient(bytes: &[u8], origin: Option<&std::path::Path>) -> Option<PersistedWorkspace> {
    // Fast path: whole workspace still deserializes cleanly.
    if let Ok(ws) = serde_json::from_slice::<PersistedWorkspace>(bytes) {
        return Some(ws);
    }
    // Degraded path: something no longer fits the schema. Preserve the original
    // before we hand back anything the app might autosave over.
    if let Some(p) = origin {
        backup_corrupt_workspace(p, bytes);
    }
    let val: serde_json::Value = serde_json::from_slice(bytes).ok()?;
    let version = val.get("version").and_then(|v| v.as_u64()).unwrap_or(1) as u32;
    let active_tab = val.get("active_tab").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
    let mut tabs = Vec::new();
    if let Some(arr) = val.get("tabs").and_then(|t| t.as_array()) {
        for tv in arr {
            match serde_json::from_value::<PersistedTab>(tv.clone()) {
                Ok(t) => tabs.push(t),
                Err(_) => tabs.push(blank_tab_from_value(tv)),
            }
        }
    }
    Some(PersistedWorkspace { version, active_tab, tabs })
}

/// Best-effort blanked tab from a tab value that failed to deserialize: keep the
/// identifying metadata (title, file/exe bindings) but drop the unparseable
/// snarl and overlays. The title is marked so the user sees which patch was
/// reset (and that a `.bak` holds the original).
fn blank_tab_from_value(v: &serde_json::Value) -> PersistedTab {
    let title = v.get("title").and_then(|x| x.as_str()).unwrap_or("Untitled");
    PersistedTab {
        title: format!("{title} (recovered)"),
        file_path: v.get("file_path").cloned().and_then(|x| serde_json::from_value(x).ok()).unwrap_or(None),
        bound_exes: v.get("bound_exes").cloned().and_then(|x| serde_json::from_value(x).ok()).unwrap_or_default(),
        auto_bypass: v.get("auto_bypass").and_then(|x| x.as_bool()).unwrap_or(false),
        snarl: Snarl::new(),
        easy_preset_path: None,
        view_salt: 0,
        overlay: crate::canvas::OverlayLayout::default(),
        config: crate::canvas::OverlayLayout::default(),
        device_memory: v.get("device_memory").cloned().and_then(|x| serde_json::from_value(x).ok()).unwrap_or_default(),
    }
}

/// Copy the original (unparseable / partly-incompatible) workspace bytes to a
/// timestamped `.corrupt-<epoch>.bak` sibling so the user can recover the exact
/// pre-degradation state even after the app autosaves the blanked version.
fn backup_corrupt_workspace(origin: &std::path::Path, bytes: &[u8]) {
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let name = origin.file_name().and_then(|f| f.to_str()).unwrap_or("workspace.json");
    let mut bak = origin.to_path_buf();
    bak.set_file_name(format!("{name}.corrupt-{ts}.bak"));
    let _ = std::fs::write(&bak, bytes);
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
    parse_workspace_resilient(&bytes, Some(path))
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
    let mut p = flexinput_core::paths::app_data_dir()?;
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
    parse_workspace_resilient(&bytes, Some(&p))
}

/// Delete the crash-recovery snapshot. Called on clean exit and immediately
/// after a recovery snapshot has been consumed on boot.
pub fn delete_recovery() {
    if let Some(p) = recovery_path() {
        let _ = std::fs::remove_file(&p);
    }
}

#[cfg(test)]
mod sample_rate_tests {
    use super::*;

    fn with(polling: u32, output: u32, manual: u32, mode: SampleRateMode) -> AppSettings {
        AppSettings {
            polling_hz: polling,
            output_hz: output,
            sample_rate_hz: manual,
            sample_rate_mode: mode,
            ..AppSettings::default()
        }
    }

    #[test]
    fn linked_rates_follow_the_output_which_can_follow_polling() {
        use SampleRateMode::*;
        assert_eq!(with(125, OUTPUT_HZ_SAME_AS_POLLING, 2000, SameAsOutput).effective_sample_rate_hz(), 125);
        assert_eq!(with(125, OUTPUT_HZ_SAME_AS_POLLING, 2000, TwiceOutput).effective_sample_rate_hz(), 250);
        assert_eq!(with(125, 500, 2000, SameAsOutput).effective_sample_rate_hz(), 500);
        assert_eq!(with(1000, 250, 2000, TwiceOutput).effective_sample_rate_hz(), 500);
        assert_eq!(with(2000, 2000, 2000, TwiceOutput).effective_sample_rate_hz(), 4000);
    }

    #[test]
    fn by_hand_is_the_default_and_leaves_a_saved_rate_alone() {
        assert_eq!(AppSettings::default().sample_rate_mode, SampleRateMode::Manual);
        assert_eq!(with(2000, 0, 500, SampleRateMode::Manual).effective_sample_rate_hz(), 500);
    }

    #[test]
    fn display_sync_is_off_by_default_and_made_valid_when_on() {
        assert!(AppSettings::default().output_sync().is_none());
        assert_eq!(snap_output_sync_per_frame(0), 1);
        assert_eq!(snap_output_sync_per_frame(5), 4);
        assert_eq!(snap_output_sync_per_frame(100), 8);
        if flexinput_virtual::display_clock::SUPPORTED {
            let on = AppSettings {
                output_display_sync: true,
                output_sync_per_frame: 5,
                output_sync_phase: 2.0,
                ..AppSettings::default()
            };
            let sync = on.output_sync().unwrap();
            assert_eq!(sync.per_frame, 4);
            assert_eq!(sync.phase, OUTPUT_SYNC_PHASE_MAX);
        }
    }

    #[test]
    fn the_slider_floor_is_polling_unless_the_saved_rate_is_lower() {
        assert_eq!(with(125, 0, 2000, SampleRateMode::Manual).sample_rate_floor_hz(), 125);
        assert_eq!(with(500, 0, 2000, SampleRateMode::Manual).sample_rate_floor_hz(), 500);
        assert_eq!(with(2000, 0, 500, SampleRateMode::Manual).sample_rate_floor_hz(), 500);
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
        // Between 1000 and 2000 -> nearer step.
        assert_eq!(snap_polling_hz(1400), 1000);
        assert_eq!(snap_polling_hz(1600), 2000);
        // Out-of-range clamps to the ends.
        assert_eq!(snap_polling_hz(50), 125);
        assert_eq!(snap_polling_hz(5000), 2000);
    }

    #[test]
    fn output_rate_keeps_same_as_polling_and_snaps_the_rest() {
        assert_eq!(snap_output_hz(OUTPUT_HZ_SAME_AS_POLLING), OUTPUT_HZ_SAME_AS_POLLING);
        assert_eq!(snap_output_hz(2000), 2000);
        assert_eq!(snap_output_hz(900), 1000);
        assert_eq!(snap_output_hz(1), 125);
    }

    #[test]
    fn ui_scale_steps_round_trip_and_snap() {
        for (i, &s) in UI_SCALE_STEPS.iter().enumerate() {
            assert_eq!(ui_scale_to_index(s), i, "scale {s} should map to index {i}");
            assert_eq!(ui_scale_from_index(i), s);
        }
        assert!(UI_SCALE_STEPS.contains(&UI_SCALE_DEFAULT));
        assert_eq!(UI_SCALE_STEPS[0], 0.5);
        assert_eq!(*UI_SCALE_STEPS.last().unwrap(), 3.0);
        assert_eq!(snap_ui_scale(1.04), 1.0);
        assert_eq!(snap_ui_scale(1.4), 1.5);
        assert_eq!(snap_ui_scale(0.1), 0.5);
        assert_eq!(snap_ui_scale(9.0), 3.0);
        assert_eq!(snap_ui_scale(f32::NAN), UI_SCALE_DEFAULT);
    }

    #[test]
    fn from_index_clamps_out_of_bounds() {
        assert_eq!(polling_hz_from_index(99), 125); // last step
    }
}

#[cfg(test)]
mod workspace_resilience_tests {
    use super::*;

    fn tab(title: &str) -> PersistedTab {
        PersistedTab {
            title: title.into(),
            file_path: None,
            bound_exes: vec![],
            auto_bypass: false,
            snarl: Snarl::new(),
            easy_preset_path: None,
            view_salt: 7,
            overlay: crate::canvas::OverlayLayout::default(),
            config: crate::canvas::OverlayLayout::default(),
            device_memory: crate::canvas::DeviceMemory::default(),
        }
    }

    #[test]
    fn one_incompatible_tab_is_blanked_not_the_whole_workspace() {
        let ws = PersistedWorkspace { version: 1, active_tab: 0, tabs: vec![tab("Good"), tab("Bad")] };
        let mut val: serde_json::Value = serde_json::to_value(&ws).unwrap();
        // Make tab[1] undeserializable (as a removed enum variant / schema drift would).
        val["tabs"][1]["snarl"] = serde_json::Value::String("not a snarl".into());
        let bytes = serde_json::to_vec(&val).unwrap();

        // origin = None → no backup file written during the test.
        let out = parse_workspace_resilient(&bytes, None).expect("must salvage, not lose all");
        assert_eq!(out.tabs.len(), 2, "both slots kept");
        assert_eq!(out.tabs[0].title, "Good");
        assert_eq!(out.tabs[0].view_salt, 7, "the good tab is untouched");
        assert_eq!(out.tabs[1].title, "Bad (recovered)", "the bad tab is blanked + marked");
    }

    #[test]
    fn clean_workspace_takes_the_fast_path() {
        let ws = PersistedWorkspace { version: 1, active_tab: 1, tabs: vec![tab("A"), tab("B")] };
        let bytes = serde_json::to_vec(&ws).unwrap();
        let out = parse_workspace_resilient(&bytes, None).unwrap();
        assert_eq!(out.active_tab, 1);
        assert_eq!(out.tabs.iter().map(|t| t.title.clone()).collect::<Vec<_>>(), ["A", "B"]);
    }
}

#[cfg(test)]
mod theme_tests {
    use super::*;

    // The accent is what is selected; the tint at 0 leaves the greys alone, and turned up it
    // moves their hue toward the tint while keeping them about as light. (The control font is
    // left at its default: it is process-wide, and other tests measure with it.)
    #[test]
    fn the_theme_settings_reach_the_style() {
        let ctx = egui::Context::default();
        let mut s = AppSettings::default();
        apply_theme_and_contrast(&ctx, &s);
        let plain = ctx.style().visuals.clone();
        assert_eq!(plain.selection.bg_fill, egui::Color32::from_rgb(0, 92, 128));

        s.accent_color = [200, 60, 30];
        s.gray_tint = [90, 120, 200];
        s.gray_tint_amount = 1.0;
        apply_theme_and_contrast(&ctx, &s);
        let v = ctx.style().visuals.clone();
        assert_eq!(v.selection.bg_fill, egui::Color32::from_rgb(200, 60, 30));
        let (a, b) = (plain.panel_fill, v.panel_fill);
        assert!(b.b() > b.r(), "the panel didn't take the blue tint: {a:?} -> {b:?}");
        let luma = |c: egui::Color32| 0.299 * c.r() as f32 + 0.587 * c.g() as f32 + 0.114 * c.b() as f32;
        assert!((luma(a) - luma(b)).abs() < 4.0, "the tint changed the lightness: {a:?} -> {b:?}");

        let mut zero = plain.clone();
        apply_gray_tint(&mut zero, [255, 0, 0], 0.0);
        assert_eq!(zero.panel_fill, plain.panel_fill);
    }
}
