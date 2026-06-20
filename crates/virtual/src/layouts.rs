use flexinput_core::SignalType;
use crate::{SinkPin, SourcePin};

macro_rules! sp {
    ($id:expr, $name:expr, $t:expr) => {
        SinkPin { id: $id, display_name: $name, signal_type: $t }
    };
}

macro_rules! op {
    ($id:expr, $name:expr, $t:expr) => {
        SourcePin { id: $id, display_name: $name, signal_type: $t }
    };
}

pub static XINPUT_SOURCE_PINS: &[SourcePin] = &[
    op!("rumble_strong", "Rumble Strong (large motor)", SignalType::Float),
    op!("rumble_weak",   "Rumble Weak (small motor)",   SignalType::Float),
];

pub static DS4_SOURCE_PINS: &[SourcePin] = &[
    op!("rumble_strong", "Rumble Strong",  SignalType::Float),
    op!("rumble_weak",   "Rumble Weak",    SignalType::Float),
    op!("lightbar_r",    "Lightbar R",     SignalType::Float),
    op!("lightbar_g",    "Lightbar G",     SignalType::Float),
    op!("lightbar_b",    "Lightbar B",     SignalType::Float),
];

/// DualSense feedback OUTPUTS a game can drive on the virtual pad: classic rumble
/// + lightbar (as DS4) PLUS the player/mic LEDs and both adaptive triggers. Pin
/// IDs match the physical DualSense's haptic INPUT pins (see
/// `flexinput_devices::layouts::dualsense_inputs`) so AutoMap / FEEDBACK_PAIRS
/// route each straight through to a real pad. All Float, normalized 0–1 the same
/// way the physical input pins expect (the read side rescales per pin).
pub static DUALSENSE_SOURCE_PINS: &[SourcePin] = &[
    op!("rumble_strong",      "Rumble Strong",            SignalType::Float),
    op!("rumble_weak",        "Rumble Weak",              SignalType::Float),
    op!("lightbar_r",         "Lightbar R",               SignalType::Float),
    op!("lightbar_g",         "Lightbar G",               SignalType::Float),
    op!("lightbar_b",         "Lightbar B",               SignalType::Float),
    op!("player_led",         "Player LED",               SignalType::Float),
    op!("mic_led",            "Mic LED",                  SignalType::Float),
    op!("trigger_r_mode",     "R.Trigger Mode",           SignalType::Float),
    op!("trigger_r_start",    "R.Trigger Start",          SignalType::Float),
    op!("trigger_r_end",      "R.Trigger End",            SignalType::Float),
    op!("trigger_r_strength", "R.Trigger Strength",       SignalType::Float),
    op!("trigger_r_freq",     "R.Trigger Freq",           SignalType::Float),
    op!("trigger_l_mode",     "L.Trigger Mode",           SignalType::Float),
    op!("trigger_l_start",    "L.Trigger Start",          SignalType::Float),
    op!("trigger_l_end",      "L.Trigger End",            SignalType::Float),
    op!("trigger_l_strength", "L.Trigger Strength",       SignalType::Float),
    op!("trigger_l_freq",     "L.Trigger Freq",           SignalType::Float),
];

pub static KEYMOUSE_DEFAULT_PINS: &[SinkPin] = &[
    // Modifier / special keys — always present, non-removable
    sp!("key_escape",  "Escape",  SignalType::Bool),
    sp!("key_shift",   "Shift",   SignalType::Bool),
    sp!("key_ctrl",    "Ctrl",    SignalType::Bool),
    sp!("key_alt",     "Alt",     SignalType::Bool),
    sp!("key_win",     "Win",     SignalType::Bool),
    // Mouse buttons
    sp!("mouse_left",    "LMB",              SignalType::Bool),
    sp!("mouse_right",   "RMB",              SignalType::Bool),
    sp!("mouse_middle",  "MMB",              SignalType::Bool),
    sp!("mouse_back",    "Mouse 4 (Back)",   SignalType::Bool),
    sp!("mouse_forward", "Mouse 5 (Forward)", SignalType::Bool),
    // Scroll (discrete Bool pulses — one true frame per tick)
    sp!("scroll_up",   "Scroll Up",   SignalType::Bool),
    sp!("scroll_down", "Scroll Down", SignalType::Bool),
    // Mouse axes (Float delta per frame; e.g. stick deflection → cursor speed)
    sp!("mouse",   "Mouse XY (delta)", SignalType::Vec2),
    sp!("mouse_x", "Mouse X (delta)",  SignalType::Float),
    sp!("mouse_y", "Mouse Y (delta)",  SignalType::Float),
    // Auto-map bus port — last so existing pin indices stay stable.
    sp!("automap_in", "Auto-Map", SignalType::AutoMap),
];

