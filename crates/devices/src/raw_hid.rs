//! The HID tier of a raw joystick: everything its report descriptor declares
//! that SDL's DirectInput driver doesn't read.
//!
//! SDL reads a DirectInput pad's X/Y/Z/Rx/Ry/Rz, two sliders, its buttons and
//! its hats — and silently drops every other field: vendor-defined data, a
//! sensor page's accelerometer and gyro, Generic Desktop velocities, extra
//! controls on other usage pages. A raw pad (`sdl:raw:`) is opened a second
//! time here, on the same HID collection SDL reads (SDL hands over its path),
//! and its input reports are decoded against its descriptor:
//!
//! * **Declared fields** SDL doesn't cover are published on raw pins
//!   (`raw:f…`, a [`RawSrc::Field`]) next to SDL's axes and buttons, so Learn
//!   sees them like any other element. Their usage names travel in the
//!   [catalog](hid_info).
//! * **Vendor blobs** — runs of opaque bytes, where many pads hide a motion
//!   sensor or extra buttons — are too many to publish byte by byte. Their
//!   latest bytes are shared through the catalog instead ([`latest_reports`]),
//!   and the UI probes them: Learn diffs them for a flipped bit or a swung
//!   byte, motion setup looks for jittering 16-bit words.
//!
//! Windows hands every open handle of a HID collection its own copy of each
//! input report, so reading here takes nothing from SDL.

use std::collections::HashMap;
use std::ffi::CString;
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};

use flexinput_core::raw_pad::{HidField, RawSrc};
use flexinput_core::Signal;

/// What a field is, when its usage says it is part of a motion sensor.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MotionHint {
    Accel,
    Gyro,
}

/// One declared input field.
#[derive(Clone, Debug, PartialEq)]
pub struct DescField {
    pub field: HidField,
    pub page: u16,
    pub usage: u16,
}

/// A raw pad's HID tier, as the UI sees it.
#[derive(Clone, Debug, Default)]
pub struct RawHidInfo {
    /// Fields published on raw pins, with their names.
    pub fields: Vec<(RawSrc, String, Option<MotionHint>)>,
    /// Vendor blobs: `(report, first byte, length)` within the payload.
    pub blobs: Vec<(u8, u16, u16)>,
}

impl RawHidInfo {
    pub fn label_of(&self, src: RawSrc) -> Option<&str> {
        self.fields.iter().find(|(s, _, _)| *s == src).map(|(_, l, _)| l.as_str())
    }
}

static CATALOG: Mutex<Option<HashMap<String, Arc<RawHidInfo>>>> = Mutex::new(None);
static LATEST: Mutex<Option<HashMap<String, HashMap<u8, Vec<u8>>>>> = Mutex::new(None);

/// The HID tier of a connected raw pad; `None` when it has none (no HID path,
/// the open failed, or it is still opening).
pub fn hid_info(dev_id: &str) -> Option<Arc<RawHidInfo>> {
    CATALOG.lock().ok()?.as_ref()?.get(dev_id).cloned()
}

/// The latest payload of each of a raw pad's input reports, by report id —
/// what Learn and motion setup probe vendor blobs with.
pub fn latest_reports(dev_id: &str) -> Option<HashMap<u8, Vec<u8>>> {
    LATEST.lock().ok()?.as_ref()?.get(dev_id).cloned()
}

// ── Descriptor ────────────────────────────────────────────────────────────────

