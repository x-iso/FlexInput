use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use enigo::{Axis, Button, Coordinate, Direction, Enigo, Key, Keyboard, Mouse, Settings};
use std::os::windows::io::AsRawHandle;
use vigem_client::{Client, XButtons, XGamepad, Xbox360Wired};

use flexinput_core::Signal;
use crate::{layouts, DeviceKind, SinkPin, SourcePin, VirtualDevice};

// All virtual gamepads are HIDMaestro-backed (user-mode driver, no ViGEmBus).
// The legacy ViGEm kinds `virtual.xinput` / `virtual.ds4` are no longer offered
// here — patches that referenced them are migrated to the `virtual.hm.*`
// equivalents on load (see canvas::migrate_vigem_device_id). The ViGEm device
// implementations remain compiled (dormant) as a fallback; `create_device` still
// builds them if an un-migrated id ever reaches it.
pub static DEVICE_KINDS: &[DeviceKind] = &[
    DeviceKind { kind_id: "virtual.hm.xinput",    display_name: "Virtual Xbox 360",          allows_multiple: true },
    DeviceKind { kind_id: "virtual.hm.ds4",       display_name: "Virtual DualShock 4",       allows_multiple: true },
    DeviceKind { kind_id: "virtual.hm.dualsense", display_name: "Virtual DualSense",         allows_multiple: true },
    DeviceKind { kind_id: "virtual.keymouse",     display_name: "Virtual Keyboard & Mouse",  allows_multiple: false },
];

/// Profile JSON for each HIDMaestro kind (vendored presets in flexinput-hidmaestro).
fn hm_profile_json(kind_id: &str) -> Option<&'static str> {
    use flexinput_hidmaestro::profile::presets;
    match kind_id {
        "virtual.hm.ds4"       => Some(presets::DUALSHOCK_4_V2_JSON),
        "virtual.hm.dualsense" => Some(presets::DUALSENSE_JSON),
        "virtual.hm.xinput"    => Some(presets::XBOX360_JSON),
        _ => None,
    }
}

/// Friendly display name for a HIDMaestro kind id.
fn hm_display_name(kind_id: &str) -> &'static str {
    match kind_id {
        "virtual.hm.ds4" => "Virtual DualShock 4",
        "virtual.hm.dualsense" => "Virtual DualSense",
        "virtual.hm.xinput" => "Virtual Xbox 360",
        _ => "Virtual Controller",
    }
}

/// Static pin metadata for a virtual device kind — the sink/source pin layouts
/// and display name — WITHOUT constructing the device or touching any OS
/// resource (no helper IPC, no ViGEm). Lets the UI add a canvas sink node
/// instantly; the real device is built asynchronously by the device-ops worker
/// and installed into the pool when ready. `instance` only affects the display
/// name suffix. Returns `None` for an unknown kind.
pub fn kind_pin_metadata(
    kind_id: &str,
    instance: usize,
) -> Option<(&'static [SinkPin], &'static [SourcePin], String)> {
    let (sink, source, base_name): (&'static [SinkPin], &'static [SourcePin], &str) = match kind_id {
        "virtual.xinput"   => (layouts::XINPUT_SINK_PINS,    layouts::XINPUT_SOURCE_PINS, "Virtual XInput Controller"),
        "virtual.ds4"      => (layouts::DS4_SINK_PINS,       layouts::DS4_SOURCE_PINS,    "Virtual DualShock 4"),
        "virtual.keymouse" => (layouts::KEYMOUSE_DEFAULT_PINS, &[],                       "Virtual Keyboard & Mouse"),
        "virtual.hm.ds4"       => (layouts::DS4_SINK_PINS,       layouts::DS4_SOURCE_PINS, "Virtual DualShock 4"),
        "virtual.hm.dualsense" => (layouts::DUALSENSE_SINK_PINS, layouts::DS4_SOURCE_PINS, "Virtual DualSense"),
        "virtual.hm.xinput"    => (layouts::XINPUT_SINK_PINS,    layouts::XINPUT_SOURCE_PINS, "Virtual Xbox 360"),
        _ => return None,
    };
    let name = if instance == 0 {
        base_name.to_string()
    } else {
        format!("{base_name} #{}", instance + 1)
    };
    Some((sink, source, name))
}

