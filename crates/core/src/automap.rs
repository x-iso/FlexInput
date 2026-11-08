/// Semantically equivalent pin-ID groups for any remaining cross-family differences.
/// All button families now use positional names (btn_south/east/west/north, btn_lb/rb,
/// btn_ls/rs, btn_start/back/guide) so most cross-family connections work via direct
/// ID match. Only truly unique cross-device aliases stay here.
const SEMANTIC_GROUPS: &[&[&str]] = &[
    // Device-unique special buttons that share purpose across families.
    &["btn_capture", "btn_mute"],
    // Digital triggers auto-map to analog trigger pins (Bool → Float coercion applied separately).
    &["btn_lt_dig", "left_trigger"],
    &["btn_rt_dig", "right_trigger"],
];

/// Feedback pin pairs that flow BACKWARD along an AutoMap wire.
///
/// When an AutoMap wire connects a physical device.source → virtual device.sink (carrying
/// forward gamepad signals: sticks, buttons, etc.), feedback signals (rumble, lightbar,
/// LEDs) flow back along the same wire in the reverse direction. This is silent and
/// automatic — no separate wires required, no UI changes.
///
/// Each entry: (virtual_output_pin_id, &[matching_physical_input_pin_ids]).
/// The engine looks up the virtual sink's output signal under `virtual_output_pin_id`
/// and, if a matching physical haptic input pin exists, routes the value there.
///
/// To extend: add a new entry for any new device-specific haptic input pin alias.
/// Example: when a new controller adopts pin "trigger_l_rumble", add it to the
/// rumble_strong entry alongside "hd_l_amp" / "hd_rumble_l".
///
/// DualSense note: `ds_l_amp`/`ds_r_amp` are NOT listed here. DualSense already
/// exposes `rumble_strong`/`rumble_weak` (classic motors, XInput-compatible),
/// which is the right fallback when a generic patch wires `rumble_strong`.
/// To drive HD haptics, the user explicitly wires `ds_l_amp` + `ds_l_freq`
/// (and the R pair) in their patch — same shape as Switch Pro's HD rumble.
pub const FEEDBACK_PAIRS: &[(&str, &[&str])] = &[
    // One-shot clicks, on a pad without those actuators: a short rumble.
    ("tp_l_click",   &["rumble_weak", "hd_l_amp"]),
    ("tp_r_click",   &["rumble_weak", "hd_r_amp"]),
    ("grip_l_click", &["rumble_weak", "hd_l_amp"]),
    ("grip_r_click", &["rumble_weak", "hd_r_amp"]),
    ("rumble_strong", &["rumble_strong", "hd_l_amp", "hd_rumble_l"]),
    ("rumble_weak",   &["rumble_weak",   "hd_r_amp", "hd_rumble_r"]),
    // HD-rumble first carrier, as a virtual DualSense with USB audio measures it
    // from the haptic waveform a game streams to it. Direct id match: pads with
    // HD actuators (Switch Pro, a USB DualSense) take it; the engine combines it
    // with any classic rumble on the same pin by magnitude.
    ("hd_l_amp",  &["hd_l_amp"]),
    ("hd_l_freq", &["hd_l_freq"]),
    ("hd_r_amp",  &["hd_r_amp"]),
    ("hd_r_freq", &["hd_r_freq"]),
    // HD-rumble second carrier (HF). Direct id match — only pads that expose the
    // hd2_* inputs (Switch Pro, DualSense) receive it; others drop it silently and
    // play carrier 1 alone.
    ("hd2_l_amp",  &["hd2_l_amp"]),
    ("hd2_l_freq", &["hd2_l_freq"]),
    ("hd2_r_amp",  &["hd2_r_amp"]),
    ("hd2_r_freq", &["hd2_r_freq"]),
    ("lightbar_r",    &["lightbar_r"]),
    ("lightbar_g",    &["lightbar_g"]),
    ("lightbar_b",    &["lightbar_b"]),
    // DualSense LEDs + adaptive triggers: a virtual DualSense surfaces these in
    // its output report decode (`poll_outputs`); forward each to the same-named
    // physical haptic input pin. Direct id match — the destination is another
    // DualSense (DS4 lacks these pins, so they're silently dropped there).
    ("player_led",         &["player_led"]),
    ("mic_led",            &["mic_led"]),
    ("trigger_r_mode",     &["trigger_r_mode"]),
    ("trigger_r_start",    &["trigger_r_start"]),
    ("trigger_r_end",      &["trigger_r_end"]),
    ("trigger_r_strength", &["trigger_r_strength"]),
    ("trigger_r_freq",     &["trigger_r_freq"]),
    ("trigger_r_strength2", &["trigger_r_strength2"]),
    ("trigger_r_period",   &["trigger_r_period"]),
    ("trigger_l_mode",     &["trigger_l_mode"]),
    ("trigger_l_start",    &["trigger_l_start"]),
    ("trigger_l_end",      &["trigger_l_end"]),
    ("trigger_l_strength", &["trigger_l_strength"]),
    ("trigger_l_freq",     &["trigger_l_freq"]),
    ("trigger_l_strength2", &["trigger_l_strength2"]),
    ("trigger_l_period",   &["trigger_l_period"]),
];

/// Resolve feedback pin-id mapping for the AutoMap reverse-flow channel.
/// Given a virtual sink's output pin id (e.g. "rumble_strong"), returns the
/// first matching pin from `physical_input_pins` (e.g. "hd_l_amp" on Switch Pro).
pub fn resolve_feedback_pin<'a>(
    virtual_out_pin: &str,
    physical_input_pins: &[&'a str],
) -> Option<&'a str> {
    let entry = FEEDBACK_PAIRS.iter().find(|(src, _)| *src == virtual_out_pin)?;
    for &candidate in entry.1 {
        if let Some(&p) = physical_input_pins.iter().find(|&&p| p == candidate) {
            return Some(p);
        }
    }
    None
}

/// A kind of feedback a module takes over from the game as a whole.
///
/// Overriding any pin of a group drops the game's feedback for the WHOLE group
/// on that device. Rumble is one sensation whether a pad plays it on classic
/// motors or HD pins: a game's `rumble_strong` can land on a DualSense's
/// `rumble_strong` while a module drives its `hd_l_amp`, and overriding only the
/// pin the module writes would leave the game's rumble playing alongside.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum FeedbackGroup {
    Rumble,
    LightBar,
    PlayerLed,
    MicLed,
    TriggerLeft,
    TriggerRight,
    Trackpad,
}

/// The feedback group a haptic / feedback pin belongs to (see [`FeedbackGroup`]).
pub fn feedback_group(pin: &str) -> Option<FeedbackGroup> {
    use FeedbackGroup::*;
    Some(match pin {
        "lightbar_r" | "lightbar_g" | "lightbar_b" => LightBar,
        "player_led" => PlayerLed,
        "mic_led" => MicLed,
        "rumble_strong" | "rumble_weak" => Rumble,
        p if p.starts_with("trigger_l_") => TriggerLeft,
        p if p.starts_with("trigger_r_") => TriggerRight,
        p if p.starts_with("hd_") || p.starts_with("hd2_") || p.starts_with("ds_") => Rumble,
        p if p.starts_with("tp_") => Trackpad,
        "grip_l_click" | "grip_r_click" => Rumble,
        _ => return None,
    })
}

/// A DualSense adaptive-trigger effect, as carried on the `trigger_*_mode` pin.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TriggerMode {
    Off,
    /// Resistance that builds from `start` to a snap at `end`, then lets go.
    Bow,
    /// Constant resistance from `start` on.
    Feedback,
    /// Two pulses — `strength` then `strength2` — repeated at `freq`.
    Galloping,
    /// A click at `start`, releasing at `end`.
    Weapon,
    /// Vibration alternating between `strength` and `strength2` at `freq`,
    /// every `period`.
    Machine,
    /// Vibration from `start` on, at `freq`.
    Vibration,
}

impl TriggerMode {
    /// In pin order: the value of mode `m` on the pin is `index / 6`.
    pub const ALL: [TriggerMode; 7] = [
        TriggerMode::Off,
        TriggerMode::Bow,
        TriggerMode::Feedback,
        TriggerMode::Galloping,
        TriggerMode::Weapon,
        TriggerMode::Machine,
        TriggerMode::Vibration,
    ];

    /// The mode a `trigger_*_mode` pin value names — the nearest sixth.
    pub fn from_pin(v: f32) -> TriggerMode {
        let i = (v.clamp(0.0, 1.0) * 6.0).round() as usize;
        Self::ALL[i.min(6)]
    }

    /// The pin value that names this mode.
    pub fn pin(self) -> f32 {
        Self::ALL.iter().position(|m| *m == self).unwrap_or(0) as f32 / 6.0
    }
}

/// A single auto-mappable signal in the canonical gamepad bus.
pub struct AutoMapPin {
    pub id: &'static str,
    pub display_name: &'static str,
    pub signal_type: SignalType,
}

use crate::SignalType;

/// The Misc pins, in the order SDL hands its spare button slots out.
pub const MISC_PINS: [&str; 6] = [
    "btn_misc1", "btn_misc2", "btn_misc3", "btn_misc4", "btn_misc5", "btn_misc6",
];

/// The capacitive sensors a backend can mark with [`capacitive_misc_mask`], in
/// bit order (bit n = `CAPACITIVE_PINS[n]`). The first four are those sensors
/// where SDL puts them — on its Misc slots — and the last four are the same
/// sensors as the raw Steam Controller reader names them: a pad's pins are
/// named by whoever reads it, and a Steam Controller read off its own reports
/// has no Misc slots at all.
pub const CAPACITIVE_PINS: [&str; 8] = [
    "btn_misc3", "btn_misc4", "btn_misc5", "btn_misc6",
    "btn_cap_l", "btn_cap_r", "btn_grip_l", "btn_grip_r",
];

/// The sensors the raw Steam Controller reader publishes: stick caps and grips,
/// under their own names.
pub const STEAM_RAW_CAPACITIVE_MASK: u8 = 0b1111_0000;

