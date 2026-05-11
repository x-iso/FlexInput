use std::collections::HashMap;
use std::time::{Duration, Instant};
use hidapi::{HidApi, HidDevice};

const SONY_VID: u16       = 0x054C;
const DS4_PIDS: &[u16]   = &[0x05C4, 0x09CC];
const DUALSENSE_PID: u16  = 0x0CE6;
const SWITCH_VID: u16     = 0x057E;
const SWITCH_PRO_PID: u16 = 0x2009;

const USAGE_PAGE_GENERIC_DESKTOP: u16 = 0x0001;
const USAGE_GAMEPAD: u16 = 0x0005;

// ── IMU sensitivity constants ─────────────────────────────────────────────────
// Normalized signal graph values: ±1.0 means ±GYRO_REF_DPS deg/s or ±ACCEL_REF_G G.
// Virtual devices that consume gyro/accel signals assume this common reference so
// they can rescale to their own hardware spec without knowing the source device.

/// ±1.0 in the signal graph corresponds to this many degrees per second.
pub const GYRO_REF_DPS: f32 = 2000.0;
/// ±1.0 in the signal graph corresponds to this many standard gravity units.
pub const ACCEL_REF_G: f32 = 8.0;

// Per-device sensitivities (physical deg/s or G per raw sensor LSB).
// DS4 / DualSense: factory ±2000 dps gyro, ±8 G accel.
const DS4_GYRO_DPS_PER_LSB: f32 = 2000.0 / 32767.0;
const DS4_ACCEL_G_PER_LSB: f32  = 8.0   / 32767.0;
// Switch Pro (ICM-20689): configured at ±4000 dps gyro (not ±2000), per empirical testing.
// If gyro reads ~2× too large, change 4000.0 → 2000.0.
const SWITCH_GYRO_DPS_PER_LSB: f32 = 4000.0 / 32767.0;
const SWITCH_ACCEL_G_PER_LSB: f32  = 8.0   / 32767.0;

// Retry open no more than once per N seconds to avoid hammering HidHide.
const RETRY_INTERVAL: Duration = Duration::from_secs(2);

#[derive(Clone, Copy, Default, Debug)]
pub struct TouchPoint {
    /// Active flag — touchpad currently sees this finger.
    pub active: bool,
    /// Normalized X in roughly [-1, 1] (left edge = -1, right edge = +1).
    pub x: f32,
    /// Normalized Y in roughly [-1, 1] (top edge = -1, bottom edge = +1).
    pub y: f32,
}

/// Switch Pro button state read directly from input report 0x30 bytes 3/4/5.
/// Used to bypass gilrs's WGI backend which loses diagonal D-Pad positions and
/// has unreliable Home/Capture/Plus/Minus mapping in BT mode.
/// Naming uses Nintendo's physical labels (A=east, B=south, X=north, Y=west).
#[derive(Clone, Copy, Default, Debug)]
pub struct SwitchProButtons {
    pub btn_a: bool, pub btn_b: bool, pub btn_x: bool, pub btn_y: bool,
    pub btn_l: bool, pub btn_r: bool, pub btn_zl: bool, pub btn_zr: bool,
    pub btn_lstick: bool, pub btn_rstick: bool,
    pub btn_minus: bool, pub btn_plus: bool,
    pub btn_home: bool, pub btn_capture: bool,
    pub dpad_up: bool, pub dpad_down: bool, pub dpad_left: bool, pub dpad_right: bool,
    /// L-Stick analog X normalized to [-1, 1] (raw 12-bit calibrated value).
    pub lstick_x: f32,
    pub lstick_y: f32,
    pub rstick_x: f32,
    pub rstick_y: f32,
}

