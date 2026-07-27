//! Gamepad-driven UI navigation (Easy mode).
//!
//! When a physical gamepad has "UI navigation" enabled AND FlexInput holds
//! window focus, the controller drives FlexInput's own UI instead of (or in
//! addition to being suppressed from) the patch it normally maps. The
//! per-frame driver lives in `app.rs::run_gamepad_nav`; this module owns the
//! navigation *state* plus the pure helpers it leans on (input snapshot +
//! edge detection, spatial neighbour-finding).
//!
//! Output suppression is NOT here — it reuses the I/O thread's existing
//! `io_bypass` gate via a shared `ui_nav_suppress` atomic set in `update()`.
//!
//! Pin ids come from `flexinput_devices::gamepad` (verified): buttons
//! `btn_south/east/west/north`, `btn_lb/rb`, `btn_lstick/rstick`,
//! `btn_start/select`, `dpad_up/down/left/right`; sticks `left_stick` /
//! `right_stick` (Vec2); triggers `left_trigger` / `right_trigger` (Float).
//! Gyro is `gyro_y` (pitch) / `gyro_z` (yaw) per the gilrs backend.

use std::collections::{HashMap, HashSet};
use std::time::Instant;

use eframe::egui;
use flexinput_core::Signal;

use crate::canvas::node::LayoutItem;

/// Cardinal navigation direction.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NavDir {
    Up,
    Down,
    Left,
    Right,
}

/// Which global shortcut a gamepad chord-capture is binding.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChordTarget {
    SeeThrough,
    Panic,
    Overlay,
    Pin,
    ConfigOverlay,
}

/// Per-shortcut edge/timing state carried between frames by `chord_fire`.
#[derive(Clone, Debug, Default)]
pub struct ChordFireState {
    /// Were all the combo's buttons held last frame? (Rising-edge tracker.)
    pub down: bool,
    /// When the combo was first fully held (for `long`).
    pressed_at: Option<Instant>,
    /// When the last tap fired (for `double`).
    last_tap_at: Option<Instant>,
    /// Latch so `long` fires once per hold, not every frame past the threshold.
    fired_this_hold: bool,
}

/// Decide whether a shortcut chord should fire this frame.
///
/// `held` = every button in the chord is currently pressed. `mode` selects the
/// firing condition, mirroring the remapper cards' discrete modes:
///  * `"down"` (default) — fire on the press edge (all-held rising edge).
///  * `"long"` — fire once after the combo is held for `gap_ms`.
///  * `"double"` — fire on a second press within `gap_ms` of the first.
///
/// `st` carries the timing/edge state between calls. Returns true exactly on
/// the frame the shortcut should toggle.
pub fn chord_fire(st: &mut ChordFireState, held: bool, mode: &str,
    gap_ms: f32, now: Instant) -> bool
{
    let gap = std::time::Duration::from_millis(gap_ms.max(1.0) as u64);
    let rising = held && !st.down;
    let falling = !held && st.down;
    let mut fired = false;
    match mode {
        "long" => {
            if rising { st.pressed_at = Some(now); st.fired_this_hold = false; }
            if held && !st.fired_this_hold {
                if let Some(t) = st.pressed_at {
                    if now.duration_since(t) >= gap {
                        fired = true;
                        st.fired_this_hold = true;
                    }
                }
            }
            if falling { st.pressed_at = None; st.fired_this_hold = false; }
        }
        "double" => {
            if rising {
                let is_double = st.last_tap_at
                    .map(|t| now.duration_since(t) < gap)
                    .unwrap_or(false);
                if is_double { fired = true; st.last_tap_at = None; }
                else { st.last_tap_at = Some(now); }
            }
        }
        // "down" and any unknown mode.
        _ => { if rising { fired = true; } }
    }
    st.down = held;
    fired
}

/// The discrete press modes offered for gamepad shortcut chords, as
/// `(value, glyph, label)`. A subset of the remapper card modes — the
/// analog/turbo/edge variants don't apply to a one-shot toggle.
pub const SHORTCUT_PRESS_MODES: &[(&str, &str, &str)] = &[
    ("down",   "↓", "On press"),
    ("long",   "⇓", "Long press"),
    ("double", "↡", "Double tap"),
];