/// Which of a pad's Misc pins are CAPACITIVE touch sensors (stick caps, grips)
/// rather than switches, as a bitmask over [`CAPACITIVE_PINS`]. 0 for every pad
/// without them. The raw Steam Controller reader publishes
/// [`STEAM_RAW_CAPACITIVE_MASK`] for the same sensors instead.
///
/// These are what the device.source "Ignore capacitive touch in Learn" toggle
/// (`suppress_touch_misc` param) hides from Learn. They assert just from holding
/// the pad, so they hijack a Remapper / Touch Zones / Lean "Learn" capture before
/// the user can press the button they meant. Only capture ignores them: a mapping
/// bound to one still fires, chosen from the picker.
///
/// It has to be a per-device table because SDL hands the Misc slots out per
/// pad: on a GameCube-style pad Misc 3/4 are the trigger buttons, and on a
/// Flydigi Vader Misc 2-6 are real macro buttons. Slots are taken from SDL
/// 3.4's `SDL_gamepad.c` mappings:
/// - Steam Controller (2nd gen, "Triton", incl. its dongles): Misc 3/4 = left /
///   right stick touch, Misc 5/6 = left / right grip touch.
/// - Steam Deck: Misc 3/4 = left / right stick touch.
/// - HORI Wireless HORIPAD for Steam: Misc 3/4 = capsense left / right stick.
///
/// Trackpads are deliberately NOT here: finger contact is how a pad is meant to
/// be used, and the pad CLICK (Misc 2 on a Steam Controller / Deck) is a switch.
pub fn capacitive_misc_mask(vid: u16, pid: u16) -> u8 {
    const STICKS: u8 = 0b0000_0011; // misc3, misc4
    const GRIPS: u8 = 0b0000_1100;  // misc5, misc6
    match (vid, pid) {
        (0x28DE, 0x1302..=0x1305) => STICKS | GRIPS,
        (0x28DE, 0x1205) => STICKS,
        (0x0F0D, 0x01AB) | (0x0F0D, 0x0196) => STICKS,
        _ => 0,
    }
}

/// The pins a [`capacitive_misc_mask`] names.
pub fn capacitive_pins(mask: u8) -> impl Iterator<Item = &'static str> {
    CAPACITIVE_PINS.iter().enumerate().filter(move |(i, _)| mask & (1 << i) != 0).map(|(_, p)| *p)
}

/// Synthetic per-device signal carrying that device's [`capacitive_misc_mask`]
/// (as a Float), published by the backend that knows its vid/pid. The engine and
/// the UI both see the device signals but not its identity, so this is how they
/// learn which pins the capacitive mute covers. Not a real pin — nothing looks
/// it up by name except those two mutes, and it is absent from `ALL_PINS`, so
/// no bus, Learn or picker ever carries it.
pub const CAPACITIVE_MASK_PIN: &str = "__capacitive_misc";

/// Synthetic signal the UI sets on a device with "Ignore capacitive touch in
/// Learn" on, carrying its [`capacitive_misc_mask`]: the pins Learn skips. In the
/// UI's live signals only, and like [`CAPACITIVE_MASK_PIN`] no real pin.
pub const CAPACITIVE_LEARN_MUTE_PIN: &str = "__capacitive_learn_mute";

/// Synthetic per-device signal from a pad whose two touch fields are two
/// touchpads of their own (a Steam Controller: `touch1` the left, `touch2` the
/// right), rather than two fingers on one; any value. Published by the backend,
/// like [`CAPACITIVE_MASK_PIN`], and no real pin.
pub const TOUCH_PADS_PIN: &str = "__touch_pads";

/// A trackpad's press, left then right on a Steam Controller: 0 untouched, 1 at
/// the force it clicks at (the set click force, else its own click's, once seen),
/// and held there past it. Real pins, on the bus like a trigger.
pub const TOUCH_PRESSURE_PINS: [&str; 2] = ["touch1_pressure", "touch2_pressure"];
/// The same pads' pressure as the sensor gives it (0–1, SDL's scale), and the
/// force their own click was seen to take, once it has clicked: for the
/// Calibration window, like [`CAPACITIVE_MASK_PIN`] no real pins.
pub const TOUCH_RAW_PRESSURE_PINS: [&str; 2] = ["__touch1_pressure", "__touch2_pressure"];
pub const TOUCH_OWN_FORCE_PINS: [&str; 2] = ["__touch1_own_force", "__touch2_own_force"];

/// The press pin that is a trackpad click's analog side, on a pad that has one:
/// the left pad's click (and the zone clicks made of it) and the right's. A
/// mapping in the Analog press mode reads the press through it.
pub fn click_pressure_pin(pin: &str) -> Option<&'static str> {
    match pin {
        "btn_touchpad" | "touchpad_left" | "touchpad_center" | "touchpad_right" | "touchpad_any" => {
            Some(TOUCH_PRESSURE_PINS[0])
        }
        "btn_touchpad2" | "btn_misc2" => Some(TOUCH_PRESSURE_PINS[1]),
        _ => None,
    }
}

/// A Steam Controller's trackpad tracking until set otherwise (Calibration
/// window): stabilisation, 0 off to 1 the most (`tp_stabilize`); each pad's
/// click force, 0 = the pad's own click (`tp_click_force_l` / `_r`, which fall
/// back to the `tp_click_force` both had before); and click stabilisation,
/// holding the pointer from this share of the click's force, 0 off
/// (`tp_click_hold`). The backend takes them on pins of the same names.
pub const TRACKPAD_STABILIZE: f32 = 0.5;
pub const TRACKPAD_CLICK_FORCE: f32 = 0.0;
pub const TRACKPAD_CLICK_HOLD: f32 = 0.6;
/// Each of those settings: its param (and pin) and its stock value.
pub const TRACKPAD_TRACKING: [(&str, f32); 4] = [
    ("tp_stabilize", TRACKPAD_STABILIZE),
    ("tp_click_force_l", TRACKPAD_CLICK_FORCE),
    ("tp_click_force_r", TRACKPAD_CLICK_FORCE),
    ("tp_click_hold", TRACKPAD_CLICK_HOLD),
];

/// A trackpad's click feedback settings (Calibration window) as device-node
/// params: on, on release too, strong, tone, strength (dB), pitch, length and
/// softness. With `pad_click_split` on, each pad has its own under the same name
/// plus `_l` / `_r`, falling back to the shared one where it has none.
pub const PAD_CLICK_PARAMS: [&str; 8] = [
    "pad_click_on", "pad_click_release", "pad_click_strong", "pad_click_tone",
    "pad_click_gain_db", "pad_click_hz", "pad_click_ms", "pad_click_soft",
];
pub const PAD_CLICK_SIDE_PARAMS: [[&str; 8]; 2] = [
    [
        "pad_click_on_l", "pad_click_release_l", "pad_click_strong_l", "pad_click_tone_l",
        "pad_click_gain_db_l", "pad_click_hz_l", "pad_click_ms_l", "pad_click_soft_l",
    ],
    [
        "pad_click_on_r", "pad_click_release_r", "pad_click_strong_r", "pad_click_tone_r",
        "pad_click_gain_db_r", "pad_click_hz_r", "pad_click_ms_r", "pad_click_soft_r",
    ],
];
/// What carries each pad's click feedback to the backend: the mode bits below,
/// the strength in dB, pitch, length and softness, left then right.
pub const PAD_CLICK_PINS: [[&str; 5]; 2] = [
    ["pad_click_mode_l", "pad_click_gain_l", "pad_click_hz_l", "pad_click_ms_l", "pad_click_soft_l"],
    ["pad_click_mode_r", "pad_click_gain_r", "pad_click_hz_r", "pad_click_ms_r", "pad_click_soft_r"],
];

/// What a Steam Controller's pad clicks and sensors are: `(full name, picker cell
/// name)`. They have one name on the bus, as they mean other things on other pads.
/// Read off its own reports the pad names them itself — the left pad's click on
/// `touchpad`, the right's on `touchpad2`, the sensors by what they sense. Read
/// through SDL it has no names of its own, and its slots come out as Misc.
pub fn steam_controller_pin_names(pin: &str) -> Option<(&'static str, &'static str)> {
    match pin {
        "btn_touchpad" => Some(("Pad click L", "Pad L")),
        "btn_touchpad2" | "btn_misc2" => Some(("Pad click R", "Pad R")),
        "btn_cap_l" | "btn_misc3" => Some(("Cap sens L", "Cap L")),
        "btn_cap_r" | "btn_misc4" => Some(("Cap sens R", "Cap R")),
        "btn_grip_l" | "btn_misc5" => Some(("Grip sens L", "Grip L")),
        "btn_grip_r" | "btn_misc6" => Some(("Grip sens R", "Grip R")),
        "touch1_active" => Some(("Pad touch L", "Touch L")),
        "touch2_active" => Some(("Pad touch R", "Touch R")),
        "touch1_pressure" => Some(("Pad press L", "Press L")),
        "touch2_pressure" => Some(("Pad press R", "Press R")),
        // Each pad's own swipe, in the picker's short form for its cell.
        "touch1_swipe_up" => Some(("Pad L swipe up", "Swipe L↑")),
        "touch1_swipe_down" => Some(("Pad L swipe down", "Swipe L↓")),
        "touch1_swipe_left" => Some(("Pad L swipe left", "Swipe L←")),
        "touch1_swipe_right" => Some(("Pad L swipe right", "Swipe L→")),
        "touch2_swipe_up" => Some(("Pad R swipe up", "Swipe R↑")),
        "touch2_swipe_down" => Some(("Pad R swipe down", "Swipe R↓")),
        "touch2_swipe_left" => Some(("Pad R swipe left", "Swipe R←")),
        "touch2_swipe_right" => Some(("Pad R swipe right", "Swipe R→")),
        "touch1_swipe_x" => Some(("Pad L swipe ↔", "Swipe L↔")),
        "touch1_swipe_y" => Some(("Pad L swipe ↕", "Swipe L↕")),
        "touch2_swipe_x" => Some(("Pad R swipe ↔", "Swipe R↔")),
        "touch2_swipe_y" => Some(("Pad R swipe ↕", "Swipe R↕")),
        _ => None,
    }
}

// What the `pad_click_mode` pin carries, as bits, from the engine to the Steam
// Controller's backend (Calibration window, "Click feedback").
pub const PAD_CLICK_ON: u8 = 1;
pub const PAD_CLICK_ON_RELEASE: u8 = 2;
pub const PAD_CLICK_STRONG: u8 = 4;
/// Play a tone of the set pitch and length, rather than the firmware's click.
pub const PAD_CLICK_TONE: u8 = 8;

