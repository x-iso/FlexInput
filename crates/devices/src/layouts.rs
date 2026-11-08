use flexinput_core::SignalType;

use crate::{identification::ControllerKind, DevicePin};

pub fn outputs_for(kind: ControllerKind) -> Vec<DevicePin> {
    let mut pins = match kind {
        ControllerKind::XInput     => xinput_outputs(),
        ControllerKind::DualShock4 => ds4_outputs(),
        ControllerKind::DualSense  => dualsense_outputs(),
        ControllerKind::SwitchPro  => switch_pro_outputs(),
        ControllerKind::JoyCon2L   => joycon2_left_outputs(),
        ControllerKind::JoyCon2R   => joycon2_right_outputs(),
        ControllerKind::Generic    => generic_outputs(),
        // MIDI devices build their own pin lists; layouts not used.
        ControllerKind::MidiIn | ControllerKind::MidiOut => return vec![],
    };
    // Auto-map bus port — always last so it never shifts existing pin indices.
    pins.push(am("automap_out", "Auto-Map"));
    pins
}

/// [`outputs_for`] for one pad, whose VID/PID can add pins its kind doesn't
/// carry: a DualSense Edge's back and Fn buttons.
pub fn outputs_for_pad(kind: ControllerKind, vid: Option<u16>, pid: Option<u16>) -> Vec<DevicePin> {
    // The default naming is SDL's: whatever pad reaches this through gilrs or
    // SDL gets its Misc slots, because that is what those paths report.
    outputs_for_pad_named(kind, vid, pid, SteamNaming::Sdl)
}

/// Who is reading a Steam Controller's spare controls, and so what they are
/// called on the bus: the raw-HID reader names each after what it is, while SDL
/// hands the same out as its Misc slots. A pad's pins have to be named by
/// whoever publishes them, so the layout follows the reader.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SteamNaming {
    /// `steam_controller`'s reader, off the pad's own reports.
    Raw,
    /// SDL's gamepad API, which keeps the Misc slots.
    Sdl,
}

/// [`outputs_for_pad`] with the Steam Controller's naming chosen.
pub fn outputs_for_pad_named(
    kind: ControllerKind,
    vid: Option<u16>,
    pid: Option<u16>,
    naming: SteamNaming,
) -> Vec<DevicePin> {
    let steam = crate::steam_haptics::is_steam_controller(vid, pid);
    let mut pins = outputs_for(kind);
    if kind == ControllerKind::DualSense && crate::identification::is_dualsense_edge(vid, pid) {
        // Ahead of the Auto-Map port, which stays last.
        let at = pins.len().saturating_sub(1);
        pins.splice(at..at, dualsense_edge_extra_outputs());
    }
    if steam && naming == SteamNaming::Raw {
        // Read off its own reports the pad has no Misc slots: it names the
        // three-dot button, the right pad's click, the stick caps and the grips
        // itself, and the SDL-era names are dropped rather than left to route
        // to nothing.
        pins.retain(|p| !flexinput_core::automap::MISC_PINS.contains(&p.id.as_str()));
    }
    if steam {
        // Its trackpads' press, ahead of the Auto-Map port.
        let at = pins.len().saturating_sub(1);
        let extra: Vec<DevicePin> = match naming {
            SteamNaming::Raw => flexinput_core::automap::TOUCH_PRESSURE_PINS
                .iter().map(|id| fl(id, id))
                .chain(STEAM_RAW_EXTRA_OUTPUTS.iter().map(|(id, name)| bo(id, name)))
                .collect(),
            SteamNaming::Sdl => flexinput_core::automap::TOUCH_PRESSURE_PINS
                .iter().map(|id| fl(id, id))
                .collect(),
        };
        pins.splice(at..at, extra);
        for pin in &mut pins {
            if let Some((name, _)) = flexinput_core::automap::steam_controller_pin_names(&pin.id) {
                pin.display_name = name.to_string();
            }
        }
    }
    pins
}

/// The controls a Steam Controller read off its own reports names itself, in the
/// order the pad's node lists them: the three-dot button (the Capture of a Switch
/// Pro), the right trackpad's click, the two stick caps and the two grips.
const STEAM_RAW_EXTRA_OUTPUTS: [(&str, &str); 6] = [
    ("btn_capture",   "Capture"),
    ("btn_touchpad2", "Pad click R"),
    ("btn_cap_l",     "Cap sens L"),
    ("btn_cap_r",     "Cap sens R"),
    ("btn_grip_l",    "Grip sens L"),
    ("btn_grip_r",    "Grip sens R"),
];

/// The DualSense Edge's back buttons on the first paddle pair and its Fn
/// buttons on the second, the order SDL reports them in.
fn dualsense_edge_extra_outputs() -> Vec<DevicePin> {
    vec![
        bo("btn_paddle_l1", "Left Back Button"),
        bo("btn_paddle_r1", "Right Back Button"),
        bo("btn_paddle_l2", "Left Fn"),
        bo("btn_paddle_r2", "Right Fn"),
    ]
}