/// Human label for a shortcut press-mode value (for the button face/tooltip).
pub fn shortcut_press_mode_label(mode: &str) -> &'static str {
    SHORTCUT_PRESS_MODES.iter()
        .find(|(v, _, _)| *v == mode)
        .map(|(_, _, l)| *l)
        .unwrap_or("On press")
}

/// Glyph for a shortcut press-mode value.
pub fn shortcut_press_mode_glyph(mode: &str) -> &'static str {
    SHORTCUT_PRESS_MODES.iter()
        .find(|(v, _, _)| *v == mode)
        .map(|(_, g, _)| *g)
        .unwrap_or("↓")
}

/// Selection model levels.
/// - `Widget`: moving the selection between sub-patch widgets.
/// - `Editing`: editing a scalar/dropdown widget's value.
/// - `CurveDots`: inside a response curve; dpad/LS highlights a dot, RT/LT
///   add/remove dots, South enters `CurveDot`.
/// - `CurveDot`: moving the highlighted dot in X/Y; hold-North edits curvature.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum EditLevel {
    #[default]
    Widget,
    Editing,
    CurveDots,
    CurveDot,
    /// Inside a remapper-family widget (Remapper / Map Action / Combiner / gyro
    /// Lean section): up/down moves the selected mapping card, North resets /
    /// West deletes the selected card, North-on-empty arms capture (Learn),
    /// LT/RT cycle the filter, South enters the card (`RemapCard`), East exits.
    RemapScroll,
    /// Inside a single entered mapping card: left/right move between header
    /// fields (press-mode / time-gap / hold / turbo, skipping grayed-out),
    /// up/down (or South) edit the focused field, East exits to `RemapScroll`.
    RemapCard,
    /// Inside a Touch Zones pad (the pinned "field" element): left/right cycles
    /// the selected COLUMN divider, up/down the ROW divider; South grabs the
    /// focused line (→ `TzGrab`), North recenters it, West removes it (mapping
    /// mode only), LT/RT add an adjacent line (mapping only), LB/RB switch the
    /// focused pad (split mode), East exits to `Widget`.
    TzLines,
    /// A Touch Zones divider is grabbed: dpad/LS along its perpendicular axis
    /// moves it, North recenters, South/East release back to `TzLines`.
    TzGrab,
    /// Inside the Touch Zones mapping CARDS widget: left/right (or LB/RB) cycles
    /// the selected zone; South advances the Learn→Assign→Add flow (idle: Learn;
    /// captured w/o output: open the Assign picker; captured w/ output: Add the
    /// mapping); North toggles gamepad-learn; West cancels a capture; East exits.
    TzCards,
}

/// What activating a left-panel (I/O panel) nav target does. Published by
/// `io_panel` each frame alongside the target's screen rect; the nav driver
/// hit-tests the cursor against these and dispatches the matching action.
///
/// Param-bearing variants carry the value range + step so the driver can edit
/// them without re-deriving the slider's bounds.
#[derive(Clone, Debug)]
pub enum LeftNavAction {
    /// Make this physical device the active input source (card click).
    SelectInput { device_id: String },
    /// Toggle a virtual output sink on/off by kind prefix.
    ToggleOutput { kind: String },
    /// Cycle the single gamepad output card to the next model (Xbox 360 → DS4 →
    /// DualSense → None → …). One nav target for the whole selector card.
    CycleGamepadOutput,
    /// Adjust a numeric param on a node (slider/drag-value): deadzone,
    /// gyro_multiplier, or mouse_speed. `(lo, hi, step)` bound the edit.
    AdjustParam {
        node: egui_snarl::NodeId,
        key: String,
        lo: f32,
        hi: f32,
        step: f32,
        default: f32,
        log: bool,
    },
    /// Toggle a bool param on a node (digital-triggers checkbox).
    ToggleParam { node: egui_snarl::NodeId, key: String },
}

/// One navigable left-panel element: its screen rect + what activating it does.
#[derive(Clone, Debug)]
pub struct LeftNavTarget {
    pub rect: egui::Rect,
    pub action: LeftNavAction,
}

/// ctx-temp id under which `io_panel` publishes `(pass_nr, Vec<LeftNavTarget>)`.
pub fn left_targets_id() -> egui::Id {
    egui::Id::new("gp_nav_left_targets")
}

