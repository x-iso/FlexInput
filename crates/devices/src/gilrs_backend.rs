use std::collections::HashMap;
use std::time::{Duration, Instant};

use glam::Vec2;
use gilrs::{Axis, Button, Gilrs};

use flexinput_core::Signal;

use crate::{
    gyro::GyroManager,
    identification::ControllerKind,
    layouts,
    DeviceBackend, PhysicalDevice,
};

pub struct GilrsBackend {
    gilrs: Gilrs,
    /// Cached count of non-ViGEmBus physical instances per (VID, PID).
    /// Rebuilt at most once every 2 s to avoid calling SetupAPI every frame.
    phys_counts: HashMap<(u16, u16), usize>,
    /// Whether ViGEmBus has at least one virtual device for each (VID, PID).
    /// Only pairs with vigem_present=true need the physical-count dedup filter.
    vigem_present: HashMap<(u16, u16), bool>,
    phys_counts_at: Instant,
    gyro: GyroManager,
}

impl GilrsBackend {
    pub fn try_new() -> Option<Self> {
        Gilrs::new().ok().map(|gilrs| Self {
            gilrs,
            phys_counts: HashMap::new(),
            vigem_present: HashMap::new(),
            // force refresh on first enumerate()
            phys_counts_at: Instant::now() - Duration::from_secs(10),
            gyro: GyroManager::new(),
        })
    }

    fn refresh_phys_counts(&mut self) {
        self.phys_counts.clear();
        self.vigem_present.clear();
        for (_, pad) in self.gilrs.gamepads() {
            #[cfg(debug_assertions)]
            eprintln!("[gilrs] name={:?} vid={:04X?} pid={:04X?}",
                pad.name(), pad.vendor_id(), pad.product_id());
            if let Some(vp) = pad.vendor_id().zip(pad.product_id()) {
                self.vigem_present.entry(vp).or_insert_with(|| {
                    crate::hidhide::has_vigem_for_vid_pid(vp.0, vp.1)
                });
                self.phys_counts.entry(vp).or_insert_with(|| {
                    crate::hidhide::physical_count_for_vid_pid(vp.0, vp.1)
                });
            }
        }
        self.phys_counts_at = Instant::now();
    }
}

impl DeviceBackend for GilrsBackend {
    fn enumerate(&mut self) -> Vec<PhysicalDevice> {
        if self.phys_counts_at.elapsed() > Duration::from_secs(2) {
            self.refresh_phys_counts();
        }

        let mut gilrs_seen: HashMap<(u16, u16), usize> = HashMap::new();

        self.gilrs
            .gamepads()
            .filter_map(|(id, pad)| {
                if let Some(vp) = pad.vendor_id().zip(pad.product_id()) {
                    // Only dedup when ViGEmBus is confirmed to have a virtual for this
                    // VID/PID. Without that, SetupAPI's USB-format HW-ID search would
                    // return 0 for Bluetooth devices and incorrectly drop them.
                    if *self.vigem_present.get(&vp).unwrap_or(&false) {
                        let phys = *self.phys_counts.get(&vp).unwrap_or(&0);
                        let seen = gilrs_seen.entry(vp).or_insert(0);
                        if *seen >= phys {
                            return None; // extra beyond physical count → ViGEmBus virtual
                        }
                        *seen += 1;
                    }
                }

                // instance_path is only needed for HidHide; skip the SetupAPI scan for now.
                let instance_path: Option<String> = None;

                let kind = ControllerKind::detect(pad.name(), pad.vendor_id(), pad.product_id());
                let display_name = if kind == ControllerKind::Generic {
                    pad.name().to_string()
                } else {
                    kind.display_name().to_string()
                };
                Some(PhysicalDevice {
                    id: format!("gilrs:{}", usize::from(id)),
                    display_name,
                    kind,
                    outputs: layouts::outputs_for(kind),
                    inputs: layouts::inputs_for(kind),
                    instance_path,
                })
            })
            .collect()
    }