/// `inputs_for`, plus what a pad has beyond its kind: a Steam Controller is a
/// Generic pad to everything else.
pub fn inputs_for_pad(kind: ControllerKind, vid: Option<u16>, pid: Option<u16>) -> Vec<DevicePin> {
    let mut pins = inputs_for(kind);
    if crate::steam_haptics::is_steam_controller(vid, pid) {
        pins.extend(steam_controller_extra_inputs());
    }
    pins
}

/// The grip motors under the same pins as a DualSense's actuators, so an Audio
/// Stream Haptics patch drives either; then the trackpads' own. Rumble stays
/// where it was, on the standard pins.
fn steam_controller_extra_inputs() -> Vec<DevicePin> {
    vec![
        fl("hd_l_amp",   "Left Grip: Amplitude (0–1)"),
        fl("hd_l_freq",  "Left Grip: Frequency (0=80Hz 1=500Hz)"),
        fl("hd_r_amp",   "Right Grip: Amplitude (0–1)"),
        fl("hd_r_freq",  "Right Grip: Frequency (0=80Hz 1=500Hz)"),
        fl("hd2_l_amp",  "Left Grip: Amp (HF carrier)"),
        fl("hd2_l_freq", "Left Grip: Freq (HF carrier)"),
        fl("hd2_r_amp",  "Right Grip: Amp (HF carrier)"),
        fl("hd2_r_freq", "Right Grip: Freq (HF carrier)"),
        fl("tp_l_amp",   "Left Trackpad: Amplitude (0–1)"),
        fl("tp_l_freq",  "Left Trackpad: Frequency (0=80Hz 1=500Hz)"),
        fl("tp_r_amp",   "Right Trackpad: Amplitude (0–1)"),
        fl("tp_r_freq",  "Right Trackpad: Frequency (0=80Hz 1=500Hz)"),
        bo("tp_l_click", "Left Trackpad: Click"),
        bo("tp_r_click", "Right Trackpad: Click"),
        bo("grip_l_click", "Left Grip: Click"),
        bo("grip_r_click", "Right Grip: Click"),
    ]
}

pub fn inputs_for(kind: ControllerKind) -> Vec<DevicePin> {
    match kind {
        ControllerKind::DualShock4 => ds4_inputs(),
        ControllerKind::DualSense  => dualsense_inputs(),
        ControllerKind::SwitchPro  => switch_pro_inputs(),
        ControllerKind::JoyCon2L
        | ControllerKind::JoyCon2R => joycon2_inputs(),
        _                          => standard_rumble_inputs(),
    }
}

// ── XInput / Xbox ─────────────────────────────────────────────────────────────

fn xinput_outputs() -> Vec<DevicePin> {
    vec![
        // Bundled sticks
        f2("left_stick",   "Left Stick",   SignalType::Vec2),
        f2("right_stick",  "Right Stick",  SignalType::Vec2),
        f2("dpad",         "D-Pad",        SignalType::Vec2),
        // Individual axes
        fl("left_stick_x", "L.Stick X"),
        fl("left_stick_y", "L.Stick Y"),
        fl("right_stick_x","R.Stick X"),
        fl("right_stick_y","R.Stick Y"),
        fl("left_trigger", "L.Trigger (LT)"),
        fl("right_trigger","R.Trigger (RT)"),
        fl("dpad_x",       "D-Pad X"),
        fl("dpad_y",       "D-Pad Y"),
        // Face
        bo("btn_south",    "A"),
        bo("btn_east",     "B"),
        bo("btn_west",     "X"),
        bo("btn_north",    "Y"),
        // Shoulder / trigger digital
        bo("btn_lb",       "LB"),
        bo("btn_rb",       "RB"),
        // Digital trigger pins — poll() emits these for XInput (thresholded
        // LT/RT) just like the PlayStation layouts; without them declared here a
        // sink could never receive the Xbox's digital-trigger mapping.
        bo("btn_lt_dig",   "LT (digital)"),
        bo("btn_rt_dig",   "RT (digital)"),
        // Stick clicks
        bo("btn_ls",       "LS (L.Stick Click)"),
        bo("btn_rs",       "RS (R.Stick Click)"),
        // Menu
        bo("btn_start",    "Start / Menu"),
        bo("btn_back",     "Back / View"),
        bo("btn_guide",    "Guide / Xbox"),
        // D-Pad discrete
        bo("dpad_up",      "D-Pad Up"),
        bo("dpad_down",    "D-Pad Down"),
        bo("dpad_left",    "D-Pad Left"),
        bo("dpad_right",   "D-Pad Right"),
    ]
}

// ── DualShock 4 ───────────────────────────────────────────────────────────────