/// All gamepad-nav runtime state. Runtime-only — never serialized. Lives on
/// `FlexInputApp`.
pub struct GamepadNav {
    /// device_id → nav-mode enabled. Seeded lazily from
    /// `settings.gamepad_ui_nav_default` when a device is first seen.
    pub mode: HashMap<String, bool>,
    /// Canonical bool-pin set asserted on the previous frame (edge detection).
    pub prev_pressed: HashSet<String>,
    /// Left-stick directional auto-repeat accumulator (in "steps").
    pub repeat_accum: f32,
    pub repeat_dir: Option<NavDir>,
    /// Widget-level vs editing-a-widget.
    pub edit_level: EditLevel,
    /// The physical device id driving nav this frame (set by `run_gamepad_nav`
    /// when a nav-enabled source is active, cleared otherwise). Used by the
    /// bottom legend bar to (a) decide whether to show and (b) pick the button
    /// glyph skin (Xbox / PlayStation / Switch).
    pub active_dev: Option<String>,
    /// West toggles fine increments / lower stick sensitivity while editing.
    pub fine_increment: bool,
    /// Pre-edit snarl snapshot, taken when entering Editing on a widget; pushed
    /// to undo as one entry when the edit gesture ends (if anything changed).
    pub edit_baseline: Option<Box<egui_snarl::Snarl<crate::canvas::NodeData>>>,
    /// Right-stick / gyro cursor overlay.
    pub cursor_pos: egui::Pos2,
    pub cursor_visible: bool,
    pub cursor_last_move: Instant,
    /// Cached recoloured cursor texture (loaded once).
    pub cursor_tex: Option<egui::TextureHandle>,
    /// Start-button hold tracking (tap = preset dropdown nav, hold = Settings).
    pub start_down_at: Option<Instant>,
    pub start_hold_fired: bool,
    /// Whether the gamepad-driven preset dropdown navigation is open.
    pub preset_nav_open: bool,
    /// Index highlighted in the preset dropdown nav (into the flat preset list).
    pub preset_nav_index: usize,
    /// One-shot: set by the driver when South is pressed in preset nav, consumed
    /// by the center panel to apply the highlighted preset.
    pub preset_nav_select: bool,
    /// Directional step requested this frame for preset nav (-1 up, +1 down, 0).
    pub preset_nav_step: i32,
    /// True while the Alt+Tab window switcher is engaged (Select held).
    pub alt_tab_active: bool,
    /// Re-arm latch for right-stick → arrow-key taps inside the switcher.
    pub rs_arrow_armed: bool,
    /// Previous-frame analog trigger pressed state (>0.5), for rising edges.
    pub prev_lt: bool,
    pub prev_rt: bool,
    /// Left-panel (I/O panel) edit-in-progress: the AdjustParam target the
    /// cursor entered with South/RT. While `Some`, stick/dpad nudges the param,
    /// West toggles fine, North resets, East/LT exits. None = not editing the
    /// left panel.
    pub left_edit: Option<LeftNavAction>,
    /// LS/D-pad selection within the left I/O panel: index into the frame's
    /// published `LeftNavTarget` list (deterministic order; clamped each frame,
    /// same stability model as `preset_nav_index`). `Some` means focus lives in
    /// the left panel and LS/D-pad move between its targets; `None` means focus
    /// is on the sub-patch (or nothing). Independent of the RS/gyro cursor.
    pub left_selected: Option<usize>,
    /// Gamepad-native settings panel (Hold-Start). Modal: captures dpad/South/
    /// East. `settings_index` is the highlighted row; `settings_editing` is true
    /// while a numeric row is being nudged.
    pub settings_open: bool,
    pub settings_index: usize,
    pub settings_editing: bool,
    /// Focused field index inside a multi-control widget element (gyro rows,
    /// curve option rows, counter min/max, …). Left/right moves between fields
    /// while editing; reset on entering a widget.
    pub field_index: usize,
    /// Highlighted control-point index while inside a response curve
    /// (`CurveDots`/`CurveDot`). Clamped to the curve's point count each frame.
    pub curve_dot: usize,
    /// True while the user is holding North to bend the segment (bias mode) at
    /// `CurveDot` level this frame. The config overlay reads it to republish the
    /// `gp_nav_curve_bias` handle-visibility flag with its OWN viewport pass —
    /// `nav_drive_curve_dot` stamps the root viewport's pass, which never matches
    /// the overlay viewport's. Reset each frame the dot editor runs.
    pub curve_bias: bool,
    /// RemapScroll cursor — spans the action buttons (Learn/Special/Add) then
    /// the mapping cards. Up/down moves it; South activates / enters.
    pub card_index: usize,
    /// True mapping-card index of the ENTERED card (`RemapCard`), resolved from
    /// the cursor at entry (cursor minus the action-button count).
    pub remap_card: usize,
    /// Focused header field inside an entered mapping card (`RemapCard`):
    /// 0=press-mode, 1=time-gap, 2=hold, 3=turbo. Left/right moves between the
    /// fields that apply for the current mode (grayed-out ones are skipped).
    pub card_field: usize,
    /// Which level `RemapCard` returns to on exit. The Remapper enters cards from
    /// `RemapScroll`; Touch Zones enters them from `TzCards`. Set at entry so the
    /// SHARED card-field driver (`nav_drive_remap_card`) pops back to the right
    /// list instead of hard-coding `RemapScroll`.
    pub card_return_level: EditLevel,
    /// Which level `CurveDots` returns to on exit. The Response Curve family exits
    /// to `Widget`; a Touch Zones per-zone curve exits to `TzCards`. Set at every
    /// curve entry so the shared curve driver serves both.
    pub curve_return_level: EditLevel,
    /// Touch Zones line editing (`TzLines`/`TzGrab`). `tz_field` = focused pad
    /// (0 or 1, split mode); `tz_axis` = 0 for a COLUMN divider (vertical line,
    /// moves in X), 1 for a ROW divider (horizontal line, moves in Y); `tz_line`
    /// = index into that axis's interior-edges array. Clamped each frame.
    pub tz_field: usize,
    pub tz_axis: u8,
    pub tz_line: usize,
    /// Virtual KB/M picker (opened from a Remapper's Special slot). When open,
    /// the modal grid captures nav input: LS/dpad move the cursor, South appends
    /// the focused pin to the output chord, North resets it, East closes.
    /// `kbm_picker_node`/`_outer` identify the remapper being edited.
    pub kbm_picker_open: bool,
    /// Index into `kbm_picker::KBM_LAYOUT` of the focused cell. The layout uses
    /// absolute (x,y) positions with separated clusters (nav cluster + arrows +
    /// mouse to the right of the main block), so the cursor is a flat cell index
    /// navigated spatially (nearest-in-direction), not a row/col pair.
    pub kbm_picker_idx: usize,
    pub kbm_picker_node: Option<egui_snarl::NodeId>,
    /// Sub-patch path from the tab canvas to the snarl holding
    /// `kbm_picker_node`: node ids of the subpatch chain, outermost first.
    /// Empty = the node sits directly on the tab canvas. Replaces the old
    /// single-level `outer` (which silently mis-addressed nodes nested more
    /// than one sub-patch deep — drafts written to the wrong snarl).
    pub kbm_picker_path: Vec<usize>,
    /// Draft param the picker appends to — `draft_output` for the Remapper,
    /// `_lean_<side>_draft` for a Lean section. And the phase param to flip into
    /// the learning/learning-equivalent state after a pick (Remapper: `ui_phase`
    /// → "learning"; Lean: `_lean_<side>_phase` → "learning").
    pub kbm_picker_draft_key: String,
    pub kbm_picker_phase_key: Option<String>,
    /// Touch Zones variant: hide the touchpad cluster + offer mouse-movement delta.
    pub kbm_picker_touch_zones: bool,
    /// Pin-id prefix whose cells render disabled for this picker session
    /// (e.g. `menu:{id}` so a Virtual Menu can't target itself). `None` = all
    /// cells usable.
    pub kbm_picker_exclude: Option<String>,
    /// Viewport that requested (and therefore draws) the picker: `None` = the
    /// main window; `Some(id)` = a sub-patch editor viewport. The modal must be
    /// rendered inside the requesting viewport or it pops up on the wrong
    /// window (immediate viewports can't share egui Windows).
    pub kbm_picker_viewport: Option<egui::ViewportId>,
    /// Press-mode picker (opened from a mapping card's press-mode field via
    /// South). Modal: up/down move `press_mode_idx`, South applies the
    /// highlighted mode to card `press_mode_card`, East cancels.
    /// `press_mode_outer` identifies the remapper-family widget being edited.
    pub press_mode_open: bool,
    pub press_mode_idx: usize,
    pub press_mode_card: usize,
    pub press_mode_outer: Option<egui_snarl::NodeId>,
    /// Shortcut-chord learn: which shortcut is currently capturing a gamepad
    /// button combo (None = not learning). While Some, nav input is diverted to
    /// chord capture (accumulate held buttons, latch on full release).
    pub chord_learn: Option<ChordTarget>,
    /// Accumulated chord during capture (canonical button pin ids).
    pub chord_draft: Vec<String>,
    /// Arm-idle latch for chord capture: false right after learn starts; flips
    /// true the first frame all buttons are released. Accumulation only begins
    /// once true, so the South press that STARTED the learn (or any held button)
    /// isn't swept into the chord.
    pub chord_arm_idle: bool,
    /// Config overlay (M3.5): index into this frame's published config-pin
    /// targets of the gamepad-focused tweak-pin. `None` = no focus yet (seeds to
    /// the top-left pin on first directional input). The focused pin's upstream
    /// device passes through to the game, so you feel/steer the parameter you're
    /// adjusting. Clamped to the target count each frame.
    pub config_index: Option<usize>,
    /// Config overlay (M3.6): while a config tweak-pin is being EDITED with the
    /// gamepad, this points the shared nav resolvers (`nav_selected_inner_node`
    /// / `nav_selected_element` / `nav_selected_kind`) at the pin instead of the
    /// sub-patch's own `selected_item`, so the existing widget/curve drivers
    /// (`nav_drive_fields`, `nav_drive_curve_dots`/`_dot`) edit the config pin
    /// with identical UX. `(outer sub-patch node, inner node, element_id)`.
    /// `None` = not editing a config pin (sub-patch nav resolves normally).
    pub config_nav_sel: Option<(egui_snarl::NodeId, egui_snarl::NodeId, String)>,
    /// UI-thread keyboard tapper for the Alt+Tab switcher. Independent of the
    /// device pool (which the I/O thread resets while nav-suppression is on).
    #[cfg(windows)]
    pub key_tapper: Option<flexinput_virtual::windows::UiKeyTapper>,
}

