use std::collections::HashMap;
use std::time::{Duration, Instant};
use hidapi::{HidApi, HidDevice};

const SONY_VID: u16           = 0x054C;
const DS4_PIDS: &[u16]       = &[0x05C4, 0x09CC];
const DUALSENSE_PIDS: &[u16] = &[0x0CE6, 0x0DF2]; // standard + DualSense Edge
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

/// Full DualSense input state parsed directly from the raw HID input report.
/// Used to override gilrs, which mis-maps axes on the Windows HID path.
#[derive(Clone, Copy, Default, Debug)]
pub struct DualSenseState {
    // Sticks: normalized [-1, 1]
    pub lx: f32, pub ly: f32,
    pub rx: f32, pub ry: f32,
    // Triggers: normalized [0, 1]
    pub l2: f32, pub r2: f32,
    // Face buttons
    pub btn_south: bool, pub btn_east: bool, pub btn_west: bool, pub btn_north: bool,
    // Shoulder / trigger digital
    pub btn_l1: bool, pub btn_r1: bool,
    pub btn_l2: bool, pub btn_r2: bool,
    // Stick clicks
    pub btn_ls: bool, pub btn_rs: bool,
    // Menu / special
    pub btn_options: bool, pub btn_create: bool, pub btn_ps: bool,
    // DPad
    pub dpad_up: bool, pub dpad_down: bool, pub dpad_left: bool, pub dpad_right: bool,
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
    /// DualSense full input state, parsed from raw HID. None for non-DualSense devices.
    pub dualsense: Option<DualSenseState>,
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
    // Switch Pro HD Rumble — per-side amplitude + frequency (0–255 each, mapped at encode).
    // hd_l/r_amp:  0=silent, 255=max safe; perceptual power-law curve applied at encode.
    // hd_l/r_freq: 0–255 mapped logarithmically over safe range 82–1253 Hz (indices 32–159).
    hd_l_amp:  u8,
    hd_l_freq: u8,
    hd_r_amp:  u8,
    hd_r_freq: u8,
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

        #[cfg(debug_assertions)]
        eprintln!("[gyro] open_device vid={:04X} pid={:04X} idx={} iface={} path={:?}",
            vid, pid, idx,
            paths.get(idx).map_or(-1, |p| p.interface_number()),
            paths.get(idx).map(|p| p.path()));

        let info = paths.get(idx)?;
        let device = match api.open_path(info.path()) {
            Ok(d) => d,
            Err(e) => {
                #[cfg(debug_assertions)]
                eprintln!("[gyro] open_path failed: {e}");
                return None;
            }
        };
        device.set_blocking_mode(false).ok()?;

        // On Windows, DualSense/DS4 LED output (report 0x02) is only processed by
        // the firmware when sent to interface 0 — but that interface is owned
        // exclusively by the Windows HID class driver and cannot be opened from
        // userspace. All LED/lightbar control is therefore unavailable on Windows
        // unless a WinRT (Windows.Gaming.Input) path is implemented in the future.
        // Trigger effects and rumble go to interface 3 (the accessible IMU interface)
        // and the firmware processes those fields on that interface.
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
    pub fn set_output_byte(&mut self, vid: u16, pid: u16, idx: usize, pin_id: &str, byte: u8) {
        let entry = match self.devices.get_mut(&(vid, pid, idx)) {
            Some(e) => e,
            None => return,
        };
        let updated = match pin_id {
            "rumble_strong" => { entry.out.rumble_strong = byte; true }
            "rumble_weak"   => { entry.out.rumble_weak   = byte; true }
            // Legacy amplitude-only HD rumble pins — route to per-side amp field.
            "hd_rumble_l" => { entry.out.hd_l_amp = byte; true }
            "hd_rumble_r" => { entry.out.hd_r_amp = byte; true }
            // Switch Pro HD rumble — amplitude + frequency per side
            "hd_l_amp"  => { entry.out.hd_l_amp  = byte; true }
            "hd_l_freq" => { entry.out.hd_l_freq  = byte; true }
            "hd_r_amp"  => { entry.out.hd_r_amp   = byte; true }
            "hd_r_freq" => { entry.out.hd_r_freq  = byte; true }
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
            "player_led" => { entry.out.player_led = byte; true }
            "mic_led"    => { entry.out.mic_led    = byte; true }
            _ => false,
        };
        if updated { entry.output_active = true; }
    }