// What a click plays until set otherwise: its pitch in Hz and its length in ms.
// Shorter feels sharper, longer softer.
/// A trigger click's pitch and length until set otherwise: high and short, which
/// a hand holding the gyro still feels least in its aim.
pub const TRIGGER_CLICK_HZ: f32 = 300.0;
pub const TRIGGER_CLICK_MS: f32 = 5.0;
/// Where a trigger's digital point sits until set otherwise, on the calibrated
/// trigger: early in the pull, the arc's 10% mark.
pub const TRIGGER_DIGITAL_THRESHOLD: f32 = 0.1;
pub const PAD_CLICK_HZ: f32 = 200.0;
pub const PAD_CLICK_MS: f32 = 12.0;
/// A trackpad's click feedback until set otherwise: on, on release too, the pad's
/// light click at this strength.
pub const PAD_CLICK_ON_DEFAULT: bool = true;
pub const PAD_CLICK_RELEASE_DEFAULT: bool = true;
pub const PAD_CLICK_DB: f32 = -7.0;
/// A trigger click's strength until set otherwise: under full, where it starts to
/// shake the gyro's aim.
pub const TRIGGER_CLICK_DB: f32 = -5.0;
/// A trigger's release click, until set otherwise, is 6 dB softer than its press.
pub const TRIGGER_RELEASE_DB: f32 = TRIGGER_CLICK_DB - 6.0;
/// What a trigger click lasts on a pad with rumble motors only, until set
/// otherwise: a motor needs about that long to spin up enough to be felt.
pub const TRIGGER_CLICK_RUMBLE_MS: f32 = 50.0;
/// The pitches a Switch Pro's `hd_*_freq` pins span, 0 to 1: its frequency
/// table (step `i` plays 10·2^((i+65)/32) Hz) from step 32 to its last, 158, an
/// octave every 32 of those 126 steps. Above the low band's top, 626 Hz, the
/// high band plays the pitch alone (`gyro.rs` `switch_carrier_fields`).
pub const SWITCH_HD_HZ: (f32, f32) = (81.75, 1252.6);

/// A pitch as a Switch Pro's `hd_*_freq` value.
pub fn switch_hd_freq(hz: f32) -> f32 {
    ((hz.max(1.0) / SWITCH_HD_HZ.0).log2() * 32.0 / 126.0).clamp(0.0, 1.0)
}

/// The pitch a Switch Pro's rumble plays when nothing gives it one (a game's
/// classic rumble, the ping): near its actuator's resonance, where it is felt
/// most.
pub const SWITCH_RUMBLE_HZ: f32 = 123.0;

/// The `hd_*_amp` value that plays `level` (0–1 of full strength, as a click's
/// dB gives it) on a Switch Pro.
///
/// Its encoder reads the pin as felt strength, a 1.8 power curve onto
/// Nintendo's 100-step amplitude table — and the table already runs on a log
/// scale (dekuNukem's `rumble_data_table.md`, as SDL encodes it): steps 1–15
/// about 19% apart from 0.8% of full, 16–32 an octave from 11.7%, 32–100 an
/// octave every 32 from 23%. A plain level through both crowded every quieter
/// click into the bottom few steps: −24 and −18 dB played alike, at 1%.
pub fn switch_hd_amp(level: f32) -> f32 {
    if level <= 0.0 {
        return 0.0;
    }
    let a = level.min(1.0) * 65535.0;
    let step = if a >= 15067.0 {
        32.0 + 32.0 * (a / 15067.0).log2()
    } else if a >= 7698.0 {
        16.0 + 16.0 * (a / 7698.0).log2()
    } else {
        1.0 + 14.0 * (a / 514.0).ln() / (7372.0f32 / 514.0).ln()
    };
    let step = step.round().clamp(1.0, 100.0);
    // The encoder's step is 1 + round(99 · pin^1.8). At least one byte's worth,
    // so the pin doesn't round to silence on its way to the pad.
    ((step - 1.0) / 99.0).powf(1.0 / 1.8).max(1.5 / 255.0)
}

/// The shortest click a Switch Pro plays reliably: at 30 ms and under, a TT
/// Max dropped them.
pub const SWITCH_CLICK_MIN_MS: f32 = 35.0;
/// Its click's length until set otherwise.
pub const TRIGGER_CLICK_SWITCH_MS: f32 = 40.0;

/// What plays a pad's trigger clicks, by its haptic input pins.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClickActuator {
    /// The pad plays them itself (a Steam Controller): the engine sends where
    /// each trigger clicks and what it plays, and the backend watches its own poll.
    Own,
    /// HD actuators played as a waveform (a DualSense on USB): strength, pitch
    /// and softness.
    Hd,
    /// A Switch Pro's HD rumble: as [`ClickActuator::Hd`], its pitch over
    /// [`SWITCH_HD_HZ`] by its own scale ([`switch_hd_freq`]), its strength by
    /// its own amplitude table ([`switch_hd_amp`]), and no shorter than
    /// [`SWITCH_CLICK_MIN_MS`].
    Switch,
    /// Rumble motors: strength and length only.
    Rumble,
}

impl ClickActuator {
    /// A click's length until set otherwise.
    pub fn default_ms(self) -> f32 {
        match self {
            ClickActuator::Rumble => TRIGGER_CLICK_RUMBLE_MS,
            ClickActuator::Switch => TRIGGER_CLICK_SWITCH_MS,
            ClickActuator::Own | ClickActuator::Hd => TRIGGER_CLICK_MS,
        }
    }

    /// The shortest click it plays, whatever is set.
    pub fn shortest_ms(self) -> f32 {
        if self == ClickActuator::Switch { SWITCH_CLICK_MIN_MS } else { 1.0 }
    }
}

pub fn click_actuator<'a>(pins: impl IntoIterator<Item = &'a str> + Clone) -> Option<ClickActuator> {
    let has = |id: &str| pins.clone().into_iter().any(|p| p == id);
    if has("grip_l_click") {
        Some(ClickActuator::Own)
    } else if has("hd_rumble_l") {
        // The Switch Pro layout's legacy pin.
        Some(ClickActuator::Switch)
    } else if has("hd_l_amp") {
        Some(ClickActuator::Hd)
    } else if has("rumble_strong") || has("rumble_weak") {
        Some(ClickActuator::Rumble)
    } else {
        None
    }
}

/// A one-shot's envelope at `t` through it (0..1): `soft` of it is the fade, half at
/// each end on a raised cosine. 0 is a plain burst, 1 a smooth swell.
pub fn click_envelope(t: f32, soft: f32) -> f32 {
    let edge = t.min(1.0 - t).max(0.0);
    let fade = soft * 0.5;
    if edge >= fade {
        1.0
    } else {
        0.5 - 0.5 * (edge / fade * std::f32::consts::PI).cos()
    }
}

/// How wide the band around a trigger's point is, as a share of its range, until
/// set otherwise (Calibration window, `trig_hysteresis`): so a finger resting on
/// the point, or an unsteady pull, doesn't flicker it.
pub const TRIGGER_HYSTERESIS: f32 = 0.02;

/// How long a trigger stays under its band before it lets go, and off rest
/// before a point the band can't guard presses, in ms, until set otherwise
/// (`trig_debounce_ms`), and the least it can be set to: shorter holds no
/// jitter back. See [`TriggerCrossing::step`].
pub const TRIGGER_DEBOUNCE_MS: f32 = 4.0;

/// A trigger's set debounce (`trig_debounce_ms`, if set) in seconds, no shorter
/// than [`TRIGGER_DEBOUNCE_MS`].
pub fn trigger_debounce_secs(ms: Option<f32>) -> f32 {
    ms.unwrap_or(TRIGGER_DEBOUNCE_MS).max(TRIGGER_DEBOUNCE_MS) / 1000.0
}

/// A trigger's hysteresis, set as a share of its full travel, as a share of its
/// calibrated range `(mn, mx)`, which [`trigger_band`] takes: the band stays as
/// wide on the pad however narrow the range. An uncalibrated range (`mn` ≈ `mx`)
/// is the whole travel.
pub fn trigger_band_width(hysteresis: f32, (mn, mx): (f32, f32)) -> f32 {
    let span = mx - mn;
    if span.abs() < 1e-4 { hysteresis } else { hysteresis / span.abs() }
}

/// The band around a trigger's point on the calibrated trigger (0–1), `(bottom,
/// top)`: `hysteresis` wide (of the calibrated range: see
/// [`trigger_band_width`]), centred on the point, and shifted inside the
/// travel where it would run past either end — so near rest it reaches from
/// rest up to its full width, and near full pull down from it.
pub fn trigger_band(point: f32, hysteresis: f32) -> (f32, f32) {
    let p = point.clamp(0.0, 1.0);
    let h = hysteresis.clamp(0.0, 1.0);
    let bottom = (p - h * 0.5).min(1.0 - h).max(0.0);
    (bottom, (bottom + h).min(1.0))
}

/// A trigger's point and its band as the travel it is read on (from
/// [`trigger_edges`]); see [`TriggerCrossing::step`].
#[derive(Clone, Copy, Default, Debug, PartialEq)]
pub struct TriggerEdges {
    /// A pull from rest presses here: the point itself. 0 = no point.
    pub press: f32,
    /// Let go short of rest, it presses again only here: the band's top.
    pub again: f32,
    /// It lets go under here: the band's bottom.
    pub release: f32,
    /// Under here it is at rest.
    pub rest: f32,
}

/// A point on the calibrated trigger (0–1 of the range `(mn, mx)`, as the
/// digital threshold is) and its [`trigger_band`], as raw travel: just past
/// the point and the band's top (full pull is its own edge), under the band's
/// bottom, at rest. `hysteresis` is a share of the full travel. An
/// uncalibrated range (`mn` ≈ `mx`) is the whole travel.
pub fn trigger_edges(point: f32, (mn, mx): (f32, f32), hysteresis: f32) -> TriggerEdges {
    // Less than any trigger's step: "just past" a point.
    const NUDGE: f32 = 1e-5;
    let width = trigger_band_width(hysteresis, (mn, mx));
    let (mn, span) = if (mx - mn).abs() < 1e-4 { (0.0, 1.0) } else { (mn, mx - mn) };
    // Full pull within rounding is full pull, or it would sit out of reach.
    let past = |v: f32| if v >= 1.0 - 1e-6 { mn + span } else { mn + v * span + NUDGE };
    let (bottom, top) = trigger_band(point, width);
    let rest = mn + NUDGE;
    TriggerEdges {
        press: past(point.clamp(0.0, 1.0)),
        again: past(top),
        release: if bottom <= 0.0 { rest } else { mn + bottom * span },
        rest,
    }
}

