use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use enigo::{Axis, Button, Coordinate, Direction, Enigo, Key, Keyboard, Mouse, Settings};
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
        "virtual.keymouse" => Box::new(VirtualKeyMouse::new()),
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
    fn is_connected(&self) -> bool { self.target.is_some() }

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
    fn is_connected(&self) -> bool { self.target.is_some() }

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

#[repr(C)]
struct CursorPoint { x: i32, y: i32 }
extern "system" { fn GetCursorPos(lp: *mut CursorPoint) -> i32; }
fn cursor_pos() -> Option<(i32, i32)> {
    let mut p = CursorPoint { x: 0, y: 0 };
    (unsafe { GetCursorPos(&mut p) } != 0).then_some((p.x, p.y))
}

#[derive(Default, Clone, Copy)]
struct MouseButtons { lmb: bool, rmb: bool, mmb: bool, mb4: bool, mb5: bool }

#[derive(Default, Clone, Copy)]
struct KeysHeld { escape: bool, shift: bool, ctrl: bool, alt: bool, win: bool }

// ── Shared state between UI thread and the 500 Hz mouse thread ───────────────

#[derive(Default, Clone)]
struct MouseShared {
    /// Desired velocity in "pixels per 60 Hz reference frame".
    /// The mouse thread converts: px_per_tick = vel * (60 / 500).
    vel_x: f32,
    vel_y: f32,
    /// Accumulated scroll clicks — consumed (zeroed) by the mouse thread.
    scroll_pending: i32,
    buttons: MouseButtons,
    muted: bool,
    suppression_enabled: bool,
    /// Set to true when VirtualKeyMouse is dropped — signals the thread to exit.
    stop: bool,
}

// ── 500 Hz mouse thread ───────────────────────────────────────────────────────

fn mouse_thread(shared: Arc<Mutex<MouseShared>>) {
    const HZ: f32    = 500.0;
    const REF: f32   = 60.0;
    const SCALE: f32 = REF / HZ; // velocity → per-tick pixels

    let mut enigo = Enigo::new(&Settings::default()).ok();
    let mut carry_x = 0.0f32;
    let mut carry_y = 0.0f32;
    let mut os_buttons = MouseButtons::default();
    let mut last_cursor  = cursor_pos();
    let mut blocked_until: Option<Instant> = None;
    let mut suppress_cooldown = 0u8;

    let tick = Duration::from_micros((1_000_000.0 / HZ) as u64);

    loop {
        let t0 = Instant::now();

        // Snapshot desired state; consume scroll atomically.
        let state = {
            let mut s = shared.lock().unwrap();
            if s.stop { break; }
            let snap = s.clone();
            s.scroll_pending = 0;
            snap
        };

        if let Some(ref mut e) = enigo {
            let now = Instant::now();

            // Physical mouse suppression
            let cur = cursor_pos();
            let suppressed = if state.suppression_enabled {
                if let (Some(pos), Some(last)) = (cur, last_cursor) {
                    if pos != last {
                        if suppress_cooldown > 0 {
                            suppress_cooldown -= 1;
                        } else {
                            blocked_until = Some(now + Duration::from_millis(500));
                        }
                    }
                }
                last_cursor = cur;
                blocked_until.map_or(false, |t| now < t)
            } else {
                last_cursor = cur;
                false
            };

            macro_rules! btn_sync {
                ($os:expr, $want:expr, $btn:expr) => {
                    if $want != $os {
                        let _ = e.button($btn, if $want { Direction::Press } else { Direction::Release });
                        $os = $want;
                    }
                };
            }
            macro_rules! btn_release {
                ($os:expr, $btn:expr) => {
                    if $os { let _ = e.button($btn, Direction::Release); $os = false; }
                };
            }

            if !state.muted && !suppressed {
                carry_x += state.vel_x * SCALE;
                carry_y += state.vel_y * SCALE;
                let dx = carry_x.trunc() as i32;
                let dy = carry_y.trunc() as i32;
                carry_x -= dx as f32;
                carry_y -= dy as f32;
                if dx != 0 || dy != 0 {
                    let _ = e.move_mouse(dx, dy, Coordinate::Rel);
                    if let Some(ref mut last) = last_cursor {
                        last.0 += dx;
                        last.1 += dy;
                    }
                    suppress_cooldown = 8; // ~16 ms at 500 Hz before suppression re-arms
                }
                if state.scroll_pending != 0 {
                    let _ = e.scroll(state.scroll_pending, Axis::Vertical);
                }
                btn_sync!(os_buttons.lmb, state.buttons.lmb, Button::Left);
                btn_sync!(os_buttons.rmb, state.buttons.rmb, Button::Right);
                btn_sync!(os_buttons.mmb, state.buttons.mmb, Button::Middle);
                btn_sync!(os_buttons.mb4, state.buttons.mb4, Button::Back);
                btn_sync!(os_buttons.mb5, state.buttons.mb5, Button::Forward);
            } else {
                carry_x = 0.0;
                carry_y = 0.0;
                btn_release!(os_buttons.lmb, Button::Left);
                btn_release!(os_buttons.rmb, Button::Right);
                btn_release!(os_buttons.mmb, Button::Middle);
                btn_release!(os_buttons.mb4, Button::Back);
                btn_release!(os_buttons.mb5, Button::Forward);
            }
        }

        let elapsed = t0.elapsed();
        if elapsed < tick {
            std::thread::sleep(tick - elapsed);
        }
    }
}

