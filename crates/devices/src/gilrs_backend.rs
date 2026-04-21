use glam::Vec2;
use gilrs::{Axis, Button, Gilrs};

use flexinput_core::Signal;

use crate::{
    identification::ControllerKind,
    layouts,
    DeviceBackend, PhysicalDevice,
};

pub struct GilrsBackend {
    gilrs: Gilrs,
}

impl GilrsBackend {
    pub fn try_new() -> Option<Self> {
        Gilrs::new().ok().map(|gilrs| Self { gilrs })
    }
}

impl DeviceBackend for GilrsBackend {
    fn enumerate(&self) -> Vec<PhysicalDevice> {
        self.gilrs
            .gamepads()
            .map(|(id, pad)| {
                let kind = ControllerKind::detect(
                    pad.name(),
                    pad.vendor_id(),
                    pad.product_id(),
                );
                let display_name = if kind == ControllerKind::Generic {
                    pad.name().to_string()
                } else {
                    format!("{} ({})", kind.display_name(), pad.name())
                };
                PhysicalDevice {
                    id: format!("gilrs:{}", usize::from(id)),
                    display_name,
                    kind,
                    outputs: layouts::outputs_for(kind),
                    inputs: layouts::inputs_for(kind),
                }
            })
            .collect()
    }

    fn poll(&mut self) -> Vec<(String, String, Signal)> {
        while self.gilrs.next_event().is_some() {}

        let mut out = Vec::new();

        for (id, pad) in self.gilrs.gamepads() {
            let dev = format!("gilrs:{}", usize::from(id));
            let kind = ControllerKind::detect(pad.name(), pad.vendor_id(), pad.product_id());

            // --- Individual axes ---
            for (axis, pin_id) in axis_map(kind) {
                if let Some(data) = pad.axis_data(*axis) {
                    out.push((dev.clone(), pin_id.to_string(), Signal::Float(data.value())));
                }
            }

            // --- Buttons ---
            for (button, pin_id) in button_map(kind) {
                if let Some(data) = pad.button_data(*button) {
                    out.push((dev.clone(), pin_id.to_string(), Signal::Bool(data.is_pressed())));
                }
            }

            // --- Bundled Vec2 sticks (all controllers share these axes) ---
            let lx = axis_val(&pad, Axis::LeftStickX);
            let ly = axis_val(&pad, Axis::LeftStickY);
            out.push((dev.clone(), "left_stick".into(), Signal::Vec2(Vec2::new(lx, ly))));

            let rx = axis_val(&pad, Axis::RightStickX);
            let ry = axis_val(&pad, Axis::RightStickY);
            out.push((dev.clone(), "right_stick".into(), Signal::Vec2(Vec2::new(rx, ry))));

            // DPad Vec2 only for controllers that expose it as axes
            if matches!(kind, ControllerKind::XInput | ControllerKind::DualShock4 | ControllerKind::DualSense | ControllerKind::Generic) {
                let dx = axis_val(&pad, Axis::DPadX);
                let dy = axis_val(&pad, Axis::DPadY);
                out.push((dev.clone(), "dpad".into(), Signal::Vec2(Vec2::new(dx, dy))));
            }
        }

        out
    }
}

fn axis_val(pad: &gilrs::Gamepad, axis: Axis) -> f32 {
    pad.axis_data(axis).map_or(0.0, |d| d.value())
}

// ── Per-controller axis maps ──────────────────────────────────────────────────

fn axis_map(kind: ControllerKind) -> &'static [(Axis, &'static str)] {
    match kind {
        ControllerKind::SwitchPro => AXIS_MAP_SWITCH,
        _                         => AXIS_MAP_STANDARD,
    }
}

const AXIS_MAP_STANDARD: &[(Axis, &str)] = &[
    (Axis::LeftStickX,  "left_stick_x"),
    (Axis::LeftStickY,  "left_stick_y"),
    (Axis::RightStickX, "right_stick_x"),
    (Axis::RightStickY, "right_stick_y"),
    (Axis::LeftZ,       "left_trigger"),   // XInput LT / DS4 L2
    (Axis::RightZ,      "right_trigger"),  // XInput RT / DS4 R2
    (Axis::DPadX,       "dpad_x"),
    (Axis::DPadY,       "dpad_y"),
];

// Switch Pro uses L2/R2 slot differently; ZL/ZR are digital via Nintendo driver.
const AXIS_MAP_SWITCH: &[(Axis, &str)] = &[
    (Axis::LeftStickX,  "left_stick_x"),
    (Axis::LeftStickY,  "left_stick_y"),
    (Axis::RightStickX, "right_stick_x"),
    (Axis::RightStickY, "right_stick_y"),
];

// ── Per-controller button maps ────────────────────────────────────────────────

fn button_map(kind: ControllerKind) -> &'static [(Button, &'static str)] {
    match kind {
        ControllerKind::DualShock4 | ControllerKind::DualSense => BUTTON_MAP_PLAYSTATION,
        ControllerKind::SwitchPro  => BUTTON_MAP_SWITCH,
        _                          => BUTTON_MAP_XINPUT,
    }
}

const BUTTON_MAP_XINPUT: &[(Button, &str)] = &[
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

const BUTTON_MAP_PLAYSTATION: &[(Button, &str)] = &[
    (Button::South,         "btn_cross"),
    (Button::East,          "btn_circle"),
    (Button::West,          "btn_square"),
    (Button::North,         "btn_triangle"),
    (Button::LeftTrigger,   "btn_l1"),
    (Button::RightTrigger,  "btn_r1"),
    (Button::LeftTrigger2,  "btn_l2_dig"),   // digital threshold of L2 axis
    (Button::RightTrigger2, "btn_r2_dig"),
    (Button::LeftThumb,     "btn_l3"),
    (Button::RightThumb,    "btn_r3"),
    (Button::Start,         "btn_options"),
    (Button::Select,        "btn_share"),
    (Button::Mode,          "btn_ps"),
    (Button::C,             "btn_touchpad"),
    (Button::DPadUp,        "dpad_up"),
    (Button::DPadDown,      "dpad_down"),
    (Button::DPadLeft,      "dpad_left"),
    (Button::DPadRight,     "dpad_right"),
];

// gilrs maps Nintendo buttons: South=B, East=A, West=Y, North=X (Nintendo layout)
const BUTTON_MAP_SWITCH: &[(Button, &str)] = &[
    (Button::South,         "btn_b"),
    (Button::East,          "btn_a"),
    (Button::West,          "btn_y"),
    (Button::North,         "btn_x"),
    (Button::LeftTrigger,   "btn_l"),
    (Button::RightTrigger,  "btn_r"),
    (Button::LeftTrigger2,  "btn_zl"),
    (Button::RightTrigger2, "btn_zr"),
    (Button::LeftThumb,     "btn_ls"),
    (Button::RightThumb,    "btn_rs"),
    (Button::Start,         "btn_plus"),
    (Button::Select,        "btn_minus"),
    (Button::Mode,          "btn_home"),
    (Button::C,             "btn_capture"),
    (Button::DPadUp,        "dpad_up"),
    (Button::DPadDown,      "dpad_down"),
    (Button::DPadLeft,      "dpad_left"),
    (Button::DPadRight,     "dpad_right"),
];