fn ds4_outputs() -> Vec<DevicePin> {
    let mut pins = vec![
        f2("left_stick",    "Left Stick",        SignalType::Vec2),
        f2("right_stick",   "Right Stick",       SignalType::Vec2),
        f2("dpad",          "D-Pad",             SignalType::Vec2),
        fl("left_stick_x",  "L.Stick X"),
        fl("left_stick_y",  "L.Stick Y"),
        fl("right_stick_x", "R.Stick X"),
        fl("right_stick_y", "R.Stick Y"),
        fl("left_trigger",  "L2 / LT (analog)"),
        fl("right_trigger", "R2 / RT (analog)"),
        fl("dpad_x",        "D-Pad X"),
        fl("dpad_y",        "D-Pad Y"),
        bo("btn_south",     "South (Cross/X)"),
        bo("btn_east",      "East (Circle/O)"),
        bo("btn_west",      "West (Square)"),
        bo("btn_north",     "North (Triangle)"),
        bo("btn_lb",        "LB / L1"),
        bo("btn_rb",        "RB / R1"),
        bo("btn_lt_dig",    "LT digital / L2"),
        bo("btn_rt_dig",    "RT digital / R2"),
        bo("btn_ls",        "LS (L.Stick Click)"),
        bo("btn_rs",        "RS (R.Stick Click)"),
        bo("btn_start",     "Start / Options"),
        bo("btn_back",      "Back / Share"),
        bo("btn_guide",     "Guide / PS"),
        bo("btn_touchpad",  "Touchpad Click"),
        bo("dpad_up",       "D-Pad Up"),
        bo("dpad_down",     "D-Pad Down"),
        bo("dpad_left",     "D-Pad Left"),
        bo("dpad_right",    "D-Pad Right"),
    ];
    pins.extend(imu_pins());
    pins.extend(vec![
        fl("touch1_x",     "Touch 1 X"),
        fl("touch1_y",     "Touch 1 Y"),
        bo("touch1_active","Touch 1 Active"),
        fl("touch2_x",     "Touch 2 X"),
        fl("touch2_y",     "Touch 2 Y"),
        bo("touch2_active","Touch 2 Active"),
        fl("battery",      "Battery (0–1)"),
    ]);
    pins
}

// ── DualSense ─────────────────────────────────────────────────────────────────

fn dualsense_outputs() -> Vec<DevicePin> {
    // Same as DS4 — hardware layout is identical at the signal level.
    // Adaptive triggers and haptics are outputs (inputs to device), listed below.
    let mut pins = ds4_outputs();
    // DualSense adds microphone button
    pins.push(bo("btn_mute", "Mute Button"));
    pins
}

// ── Switch Pro ────────────────────────────────────────────────────────────────

fn switch_pro_outputs() -> Vec<DevicePin> {
    // Positional names used throughout — Nintendo physical South = B label, but gilrs
    // fires Button::South for it, so btn_south is the correct positional ID.
    let mut pins = vec![
        f2("left_stick",    "Left Stick",    SignalType::Vec2),
        f2("right_stick",   "Right Stick",   SignalType::Vec2),
        f2("dpad",          "D-Pad",         SignalType::Vec2),
        fl("left_stick_x",  "L.Stick X"),
        fl("left_stick_y",  "L.Stick Y"),
        fl("right_stick_x", "R.Stick X"),
        fl("right_stick_y", "R.Stick Y"),
        fl("dpad_x",        "D-Pad X"),
        fl("dpad_y",        "D-Pad Y"),
        bo("btn_south",     "South (B)"),
        bo("btn_east",      "East (A)"),
        bo("btn_west",      "West (Y)"),
        bo("btn_north",     "North (X)"),
        bo("btn_lb",        "LB / L"),
        bo("btn_rb",        "RB / R"),
        bo("btn_lt_dig",    "LT / ZL (digital)"),
        bo("btn_rt_dig",    "RT / ZR (digital)"),
        bo("btn_ls",        "LS (L.Stick Click)"),
        bo("btn_rs",        "RS (R.Stick Click)"),
        bo("btn_start",     "Start / +"),
        bo("btn_back",      "Back / −"),
        bo("btn_guide",     "Guide / Home"),
        bo("btn_capture",   "Capture"),
        bo("dpad_up",       "D-Pad Up"),
        bo("dpad_down",     "D-Pad Down"),
        bo("dpad_left",     "D-Pad Left"),
        bo("dpad_right",    "D-Pad Right"),
    ];
    pins.extend(imu_pins());
    pins
}

// ── Joy-Con 2 ─────────────────────────────────────────────────────────────────
//
// Each half is an independent BLE peripheral and gets its own device entry, so
// these layouts describe ONE Joy-Con rather than a pair.
//
// The single stick is named for the half it lives on (`left_stick` on the L,
// `right_stick` on the R) rather than always `left_stick`. That way merging the
// two halves in the graph produces exactly the pin set of a normal two-stick
// pad, with no renaming step.
//
// Face-button naming follows the Switch Pro layout above: positional ids, with
// the Nintendo label in the display name. Nintendo's A is positional East, B is
// South, X is North, Y is West.