// ── VirtualKeyMouse ───────────────────────────────────────────────────────────

pub struct VirtualKeyMouse {
    pub suppression_enabled: bool,
    pub muted: bool,

    // Desired per-frame velocity / state set by send()
    mouse_vel_x: f32,
    mouse_vel_y: f32,
    scroll_delta: i32,
    buttons: MouseButtons,
    keys: KeysHeld,
    learned_keys: HashMap<String, bool>,

    // Keyboard output on the UI thread (no need for 500 Hz)
    enigo_keys: Option<Enigo>,
    os_keys: KeysHeld,
    os_learned_keys: HashMap<String, bool>,

    // Shared with the mouse thread
    mouse_shared: Arc<Mutex<MouseShared>>,
    _mouse_thread: std::thread::JoinHandle<()>,

    enigo_ok: bool,
}

impl VirtualKeyMouse {
    pub fn new() -> Self {
        let shared = Arc::new(Mutex::new(MouseShared {
            suppression_enabled: true,
            ..Default::default()
        }));
        let shared2 = Arc::clone(&shared);
        let thread = std::thread::Builder::new()
            .name("keymouse-500hz".into())
            .spawn(move || mouse_thread(shared2))
            .expect("failed to spawn mouse thread");

        let enigo_keys = Enigo::new(&Settings::default()).ok();
        let ok = enigo_keys.is_some();
        Self {
            suppression_enabled: true,
            muted: false,
            mouse_vel_x: 0.0,
            mouse_vel_y: 0.0,
            scroll_delta: 0,
            buttons: MouseButtons::default(),
            keys: KeysHeld::default(),
            learned_keys: HashMap::new(),
            enigo_keys,
            os_keys: KeysHeld::default(),
            os_learned_keys: HashMap::new(),
            mouse_shared: shared,
            _mouse_thread: thread,
            enigo_ok: ok,
        }
    }
}

impl Drop for VirtualKeyMouse {
    fn drop(&mut self) {
        if let Ok(mut s) = self.mouse_shared.lock() {
            s.stop = true;
        }
    }
}

impl VirtualDevice for VirtualKeyMouse {
    fn id(&self) -> &str { "virtual.keymouse" }
    fn display_name(&self) -> &str { "Virtual Keyboard & Mouse" }
    fn sink_pins(&self) -> &'static [SinkPin] { layouts::KEYMOUSE_DEFAULT_PINS }
    fn is_connected(&self) -> bool { self.enigo_ok }

    fn send(&mut self, pin: &str, value: Signal) {
        match pin {
            // Velocity pins — the mouse thread spreads these at 500 Hz.
            // Semantics: value = pixels per 60 Hz reference frame (unchanged from before).
            "mouse_x"       => { if let Signal::Float(f) = value { self.mouse_vel_x += f; } }
            "mouse_y"       => { if let Signal::Float(f) = value { self.mouse_vel_y += f; } }
            "scroll_up"     => { if matches!(value, Signal::Bool(true)) { self.scroll_delta += 1; } }
            "scroll_down"   => { if matches!(value, Signal::Bool(true)) { self.scroll_delta -= 1; } }
            "mouse_left"    => { if let Signal::Bool(b) = value { self.buttons.lmb = b; } }
            "mouse_right"   => { if let Signal::Bool(b) = value { self.buttons.rmb = b; } }
            "mouse_middle"  => { if let Signal::Bool(b) = value { self.buttons.mmb = b; } }
            "mouse_back"    => { if let Signal::Bool(b) = value { self.buttons.mb4 = b; } }
            "mouse_forward" => { if let Signal::Bool(b) = value { self.buttons.mb5 = b; } }
            "key_escape"    => { if let Signal::Bool(b) = value { self.keys.escape = b; } }
            "key_shift"     => { if let Signal::Bool(b) = value { self.keys.shift  = b; } }
            "key_ctrl"      => { if let Signal::Bool(b) = value { self.keys.ctrl   = b; } }
            "key_alt"       => { if let Signal::Bool(b) = value { self.keys.alt    = b; } }
            "key_win"       => { if let Signal::Bool(b) = value { self.keys.win    = b; } }
            _ => { if let Signal::Bool(b) = value { self.learned_keys.insert(pin.to_string(), b); } }
        }
    }