/// The variable, non-constant INPUT fields a report descriptor declares, and
/// whether its reports are numbered (carry an id byte).
pub fn parse_input_fields(desc: &[u8]) -> (Vec<DescField>, bool) {
    #[derive(Clone, Copy, Default)]
    struct Globals {
        page: u16,
        min: i32,
        max: i32,
        size: u32,
        count: u32,
        report: u8,
    }
    let mut g = Globals::default();
    let mut stack: Vec<Globals> = Vec::new();
    // Locals: explicit usages, or a min..max range.
    let mut usages: Vec<(u16, u16)> = Vec::new();
    let mut umin: Option<(Option<u16>, u16)> = None;
    let mut umax: Option<u16> = None;
    let mut offsets: HashMap<u8, u32> = HashMap::new();
    let mut numbered = false;
    let mut out = Vec::new();

    let mut i = 0usize;
    while i < desc.len() {
        let prefix = desc[i];
        if prefix == 0xFE {
            // Long item: size byte, tag byte, data. Never used by real devices.
            let len = desc.get(i + 1).copied().unwrap_or(0) as usize;
            i += 3 + len;
            continue;
        }
        let len = match prefix & 3 { 0 => 0, 1 => 1, 2 => 2, _ => 4 };
        let Some(data) = desc.get(i + 1..i + 1 + len) else { break };
        i += 1 + len;
        let mut u: u32 = 0;
        for (k, b) in data.iter().enumerate() {
            u |= (*b as u32) << (8 * k);
        }
        let s: i32 = match len {
            1 => u as u8 as i8 as i32,
            2 => u as u16 as i16 as i32,
            4 => u as i32,
            _ => 0,
        };
        let (typ, tag) = ((prefix >> 2) & 3, prefix >> 4);
        match (typ, tag) {
            // ── Main ──
            (0, 8) => {
                // Input.
                let constant = u & 1 != 0;
                let variable = u & 2 != 0;
                let off = offsets.entry(g.report).or_insert(0);
                for n in 0..g.count {
                    let at = *off + n * g.size;
                    if constant || !variable || g.size == 0 || g.size > 32 || at > u16::MAX as u32 {
                        continue;
                    }
                    let (page, usage) = if !usages.is_empty() {
                        usages[(n as usize).min(usages.len() - 1)]
                    } else if let Some((p, lo)) = umin {
                        let hi = umax.unwrap_or(lo);
                        (p.unwrap_or(g.page), lo.saturating_add(n as u16).min(hi))
                    } else {
                        (g.page, 0)
                    };
                    out.push(DescField {
                        field: HidField { report: g.report, bit: at as u16, size: g.size as u8, min: g.min, max: g.max },
                        page,
                        usage,
                    });
                }
                *off += g.count * g.size;
                usages.clear();
                umin = None;
                umax = None;
            }
            (0, 9) | (0, 10) | (0, 11) | (0, 12) => {
                // Output, Collection, Feature, End Collection: locals end here.
                usages.clear();
                umin = None;
                umax = None;
            }
            // ── Global ──
            (1, 0) => g.page = u as u16,
            (1, 1) => g.min = s,
            (1, 2) => {
                // Signed per spec, but a non-negative minimum with a "negative"
                // maximum means the descriptor wrote it unsigned (0..255 as 0xFF).
                g.max = if g.min >= 0 && s < g.min { u.min(i32::MAX as u32) as i32 } else { s };
            }
            (1, 7) => g.size = u,
            (1, 8) => {
                g.report = u as u8;
                numbered = true;
            }
            (1, 9) => g.count = u,
            (1, 10) => stack.push(g),
            (1, 11) => {
                if let Some(p) = stack.pop() {
                    g = p;
                }
            }
            // ── Local ── (a 4-byte usage carries its own page in the top half)
            (2, 0) => usages.push(if len == 4 { ((u >> 16) as u16, u as u16) } else { (g.page, u as u16) }),
            (2, 1) => umin = Some((if len == 4 { Some((u >> 16) as u16) } else { None }, u as u16)),
            (2, 2) => umax = Some(u as u16),
            _ => {}
        }
    }
    (out, numbered)
}

/// What SDL's DirectInput driver already reads: the Generic Desktop axes,
/// sliders and hat, the Button page, and the Simulation controls DirectInput
/// maps onto its axes. Everything else is this tier's.
pub fn sdl_reads(page: u16, usage: u16) -> bool {
    match page {
        0x01 => (0x30..=0x39).contains(&usage),
        0x09 => true,
        0x02 => matches!(usage, 0xBA | 0xBB | 0xC4 | 0xC5 | 0xC8),
        _ => false,
    }
}

pub fn is_vendor_page(page: u16) -> bool {
    page >= 0xFF00 || page == 0
}

pub fn motion_hint(page: u16, usage: u16) -> Option<MotionHint> {
    match (page, usage) {
        // Sensor page data fields (the top nibble is a modifier).
        (0x20, u) if matches!(u & 0x0FFF, 0x0453..=0x0455) => Some(MotionHint::Accel),
        (0x20, u) if matches!(u & 0x0FFF, 0x0457..=0x0459) => Some(MotionHint::Gyro),
        // Generic Desktop rotational velocity.
        (0x01, 0x43..=0x45) => Some(MotionHint::Gyro),
        _ => None,
    }
}