/// Pins common to both halves: IMU, the optical mouse sensor, and battery.
fn joycon2_shared_outputs() -> Vec<DevicePin> {
    let mut pins = imu_pins();
    pins.extend(vec![
        // Absolute orientation, which this controller can supply and a
        // rate-only pad cannot: the firmware fuses gyro, accel and
        // magnetometer, so yaw is drift-free rather than integrated.
        f2("orientation", "Orientation (absolute)", SignalType::Vec4),
    ]);
    if JC2_MOUSE_STREAMED {
        pins.extend(joycon2_optical_outputs());
    }
    pins.extend(vec![
        fl("battery", "Battery (0–1)"),
        bo("charging", "Charging"),
    ]);
    pins
}

/// Whether Joy-Con 2s stream their optical sensor — see
/// `flexinput_joycon2::protocol::feature::MOUSE_STREAMED`. While they don't,
/// the sensor's pins aren't offered at all, rather than offered and dead.
#[cfg(feature = "joycon2")]
pub(crate) const JC2_MOUSE_STREAMED: bool = flexinput_joycon2::protocol::feature::MOUSE_STREAMED;
#[cfg(not(feature = "joycon2"))]
pub(crate) const JC2_MOUSE_STREAMED: bool = false;

/// The Joy-Con 2 optical (mouse) sensor's pins.
fn joycon2_optical_outputs() -> Vec<DevicePin> {
    vec![
        // Optical sensor motion in the Virtual KB/M's own mouse units (+Y up,
        // the default Mouse speed of 100 = one pixel per sensor count), so a
        // plain wire onto its `mouse` / `mouse_x` / `mouse_y` works. Unlike
        // every other Float pin here these are NOT clamped to −1..1: mouse
        // travel has no natural full-scale, and clamping would cap pointer
        // speed.
        f2("optical_motion", "Mouse Sensor XY", SignalType::Vec2),
        fl("optical_x", "Mouse Sensor X"),
        fl("optical_y", "Mouse Sensor Y"),
        // On while the sensor sits on a surface or a finger covers it, so it
        // doubles as a touch button — e.g. a gyro on/off gate.
        bo("optical_contact", "Mouse Sensor Contact"),
        // Raw distance byte: 0 on a surface, ≈12 lifted. Diagnostic.
        fl("optical_distance", "Mouse Sensor Distance (raw)"),
    ]
}

fn joycon2_left_outputs() -> Vec<DevicePin> {
    let mut pins = vec![
        f2("left_stick", "Stick", SignalType::Vec2),
        f2("dpad", "D-Pad", SignalType::Vec2),
        fl("left_stick_x", "Stick X"),
        fl("left_stick_y", "Stick Y"),
        fl("dpad_x", "D-Pad X"),
        fl("dpad_y", "D-Pad Y"),
        bo("btn_lb", "L"),
        bo("btn_lt_dig", "ZL (digital)"),
        bo("btn_ls", "Stick Click"),
        bo("btn_back", "Minus"),
        bo("btn_capture", "Capture"),
        // ⭐ The rail buttons, published under the STANDARD paddle ids.
        //
        // They kept device-native ids (`btn_sl`/`btn_sr`) on the reasoning that
        // they have no positional equivalent on a normal pad. True, but the
        // consequence was that they reached nothing: AutoMap carries a fixed
        // vocabulary, and a pin outside it is dropped between the backend and
        // the bus. A button that exists, is decoded, is displayed, and can
        // never be mapped is worse than one that is simply absent.
        //
        // Rear paddles are the closest standard analogue — extra buttons under
        // the fingers, no fixed function — and this half's two become L1/L2.
        // The DISPLAY names stay SL and SR, because that is what is printed on
        // the hardware.
        //
        // ❗ ONE id per switch, not two. Keeping `btn_sl`/`btn_sr` alongside as
        // aliases was tried and is worse: two bindable pins for one physical
        // button means a preset can drive an action twice, and the second name
        // is not in the vocabulary either, so it would still reach nothing.
        // A test asserts no declared button is left outside AutoMap.
        bo("btn_paddle_l1", "SL"),
        bo("btn_paddle_l2", "SR"),
        bo("dpad_up", "D-Pad Up"),
        bo("dpad_down", "D-Pad Down"),
        bo("dpad_left", "D-Pad Left"),
        bo("dpad_right", "D-Pad Right"),
    ];
    pins.extend(joycon2_shared_outputs());
    pins
}

fn joycon2_right_outputs() -> Vec<DevicePin> {
    let mut pins = vec![
        f2("right_stick", "Stick", SignalType::Vec2),
        fl("right_stick_x", "Stick X"),
        fl("right_stick_y", "Stick Y"),
        bo("btn_south", "South (B)"),
        bo("btn_east", "East (A)"),
        bo("btn_west", "West (Y)"),
        bo("btn_north", "North (X)"),
        bo("btn_rb", "R"),
        bo("btn_rt_dig", "ZR (digital)"),
        bo("btn_rs", "Stick Click"),
        bo("btn_start", "Plus"),
        bo("btn_guide", "Home"),
        // New on Switch 2: the GameChat button.
        bo("btn_c", "C (GameChat)"),
        // The rail buttons — see the left half for why these carry paddle ids.
        // This half's two become R1/R2.
        bo("btn_paddle_r1", "SL"),
        bo("btn_paddle_r2", "SR"),
    ];
    pins.extend(joycon2_shared_outputs());
    pins
}

