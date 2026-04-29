use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use enigo::{Axis, Button, Coordinate, Direction, Enigo, Key, Keyboard, Mouse, Settings};
use std::os::windows::io::AsRawHandle;
use vigem_client::{Client, XButtons, XGamepad, Xbox360Wired};

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

    fn reset_outputs(&mut self) {
        self.buttons = 0;
        self.left_trigger = 0;
        self.right_trigger = 0;
        self.thumb_lx = 0;
        self.thumb_ly = 0;
        self.thumb_rx = 0;
        self.thumb_ry = 0;
        self.flush();
    }
}

// ── DS4 (custom implementation with gyro support via DS4ReportEx IOCTL) ────────

/// Win32 types and functions for synchronous overlapped IOCTL calls.
mod vigem_ioctl {
    use std::ffi::c_void;

    // CTL_CODE(0x2A, fn, METHOD_BUFFERED=0, FILE_WRITE_DATA=2)
    // = (0x2A << 16) | (2 << 14) | (fn << 2) | 0
    pub const IOCTL_PLUGIN_TARGET: u32        = 0x2AA004; // fn=0x801
    pub const IOCTL_UNPLUG_TARGET: u32        = 0x2AA008; // fn=0x802
    pub const IOCTL_WAIT_DEVICE_READY: u32    = 0x2AA010; // fn=0x804
    pub const IOCTL_DS4_SUBMIT_REPORT: u32    = 0x2AA80C; // fn=0xA03
    // Extended report (ViGEmBus ≥ 1.17) — fn=0xA08
    pub const IOCTL_DS4_SUBMIT_REPORT_EX: u32 = 0x2AA820;

    pub const DS4_TARGET_KIND: i32 = 2;
    pub const DS4_VID: u16         = 0x054C;
    pub const DS4_PID: u16         = 0x05C4;

    /// Minimal OVERLAPPED for synchronous overlapped I/O (Offset fields zeroed).
    #[repr(C)]
    pub struct Overlapped {
        pub internal:      usize,
        pub internal_high: usize,
        pub offset:        u32,
        pub offset_high:   u32,
        pub event:         *mut c_void,
    }

    // ── ViGEm lifecycle structures ─────────────────────────────────────────────

    #[repr(C)]
    pub struct PluginTarget {
        pub size:   u32,
        pub serial: u32,
        pub kind:   i32,
        pub vid:    u16,
        pub pid:    u16,
    }

    /// Shared layout for Unplug and WaitDeviceReady (just size + serial).
    #[repr(C)]
    pub struct LifecycleTarget {
        pub size:   u32,
        pub serial: u32,
    }

    // ── DS4 basic report (same layout as vigem_client's DS4Report) ─────────────

    #[repr(C)]
    pub struct DS4Report {
        pub lx: u8, pub ly: u8, pub rx: u8, pub ry: u8,
        pub buttons: u16,
        pub special: u8,
        pub lt: u8, pub rt: u8,
    }

    #[repr(C)]
    pub struct DS4Submit {
        pub size:   u32,
        pub serial: u32,
        pub report: DS4Report,
    }

    // ── DS4 extended report (with gyro/accel) ─────────────────────────────────
    // Layout matches the C DS4_TOUCH / DS4_REPORT_EX structs in ViGEmBus,
    // using natural #[repr(C)] alignment (no packing pragma).

    #[repr(C)]
    pub struct DS4Touch {
        pub counter: u8,
        /// bit 7 = finger-up (inactive), bits 6:0 = tracking ID
        pub track1:  u8,
        pub pos1:    [u8; 3],
        pub track2:  u8,
        pub pos2:    [u8; 3],
    }
    impl Default for DS4Touch {
        fn default() -> Self {
            Self { counter: 0, track1: 0x80, pos1: [0; 3], track2: 0x80, pos2: [0; 3] }
        }
    }

    #[repr(C)]
    pub struct DS4ReportEx {
        pub lx: u8, pub ly: u8, pub rx: u8, pub ry: u8,
        pub buttons:  u16,
        pub special:  u8,
        pub lt: u8, pub rt: u8,
        // Natural alignment inserts 1 pad byte before timestamp (u16).
        pub timestamp:       u16,
        pub battery:         u8,
        // Natural alignment inserts 1 pad byte before gyro_x (i16).
        pub gyro_x:  i16, pub gyro_y:  i16, pub gyro_z:  i16,
        pub accel_x: i16, pub accel_y: i16, pub accel_z: i16,
        pub _unk1:           [u8; 5],
        pub battery_special: u8,
        pub _unk2:           [u8; 2],
        pub touch_n:         u8,
        pub touch_cur:       DS4Touch,
        pub touch_prev:      [DS4Touch; 2],
    }