#[derive(Clone, Copy, Default, Debug)]
pub struct HidReading {
    pub gyro_x: f32,
    pub gyro_y: f32,
    pub gyro_z: f32,
    pub accel_x: f32,
    pub accel_y: f32,
    pub accel_z: f32,
    /// True when the source device exposes a touchpad (DS4 / DualSense).
    pub has_touchpad: bool,
    pub touch1: TouchPoint,
    pub touch2: TouchPoint,
    /// Touchpad click (the whole touchpad is also a button on DS4 / DualSense).
    /// Read straight from the HID report because gilrs's Windows backend doesn't
    /// expose it reliably.
    pub touchpad_click: bool,
    /// Microphone mute button (DualSense only).
    pub mic_button: bool,
    /// Switch Pro full button state, parsed from raw HID. None for non-Switch devices.
    pub switch_buttons: Option<SwitchProButtons>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Connection { Usb, Bt }

enum DeviceKind {
    Ds4,
    DualSense { connection: Option<Connection> },
    SwitchPro { initialized: bool, packet_counter: u8 },
}

struct HidEntry {
    device: HidDevice,
    kind: DeviceKind,
    last: HidReading,
    out: OutputState,
    /// Becomes true after the first `set_output_byte` call. We keep refreshing
    /// the output report every frame from then on so that rumble/lightbar stay
    /// in sync (especially over BT) without needing a dirty flag per pin.
    output_active: bool,
}

#[derive(Clone, Copy, Default)]
struct OutputState {
    /// Big / heavy / low-frequency motor (DS4 "left", DualSense `motor_left`).
    rumble_strong: u8,
    /// Small / light / high-frequency motor (DS4 "right", DualSense `motor_right`).
    rumble_weak: u8,
    lightbar_r: u8,
    lightbar_g: u8,
    lightbar_b: u8,
    // DualSense adaptive trigger — right
    // mode: 0=Off, 1=Feedback, 2=Weapon, 3=Vibration
    trigger_r_mode:     u8,
    trigger_r_start:    u8, // zone 0–9 (stored as raw zone; caller passes 0–9 already scaled)
    trigger_r_end:      u8, // zone 0–9 (Weapon mode only)
    trigger_r_strength: u8, // force 0–7
    trigger_r_freq:     u8, // 0–255 (Vibration mode only)
    // DualSense adaptive trigger — left
    trigger_l_mode:     u8,
    trigger_l_start:    u8,
    trigger_l_end:      u8,
    trigger_l_strength: u8,
    trigger_l_freq:     u8,
    // DualSense LEDs
    // player_led: 0=off, 1=P1(0x04), 2=P2(0x0A), 3=P3(0x15), 4=P4(0x1B)
    player_led: u8,
    // mic_led: 0=off, 1=on(orange), 2=pulsing
    mic_led:    u8,
}

pub struct GyroManager {
    api: Option<HidApi>,
    // key: (vid, pid, instance_index)
    devices: HashMap<(u16, u16, usize), HidEntry>,
    // tracks the last failed open attempt to rate-limit retries
    failed_opens: HashMap<(u16, u16, usize), Instant>,
}

impl GyroManager {
    pub fn new() -> Self {
        Self { api: HidApi::new().ok(), devices: HashMap::new(), failed_opens: HashMap::new() }
    }

    /// Returns the latest IMU + touchpad reading for the Nth physical device with this VID/PID.
    pub fn read(&mut self, vid: u16, pid: u16, idx: usize) -> Option<HidReading> {
        if classify(vid, pid).is_none() {
            return None;
        }

        if !self.devices.contains_key(&(vid, pid, idx)) {
            let key = (vid, pid, idx);
            let should_try = self.failed_opens.get(&key)
                .map_or(true, |t| t.elapsed() >= RETRY_INTERVAL);
            if should_try {
                // Refresh device list so newly-plugged devices are visible.
                if let Some(api) = &mut self.api {
                    let _ = api.refresh_devices();
                }
                match self.open_device(vid, pid, idx) {
                    Some(entry) => { self.devices.insert(key, entry); }
                    None        => { self.failed_opens.insert(key, Instant::now()); }
                }
            }
        }

        let entry = self.devices.get_mut(&(vid, pid, idx))?;

        if let DeviceKind::SwitchPro { initialized, packet_counter } = &mut entry.kind {
            if !*initialized {
                *initialized = init_switch_pro(&entry.device, packet_counter);
            }
        }

        // If reading fails (device disconnected), drop and retry next cycle.
        let ok = drain_reports(entry);
        if !ok {
            self.devices.remove(&(vid, pid, idx));
            return None;
        }
        Some(entry.last)
    }

    pub fn remove(&mut self, vid: u16, pid: u16, idx: usize) {
        self.devices.remove(&(vid, pid, idx));
    }

    fn open_device(&self, vid: u16, pid: u16, idx: usize) -> Option<HidEntry> {
        let api = self.api.as_ref()?;
        let kind_tag = classify(vid, pid)?;

        // Primary filter: usage_page + usage (correct, but returns 0 when HidHide
        // intercepts enumeration on Windows even for whitelisted apps).
        let mut paths: Vec<_> = api
            .device_list()
            .filter(|d| {
                d.vendor_id() == vid
                    && d.product_id() == pid
                    && d.usage_page() == USAGE_PAGE_GENERIC_DESKTOP
                    && d.usage() == USAGE_GAMEPAD
            })
            .collect();

        // Fallback: if usage fields came back as 0 (HidHide / Windows quirk),
        // use known interface numbers for each controller instead.
        if paths.is_empty() {
            let iface = preferred_interface(&kind_tag);
            paths = api
                .device_list()
                .filter(|d| {
                    d.vendor_id() == vid
                        && d.product_id() == pid
                        && d.interface_number() == iface
                })
                .collect();
        }

        // Last resort: accept any interface with the right VID/PID (e.g. BT
        // connections that only expose a single interface).
        if paths.is_empty() {
            paths = api
                .device_list()
                .filter(|d| d.vendor_id() == vid && d.product_id() == pid)
                .collect();
        }

        let info = paths.get(idx)?;
        let device = api.open_path(info.path()).ok()?;
        device.set_blocking_mode(false).ok()?;

        let kind = match kind_tag {
            KindTag::Ds4       => DeviceKind::Ds4,
            KindTag::DualSense => DeviceKind::DualSense { connection: None },
            KindTag::SwitchPro => DeviceKind::SwitchPro { initialized: false, packet_counter: 0 },
        };
        Some(HidEntry {
            device,
            kind,
            last: HidReading::default(),
            out: OutputState::default(),
            output_active: false,
        })
    }

