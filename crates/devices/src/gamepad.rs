use flexinput_core::SignalType;
use gilrs::{Axis, Button};

use crate::DevicePin;

/// Standard output pins exposed by a gamepad source node.
/// Order here determines display order in the panel and canvas node.
///
/// Pin IDs are POSITIONAL and identical to the native layouts in `layouts.rs`
/// (`btn_ls`/`btn_rs`/`btn_back`/`btn_guide`, not the old `btn_lstick`/
/// `btn_rstick`/`btn_select`/`btn_mode`). This is load-bearing, not cosmetic:
/// the AutoMap bus (`core::automap::ALL_PINS`) fills itself by looking up each
/// canonical pin id in the device's sample map, so a Generic pad using its own
/// vocabulary silently drops those buttons from every AutoMap path — Remapper,
/// Splitter, Collector, virtual-pad wire — and from `gamepad_nav::NAV_BUTTONS`.
/// Direct pin-to-pin wires still worked, which is what made it look device-
/// specific. Guarded by `layout_button_pins_are_in_automap_bus`.
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
        pin("btn_ls",        "LS (L.Stick Click)", SignalType::Bool),
        pin("btn_rs",        "RS (R.Stick Click)", SignalType::Bool),
        // Menu / system
        pin("btn_start",     "Start / ≡",     SignalType::Bool),
        pin("btn_back",      "Back / Select / ⧉", SignalType::Bool),
        pin("btn_guide",     "Guide / Mode / ⊙",  SignalType::Bool),
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
    (Button::LeftThumb,     "btn_ls"),
    (Button::RightThumb,    "btn_rs"),
    (Button::Start,         "btn_start"),
    (Button::Select,        "btn_back"),
    (Button::Mode,          "btn_guide"),
    (Button::DPadUp,        "dpad_up"),
    (Button::DPadDown,      "dpad_down"),
    (Button::DPadLeft,      "dpad_left"),
    (Button::DPadRight,     "dpad_right"),
];