impl Default for GamepadNav {
    fn default() -> Self {
        Self {
            mode: HashMap::new(),
            prev_pressed: HashSet::new(),
            repeat_accum: 0.0,
            repeat_dir: None,
            edit_level: EditLevel::Widget,
            active_dev: None,
            fine_increment: false,
            edit_baseline: None,
            cursor_pos: egui::Pos2::ZERO,
            cursor_visible: false,
            cursor_last_move: Instant::now(),
            cursor_tex: None,
            start_down_at: None,
            start_hold_fired: false,
            preset_nav_open: false,
            preset_nav_index: 0,
            preset_nav_select: false,
            preset_nav_step: 0,
            alt_tab_active: false,
            rs_arrow_armed: true,
            prev_lt: false,
            prev_rt: false,
            left_edit: None,
            left_selected: None,
            config_index: None,
            config_nav_sel: None,
            settings_open: false,
            settings_index: 0,
            settings_editing: false,
            field_index: 0,
            curve_dot: 0,
            curve_bias: false,
            card_index: 0,
            remap_card: 0,
            card_field: 0,
            card_return_level: EditLevel::RemapScroll,
            curve_return_level: EditLevel::Widget,
            tz_field: 0,
            tz_axis: 0,
            tz_line: 0,
            kbm_picker_open: false,
            kbm_picker_idx: 0,
            kbm_picker_node: None,
            kbm_picker_path: Vec::new(),
            kbm_picker_draft_key: String::from("draft_output"),
            kbm_picker_phase_key: None,
            kbm_picker_touch_zones: false,
            kbm_picker_exclude: None,
            kbm_picker_viewport: None,
            press_mode_open: false,
            press_mode_idx: 0,
            press_mode_card: 0,
            press_mode_outer: None,
            chord_learn: None,
            chord_draft: Vec::new(),
            chord_arm_idle: false,
            #[cfg(windows)]
            key_tapper: None,
        }
    }
}