/// Where a trigger stands against its point between reads.
#[derive(Clone, Copy, Default, Debug, PartialEq)]
pub struct TriggerCrossing {
    down: bool,
    /// Let go short of rest: the next press waits for the band's top.
    short: bool,
    /// How long it has stayed past the edge it is waiting out: while down, the
    /// band's bottom; while up, a point the band can't guard.
    held: Option<f32>,
}

impl TriggerCrossing {
    pub fn down(&self) -> bool {
        self.down
    }

    /// One read of `travel`, `dt` seconds after the last, against `edges` (no
    /// point forgets): `Some(true)` as it presses, `Some(false)` as it lets go.
    /// A pull from rest presses at the point, even one at rest, which presses on
    /// any movement off it. Once down, it lets go only after staying under the
    /// band's bottom for `hold` seconds; let go short of rest, it presses again
    /// only past the band's top, so jitter across the point is silent both ways.
    /// Where the band reaches rest, nothing under the point guards it, so a
    /// press waits `hold` too: a trigger barely off rest and back is jitter.
    pub fn step(&mut self, edges: &TriggerEdges, travel: f32, dt: f32, hold: f32) -> Option<bool> {
        if edges.press <= 0.0 {
            *self = Self::default();
            return None;
        }
        // Whether it has stayed past an edge for `hold`, from the first read there.
        let waited = |held: &mut Option<f32>| {
            let t = held.map_or(0.0, |t| t + dt);
            *held = Some(t);
            t + 1e-6 >= hold
        };
        if self.down {
            if travel >= edges.release {
                self.held = None;
                return None;
            }
            if !waited(&mut self.held) {
                return None;
            }
            *self = Self { down: false, short: travel >= edges.rest, held: None };
            return Some(false);
        }
        if travel < edges.rest {
            self.short = false;
        }
        if travel < if self.short { edges.again } else { edges.press } {
            self.held = None;
            return None;
        }
        let unguarded = !self.short && edges.release <= edges.rest;
        if unguarded && !waited(&mut self.held) {
            return None;
        }
        *self = Self { down: true, short: false, held: None };
        Some(true)
    }
}

/// A one-shot click pin, and the private pins that shape its next click: pitch in
/// Hz, length in ms, softness 0..1. With none set, it plays the pad's plain click.
pub const CLICK_TONE_PINS: [(&str, [&str; 3]); 4] = [
    ("tp_l_click", ["tp_l_click_hz", "tp_l_click_ms", "tp_l_click_soft"]),
    ("tp_r_click", ["tp_r_click_hz", "tp_r_click_ms", "tp_r_click_soft"]),
    ("grip_l_click", ["grip_l_click_hz", "grip_l_click_ms", "grip_l_click_soft"]),
    ("grip_r_click", ["grip_r_click_hz", "grip_r_click_ms", "grip_r_click_soft"]),
];

/// Read a device's capacitive mask back from a signal map (0 if not published).
pub fn capacitive_mask_of<S: std::hash::BuildHasher>(
    signals: &std::collections::HashMap<(String, String), crate::Signal, S>,
    dev_id: &str,
) -> u8 {
    signals.get(&(dev_id.to_string(), CAPACITIVE_MASK_PIN.to_string()))
        .map(|s| s.as_float() as u8)
        .unwrap_or(0)
}

/// Every haptic / feedback INPUT port across all controller families — the
/// universal set of injection targets exposed as inlets by the Feedback Control
/// module. This is the union of all `inputs_for(kind)` families in
/// `crates/devices/src/layouts.rs`. When injected onto a pad that lacks a given
/// port, the value is silently dropped (the pin simply isn't in the device's
/// haptic-input list — see the engine feedback pass / `resolve_feedback_pin`).
///
/// Keep this in sync with `layouts.rs`; `feedback_inlet_union_covers_all_families`
/// in `crates/devices` (or the core test below) guards against drift.
pub const FEEDBACK_INLET_PINS: &[AutoMapPin] = &[
    // ── Classic rumble (XInput-compatible; every pad) ─────────────────────────
    AutoMapPin { id: "rumble_strong", display_name: "Rumble (strong)",         signal_type: SignalType::Float },
    AutoMapPin { id: "rumble_weak",   display_name: "Rumble (weak)",           signal_type: SignalType::Float },
    // ── Light bar (DS4 / DualSense) ───────────────────────────────────────────
    AutoMapPin { id: "lightbar_r",    display_name: "Light Bar R",             signal_type: SignalType::Float },
    AutoMapPin { id: "lightbar_g",    display_name: "Light Bar G",             signal_type: SignalType::Float },
    AutoMapPin { id: "lightbar_b",    display_name: "Light Bar B",             signal_type: SignalType::Float },
    // ── HD rumble carrier 1 (LF) — per-side amp + carrier freq ────────────────
    // Device-agnostic: a Switch Pro renders this via its HD-rumble table; a
    // DualSense renders it as a PCM sine on the LRA. "carrier 1" = the low band.
    AutoMapPin { id: "hd_l_amp",      display_name: "HD Rumble L: Amplitude",  signal_type: SignalType::Float },
    AutoMapPin { id: "hd_l_freq",     display_name: "HD Rumble L: Frequency",  signal_type: SignalType::Float },
    AutoMapPin { id: "hd_r_amp",      display_name: "HD Rumble R: Amplitude",  signal_type: SignalType::Float },
    AutoMapPin { id: "hd_r_freq",     display_name: "HD Rumble R: Frequency",  signal_type: SignalType::Float },
    AutoMapPin { id: "hd_rumble_l",   display_name: "HD Rumble L (legacy)",    signal_type: SignalType::Float },
    AutoMapPin { id: "hd_rumble_r",   display_name: "HD Rumble R (legacy)",    signal_type: SignalType::Float },
    // ── HD rumble carrier 2 (HF) — the SECOND simultaneous carrier ────────────
    // The Switch Pro / DualSense LRAs each play two carriers at once (HF + LF)
    // that mix on the actuator. Carrier 1 (hd_*) is the low band; these hd2_*
    // pins are the high band. Device-agnostic, same as carrier 1: Switch Pro packs
    // both into one HD-rumble packet (table encode); a DualSense sums two PCM
    // sines per LRA. A sink that ignores carrier 2 simply plays carrier 1 alone.
    AutoMapPin { id: "hd2_l_amp",     display_name: "HD Rumble L: Amp (HF)",   signal_type: SignalType::Float },
    AutoMapPin { id: "hd2_l_freq",    display_name: "HD Rumble L: Freq (HF)",  signal_type: SignalType::Float },
    AutoMapPin { id: "hd2_r_amp",     display_name: "HD Rumble R: Amp (HF)",   signal_type: SignalType::Float },
    AutoMapPin { id: "hd2_r_freq",    display_name: "HD Rumble R: Freq (HF)",  signal_type: SignalType::Float },
    // ── DualSense HD haptics (USB only) ───────────────────────────────────────
    AutoMapPin { id: "ds_l_amp",      display_name: "DS Haptic L: Amplitude",  signal_type: SignalType::Float },
    AutoMapPin { id: "ds_l_freq",     display_name: "DS Haptic L: Frequency",  signal_type: SignalType::Float },
    AutoMapPin { id: "ds_r_amp",      display_name: "DS Haptic R: Amplitude",  signal_type: SignalType::Float },
    AutoMapPin { id: "ds_r_freq",     display_name: "DS Haptic R: Frequency",  signal_type: SignalType::Float },
    // ── DualSense LEDs ────────────────────────────────────────────────────────
    AutoMapPin { id: "player_led",    display_name: "Player LED",              signal_type: SignalType::Float },
    AutoMapPin { id: "mic_led",       display_name: "Mic LED",                 signal_type: SignalType::Float },
    // ── DualSense adaptive triggers ───────────────────────────────────────────
    // All 0..1. `mode` is one of seven effects at sixths — 0 off, 1/6 bow,
    // 2/6 feedback, 3/6 galloping, 4/6 weapon, 5/6 machine, 1 vibration — see
    // `TriggerMode`. The four original effects kept their old values
    // (0, 1/3, 2/3, 1), so anything written for them still means the same.
    // start/end are zones 0-9 (/9); strength and strength2 are the effect's
    // 3-bit forces 0-7 (/7); freq and period are raw bytes (/255). strength2 is
    // bow's snap force, galloping's second foot, machine's second amplitude;
    // period is machine's alone.
    AutoMapPin { id: "trigger_r_mode",     display_name: "R.Trigger Mode",     signal_type: SignalType::Float },
    AutoMapPin { id: "trigger_r_start",    display_name: "R.Trigger Start",    signal_type: SignalType::Float },
    AutoMapPin { id: "trigger_r_end",      display_name: "R.Trigger End",      signal_type: SignalType::Float },
    AutoMapPin { id: "trigger_r_strength", display_name: "R.Trigger Strength", signal_type: SignalType::Float },
    AutoMapPin { id: "trigger_r_freq",     display_name: "R.Trigger Freq",     signal_type: SignalType::Float },
    AutoMapPin { id: "trigger_r_strength2", display_name: "R.Trigger Strength 2", signal_type: SignalType::Float },
    AutoMapPin { id: "trigger_r_period",   display_name: "R.Trigger Period",   signal_type: SignalType::Float },
    AutoMapPin { id: "trigger_l_mode",     display_name: "L.Trigger Mode",     signal_type: SignalType::Float },
    AutoMapPin { id: "trigger_l_start",    display_name: "L.Trigger Start",    signal_type: SignalType::Float },
    AutoMapPin { id: "trigger_l_end",      display_name: "L.Trigger End",      signal_type: SignalType::Float },
    AutoMapPin { id: "trigger_l_strength", display_name: "L.Trigger Strength", signal_type: SignalType::Float },
    AutoMapPin { id: "trigger_l_freq",     display_name: "L.Trigger Freq",     signal_type: SignalType::Float },
    AutoMapPin { id: "trigger_l_strength2", display_name: "L.Trigger Strength 2", signal_type: SignalType::Float },
    AutoMapPin { id: "trigger_l_period",   display_name: "L.Trigger Period",   signal_type: SignalType::Float },
    // ── Steam Controller trackpads ────────────────────────────────────────────
    // Last, as a Feedback Control node's wires sit on these by position.
    AutoMapPin { id: "tp_l_amp",   display_name: "Trackpad L: Amplitude", signal_type: SignalType::Float },
    AutoMapPin { id: "tp_l_freq",  display_name: "Trackpad L: Frequency", signal_type: SignalType::Float },
    AutoMapPin { id: "tp_r_amp",   display_name: "Trackpad R: Amplitude", signal_type: SignalType::Float },
    AutoMapPin { id: "tp_r_freq",  display_name: "Trackpad R: Frequency", signal_type: SignalType::Float },
    AutoMapPin { id: "tp_l_click", display_name: "Trackpad L: Click",     signal_type: SignalType::Bool },
    AutoMapPin { id: "tp_r_click", display_name: "Trackpad R: Click",     signal_type: SignalType::Bool },
    AutoMapPin { id: "grip_l_click", display_name: "Grip L: Click",       signal_type: SignalType::Bool },
    AutoMapPin { id: "grip_r_click", display_name: "Grip R: Click",       signal_type: SignalType::Bool },
];

