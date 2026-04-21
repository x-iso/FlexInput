use vigem_client::{Client, DS4Report, DualShock4Wired, XButtons, XGamepad, Xbox360Wired};

use flexinput_core::Signal;
use crate::{layouts, DeviceKind, SinkPin, VirtualDevice};

pub static DEVICE_KINDS: &[DeviceKind] = &[
    DeviceKind { kind_id: "virtual.xinput",   display_name: "Virtual XInput Controller", allows_multiple: true },
    DeviceKind { kind_id: "virtual.ds4",      display_name: "Virtual DualShock 4",       allows_multiple: true },
    DeviceKind { kind_id: "virtual.keymouse", display_name: "Virtual Keyboard & Mouse",  allows_multiple: false },
];

pub fn create_device(kind_id: &str, instance: usize) -> Box<dyn VirtualDevice> {
    match kind_id {
        "virtual.xinput"   => Box::new(VirtualXInput::new(instance)),
        "virtual.ds4"      => Box::new(VirtualDS4::new(instance)),
        "virtual.keymouse" => Box::new(VirtualKeyMouse),
        _ => panic!("Unknown virtual device kind: {kind_id}"),
    }
}

fn instance_label(base_id: &str, base_name: &str, instance: usize) -> (String, String) {
    if instance == 0 {
        (base_id.to_string(), base_name.to_string())
    } else {
        (format!("{base_id}.{instance}"), format!("{base_name} #{}", instance + 1))
    }
}

// ── XInput ────────────────────────────────────────────────────────────────────

pub struct VirtualXInput {
    id: String,
    display_name: String,
    /// None when ViGEmBus driver is not installed.
    target: Option<Xbox360Wired<Client>>,
    thumb_lx: i16,
    thumb_ly: i16,
    thumb_rx: i16,
    thumb_ry: i16,
    left_trigger: u8,
    right_trigger: u8,
    buttons: u16,
}

impl VirtualXInput {
    pub fn new(instance: usize) -> Self {
        let (id, display_name) = instance_label("virtual.xinput", "Virtual XInput Controller", instance);
        let target = Client::connect().ok().and_then(|client| {
            let mut t = Xbox360Wired::new(client, vigem_client::TargetId::XBOX360_WIRED);
            t.plugin().ok()?;
            t.wait_ready().ok()?;
            Some(t)
        });
        Self {
            id, display_name,
            target,
            thumb_lx: 0, thumb_ly: 0,
            thumb_rx: 0, thumb_ry: 0,
            left_trigger: 0, right_trigger: 0,
            buttons: 0,
        }
    }
}

impl VirtualDevice for VirtualXInput {
    fn id(&self) -> &str { &self.id }
    fn display_name(&self) -> &str { &self.display_name }
    fn sink_pins(&self) -> &'static [SinkPin] { layouts::XINPUT_SINK_PINS }

    fn send(&mut self, pin: &str, value: Signal) {
        match pin {
            "left_stick" => if let Signal::Vec2(v) = value {
                self.thumb_lx = float_to_i16(v.x);
                self.thumb_ly = float_to_i16(v.y);
            },
            "right_stick" => if let Signal::Vec2(v) = value {
                self.thumb_rx = float_to_i16(v.x);
                self.thumb_ry = float_to_i16(v.y);
            },
            "dpad" => if let Signal::Vec2(v) = value {
                set_bit(&mut self.buttons, XButtons::LEFT,  v.x < -0.5);
                set_bit(&mut self.buttons, XButtons::RIGHT, v.x >  0.5);
                set_bit(&mut self.buttons, XButtons::UP,    v.y >  0.5);
                set_bit(&mut self.buttons, XButtons::DOWN,  v.y < -0.5);
            },
            "left_stick_x"  => if let Signal::Float(f) = value { self.thumb_lx = float_to_i16(f); },
            "left_stick_y"  => if let Signal::Float(f) = value { self.thumb_ly = float_to_i16(f); },
            "right_stick_x" => if let Signal::Float(f) = value { self.thumb_rx = float_to_i16(f); },
            "right_stick_y" => if let Signal::Float(f) = value { self.thumb_ry = float_to_i16(f); },
            "left_trigger"  => if let Signal::Float(f) = value { self.left_trigger  = float_to_u8(f); },
            "right_trigger" => if let Signal::Float(f) = value { self.right_trigger = float_to_u8(f); },
            "btn_south"  => set_bool_bit(&mut self.buttons, XButtons::A,      &value),
            "btn_east"   => set_bool_bit(&mut self.buttons, XButtons::B,      &value),
            "btn_west"   => set_bool_bit(&mut self.buttons, XButtons::X,      &value),
            "btn_north"  => set_bool_bit(&mut self.buttons, XButtons::Y,      &value),
            "btn_lb"     => set_bool_bit(&mut self.buttons, XButtons::LB,     &value),
            "btn_rb"     => set_bool_bit(&mut self.buttons, XButtons::RB,     &value),
            "btn_ls"     => set_bool_bit(&mut self.buttons, XButtons::LTHUMB, &value),
            "btn_rs"     => set_bool_bit(&mut self.buttons, XButtons::RTHUMB, &value),
            "btn_start"  => set_bool_bit(&mut self.buttons, XButtons::START,  &value),
            "btn_back"   => set_bool_bit(&mut self.buttons, XButtons::BACK,   &value),
            "btn_guide"  => set_bool_bit(&mut self.buttons, XButtons::GUIDE,  &value),
            "dpad_up"    => set_bool_bit(&mut self.buttons, XButtons::UP,     &value),
            "dpad_down"  => set_bool_bit(&mut self.buttons, XButtons::DOWN,   &value),
            "dpad_left"  => set_bool_bit(&mut self.buttons, XButtons::LEFT,   &value),
            "dpad_right" => set_bool_bit(&mut self.buttons, XButtons::RIGHT,  &value),
            _ => {}
        }
    }