/// Canonical bool pins we track for navigation (face/shoulder/stick-click/
/// menu/dpad buttons). Triggers and sticks are read as analog, separately.
// NOTE: pin ids must match what the gilrs backend actually emits
// (`crates/devices/src/gilrs_backend.rs` BUTTON_MAP_*), NOT the names in
// `gamepad.rs::standard_outputs()` — the backend uses `btn_ls`/`btn_rs` for
// stick clicks, `btn_back` for Select/minus, and `btn_guide` for Mode/Home.
pub const NAV_BUTTONS: &[&str] = &[
    "btn_south",
    "btn_east",
    "btn_west",
    "btn_north",
    "btn_lb",
    "btn_rb",
    "btn_ls",
    "btn_rs",
    "btn_start",
    "btn_back",
    "dpad_up",
    "dpad_down",
    "dpad_left",
    "dpad_right",
];

/// One frame's worth of navigation input for a single device.
#[derive(Clone)]
pub struct NavInput {
    /// Bool pins currently asserted.
    pub pressed: HashSet<String>,
    /// Pins that went from released → pressed this frame.
    pub rising: HashSet<String>,
    pub lstick: egui::Vec2,
    pub rstick: egui::Vec2,
    pub lt: f32,
    pub rt: f32,
    /// Local pitch (gyro_y) / yaw (gyro_z), raw deg/s-ish units.
    pub gyro_pitch: f32,
    pub gyro_yaw: f32,
}