    /// Stage one byte of an output report (rumble/lightbar) for the Nth physical
    /// device with this VID/PID. Has no effect if the device isn't open. Call
    /// `flush_outputs()` once per frame to actually transmit.
    ///
    /// Pin name conventions: DS4/DualSense use `rumble_strong`/`rumble_weak`,
    /// Switch Pro uses `hd_rumble_l`/`hd_rumble_r`. Both pairs map to the same
    /// internal storage (left side = strong = big motor; right = weak = small).
    pub fn set_output_byte(&mut self, vid: u16, pid: u16, idx: usize, pin_id: &str, byte: u8) {
        let entry = match self.devices.get_mut(&(vid, pid, idx)) {
            Some(e) => e,
            None => return,
        };
        let updated = match pin_id {
            "rumble_strong" | "hd_rumble_l" => { entry.out.rumble_strong = byte; true }
            "rumble_weak"   | "hd_rumble_r" => { entry.out.rumble_weak   = byte; true }
            "lightbar_r"    => { entry.out.lightbar_r = byte; true }
            "lightbar_g"    => { entry.out.lightbar_g = byte; true }
            "lightbar_b"    => { entry.out.lightbar_b = byte; true }
            // Adaptive trigger pins — caller passes Float 0–1 already scaled to
            // the appropriate range before calling set_output_byte:
            //   mode:     0–3  (0=Off,1=Feedback,2=Weapon,3=Vibration)
            //   start/end: 0–9 (zone index along trigger travel)
            //   strength:  0–7 (force level)
            //   freq:      0–255 (vibration frequency, Vibration mode only)
            "trigger_r_mode"     => { entry.out.trigger_r_mode     = byte; true }
            "trigger_r_start"    => { entry.out.trigger_r_start    = byte; true }
            "trigger_r_end"      => { entry.out.trigger_r_end      = byte; true }
            "trigger_r_strength" => { entry.out.trigger_r_strength = byte; true }
            "trigger_r_freq"     => { entry.out.trigger_r_freq     = byte; true }
            "trigger_l_mode"     => { entry.out.trigger_l_mode     = byte; true }
            "trigger_l_start"    => { entry.out.trigger_l_start    = byte; true }
            "trigger_l_end"      => { entry.out.trigger_l_end      = byte; true }
            "trigger_l_strength" => { entry.out.trigger_l_strength = byte; true }
            "trigger_l_freq"     => { entry.out.trigger_l_freq     = byte; true }
            // DualSense LEDs — caller passes scaled byte
            // player_led: 0=off, 1=P1, 2=P2, 3=P3, 4=P4
            // mic_led:    0=off, 1=on(orange), 2=pulsing
            "player_led" => { entry.out.player_led = byte; true }
            "mic_led"    => { entry.out.mic_led    = byte; true }
            _ => false,
        };
        if updated { entry.output_active = true; }
    }

