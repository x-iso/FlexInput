use flexinput_core::SignalType;
use gilrs::{Axis, Button};

use crate::DevicePin;

/// Standard output pins exposed by a gamepad source node.
/// Order here determines display order in the panel and canvas node.
pub fn standard_outputs() -> Vec<DevicePin> {
    vec![
        // Bundled Vec2 sticks (most useful for direct routing)
        pin("left_stick",    "Left Stick",    SignalType::Vec2),
        pin("right_stick",   "Right Stick",   SignalType::Vec2),
        pin("dpad",          "D-Pad",         SignalType::Vec2),
        // Individual float axes
        pin("left_stick_x",  "L.Stick X",     SignalType::Float),
        pin("left_stick_y",  "L.Stick Y",     SignalType::Float),
        pin("right_stick_x", "R.Stick X",     SignalType::Float),
        pin("right_stick_y", "R.Stick Y",     SignalType::Float),
        pin("left_trigger",  "L.Trigger",     SignalType::Float),
        pin("right_trigger", "R.Trigger",     SignalType::Float),
        pin("dpad_x",        "D-Pad X",       SignalType::Float),
        pin("dpad_y",        "D-Pad Y",       SignalType::Float),
        // Face buttons
        pin("btn_south",     "South (A/✕)",   SignalType::Bool),
        pin("btn_east",      "East (B/○)",    SignalType::Bool),
        pin("btn_west",      "West (X/□)",    SignalType::Bool),
        pin("btn_north",     "North (Y/△)",   SignalType::Bool),
        // Shoulder / trigger buttons
        pin("btn_lb",        "LB / L1",       SignalType::Bool),
        pin("btn_rb",        "RB / R1",       SignalType::Bool),
        pin("btn_lt_dig",    "LT dig. / L2",  SignalType::Bool),
        pin("btn_rt_dig",    "RT dig. / R2",  SignalType::Bool),
        // Stick clicks
        pin("btn_lstick",    "L.Stick Click", SignalType::Bool),
        pin("btn_rstick",    "R.Stick Click", SignalType::Bool),
        // Menu / system
        pin("btn_start",     "Start / ≡",     SignalType::Bool),
        pin("btn_select",    "Select / ⧉",    SignalType::Bool),
        pin("btn_mode",      "Mode / PS / ⊙", SignalType::Bool),
        // D-pad as discrete buttons (when not exposed as axis)
        pin("dpad_up",       "D-Pad Up",      SignalType::Bool),
        pin("dpad_down",     "D-Pad Down",    SignalType::Bool),
        pin("dpad_left",     "D-Pad Left",    SignalType::Bool),
        pin("dpad_right",    "D-Pad Right",   SignalType::Bool),
    ]
}

/// Haptic / force-feedback input pins (signals going *to* the device).
pub fn standard_inputs() -> Vec<DevicePin> {
    vec![
        pin("rumble_strong", "Rumble (strong)", SignalType::Float),
        pin("rumble_weak",   "Rumble (weak)",   SignalType::Float),
    ]
}

fn pin(id: &str, name: &str, signal_type: SignalType) -> DevicePin {
    DevicePin {
        id: id.to_string(),
        display_name: name.to_string(),
        signal_type,
    }
}

/// gilrs axis → our pin ID.
pub const AXIS_MAP: &[(Axis, &str)] = &[
    (Axis::LeftStickX,  "left_stick_x"),
    (Axis::LeftStickY,  "left_stick_y"),
    (Axis::RightStickX, "right_stick_x"),
    (Axis::RightStickY, "right_stick_y"),
    (Axis::LeftZ,       "left_trigger"),
    (Axis::RightZ,      "right_trigger"),
    (Axis::DPadX,       "dpad_x"),
    (Axis::DPadY,       "dpad_y"),
];

/// gilrs button → our pin ID.
pub const BUTTON_MAP: &[(Button, &str)] = &[
    (Button::South,         "btn_south"),
    (Button::East,          "btn_east"),
    (Button::West,          "btn_west"),
    (Button::North,         "btn_north"),
    (Button::LeftTrigger,   "btn_lb"),
    (Button::RightTrigger,  "btn_rb"),
    (Button::LeftTrigger2,  "btn_lt_dig"),
    (Button::RightTrigger2, "btn_rt_dig"),
    (Button::LeftThumb,     "btn_lstick"),
    (Button::RightThumb,    "btn_rstick"),
    (Button::Start,         "btn_start"),
    (Button::Select,        "btn_select"),
    (Button::Mode,          "btn_mode"),
    (Button::DPadUp,        "dpad_up"),
    (Button::DPadDown,      "dpad_down"),
    (Button::DPadLeft,      "dpad_left"),
    (Button::DPadRight,     "dpad_right"),
];