impl NavInput {
    pub fn is_rising(&self, pin: &str) -> bool {
        self.rising.contains(pin)
    }
}

fn sig_vec2(signals: &HashMap<(String, String), Signal>, dev: &str, pin: &str) -> egui::Vec2 {
    match signals.get(&(dev.to_string(), pin.to_string())) {
        Some(Signal::Vec2(v)) => egui::vec2(v.x, v.y),
        _ => egui::Vec2::ZERO,
    }
}

fn sig_float(signals: &HashMap<(String, String), Signal>, dev: &str, pin: &str) -> f32 {
    signals
        .get(&(dev.to_string(), pin.to_string()))
        .map(|s| s.as_float())
        .unwrap_or(0.0)
}

fn sig_bool(signals: &HashMap<(String, String), Signal>, dev: &str, pin: &str) -> bool {
    signals
        .get(&(dev.to_string(), pin.to_string()))
        .map(|s| s.as_bool())
        .unwrap_or(false)
}

/// Snapshot the nav-relevant signals for `dev_id` and compute rising edges
/// against `prev_pressed`. Caller stores `out.pressed` back into
/// `prev_pressed` at the end of the frame.
pub fn read_nav_input(
    signals: &HashMap<(String, String), Signal>,
    dev_id: &str,
    prev_pressed: &HashSet<String>,
) -> NavInput {
    let mut pressed = HashSet::new();
    for &pin in NAV_BUTTONS {
        if sig_bool(signals, dev_id, pin) {
            pressed.insert(pin.to_string());
        }
    }
    // D-pad fallback: many pads (Switch Pro, DS4/DualSense) report the d-pad as
    // a HAT axis (`dpad` Vec2 / `dpad_x` / `dpad_y`) rather than the four digital
    // `dpad_*` button pins. Synthesize the digital pins from the axis so nav
    // sees discrete d-pad presses regardless of device. +y = up (gilrs).
    {
        let (dx, dy);
        let v = sig_vec2(signals, dev_id, "dpad");
        if v != egui::Vec2::ZERO {
            dx = v.x;
            dy = v.y;
        } else {
            dx = sig_float(signals, dev_id, "dpad_x");
            dy = sig_float(signals, dev_id, "dpad_y");
        }
        const T: f32 = 0.5;
        if dx > T { pressed.insert("dpad_right".into()); }
        if dx < -T { pressed.insert("dpad_left".into()); }
        if dy > T { pressed.insert("dpad_up".into()); }
        if dy < -T { pressed.insert("dpad_down".into()); }
    }
    let rising: HashSet<String> = pressed.difference(prev_pressed).cloned().collect();

    // Triggers: analog when available, else the digital ZL/ZR buttons (Switch
    // Pro is digital-only and emits `btn_lt_dig`/`btn_rt_dig`, never the analog
    // `left_trigger`/`right_trigger` pins).
    let lt = {
        let a = sig_float(signals, dev_id, "left_trigger");
        if a > 0.0 { a } else if sig_bool(signals, dev_id, "btn_lt_dig") { 1.0 } else { 0.0 }
    };
    let rt = {
        let a = sig_float(signals, dev_id, "right_trigger");
        if a > 0.0 { a } else if sig_bool(signals, dev_id, "btn_rt_dig") { 1.0 } else { 0.0 }
    };

    NavInput {
        pressed,
        rising,
        lstick: sig_vec2(signals, dev_id, "left_stick"),
        rstick: sig_vec2(signals, dev_id, "right_stick"),
        lt,
        rt,
        gyro_pitch: sig_float(signals, dev_id, "gyro_y"),
        gyro_yaw: sig_float(signals, dev_id, "gyro_z"),
    }
}