    #[repr(C)]
    pub struct DS4ExSubmit {
        pub size:   u32,
        pub serial: u32,
        pub report: DS4ReportEx,
    }

    // ── Win32 kernel32 imports ─────────────────────────────────────────────────

    #[link(name = "kernel32")]
    extern "system" {
        pub fn CreateEventW(
            attrs:         *mut c_void,
            manual_reset:  i32,
            initial_state: i32,
            name:          *const u16,
        ) -> *mut c_void;

        pub fn CloseHandle(handle: *mut c_void) -> i32;

        pub fn DeviceIoControl(
            device:      *mut c_void,
            code:        u32,
            in_buf:      *const c_void,
            in_size:     u32,
            out_buf:     *mut c_void,
            out_size:    u32,
            returned:    *mut u32,
            overlapped:  *mut Overlapped,
        ) -> i32;

        pub fn GetOverlappedResult(
            file:       *mut c_void,
            overlapped: *mut Overlapped,
            transferred: *mut u32,
            wait:        i32,
        ) -> i32;
    }

    /// Submit a synchronous IOCTL on an overlapped device handle.
    /// Returns true on success.
    pub unsafe fn ioctl(
        device: *mut c_void,
        event:  *mut c_void,
        code:   u32,
        buf:    *const c_void,
        size:   u32,
    ) -> bool {
        let mut ovl = Overlapped {
            internal: 0, internal_high: 0, offset: 0, offset_high: 0, event,
        };
        let mut n = 0u32;
        DeviceIoControl(device, code, buf, size, std::ptr::null_mut(), 0, &mut n, &mut ovl);
        GetOverlappedResult(device, &mut ovl, &mut n, 1 /* bWait=TRUE */) != 0
    }
}

// ── DS4 button bit masks ───────────────────────────────────────────────────────
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

pub struct VirtualDS4 {
    id:           String,
    display_name: String,
    /// Keeps the ViGEm bus connection alive; raw handle borrowed via `dev`.
    _client:      Option<Client>,
    /// Raw handle from `_client`; null when ViGEmBus is unavailable.
    dev:          *mut std::ffi::c_void,
    serial:       u32,
    /// Windows event for overlapped I/O; null when not connected.
    event:        *mut std::ffi::c_void,
    /// Whether `IOCTL_DS4_SUBMIT_REPORT_EX` is supported by installed ViGEmBus.
    has_extended: bool,
    // Report state
    lx: u8, ly: u8, rx: u8, ry: u8,
    lt: u8, rt: u8,
    buttons:  u16,
    special:  u8,
    dpad:     [bool; 4],
    // Gyro / accelerometer (float in [-1, 1], converted to i16 in flush)
    gyro_x:  f32, gyro_y:  f32, gyro_z:  f32,
    accel_x: f32, accel_y: f32, accel_z: f32,
}

// Raw pointer fields are not Send by default; declare it safe because the
// handles are owned exclusively by this struct and accessed on one thread.
unsafe impl Send for VirtualDS4 {}