/// A field's usage name, where the usage is one people would recognise.
pub fn usage_name(page: u16, usage: u16) -> Option<&'static str> {
    Some(match (page, usage) {
        (0x01, 0x3D) => "Start",
        (0x01, 0x3E) => "Select",
        (0x01, 0x40) => "Velocity X",
        (0x01, 0x41) => "Velocity Y",
        (0x01, 0x42) => "Velocity Z",
        (0x01, 0x43) => "Rotation rate X",
        (0x01, 0x44) => "Rotation rate Y",
        (0x01, 0x45) => "Rotation rate Z",
        (0x01, 0x85) => "System Main Menu",
        (0x01, 0x90) => "D-pad Up",
        (0x01, 0x91) => "D-pad Down",
        (0x01, 0x92) => "D-pad Right",
        (0x01, 0x93) => "D-pad Left",
        (0x02, 0xB0) => "Aileron",
        (0x02, 0xB2) => "Collective",
        (0x02, 0xB8) => "Elevator",
        (0x02, 0xBA) => "Rudder",
        (0x02, 0xBB) => "Throttle",
        (0x02, 0xC4) => "Accelerator",
        (0x02, 0xC5) => "Brake",
        (0x02, 0xC6) => "Clutch",
        (0x02, 0xC8) => "Steering",
        (0x05, 0x20) => "Point of View",
        (0x05, 0x21) => "Turn Right/Left",
        (0x05, 0x22) => "Pitch Forward/Backward",
        (0x05, 0x23) => "Roll Right/Left",
        (0x05, 0x24) => "Move Right/Left",
        (0x05, 0x25) => "Move Forward/Backward",
        (0x05, 0x26) => "Move Up/Down",
        (0x05, 0x27) => "Lean Right/Left",
        (0x05, 0x28) => "Lean Forward/Backward",
        (0x05, 0x37) => "Gamepad Fire/Jump",
        (0x05, 0x39) => "Gamepad Trigger",
        (0x0C, 0xE2) => "Mute",
        (0x0C, 0xE9) => "Volume Up",
        (0x0C, 0xEA) => "Volume Down",
        (0x0C, 0xCD) => "Play/Pause",
        (0x0C, 0x223) => "Home",
        (0x0C, 0x224) => "Back",
        (0x20, u) => match u & 0x0FFF {
            0x0453 => "Accelerometer X",
            0x0454 => "Accelerometer Y",
            0x0455 => "Accelerometer Z",
            0x0457 => "Gyro X",
            0x0458 => "Gyro Y",
            0x0459 => "Gyro Z",
            _ => return None,
        },
        _ => return None,
    })
}

/// Fewer than this many opaque bytes in a row are published as fields of
/// their own; this many or more are a blob, probed by the UI instead.
const BLOB_MIN_BYTES: usize = 4;
/// A sanity cap on fields published per poll.
const MAX_FIELDS: usize = 64;

/// Split a descriptor's fields into what this tier publishes and the vendor
/// blobs it leaves to probing.
pub fn classify(fields: &[DescField]) -> RawHidInfo {
    let mut info = RawHidInfo::default();
    let candidates: Vec<&DescField> = fields.iter().filter(|f| !sdl_reads(f.page, f.usage)).collect();
    // Runs of opaque bytes: vendor-page, byte-wide, byte-aligned and back to back.
    let mut in_blob = vec![false; candidates.len()];
    let mut i = 0;
    while i < candidates.len() {
        let opaque = |f: &DescField| is_vendor_page(f.page) && f.field.size == 8 && f.field.bit % 8 == 0;
        if !opaque(candidates[i]) {
            i += 1;
            continue;
        }
        let mut j = i + 1;
        while j < candidates.len()
            && opaque(candidates[j])
            && candidates[j].field.report == candidates[i].field.report
            && candidates[j].field.bit == candidates[j - 1].field.bit + 8
        {
            j += 1;
        }
        if j - i >= BLOB_MIN_BYTES {
            let f = &candidates[i].field;
            info.blobs.push((f.report, f.bit / 8, (j - i) as u16));
            in_blob[i..j].iter_mut().for_each(|b| *b = true);
        }
        i = j;
    }
    for (f, blob) in candidates.iter().zip(in_blob) {
        if blob || info.fields.len() >= MAX_FIELDS {
            continue;
        }
        let src = RawSrc::Field(f.field);
        let label = match usage_name(f.page, f.usage) {
            Some(n) => n.to_string(),
            None if is_vendor_page(f.page) => format!("Vendor {:04X}:{:02X} · {}", f.page, f.usage, src.label()),
            None => format!("Usage {:04X}:{:02X} · {}", f.page, f.usage, src.label()),
        };
        info.fields.push((src, label, motion_hint(f.page, f.usage)));
    }
    info
}