/// Joy-Con 2 has a single LRA per half, driven by the same dual-carrier HD
/// rumble encoding as the Switch Pro. The pin names match `switch_pro_inputs`
/// so an Audio Stream Haptics patch built for a Pro Controller drives a Joy-Con
/// unchanged — there is just one actuator instead of two, so only the `_l` pins
/// exist regardless of which half this is.
fn joycon2_inputs() -> Vec<DevicePin> {
    vec![
        fl("hd_l_amp", "HD Rumble: Amplitude (perceptual 0–1)"),
        fl("hd_l_freq", "HD Rumble: Frequency (0=82Hz 0.5=320Hz 1=1253Hz)"),
        fl("hd2_l_amp", "HD Rumble: Amp (HF carrier)"),
        fl("hd2_l_freq", "HD Rumble: Freq (HF carrier)"),
        // player_led: bits 0–3 select which of the 4 LEDs light.
        // 0=off, 0.25=P1, 0.5=P2, 0.75=P3, 1.0=P4 — same encoding as DualSense.
        fl("player_led", "Player LED (0=off 0.25=P1 0.5=P2 0.75=P3 1=P4)"),
    ]
}

// ── Generic fallback ──────────────────────────────────────────────────────────

fn generic_outputs() -> Vec<DevicePin> {
    // Base generic list (sticks/buttons/dpad) shared with the gilrs path.
    let mut pins = crate::gamepad::standard_outputs();
    // Extended capabilities the SDL backend relays for pads FlexInput doesn't
    // parse natively (Steam Controller, third-party): gyro/accel (SDL sensor
    // API), touchpad fingers (raw SDL_GetGamepadTouchpadFinger), and the extra
    // paddles/misc buttons SDL exposes. These pins are declared here so a sink
    // can map them; the SDL backend only emits the ones a given pad actually
    // reports (gilrs `Generic` pads simply never drive them). Names/units match
    // the raw-HID path (imu_pins + DS4 touch pins) so gyro→aim mappings and
    // touch routing behave identically regardless of source.
    pins.extend(imu_pins());
    pins.extend(vec![
        fl("touch1_x",      "Touch 1 X"),
        fl("touch1_y",      "Touch 1 Y"),
        bo("touch1_active", "Touch 1 Active"),
        fl("touch2_x",      "Touch 2 X"),
        fl("touch2_y",      "Touch 2 Y"),
        bo("touch2_active", "Touch 2 Active"),
        bo("btn_touchpad",  "Touchpad Click"),
        // Extra buttons (rear paddles / misc) reported by SDL. Icons/labels can
        // be refined later; the signals are live and mappable now.
        bo("btn_paddle_l1", "Left Paddle 1 (P3)"),
        bo("btn_paddle_r1", "Right Paddle 1 (P1)"),
        bo("btn_paddle_l2", "Left Paddle 2 (P4)"),
        bo("btn_paddle_r2", "Right Paddle 2 (P2)"),
        bo("btn_misc1",     "Misc 1 (Share/Capture)"),
        bo("btn_misc2",     "Misc 2"),
        bo("btn_misc3",     "Misc 3"),
        bo("btn_misc4",     "Misc 4"),
        bo("btn_misc5",     "Misc 5"),
        bo("btn_misc6",     "Misc 6"),
    ]);
    pins
}

// ── Haptic inputs ─────────────────────────────────────────────────────────────

fn standard_rumble_inputs() -> Vec<DevicePin> {
    vec![
        fl("rumble_strong", "Rumble (strong)"),
        fl("rumble_weak",   "Rumble (weak)"),
    ]
}

fn ds4_inputs() -> Vec<DevicePin> {
    let mut pins = standard_rumble_inputs();
    pins.extend(vec![
        fl("lightbar_r", "Light Bar R"),
        fl("lightbar_g", "Light Bar G"),
        fl("lightbar_b", "Light Bar B"),
    ]);
    pins
}