/// True if the device shows any nav activity this frame (used to pick the
/// single active device among several nav-enabled pads, and to decide
/// suppression).
#[allow(dead_code)]
pub fn has_activity(nav: &NavInput) -> bool {
    !nav.pressed.is_empty()
        || nav.lstick.length() > 0.3
        || nav.rstick.length() > 0.1
        || nav.lt > 0.5
        || nav.rt > 0.5
}

fn item_center(item: &LayoutItem) -> egui::Vec2 {
    let (p, s) = item.bbox();
    egui::vec2(p[0] + s[0] * 0.5, p[1] + s[1] * 0.5)
}

/// Spatial neighbour-finding: from `cur`, pick the nearest item in screen
/// direction `dir` using body-local centres. Returns the top-left-most item
/// when there is no current selection. `None` when no candidate lies in the
/// pressed direction.
pub fn nearest_in_dir(items: &[LayoutItem], cur: Option<usize>, dir: NavDir) -> Option<usize> {
    // Only Module items are "navigable widgets"; decorations are passive.
    let navigable = |i: usize| matches!(items.get(i), Some(LayoutItem::Module(_)));

    let cur = match cur.filter(|&i| i < items.len()) {
        Some(i) => i,
        None => {
            // No selection → top-left-most navigable item.
            return items
                .iter()
                .enumerate()
                .filter(|(i, _)| navigable(*i))
                .min_by(|(_, a), (_, b)| {
                    let ca = item_center(a);
                    let cb = item_center(b);
                    (ca.y, ca.x)
                        .partial_cmp(&(cb.y, cb.x))
                        .unwrap_or(std::cmp::Ordering::Equal)
                })
                .map(|(i, _)| i);
        }
    };

    let c = item_center(&items[cur]);
    let mut best: Option<(usize, f32)> = None;

    for (i, item) in items.iter().enumerate() {
        if i == cur || !navigable(i) {
            continue;
        }
        let o = item_center(item);
        let dx = o.x - c.x;
        let dy = o.y - c.y;
        // primary = distance along the pressed axis (must be positive);
        // cross = lateral offset (penalised so straight-ahead wins).
        let (primary, cross) = match dir {
            NavDir::Right => (dx, dy),
            NavDir::Left => (-dx, dy),
            NavDir::Down => (dy, dx),
            NavDir::Up => (-dy, dx),
        };
        if primary <= 0.0 {
            continue;
        }
        let score = primary + 2.0 * cross.abs();
        if best.map(|(_, s)| score < s).unwrap_or(true) {
            best = Some((i, score));
        }
    }

    best.map(|(i, _)| i)
}