impl VirtualDS4 {
    pub fn new(instance: usize) -> Self {
        let (id, display_name) = instance_label("virtual.ds4", "Virtual DualShock 4", instance);
        let mut out = VirtualDS4 {
            id, display_name,
            _client: None, dev: std::ptr::null_mut(),
            serial: 0, event: std::ptr::null_mut(), has_extended: false,
            lx: 0x80, ly: 0x80, rx: 0x80, ry: 0x80,
            lt: 0, rt: 0, buttons: 0, special: 0, dpad: [false; 4],
            gyro_x: 0.0, gyro_y: 0.0, gyro_z: 0.0,
            accel_x: 0.0, accel_y: 0.0, accel_z: 0.0,
        };

        let client = match Client::connect() {
            Ok(c) => c,
            Err(_) => return out,
        };
        let dev = client.as_raw_handle() as *mut std::ffi::c_void;
        let event = unsafe {
            vigem_ioctl::CreateEventW(std::ptr::null_mut(), 0, 0, std::ptr::null())
        };
        if event.is_null() { return out; }

        // Find a free serial number and plug in the DS4 target.
        let mut serial = 1u32;
        loop {
            let plug = vigem_ioctl::PluginTarget {
                size:   std::mem::size_of::<vigem_ioctl::PluginTarget>() as u32,
                serial,
                kind:   vigem_ioctl::DS4_TARGET_KIND,
                vid:    vigem_ioctl::DS4_VID,
                pid:    vigem_ioctl::DS4_PID,
            };
            if unsafe { vigem_ioctl::ioctl(
                dev, event, vigem_ioctl::IOCTL_PLUGIN_TARGET,
                &plug as *const _ as _, std::mem::size_of_val(&plug) as u32,
            ) } { break; }
            serial += 1;
            if serial > 65535 {
                unsafe { vigem_ioctl::CloseHandle(event); }
                return out;
            }
        }

        // Wait until the virtual controller is enumerated and ready.
        let wait = vigem_ioctl::LifecycleTarget {
            size: std::mem::size_of::<vigem_ioctl::LifecycleTarget>() as u32,
            serial,
        };
        unsafe { vigem_ioctl::ioctl(
            dev, event, vigem_ioctl::IOCTL_WAIT_DEVICE_READY,
            &wait as *const _ as _, std::mem::size_of_val(&wait) as u32,
        ); }

        // Assume extended IOCTL is available; flush() will fall back to basic if not.
        // Starting optimistic avoids a probe-timing race where the device isn't ready yet.
        let has_extended = true;

        #[cfg(debug_assertions)]
        eprintln!("[VirtualDS4] plugged in serial={serial}, will try extended IOCTL");

        out._client      = Some(client);
        out.dev          = dev;
        out.serial       = serial;
        out.event        = event;
        out.has_extended = has_extended;
        out
    }
}

impl Drop for VirtualDS4 {
    fn drop(&mut self) {
        if self.dev.is_null() { return; }
        let unplug = vigem_ioctl::LifecycleTarget {
            size:   std::mem::size_of::<vigem_ioctl::LifecycleTarget>() as u32,
            serial: self.serial,
        };
        unsafe {
            vigem_ioctl::ioctl(
                self.dev, self.event, vigem_ioctl::IOCTL_UNPLUG_TARGET,
                &unplug as *const _ as _, std::mem::size_of_val(&unplug) as u32,
            );
            vigem_ioctl::CloseHandle(self.event);
        }
    }
}

impl VirtualDevice for VirtualDS4 {
    fn id(&self)           -> &str { &self.id }
    fn display_name(&self) -> &str { &self.display_name }
    fn sink_pins(&self)    -> &'static [SinkPin] { layouts::DS4_SINK_PINS }
    fn is_connected(&self) -> bool { !self.dev.is_null() }

    fn send(&mut self, pin: &str, value: Signal) {
        match pin {
            "left_stick" => if let Signal::Vec2(v) = value {
                self.lx = ds4_axis_x(v.x); self.ly = ds4_axis_y(v.y);
            },
            "right_stick" => if let Signal::Vec2(v) = value {
                self.rx = ds4_axis_x(v.x); self.ry = ds4_axis_y(v.y);
            },
            "dpad" => if let Signal::Vec2(v) = value {
                self.dpad[0] = v.y >  0.5; self.dpad[1] = v.x >  0.5;
                self.dpad[2] = v.y < -0.5; self.dpad[3] = v.x < -0.5;
            },
            "left_stick_x"  => if let Signal::Float(f) = value { self.lx = ds4_axis_x(f); },
            "left_stick_y"  => if let Signal::Float(f) = value { self.ly = ds4_axis_y(f); },
            "right_stick_x" => if let Signal::Float(f) = value { self.rx = ds4_axis_x(f); },
            "right_stick_y" => if let Signal::Float(f) = value { self.ry = ds4_axis_y(f); },
            "l2"            => if let Signal::Float(f) = value { self.lt = float_to_u8(f); },
            "r2"            => if let Signal::Float(f) = value { self.rt = float_to_u8(f); },
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
            "gyro_x"  => if let Signal::Float(f) = value { self.gyro_x  = f; },
            "gyro_y"  => if let Signal::Float(f) = value { self.gyro_y  = f; },
            "gyro_z"  => if let Signal::Float(f) = value { self.gyro_z  = f; },
            "accel_x" => if let Signal::Float(f) = value { self.accel_x = f; },
            "accel_y" => if let Signal::Float(f) = value { self.accel_y = f; },
            "accel_z" => if let Signal::Float(f) = value { self.accel_z = f; },
            _ => {}
        }
    }