    fn flush(&mut self) {
        let Some(target) = &mut self.target else { return; };
        let report = XGamepad {
            buttons: XButtons { raw: self.buttons },
            left_trigger: self.left_trigger,
            right_trigger: self.right_trigger,
            thumb_lx: self.thumb_lx,
            thumb_ly: self.thumb_ly,
            thumb_rx: self.thumb_rx,
            thumb_ry: self.thumb_ry,
        };
        let _ = target.update(&report);
    }
}

// ── DS4 ───────────────────────────────────────────────────────────────────────

pub struct VirtualDS4 {
    id: String,
    display_name: String,
    /// None when ViGEmBus driver is not installed.
    target: Option<DualShock4Wired<Client>>,
    thumb_lx: u8,
    thumb_ly: u8,
    thumb_rx: u8,
    thumb_ry: u8,
    trigger_l: u8,
    trigger_r: u8,
    /// Face/shoulder button bits (bits 4-15; dpad nibble handled separately).
    buttons: u16,
    /// PS button (bit 0) and touchpad click (bit 1).
    special: u8,
    /// DPad state: [up, right, down, left]
    dpad: [bool; 4],
}

impl VirtualDS4 {
    pub fn new(instance: usize) -> Self {
        let (id, display_name) = instance_label("virtual.ds4", "Virtual DualShock 4", instance);
        let target = Client::connect().ok().and_then(|client| {
            let mut t = DualShock4Wired::new(client, vigem_client::TargetId::DUALSHOCK4_WIRED);
            t.plugin().ok()?;
            t.wait_ready().ok()?;
            Some(t)
        });
        Self {
            id, display_name,
            target,
            thumb_lx: 0x80, thumb_ly: 0x80,
            thumb_rx: 0x80, thumb_ry: 0x80,
            trigger_l: 0, trigger_r: 0,
            buttons: 0, special: 0,
            dpad: [false; 4],
        }
    }
}

mod ds4_btn {
    pub const SQUARE:   u16 = 0x0010;
    pub const CROSS:    u16 = 0x0020;
    pub const CIRCLE:   u16 = 0x0040;
    pub const TRIANGLE: u16 = 0x0080;
    pub const L1:       u16 = 0x0100;
    pub const R1:       u16 = 0x0200;
    pub const L2_DIG:   u16 = 0x0400;
    pub const R2_DIG:   u16 = 0x0800;
    pub const SHARE:    u16 = 0x1000;
    pub const OPTIONS:  u16 = 0x2000;
    pub const L3:       u16 = 0x4000;
    pub const R3:       u16 = 0x8000;
    pub const PS:       u8  = 0x01;
    pub const TOUCHPAD: u8  = 0x02;
}

impl VirtualDevice for VirtualDS4 {
    fn id(&self) -> &str { &self.id }
    fn display_name(&self) -> &str { &self.display_name }
    fn sink_pins(&self) -> &'static [SinkPin] { layouts::DS4_SINK_PINS }