    /// Stage a signed float output value for AC rumble pins (hd_l_ac / hd_r_ac).
    /// Send pending output reports for every device that has been driven at
    /// least once. Call once per frame.
    pub fn flush_outputs(&mut self) {
        for entry in self.devices.values_mut() {
            if !entry.output_active { continue; }
            let HidEntry { device, kind, out, .. } = entry;
            match kind {
                DeviceKind::Ds4 => {
                    hid_write(device, &build_ds4_usb_out(out));
                }
                DeviceKind::DualSense { connection } => {
                    if matches!(connection, Some(Connection::Bt)) { continue; }
                    hid_write(device, &build_dualsense_usb_out(out));
                }
                DeviceKind::SwitchPro { initialized, packet_counter } => {
                    if !*initialized { continue; }
                    let left  = switch_rumble_encode(out.hd_l_amp as f32 / 255.0, out.hd_l_freq as f32 / 255.0);
                    let right = switch_rumble_encode(out.hd_r_amp as f32 / 255.0, out.hd_r_freq as f32 / 255.0);
                    let pkt = build_switch_rumble_only(*packet_counter, left, right);
                    *packet_counter = packet_counter.wrapping_add(1);
                    hid_write(device, &pkt);
                }
            }
        }
    }
}

fn hid_write(device: &HidDevice, data: &[u8]) {
    let res = device.write(data);
    #[cfg(debug_assertions)]
    if let Err(e) = &res {
        eprintln!("[hid-write] failed ({} bytes): {:?}", data.len(), e);
    }
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
    // Payload offsets (relative to payload base po):
    //   lx(0) ly(1) rx(2) ry(3) l2(4) r2(5) seq(6)
    //   buttons[0](7): dpad(3:0) square(4) cross(5) circle(6) triangle(7)
    //   buttons[1](8): l1(0) r1(1) l2_dig(2) r2_dig(3) create(4) options(5) l3(6) r3(7)
    //   buttons[2](9): ps(0) touchpad(1) mute(2)
    //   reserved[4](11-14) gyro[3](15-20) accel[3](21-26)
    //   timestamp(27-30) reserved2(31) touch[2](32-39)
    // USB: report 0x01, payload base po=1. BT: report 0x31, payload base po=2.
    let (conn, po, go, ao, t1, t2) = match buf[0] {
        0x01 if buf.len() >= 41 => (Connection::Usb, 1usize, 16, 22, 33, 37),
        0x31 if buf.len() >= 79 => (Connection::Bt,  2usize, 17, 23, 34, 38),
        _ => return None,
    };
    *connection = Some(conn);

    let btn0 = buf[po + 7];
    let btn1 = buf[po + 8];
    let btn2 = buf[po + 9];

    // D-Pad: low nibble of buttons[0], 0-7 clockwise from north, 8=neutral.
    let dpad = btn0 & 0x0F;

    let ds = DualSenseState {
        lx:  (buf[po]     as f32 - 128.0) / 128.0,
        ly: -(buf[po + 1] as f32 - 128.0) / 128.0, // HID Y increases downward; invert to +Y=up
        rx:  (buf[po + 2] as f32 - 128.0) / 128.0,
        ry: -(buf[po + 3] as f32 - 128.0) / 128.0, // same
        l2: buf[po + 4] as f32 / 255.0,
        r2: buf[po + 5] as f32 / 255.0,
        btn_west:    btn0 & 0x10 != 0, // Square
        btn_south:   btn0 & 0x20 != 0, // Cross
        btn_east:    btn0 & 0x40 != 0, // Circle
        btn_north:   btn0 & 0x80 != 0, // Triangle
        btn_l1:      btn1 & 0x01 != 0,
        btn_r1:      btn1 & 0x02 != 0,
        btn_l2:      btn1 & 0x04 != 0,
        btn_r2:      btn1 & 0x08 != 0,
        btn_create:  btn1 & 0x10 != 0,
        btn_options: btn1 & 0x20 != 0,
        btn_ls:      btn1 & 0x40 != 0,
        btn_rs:      btn1 & 0x80 != 0,
        btn_ps:      btn2 & 0x01 != 0,
        dpad_up:    matches!(dpad, 0 | 1 | 7),
        dpad_right: matches!(dpad, 1 | 2 | 3),
        dpad_down:  matches!(dpad, 3 | 4 | 5),
        dpad_left:  matches!(dpad, 5 | 6 | 7),
    };

    let mut r = build(buf, go, ao, DS4_GYRO_DPS_PER_LSB, DS4_ACCEL_G_PER_LSB);
    r.has_touchpad    = true;
    r.touch1          = parse_dualsense_touch(buf, t1);
    r.touch2          = parse_dualsense_touch(buf, t2);
    r.touchpad_click  = btn2 & 0x02 != 0;
    r.mic_button      = btn2 & 0x04 != 0;
    r.dualsense       = Some(ds);
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
        x:  (raw_x as f32 - HALF_W) / HALF_W,
        y: -(raw_y as f32 - HALF_H) / HALF_H, // touchpad Y increases downward; invert to +Y=up
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

/// Encode amplitude + frequency into a 4-byte Switch Pro HD rumble packet.
///
///   amp  — vibration intensity 0.0–1.0 (0 = silent, 1 = max safe ~1003 units)
///   freq — normalized 0.0–1.0 mapping to 41–1253 Hz
///          (0.0=41 Hz, ~0.6=320 Hz default, 1.0=1253 Hz)
///
/// The kernel uses one shared frequency table (joycon_rumble_frequencies[]) for
/// both the HF field (bytes 0–1, u16 `high`) and the LF field (byte 2, u8 `low`).
/// A single freq input drives both bands simultaneously, which is how the Switch
/// OS and kernel driver work by default.
///
/// Encoding from Linux drivers/hid/nintendo.c joycon_encode_rumble():
///   data[0] = (freq.high >> 8) & 0xFF
///   data[1] = (freq.high & 0xFF) + amp.high
///   data[2] =  freq.low          + ((amp.low >> 8) & 0xFF)
///   data[3] =  amp.low & 0xFF
fn switch_rumble_encode(amp: f32, freq: f32) -> [u8; 4] {
    // joycon_rumble_frequencies[] from drivers/hid/hid-nintendo.c.
    // Each entry: (high: u16, low: u8) for the given Hz.
    // 160 entries, 41 Hz (index 0) → 1253 Hz (index 159).
    // Index 96 = 320 Hz (firmware default).
    #[rustfmt::skip]
    const FREQ_TABLE: &[(u16, u8)] = &[
        (0x0000,0x01),(0x0000,0x02),(0x0000,0x03),(0x0000,0x04),(0x0000,0x05),
        (0x0000,0x06),(0x0000,0x07),(0x0000,0x08),(0x0000,0x09),(0x0000,0x0a),
        (0x0000,0x0b),(0x0000,0x0c),(0x0000,0x0d),(0x0000,0x0e),(0x0000,0x0f),
        (0x0000,0x10),(0x0000,0x11),(0x0000,0x12),(0x0000,0x13),(0x0000,0x14),
        (0x0000,0x15),(0x0000,0x16),(0x0000,0x17),(0x0000,0x18),(0x0000,0x19),
        (0x0000,0x1a),(0x0000,0x1b),(0x0000,0x1c),(0x0000,0x1d),(0x0000,0x1e),
        (0x0000,0x1f),(0x0000,0x20),(0x0400,0x21),(0x0800,0x22),(0x0c00,0x23),
        (0x1000,0x24),(0x1400,0x25),(0x1800,0x26),(0x1c00,0x27),(0x2000,0x28),
        (0x2400,0x29),(0x2800,0x2a),(0x2c00,0x2b),(0x3000,0x2c),(0x3400,0x2d),
        (0x3800,0x2e),(0x3c00,0x2f),(0x4000,0x30),(0x4400,0x31),(0x4800,0x32),
        (0x4c00,0x33),(0x5000,0x34),(0x5400,0x35),(0x5800,0x36),(0x5c00,0x37),
        (0x6000,0x38),(0x6400,0x39),(0x6800,0x3a),(0x6c00,0x3b),(0x7000,0x3c),
        (0x7400,0x3d),(0x7800,0x3e),(0x7c00,0x3f),(0x8000,0x40),(0x8400,0x41),
        (0x8800,0x42),(0x8c00,0x43),(0x9000,0x44),(0x9400,0x45),(0x9800,0x46),
        (0x9c00,0x47),(0xa000,0x48),(0xa400,0x49),(0xa800,0x4a),(0xac00,0x4b),
        (0xb000,0x4c),(0xb400,0x4d),(0xb800,0x4e),(0xbc00,0x4f),(0xc000,0x50),
        (0xc400,0x51),(0xc800,0x52),(0xcc00,0x53),(0xd000,0x54),(0xd400,0x55),
        (0xd800,0x56),(0xdc00,0x57),(0xe000,0x58),(0xe400,0x59),(0xe800,0x5a),
        (0xec00,0x5b),(0xf000,0x5c),(0xf400,0x5d),(0xf800,0x5e),(0xfc00,0x5f),
        // index 96 = 320 Hz
        (0x0001,0x60),(0x0401,0x61),(0x0801,0x62),(0x0c01,0x63),(0x1001,0x64),
        (0x1401,0x65),(0x1801,0x66),(0x1c01,0x67),(0x2001,0x68),(0x2401,0x69),
        (0x2801,0x6a),(0x2c01,0x6b),(0x3001,0x6c),(0x3401,0x6d),(0x3801,0x6e),
        (0x3c01,0x6f),(0x4001,0x70),(0x4401,0x71),(0x4801,0x72),(0x4c01,0x73),
        (0x5001,0x74),(0x5401,0x75),(0x5801,0x76),(0x5c01,0x77),(0x6001,0x78),
        (0x6401,0x79),(0x6801,0x7a),(0x6c01,0x7b),(0x7001,0x7c),(0x7401,0x7d),
        (0x7801,0x7e),(0x7c01,0x7f),
        // index 128 = 640 Hz, low = 0x00 for these (above LF range)
        (0x8001,0x00),(0x8401,0x00),(0x8801,0x00),(0x8c01,0x00),(0x9001,0x00),
        (0x9401,0x00),(0x9801,0x00),(0x9c01,0x00),(0xa001,0x00),(0xa401,0x00),
        (0xa801,0x00),(0xac01,0x00),(0xb001,0x00),(0xb401,0x00),(0xb801,0x00),
        (0xbc01,0x00),(0xc001,0x00),(0xc401,0x00),(0xc801,0x00),(0xcc01,0x00),
        (0xd001,0x00),(0xd401,0x00),(0xd801,0x00),(0xdc01,0x00),(0xe001,0x00),
        (0xe401,0x00),(0xe801,0x00),(0xec01,0x00),(0xf001,0x00),(0xf401,0x00),
        (0xf801,0x00),(0xfc01,0x00), // index 159 = 1253 Hz
    ];
    // joycon_rumble_amplitudes[] safe range (0–1003 units) from drivers/hid/hid-nintendo.c.
    // (high: u8, low: u16) — added into frequency bytes per joycon_encode_rumble().
    // 101 entries: index 0 = silent (0 units), index 100 = max safe (1003 units).
    #[rustfmt::skip]
    const AMP_TABLE: &[(u8, u16)] = &[
        (0x00,0x0040),
        (0x02,0x8040),(0x04,0x0041),(0x06,0x8041),(0x08,0x0042),(0x0a,0x8042),
        (0x0c,0x0043),(0x0e,0x8043),(0x10,0x0044),(0x12,0x8044),(0x14,0x0045),
        (0x16,0x8045),(0x18,0x0046),(0x1a,0x8046),(0x1c,0x0047),(0x1e,0x8047),
        (0x20,0x0048),(0x22,0x8048),(0x24,0x0049),(0x26,0x8049),(0x28,0x004a),
        (0x2a,0x804a),(0x2c,0x004b),(0x2e,0x804b),(0x30,0x004c),(0x32,0x804c),
        (0x34,0x004d),(0x36,0x804d),(0x38,0x004e),(0x3a,0x804e),(0x3c,0x004f),
        (0x3e,0x804f),(0x40,0x0050),(0x42,0x8050),(0x44,0x0051),(0x46,0x8051),
        (0x48,0x0052),(0x4a,0x8052),(0x4c,0x0053),(0x4e,0x8053),(0x50,0x0054),
        (0x52,0x8054),(0x54,0x0055),(0x56,0x8055),(0x58,0x0056),(0x5a,0x8056),
        (0x5c,0x0057),(0x5e,0x8057),(0x60,0x0058),(0x62,0x8058),(0x64,0x0059),
        (0x66,0x8059),(0x68,0x005a),(0x6a,0x805a),(0x6c,0x005b),(0x6e,0x805b),
        (0x70,0x005c),(0x72,0x805c),(0x74,0x005d),(0x76,0x805d),(0x78,0x005e),
        (0x7a,0x805e),(0x7c,0x005f),(0x7e,0x805f),(0x80,0x0060),(0x82,0x8060),
        (0x84,0x0061),(0x86,0x8061),(0x88,0x0062),(0x8a,0x8062),(0x8c,0x0063),
        (0x8e,0x8063),(0x90,0x0064),(0x92,0x8064),(0x94,0x0065),(0x96,0x8065),
        (0x98,0x0066),(0x9a,0x8066),(0x9c,0x0067),(0x9e,0x8067),(0xa0,0x0068),
        (0xa2,0x8068),(0xa4,0x0069),(0xa6,0x8069),(0xa8,0x006a),(0xaa,0x806a),
        (0xac,0x006b),(0xae,0x806b),(0xb0,0x006c),(0xb2,0x806c),(0xb4,0x006d),
        (0xb6,0x806d),(0xb8,0x006e),(0xba,0x806e),(0xbc,0x006f),(0xbe,0x806f),
        (0xc0,0x0070),(0xc2,0x8070),(0xc4,0x0071),(0xc6,0x8071),(0xc8,0x0072),
    ];

    // FREQ_TABLE has 159 entries (valid indices 0–158).
    // Safe range: indices 32–127 (~82–626 Hz) — these have both HF and LF fields non-zero.
    // Indices 128–158 have fl=0x00 (above LF range); capped at 127 to avoid firmware glitches.
    // The fh field is NOT a linear numeric encoding — it wraps at index 95→96 — so we must
    // use it as an opaque lookup key and never interpolate between entries.
    const FREQ_LO: usize = 32;  // ~82 Hz
    const FREQ_HI: usize = 127; // ~626 Hz
    let f_idx = ((FREQ_LO as f32 + freq.clamp(0.0, 1.0) * (FREQ_HI - FREQ_LO) as f32)
        .round() as usize)
        .clamp(FREQ_LO, FREQ_HI);
    let (fh, fl) = FREQ_TABLE[f_idx];

    // Perceptual amplitude: input 0–1 → power-law curve (exponent 1.8).
    // Skips index 0 (silence) for any non-zero input so amp responds from the very start.
    let amp_c = amp.clamp(0.0, 1.0);
    let a_idx = if amp_c == 0.0 {
        0
    } else {
        let linear = amp_c.powf(1.8);
        (1 + (linear * (AMP_TABLE.len() - 2) as f32).round() as usize).min(AMP_TABLE.len() - 1)
    };

    let (ah, al) = AMP_TABLE[a_idx];
    [
        ((fh >> 8) & 0xFF) as u8,
        ((fh & 0xFF) as u8).wrapping_add(ah),
        fl.wrapping_add(((al >> 8) & 0xFF) as u8),
        (al & 0xFF) as u8,
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
/// One-shot lightbar init report: sends LIGHTBAR_SETUP=LIGHT_ON so the firmware
/// DualSense USB output report 0x02.
fn build_dualsense_usb_out(out: &OutputState) -> [u8; 63] {
    // Layout: Linux hid-playstation.c dualsense_output_report_common (47 bytes, __packed).
    // Report ID 0x02 at buf[0]; common struct starts at buf[1] (struct offset + 1).
    //
    // buf[ 0] = 0x02  report_id
    // buf[ 1] = valid_flag0  bit0=compatible_vibration, bit1=haptics_select
    // buf[ 2] = valid_flag1  bit0=mic_mute_led, bit1=power_save, bit2=lightbar,
    //                        bit3=RELEASE_LEDS (must be 0!), bit4=player_indicator
    // buf[ 3] = motor_right  (rumble weak)
    // buf[ 4] = motor_left   (rumble strong)
    // buf[ 5-8]  audio (leave 0)
    // buf[ 9] = mute_button_led
    // buf[10] = power_save_control
    // buf[11-37] = reserved2[27] — trigger effects placed here (non-kernel extension)
    // buf[38] = audio_control2
    // buf[39] = valid_flag2   bit1=lightbar_setup_control_enable, bit2=compat_vibration2
    // buf[40-41] = reserved3[2]
    // buf[42] = lightbar_setup  (0x02 = lightbar_on)
    // buf[43] = led_brightness  (0x02 = bright)
    // buf[44] = player_leds bitmask
    // buf[45] = lightbar_red
    // buf[46] = lightbar_green
    // buf[47] = lightbar_blue
    // buf[48-62] = padding
    // Offsets match DualSense-Windows DS5_Output.cpp (their buffer has no report ID,
    // so their 0x00 = our r[1], their 0x2C = our r[45], etc.)
    let mut r = [0u8; 63];
    r[0] = 0x02;
    r[1] = 0xFF;  // valid_flag0
    r[2] = 0xF7;  // valid_flag1 (matches DualSense-Windows exactly)
    r[3] = out.rumble_weak;
    r[4] = out.rumble_strong;
    r[9] = out.mic_led.min(2);
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
    r[39] = 0x03; // valid_flag2: matches DualSense-Windows exactly (their 0x26=0x03)
    r[42] = 0x02; // lightbar_setup (their 0x29)
    r[43] = 0x00; // led_brightness: 0 = firmware default
    r[44] = player_led_mask(out.player_led);
    r[45] = out.lightbar_r;
    r[46] = out.lightbar_g;
    r[47] = out.lightbar_b;
    r
}

// ── VID/PID classification ─────────────────────────────────────────────────────

enum KindTag { Ds4, DualSense, SwitchPro }

fn classify(vid: u16, pid: u16) -> Option<KindTag> {
    match vid {
        SONY_VID   if DS4_PIDS.contains(&pid)      => Some(KindTag::Ds4),
        SONY_VID   if DUALSENSE_PIDS.contains(&pid) => Some(KindTag::DualSense),
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