fn dualsense_inputs() -> Vec<DevicePin> {
    let mut pins = ds4_inputs();
    pins.extend(vec![
        // ── HD haptics (USB only — driven through the controller's 4-ch audio
        // endpoint, channels 3/4 = left/right LRA). Bluetooth has no audio
        // endpoint so these pins are silently no-op on BT; rumble_strong/weak
        // remain as the XInput-compatible fallback the user wires explicitly.
        // ds_l/r_amp:  0=silent → 1=max
        // ds_l/r_freq: 0=80Hz  → 1=500Hz (covers LRA usable band; ~160Hz resonance)
        fl("ds_l_amp",  "DS Haptic L: Amplitude (USB only, 0–1)"),
        fl("ds_l_freq", "DS Haptic L: Frequency (USB only, 0=80Hz 1=500Hz)"),
        fl("ds_r_amp",  "DS Haptic R: Amplitude (USB only, 0–1)"),
        fl("ds_r_freq", "DS Haptic R: Frequency (USB only, 0=80Hz 1=500Hz)"),
        // Device-agnostic HD-rumble carriers (same pins as Switch Pro). A USB
        // DualSense synthesizes these as PCM sines on its LRAs — carrier 1 (hd_*)
        // = low band, carrier 2 (hd2_*) = high band, summed per actuator. This is
        // what the Audio Stream Haptics module drives, so one patch works on both
        // Switch Pro and DualSense. (ds_* above are kept as carrier-1 aliases.)
        fl("hd_l_amp",   "HD Haptic L: Amplitude (USB only, 0–1)"),
        fl("hd_l_freq",  "HD Haptic L: Frequency (USB only)"),
        fl("hd_r_amp",   "HD Haptic R: Amplitude (USB only, 0–1)"),
        fl("hd_r_freq",  "HD Haptic R: Frequency (USB only)"),
        fl("hd2_l_amp",  "HD Haptic L: Amp (HF carrier, USB only)"),
        fl("hd2_l_freq", "HD Haptic L: Freq (HF carrier, USB only)"),
        fl("hd2_r_amp",  "HD Haptic R: Amp (HF carrier, USB only)"),
        fl("hd2_r_freq", "HD Haptic R: Freq (HF carrier, USB only)"),
        // LEDs. All accept Float 0–1.
        // player_led: 0=off, 0.25=P1, 0.5=P2, 0.75=P3, 1.0=P4
        fl("player_led", "Player LED (0=off 0.25=P1 0.5=P2 0.75=P3 1=P4)"),
        // mic_led: 0=off, 0.5=on(orange), 1.0=pulsing
        fl("mic_led",    "Mic LED (0=off 0.5=on 1=pulse)"),
        // Adaptive triggers. All accept Float 0–1, scaled per pin.
        // Mode, in sixths (`TriggerMode`): 0=off, 1/6=bow, 2/6=resist,
        // 3/6=galloping, 4/6=click, 5/6=machine, 1=vibration. The original four
        // (0, 0.33, 0.66, 1) are unchanged.
        fl("trigger_r_mode",      "R.Trigger Mode (0 off · ⅙ bow · ⅓ resist · ½ gallop · ⅔ click · ⅚ machine · 1 vib)"),
        // Start/End: trigger travel position, 0=rest 1=fully pressed
        fl("trigger_r_start",     "R.Trigger Start (0=rest 1=full)"),
        // End: bow, galloping, click and machine
        fl("trigger_r_end",       "R.Trigger End (bow/gallop/click/machine)"),
        // Strength: 0=none 1=max
        fl("trigger_r_strength",  "R.Trigger Strength (0–1)"),
        // Freq: galloping, machine and vibration
        fl("trigger_r_freq",      "R.Trigger Freq (gallop/machine/vib)"),
        // The second force: bow's snap, galloping's second foot, machine's B
        fl("trigger_r_strength2", "R.Trigger Strength 2 (bow/gallop/machine)"),
        // Machine's period
        fl("trigger_r_period",    "R.Trigger Period (machine)"),
        fl("trigger_l_mode",      "L.Trigger Mode (0 off · ⅙ bow · ⅓ resist · ½ gallop · ⅔ click · ⅚ machine · 1 vib)"),
        fl("trigger_l_start",     "L.Trigger Start (0=rest 1=full)"),
        fl("trigger_l_end",       "L.Trigger End (bow/gallop/click/machine)"),
        fl("trigger_l_strength",  "L.Trigger Strength (0–1)"),
        fl("trigger_l_freq",      "L.Trigger Freq (gallop/machine/vib)"),
        fl("trigger_l_strength2", "L.Trigger Strength 2 (bow/gallop/machine)"),
        fl("trigger_l_period",    "L.Trigger Period (machine)"),
    ]);
    pins
}

fn switch_pro_inputs() -> Vec<DevicePin> {
    vec![
        // Per-side amplitude + carrier frequency.
        // hd_l/r_amp:  0=silent → 1=max; perceptual power-law curve (more resolution at low amp).
        // hd_l/r_freq: 0=82 Hz → 1=1253 Hz along the pad's frequency table, an
        // octave every 32 of its 126 steps (`automap::switch_hd_freq`).
        fl("hd_l_amp",  "HD Rumble L: Amplitude (perceptual 0–1)"),
        fl("hd_l_freq", "HD Rumble L: Frequency (0=82Hz 0.5=320Hz 1=1253Hz)"),
        fl("hd_r_amp",  "HD Rumble R: Amplitude (perceptual 0–1)"),
        fl("hd_r_freq", "HD Rumble R: Frequency (0=82Hz 0.5=320Hz 1=1253Hz)"),
        // Second simultaneous carrier (HF band). The Switch Pro packs carrier 1
        // (hd_*) + carrier 2 (hd2_*) into one dual-band HD-rumble packet.
        fl("hd2_l_amp",  "HD Rumble L: Amp (HF carrier)"),
        fl("hd2_l_freq", "HD Rumble L: Freq (HF carrier)"),
        fl("hd2_r_amp",  "HD Rumble R: Amp (HF carrier)"),
        fl("hd2_r_freq", "HD Rumble R: Freq (HF carrier)"),
        // Legacy single-pin aliases kept for patch backward compatibility.
        fl("hd_rumble_l", "HD Rumble L (legacy — use hd_l_amp)"),
        fl("hd_rumble_r", "HD Rumble R (legacy — use hd_r_amp)"),
    ]
}