// ── Reading ───────────────────────────────────────────────────────────────────

/// The UI samples a few dozen times a second; sharing every report at the
/// device's own rate would only cost locks.
const SHARE_EVERY: Duration = Duration::from_millis(5);

/// A tier that was streaming and has heard nothing for this long has
/// probably been cut off under its open handle (HidHide cloaking a device
/// already open makes reads return nothing, without an error) — see
/// [`HidTier::stalled`].
const STALL_REOPEN: Duration = Duration::from_secs(5);
/// Reports after which a tier counts as streaming rather than speaking only
/// on change.
const STREAMING_REPORTS: u32 = 200;

/// A raw pad's open HID handle and its latest reports.
pub(crate) struct HidTier {
    dev: hidapi::HidDevice,
    dev_id: String,
    numbered: bool,
    /// Report id → latest payload.
    pub reports: HashMap<u8, Vec<u8>>,
    /// The published fields' raw pins, built once.
    pins: Vec<(RawSrc, String)>,
    buf: Vec<u8>,
    last_share: Instant,
    unshared: bool,
    failed: bool,
    received: u32,
    last_report: Instant,
}

impl HidTier {
    /// The tier went quiet after streaming, or its reads failed: the backend
    /// drops it and opens it again (a reopen lands with any cloak already in
    /// place, which FlexInput is whitelisted through). A pad that only
    /// reports on change can trip this after a busy spell; a needless reopen
    /// costs a few milliseconds on the opener thread.
    pub fn stalled(&self) -> bool {
        self.failed || (self.received >= STREAMING_REPORTS && self.last_report.elapsed() >= STALL_REOPEN)
    }

    /// Drain every report waiting on the handle (non-blocking) and share the
    /// latest with the UI now and then.
    pub fn drain(&mut self) {
        if self.failed {
            return;
        }
        for _ in 0..64 {
            match self.dev.read(&mut self.buf) {
                Ok(0) => break,
                Ok(n) => {
                    let data = &self.buf[..n];
                    let (id, payload) = if self.numbered {
                        match data.split_first() {
                            Some((id, rest)) => (*id, rest),
                            None => continue,
                        }
                    } else {
                        (0, data)
                    };
                    let slot = self.reports.entry(id).or_default();
                    slot.clear();
                    slot.extend_from_slice(payload);
                    self.unshared = true;
                    self.received = self.received.saturating_add(1);
                    self.last_report = Instant::now();
                }
                Err(e) => {
                    eprintln!("[raw-hid] {}: read failed ({e}) — reopening", self.dev_id);
                    self.failed = true;
                    break;
                }
            }
        }
        if self.unshared && self.last_share.elapsed() >= SHARE_EVERY {
            if let Ok(mut g) = LATEST.lock() {
                g.get_or_insert_with(HashMap::new).insert(self.dev_id.clone(), self.reports.clone());
            }
            self.last_share = Instant::now();
            self.unshared = false;
        }
    }

    /// Publish every declared field this tier reads, on its raw pin.
    pub fn publish(&self, out: &mut Vec<(String, String, Signal)>) {
        for (src, pin) in &self.pins {
            let RawSrc::Field(f) = src else { continue };
            let Some(v) = self.reports.get(&f.report).and_then(|p| f.value(p)) else { continue };
            out.push((self.dev_id.clone(), pin.clone(), v.signal()));
        }
    }
}

impl Drop for HidTier {
    fn drop(&mut self) {
        if let Ok(mut g) = CATALOG.lock() {
            if let Some(m) = g.as_mut() {
                m.remove(&self.dev_id);
            }
        }
        if let Ok(mut g) = LATEST.lock() {
            if let Some(m) = g.as_mut() {
                m.remove(&self.dev_id);
            }
        }
    }
}