/// Basic feedback OUTPUT taps the game is requesting on the virtual destination
/// device — exposed as outlets by the Feedback Control module. This is the
/// subset that virtual source-pins actually publish via `poll_outputs`
/// (`*_SOURCE_PINS` in `crates/virtual/src/layouts.rs`): classic rumble plus
/// the DS4/DualSense light bar.
pub const FEEDBACK_OUTLET_PINS: &[AutoMapPin] = &[
    AutoMapPin { id: "rumble_strong", display_name: "Rumble (strong)", signal_type: SignalType::Float },
    AutoMapPin { id: "rumble_weak",   display_name: "Rumble (weak)",   signal_type: SignalType::Float },
    AutoMapPin { id: "lightbar_r",    display_name: "Light Bar R",     signal_type: SignalType::Float },
    AutoMapPin { id: "lightbar_g",    display_name: "Light Bar G",     signal_type: SignalType::Float },
    AutoMapPin { id: "lightbar_b",    display_name: "Light Bar B",     signal_type: SignalType::Float },
];

/// Canonical list of all gamepad signals that AutoMap covers.
/// Used by AutoMap Splitter/Collector UIs and for signal resolution.
/// Order: bundled vectors first, then individual axes, then buttons.
///
/// ⭐ **Append-only.** A network peer sends these slots by position and decodes
/// the common prefix between its build and its peer's (see the layout hash in
/// `crates/net/src/frame.rs`), so a new pin goes at the tail and a mid-list
/// insertion is a wire-format break that only shows up as a warning between two
/// builds.
pub const ALL_PINS: &[AutoMapPin] = &[
    AutoMapPin { id: "left_stick",    display_name: "Left Stick",           signal_type: SignalType::Vec2 },
    AutoMapPin { id: "right_stick",   display_name: "Right Stick",          signal_type: SignalType::Vec2 },
    AutoMapPin { id: "dpad",          display_name: "D-Pad",                signal_type: SignalType::Vec2 },
    AutoMapPin { id: "left_stick_x",  display_name: "L.Stick X",            signal_type: SignalType::Float },
    AutoMapPin { id: "left_stick_y",  display_name: "L.Stick Y",            signal_type: SignalType::Float },
    AutoMapPin { id: "right_stick_x", display_name: "R.Stick X",            signal_type: SignalType::Float },
    AutoMapPin { id: "right_stick_y", display_name: "R.Stick Y",            signal_type: SignalType::Float },
    AutoMapPin { id: "left_trigger",  display_name: "Left Trigger (analog)", signal_type: SignalType::Float },
    AutoMapPin { id: "right_trigger", display_name: "Right Trigger (analog)",signal_type: SignalType::Float },
    AutoMapPin { id: "dpad_x",        display_name: "D-Pad X",              signal_type: SignalType::Float },
    AutoMapPin { id: "dpad_y",        display_name: "D-Pad Y",              signal_type: SignalType::Float },
    AutoMapPin { id: "gyro_x",        display_name: "Gyro X (roll)",        signal_type: SignalType::Float },
    AutoMapPin { id: "gyro_y",        display_name: "Gyro Y (pitch)",       signal_type: SignalType::Float },
    AutoMapPin { id: "gyro_z",        display_name: "Gyro Z (yaw)",         signal_type: SignalType::Float },
    AutoMapPin { id: "accel_x",       display_name: "Accel X",              signal_type: SignalType::Float },
    AutoMapPin { id: "accel_y",       display_name: "Accel Y",              signal_type: SignalType::Float },
    AutoMapPin { id: "accel_z",       display_name: "Accel Z",              signal_type: SignalType::Float },
    // Absolute device orientation as a quaternion (x,y,z,w).
    //
    // Only devices whose firmware fuses the IMU itself can supply this — the
    // Joy-Con 2 reports absolute angles rather than a rate, with yaw held by a
    // magnetometer. Everything else leaves it unset, and consumers fall back to
    // the Gyro 3DOF module's own integrated estimate.
    //
    // It has to live in ALL_PINS or the bus cannot carry it and it could never
    // be picked in a Remapper or Splitter, however the device declares it.
    AutoMapPin { id: "orientation",    display_name: "Orientation (absolute)", signal_type: SignalType::Vec4 },
    // Joy-Con 2 optical mouse sensor: motion in the Virtual KB/M's mouse units
    // (+Y up; Mouse speed 100 = a pixel per count), a contact button (on a
    // surface or covered by a finger) and the raw distance byte. Declared and
    // published by the backend all along, but — like `btn_c` below — dropped
    // before the bus until listed here. Deliberately NOT `mouse*`: a direct
    // match onto the KB/M pins would steer every Joy-Con into the pointer,
    // and code all over reads a `mouse` prefix as a KB/M OUTPUT (the picker's
    // output-only stage, the AutoMap "KB/M" group, JSM's editor guard).
    AutoMapPin { id: "optical_motion",   display_name: "Mouse Sensor XY",       signal_type: SignalType::Vec2 },
    AutoMapPin { id: "optical_x",        display_name: "Mouse Sensor X",        signal_type: SignalType::Float },
    AutoMapPin { id: "optical_y",        display_name: "Mouse Sensor Y",        signal_type: SignalType::Float },
    AutoMapPin { id: "optical_contact",  display_name: "Mouse Sensor Contact",  signal_type: SignalType::Bool },
    AutoMapPin { id: "optical_distance", display_name: "Mouse Sensor Distance", signal_type: SignalType::Float },
    AutoMapPin { id: "btn_south",     display_name: "South",                signal_type: SignalType::Bool },
    AutoMapPin { id: "btn_east",      display_name: "East",                 signal_type: SignalType::Bool },
    AutoMapPin { id: "btn_west",      display_name: "West",                 signal_type: SignalType::Bool },
    AutoMapPin { id: "btn_north",     display_name: "North",                signal_type: SignalType::Bool },
    AutoMapPin { id: "btn_lb",        display_name: "LB",                   signal_type: SignalType::Bool },
    AutoMapPin { id: "btn_rb",        display_name: "RB",                   signal_type: SignalType::Bool },
    AutoMapPin { id: "btn_lt_dig",    display_name: "LT (dig)",             signal_type: SignalType::Bool },
    AutoMapPin { id: "btn_rt_dig",    display_name: "RT (dig)",             signal_type: SignalType::Bool },
    AutoMapPin { id: "btn_ls",        display_name: "L.Stick Click",        signal_type: SignalType::Bool },
    AutoMapPin { id: "btn_rs",        display_name: "R.Stick Click",        signal_type: SignalType::Bool },
    AutoMapPin { id: "btn_start",     display_name: "Start",                signal_type: SignalType::Bool },
    AutoMapPin { id: "btn_back",      display_name: "Back",                 signal_type: SignalType::Bool },
    AutoMapPin { id: "btn_guide",     display_name: "Guide",                signal_type: SignalType::Bool },
    AutoMapPin { id: "btn_capture",   display_name: "Capture",              signal_type: SignalType::Bool },
    AutoMapPin { id: "btn_mute",      display_name: "Mute",                 signal_type: SignalType::Bool },
    AutoMapPin { id: "btn_touchpad",  display_name: "Touchpad Click",       signal_type: SignalType::Bool },
    AutoMapPin { id: "touch1_x",      display_name: "Touch 1 X",            signal_type: SignalType::Float },
    AutoMapPin { id: "touch1_y",      display_name: "Touch 1 Y",            signal_type: SignalType::Float },
    AutoMapPin { id: "touch1_active", display_name: "Touch 1 Active",       signal_type: SignalType::Bool },
    AutoMapPin { id: "touch2_x",      display_name: "Touch 2 X",            signal_type: SignalType::Float },
    AutoMapPin { id: "touch2_y",      display_name: "Touch 2 Y",            signal_type: SignalType::Float },
    AutoMapPin { id: "touch2_active", display_name: "Touch 2 Active",       signal_type: SignalType::Bool },
    AutoMapPin { id: "dpad_up",       display_name: "D-Pad Up",             signal_type: SignalType::Bool },
    AutoMapPin { id: "dpad_down",     display_name: "D-Pad Down",           signal_type: SignalType::Bool },
    AutoMapPin { id: "dpad_left",     display_name: "D-Pad Left",           signal_type: SignalType::Bool },
    AutoMapPin { id: "dpad_right",    display_name: "D-Pad Right",          signal_type: SignalType::Bool },
    // ── Extra buttons: rear paddles + misc (SDL-reported on generic pads) ──────
    // These must appear here (not just in layouts.rs `generic_outputs()`) or they
    // can't be selected inside a Remapper/AutoMap Splitter — the AutoMap bus only
    // carries pins listed in ALL_PINS. IDs are standard/portable (paddle L1/L2…);
    // the display names are the generic on-icon labels (PL1/PR1/PL2/PR2, M1…M6),
    // since SDL exposes no vendor-specific paddle names. Keep in sync with
    // generic_outputs() — guarded by `all_pins_covers_generic_extra_buttons`.
    // ⭐ Switch 2's GameChat button. No positional equivalent on any other
    // pad, so it keeps its own id rather than being aliased onto one — but it
    // MUST be in this list, because a pin absent from here never reaches the
    // bus at all. It was declared by the layout, published by the backend, and
    // silently dropped in between.
    AutoMapPin { id: "btn_c",         display_name: "C (GameChat)",         signal_type: SignalType::Bool },
    AutoMapPin { id: "btn_paddle_l1", display_name: "Paddle L1",            signal_type: SignalType::Bool },
    AutoMapPin { id: "btn_paddle_r1", display_name: "Paddle R1",            signal_type: SignalType::Bool },
    AutoMapPin { id: "btn_paddle_l2", display_name: "Paddle L2",            signal_type: SignalType::Bool },
    AutoMapPin { id: "btn_paddle_r2", display_name: "Paddle R2",            signal_type: SignalType::Bool },
    AutoMapPin { id: "btn_misc1",     display_name: "Misc 1",               signal_type: SignalType::Bool },
    AutoMapPin { id: "btn_misc2",     display_name: "Misc 2",               signal_type: SignalType::Bool },
    AutoMapPin { id: "btn_misc3",     display_name: "Misc 3",               signal_type: SignalType::Bool },
    AutoMapPin { id: "btn_misc4",     display_name: "Misc 4",               signal_type: SignalType::Bool },
    AutoMapPin { id: "btn_misc5",     display_name: "Misc 5",               signal_type: SignalType::Bool },
    AutoMapPin { id: "btn_misc6",     display_name: "Misc 6",               signal_type: SignalType::Bool },
    // ── Virtual Keyboard & Mouse static pins ──────────────────────────────────
    // IDs match KEYMOUSE_DEFAULT_PINS in crates/virtual/src/layouts.rs so they
    // direct-match through resolve_mapping when wired to a virtual KB/M sink.
    AutoMapPin { id: "key_escape",    display_name: "Key: Escape",          signal_type: SignalType::Bool },
    AutoMapPin { id: "key_shift",     display_name: "Key: Shift",           signal_type: SignalType::Bool },
    AutoMapPin { id: "key_ctrl",      display_name: "Key: Ctrl",            signal_type: SignalType::Bool },
    AutoMapPin { id: "key_alt",       display_name: "Key: Alt",             signal_type: SignalType::Bool },
    AutoMapPin { id: "key_win",       display_name: "Key: Win",             signal_type: SignalType::Bool },
    AutoMapPin { id: "mouse_left",    display_name: "Mouse: LMB",           signal_type: SignalType::Bool },
    AutoMapPin { id: "mouse_right",   display_name: "Mouse: RMB",           signal_type: SignalType::Bool },
    AutoMapPin { id: "mouse_middle",  display_name: "Mouse: MMB",           signal_type: SignalType::Bool },
    AutoMapPin { id: "mouse_back",    display_name: "Mouse: Back",          signal_type: SignalType::Bool },
    AutoMapPin { id: "mouse_forward", display_name: "Mouse: Forward",       signal_type: SignalType::Bool },
    AutoMapPin { id: "scroll_up",     display_name: "Mouse: Scroll Up",     signal_type: SignalType::Bool },
    AutoMapPin { id: "scroll_down",   display_name: "Mouse: Scroll Down",   signal_type: SignalType::Bool },
    AutoMapPin { id: "scroll_left",   display_name: "Mouse: Scroll Left",   signal_type: SignalType::Bool },
    AutoMapPin { id: "scroll_right",  display_name: "Mouse: Scroll Right",  signal_type: SignalType::Bool },
    // Analog scroll rate for variable-speed scrolling (stick/trigger/touch-zone
    // deflection → continuous scroll). +Y scrolls up, +X scrolls right.
    AutoMapPin { id: "scroll_y",      display_name: "Mouse: Scroll Y (rate)", signal_type: SignalType::Float },
    AutoMapPin { id: "scroll_x",      display_name: "Mouse: Scroll X (rate)", signal_type: SignalType::Float },
    AutoMapPin { id: "mouse",         display_name: "Mouse: XY (delta)",    signal_type: SignalType::Vec2 },
    AutoMapPin { id: "mouse_x",       display_name: "Mouse: X (delta)",     signal_type: SignalType::Float },
    AutoMapPin { id: "mouse_y",       display_name: "Mouse: Y (delta)",     signal_type: SignalType::Float },
    // Absolute-displacement pointer move (trackpad feel): the value is a one-shot
    // pixel DISPLACEMENT applied directly, NOT a velocity integrated over dt — so
    // it's robust to the async mouse-emit timing (a velocity here flings the cursor
    // when the emit thread re-integrates it). Touch Zones' Touchpad mode drives this.
    AutoMapPin { id: "mouse_move",    display_name: "Mouse: XY (move)",     signal_type: SignalType::Vec2 },
    AutoMapPin { id: "mouse_move_x",  display_name: "Mouse: X (move)",      signal_type: SignalType::Float },
    AutoMapPin { id: "mouse_move_y",  display_name: "Mouse: Y (move)",      signal_type: SignalType::Float },
    // Smooth scroll RATE: scroll_y/x's scale, sent as hi-res wheel instead of
    // whole notches. +Y scrolls up, +X scrolls right.
    AutoMapPin { id: "trackpad_scroll_y", display_name: "Mouse: Smooth Scroll Y (rate)", signal_type: SignalType::Float },
    AutoMapPin { id: "trackpad_scroll_x", display_name: "Mouse: Smooth Scroll X (rate)", signal_type: SignalType::Float },
    // Scroll DISPLACEMENT in notches (fractions allowed), applied once like
    // mouse_move. +Y scrolls up, +X scrolls right.
    AutoMapPin { id: "scroll_move_y", display_name: "Mouse: Scroll Y (move)", signal_type: SignalType::Float },
    AutoMapPin { id: "scroll_move_x", display_name: "Mouse: Scroll X (move)", signal_type: SignalType::Float },
    // A trackpad's press (`TOUCH_PRESSURE_PINS`).
    AutoMapPin { id: "touch1_pressure", display_name: "Touch 1 Pressure", signal_type: SignalType::Float },
    AutoMapPin { id: "touch2_pressure", display_name: "Touch 2 Pressure", signal_type: SignalType::Float },
    // ── A pad's own names, read off its own reports ──────────────────────────
    // The Steam Controller's raw-HID reader names its spare controls after what
    // they are; SDL, reading the same pad, hands them out as Misc slots. Both
    // names are in the bus, so whichever route reads the pad has its pins, and
    // only that pad's layout declares the dedicated ones. Appended at the tail:
    // the net layout is append-only (see `crates/net/src/frame.rs`).
    AutoMapPin { id: "btn_touchpad2", display_name: "Touchpad 2 Click",   signal_type: SignalType::Bool },
    AutoMapPin { id: "btn_cap_l",     display_name: "Cap Sense L",        signal_type: SignalType::Bool },
    AutoMapPin { id: "btn_cap_r",     display_name: "Cap Sense R",        signal_type: SignalType::Bool },
    AutoMapPin { id: "btn_grip_l",    display_name: "Grip Sense L",       signal_type: SignalType::Bool },
    AutoMapPin { id: "btn_grip_r",    display_name: "Grip Sense R",       signal_type: SignalType::Bool },
];