pub static XINPUT_SINK_PINS: &[SinkPin] = &[
    sp!("left_stick",    "Left Stick",        SignalType::Vec2),
    sp!("right_stick",   "Right Stick",       SignalType::Vec2),
    sp!("dpad",          "D-Pad",             SignalType::Vec2),
    sp!("left_stick_x",  "L.Stick X",         SignalType::Float),
    sp!("left_stick_y",  "L.Stick Y",         SignalType::Float),
    sp!("right_stick_x", "R.Stick X",         SignalType::Float),
    sp!("right_stick_y", "R.Stick Y",         SignalType::Float),
    sp!("left_trigger",  "L.Trigger (LT)",    SignalType::Float),
    sp!("right_trigger", "R.Trigger (RT)",    SignalType::Float),
    sp!("btn_south",     "A",                 SignalType::Bool),
    sp!("btn_east",      "B",                 SignalType::Bool),
    sp!("btn_west",      "X",                 SignalType::Bool),
    sp!("btn_north",     "Y",                 SignalType::Bool),
    sp!("btn_lb",        "LB",                SignalType::Bool),
    sp!("btn_rb",        "RB",                SignalType::Bool),
    sp!("btn_ls",        "LS (L.Stick Click)", SignalType::Bool),
    sp!("btn_rs",        "RS (R.Stick Click)", SignalType::Bool),
    sp!("btn_start",     "Start / Menu",      SignalType::Bool),
    sp!("btn_back",      "Back / View",       SignalType::Bool),
    sp!("btn_guide",     "Guide / Xbox",      SignalType::Bool),
    sp!("dpad_up",       "D-Pad Up",          SignalType::Bool),
    sp!("dpad_down",     "D-Pad Down",        SignalType::Bool),
    sp!("dpad_left",     "D-Pad Left",        SignalType::Bool),
    sp!("dpad_right",    "D-Pad Right",       SignalType::Bool),
    // Auto-map bus port — always last to avoid shifting existing pin indices.
    sp!("automap_in",    "Auto-Map",          SignalType::AutoMap),
];

pub static DUALSENSE_SINK_PINS: &[SinkPin] = &[
    sp!("left_stick",    "Left Stick",           SignalType::Vec2),
    sp!("right_stick",   "Right Stick",          SignalType::Vec2),
    sp!("dpad",          "D-Pad",                SignalType::Vec2),
    sp!("left_stick_x",  "L.Stick X",            SignalType::Float),
    sp!("left_stick_y",  "L.Stick Y",            SignalType::Float),
    sp!("right_stick_x", "R.Stick X",            SignalType::Float),
    sp!("right_stick_y", "R.Stick Y",            SignalType::Float),
    sp!("left_trigger",  "L2 / LT (analog)",     SignalType::Float),
    sp!("right_trigger", "R2 / RT (analog)",     SignalType::Float),
    sp!("dpad_x",        "D-Pad X",              SignalType::Float),
    sp!("dpad_y",        "D-Pad Y",              SignalType::Float),
    sp!("btn_south",     "South (Cross/X)",      SignalType::Bool),
    sp!("btn_east",      "East (Circle/O)",      SignalType::Bool),
    sp!("btn_west",      "West (Square)",        SignalType::Bool),
    sp!("btn_north",     "North (Triangle)",     SignalType::Bool),
    sp!("btn_lb",        "LB / L1",              SignalType::Bool),
    sp!("btn_rb",        "RB / R1",              SignalType::Bool),
    sp!("btn_lt_dig",    "LT digital / L2",      SignalType::Bool),
    sp!("btn_rt_dig",    "RT digital / R2",      SignalType::Bool),
    sp!("btn_ls",        "LS (L.Stick Click)",   SignalType::Bool),
    sp!("btn_rs",        "RS (R.Stick Click)",   SignalType::Bool),
    sp!("btn_start",     "Start / Options",      SignalType::Bool),
    sp!("btn_back",      "Back / Create",        SignalType::Bool),
    sp!("btn_guide",     "Guide / PS",           SignalType::Bool),
    sp!("btn_touchpad",  "Touchpad Click",       SignalType::Bool),
    sp!("btn_mute",      "Mute",                 SignalType::Bool),
    sp!("dpad_up",       "D-Pad Up",             SignalType::Bool),
    sp!("dpad_down",     "D-Pad Down",           SignalType::Bool),
    sp!("dpad_left",     "D-Pad Left",           SignalType::Bool),
    sp!("dpad_right",    "D-Pad Right",          SignalType::Bool),
    sp!("gyro_x",        "Gyro X",               SignalType::Float),
    sp!("gyro_y",        "Gyro Y",               SignalType::Float),
    sp!("gyro_z",        "Gyro Z",               SignalType::Float),
    sp!("accel_x",       "Accel X",              SignalType::Float),
    sp!("accel_y",       "Accel Y",              SignalType::Float),
    sp!("accel_z",       "Accel Z",              SignalType::Float),
    // Touchpad axes — forwarded into the report when `active` is set (encoder
    // packs X/Y into the finger bytes; see hidmaestro encode_extended).
    sp!("touch1_x",      "Touch 1 X",            SignalType::Float),
    sp!("touch1_y",      "Touch 1 Y",            SignalType::Float),
    sp!("touch1_active", "Touch 1 Active",       SignalType::Bool),
    sp!("touch2_x",      "Touch 2 X",            SignalType::Float),
    sp!("touch2_y",      "Touch 2 Y",            SignalType::Float),
    sp!("touch2_active", "Touch 2 Active",       SignalType::Bool),
    // No battery sink: the virtual pad always reports full charge (a low
    // reported battery makes some hosts disable haptics). Battery remains a
    // SOURCE pin on physical devices.
    sp!("automap_in",    "Auto-Map",             SignalType::AutoMap),
];