    fn flush(&mut self) {
        if self.dev.is_null() { return; }
        let dpad    = encode_dpad(self.dpad[0], self.dpad[1], self.dpad[2], self.dpad[3]);
        let buttons = (self.buttons & !0xF) | dpad;
        let tp_pressed = self.special & ds4_btn::TOUCHPAD != 0;

        if self.has_extended {
            let ok = unsafe {
                let mut sub: vigem_ioctl::DS4ExSubmit = std::mem::zeroed();
                sub.size   = std::mem::size_of::<vigem_ioctl::DS4ExSubmit>() as u32;
                sub.serial = self.serial;
                sub.report.lx      = self.lx;  sub.report.ly = self.ly;
                sub.report.rx      = self.rx;  sub.report.ry = self.ry;
                sub.report.buttons = buttons;
                sub.report.special = self.special;
                sub.report.lt      = self.lt;  sub.report.rt = self.rt;
                sub.report.battery = 0xFF;
                sub.report.gyro_x  = float_to_i16(self.gyro_x);
                sub.report.gyro_y  = float_to_i16(self.gyro_y);
                sub.report.gyro_z  = float_to_i16(self.gyro_z);
                sub.report.accel_x = float_to_i16(self.accel_x);
                sub.report.accel_y = float_to_i16(self.accel_y);
                sub.report.accel_z = float_to_i16(self.accel_z);
                // When the touchpad button is pressed, report a single center touch.
                // Some software requires at least one active touch point alongside
                // the touchpad-click bit in the special byte.
                if tp_pressed {
                    sub.report.touch_n = 1;
                    sub.report.touch_cur = vigem_ioctl::DS4Touch {
                        counter: 0,
                        track1: 0x00,             // active, tracking ID 0
                        pos1: [0xC0, 0x63, 0x1D], // X=960, Y=470 (DS4 touchpad centre)
                        track2: 0x80,             // second slot inactive
                        pos2: [0; 3],
                    };
                } else {
                    sub.report.touch_cur     = vigem_ioctl::DS4Touch::default();
                }
                sub.report.touch_prev[0] = vigem_ioctl::DS4Touch::default();
                sub.report.touch_prev[1] = vigem_ioctl::DS4Touch::default();
                vigem_ioctl::ioctl(
                    self.dev, self.event, vigem_ioctl::IOCTL_DS4_SUBMIT_REPORT_EX,
                    &sub as *const _ as _, std::mem::size_of_val(&sub) as u32,
                )
            };
            if ok { return; }
            // Extended IOCTL failed — fall through to basic this frame but keep
            // has_extended = true so we retry next frame. Transient failures
            // (device not yet ready) would otherwise permanently kill gyro/touchpad.
            #[cfg(debug_assertions)]
            eprintln!("[VirtualDS4] extended IOCTL failed, falling back to basic this frame");
        }

        // Basic report path (also fallback when extended fails).
        let sub = vigem_ioctl::DS4Submit {
            size:   std::mem::size_of::<vigem_ioctl::DS4Submit>() as u32,
            serial: self.serial,
            report: vigem_ioctl::DS4Report {
                lx: self.lx, ly: self.ly, rx: self.rx, ry: self.ry,
                buttons, special: self.special, lt: self.lt, rt: self.rt,
            },
        };
        unsafe {
            vigem_ioctl::ioctl(
                self.dev, self.event, vigem_ioctl::IOCTL_DS4_SUBMIT_REPORT,
                &sub as *const _ as _, std::mem::size_of_val(&sub) as u32,
            );
        }
    }

    fn reset_outputs(&mut self) {
        let center = ds4_axis_x(0.0); // 127 — neutral stick position
        self.lx = center; self.ly = center;
        self.rx = center; self.ry = center;
        self.lt = 0; self.rt = 0;
        self.buttons = 0;
        self.special = 0;
        self.dpad = [false; 4];
        self.gyro_x = 0.0; self.gyro_y = 0.0; self.gyro_z = 0.0;
        self.accel_x = 0.0; self.accel_y = 0.0; self.accel_z = 0.0;
        self.flush();
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

    fn reset_outputs(&mut self) {
        self.mouse_vel_x = 0.0;
        self.mouse_vel_y = 0.0;
        self.scroll_delta = 0;
        self.buttons = MouseButtons::default();
        self.keys = KeysHeld::default();
        for v in self.learned_keys.values_mut() { *v = false; }
        // Flush with muted=true so the mouse thread gets zero velocity and all
        // keys/buttons are released via the existing muted-release path.
        let prev_muted = self.muted;
        self.muted = true;
        self.flush();
        self.muted = prev_muted;
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