/// Whether `id` is one of [`ALL_PINS`]. A set lookup: the engine asks this per
/// bus entry every tick, where a scan of the list cost a hundred-odd string
/// compares each time.
pub fn is_canonical_pin(id: &str) -> bool {
    known_index(id).is_some_and(|ix| ix < ALL_PINS.len())
}

/// Marks a bus pin as taken over by a mapping module upstream: `{prefix}{pin}`.
pub const CONSUMED_PREFIX: &str = "__consumed__:";

/// Pins with a fixed slot in the engine's dense maps: [`ALL_PINS`] first, at the
/// same indices, then the ones a mapping module derives every tick, then each
/// canonical pin's consumed marker.
pub struct KnownPins {
    pub names: Vec<&'static str>,
    index: std::collections::HashMap<&'static str, u32, crate::FastHasher>,
    consumed_base: usize,
}

/// The slot of `ALL_PINS[pin_ix]`'s consumed marker.
#[inline]
pub fn consumed_marker_index(pin_ix: usize) -> usize {
    known_pins().consumed_base + pin_ix
}

const DERIVED_PINS: &[&str] = &[
    "left_stick_up", "left_stick_down", "left_stick_left", "left_stick_right",
    "right_stick_up", "right_stick_down", "right_stick_left", "right_stick_right",
    "touchpad_left", "touchpad_center", "touchpad_right", "touchpad_any",
    "touch_left", "touch_center", "touch_right",
];

pub fn known_pins() -> &'static KnownPins {
    static KNOWN: std::sync::OnceLock<KnownPins> = std::sync::OnceLock::new();
    KNOWN.get_or_init(|| {
        let mut names: Vec<&'static str> = ALL_PINS.iter().map(|p| p.id).collect();
        for &p in DERIVED_PINS.iter()
            .chain(crate::imu::IMU_PINS.iter().map(|p| &p.id))
            .chain(crate::touchzones::SWIPE_PINS.iter().flatten())
            .chain(crate::touchzones::SWIPE_AXIS_PINS.iter().flatten())
        {
            if !names.contains(&p) {
                names.push(p);
            }
        }
        let consumed_base = names.len();
        names.extend(ALL_PINS.iter().map(|p| -> &'static str {
            Box::leak(format!("{CONSUMED_PREFIX}{}", p.id).into_boxed_str())
        }));
        let index = names.iter().enumerate().map(|(i, &n)| (n, i as u32)).collect();
        KnownPins { names, index, consumed_base }
    })
}

#[inline]
pub fn known_index(id: &str) -> Option<usize> {
    known_pins().index.get(id).map(|&i| i as usize)
}

/// Family-specific button glyph for a cross-family pin, or `None` if the pin
/// is family-neutral. `family_slug` is the middle segment of a gilrs device ID
/// (e.g. `"dualsense"` from `"gilrs:dualsense:0"`). Pass `None` / unknown to get
/// the neutral positional label embedded in [`ALL_PINS`] (e.g. "South").
///
/// Used by the AutoMap Splitter body to swap "South (B/Cross/X)" for the single
/// label that matches the actually-connected upstream device.
pub fn family_label(pin_id: &str, family_slug: Option<&str>) -> Option<&'static str> {
    let fam = family_slug?;
    let (xi, ps, sw) = match pin_id {
        "btn_south"  => ("A", "Cross", "B"),
        "btn_east"   => ("B", "Circle", "A"),
        "btn_west"   => ("X", "Square", "Y"),
        "btn_north"  => ("Y", "Triangle", "X"),
        "btn_lb"     => ("LB", "L1", "L"),
        "btn_rb"     => ("RB", "R1", "R"),
        "btn_lt_dig" => ("LT", "L2", "ZL"),
        "btn_rt_dig" => ("RT", "R2", "ZR"),
        "btn_start"  => ("Start", "Options", "+"),
        "btn_back"   => ("Back", "Share", "−"),
        "btn_guide"  => ("Guide", "PS", "Home"),
        _ => return None,
    };
    Some(match fam {
        "xinput"                  => xi,
        "ds4" | "dualsense"       => ps,
        "switch_pro"              => sw,
        _                         => return None,
    })
}