    fn poll(&mut self) -> Vec<(String, String, Signal)> {
        while self.gilrs.next_event().is_some() {}

        // Flush staged rumble / lightbar outputs *before* reading inputs so any
        // device.sink writes from the previous frame land on the controller.
        self.gyro.flush_outputs();

        let mut out = Vec::new();
        // Track per-(VID,PID) instance index for gyro correlation.
        let mut gyro_idx: HashMap<(u16, u16), usize> = HashMap::new();

        for (id, pad) in self.gilrs.gamepads() {
            let dev = format!("gilrs:{}", usize::from(id));
            let kind = ControllerKind::detect(pad.name(), pad.vendor_id(), pad.product_id());

            for (axis, pin_id) in axis_map(kind) {
                let v = pad.axis_data(*axis).map_or(0.0, |d| d.value());
                out.push((dev.clone(), pin_id.to_string(), Signal::Float(v)));
            }

            for (button, pin_id) in button_map(kind) {
                let pressed = pad.button_data(*button).map_or(false, |d| d.is_pressed());
                out.push((dev.clone(), pin_id.to_string(), Signal::Bool(pressed)));
            }

            let lx = axis_val(&pad, Axis::LeftStickX);
            let ly = axis_val(&pad, Axis::LeftStickY);
            out.push((dev.clone(), "left_stick".into(), Signal::Vec2(Vec2::new(lx, ly))));

            let rx = axis_val(&pad, Axis::RightStickX);
            let ry = axis_val(&pad, Axis::RightStickY);
            out.push((dev.clone(), "right_stick".into(), Signal::Vec2(Vec2::new(rx, ry))));

            if matches!(kind, ControllerKind::XInput | ControllerKind::DualShock4 | ControllerKind::DualSense | ControllerKind::Generic) {
                let dx = axis_val(&pad, Axis::DPadX);
                let dy = axis_val(&pad, Axis::DPadY);
                out.push((dev.clone(), "dpad".into(), Signal::Vec2(Vec2::new(dx, dy))));
            }

            // Analog triggers: ButtonData::value() works for both XInput and native HID.
            // Switch Pro ZL/ZR are digital-only and handled by button_map already.
            if !matches!(kind, ControllerKind::SwitchPro) {
                let lt = pad.button_data(Button::LeftTrigger2).map_or(0.0, |d| d.value());
                let rt = pad.button_data(Button::RightTrigger2).map_or(0.0, |d| d.value());
                let (l_pin, r_pin) = match kind {
                    ControllerKind::DualShock4 | ControllerKind::DualSense => ("l2", "r2"),
                    _ => ("left_trigger", "right_trigger"),
                };
                out.push((dev.clone(), l_pin.into(), Signal::Float(lt)));
                out.push((dev.clone(), r_pin.into(), Signal::Float(rt)));
            }

            // Gyro via raw HID for DS4 / DualSense.
            if let Some((vid, pid)) = pad.vendor_id().zip(pad.product_id()) {
                let vp = (vid, pid);
                let idx = *gyro_idx.entry(vp).or_insert(0);
                gyro_idx.insert(vp, idx + 1);

                if let Some(g) = self.gyro.read(vid, pid, idx) {
                    out.push((dev.clone(), "gyro_x".into(),  Signal::Float(g.gyro_x)));
                    out.push((dev.clone(), "gyro_y".into(),  Signal::Float(g.gyro_y)));
                    out.push((dev.clone(), "gyro_z".into(),  Signal::Float(g.gyro_z)));
                    out.push((dev.clone(), "accel_x".into(), Signal::Float(g.accel_x)));
                    out.push((dev.clone(), "accel_y".into(), Signal::Float(g.accel_y)));
                    out.push((dev.clone(), "accel_z".into(), Signal::Float(g.accel_z)));
                    if g.has_touchpad {
                        out.push((dev.clone(), "touch1_x".into(),      Signal::Float(g.touch1.x)));
                        out.push((dev.clone(), "touch1_y".into(),      Signal::Float(g.touch1.y)));
                        out.push((dev.clone(), "touch1_active".into(), Signal::Bool(g.touch1.active)));
                        out.push((dev.clone(), "touch2_x".into(),      Signal::Float(g.touch2.x)));
                        out.push((dev.clone(), "touch2_y".into(),      Signal::Float(g.touch2.y)));
                        out.push((dev.clone(), "touch2_active".into(), Signal::Bool(g.touch2.active)));
                        // gilrs's Windows backend doesn't fire Button::C for the touchpad
                        // click on DS4/DualSense, and DualSense's mute button isn't mapped
                        // at all — so we override with the values parsed from the raw HID
                        // report. Pushed last so the per-frame HashMap lookup wins.
                        out.push((dev.clone(), "btn_touchpad".into(),  Signal::Bool(g.touchpad_click)));
                    }
                    if matches!(kind, ControllerKind::DualSense) {
                        out.push((dev.clone(), "btn_mute".into(),      Signal::Bool(g.mic_button)));
                    }
                }
            }
        }

        out
    }