    fn flush(&mut self) {
        // Push latest desired state to the mouse thread.
        if let Ok(mut s) = self.mouse_shared.lock() {
            s.vel_x               = std::mem::take(&mut self.mouse_vel_x);
            s.vel_y               = std::mem::take(&mut self.mouse_vel_y);
            s.scroll_pending     += std::mem::take(&mut self.scroll_delta);
            s.buttons             = self.buttons;
            s.muted               = self.muted;
            s.suppression_enabled = self.suppression_enabled;
        }

        // ── Keyboard on the UI thread (60 Hz is plenty for keys) ─────────────
        let Some(enigo) = &mut self.enigo_keys else { return; };

        macro_rules! key_sync {
            ($os:expr, $want:expr, $key:expr) => {
                if $want != $os {
                    let _ = enigo.key($key, if $want { Direction::Press } else { Direction::Release });
                    $os = $want;
                }
            };
        }
        macro_rules! key_release {
            ($os:expr, $key:expr) => {
                if $os { let _ = enigo.key($key, Direction::Release); $os = false; }
            };
        }

        if !self.muted {
            key_sync!(self.os_keys.escape, self.keys.escape, Key::Escape);
            key_sync!(self.os_keys.shift,  self.keys.shift,  Key::Shift);
            key_sync!(self.os_keys.ctrl,   self.keys.ctrl,   Key::Control);
            key_sync!(self.os_keys.alt,    self.keys.alt,    Key::Alt);
            key_sync!(self.os_keys.win,    self.keys.win,    Key::Meta);
        } else {
            key_release!(self.os_keys.escape, Key::Escape);
            key_release!(self.os_keys.shift,  Key::Shift);
            key_release!(self.os_keys.ctrl,   Key::Control);
            key_release!(self.os_keys.alt,    Key::Alt);
            key_release!(self.os_keys.win,    Key::Meta);
        }

        let learned: Vec<(String, bool)> = self.learned_keys.iter()
            .map(|(k, &v)| (k.clone(), v))
            .collect();
        for (pin_name, want) in learned {
            let Some(key) = egui_key_name_to_enigo(&pin_name) else { continue; };
            let os = *self.os_learned_keys.get(&pin_name).unwrap_or(&false);
            if !self.muted {
                if want != os {
                    let _ = enigo.key(key, if want { Direction::Press } else { Direction::Release });
                    self.os_learned_keys.insert(pin_name, want);
                }
            } else if os {
                let _ = enigo.key(key, Direction::Release);
                self.os_learned_keys.insert(pin_name, false);
            }
        }
    }
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

/// Maps an egui Key debug name (format!("{key:?}")) to an enigo Key.
fn egui_key_name_to_enigo(name: &str) -> Option<Key> {
    Some(match name {
        "Enter"      => Key::Return,
        "Space"      => Key::Space,
        "Tab"        => Key::Tab,
        "Backspace"  => Key::Backspace,
        "Delete"     => Key::Delete,
        "Home"       => Key::Home,
        "End"        => Key::End,
        "PageUp"     => Key::PageUp,
        "PageDown"   => Key::PageDown,
        "ArrowUp"    => Key::UpArrow,
        "ArrowDown"  => Key::DownArrow,
        "ArrowLeft"  => Key::LeftArrow,
        "ArrowRight" => Key::RightArrow,
        "CapsLock"   => Key::CapsLock,
        "F1"  => Key::F1,  "F2"  => Key::F2,  "F3"  => Key::F3,  "F4"  => Key::F4,
        "F5"  => Key::F5,  "F6"  => Key::F6,  "F7"  => Key::F7,  "F8"  => Key::F8,
        "F9"  => Key::F9,  "F10" => Key::F10, "F11" => Key::F11, "F12" => Key::F12,
        "F13" => Key::F13, "F14" => Key::F14, "F15" => Key::F15, "F16" => Key::F16,
        "F17" => Key::F17, "F18" => Key::F18, "F19" => Key::F19, "F20" => Key::F20,
        // Single uppercase letter → VK code (VK_A=0x41 … VK_Z=0x5A).
        // Key::Other sends the actual virtual-key scancode, which works in games
        // that use WM_KEYDOWN; Key::Unicode would only work in text-input fields.
        n if n.len() == 1 => {
            let c = n.chars().next()?;
            if c.is_ascii_uppercase() { Key::Other(c as u32) } else { return None; }
        }
        // Num0–Num9 → VK_0=0x30 … VK_9=0x39
        n if n.starts_with("Num") && n.len() == 4 => {
            let c = n.chars().nth(3)?;
            if c.is_ascii_digit() { Key::Other(c as u32) } else { return None; }
        }
        _ => return None,
    })
}