    /// Send pending output reports for every device that has been driven at
    /// least once. Call once per frame.
    pub fn flush_outputs(&mut self) {
        for entry in self.devices.values_mut() {
            if !entry.output_active { continue; }
            // Destructure so we can borrow device + out + kind disjointly.
            let HidEntry { device, kind, out, .. } = entry;
            match kind {
                DeviceKind::Ds4 => {
                    hid_write(device, &build_ds4_usb_out(out), "ds4");
                }
                DeviceKind::DualSense { connection } => {
                    // BT path needs a 78-byte report 0x31 with seq_tag, padding
                    // and a CRC32 trailer — not implemented yet, so skip.
                    if !matches!(connection, Some(Connection::Bt)) {
                        hid_write(device, &build_dualsense_usb_out(out), "dualsense");
                    }
                }
                DeviceKind::SwitchPro { initialized, packet_counter } => {
                    // Rumble subcommand 0x48 must have completed first.
                    if !*initialized { continue; }
                    let left  = switch_rumble_encode(out.rumble_strong as f32 / 255.0);
                    let right = switch_rumble_encode(out.rumble_weak   as f32 / 255.0);
                    let pkt = build_switch_rumble_only(*packet_counter, left, right);
                    *packet_counter = packet_counter.wrapping_add(1);
                    hid_write(device, &pkt, "switch_pro");
                }
            }
        }
    }
}

/// Send an output report and surface failures in debug builds. HID writes can
/// fail if HidHide hasn't whitelisted FlexInput, if the controller dropped, or
/// if another process holds an exclusive handle.
fn hid_write(device: &HidDevice, data: &[u8], _tag: &str) {
    let res = device.write(data);
    #[cfg(debug_assertions)]
    if let Err(e) = res {
        eprintln!("[hid-out:{}] write failed ({} bytes): {:?}", _tag, data.len(), e);
    }
    #[cfg(not(debug_assertions))]
    let _ = res;
}

// ── Switch Pro initialisation ─────────────────────────────────────────────────

fn init_switch_pro(device: &HidDevice, counter: &mut u8) -> bool {
    let mut buf = [0u8; 64];

    // USB handshake (silently ignored / fails on BT — that's fine).
    let _ = device.write(&pad64([0x80, 0x02]));
    if let Ok(n) = device.read_timeout(&mut buf, 100) {
        if n > 0 && buf[0] == 0x81 {
            // USB confirmed: disable USB inactivity timeout.
            let _ = device.write(&pad64([0x80, 0x04]));
            let _ = device.read_timeout(&mut buf, 100);
        }
    }

    // Subcommand 0x48 0x01 — enable vibration. Without this, output report
    // 0x10 (rumble-only) is silently ignored even with valid encoded packets.
    if device.write(&subcommand(*counter, 0x48, &[0x01])).is_err() {
        return false;
    }
    *counter = counter.wrapping_add(1);
    wait_for_ack(device, 0x21, &mut buf);

    // Subcommand 0x40 0x01 — enable IMU.
    if device.write(&subcommand(*counter, 0x40, &[0x01])).is_err() {
        return false;
    }
    *counter = counter.wrapping_add(1);
    wait_for_ack(device, 0x21, &mut buf);

    // Subcommand 0x03 0x30 — full input report mode (sends 0x30 with IMU).
    if device.write(&subcommand(*counter, 0x03, &[0x30])).is_err() {
        return false;
    }
    *counter = counter.wrapping_add(1);
    wait_for_ack(device, 0x21, &mut buf);

    true
}

fn wait_for_ack(device: &HidDevice, expected_id: u8, buf: &mut [u8; 64]) {
    for _ in 0..15 {
        if let Ok(n) = device.read_timeout(buf, 50) {
            if n > 0 && buf[0] == expected_id {
                return;
            }
        }
    }
}

/// Build a 64-byte padded Switch Pro output report (report ID 0x01).
fn subcommand(counter: u8, id: u8, args: &[u8]) -> [u8; 64] {
    let mut cmd = [0u8; 64];
    cmd[0] = 0x01; // output report ID
    cmd[1] = counter & 0x0F;
    // Neutral rumble data at bytes 2–9.
    cmd[2] = 0x00; cmd[3] = 0x01; cmd[4] = 0x40; cmd[5] = 0x40;
    cmd[6] = 0x00; cmd[7] = 0x01; cmd[8] = 0x40; cmd[9] = 0x40;
    cmd[10] = id;
    for (i, &b) in args.iter().enumerate() {
        if 11 + i < 64 { cmd[11 + i] = b; }
    }
    cmd
}

/// Pad a short slice into a 64-byte array (for 0x80 USB handshake reports).
fn pad64(prefix: impl AsRef<[u8]>) -> [u8; 64] {
    let mut buf = [0u8; 64];
    for (i, &b) in prefix.as_ref().iter().enumerate() {
        if i < 64 { buf[i] = b; }
    }
    buf
}

// ── Report reading ────────────────────────────────────────────────────────────

/// Returns false if the device has errored out (caller should drop the entry).
fn drain_reports(entry: &mut HidEntry) -> bool {
    let mut buf = [0u8; 128];
    loop {
        match entry.device.read(&mut buf) {
            Ok(0) => break,
            Err(_) => return false,
            Ok(n) => {
                if let Some(r) = parse_report(&buf[..n], &mut entry.kind) {
                    entry.last = r;
                }
            }
        }
    }
    true
}

fn parse_report(buf: &[u8], kind: &mut DeviceKind) -> Option<HidReading> {
    if buf.is_empty() { return None; }
    match kind {
        DeviceKind::Ds4 => parse_ds4(buf),
        DeviceKind::DualSense { connection } => parse_dualsense(buf, connection),
        DeviceKind::SwitchPro { .. } => parse_switch_pro(buf),
    }
}

fn parse_ds4(buf: &[u8]) -> Option<HidReading> {
    // Layout reference: Linux drivers/hid/hid-sony.c, struct dualshock4_input_report_common.
    //   payload offsets: lx,ly(0,1) rx,ry(2,3) buttons[3](4-6) l2,r2(7,8)
    //                    timestamp(9,10) battery(11) gyro[3](12-17) accel[3](18-23)
    //   buttons[2] (payload 6): bit 0 = PS, bit 1 = Touchpad click, bits 2-7 = counter.
    // USB: report 0x01, payload starts at byte 1 → gyro 13, accel 19, btn2 byte 7.
    // BT:  report 0x11, BT prefix is 2 bytes, payload starts at byte 3 → gyro 15, accel 21, btn2 byte 9.
    let (go, ao, btn2) = match buf[0] {
        0x01 if buf.len() >= 25 => (13, 19, 7),
        0x11 if buf.len() >= 77 => (15, 21, 9),
        _ => return None,
    };
    let mut r = build(buf, go, ao, DS4_GYRO_DPS_PER_LSB, DS4_ACCEL_G_PER_LSB);
    r.touchpad_click = buf[btn2] & 0x02 != 0;
    Some(r)
}

fn parse_dualsense(buf: &[u8], connection: &mut Option<Connection>) -> Option<HidReading> {
    // Layout reference: Linux drivers/hid/hid-playstation.c, struct dualsense_input_report.
    //   payload offsets: x,y(0,1) rx,ry(2,3) z,rz(4,5) seq(6) buttons[4](7-10)
    //                    reserved[4](11-14) gyro[3](15-20) accel[3](21-26)
    //                    timestamp(27-30) reserved2(31) touch[2](32-39)
    // USB: report 0x01, payload starts at byte 1 → gyro 16, accel 22, touch 33/37.
    // BT:  report 0x31, payload starts at byte 2 (1-byte BT preamble) → gyro 17, accel 23, touch 34/38.
    // buttons[2] (payload offset 9): bit 0 = PS, bit 1 = Touchpad click, bit 2 = Mute.
    let (conn, go, ao, t1, t2, btn2) = match buf[0] {
        // USB needs ≥41 bytes to cover touch2 (37+8 ish, but 41 covers touch2's 4 bytes at 37-40).
        // For just gyro/accel reading without touch, ≥28 is enough; we pick the larger so we get touch.
        0x01 if buf.len() >= 41 => (Connection::Usb, 16, 22, 33, 37, 10),
        // BT report 0x31 is 78 bytes including 4-byte CRC; touch fits comfortably.
        0x31 if buf.len() >= 79 => (Connection::Bt,  17, 23, 34, 38, 11),
        _ => return None,
    };
    *connection = Some(conn);

    let mut r = build(buf, go, ao, DS4_GYRO_DPS_PER_LSB, DS4_ACCEL_G_PER_LSB);
    r.has_touchpad = true;
    r.touch1 = parse_dualsense_touch(buf, t1);
    r.touch2 = parse_dualsense_touch(buf, t2);
    r.touchpad_click = buf[btn2] & 0x02 != 0;
    r.mic_button     = buf[btn2] & 0x04 != 0;
    Some(r)
}

/// Parse one DualSense `dualsense_touch_point` (4 bytes). Coordinates are
/// 12-bit (X 0..1919, Y 0..1079) and we normalise to roughly [-1, 1] with
/// the centre of the touchpad mapping to 0.
fn parse_dualsense_touch(buf: &[u8], off: usize) -> TouchPoint {
    if off + 4 > buf.len() { return TouchPoint::default(); }
    let contact = buf[off];
    let active = (contact & 0x80) == 0;
    let x_lo = buf[off + 1] as u16;
    let mid  = buf[off + 2] as u16;
    let y_hi = buf[off + 3] as u16;
    let raw_x = ((mid & 0x0F) << 8) | x_lo;
    let raw_y = (y_hi << 4) | ((mid & 0xF0) >> 4);
    // DualSense touchpad: 1920 × 1080 sensor area.
    const HALF_W: f32 = 1920.0 / 2.0;
    const HALF_H: f32 = 1080.0 / 2.0;
    TouchPoint {
        active,
        x: (raw_x as f32 - HALF_W) / HALF_W,
        y: (raw_y as f32 - HALF_H) / HALF_H,
    }
}

fn parse_switch_pro(buf: &[u8]) -> Option<HidReading> {
    // Report 0x30: standard input report with full button state and 3 IMU samples.
    // Layout (per dekuNukem/Nintendo_Switch_Reverse_Engineering bluetooth_hid_notes.md):
    //   byte 3 = right buttons:  bit0=Y, bit1=X, bit2=B, bit3=A, bit6=R, bit7=ZR
    //   byte 4 = shared:         bit0=Minus, bit1=Plus, bit2=RStick, bit3=LStick,
    //                            bit4=Home, bit5=Capture
    //   byte 5 = left buttons:   bit0=Down, bit1=Up, bit2=Right, bit3=Left, bit6=L, bit7=ZL
    //   bytes 6..9   = left analog stick  (3 bytes packed 12-bit X / 12-bit Y)
    //   bytes 9..12  = right analog stick (same packing)
    //   bytes 13/25/37 = three IMU samples [ax, ay, az, gx, gy, gz] i16 LE each.
    if buf[0] != 0x30 || buf.len() < 49 { return None; }

    let right  = buf[3];
    let shared = buf[4];
    let left   = buf[5];

    let switch_buttons = SwitchProButtons {
        btn_y:       right  & 0x01 != 0,
        btn_x:       right  & 0x02 != 0,
        btn_b:       right  & 0x04 != 0,
        btn_a:       right  & 0x08 != 0,
        btn_r:       right  & 0x40 != 0,
        btn_zr:      right  & 0x80 != 0,
        btn_minus:   shared & 0x01 != 0,
        btn_plus:    shared & 0x02 != 0,
        btn_rstick:  shared & 0x04 != 0,
        btn_lstick:  shared & 0x08 != 0,
        btn_home:    shared & 0x10 != 0,
        btn_capture: shared & 0x20 != 0,
        dpad_down:   left   & 0x01 != 0,
        dpad_up:     left   & 0x02 != 0,
        dpad_right:  left   & 0x04 != 0,
        dpad_left:   left   & 0x08 != 0,
        btn_l:       left   & 0x40 != 0,
        btn_zl:      left   & 0x80 != 0,
        // Stick analog values are calibrated 12-bit; without per-controller calibration we
        // leave these at zero — gilrs's stick path remains the source for stick axes.
        lstick_x: 0.0, lstick_y: 0.0, rstick_x: 0.0, rstick_y: 0.0,
    };

    let (mut ax, mut ay, mut az) = (0i32, 0i32, 0i32);
    let (mut gx, mut gy, mut gz) = (0i32, 0i32, 0i32);
    for s in 0..3usize {
        let o = 13 + s * 12;
        ax += ri16(buf, o)      as i32;
        ay += ri16(buf, o + 2)  as i32;
        az += ri16(buf, o + 4)  as i32;
        gx += ri16(buf, o + 6)  as i32;
        gy += ri16(buf, o + 8)  as i32;
        gz += ri16(buf, o + 10) as i32;
    }
    let gs = SWITCH_GYRO_DPS_PER_LSB / GYRO_REF_DPS;
    let as_ = SWITCH_ACCEL_G_PER_LSB / ACCEL_REF_G;
    Some(HidReading {
        gyro_x:  (gx / 3) as f32 * gs,
        gyro_y: -(gy / 3) as f32 * gs,
        gyro_z: -(gz / 3) as f32 * gs,
        accel_x: (ax / 3) as f32 * as_,
        accel_y: (ay / 3) as f32 * as_,
        accel_z: (az / 3) as f32 * as_,
        switch_buttons: Some(switch_buttons),
        ..HidReading::default()
    })
}

fn build(buf: &[u8], gyro_off: usize, accel_off: usize, gyro_dps_per_lsb: f32, accel_g_per_lsb: f32) -> HidReading {
    let gs  = gyro_dps_per_lsb  / GYRO_REF_DPS;
    let as_ = accel_g_per_lsb   / ACCEL_REF_G;
    // DS4/DualSense raw byte order is (pitch, yaw, roll) — remap to standard (roll, pitch, yaw)
    // so that gyro_x=roll, gyro_y=pitch, gyro_z=yaw matches Switch Pro and the 3DOF module.
    // Accel raw order is (side, vertical, fwd-tilt) — move vertical to z so that accel_z is
    // the gravity axis (≈ +1 when flat face-up), matching Switch Pro's accel_z orientation.
    HidReading {
        gyro_x:  ri16(buf, gyro_off + 4)  as f32 * gs,   // raw[2] roll
        gyro_y:  ri16(buf, gyro_off)      as f32 * gs,   // raw[0] pitch
        gyro_z: -ri16(buf, gyro_off + 2)  as f32 * gs,   // raw[1] yaw, negated: right=positive
        accel_x: ri16(buf, accel_off)     as f32 * as_,  // raw[0] side
        accel_y: ri16(buf, accel_off + 4) as f32 * as_,  // raw[2] fwd-tilt
        accel_z: ri16(buf, accel_off + 2) as f32 * as_,  // raw[1] vertical → z (+1 when flat)
        ..HidReading::default()
    }
}

fn ri16(buf: &[u8], off: usize) -> i16 {
    i16::from_le_bytes([buf[off], buf[off + 1]])
}

// ── Output report builders (USB only — BT requires CRC + extra wrapping) ──────

/// DS4 USB output report 0x05 (32 bytes incl. report ID).
/// Sets rumble (heavy/light motors) and lightbar RGB.
fn build_ds4_usb_out(out: &OutputState) -> [u8; 32] {
    let mut r = [0u8; 32];
    r[0] = 0x05;                // Report ID
    r[1] = 0x07;                // valid: bit0 = rumble, bit1 = lightbar, bit2 = flash
    r[2] = 0x04;                // packet type — must be 0x04
    r[4] = out.rumble_weak;     // small motor (high-freq / right)
    r[5] = out.rumble_strong;   // large motor (low-freq  / left)
    r[6] = out.lightbar_r;
    r[7] = out.lightbar_g;
    r[8] = out.lightbar_b;
    // bytes 9, 10: flash on/off durations — leave at 0 (steady)
    r
}

/// Switch Pro rumble-only output report 0x10 (49 bytes — we send 64 to be safe).
/// `left` / `right` are the encoded 4-byte rumble packets per side.
fn build_switch_rumble_only(counter: u8, left: [u8; 4], right: [u8; 4]) -> [u8; 64] {
    let mut r = [0u8; 64];
    r[0] = 0x10;                  // Report ID
    r[1] = counter & 0x0F;        // Sequence (low nibble)
    r[2..6].copy_from_slice(&left);
    r[6..10].copy_from_slice(&right);
    r
}

/// Encode 0..1 amplitude into a 4-byte Switch Pro rumble sample at default
/// frequencies (LF 160 Hz, HF 320 Hz). The neutral / off packet is
/// `[0x00, 0x01, 0x40, 0x40]` — sending that stops vibration on that side.
///
/// This is an *approximate* linear scale. Real firmware uses a 100-entry
/// non-linear lookup table (drivers/hid/nintendo.c `JC_RUMBLE_AMP_LOOKUP`);
/// the closed-form here trades fidelity for code size and produces a usable,
/// monotonic amplitude curve. If the rumble feels uncalibrated we can swap in
/// the real table later.
fn switch_rumble_encode(amp: f32) -> [u8; 4] {
    let amp = amp.clamp(0.0, 1.0);
    if amp < 0.005 {
        return [0x00, 0x01, 0x40, 0x40];
    }
    // Map 0..1 → 7-bit encoded amp 0..0x7C (kernel's max LUT entry).
    let enc = (amp * 0x7C as f32).round() as u8;
    [
        0x00,                                  // HF freq high (320 Hz default)
        0x01 | (enc & 0x7E),                   // HF freq low | HF amp (6-bit slot)
        0x40 | (enc >> 1),                     // LF freq high | LF amp upper
        0x40 | ((enc & 0x01) << 6),            // LF freq low  | LF amp lsb
    ]
}

/// Encode one DualSense adaptive trigger effect into 11 bytes:
/// byte[0] = mode byte, bytes[1..10] = effect params.
///
/// mode: 0=Off(0x05), 1=Feedback(0x21), 2=Weapon(0x25), 3=Vibration(0x26)
/// start/end: zone 0–9 along trigger travel (0=rest, 9=fully pressed)
/// strength: force 0–7
/// freq: oscillation 0–255 (Vibration mode only)
///
/// Bit-packing derived from MysteriousJ/Joystick-Input-Examples and confirmed
/// against Linux hid-playstation.c trigger effect structs.
fn encode_trigger_effect(mode: u8, start: u8, end: u8, strength: u8, freq: u8) -> [u8; 11] {
    let mut p = [0u8; 11];
    match mode {
        1 => {
            // Feedback (0x21): constant resistance from start zone onwards.
            p[0] = 0x21;
            let s = (start.min(9)) as u16;
            let f = (strength.min(7)) as u32;
            // active_zones: bits s..9 set (10-bit field, LSB-first across 2 bytes)
            let active: u16 = if s < 10 { ((1u16 << (10 - s)) - 1) << s } else { 0 };
            // force_zones: 3-bit force value repeated for each active zone (10 zones × 3 bits)
            let mut force = 0u32;
            for z in s..10 { force |= f << (z * 3); }
            p[1] = (active & 0xFF) as u8;
            p[2] = ((active >> 8) & 0xFF) as u8;
            p[3] = (force & 0xFF) as u8;
            p[4] = ((force >> 8) & 0xFF) as u8;
            p[5] = ((force >> 16) & 0xFF) as u8;
            p[6] = ((force >> 24) & 0xFF) as u8;
        }
        2 => {
            // Weapon (0x25): hard resistance between start and end, then releases.
            p[0] = 0x25;
            let s = (start.clamp(2, 7)) as u16;
            let e = (end.max(start.saturating_add(1)).clamp(3, 8)) as u16;
            let start_end: u16 = (1u16 << s) | (1u16 << e);
            p[1] = (start_end & 0xFF) as u8;
            p[2] = ((start_end >> 8) & 0xFF) as u8;
            p[3] = strength.min(7);
        }
        3 => {
            // Vibration (0x26): oscillating resistance in a zone range.
            p[0] = 0x26;
            let s = (start.min(9)) as u16;
            let f = (strength.min(7)) as u32;
            let active: u16 = if s < 10 { ((1u16 << (10 - s)) - 1) << s } else { 0 };
            let mut force = 0u32;
            for z in s..10 { force |= f << (z * 3); }
            p[1] = (active & 0xFF) as u8;
            p[2] = ((active >> 8) & 0xFF) as u8;
            p[3] = (force & 0xFF) as u8;
            p[4] = ((force >> 8) & 0xFF) as u8;
            p[5] = ((force >> 16) & 0xFF) as u8;
            p[6] = ((force >> 24) & 0xFF) as u8;
            p[9] = freq;
        }
        _ => {
            // Off (0x05) — deactivate any effect.
            p[0] = 0x05;
        }
    }
    p
}

/// Map player_led index (0–4) to the DualSense 5-bit LED bitmask.
fn player_led_mask(idx: u8) -> u8 {
    match idx {
        1 => 0x04,
        2 => 0x0A,
        3 => 0x15,
        4 => 0x1B,
        _ => 0x00, // off
    }
}

/// DualSense USB output report 0x02 (63 bytes incl. report ID).
/// Layout reference: Linux drivers/hid/hid-playstation.c, struct
/// `dualsense_output_report_common` (47 bytes) sits at buffer offset 1.
///
/// Valid-flag values mirror pydualsense / DS4Windows: enable everything except
/// `release LEDs` (which would hand the lightbar back to the system).
fn build_dualsense_usb_out(out: &OutputState) -> [u8; 63] {
    let mut r = [0u8; 63];
    r[0] = 0x02;                // Report ID
    r[1] = 0xFF;                // valid_flag0: rumble + trigger motors + audio enabled
    r[2] = 0xF7;                // valid_flag1: LED paths enabled, bit 3 (release_leds) = 0
    r[3] = out.rumble_weak;     // motor_right — high-freq
    r[4] = out.rumble_strong;   // motor_left  — low-freq
    // Adaptive trigger effects: right at r[11..22], left at r[22..33]
    let rt = encode_trigger_effect(
        out.trigger_r_mode, out.trigger_r_start,
        out.trigger_r_end,  out.trigger_r_strength, out.trigger_r_freq,
    );
    let lt = encode_trigger_effect(
        out.trigger_l_mode, out.trigger_l_start,
        out.trigger_l_end,  out.trigger_l_strength, out.trigger_l_freq,
    );
    r[11..22].copy_from_slice(&rt);
    r[22..33].copy_from_slice(&lt);
    r[39] = 0x07;               // valid_flag2: lightbar setup + compat vibration v2
    r[42] = 0x02;               // lightbar_setup: 0x02 = lightbar_on
    r[43] = 0x02;               // led_brightness: 0x02 = bright
    r[44] = player_led_mask(out.player_led); // player indicator LEDs
    r[45] = out.lightbar_r;     // lightbar_red
    r[46] = out.lightbar_g;     // lightbar_green
    r[47] = out.lightbar_b;     // lightbar_blue
    // Mic mute LED: r[9] = mute_button_led (0=off, 1=on/orange, 2=pulsing)
    r[9]  = out.mic_led.min(2);
    r
}

// ── VID/PID classification ─────────────────────────────────────────────────────

enum KindTag { Ds4, DualSense, SwitchPro }

fn classify(vid: u16, pid: u16) -> Option<KindTag> {
    match vid {
        SONY_VID   if DS4_PIDS.contains(&pid)   => Some(KindTag::Ds4),
        SONY_VID   if pid == DUALSENSE_PID       => Some(KindTag::DualSense),
        SWITCH_VID if pid == SWITCH_PRO_PID      => Some(KindTag::SwitchPro),
        _ => None,
    }
}

// Windows HID interface numbers for the main gamepad interface when usage_page
// fields aren't available (e.g. HidHide intercepts enumeration).
// DualSense/DS4 USB: interface 3 carries input reports with IMU data.
// Switch Pro USB:    interface 0.
// BT connections expose a single interface (0) for all three controllers.
fn preferred_interface(kind: &KindTag) -> i32 {
    match kind {
        KindTag::Ds4 | KindTag::DualSense => 3,
        KindTag::SwitchPro => 0,
    }
}
