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

// Retry open no more than once per N seconds to avoid hammering HidHide.
const RETRY_INTERVAL: Duration = Duration::from_secs(2);

#[derive(Clone, Copy, Default, Debug)]
pub struct GyroReading {
    pub gyro_x: f32,
    pub gyro_y: f32,
    pub gyro_z: f32,
    pub accel_x: f32,
    pub accel_y: f32,
    pub accel_z: f32,
}

enum DeviceKind {
    Ds4,
    DualSense,
    SwitchPro { initialized: bool, packet_counter: u8 },
}

struct HidEntry {
    device: HidDevice,
    kind: DeviceKind,
    last: GyroReading,
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

    /// Returns the latest gyro/accel reading for the Nth physical device with this VID/PID.
    pub fn read(&mut self, vid: u16, pid: u16, idx: usize) -> Option<GyroReading> {
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
            KindTag::DualSense => DeviceKind::DualSense,
            KindTag::SwitchPro => DeviceKind::SwitchPro { initialized: false, packet_counter: 0 },
        };
        Some(HidEntry { device, kind, last: GyroReading::default() })
    }
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
                if let Some(r) = parse_report(&buf[..n], &entry.kind) {
                    entry.last = r;
                }
            }
        }
    }
    true
}

fn parse_report(buf: &[u8], kind: &DeviceKind) -> Option<GyroReading> {
    if buf.is_empty() { return None; }
    match kind {
        DeviceKind::Ds4       => parse_ds4(buf),
        DeviceKind::DualSense => parse_dualsense(buf),
        DeviceKind::SwitchPro { .. } => parse_switch_pro(buf),
    }
}

fn parse_ds4(buf: &[u8]) -> Option<GyroReading> {
    // USB: report 0x01, 64 bytes — gyro at bytes 13-18, accel at 19-24.
    // BT:  report 0x11, 78 bytes — same fields shifted +2.
    let (go, ao) = match buf[0] {
        0x01 if buf.len() >= 25 => (13, 19),
        0x11 if buf.len() >= 77 => (15, 21),
        _ => return None,
    };
    Some(build(buf, go, ao))
}

fn parse_dualsense(buf: &[u8]) -> Option<GyroReading> {
    // USB: report 0x01, 64 bytes — gyro at bytes 15-20, accel at 21-26.
    // BT:  report 0x31, 78 bytes — shifted +2.
    let (go, ao) = match buf[0] {
        0x01 if buf.len() >= 27 => (15, 21),
        0x31 if buf.len() >= 79 => (17, 23),
        _ => return None,
    };
    Some(build(buf, go, ao))
}

fn parse_switch_pro(buf: &[u8]) -> Option<GyroReading> {
    // Report 0x30: 3 IMU samples at bytes 13, 25, 37.
    // Each sample: [accel X, accel Y, accel Z, gyro X, gyro Y, gyro Z] as i16 LE.
    if buf[0] != 0x30 || buf.len() < 49 { return None; }

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
    Some(GyroReading {
        gyro_x:  (gx / 3) as f32 / 32767.0,
        gyro_y:  (gy / 3) as f32 / 32767.0,
        gyro_z:  (gz / 3) as f32 / 32767.0,
        accel_x: (ax / 3) as f32 / 32767.0,
        accel_y: (ay / 3) as f32 / 32767.0,
        accel_z: (az / 3) as f32 / 32767.0,
    })
}

fn build(buf: &[u8], gyro_off: usize, accel_off: usize) -> GyroReading {
    GyroReading {
        gyro_x:  ri16(buf, gyro_off)      as f32 / 32767.0,
        gyro_y:  ri16(buf, gyro_off + 2)  as f32 / 32767.0,
        gyro_z:  ri16(buf, gyro_off + 4)  as f32 / 32767.0,
        accel_x: ri16(buf, accel_off)     as f32 / 32767.0,
        accel_y: ri16(buf, accel_off + 2) as f32 / 32767.0,
        accel_z: ri16(buf, accel_off + 4) as f32 / 32767.0,
    }
}

fn ri16(buf: &[u8], off: usize) -> i16 {
    i16::from_le_bytes([buf[off], buf[off + 1]])
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
