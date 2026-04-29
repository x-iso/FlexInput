use flexinput_core::SignalType;
use crate::SinkPin;

macro_rules! sp {
    ($id:expr, $name:expr, $t:expr) => {
        SinkPin { id: $id, display_name: $name, signal_type: $t }
    };
}

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
    sp!("mouse_x", "Mouse X (delta)", SignalType::Float),
    sp!("mouse_y", "Mouse Y (delta)", SignalType::Float),
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

pub static DS4_SINK_PINS: &[SinkPin] = &[
    sp!("left_stick",    "Left Stick",        SignalType::Vec2),
    sp!("right_stick",   "Right Stick",       SignalType::Vec2),
    sp!("dpad",          "D-Pad",             SignalType::Vec2),
    sp!("left_stick_x",  "L.Stick X",         SignalType::Float),
    sp!("left_stick_y",  "L.Stick Y",         SignalType::Float),
    sp!("right_stick_x", "R.Stick X",         SignalType::Float),
    sp!("right_stick_y", "R.Stick Y",         SignalType::Float),
    sp!("l2",            "L2 (analog)",       SignalType::Float),
    sp!("r2",            "R2 (analog)",       SignalType::Float),
    sp!("btn_cross",     "Cross (X)",         SignalType::Bool),
    sp!("btn_circle",    "Circle (O)",        SignalType::Bool),
    sp!("btn_square",    "Square",            SignalType::Bool),
    sp!("btn_triangle",  "Triangle",          SignalType::Bool),
    sp!("btn_l1",        "L1",                SignalType::Bool),
    sp!("btn_r1",        "R1",                SignalType::Bool),
    sp!("btn_l2_dig",    "L2 (digital)",      SignalType::Bool),
    sp!("btn_r2_dig",    "R2 (digital)",      SignalType::Bool),
    sp!("btn_l3",        "L3",                SignalType::Bool),
    sp!("btn_r3",        "R3",                SignalType::Bool),
    sp!("btn_options",   "Options",           SignalType::Bool),
    sp!("btn_share",     "Share / Create",    SignalType::Bool),
    sp!("btn_ps",        "PS Button",         SignalType::Bool),
    sp!("btn_touchpad",  "Touchpad Click",    SignalType::Bool),
    sp!("dpad_up",       "D-Pad Up",          SignalType::Bool),
    sp!("dpad_down",     "D-Pad Down",        SignalType::Bool),
    sp!("dpad_left",     "D-Pad Left",        SignalType::Bool),
    sp!("dpad_right",    "D-Pad Right",       SignalType::Bool),
    sp!("gyro_x",        "Gyro X",            SignalType::Float),
    sp!("gyro_y",        "Gyro Y",            SignalType::Float),
    sp!("gyro_z",        "Gyro Z",            SignalType::Float),
    sp!("accel_x",       "Accel X",           SignalType::Float),
    sp!("accel_y",       "Accel Y",           SignalType::Float),
    sp!("accel_z",       "Accel Z",           SignalType::Float),
    // Auto-map bus port — always last to avoid shifting existing pin indices.
    sp!("automap_in",    "Auto-Map",          SignalType::AutoMap),
];
