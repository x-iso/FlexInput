use flexinput_core::SignalType;

use crate::{identification::ControllerKind, DevicePin};

pub fn outputs_for(kind: ControllerKind) -> Vec<DevicePin> {
    match kind {
        ControllerKind::XInput     => xinput_outputs(),
        ControllerKind::DualShock4 => ds4_outputs(),
        ControllerKind::DualSense  => dualsense_outputs(),
        ControllerKind::SwitchPro  => switch_pro_outputs(),
        ControllerKind::Generic    => generic_outputs(),
        // MIDI devices build their own pin lists; layouts not used.
        ControllerKind::MidiIn | ControllerKind::MidiOut => vec![],
    }
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
        f2("left_stick",   "Left Stick",   SignalType::Vec2),
        f2("right_stick",  "Right Stick",  SignalType::Vec2),
        f2("dpad",         "D-Pad",        SignalType::Vec2),
        fl("left_stick_x", "L.Stick X"),
        fl("left_stick_y", "L.Stick Y"),
        fl("right_stick_x","R.Stick X"),
        fl("right_stick_y","R.Stick Y"),
        fl("l2",           "L2 (analog)"),
        fl("r2",           "R2 (analog)"),
        fl("dpad_x",       "D-Pad X"),
        fl("dpad_y",       "D-Pad Y"),
        bo("btn_cross",    "Cross (X)"),
        bo("btn_circle",   "Circle (O)"),
        bo("btn_square",   "Square"),
        bo("btn_triangle", "Triangle"),
        bo("btn_l1",       "L1"),
        bo("btn_r1",       "R1"),
        bo("btn_l3",       "L3 (L.Stick Click)"),
        bo("btn_r3",       "R3 (R.Stick Click)"),
        bo("btn_options",  "Options"),
        bo("btn_share",    "Share / Create"),
        bo("btn_ps",       "PS Button"),
        bo("btn_touchpad", "Touchpad Click"),
        bo("dpad_up",      "D-Pad Up"),
        bo("dpad_down",    "D-Pad Down"),
        bo("dpad_left",    "D-Pad Left"),
        bo("dpad_right",   "D-Pad Right"),
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
    // Nintendo face-button layout: A=right, B=bottom (opposite to Xbox convention).
    let mut pins = vec![
        f2("left_stick",   "Left Stick",   SignalType::Vec2),
        f2("right_stick",  "Right Stick",  SignalType::Vec2),
        fl("left_stick_x", "L.Stick X"),
        fl("left_stick_y", "L.Stick Y"),
        fl("right_stick_x","R.Stick X"),
        fl("right_stick_y","R.Stick Y"),
        // Nintendo face buttons (gilrs maps: South=B, East=A, West=Y, North=X)
        bo("btn_b",        "B (bottom)"),
        bo("btn_a",        "A (right)"),
        bo("btn_y",        "Y (left)"),
        bo("btn_x",        "X (top)"),
        // Shoulder
        bo("btn_l",        "L"),
        bo("btn_r",        "R"),
        // ZL/ZR are digital on Switch Pro (no analog axis via Nintendo driver)
        bo("btn_zl",       "ZL"),
        bo("btn_zr",       "ZR"),
        // Stick clicks
        bo("btn_ls",       "L.Stick Click"),
        bo("btn_rs",       "R.Stick Click"),
        // Menu
        bo("btn_plus",     "+ (Plus / Start)"),
        bo("btn_minus",    "- (Minus / Select)"),
        bo("btn_home",     "Home"),
        bo("btn_capture",  "Capture"),
        // D-Pad (discrete buttons on Switch Pro, not an analog hat)
        bo("dpad_up",      "D-Pad Up"),
        bo("dpad_down",    "D-Pad Down"),
        bo("dpad_left",    "D-Pad Left"),
        bo("dpad_right",   "D-Pad Right"),
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
        fl("haptic_l",             "Haptic L"),
        fl("haptic_r",             "Haptic R"),
        fl("adaptive_trigger_l",   "Adaptive Trigger L"),
        fl("adaptive_trigger_r",   "Adaptive Trigger R"),
    ]);
    pins
}

fn switch_pro_inputs() -> Vec<DevicePin> {
    vec![
        fl("hd_rumble_l", "HD Rumble L"),
        fl("hd_rumble_r", "HD Rumble R"),
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