// ── IMU pins (shared by DS4, DualSense, Switch Pro) ───────────────────────────
// Actual values require direct HID access (future work); pins are defined now
// so patches can reference them already.

fn imu_pins() -> Vec<DevicePin> {
    vec![
        fl("gyro_x",  "Gyro X (roll)"),
        fl("gyro_y",  "Gyro Y (pitch)"),
        fl("gyro_z",  "Gyro Z (yaw)"),
        fl("accel_x", "Accel X"),
        fl("accel_y", "Accel Y"),
        fl("accel_z", "Accel Z"),
    ]
}

// ── helpers ───────────────────────────────────────────────────────────────────

fn f2(id: &str, name: &str, t: SignalType) -> DevicePin {
    DevicePin { id: id.into(), display_name: name.into(), signal_type: t }
}
fn fl(id: &str, name: &str) -> DevicePin {
    f2(id, name, SignalType::Float)
}
fn bo(id: &str, name: &str) -> DevicePin {
    f2(id, name, SignalType::Bool)
}
fn am(id: &str, name: &str) -> DevicePin {
    f2(id, name, SignalType::AutoMap)
}

#[cfg(test)]
mod automap_vocabulary_tests {
    use super::*;
    use crate::ControllerKind;

    /// ⛔ Every BUTTON a layout declares must exist in the AutoMap vocabulary.
    ///
    /// ⭐ A pin outside `flexinput_core::automap::ALL_PINS` is dropped between
    /// the backend and the bus — silently, with nothing logged and nothing
    /// missing from the UI. `btn_sl`, `btn_sr` and `btn_c` were declared here,
    /// decoded correctly, published every report, shown in the device node, and
    /// reached nothing at all. The only way to notice was to press one and
    /// wonder why nothing happened.
    ///
    /// Buttons only. Analogue and diagnostic pins are deliberately outside the
    /// vocabulary — raw probe values and mouse deltas have no standard
    /// meaning to map onto — but a button is exactly the thing a person expects
    /// to be able to bind.
    #[test]
    fn every_declared_button_is_carried_by_automap() {
        let vocab: std::collections::HashSet<&str> =
            flexinput_core::automap::ALL_PINS.iter().map(|p| p.id).collect();
        let kinds = [
            ControllerKind::JoyCon2L,
            ControllerKind::JoyCon2R,
            ControllerKind::SwitchPro,
            ControllerKind::DualSense,
            ControllerKind::DualShock4,
            ControllerKind::XInput,
            ControllerKind::Generic,
        ];
        let mut orphans: Vec<String> = Vec::new();
        for kind in kinds {
            for pin in outputs_for(kind) {
                if !pin.id.starts_with("btn_") {
                    continue;
                }
                if !vocab.contains(pin.id.as_str()) {
                    orphans.push(format!("{kind:?}/{}", pin.id));
                }
            }
        }
        assert!(
            orphans.is_empty(),
            "these buttons can never be mapped — they are published but absent \
             from ALL_PINS: {orphans:?}",
        );
    }

    // Only an Edge gains the back/Fn pins, ahead of the Auto-Map port.
    #[test]
    fn dualsense_edge_adds_back_and_fn_pins() {
        let ids = |vid, pid| -> Vec<String> {
            outputs_for_pad(ControllerKind::DualSense, Some(vid), Some(pid))
                .into_iter().map(|p| p.id).collect()
        };
        let edge = ids(0x054C, 0x0DF2);
        let plain = ids(0x054C, 0x0CE6);
        let extra = ["btn_paddle_l1", "btn_paddle_r1", "btn_paddle_l2", "btn_paddle_r2"];
        for pin in extra {
            assert!(edge.iter().any(|p| p == pin), "Edge lacks {pin}");
            assert!(!plain.iter().any(|p| p == pin), "plain DualSense has {pin}");
        }
        assert_eq!(edge.len(), plain.len() + extra.len());
        assert_eq!(edge.last().map(String::as_str), Some("automap_out"));
    }

    /// Its id calls it Generic, so its pins go by its USB ids, after the rumble
    /// every pad has. No pin is claimed that the backend wouldn't take.
    #[test]
    fn a_steam_controller_declares_its_haptic_pins_and_a_generic_pad_doesnt() {
        let ids = |vid, pid| -> Vec<String> {
            inputs_for_pad(ControllerKind::Generic, Some(vid), Some(pid))
                .into_iter().map(|p| p.id).collect()
        };
        let steam = ids(0x28DE, 0x1304);
        assert_eq!(steam[..2], ["rumble_strong", "rumble_weak"]);
        for pin in ["hd_l_amp", "hd2_r_freq", "tp_l_amp", "tp_r_freq", "tp_l_click", "tp_r_click"] {
            assert!(steam.iter().any(|p| p == pin), "lacks {pin}");
        }
        // Feedback Control offers each of them.
        for pin in &steam {
            assert!(flexinput_core::automap::FEEDBACK_INLET_PINS.iter().any(|p| p.id == pin), "{pin}");
        }
        assert_eq!(ids(0x045E, 0x0B12), ["rumble_strong", "rumble_weak"]);
    }