pub static DS4_SINK_PINS: &[SinkPin] = &[
    sp!("left_stick",    "Left Stick",           SignalType::Vec2),
    sp!("right_stick",   "Right Stick",          SignalType::Vec2),
    sp!("dpad",          "D-Pad",                SignalType::Vec2),
    sp!("left_stick_x",  "L.Stick X",            SignalType::Float),
    sp!("left_stick_y",  "L.Stick Y",            SignalType::Float),
    sp!("right_stick_x", "R.Stick X",            SignalType::Float),
    sp!("right_stick_y", "R.Stick Y",            SignalType::Float),
    sp!("left_trigger",  "L2 / LT (analog)",     SignalType::Float),
    sp!("right_trigger", "R2 / RT (analog)",     SignalType::Float),
    sp!("btn_south",     "South (Cross/X)",      SignalType::Bool),
    sp!("btn_east",      "East (Circle/O)",      SignalType::Bool),
    sp!("btn_west",      "West (Square)",        SignalType::Bool),
    sp!("btn_north",     "North (Triangle)",     SignalType::Bool),
    sp!("btn_lb",        "LB / L1",              SignalType::Bool),
    sp!("btn_rb",        "RB / R1",              SignalType::Bool),
    sp!("btn_lt_dig",    "LT digital / L2",      SignalType::Bool),
    sp!("btn_rt_dig",    "RT digital / R2",      SignalType::Bool),
    sp!("btn_ls",        "LS (L.Stick Click)",   SignalType::Bool),
    sp!("btn_rs",        "RS (R.Stick Click)",   SignalType::Bool),
    sp!("btn_start",     "Start / Options",      SignalType::Bool),
    sp!("btn_back",      "Back / Share",         SignalType::Bool),
    sp!("btn_guide",     "Guide / PS",           SignalType::Bool),
    sp!("btn_touchpad",  "Touchpad Click",       SignalType::Bool),
    sp!("dpad_up",       "D-Pad Up",             SignalType::Bool),
    sp!("dpad_down",     "D-Pad Down",           SignalType::Bool),
    sp!("dpad_left",     "D-Pad Left",           SignalType::Bool),
    sp!("dpad_right",    "D-Pad Right",          SignalType::Bool),
    sp!("gyro_x",        "Gyro X",               SignalType::Float),
    sp!("gyro_y",        "Gyro Y",               SignalType::Float),
    sp!("gyro_z",        "Gyro Z",               SignalType::Float),
    sp!("accel_x",       "Accel X",              SignalType::Float),
    sp!("accel_y",       "Accel Y",              SignalType::Float),
    sp!("accel_z",       "Accel Z",              SignalType::Float),
    // Touchpad axes — forwarded into the report when `active` is set (encoder
    // packs X/Y into the finger bytes; see hidmaestro encode_extended).
    sp!("touch1_x",      "Touch 1 X",            SignalType::Float),
    sp!("touch1_y",      "Touch 1 Y",            SignalType::Float),
    sp!("touch1_active", "Touch 1 Active",       SignalType::Bool),
    sp!("touch2_x",      "Touch 2 X",            SignalType::Float),
    sp!("touch2_y",      "Touch 2 Y",            SignalType::Float),
    sp!("touch2_active", "Touch 2 Active",       SignalType::Bool),
    // Auto-map bus port — always last to avoid shifting existing pin indices.
    sp!("automap_in",    "Auto-Map",             SignalType::AutoMap),
];