    fn send(&mut self, device_id: &str, pin_id: &str, signal: Signal) {
        // Currently only physical PlayStation controllers (DS4/DualSense) are
        // wired up via the raw-HID path in GyroManager. XInput rumble would
        // need a separate gilrs ff_handle path.
        let (vid, pid, idx) = match self.lookup_phys(device_id) {
            Some(t) => t,
            None => return,
        };
        let byte = match signal {
            Signal::Float(f) => (f.clamp(0.0, 1.0) * 255.0) as u8,
            Signal::Bool(b)  => if b { 255 } else { 0 },
            _ => return,
        };
        self.gyro.set_output_byte(vid, pid, idx, pin_id, byte);
    }
}

impl GilrsBackend {
    /// Resolve a `gilrs:N` device id to the (vid, pid, instance index) tuple
    /// that GyroManager keys its open HID handles by. Walks gilrs.gamepads()
    /// in the same order as `poll()` to keep the per-(vid,pid) index aligned.
    fn lookup_phys(&self, device_id: &str) -> Option<(u16, u16, usize)> {
        let mut idx_per: HashMap<(u16, u16), usize> = HashMap::new();
        for (id, pad) in self.gilrs.gamepads() {
            let dev = format!("gilrs:{}", usize::from(id));
            let vp = pad.vendor_id().zip(pad.product_id());
            if dev == device_id {
                let (v, p) = vp?;
                let idx = *idx_per.get(&(v, p)).unwrap_or(&0);
                return Some((v, p, idx));
            }
            if let Some((v, p)) = vp {
                *idx_per.entry((v, p)).or_insert(0) += 1;
            }
        }
        None
    }
}

fn axis_val(pad: &gilrs::Gamepad, axis: Axis) -> f32 {
    pad.axis_data(axis).map_or(0.0, |d| d.value())
}

fn axis_map(kind: ControllerKind) -> &'static [(Axis, &'static str)] {
    match kind {
        ControllerKind::DualShock4 | ControllerKind::DualSense => AXIS_MAP_DS4,
        ControllerKind::SwitchPro                              => AXIS_MAP_SWITCH,
        _                                                      => AXIS_MAP_STANDARD,
    }
}

const AXIS_MAP_STANDARD: &[(Axis, &str)] = &[
    (Axis::LeftStickX,  "left_stick_x"),
    (Axis::LeftStickY,  "left_stick_y"),
    (Axis::RightStickX, "right_stick_x"),
    (Axis::RightStickY, "right_stick_y"),
    // Triggers emitted separately via ButtonData::value() — Axis::LeftZ/RightZ
    // is not reliably populated on all Windows gilrs backends.
    (Axis::DPadX,       "dpad_x"),
    (Axis::DPadY,       "dpad_y"),
];

const AXIS_MAP_DS4: &[(Axis, &str)] = &[
    (Axis::LeftStickX,  "left_stick_x"),
    (Axis::LeftStickY,  "left_stick_y"),
    (Axis::RightStickX, "right_stick_x"),
    (Axis::RightStickY, "right_stick_y"),
    // Triggers emitted separately via ButtonData::value().
    (Axis::DPadX,       "dpad_x"),
    (Axis::DPadY,       "dpad_y"),
];

const AXIS_MAP_SWITCH: &[(Axis, &str)] = &[
    (Axis::LeftStickX,  "left_stick_x"),
    (Axis::LeftStickY,  "left_stick_y"),
    (Axis::RightStickX, "right_stick_x"),
    (Axis::RightStickY, "right_stick_y"),
];

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
    (Button::LeftTrigger2,  "btn_l2_dig"),
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