/// A second trackpad's click, bridged onto a destination with ONE touchpad
/// button. A DualSense / DS4 report has a single touchpad click, so a two-pad
/// controller passed straight through onto one presses it from either pad — as
/// a real touchpad is clicked wherever the finger happens to be. Both sources
/// reach the destination and the engine combines them, which for a button is an
/// OR; unlike a semantic group, the direct match never owns the destination
/// alone.
pub const TOUCHPAD_CLICK_BRIDGE: &[(&str, &str)] = &[("btn_touchpad2", "btn_touchpad")];

/// Given lists of source and destination pin IDs, returns `(src_id, dst_id)` pairs
/// for every auto-mappable signal. A single source pin can map to multiple destination
/// pins when a semantic group bridges digital ↔ analog (e.g. `btn_lt_dig` fires both
/// `btn_lt_dig` AND `left_trigger` on a virtual DS4) — type coercion happens later.
pub fn resolve_mapping<'a>(src_pins: &[&'a str], dst_pins: &[&'a str]) -> Vec<(&'a str, &'a str)> {
    let mut result = Vec::new();
    let mut claimed_dst = std::collections::HashSet::new();

    // Pass 1: direct ID matches across ALL sources first. A direct match (same name on
    // both sides) is the authoritative owner of its destination and must not be pre-empted
    // by an earlier source's semantic fan-out. Doing every direct match before any fan-out
    // makes ordering of `src_pins` irrelevant.
    for &src_id in src_pins {
        if let Some(&dst_id) = dst_pins.iter().find(|&&d| d == src_id) {
            if claimed_dst.insert(dst_id) {
                result.push((src_id, dst_id));
            }
        }
    }

    // Pass 2: semantic group fan-out for destinations a direct match didn't claim.
    // Lets btn_capture↔btn_mute bridge when only one name exists on each side.
    for &src_id in src_pins {
        if let Some(group) = SEMANTIC_GROUPS.iter().find(|g| g.contains(&src_id)).copied() {
            for &group_id in group {
                if group_id == src_id { continue; }
                if let Some(&dst_id) = dst_pins.iter().find(|&&d| d == group_id) {
                    if claimed_dst.insert(dst_id) {
                        result.push((src_id, dst_id));
                    }
                }
            }
        }
    }

    // Pass 3: the digital↔analog trigger bridge is special — both the analog
    // (`left_trigger`/`right_trigger`) and digital (`btn_lt_dig`/`btn_rt_dig`) source pins
    // must reach the analog destination, even though pass 1 already gave it a direct owner.
    // On controllers whose analog trigger is dead (Switch Pro ZL/ZR are digital-only, so
    // `left_trigger` is never published), the engine combines these by magnitude and the
    // live digital press wins. Without this extra mapping the digital source could never
    // drive the analog trigger. Emitted in addition to (not instead of) the direct mapping.
    for &(dig, analog) in &[("btn_lt_dig", "left_trigger"), ("btn_rt_dig", "right_trigger")] {
        let has_dig_src = src_pins.contains(&dig);
        let has_analog_dst = dst_pins.contains(&analog);
        if has_dig_src && has_analog_dst && !result.iter().any(|&(s, d)| s == dig && d == analog) {
            result.push((dig, analog));
        }
    }

    // Pass 4: a second pad's click onto a one-touchpad destination (see
    // [`TOUCHPAD_CLICK_BRIDGE`]). Not a fallback: a pad with two trackpads means
    // BOTH of them when it is passed through onto a pad with one touchpad, so
    // the pair is emitted beside the direct match and the engine ORs the two
    // into the one button.
    for &(second, single) in TOUCHPAD_CLICK_BRIDGE {
        if src_pins.contains(&second)
            && dst_pins.contains(&single)
            // A destination that has its own second click pin is a two-pad one:
            // the direct match already keeps the two apart.
            && !dst_pins.contains(&second)
            && !result.iter().any(|&(s, d)| s == second && d == single)
        {
            result.push((second, single));
        }
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;

    const MS: f32 = 0.001;
    const HOLD: f32 = TRIGGER_DEBOUNCE_MS * MS;

    // A pull from rest presses at the point; jitter across it is silent both
    // ways, the band's bottom lets go and, short of rest, only its top presses
    // again.
    #[test]
    fn a_trigger_presses_at_its_point_and_jitter_across_its_band_is_silent() {
        // A 10% band around the middle: 0.45 to 0.55.
        let e = trigger_edges(0.5, (0.0, 1.0), 0.1);
        let mut t = TriggerCrossing::default();
        let mut step = |v: f32| t.step(&e, v, MS, 0.0);
        assert_eq!(step(0.5), None, "on the point");
        assert_eq!(step(0.501), Some(true), "just past it, from rest");
        assert_eq!(step(0.9), None, "no second press further in");
        assert_eq!(step(0.46), None, "back inside the band");
        assert_eq!(step(0.52), None, "so jitter across the point is silent");
        assert_eq!(step(0.44), Some(false), "past its bottom");
        assert_eq!(step(0.52), None, "short of rest, the point doesn't press again");
        assert_eq!(step(0.551), Some(true), "its top does");
        assert_eq!(step(0.3), Some(false));
        assert_eq!(step(0.0), None, "at rest");
        assert_eq!(step(0.501), Some(true), "so the point presses again");
    }

    // Once down, it lets go only after staying under the band for the hold.
    #[test]
    fn a_trigger_lets_go_only_after_staying_under_its_band_for_the_hold() {
        let e = trigger_edges(0.5, (0.0, 1.0), 0.1);
        let mut t = TriggerCrossing::default();
        let mut step = |v: f32| t.step(&e, v, MS, HOLD);
        assert_eq!(step(0.6), Some(true), "the band guards this point: no wait");
        for _ in 0..3 {
            assert_eq!(step(0.4), None, "a dip shorter than the hold");
        }
        assert_eq!(step(0.6), None, "is jitter");
        for _ in 0..4 {
            assert_eq!(step(0.4), None);
        }
        assert_eq!(step(0.4), Some(false), "4 ms under lets go");
    }

    // A point at rest has no band under it: a press off rest waits the hold
    // too, so a trigger blipping off rest and back, either way, stays quiet.
    #[test]
    fn a_point_at_rest_waits_the_hold_both_ways() {
        let e = trigger_edges(0.0, (0.0, 1.0), 0.1);
        let mut t = TriggerCrossing::default();
        let mut step = |v: f32| t.step(&e, v, MS, HOLD);
        for _ in 0..3 {
            assert_eq!(step(0.001), None, "off rest, shorter than the hold");
            assert_eq!(step(0.0), None, "and back: jitter");
        }
        for _ in 0..4 {
            assert_eq!(step(0.001), None);
        }
        assert_eq!(step(0.002), Some(true), "4 ms off rest presses");
        for _ in 0..3 {
            assert_eq!(step(0.0), None, "a dip to rest shorter than the hold");
        }
        assert_eq!(step(0.001), None, "is jitter");
        for _ in 0..4 {
            assert_eq!(step(0.0), None);
        }
        assert_eq!(step(0.0), Some(false), "4 ms at rest lets go");
    }

    // A digital trigger jumps 0 → 1 → 0, and presses and lets go at any point.
    #[test]
    fn a_digital_trigger_presses_and_releases_at_any_point() {
        for point in [0.0, 0.5, 1.0] {
            let e = trigger_edges(point, (0.0, 1.0), TRIGGER_HYSTERESIS);
            let mut t = TriggerCrossing::default();
            for _ in 0..2 {
                assert_eq!(t.step(&e, 1.0, MS, 0.0), Some(true), "{point}");
                assert_eq!(t.step(&e, 0.0, MS, 0.0), Some(false), "{point}");
            }
        }
    }

    // A point at rest presses on any movement off it, whatever the band, and
    // lets go only at rest, where the trigger sits at or under the range's start.
    #[test]
    fn a_point_at_rest_presses_on_any_movement_and_lets_go_at_rest() {
        let e = trigger_edges(0.0, (0.06, 0.9), 0.1);
        let mut t = TriggerCrossing::default();
        for _ in 0..2 {
            assert_eq!(t.step(&e, 0.055, MS, 0.0), None, "at rest");
            assert_eq!(t.step(&e, 0.061, MS, 0.0), Some(true), "barely off it");
            assert_eq!(t.step(&e, 0.0605, MS, 0.0), None, "barely-pressed jitter holds");
            assert_eq!(t.step(&e, 0.06, MS, 0.0), Some(false), "back at rest");
        }
        assert_eq!(t.step(&TriggerEdges::default(), 0.9, MS, 0.0), None, "no point forgets");
        assert!(!t.down());
    }

    // The band is the hysteresis wide wherever the point sits: centred, and
    // shifted inside the travel near either end.
    #[test]
    fn a_trigger_band_keeps_its_width_inside_the_travel() {
        let near = |(a, b): (f32, f32), (c, d): (f32, f32)| (a - c).abs() < 1e-6 && (b - d).abs() < 1e-6;
        assert!(near(trigger_band(0.5, 0.1), (0.45, 0.55)));
        assert!(near(trigger_band(0.98, 0.1), (0.9, 1.0)), "shifted down at full pull");
        assert!(near(trigger_band(1.0, 0.1), (0.9, 1.0)));
        assert!(near(trigger_band(0.02, 0.1), (0.0, 0.1)), "shifted up near rest");
        assert!(near(trigger_band(0.0, 0.1), (0.0, 0.1)), "at rest");
        assert!(near(trigger_band(0.5, 0.0), (0.5, 0.5)), "none");
        // Raw edges: past the point and the top, at full pull, under the bottom;
        // the band is 10% of the full travel, not of the 0.1..0.9 range.
        let e = trigger_edges(0.5, (0.1, 0.9), 0.1);
        assert!((e.press - 0.5).abs() < 1e-4 && e.press > 0.5);
        assert!((e.again - 0.55).abs() < 1e-4 && e.again > 0.55);
        assert!((e.release - 0.45).abs() < 1e-6);
        assert!((trigger_band_width(0.1, (0.1, 0.9)) - 0.125).abs() < 1e-6);
        assert_eq!(trigger_band_width(0.1, (0.5, 0.5)), 0.1, "uncalibrated");
        assert_eq!(trigger_edges(0.98, (0.1, 0.9), 0.1).again, 0.9, "full pull");
        assert_eq!(trigger_edges(0.02, (0.1, 0.9), 0.1).release, e.rest, "rest");
    }

    #[test]
    fn a_pads_clicks_go_to_the_best_actuator_it_has() {
        assert_eq!(click_actuator(["rumble_strong", "hd_l_amp", "grip_l_click"]), Some(ClickActuator::Own));
        assert_eq!(click_actuator(["hd_l_amp", "hd_rumble_l"]), Some(ClickActuator::Switch));
        assert_eq!(click_actuator(["rumble_strong", "hd_l_amp"]), Some(ClickActuator::Hd));
        assert_eq!(click_actuator(["rumble_weak"]), Some(ClickActuator::Rumble));
        assert_eq!(click_actuator(["lightbar_r"]), None);
    }

    // The table's ends, and an octave every 32 of its 126 steps.
    #[test]
    fn a_switch_pitch_lands_on_its_table() {
        assert_eq!(switch_hd_freq(81.75), 0.0);
        assert!((switch_hd_freq(163.5) - 32.0 / 126.0).abs() < 1e-5);
        assert!((switch_hd_freq(1252.6) - 1.0).abs() < 1e-3);
        assert_eq!(switch_hd_freq(5000.0), 1.0);
        // The default pitch stays on the step it has played on: 51.
        let step = 32.0 + switch_hd_freq(SWITCH_RUMBLE_HZ) * 126.0;
        assert_eq!(step.round(), 51.0, "{step}");
    }

    /// The step the encoder lands on for a pin value (`gyro.rs`
    /// `switch_carrier_fields`, after the pin's trip through a byte).
    fn switch_step(pin: f32) -> u32 {
        let pin = ((pin * 255.0) as u8) as f32 / 255.0;
        if pin == 0.0 { 0 } else { 1 + (pin.powf(1.8) * 99.0).round() as u32 }
    }

    // A level lands on the table's step for it, so a click's dB is the pad's: a
    // quarter of full strength near step 37, half near 68, full at 100, and the
    // quietest still above silence.
    #[test]
    fn a_switch_strength_lands_on_its_table() {
        assert_eq!(switch_hd_amp(0.0), 0.0);
        assert_eq!(switch_step(switch_hd_amp(1.0)), 100);
        assert!((66..=69).contains(&switch_step(switch_hd_amp(0.5))), "{}", switch_step(switch_hd_amp(0.5)));
        assert!((35..=38).contains(&switch_step(switch_hd_amp(0.25))), "{}", switch_step(switch_hd_amp(0.25)));
        assert_eq!(switch_step(switch_hd_amp(0.001)), 1);
        // −24 and −18 dB no longer land on the same step.
        let db = |d: f32| switch_step(switch_hd_amp(10f32.powf(d / 20.0)));
        assert!(db(-24.0) + 5 < db(-18.0), "{} {}", db(-24.0), db(-18.0));
    }

    #[test]
    fn a_click_envelope_fades_only_as_soft_as_asked() {
        assert_eq!(click_envelope(0.0, 0.0), 1.0);
        assert_eq!(click_envelope(0.5, 1.0), 1.0);
        assert!(click_envelope(0.05, 1.0) < 0.1);
        assert!(click_envelope(0.95, 0.5) < 0.5);
    }

    /// Slots per SDL 3.4's mappings: Triton sticks+grips, Deck / HORI sticks,
    /// and nothing for pads whose Misc slots are real buttons.
    #[test]
    fn capacitive_mask_names_only_stick_caps_and_grips() {
        let pins = |vid, pid| capacitive_pins(capacitive_misc_mask(vid, pid)).collect::<Vec<_>>();
        let sticks_grips = ["btn_misc3", "btn_misc4", "btn_misc5", "btn_misc6"];
        for pid in 0x1302..=0x1305 {
            assert_eq!(pins(0x28DE, pid), sticks_grips, "Steam Controller {pid:04X}, read through SDL");
        }
        assert_eq!(pins(0x28DE, 0x1205), ["btn_misc3", "btn_misc4"], "Steam Deck");
        assert_eq!(pins(0x0F0D, 0x01AB), ["btn_misc3", "btn_misc4"], "HORI (USB)");
        assert_eq!(pins(0x0F0D, 0x0196), ["btn_misc3", "btn_misc4"], "HORI (BT)");
        assert!(pins(0x2DC8, 0x6012).is_empty(), "other pads mute nothing");
        // Read off its own reports the same pad names those sensors itself, and
        // marks them under those names: a mute that skipped `btn_misc5` would
        // leave its grip hijacking Learn.
        assert_eq!(
            capacitive_pins(STEAM_RAW_CAPACITIVE_MASK).collect::<Vec<_>>(),
            ["btn_cap_l", "btn_cap_r", "btn_grip_l", "btn_grip_r"],
        );
    }

    /// A pad with two trackpads, passed through onto one with a single touchpad
    /// button: the second pad's click joins the first's, and its touch point is
    /// the sink's second finger.
    #[test]
    fn a_second_pad_click_joins_the_first_on_a_one_touchpad_sink() {
        let src = ["btn_touchpad", "btn_touchpad2", "touch1_x", "touch2_x"];
        let dst = ["btn_touchpad", "touch1_x", "touch1_active", "touch2_x", "touch2_active"];
        let m = resolve_mapping(&src, &dst);
        assert!(m.contains(&("btn_touchpad", "btn_touchpad")), "the left pad keeps its own name");
        assert!(m.contains(&("btn_touchpad2", "btn_touchpad")), "and the right's joins it");
        assert!(m.contains(&("touch2_x", "touch2_x")), "the second pad is the second touch point");
        // A sink that HAS two click pins — a pad passed to a pad — gets no bridge.
        let dst2 = ["btn_touchpad", "btn_touchpad2"];
        assert_eq!(
            resolve_mapping(&["btn_touchpad", "btn_touchpad2"], &dst2),
            vec![("btn_touchpad", "btn_touchpad"), ("btn_touchpad2", "btn_touchpad2")],
        );
    }

    // An override can only take over a kind of feedback it can name: every
    // feedback pin a device exposes must belong to a group.
    #[test]
    fn every_feedback_pin_has_a_group() {
        for pin in FEEDBACK_INLET_PINS {
            assert!(feedback_group(pin.id).is_some(), "feedback pin `{}` has no group", pin.id);
        }
        assert_eq!(feedback_group("rumble_strong"), feedback_group("hd2_r_freq"));
        assert_ne!(feedback_group("trigger_l_mode"), feedback_group("trigger_r_mode"));
        assert_eq!(feedback_group("left_stick"), None);
    }

    /// The four effects the trigger mode pin carried before it had seven keep
    /// their values — 0, 1/3, 2/3, 1, and the 0.33 / 0.66 a producer might
    /// have written for them — so an existing patch means the same thing.
    #[test]
    fn the_original_trigger_modes_keep_their_pin_values() {
        use TriggerMode::*;
        for (v, m) in [(0.0, Off), (1.0 / 3.0, Feedback), (0.33, Feedback), (2.0 / 3.0, Weapon),
            (0.66, Weapon), (1.0, Vibration)]
        {
            assert_eq!(TriggerMode::from_pin(v), m, "{v}");
        }
        for m in TriggerMode::ALL {
            assert_eq!(TriggerMode::from_pin(m.pin()), m, "{m:?} round-trips");
        }
    }

    // Every gamepad source node exposes both the analog trigger pin and the digital
    // trigger button pin (see gamepad::standard_outputs). A virtual gamepad sink exposes
    // both too. The analog destination must be reachable from BOTH sources so the engine
    // can pick whichever carries a live signal — critical for Switch Pro / DualSense, whose
    // ZL/ZR (or whose HID-parsed L2/R2) only populate the digital pin in some paths.
    #[test]
    fn digital_trigger_reaches_analog_destination() {
        // Order deliberately puts analog before digital, matching standard_outputs().
        let src = ["left_trigger", "right_trigger", "btn_lt_dig", "btn_rt_dig"];
        let dst = ["left_trigger", "right_trigger", "btn_lt_dig", "btn_rt_dig"];
        let m = resolve_mapping(&src, &dst);

        assert!(m.contains(&("btn_lt_dig", "left_trigger")), "digital LT must reach analog LT: {m:?}");
        assert!(m.contains(&("btn_rt_dig", "right_trigger")), "digital RT must reach analog RT: {m:?}");
        // Direct matches still present.
        assert!(m.contains(&("left_trigger", "left_trigger")));
        assert!(m.contains(&("btn_lt_dig", "btn_lt_dig")));
    }

    // When the source has no analog trigger pin at all (a hypothetical digital-only node),
    // the digital source must still drive the analog destination.
    #[test]
    fn digital_only_source_drives_analog() {
        let src = ["btn_lt_dig", "btn_rt_dig"];
        let dst = ["left_trigger", "right_trigger", "btn_lt_dig", "btn_rt_dig"];
        let m = resolve_mapping(&src, &dst);
        assert!(m.contains(&("btn_lt_dig", "left_trigger")), "{m:?}");
        assert!(m.contains(&("btn_rt_dig", "right_trigger")), "{m:?}");
    }

    #[test]
    fn capture_mute_bridge_still_works() {
        let m = resolve_mapping(&["btn_capture"], &["btn_mute"]);
        assert_eq!(m, vec![("btn_capture", "btn_mute")]);
    }

    // No duplicate (src, dst) pairs — the engine would double-process them.
    #[test]
    fn no_duplicate_pairs() {
        let src = ["left_trigger", "btn_lt_dig"];
        let dst = ["left_trigger", "btn_lt_dig"];
        let mut m = resolve_mapping(&src, &dst);
        let n = m.len();
        m.sort();
        m.dedup();
        assert_eq!(m.len(), n, "duplicate pairs in resolve_mapping output");
    }
}
