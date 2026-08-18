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
        // Relative optical-mouse deltas, in report units per frame. Unlike
        // every other Float pin here these are NOT normalised to −1..1: mouse
        // travel has no natural full-scale, and clamping it would cap pointer
        // speed. Scale them with a curve or the RWS module.
        fl("mouse_dx", "Mouse ΔX (relative)"),
        fl("mouse_dy", "Mouse ΔY (relative)"),
        // 0 when the sensor is flat on a surface, rising as it lifts. The
        // research notes mark the exact units unknown, so it is passed through
        // raw for now and is mainly useful as a "is it on a desk" gate.
        fl("mouse_liftoff", "Mouse Lift-off"),
        fl("battery", "Battery (0–1)"),
        bo("charging", "Charging"),
    ]);
    pins.extend(joycon2_probe_outputs());
    pins
}

/// Raw, undecoded IMU bytes, exposed so they can be judged by hand.
///
/// The motion block has twelve bytes nobody has pinned down. Every attempt to
/// decode them has been a script scoring a guessed layout against a saved
/// capture, and each one only ever tested the layouts someone thought to write.
/// These pins take the other route: put the numbers where a person can watch
/// them while turning the controller. A rate field is unmistakable that way —
/// near zero at rest, swinging with the turn, back to zero — and so is the
/// absence of one.
///
/// Both readings of the same twelve bytes are here, because which is right is
/// the open question:
///
/// * `probe_i16_*` — six i16 in byte order, the layout a raw gyro + raw
///   magnetometer pair would use.
/// * `probe_i24_*` — three tagged 24-bit values, what the parser currently
///   believes. `probe_i24_2` is the heading, the one field that demonstrably
///   tracks rotation, and it is here as a CONTROL: it shows what a field that
///   really does follow the controller looks like on the same axes, in the same
///   units, at the same instant. Without it "this pin does nothing" is
///   ambiguous between a dead field and a dead wire.
///
/// Every pin is normalised by its own full scale, so ±1 is the widest value the
/// field can hold. Nothing else is applied — no bias removal, no permutation,
/// no sign flip — because the whole point is to see what the hardware sends,
/// and anything applied here could only hide it.
fn joycon2_probe_outputs() -> Vec<DevicePin> {
    vec![
        fl("probe_i16_0", "Probe i16 #0 — block bytes 0–1"),
        fl("probe_i16_1", "Probe i16 #1 — block bytes 2–3"),
        fl("probe_i16_2", "Probe i16 #2 — block bytes 4–5"),
        fl("probe_i16_3", "Probe i16 #3 — block bytes 6–7"),
        fl("probe_i16_4", "Probe i16 #4 — block bytes 8–9"),
        fl("probe_i16_5", "Probe i16 #5 — block bytes 10–11"),
        fl("probe_i24_0", "Probe i24 #0 — block bytes 1–3"),
        fl("probe_i24_1", "Probe i24 #1 — block bytes 5–7"),
        fl("probe_i24_2", "Probe i24 #2 — heading (known good, use as control)"),
        // ⭐ The controller's real gyro, recovered by differencing the fields
        // above. Normalised like every other gyro pin, so these are wireable
        // straight into an aim mapping and directly comparable in feel against
        // `gyro_x/y/z` — which derive roll and pitch from the accelerometer and
        // are therefore at their worst during a hard flick.
        //
        // Field order, not axis order: which one is roll, pitch or yaw is the
        // open question, and these pins exist to answer it by hand rather than
        // ship a guessed permutation.
        fl("probe_rate_0", "Probe rate: Roll (decoupled body rate)"),
        fl("probe_rate_1", "Probe rate: Pitch (decoupled body rate)"),
        fl("probe_rate_2", "Probe rate: Yaw (decoupled body rate)"),
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
        // The rail buttons, only reachable when the Joy-Con is detached. No
        // positional equivalent on a standard pad, so they keep their own ids.
        bo("btn_sl", "SL"),
        bo("btn_sr", "SR"),
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
        bo("btn_sl", "SL"),
        bo("btn_sr", "SR"),
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
        fl("hd_l_freq", "HD Rumble: Frequency (0=82Hz 0.5=320Hz 1=626Hz)"),
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
        // Mode: 0=off, 0.33=Feedback(constant resist), 0.66=Weapon(click), 1=Vibration
        fl("trigger_r_mode",     "R.Trigger Mode (0=off 0.33=resist 0.66=click 1=vib)"),
        // Start/End: trigger travel position, 0=rest 1=fully pressed
        fl("trigger_r_start",    "R.Trigger Start (0=rest 1=full)"),
        // End only used in Weapon(click) mode
        fl("trigger_r_end",      "R.Trigger End (Weapon mode only)"),
        // Strength: 0=none 1=max
        fl("trigger_r_strength", "R.Trigger Strength (0–1)"),
        // Freq: vibration speed, only used in Vibration mode
        fl("trigger_r_freq",     "R.Trigger Freq (Vibration mode only)"),
        fl("trigger_l_mode",     "L.Trigger Mode (0=off 0.33=resist 0.66=click 1=vib)"),
        fl("trigger_l_start",    "L.Trigger Start (0=rest 1=full)"),
        fl("trigger_l_end",      "L.Trigger End (Weapon mode only)"),
        fl("trigger_l_strength", "L.Trigger Strength (0–1)"),
        fl("trigger_l_freq",     "L.Trigger Freq (Vibration mode only)"),
    ]);
    pins
}

fn switch_pro_inputs() -> Vec<DevicePin> {
    vec![
        // Per-side amplitude + carrier frequency.
        // hd_l/r_amp:  0=silent → 1=max; perceptual power-law curve (more resolution at low amp).
        // hd_l/r_freq: 0=82 Hz → 1=626 Hz; linear over safe dual-band range.
        fl("hd_l_amp",  "HD Rumble L: Amplitude (perceptual 0–1)"),
        fl("hd_l_freq", "HD Rumble L: Frequency (0=82Hz 0.5=320Hz 1=626Hz)"),
        fl("hd_r_amp",  "HD Rumble R: Amplitude (perceptual 0–1)"),
        fl("hd_r_freq", "HD Rumble R: Frequency (0=82Hz 0.5=320Hz 1=626Hz)"),
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