/// An opened HID tier, before the backend takes it.
pub(crate) struct Opened {
    dev: hidapi::HidDevice,
    numbered: bool,
    info: RawHidInfo,
}

impl Opened {
    /// Hand the tier to the pad it was opened for, publishing its catalog.
    pub fn into_tier(self, dev_id: &str) -> HidTier {
        let pins = self.info.fields.iter().map(|(s, _, _)| (*s, s.pin_id())).collect();
        let info = Arc::new(self.info);
        if let Ok(mut g) = CATALOG.lock() {
            g.get_or_insert_with(HashMap::new).insert(dev_id.to_string(), info);
        }
        HidTier {
            dev: self.dev,
            dev_id: dev_id.to_string(),
            numbered: self.numbered,
            reports: HashMap::new(),
            pins,
            buf: vec![0u8; 1024],
            last_share: Instant::now(),
            unshared: false,
            failed: false,
            received: 0,
            last_report: Instant::now(),
        }
    }
}

fn open(api: &hidapi::HidApi, path: &CString) -> Result<Opened, String> {
    let dev = api.open_path(path).map_err(|e| format!("open failed: {e}"))?;
    let mut desc = vec![0u8; 4096];
    let n = dev.get_report_descriptor(&mut desc).map_err(|e| format!("no report descriptor: {e}"))?;
    let (fields, numbered) = parse_input_fields(&desc[..n]);
    dev.set_blocking_mode(false).map_err(|e| format!("can't read without blocking: {e}"))?;
    Ok(Opened { dev, numbered, info: classify(&fields) })
}

/// A worker that opens HID tiers off the I/O thread: `HidApi::new` walks every
/// HID device on the system, and an open is device I/O — neither belongs on
/// the real-time loop.
pub(crate) struct Opener {
    jobs: mpsc::Sender<(String, CString)>,
    done: mpsc::Receiver<(String, Result<Opened, String>)>,
}

impl Opener {
    pub fn spawn() -> Option<Self> {
        let (jtx, jrx) = mpsc::channel::<(String, CString)>();
        let (dtx, drx) = mpsc::channel();
        std::thread::Builder::new()
            .name("raw-hid-open".into())
            .spawn(move || {
                let api = match hidapi::HidApi::new() {
                    Ok(api) => api,
                    Err(e) => {
                        eprintln!("[raw-hid] hidapi unavailable ({e}) — no HID tier for raw pads");
                        return;
                    }
                };
                while let Ok((dev_id, path)) = jrx.recv() {
                    if dtx.send((dev_id, open(&api, &path))).is_err() {
                        break;
                    }
                }
            })
            .ok()?;
        Some(Opener { jobs: jtx, done: drx })
    }

    pub fn request(&self, dev_id: &str, path: &str) {
        if let Ok(p) = CString::new(path) {
            let _ = self.jobs.send((dev_id.to_string(), p));
        }
    }