    fn send(&mut self, pin: &str, value: Signal) {
        match pin {
            "left_stick" => if let Signal::Vec2(v) = value {
                self.thumb_lx = ds4_axis_x(v.x);
                self.thumb_ly = ds4_axis_y(v.y);
            },
            "right_stick" => if let Signal::Vec2(v) = value {
                self.thumb_rx = ds4_axis_x(v.x);
                self.thumb_ry = ds4_axis_y(v.y);
            },
            "dpad" => if let Signal::Vec2(v) = value {
                self.dpad[0] = v.y >  0.5;
                self.dpad[1] = v.x >  0.5;
                self.dpad[2] = v.y < -0.5;
                self.dpad[3] = v.x < -0.5;
            },
            "left_stick_x"  => if let Signal::Float(f) = value { self.thumb_lx = ds4_axis_x(f); },
            "left_stick_y"  => if let Signal::Float(f) = value { self.thumb_ly = ds4_axis_y(f); },
            "right_stick_x" => if let Signal::Float(f) = value { self.thumb_rx = ds4_axis_x(f); },
            "right_stick_y" => if let Signal::Float(f) = value { self.thumb_ry = ds4_axis_y(f); },
            "l2"        => if let Signal::Float(f) = value { self.trigger_l = float_to_u8(f); },
            "r2"        => if let Signal::Float(f) = value { self.trigger_r = float_to_u8(f); },
            "btn_cross"    => set_bool_bit(&mut self.buttons, ds4_btn::CROSS,    &value),
            "btn_circle"   => set_bool_bit(&mut self.buttons, ds4_btn::CIRCLE,   &value),
            "btn_square"   => set_bool_bit(&mut self.buttons, ds4_btn::SQUARE,   &value),
            "btn_triangle" => set_bool_bit(&mut self.buttons, ds4_btn::TRIANGLE, &value),
            "btn_l1"       => set_bool_bit(&mut self.buttons, ds4_btn::L1,       &value),
            "btn_r1"       => set_bool_bit(&mut self.buttons, ds4_btn::R1,       &value),
            "btn_l2_dig"   => set_bool_bit(&mut self.buttons, ds4_btn::L2_DIG,   &value),
            "btn_r2_dig"   => set_bool_bit(&mut self.buttons, ds4_btn::R2_DIG,   &value),
            "btn_l3"       => set_bool_bit(&mut self.buttons, ds4_btn::L3,       &value),
            "btn_r3"       => set_bool_bit(&mut self.buttons, ds4_btn::R3,       &value),
            "btn_options"  => set_bool_bit(&mut self.buttons, ds4_btn::OPTIONS,  &value),
            "btn_share"    => set_bool_bit(&mut self.buttons, ds4_btn::SHARE,    &value),
            "btn_ps"       => set_bool_u8(&mut self.special, ds4_btn::PS,       &value),
            "btn_touchpad" => set_bool_u8(&mut self.special, ds4_btn::TOUCHPAD, &value),
            "dpad_up"    => { self.dpad[0] = matches!(value, Signal::Bool(true)); },
            "dpad_right" => { self.dpad[1] = matches!(value, Signal::Bool(true)); },
            "dpad_down"  => { self.dpad[2] = matches!(value, Signal::Bool(true)); },
            "dpad_left"  => { self.dpad[3] = matches!(value, Signal::Bool(true)); },
            _ => {}
        }
    }

    fn flush(&mut self) {
        let Some(target) = &mut self.target else { return; };
        let dpad_nibble = encode_dpad(self.dpad[0], self.dpad[1], self.dpad[2], self.dpad[3]);
        let report = DS4Report {
            thumb_lx: self.thumb_lx,
            thumb_ly: self.thumb_ly,
            thumb_rx: self.thumb_rx,
            thumb_ry: self.thumb_ry,
            buttons: (self.buttons & !0xF) | dpad_nibble,
            special: self.special,
            trigger_l: self.trigger_l,
            trigger_r: self.trigger_r,
        };
        let _ = target.update(&report);
    }
}

// ── Keyboard & Mouse ──────────────────────────────────────────────────────────

pub struct VirtualKeyMouse;

impl VirtualDevice for VirtualKeyMouse {
    fn id(&self) -> &str { "virtual.keymouse" }
    fn display_name(&self) -> &str { "Virtual Keyboard & Mouse" }
    fn sink_pins(&self) -> &'static [SinkPin] { layouts::KEYMOUSE_DEFAULT_PINS }
    fn send(&mut self, _pin: &str, _value: Signal) { /* TODO: enigo / SendInput */ }
    fn flush(&mut self) {}
}

// ── helpers ───────────────────────────────────────────────────────────────────

fn float_to_i16(f: f32) -> i16 {
    (f.clamp(-1.0, 1.0) * 32767.0) as i16
}

fn float_to_u8(f: f32) -> u8 {
    (f.clamp(0.0, 1.0) * 255.0) as u8
}

fn ds4_axis_x(f: f32) -> u8 {
    ((f.clamp(-1.0, 1.0) + 1.0) * 0.5 * 255.0) as u8
}

fn ds4_axis_y(f: f32) -> u8 {
    ((-f.clamp(-1.0, 1.0) + 1.0) * 0.5 * 255.0) as u8
}

fn encode_dpad(up: bool, right: bool, down: bool, left: bool) -> u16 {
    match (up, right, down, left) {
        (true,  false, false, false) => 0,
        (true,  true,  false, false) => 1,
        (false, true,  false, false) => 2,
        (false, true,  true,  false) => 3,
        (false, false, true,  false) => 4,
        (false, false, true,  true ) => 5,
        (false, false, false, true ) => 6,
        (true,  false, false, true ) => 7,
        _                            => 8,
    }
}

fn set_bit(bits: &mut u16, mask: u16, on: bool) {
    if on { *bits |= mask; } else { *bits &= !mask; }
}

fn set_bool_bit(bits: &mut u16, mask: u16, value: &Signal) {
    if let Signal::Bool(b) = value { set_bit(bits, mask, *b); }
}

fn set_bool_u8(bits: &mut u8, mask: u8, value: &Signal) {
    if let Signal::Bool(b) = *value {
        if b { *bits |= mask; } else { *bits &= !mask; }
    }
}