/// Spatial neighbour-finding for the left I/O panel: from `cur` (an index into
/// `targets`), pick the nearest target in screen direction `dir` using each
/// target's rect centre. Mirrors `nearest_in_dir`'s primary-axis + penalised-
/// cross scoring, but over screen rects. With `cur = None`, seeds at the
/// top-left-most target. `None` when nothing lies in the pressed direction.
pub fn nearest_target_rect_in_dir(
    targets: &[LeftNavTarget],
    cur: Option<usize>,
    dir: NavDir,
) -> Option<usize> {
    if targets.is_empty() {
        return None;
    }
    let cur = match cur.filter(|&i| i < targets.len()) {
        Some(i) => i,
        None => {
            // No selection → top-left-most target.
            return targets
                .iter()
                .enumerate()
                .min_by(|(_, a), (_, b)| {
                    let ca = a.rect.center();
                    let cb = b.rect.center();
                    (ca.y, ca.x)
                        .partial_cmp(&(cb.y, cb.x))
                        .unwrap_or(std::cmp::Ordering::Equal)
                })
                .map(|(i, _)| i);
        }
    };

    let c = targets[cur].rect.center();
    let mut best: Option<(usize, f32)> = None;
    for (i, t) in targets.iter().enumerate() {
        if i == cur {
            continue;
        }
        let o = t.rect.center();
        let dx = o.x - c.x;
        let dy = o.y - c.y;
        let (primary, cross) = match dir {
            NavDir::Right => (dx, dy),
            NavDir::Left => (-dx, dy),
            NavDir::Down => (dy, dx),
            NavDir::Up => (-dy, dx),
        };
        if primary <= 0.0 {
            continue;
        }
        let score = primary + 2.0 * cross.abs();
        if best.map(|(_, s)| score < s).unwrap_or(true) {
            best = Some((i, score));
        }
    }
    best.map(|(i, _)| i)
}

/// Map a left-stick vector to a dominant direction once it clears the
/// engage threshold. Returns `None` when near-centred.
pub fn stick_dir(v: egui::Vec2) -> Option<NavDir> {
    if v.length() < 0.5 {
        return None;
    }
    if v.x.abs() >= v.y.abs() {
        Some(if v.x >= 0.0 { NavDir::Right } else { NavDir::Left })
    } else {
        // Screen Y grows downward; gamepad up-stick is +y in our convention.
        Some(if v.y >= 0.0 { NavDir::Up } else { NavDir::Down })
    }
}

#[cfg(test)]
mod chord_fire_tests {
    use super::*;
    use std::time::{Duration, Instant};

    #[test]
    fn down_fires_once_on_press_edge() {
        let mut st = ChordFireState::default();
        let t = Instant::now();
        // First frame held → fire; subsequent held frames → no fire.
        assert!(chord_fire(&mut st, true, "down", 200.0, t));
        assert!(!chord_fire(&mut st, true, "down", 200.0, t));
        // Release then press again → fires again.
        assert!(!chord_fire(&mut st, false, "down", 200.0, t));
        assert!(chord_fire(&mut st, true, "down", 200.0, t));
    }

    #[test]
    fn long_fires_once_after_hold_elapses() {
        let mut st = ChordFireState::default();
        let t0 = Instant::now();
        // Press: no immediate fire.
        assert!(!chord_fire(&mut st, true, "long", 200.0, t0));
        // Still held, before the gap: no fire.
        assert!(!chord_fire(&mut st, true, "long", 200.0, t0 + Duration::from_millis(100)));
        // Past the gap: fires exactly once.
        assert!(chord_fire(&mut st, true, "long", 200.0, t0 + Duration::from_millis(250)));
        assert!(!chord_fire(&mut st, true, "long", 200.0, t0 + Duration::from_millis(400)));
    }

    #[test]
    fn double_needs_two_taps_within_gap() {
        let mut st = ChordFireState::default();
        let t0 = Instant::now();
        // First tap (press+release): no fire.
        assert!(!chord_fire(&mut st, true, "double", 300.0, t0));
        assert!(!chord_fire(&mut st, false, "double", 300.0, t0 + Duration::from_millis(50)));
        // Second press within the gap: fires.
        assert!(chord_fire(&mut st, true, "double", 300.0, t0 + Duration::from_millis(150)));
    }

    #[test]
    fn double_too_slow_does_not_fire() {
        let mut st = ChordFireState::default();
        let t0 = Instant::now();
        assert!(!chord_fire(&mut st, true, "double", 300.0, t0));
        assert!(!chord_fire(&mut st, false, "double", 300.0, t0 + Duration::from_millis(50)));
        // Second press after the gap: treated as a fresh first tap, no fire.
        assert!(!chord_fire(&mut st, true, "double", 300.0, t0 + Duration::from_millis(500)));
    }
}