    /// Opens that have finished.
    pub fn finished(&self) -> Vec<(String, Result<Opened, String>)> {
        self.done.try_iter().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A trimmed DualShock 4 (USB) descriptor: report 1 carries the sticks,
    /// hat, buttons and triggers, then 54 vendor bytes (where the motion
    /// sensor lives); report 5 is output-only.
    const DS4_LIKE: &[u8] = &[
        0x05, 0x01, 0x09, 0x05, 0xA1, 0x01, // Generic Desktop · Gamepad · Collection
        0x85, 0x01, //   Report ID 1
        0x09, 0x30, 0x09, 0x31, 0x09, 0x32, 0x09, 0x35, // X Y Z Rz
        0x15, 0x00, 0x26, 0xFF, 0x00, 0x75, 0x08, 0x95, 0x04, 0x81, 0x02, // 4 × u8, Input
        0x09, 0x39, 0x15, 0x00, 0x25, 0x07, 0x35, 0x00, 0x46, 0x3B, 0x01, 0x65, 0x14,
        0x75, 0x04, 0x95, 0x01, 0x81, 0x42, // Hat, 4 bits
        0x65, 0x00, 0x05, 0x09, 0x19, 0x01, 0x29, 0x0E, 0x15, 0x00, 0x25, 0x01,
        0x75, 0x01, 0x95, 0x0E, 0x81, 0x02, // 14 buttons
        0x06, 0x00, 0xFF, 0x09, 0x20, 0x75, 0x06, 0x95, 0x01, 0x81, 0x02, // vendor 6 bits (counter)
        0x05, 0x01, 0x09, 0x33, 0x09, 0x34, 0x15, 0x00, 0x26, 0xFF, 0x00,
        0x75, 0x08, 0x95, 0x02, 0x81, 0x02, // Rx Ry (triggers)
        0x06, 0x00, 0xFF, 0x09, 0x21, 0x95, 0x36, 0x81, 0x02, // 54 vendor bytes
        0x85, 0x05, 0x09, 0x22, 0x95, 0x1F, 0x91, 0x02, // report 5, output
        0xC0,
    ];

    #[test]
    fn a_ds4_like_descriptor_splits_into_sdl_fields_a_counter_and_a_blob() {
        let (fields, numbered) = parse_input_fields(DS4_LIKE);
        assert!(numbered);
        // X Y Z Rz + hat + 14 buttons + counter + Rx Ry + 54 vendor bytes.
        assert_eq!(fields.len(), 4 + 1 + 14 + 1 + 2 + 54);
        let x = &fields[0];
        assert_eq!((x.page, x.usage, x.field.bit, x.field.size, x.field.max), (0x01, 0x30, 0, 8, 255));
        let first_button = &fields[5];
        assert_eq!((first_button.page, first_button.usage, first_button.field.bit), (0x09, 1, 36));
        let rx = fields.iter().find(|f| f.usage == 0x33).unwrap();
        assert_eq!(rx.field.bit, 56, "4 bytes + 4-bit hat + 14 buttons + 6-bit counter");

        let info = classify(&fields);
        assert_eq!(info.blobs, vec![(1, 9, 54)], "the vendor bytes after Rx/Ry");
        assert_eq!(info.fields.len(), 1, "only the counter is SDL's to miss and not a blob");
        // Logical max is still the buttons' 1: globals carry until changed.
        assert_eq!(info.fields[0].0, RawSrc::Field(HidField { report: 1, bit: 50, size: 6, min: 0, max: 1 }));
        assert!(info.fields[0].1.starts_with("Vendor FF00:20"), "{}", info.fields[0].1);
    }

    #[test]
    fn a_sensor_page_accelerometer_is_named_and_hinted() {
        let desc = [
            0x05, 0x20, 0x09, 0x73, 0xA1, 0x00, // Sensor page · Accelerometer 3D
            0x0A, 0x53, 0x04, 0x0A, 0x54, 0x04, 0x0A, 0x55, 0x04,
            0x16, 0x00, 0x80, 0x26, 0xFF, 0x7F, 0x75, 0x10, 0x95, 0x03, 0x81, 0x02,
            0xC0,
        ];
        let (fields, numbered) = parse_input_fields(&desc);
        assert!(!numbered);
        let info = classify(&fields);
        assert_eq!(info.fields.len(), 3);
        assert_eq!(info.fields[2].1, "Accelerometer Z");
        assert_eq!(info.fields[0].2, Some(MotionHint::Accel));
        assert_eq!(info.fields[1].0, RawSrc::Field(HidField { report: 0, bit: 16, size: 16, min: -32768, max: 32767 }));
    }

    #[test]
    fn push_pop_and_usage_ranges_are_honoured() {
        let desc = [
            0x05, 0x09, 0x19, 0x01, 0x29, 0x03, 0x15, 0x00, 0x25, 0x01, 0x75, 0x01, 0x95, 0x04,
            0xA4, // push
            0x05, 0x0C, 0x0A, 0xE9, 0x00, 0x95, 0x01, 0x81, 0x02, // Consumer Volume Up, 1 bit
            0xB4, // pop: Button page, count 4 again
            0x19, 0x01, 0x29, 0x03, 0x81, 0x02, // 4 buttons; the 4th repeats usage 3
            0x95, 0x03, 0x81, 0x01, // 3 bits constant padding
        ];
        let (fields, _) = parse_input_fields(&desc);
        assert_eq!(fields.len(), 5);
        assert_eq!((fields[0].page, fields[0].usage, fields[0].field.bit), (0x0C, 0xE9, 0));
        assert_eq!(fields[4].usage, 3, "a range shorter than the count repeats its last usage");
        assert_eq!(fields[4].field.bit, 4);
        let info = classify(&fields);
        assert_eq!(info.fields.len(), 1);
        assert_eq!(info.fields[0].1, "Volume Up");
    }
}