pub fn create_device(kind_id: &str, instance: usize) -> Box<dyn VirtualDevice> {
    if let Some(profile_json) = hm_profile_json(kind_id) {
        // HIDMaestro device: the helper creates the node + sections (one UAC on
        // first use). `instance` is BOTH the index hint and the per-instance
        // disambiguator. Pass the INSTANCE-QUALIFIED id (e.g.
        // `virtual.hm.xinput.1`) — NOT the bare kind_id — as the device_id, so
        // a second pad of the same kind gets its own helper record. The helper
        // reclaims by device_id; passing the bare kind_id made the 2nd x360
        // reclaim the 1st's node (no new pad, and both fought over one index).
        let (device_id, display_name) =
            instance_label(kind_id, hm_display_name(kind_id), instance);
        let dev = crate::hidmaestro_device::HidMaestroDevice::create(
            device_id,
            display_name,
            profile_json,
            instance as u32,
        );
        // open()/create() only return None on an invalid bundled preset (a build
        // bug) — fall back to a disconnected device so the UI never panics.
        if let Some(d) = dev {
            return Box::new(d);
        }
    }
    match kind_id {
        "virtual.xinput"   => Box::new(VirtualXInput::new(instance)),
        "virtual.ds4"      => Box::new(VirtualDS4::new(instance)),
        "virtual.keymouse" => {
            // Prefer the HIDMaestro-backed implementation when the driver is
            // installed: output lands as real HID reports (kernel-side), immune
            // to the SendInput-eating hook stacks that leave the enigo backend
            // silently dead (see keymouse_hm module docs). Only attempted when
            // the driver is already present so adding a keyboard/mouse device
            // never surprise-elevates a user who has never used HIDMaestro.
            if flexinput_hidmaestro::hidmaestro_available() {
                if let Some(hm) = crate::keymouse_hm::VirtualKeyMouseHm::create() {
                    return Box::new(hm);
                }
                eprintln!("[keymouse] HIDMaestro nodes unavailable — falling back to SendInput backend");
            }
            Box::new(VirtualKeyMouse::new())
        }
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

/// Rumble state written by the ViGEm notification thread, read by poll_outputs().
#[derive(Default, Clone, Copy)]
struct XRumble { large: u8, small: u8 }

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
    /// Latest rumble values received from the game via ViGEm notifications.
    rumble: Arc<Mutex<XRumble>>,
    /// Join handle kept alive so the thread lives as long as this struct.
    _notif_thread: Option<std::thread::JoinHandle<()>>,
}

impl VirtualXInput {
    pub fn new(instance: usize) -> Self {
        let (id, display_name) = instance_label("virtual.xinput", "Virtual XInput Controller", instance);
        let rumble = Arc::new(Mutex::new(XRumble::default()));
        let mut notif_thread = None;

        let target = Client::connect().ok().and_then(|client| {
            let mut t = Xbox360Wired::new(client, vigem_client::TargetId::XBOX360_WIRED);
            t.plugin().ok()?;
            t.wait_ready().ok()?;

            // request_notification() must be called before t is moved.
            // spawn_thread() takes ownership of the request and loops internally,
            // calling the callback each time the game sends XInputSetState.
            let rumble2 = Arc::clone(&rumble);
            if let Ok(req) = t.request_notification() {
                notif_thread = Some(req.spawn_thread(move |_, data| {
                    if let Ok(mut r) = rumble2.lock() {
                        r.large = data.large_motor;
                        r.small = data.small_motor;
                    }
                }));
            }

            Some(t)
        });

        Self {
            id, display_name,
            target,
            thumb_lx: 0, thumb_ly: 0,
            thumb_rx: 0, thumb_ry: 0,
            left_trigger: 0, right_trigger: 0,
            buttons: 0,
            rumble,
            _notif_thread: notif_thread,
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
            "left_stick_x"  => self.thumb_lx = float_to_i16(value.as_float()),
            "left_stick_y"  => self.thumb_ly = float_to_i16(value.as_float()),
            "right_stick_x" => self.thumb_rx = float_to_i16(value.as_float()),
            "right_stick_y" => self.thumb_ry = float_to_i16(value.as_float()),
            "left_trigger"  => self.left_trigger  = float_to_u8(value.as_float()),
            "right_trigger" => self.right_trigger = float_to_u8(value.as_float()),
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

    fn source_pins(&self) -> &'static [SourcePin] { layouts::XINPUT_SOURCE_PINS }

    fn poll_outputs(&mut self) -> Vec<(&'static str, Signal)> {
        let r = self.rumble.lock().map(|r| *r).unwrap_or_default();
        vec![
            ("rumble_strong", Signal::Float(r.large as f32 / 255.0)),
            ("rumble_weak",   Signal::Float(r.small as f32 / 255.0)),
        ]
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
    pub const IOCTL_DS4_SUBMIT_REPORT: u32        = 0x002AA80C; // BUSENUM_W_IOCTL(0x801+0x202)
    // DS4 rumble/lightbar notification from game — BUSENUM_W_IOCTL(0x801+0x203)
    pub const IOCTL_DS4_REQUEST_NOTIFICATION: u32 = 0x002AA810;

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
    // ViGEmBus defines these structs with #pragma pack(1) (pshpack1.h).
    // DS4_SUBMIT_REPORT_EX = 72 bytes, DS4_REPORT_EX = 63 bytes, DS4_TOUCH = 9 bytes.
    // Must use #[repr(C, packed)] to match — natural alignment adds padding and the
    // driver rejects the IOCTL when Size != sizeof(DS4_SUBMIT_REPORT_EX).

    #[repr(C, packed)]
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

    // DS4_REPORT_EX in C is a union { struct { ... }; UCHAR ReportBuffer[63]; }.
    // The named fields sum to 60 bytes; the union size is 63 (the array arm wins).
    // We model this as a flat 63-byte array and project field accessors via methods.
    #[repr(C, packed)]
    pub struct DS4ReportEx {
        pub buf: [u8; 63],
    }

    // RESERVED: setters for the extended (gyro/accel/touchpad) DS4 report.
    // The `IOCTL_DS4_SUBMIT_REPORT_EX` path is scaffolded but never enabled —
    // `VirtualDS4::has_extended` is hardwired false pending bus-version
    // detection — so the basic `DS4Report` submit is the only live path today.
    #[allow(dead_code)]
    impl DS4ReportEx {
        pub fn set_lx(&mut self, v: u8) { self.buf[0] = v; }
        pub fn set_ly(&mut self, v: u8) { self.buf[1] = v; }
        pub fn set_rx(&mut self, v: u8) { self.buf[2] = v; }
        pub fn set_ry(&mut self, v: u8) { self.buf[3] = v; }
        pub fn set_buttons(&mut self, v: u16) {
            self.buf[4] = v as u8; self.buf[5] = (v >> 8) as u8;
        }
        pub fn set_special(&mut self, v: u8) { self.buf[6] = v; }
        pub fn set_lt(&mut self, v: u8) { self.buf[7] = v; }
        pub fn set_rt(&mut self, v: u8) { self.buf[8] = v; }
        // buf[9..10] = wTimestamp (leave 0)
        pub fn set_battery(&mut self, v: u8) { self.buf[11] = v; }
        pub fn set_gyro_x(&mut self, v: i16) {
            let b = v.to_le_bytes(); self.buf[12] = b[0]; self.buf[13] = b[1];
        }
        pub fn set_gyro_y(&mut self, v: i16) {
            let b = v.to_le_bytes(); self.buf[14] = b[0]; self.buf[15] = b[1];
        }
        pub fn set_gyro_z(&mut self, v: i16) {
            let b = v.to_le_bytes(); self.buf[16] = b[0]; self.buf[17] = b[1];
        }
        pub fn set_accel_x(&mut self, v: i16) {
            let b = v.to_le_bytes(); self.buf[18] = b[0]; self.buf[19] = b[1];
        }
        pub fn set_accel_y(&mut self, v: i16) {
            let b = v.to_le_bytes(); self.buf[20] = b[0]; self.buf[21] = b[1];
        }
        pub fn set_accel_z(&mut self, v: i16) {
            let b = v.to_le_bytes(); self.buf[22] = b[0]; self.buf[23] = b[1];
        }
        // buf[24..28] = _bUnknown1[5], buf[29] = bBatteryLvlSpecial, buf[30..31] = _bUnknown2
        pub fn set_touch_n(&mut self, v: u8) { self.buf[32] = v; }
        // DS4_TOUCH at offset 33 (current) and 42/51 (previous[0/1])
        // DS4_TOUCH layout: counter(1) track1(1) pos1[3] track2(1) pos2[3] = 9 bytes
        pub fn set_touch_cur(&mut self, counter: u8, track1: u8, pos1: [u8;3], track2: u8, pos2: [u8;3]) {
            let b = &mut self.buf[33..42];
            b[0]=counter; b[1]=track1; b[2]=pos1[0]; b[3]=pos1[1]; b[4]=pos1[2];
            b[5]=track2;  b[6]=pos2[0]; b[7]=pos2[1]; b[8]=pos2[2];
        }
        pub fn clear_touch_prev(&mut self) {
            // track bits 7 = finger-up (inactive)
            self.buf[42] = 0; self.buf[43] = 0x80; self.buf[44..47].fill(0);
            self.buf[47] = 0x80; self.buf[48..51].fill(0);
            self.buf[51] = 0; self.buf[52] = 0x80; self.buf[53..56].fill(0);
            self.buf[56] = 0x80; self.buf[57..60].fill(0);
        }
    }

    #[repr(C, packed)]
    pub struct DS4ExSubmit {
        pub size:   u32,
        pub serial: u32,
        pub report: DS4ReportEx,
    }

    const _: () = {
        assert!(std::mem::size_of::<DS4Touch>()    ==  9);
        assert!(std::mem::size_of::<DS4ReportEx>() == 63);
        assert!(std::mem::size_of::<DS4ExSubmit>() == 71);
    };

    // ── DS4 notification (rumble + lightbar from game) ────────────────────────
    // Real DS4_REQUEST_NOTIFICATION layout (16 bytes, observed from raw driver output):
    //   Size(4) + SerialNo(4) + SmallMotor(1) + LargeMotor(1) + R(1) + G(1) + B(1) + pad[3]
    // No padding between SerialNo and DS4_OUTPUT_REPORT — driver writes motors at offset 8.
    #[repr(C)]
    pub struct DS4NotificationBuffer {
        pub size:        u32,
        pub serial:      u32,
        pub small_motor: u8,
        pub large_motor: u8,
        pub lightbar_r:  u8,
        pub lightbar_g:  u8,
        pub lightbar_b:  u8,
        pub _tail:       [u8; 3], // pad to 16 bytes (matches driver-reported Size)
    }

    /// Submit a non-blocking overlapped IOCTL (does not wait for completion).
    /// The caller must keep `overlapped` pinned until polled.
    pub unsafe fn ioctl_async(
        device:     *mut c_void,
        overlapped: *mut Overlapped,
        code:       u32,
        buf:        *mut c_void,
        size:       u32,
    ) {
        let mut n = 0u32;
        DeviceIoControl(device, code, buf, size, buf, size, &mut n, overlapped);
    }

    /// Non-blocking poll of a pending overlapped operation.
    /// Returns Some(()) if completed, None if still pending, Err(code) on error/abort.
    ///
    /// RESERVED: the notification thread currently blocks on
    /// `GetOverlappedResult(bWait=TRUE)` with its own duplicated handle;
    /// this poll variant is kept for a possible non-blocking redesign.
    #[allow(dead_code)]
    pub unsafe fn poll_overlapped(
        device:     *mut c_void,
        overlapped: *mut Overlapped,
    ) -> Result<Option<()>, u32> {
        let mut n = 0u32;
        if GetOverlappedResult(device, overlapped, &mut n, 0 /* bWait=FALSE */) != 0 {
            Ok(Some(()))
        } else {
            let err = GetLastError();
            const ERROR_IO_INCOMPLETE: u32 = 996;
            if err == ERROR_IO_INCOMPLETE { Ok(None) } else { Err(err) }
        }
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

        pub fn GetLastError() -> u32;
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

/// Rumble + lightbar values received from a game via DS4 notification IOCTL.
#[derive(Default, Clone, Copy)]
struct DS4Rumble {
    large: u8, small: u8,
    r: u8, g: u8, b: u8,
}

/// Shared state between the DS4 notification thread and poll_outputs() reader.
struct DS4NotifShared {
    rumble: DS4Rumble,
    stop:   bool,
}

pub struct VirtualDS4 {
    id:           String,
    display_name: String,
    /// Keeps the ViGEm bus connection alive; raw handle borrowed via `dev`.
    _client:      Option<Client>,
    /// Raw handle from `_client`; null when ViGEmBus is unavailable.
    dev:          *mut std::ffi::c_void,
    serial:       u32,
    /// Windows event for overlapped I/O (submit); null when not connected.
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
    /// Rumble/lightbar feedback from game — updated by background notification thread.
    notif_shared: Arc<Mutex<DS4NotifShared>>,
    _notif_thread: Option<std::thread::JoinHandle<()>>,
}

// Raw pointer fields are not Send by default; declare it safe because the
// handles are owned exclusively by this struct and accessed on one thread.
unsafe impl Send for VirtualDS4 {}

impl VirtualDS4 {
    fn make_notif_buf(serial: u32) -> Box<vigem_ioctl::DS4NotificationBuffer> {
        Box::new(vigem_ioctl::DS4NotificationBuffer {
            size:        16,
            serial,
            small_motor: 0, large_motor: 0,
            lightbar_r:  0, lightbar_g:  0, lightbar_b: 0,
            _tail:       [0; 3],
        })
    }

    // RESERVED: overlapped-struct builder for the non-blocking notification
    // design (pairs with `vigem_ioctl::poll_overlapped`); the current
    // notification thread blocks with its own handle and doesn't need it.
    #[allow(dead_code)]
    fn make_notif_ovl() -> Box<vigem_ioctl::Overlapped> {
        Box::new(vigem_ioctl::Overlapped {
            internal: 0, internal_high: 0, offset: 0, offset_high: 0,
            event: unsafe { vigem_ioctl::CreateEventW(std::ptr::null_mut(), 0, 0, std::ptr::null()) },
        })
    }

    pub fn new(instance: usize) -> Self {
        let (id, display_name) = instance_label("virtual.ds4", "Virtual DualShock 4", instance);
        let notif_shared = Arc::new(Mutex::new(DS4NotifShared {
            rumble: DS4Rumble::default(),
            stop:   false,
        }));
        let mut out = VirtualDS4 {
            id, display_name,
            _client: None, dev: std::ptr::null_mut(),
            serial: 0, event: std::ptr::null_mut(), has_extended: false,
            lx: 0x80, ly: 0x80, rx: 0x80, ry: 0x80,
            lt: 0, rt: 0, buttons: 0, special: 0, dpad: [false; 4],
            gyro_x: 0.0, gyro_y: 0.0, gyro_z: 0.0,
            accel_x: 0.0, accel_y: 0.0, accel_z: 0.0,
            notif_shared,
            _notif_thread: None,
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

        out._client      = Some(client);
        out.dev          = dev;
        out.serial       = serial;
        out.event        = event;
        out.has_extended = false;

        eprintln!("[VirtualDS4] plugged in serial={serial}");

        // Spawn a notification thread with its OWN duplicated ViGEm handle.
        // Sharing the main handle blocks all subsequent submit IOCTLs behind the
        // long-pending notification IOCTL, throttling DS4 polling to a crawl and
        // also stalling physical-device HID writes on the same I/O thread.
        if let Some(client_ref) = out._client.as_ref() {
            if let Ok(notif_client) = client_ref.try_clone() {
                let notif_dev = notif_client.as_raw_handle() as *mut std::ffi::c_void;
                let shared = Arc::clone(&out.notif_shared);
                let dev_ptr = SendPtr(notif_dev);
                let handle = std::thread::Builder::new()
                    .name(format!("ds4-notif-{instance}"))
                    .spawn(move || {
                        // Move the duplicated client into the thread so its handle
                        // stays open for the lifetime of the notification loop.
                        let _hold = notif_client;
                        ds4_notif_thread(dev_ptr, serial, shared);
                    })
                    .ok();
                out._notif_thread = handle;
            } else {
                eprintln!("[VirtualDS4] failed to duplicate ViGEm handle — feedback disabled");
            }
        }

        out
    }

}

/// Raw-pointer wrapper marked Send so it can cross to the notification thread.
/// SAFETY: the ViGEm device handle is shared between the main I/O thread (issuing
/// DS4_SUBMIT_REPORT) and the notification thread (waiting on DS4_REQUEST_NOTIFICATION).
/// Windows kernel I/O on a HANDLE is thread-safe for distinct overlapped operations.
struct SendPtr(*mut std::ffi::c_void);
unsafe impl Send for SendPtr {}

/// Dedicated thread that blocks on DS4 notification IOCTLs. Sleeps in the kernel
/// until the game sends rumble/lightbar, then updates `shared.rumble` and loops.
fn ds4_notif_thread(dev_ptr: SendPtr, serial: u32, shared: Arc<Mutex<DS4NotifShared>>) {
    let dev = dev_ptr.0;
    let mut buf = VirtualDS4::make_notif_buf(serial);
    let event = unsafe {
        vigem_ioctl::CreateEventW(std::ptr::null_mut(), 0, 0, std::ptr::null())
    };
    if event.is_null() { return; }
    let mut ovl = vigem_ioctl::Overlapped {
        internal: 0, internal_high: 0, offset: 0, offset_high: 0, event,
    };
    loop {
        // Check stop flag before each blocking call.
        if shared.lock().map(|s| s.stop).unwrap_or(true) { break; }

        // Reset overlapped state and reissue the request.
        ovl.internal = 0;
        ovl.internal_high = 0;
        unsafe {
            vigem_ioctl::ioctl_async(
                dev, &mut ovl as *mut _,
                vigem_ioctl::IOCTL_DS4_REQUEST_NOTIFICATION,
                buf.as_mut() as *mut _ as _,
                16,
            );
        }
        // Block until completion (bWait=TRUE) — kernel parks this thread, no CPU spin.
        let mut n = 0u32;
        let ok = unsafe {
            vigem_ioctl::GetOverlappedResult(dev, &mut ovl as *mut _, &mut n, 1)
        };
        if ok == 0 {
            let err = unsafe { vigem_ioctl::GetLastError() };
            // ERROR_OPERATION_ABORTED (995) fires when the target is unplugged.
            if err != 995 {
                eprintln!("[VirtualDS4 notif-thread] error winerr={err:#010x}, exiting");
            }
            break;
        }

        let new_rumble = DS4Rumble {
            large: buf.large_motor,
            small: buf.small_motor,
            r:     buf.lightbar_r,
            g:     buf.lightbar_g,
            b:     buf.lightbar_b,
        };
        if let Ok(mut s) = shared.lock() {
            if s.stop { break; }
            let changed = (new_rumble.large, new_rumble.small, new_rumble.r, new_rumble.g, new_rumble.b)
                != (s.rumble.large, s.rumble.small, s.rumble.r, s.rumble.g, s.rumble.b);
            if changed {
                eprintln!("[VirtualDS4] notification: large={} small={} rgb=({},{},{})",
                    new_rumble.large, new_rumble.small, new_rumble.r, new_rumble.g, new_rumble.b);
            }
            s.rumble = new_rumble;
        }
    }
    unsafe { vigem_ioctl::CloseHandle(event); }
}

impl Drop for VirtualDS4 {
    fn drop(&mut self) {
        // Signal the notification thread to exit on its next wake-up.
        if let Ok(mut s) = self.notif_shared.lock() { s.stop = true; }
        if self.dev.is_null() { return; }
        let unplug = vigem_ioctl::LifecycleTarget {
            size:   std::mem::size_of::<vigem_ioctl::LifecycleTarget>() as u32,
            serial: self.serial,
        };
        unsafe {
            // Unplugging aborts any pending notification IOCTL with ERROR_OPERATION_ABORTED,
            // unblocking the notification thread so it can observe `stop` and exit.
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
            "left_stick_x"  => self.lx = ds4_axis_x(value.as_float()),
            "left_stick_y"  => self.ly = ds4_axis_y(value.as_float()),
            "right_stick_x" => self.rx = ds4_axis_x(value.as_float()),
            "right_stick_y" => self.ry = ds4_axis_y(value.as_float()),
            "left_trigger"  => self.lt = float_to_u8(value.as_float()),
            "right_trigger" => self.rt = float_to_u8(value.as_float()),
            "btn_south"    => set_bool_bit(&mut self.buttons, ds4_btn::CROSS,    &value),
            "btn_east"     => set_bool_bit(&mut self.buttons, ds4_btn::CIRCLE,   &value),
            "btn_west"     => set_bool_bit(&mut self.buttons, ds4_btn::SQUARE,   &value),
            "btn_north"    => set_bool_bit(&mut self.buttons, ds4_btn::TRIANGLE, &value),
            "btn_lb"       => set_bool_bit(&mut self.buttons, ds4_btn::L1,       &value),
            "btn_rb"       => set_bool_bit(&mut self.buttons, ds4_btn::R1,       &value),
            "btn_lt_dig"   => set_bool_bit(&mut self.buttons, ds4_btn::L2_DIG,   &value),
            "btn_rt_dig"   => set_bool_bit(&mut self.buttons, ds4_btn::R2_DIG,   &value),
            "btn_ls"       => set_bool_bit(&mut self.buttons, ds4_btn::L3,       &value),
            "btn_rs"       => set_bool_bit(&mut self.buttons, ds4_btn::R3,       &value),
            "btn_start"    => set_bool_bit(&mut self.buttons, ds4_btn::OPTIONS,  &value),
            "btn_back"     => set_bool_bit(&mut self.buttons, ds4_btn::SHARE,    &value),
            "btn_guide"    => set_bool_u8(&mut self.special, ds4_btn::PS,        &value),
            "btn_touchpad" => set_bool_u8(&mut self.special, ds4_btn::TOUCHPAD,  &value),
            "dpad_up"    => self.dpad[0] = value.as_bool(),
            "dpad_right" => self.dpad[1] = value.as_bool(),
            "dpad_down"  => self.dpad[2] = value.as_bool(),
            "dpad_left"  => self.dpad[3] = value.as_bool(),
            "gyro_x"  => self.gyro_x  = value.as_float(),
            "gyro_y"  => self.gyro_y  = value.as_float(),
            "gyro_z"  => self.gyro_z  = value.as_float(),
            "accel_x" => self.accel_x = value.as_float(),
            "accel_y" => self.accel_y = value.as_float(),
            "accel_z" => self.accel_z = value.as_float(),
            _ => {}
        }
    }

    fn flush(&mut self) {
        if self.dev.is_null() { return; }
        let dpad    = encode_dpad(self.dpad[0], self.dpad[1], self.dpad[2], self.dpad[3]);
        let buttons = (self.buttons & !0xF) | dpad;

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

    fn source_pins(&self) -> &'static [SourcePin] { layouts::DS4_SOURCE_PINS }

    fn poll_outputs(&mut self) -> Vec<(&'static str, Signal)> {
        if self.dev.is_null() { return vec![]; }
        // Cheap read of values populated by the notification thread.
        let r = self.notif_shared.lock().map(|s| s.rumble).unwrap_or_default();
        vec![
            ("rumble_strong", Signal::Float(r.large as f32 / 255.0)),
            ("rumble_weak",   Signal::Float(r.small as f32 / 255.0)),
            ("lightbar_r",    Signal::Float(r.r     as f32 / 255.0)),
            ("lightbar_g",    Signal::Float(r.g     as f32 / 255.0)),
            ("lightbar_b",    Signal::Float(r.b     as f32 / 255.0)),
        ]
    }
}

// ── Keyboard & Mouse ──────────────────────────────────────────────────────────

#[repr(C)]
struct CursorPoint { x: i32, y: i32 }
extern "system" { fn GetCursorPos(lp: *mut CursorPoint) -> i32; }

// Thread-priority FFI (no windows-sys dep in this crate; matches the raw
// `extern "system"` style already used here). HANDLE is pointer-sized.
extern "system" {
    fn GetCurrentThread() -> isize;
    fn SetThreadPriority(thread: isize, priority: i32) -> i32;
}
/// THREAD_PRIORITY_TIME_CRITICAL — the mouse emission loop is the hard
/// real-time leg of mouse output: a tight bounded sleep loop that yields the
/// CPU every iteration, so it can't starve other threads, but while runnable it
/// must preempt the game's render/worker threads or it gets periodically
/// descheduled and motion tears.
const THREAD_PRIORITY_TIME_CRITICAL: i32 = 15;
pub(crate) fn cursor_pos() -> Option<(i32, i32)> {
    let mut p = CursorPoint { x: 0, y: 0 };
    (unsafe { GetCursorPos(&mut p) } != 0).then_some((p.x, p.y))
}

#[derive(Default, Clone, Copy)]
struct MouseButtons { lmb: bool, rmb: bool, mmb: bool, mb4: bool, mb5: bool }

#[derive(Default, Clone, Copy)]
struct KeysHeld { escape: bool, shift: bool, ctrl: bool, alt: bool, win: bool }

// ── Shared state between UI thread and the 1 kHz mouse thread ────────────────

#[derive(Default, Clone)]
struct MouseShared {
    /// Desired velocity in "pixels per 60 Hz reference frame".
    /// The mouse thread converts to pixels via the REAL elapsed dt each tick.
    vel_x: f32,
    vel_y: f32,
    /// Accumulated one-shot pointer DISPLACEMENT in px — applied directly to the
    /// carry (not integrated over dt) and consumed (zeroed) by the mouse thread,
    /// exactly like scroll_pending. The trackpad path; robust to tick jitter.
    disp_x: f32,
    disp_y: f32,
    /// Accumulated scroll clicks — consumed (zeroed) by the mouse thread.
    scroll_pending: i32,   // vertical (+up)
    hscroll_pending: i32,  // horizontal (+right)
    /// Analog scroll rate (notches/sec-ish); persists across ticks like vel_x/y.
    scroll_vel_v: f32,     // vertical (+up)
    scroll_vel_h: f32,     // horizontal (+right)
    buttons: MouseButtons,
    muted: bool,
    /// Set to true when VirtualKeyMouse is dropped — signals the thread to exit.
    stop: bool,
}

// ── 1 kHz mouse thread ──────────────────────────────────────────────────────

fn mouse_thread(shared: Arc<Mutex<MouseShared>>) {
    // Run the emission loop at 1 kHz: smaller per-tick deltas mean the integer
    // mickey quantization (SendInput only takes whole pixels) steps twice as
    // often, so slow stick-aim reads noticeably smoother than at 500 Hz.
    const HZ: f32  = 1000.0;
    const REF: f32 = 60.0; // velocity unit = pixels per 60 Hz reference frame
    // Ticks after our OWN move during which a cursor mismatch is attributed to
    // us (GetCursorPos can lag the SendInput) rather than to a physical mouse.
    // ~16 ms regardless of HZ.
    const SELF_MOVE_COOLDOWN: u16 = (HZ * 0.016) as u16;

    // Raise this thread above the game's render/worker threads. Without it the
    // 1 kHz loop runs at default priority and a busy game periodically
    // deschedules it; with dt-scaling the missed time is then delivered as one
    // large jump on resume — the periodic "tearing" mouse motion.
    unsafe {
        SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_TIME_CRITICAL);
    }

    let mut enigo = Enigo::new(&Settings::default()).ok();
    let mut carry_x = 0.0f32;
    let mut carry_y = 0.0f32;
    // Sub-notch remainder for analog (variable-speed) scroll on each axis.
    let mut scroll_carry_v = 0.0f32;
    let mut scroll_carry_h = 0.0f32;
    // Analog scroll rate unit: notches/sec at |rate| = 1.0 (matches keymouse_hm).
    const SCROLL_REF: f32 = 18.0;
    let mut os_buttons = MouseButtons::default();
    let mut last_cursor  = cursor_pos();
    let mut blocked_until: Option<Instant> = None;
    let mut suppress_cooldown = 0u16;

    let tick = Duration::from_micros((1_000_000.0 / HZ) as u64);
    // Wall-clock time of the previous emission, used to scale velocity by the
    // REAL elapsed interval instead of assuming a perfect tick. Under load
    // (e.g. a virtual gamepad flushing concurrently) the OS scheduler stretches
    // ticks to 1.5–3 ms; a fixed per-tick distance then makes cursor speed
    // lurch with scheduling jitter. dt-scaling keeps motion wall-clock-correct.
    let mut last_emit = Instant::now();

    loop {
        let t0 = Instant::now();

        // Real elapsed time since the previous tick, used to scale velocity so
        // average cursor speed is wall-clock-correct. CRITICAL: clamp it small.
        //
        // Windows `thread::sleep` at 1 kHz is jittery — even a TIME_CRITICAL
        // thread is occasionally descheduled 5–30 ms by a busy game. If we
        // applied the full gap, the accumulated distance would be emitted as one
        // large `move_mouse` jump → the periodic "tearing". Clamping the applied
        // dt to MAX_DT means a gap can move at most MAX_DT worth in one tick:
        // common sub-MAX_DT jitter still applies at correct speed, while a rare
        // big gap loses a sliver of distance (imperceptible for relative aim)
        // instead of lurching. carry_x/y stays sub-pixel as a result, so no
        // banked backlog can build up and discharge as a jump later.
        const MAX_DT: f32 = 0.004; // 4 ms = 4× the nominal 1 kHz tick
        let dt = (t0 - last_emit).as_secs_f32().min(MAX_DT);
        last_emit = t0;

        // Snapshot desired state; consume scroll atomically.
        let state = {
            let mut s = shared.lock().unwrap();
            if s.stop { break; }
            let snap = s.clone();
            s.scroll_pending = 0;
            s.hscroll_pending = 0;
            s.disp_x = 0.0;
            s.disp_y = 0.0;
            snap
        };

        // Effective suppression: user setting AND not "mixed mode" (a virtual
        // gamepad active alongside us). Read every tick so settings/mode apply
        // live. The block window (ms) is user-configurable.
        let (suppression_on, release_ms) = crate::mouse_suppression_effective();

        // Mixed-output braiding: when active, emit the accumulated mouse motion
        // only when the shared turn token says it's the mouse's turn (so a mouse
        // SendInput never coincides with a gamepad HID submit). Between turns we
        // keep accumulating carry below but skip the SendInput — no motion lost.
        // Only braid while mixed mode is on; otherwise emit every tick as normal.
        // Buttons/scroll are unaffected (sporadic, not the continuous stream
        // that drives the game's input-mode arbiter).
        let emit_move = if crate::MOUSE_MIXED_MODE_ACTIVE.load(std::sync::atomic::Ordering::Relaxed) {
            crate::braid_try_mouse()
        } else {
            true
        };

        if let Some(ref mut e) = enigo {
            let now = Instant::now();

            // Physical mouse suppression
            let cur = cursor_pos();
            let suppressed = if suppression_on {
                if let (Some(pos), Some(last)) = (cur, last_cursor) {
                    if pos != last {
                        if suppress_cooldown > 0 {
                            suppress_cooldown -= 1;
                        } else {
                            blocked_until = Some(now + Duration::from_millis(release_ms as u64));
                        }
                    }
                }
                last_cursor = cur;
                blocked_until.map_or(false, |t| now < t)
            } else {
                last_cursor = cur;
                blocked_until = None;
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
                // Scale by REAL elapsed time, not a fixed per-tick constant, so
                // cursor speed stays wall-clock-correct under scheduler jitter.
                // Always accumulate the true desired distance so braiding never
                // loses motion — only the SendInput timing is gated by emit_move.
                carry_x += state.vel_x * REF * dt + state.disp_x;
                carry_y += state.vel_y * REF * dt + state.disp_y;
                if emit_move {
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
                        suppress_cooldown = SELF_MOVE_COOLDOWN; // re-arm window after our own move
                    }
                }
                // Scroll: digital clicks plus dt-scaled analog rate (sub-notch
                // carry). Vertical +up, horizontal +right. enigo's vertical sign
                // convention is preserved from before; horizontal follows suit.
                scroll_carry_v += state.scroll_vel_v * SCROLL_REF * dt;
                scroll_carry_h += state.scroll_vel_h * SCROLL_REF * dt;
                let vclicks = scroll_carry_v.trunc() as i32;
                let hclicks = scroll_carry_h.trunc() as i32;
                scroll_carry_v -= vclicks as f32;
                scroll_carry_h -= hclicks as f32;
                let v_total = state.scroll_pending + vclicks;
                let h_total = state.hscroll_pending + hclicks;
                if v_total != 0 { let _ = e.scroll(v_total, Axis::Vertical); }
                if h_total != 0 { let _ = e.scroll(h_total, Axis::Horizontal); }
                btn_sync!(os_buttons.lmb, state.buttons.lmb, Button::Left);
                btn_sync!(os_buttons.rmb, state.buttons.rmb, Button::Right);
                btn_sync!(os_buttons.mmb, state.buttons.mmb, Button::Middle);
                btn_sync!(os_buttons.mb4, state.buttons.mb4, Button::Back);
                btn_sync!(os_buttons.mb5, state.buttons.mb5, Button::Forward);
            } else {
                carry_x = 0.0;
                carry_y = 0.0;
                scroll_carry_v = 0.0;
                scroll_carry_h = 0.0;
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
    pub muted: bool,

    // Desired per-frame velocity / state set by send()
    mouse_vel_x: f32,
    mouse_vel_y: f32,
    // One-shot pointer displacement (mouse_move*): applied once, not integrated.
    mouse_disp_x: f32,
    mouse_disp_y: f32,
    scroll_delta: i32,   // vertical digital scroll clicks (scroll_up/down)
    hscroll_delta: i32,  // horizontal digital scroll clicks (scroll_left/right)
    scroll_vel_v: f32,   // analog vertical scroll rate (scroll_y), +up
    scroll_vel_h: f32,   // analog horizontal scroll rate (scroll_x), +right
    buttons: MouseButtons,
    keys: KeysHeld,
    learned_keys: HashMap<String, bool>,

    // Keyboard output on the UI thread (no need for the I/O polling rate)
    enigo_keys: Option<Enigo>,
    os_keys: KeysHeld,
    os_learned_keys: HashMap<String, bool>,
    // Per-key auto-repeat timing for learned keys. We re-send Direction::Press
    // at OS-typical rates (initial ~500ms delay, then ~30 Hz) so apps that
    // listen for WM_KEYDOWN repeats (text fields, games using typematic) feel
    // a held synthetic key the same as a real one.
    key_press_at:  HashMap<String, std::time::Instant>,
    key_repeat_at: HashMap<String, std::time::Instant>,

    // Shared with the mouse thread
    mouse_shared: Arc<Mutex<MouseShared>>,
    _mouse_thread: std::thread::JoinHandle<()>,

    enigo_ok: bool,
}

impl VirtualKeyMouse {
    pub fn new() -> Self {
        let shared = Arc::new(Mutex::new(MouseShared::default()));
        let shared2 = Arc::clone(&shared);
        let thread = std::thread::Builder::new()
            .name("keymouse-1khz".into())
            .spawn(move || mouse_thread(shared2))
            .expect("failed to spawn mouse thread");

        let enigo_keys = Enigo::new(&Settings::default()).ok();
        let ok = enigo_keys.is_some();
        Self {
            muted: false,
            mouse_vel_x: 0.0,
            mouse_vel_y: 0.0,
            mouse_disp_x: 0.0,
            mouse_disp_y: 0.0,
            scroll_delta: 0,
            hscroll_delta: 0,
            scroll_vel_v: 0.0,
            scroll_vel_h: 0.0,
            buttons: MouseButtons::default(),
            keys: KeysHeld::default(),
            learned_keys: HashMap::new(),
            enigo_keys,
            os_keys: KeysHeld::default(),
            os_learned_keys: HashMap::new(),
            key_press_at:  HashMap::new(),
            key_repeat_at: HashMap::new(),
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
            // Velocity pins — the dedicated mouse thread spreads these at 1 kHz.
            // Semantics: value = pixels per 60 Hz reference frame (unchanged from before).
            "mouse" => { if let Signal::Vec2(v) = value { self.mouse_vel_x += v.x; self.mouse_vel_y += -v.y; } }
            "mouse_x"       => { if let Signal::Float(f) = value { self.mouse_vel_x += f; } }
            "mouse_y"       => { if let Signal::Float(f) = value { self.mouse_vel_y += -f; } }
            // Absolute pointer move (+Y up → negate for screen y-down like "mouse").
            "mouse_move"    => { if let Signal::Vec2(v) = value { self.mouse_disp_x += v.x; self.mouse_disp_y += -v.y; } }
            "mouse_move_x"  => { if let Signal::Float(f) = value { self.mouse_disp_x += f; } }
            "mouse_move_y"  => { if let Signal::Float(f) = value { self.mouse_disp_y += -f; } }
            "scroll_up"     => { if matches!(value, Signal::Bool(true)) { self.scroll_delta += 1; } }
            "scroll_down"   => { if matches!(value, Signal::Bool(true)) { self.scroll_delta -= 1; } }
            "scroll_right"  => { if matches!(value, Signal::Bool(true)) { self.hscroll_delta += 1; } }
            "scroll_left"   => { if matches!(value, Signal::Bool(true)) { self.hscroll_delta -= 1; } }
            "scroll_y"      => { if let Signal::Float(f) = value { self.scroll_vel_v += f; } }
            "scroll_x"      => { if let Signal::Float(f) = value { self.scroll_vel_h += f; } }
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
            s.disp_x             += std::mem::take(&mut self.mouse_disp_x);
            s.disp_y             += std::mem::take(&mut self.mouse_disp_y);
            s.scroll_pending     += std::mem::take(&mut self.scroll_delta);
            s.hscroll_pending    += std::mem::take(&mut self.hscroll_delta);
            s.scroll_vel_v        = std::mem::take(&mut self.scroll_vel_v);
            s.scroll_vel_h        = std::mem::take(&mut self.scroll_vel_h);
            s.buttons             = self.buttons;
            s.muted               = self.muted;
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

        let now = std::time::Instant::now();
        const REPEAT_INITIAL: std::time::Duration = std::time::Duration::from_millis(500);
        const REPEAT_INTERVAL: std::time::Duration = std::time::Duration::from_millis(33); // ~30 Hz
        let learned: Vec<(String, bool)> = self.learned_keys.iter()
            .map(|(k, &v)| (k.clone(), v))
            .collect();
        for (pin_name, want) in learned {
            let Some(key) = egui_key_name_to_enigo(&pin_name) else { continue; };
            let os = *self.os_learned_keys.get(&pin_name).unwrap_or(&false);
            if !self.muted {
                if want != os {
                    let _ = enigo.key(key, if want { Direction::Press } else { Direction::Release });
                    self.os_learned_keys.insert(pin_name.clone(), want);
                    if want {
                        self.key_press_at.insert(pin_name.clone(), now);
                        self.key_repeat_at.insert(pin_name.clone(), now);
                    } else {
                        self.key_press_at.remove(&pin_name);
                        self.key_repeat_at.remove(&pin_name);
                    }
                } else if want {
                    // Held — re-send Press to drive OS-level typematic repeat.
                    let press_at = self.key_press_at.entry(pin_name.clone()).or_insert(now);
                    let elapsed_since_press = now.saturating_duration_since(*press_at);
                    if elapsed_since_press >= REPEAT_INITIAL {
                        let last = self.key_repeat_at.entry(pin_name.clone()).or_insert(now);
                        if now.saturating_duration_since(*last) >= REPEAT_INTERVAL {
                            let _ = enigo.key(key, Direction::Press);
                            *last = now;
                        }
                    }
                }
            } else if os {
                let _ = enigo.key(key, Direction::Release);
                self.os_learned_keys.insert(pin_name.clone(), false);
                self.key_press_at.remove(&pin_name);
                self.key_repeat_at.remove(&pin_name);
            }
        }
    }

    fn reset_outputs(&mut self) {
        self.mouse_vel_x = 0.0;
        self.mouse_vel_y = 0.0;
        self.mouse_disp_x = 0.0;
        self.mouse_disp_y = 0.0;
        self.scroll_delta = 0;
        self.hscroll_delta = 0;
        self.scroll_vel_v = 0.0;
        self.scroll_vel_h = 0.0;
        self.buttons = MouseButtons::default();
        self.keys = KeysHeld::default();
        for v in self.learned_keys.values_mut() { *v = false; }
        self.key_press_at.clear();
        self.key_repeat_at.clear();
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
    // Universal coercion: any Float >= 0.5 → true, any Bool → its value, etc.
    set_bit(bits, mask, value.as_bool());
}

fn set_bool_u8(bits: &mut u8, mask: u8, value: &Signal) {
    if value.as_bool() { *bits |= mask; } else { *bits &= !mask; }
}

/// Maps an egui Key debug name (format!("{key:?}")) to an enigo Key.
fn egui_key_name_to_enigo(name: &str) -> Option<Key> {
    // Accept both the legacy AutoMap Collector form ("A", "Space", "Num0",
    // raw `{egui::Key:?}`) and the Remapper form ("key_a", "key_space",
    // "key_num0", lowercased with a "key_" prefix).
    let rest = name.strip_prefix("key_").unwrap_or(name);
    let raw_lower = rest.eq_ignore_ascii_case(rest);
    let _ = raw_lower;
    Some(match rest.to_ascii_lowercase().as_str() {
        "enter" | "return" => Key::Return,
        "space"            => Key::Space,
        "tab"              => Key::Tab,
        "backspace"        => Key::Backspace,
        "delete"           => Key::Delete,
        "home"             => Key::Home,
        "end"              => Key::End,
        "pageup"           => Key::PageUp,
        "pagedown"         => Key::PageDown,
        "printscreen"      => Key::PrintScr, // VK_SNAPSHOT (the Print Screen key)
        "pause"            => Key::Pause,    // VK_PAUSE (Pause/Break)
        "arrowup"          => Key::UpArrow,
        "arrowdown"        => Key::DownArrow,
        "arrowleft"        => Key::LeftArrow,
        "arrowright"       => Key::RightArrow,
        "capslock"         => Key::CapsLock,
        "f1"  => Key::F1,  "f2"  => Key::F2,  "f3"  => Key::F3,  "f4"  => Key::F4,
        "f5"  => Key::F5,  "f6"  => Key::F6,  "f7"  => Key::F7,  "f8"  => Key::F8,
        "f9"  => Key::F9,  "f10" => Key::F10, "f11" => Key::F11, "f12" => Key::F12,
        "f13" => Key::F13, "f14" => Key::F14, "f15" => Key::F15, "f16" => Key::F16,
        "f17" => Key::F17, "f18" => Key::F18, "f19" => Key::F19, "f20" => Key::F20,
        // Single letter → VK code (VK_A=0x41 … VK_Z=0x5A). Match on the
        // lowercased form then send the uppercase VK scancode.
        n if n.len() == 1 => {
            let c = n.chars().next()?;
            if c.is_ascii_alphabetic() {
                Key::Other(c.to_ascii_uppercase() as u32)
            } else { return None; }
        }
        // Digits: bare "0".."9" or "num0".."num9".
        n if n.len() == 1 && n.chars().next().map(|c| c.is_ascii_digit()).unwrap_or(false) => {
            let c = n.chars().next()?;
            Key::Other(c as u32)
        }
        n if n.starts_with("num") && n.len() == 4 => {
            let c = n.chars().nth(3)?;
            if c.is_ascii_digit() { Key::Other(c as u32) } else { return None; }
        }
        _ => return None,
    })
}

/// Minimal UI-thread keyboard tapper for the gamepad Alt+Tab window switcher.
///
/// Independent of the virtual-device pool (which the I/O thread resets every
/// tick while UI-nav suppression is active). Owns its own `Enigo` and presses
/// keys synchronously on the caller's (UI) thread — exactly what the switcher
/// needs: hold Alt, tap Tab / arrow keys, release Alt to commit.
pub struct UiKeyTapper {
    enigo: Option<Enigo>,
    alt_down: bool,
}

impl Default for UiKeyTapper {
    fn default() -> Self {
        Self::new()
    }
}

impl UiKeyTapper {
    pub fn new() -> Self {
        Self {
            enigo: Enigo::new(&Settings::default()).ok(),
            alt_down: false,
        }
    }

    /// Press (hold) Alt if not already held.
    pub fn alt_down(&mut self) {
        if self.alt_down {
            return;
        }
        if let Some(e) = &mut self.enigo {
            let _ = e.key(Key::Alt, Direction::Press);
            self.alt_down = true;
        }
    }

    /// Release Alt if held (commits the Windows switcher selection).
    pub fn alt_up(&mut self) {
        if !self.alt_down {
            return;
        }
        if let Some(e) = &mut self.enigo {
            let _ = e.key(Key::Alt, Direction::Release);
        }
        self.alt_down = false;
    }

    pub fn is_alt_down(&self) -> bool {
        self.alt_down
    }

    /// Tap a key once (press + release). `name` accepts the egui-debug or
    /// `key_*` forms understood by `egui_key_name_to_enigo`, e.g. "tab",
    /// "arrowleft".
    pub fn tap(&mut self, name: &str) {
        if let (Some(e), Some(k)) = (&mut self.enigo, egui_key_name_to_enigo(name)) {
            let _ = e.key(k, Direction::Click);
        }
    }
}

#[cfg(test)]
mod instance_id_tests {
    use super::*;

    /// Mirror of `device_ops::build_device`'s split: device_id -> (kind, instance).
    /// The id `instance_label` produces MUST parse back to the same instance, or
    /// the helper keys two same-kind pads onto one record (the multi-x360 bug:
    /// the 2nd pad reclaimed the 1st's node). This locks producer/consumer
    /// agreement on the `kind_id[.N]` format.
    fn split(id: &str) -> (&str, usize) {
        match id.rfind('.') {
            Some(dot) => match id[dot + 1..].parse::<usize>() {
                Ok(n) => (&id[..dot], n),
                Err(_) => (id, 0),
            },
            None => (id, 0),
        }
    }

    #[test]
    fn instance_label_id_round_trips_through_split() {
        // 3-segment kind (HIDMaestro) is the regression case.
        for kind in ["virtual.hm.xinput", "virtual.hm.ds4", "virtual.hm.dualsense"] {
            for inst in 0..4usize {
                let (id, _name) = instance_label(kind, "X", inst);
                let (k, n) = split(&id);
                assert_eq!((k, n), (kind, inst), "id {id:?} must split back to ({kind}, {inst})");
            }
        }
    }

    #[test]
    fn distinct_instances_yield_distinct_ids() {
        // The whole point: two pads of the same kind get different device_ids.
        let (id0, _) = instance_label("virtual.hm.xinput", "X", 0);
        let (id1, _) = instance_label("virtual.hm.xinput", "X", 1);
        assert_ne!(id0, id1);
        assert_eq!(id0, "virtual.hm.xinput");
        assert_eq!(id1, "virtual.hm.xinput.1");
    }
}
