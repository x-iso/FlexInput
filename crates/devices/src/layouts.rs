use flexinput_core::SignalType;

use crate::{identification::ControllerKind, DevicePin};

pub fn outputs_for(kind: ControllerKind) -> Vec<DevicePin> {
    let mut pins = match kind {
        ControllerKind::XInput     => xinput_outputs(),
        ControllerKind::DualShock4 => ds4_outputs(),
        ControllerKind::DualSense  => dualsense_outputs(),
        ControllerKind::SwitchPro  => switch_pro_outputs(),
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

// ── Generic fallback ──────────────────────────────────────────────────────────

fn generic_outputs() -> Vec<DevicePin> {
    // Re-use the old generic list from gamepad.rs for anything unrecognised.
    crate::gamepad::standard_outputs()
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