    #[test]
    fn a_steam_controllers_sensors_are_named_for_what_they_are() {
        let name = |naming: SteamNaming, pin: &str| outputs_for_pad_named(
                ControllerKind::Generic, Some(0x28DE), Some(0x1304), naming)
            .into_iter().find(|p| p.id == pin).map(|p| p.display_name);
        for (pin, want) in [("btn_touchpad", "Pad click L"), ("btn_touchpad2", "Pad click R"),
                            ("btn_cap_l", "Cap sens L"), ("btn_cap_r", "Cap sens R"),
                            ("btn_grip_l", "Grip sens L"), ("btn_grip_r", "Grip sens R")] {
            assert_eq!(name(SteamNaming::Raw, pin).as_deref(), Some(want), "raw naming");
        }
        for (pin, want) in [("btn_touchpad", "Pad click L"), ("btn_misc2", "Pad click R"),
                            ("btn_misc3", "Cap sens L"), ("btn_misc4", "Cap sens R"),
                            ("btn_misc5", "Grip sens L"), ("btn_misc6", "Grip sens R")] {
            assert_eq!(name(SteamNaming::Sdl, pin).as_deref(), Some(want), "SDL naming");
        }
        assert_eq!(name(SteamNaming::Sdl, "btn_touchpad2"), None, "SDL's pad has no such pin");
        assert_eq!(name(SteamNaming::Raw, "btn_misc3"), None, "nor does the raw one have Misc");
        assert_eq!(outputs_for_pad(
                ControllerKind::Generic, Some(0x045E), Some(0x0B12))
            .into_iter().find(|p| p.id == "btn_misc3").map(|p| p.display_name).as_deref(),
            Some("Misc 3"), "another pad's Misc pin keeps SDL's name");
        assert_eq!(outputs_for_pad_named(
                ControllerKind::Generic, Some(0x045E), Some(0x0B12), SteamNaming::Raw)
            .into_iter().find(|p| p.id == "btn_touchpad").map(|p| p.display_name).as_deref(),
            Some("Touchpad Click"), "another pad's stays");
        assert_eq!(name(SteamNaming::Sdl, "btn_misc3").as_deref(), Some("Cap sens L"),
            "read through SDL, this pad's Misc 3 is still named for its left cap");
    }

    /// Read off its own reports, the pad carries no Misc slots at all — every
    /// one of them is a pin named for the control it is — and the three-dot
    /// button is Capture, so a mapping written for a Switch Pro's Capture runs
    /// on it. A pad read through SDL keeps the Misc pins, which is what that
    /// path reports.
    #[test]
    fn a_steam_controller_read_raw_names_its_own_controls() {
        let ids = |naming: SteamNaming| -> Vec<String> {
            outputs_for_pad_named(ControllerKind::Generic, Some(0x28DE), Some(0x1302), naming)
                .into_iter().map(|p| p.id).collect()
        };
        let raw = ids(SteamNaming::Raw);
        for pin in ["btn_capture", "btn_touchpad2", "btn_cap_l", "btn_cap_r", "btn_grip_l", "btn_grip_r"] {
            assert!(raw.iter().any(|p| p == pin), "raw layout lacks {pin}");
        }
        for pin in flexinput_core::automap::MISC_PINS {
            assert!(!raw.iter().any(|p| p == pin), "raw layout still declares {pin}");
        }
        // Its shared controls keep the positional names, so a patch wired for
        // any other pad works: View is Start, Menu is Back, the left pad's click
        // is the touchpad button.
        for pin in ["btn_start", "btn_back", "btn_guide", "btn_touchpad",
                    "btn_paddle_l1", "btn_paddle_r2", "left_trigger", "touch2_x"] {
            assert!(raw.iter().any(|p| p == pin), "raw layout lacks {pin}");
        }
        // Every one of them reaches the AutoMap bus, or it could never be mapped.
        let bus: std::collections::HashSet<&str> =
            flexinput_core::automap::ALL_PINS.iter().map(|p| p.id).collect();
        for pin in &raw {
            assert!(!pin.starts_with("btn_") || bus.contains(pin.as_str()), "{pin} is off the bus");
        }
        // The Auto-Map port stays last.
        assert_eq!(raw.last().map(String::as_str), Some("automap_out"));
        // SDL's route keeps its vocabulary.
        let sdl = ids(SteamNaming::Sdl);
        for pin in flexinput_core::automap::MISC_PINS {
            assert!(sdl.iter().any(|p| p == pin), "SDL layout lacks {pin}");
        }
        assert!(!sdl.iter().any(|p| p == "btn_touchpad2"), "and no pin SDL never reports");
    }
}
