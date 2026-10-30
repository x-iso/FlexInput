//! UHID: a HID device served from this process through `/dev/uhid`, the Linux
//! counterpart of a HIDMaestro node. The kernel binds a driver to it by VID/PID
//! like any HID device (hid-playstation for the Sony pads, hid-generic for the
//! keyboard, mouse and media keys), so a game sees what a real controller gives
//! it: evdev nodes, hidraw for SDL and Steam, the motion sensors and touchpad,
//! with rumble and the lightbar coming back as output reports. The profiles,
//! the report encoder and the feature answers are HIDMaestro's.
//!
//! A reader thread answers the host: the feature reports drivers read while
//! probing (calibration, pairing, firmware, from the model), and the output
//! reports games write, queued for `poll_outputs` the way the HIDMaestro output
//! ring queues them.
//!
//! [`Passthrough`] is the other use: a REAL pad's HID interface served from
//! here, for the Bluetooth pads on FlexInput's own adapter. It is what BlueZ
//! does for every Bluetooth HID device, and the host's requests go to the pad
//! instead of to a model.

use std::fs::File;
use std::io::{Read, Write};
use std::os::fd::AsRawFd;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::thread::JoinHandle;

use flexinput_core::Signal;
use flexinput_hidmaestro::encode::{encode_report_into, PinState};
use flexinput_hidmaestro::Profile;
// The same requests the USB/IP passthrough hands out on Windows, so the
// Bluetooth transport serves both with one loop.
use flexinput_usbip::{HostRequest, ReportKind};

use crate::sony_feedback::SonyFeedback;
use crate::{layouts, SinkPin, SourcePin};

// <linux/uhid.h>
const UHID_DESTROY: u32 = 1;
const UHID_OUTPUT: u32 = 6;
const UHID_GET_REPORT: u32 = 9;
const UHID_GET_REPORT_REPLY: u32 = 10;
const UHID_CREATE2: u32 = 11;
const UHID_INPUT2: u32 = 12;
const UHID_SET_REPORT: u32 = 13;
const UHID_SET_REPORT_REPLY: u32 = 14;
const UHID_FEATURE_REPORT: u8 = 0;
const UHID_OUTPUT_REPORT: u8 = 1;
#[cfg(test)]
const UHID_INPUT_REPORT: u8 = 2;
const UHID_DATA_MAX: usize = 4096;
/// `sizeof(struct uhid_event)`: the type, then its largest member, create2
/// (name, phys, uniq, rd_size, bus, vendor, product, version, country, rd_data).
const EVENT_SIZE: usize = 4 + 128 + 64 + 64 + 2 + 2 + 4 * 4 + 4096;
const BUS_USB: u16 = 0x03;
/// `BUS_BLUETOOTH`, what a [`Passthrough`] of a Bluetooth pad is created on:
/// the kernel drivers and SDL pick their Bluetooth report formats by it.
pub const BUS_BLUETOOTH: u16 = 0x05;

/// Output reports kept for the next poll. A node nobody polls (the keyboard's
/// LED reports) must not grow without bound.
const MAX_QUEUED_OUTPUTS: usize = 64;

/// The open device, shared by the writer (input reports) and the reader thread
/// (replies). Writes to `/dev/uhid` are one event each, so the two don't
/// interleave.
struct Node {
    file: File,
}

impl Node {
    /// Open `/dev/uhid` and create the device from a profile.
    fn create(name: &str, phys: &str, profile: &Profile) -> std::io::Result<Node> {
        // uniq: empty; hid-playstation fills it from the pad's MAC.
        Self::create_raw(&PassthroughSpec {
            name,
            phys,
            uniq: "",
            bus: BUS_USB,
            vid: profile.vid,
            pid: profile.pid,
            version: flexinput_core::own_virtual::VERSION,
            descriptor: &profile.descriptor,
        })
    }

    /// Open `/dev/uhid` and create the device.
    fn create_raw(spec: &PassthroughSpec) -> std::io::Result<Node> {
        use std::os::unix::fs::OpenOptionsExt;
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(libc::O_CLOEXEC)
            .open(super::access::UHID)?;
        let rd = spec.descriptor;
        if rd.len() > UHID_DATA_MAX {
            return Err(std::io::Error::other("report descriptor too long"));
        }
        let mut ev = vec![0u8; 280 + rd.len()];
        ev[0..4].copy_from_slice(&UHID_CREATE2.to_ne_bytes());
        put_str(&mut ev[4..132], spec.name);
        put_str(&mut ev[132..196], spec.phys);
        put_str(&mut ev[196..260], spec.uniq);
        ev[260..262].copy_from_slice(&(rd.len() as u16).to_ne_bytes());
        ev[262..264].copy_from_slice(&spec.bus.to_ne_bytes());
        ev[264..268].copy_from_slice(&(spec.vid as u32).to_ne_bytes());
        ev[268..272].copy_from_slice(&(spec.pid as u32).to_ne_bytes());
        ev[272..276].copy_from_slice(&(spec.version as u32).to_ne_bytes());
        // country [276..280]: 0.
        ev[280..].copy_from_slice(rd);
        (&file).write_all(&ev)?;
        Ok(Node { file })
    }

    /// Send one input report, report id included.
    fn input(&self, report: &[u8]) {
        let len = report.len().min(UHID_DATA_MAX);
        let mut ev = [0u8; 6 + UHID_DATA_MAX];
        ev[0..4].copy_from_slice(&UHID_INPUT2.to_ne_bytes());
        ev[4..6].copy_from_slice(&(len as u16).to_ne_bytes());
        ev[6..6 + len].copy_from_slice(&report[..len]);
        // Fails only while the kernel hasn't started the device, or after it
        // went away; either way there is no one to report to.
        let _ = (&self.file).write(&ev[..6 + len]);
    }
}

impl Drop for Node {
    fn drop(&mut self) {
        // Closing the node destroys the device too; saying so is cleaner.
        let _ = (&self.file).write(&UHID_DESTROY.to_ne_bytes());
    }
}

/// Copy `s` into a fixed C string field, truncated and NUL-terminated.
fn put_str(field: &mut [u8], s: &str) {
    let n = s.len().min(field.len() - 1);
    field[..n].copy_from_slice(&s.as_bytes()[..n]);
}

/// The reader thread: answer feature reads from `answers`, accept feature
/// writes, queue output reports (report id stripped when the descriptor
/// numbers its reports) in `outputs`. Polls with a timeout so `stop` ends it.
fn serve(
    node: Arc<Node>,
    answers: Vec<(u8, Vec<u8>)>,
    numbered: bool,
    outputs: Arc<Mutex<Vec<Vec<u8>>>>,
    stop: Arc<AtomicBool>,
) {
    let fd = node.file.as_raw_fd();
    let mut ev = vec![0u8; EVENT_SIZE];
    while !stop.load(Ordering::Relaxed) {
        let mut pfd = libc::pollfd { fd, events: libc::POLLIN, revents: 0 };
        // SAFETY: one valid pollfd for the node's open fd.
        let ready = unsafe { libc::poll(&mut pfd, 1, 100) };
        if ready <= 0 {
            continue;
        }
        let n = match (&node.file).read(&mut ev) {
            Ok(n) if n >= 4 => n,
            Ok(_) => continue,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => break,
        };
        let kind = u32::from_ne_bytes([ev[0], ev[1], ev[2], ev[3]]);
        match kind {
            UHID_GET_REPORT if n >= 10 => {
                let id = &ev[4..8];
                let (rnum, rtype) = (ev[8], ev[9]);
                let reply = answers.iter().find(|(r, _)| rtype == UHID_FEATURE_REPORT && *r == rnum);
                let data = reply.map_or(&[][..], |(_, d)| d.as_slice());
                let mut out = vec![0u8; 12 + data.len()];
                out[0..4].copy_from_slice(&UHID_GET_REPORT_REPLY.to_ne_bytes());
                out[4..8].copy_from_slice(id);
                let err: u16 = if reply.is_some() { 0 } else { libc::EIO as u16 };
                out[8..10].copy_from_slice(&err.to_ne_bytes());
                out[10..12].copy_from_slice(&(data.len() as u16).to_ne_bytes());
                out[12..].copy_from_slice(data);
                let _ = (&node.file).write(&out);
            }
            UHID_SET_REPORT if n >= 8 => {
                // Nothing a feature write changes is modelled; accept it.
                let mut out = [0u8; 10];
                out[0..4].copy_from_slice(&UHID_SET_REPORT_REPLY.to_ne_bytes());
                out[4..8].copy_from_slice(&ev[4..8]);
                let _ = (&node.file).write(&out);
            }
            UHID_OUTPUT if n >= 4 + UHID_DATA_MAX + 3 => {
                let size = u16::from_ne_bytes([ev[4 + UHID_DATA_MAX], ev[5 + UHID_DATA_MAX]]) as usize;
                let rtype = ev[6 + UHID_DATA_MAX];
                let data = &ev[4..4 + size.min(UHID_DATA_MAX)];
                if rtype == UHID_OUTPUT_REPORT && !data.is_empty() {
                    let frame = if numbered { &data[1..] } else { data };
                    let mut q = outputs.lock().unwrap_or_else(|e| e.into_inner());
                    if q.len() >= MAX_QUEUED_OUTPUTS {
                        q.remove(0);
                    }
                    q.push(frame.to_vec());
                }
            }
            // START / STOP / OPEN / CLOSE: nothing to do.
            _ => {}
        }
    }
}

/// A UHID-backed virtual device built from a HIDMaestro profile: the Sony pads
/// through the gamepad encoder, the keyboard / mouse / media nodes through raw
/// reports (`keymouse_hm`). Same surface as `HidMaestroDevice`.
pub struct UhidDevice {
    id: String,
    display_name: String,
    profile: Profile,
    pins: PinState,
    report_buf: Vec<u8>,
    /// `None` when the device couldn't be created: it then drops writes and
    /// reads as disconnected, like a HIDMaestro device without its section.
    node: Option<Arc<Node>>,
    outputs: Arc<Mutex<Vec<Vec<u8>>>>,
    stop: Arc<AtomicBool>,
    reader: Option<JoinHandle<()>>,
    rumble: (f32, f32),
    sony: SonyFeedback,
}

impl UhidDevice {
    /// Create the device. `index` tells two pads of a kind apart where the
    /// feature answers carry it (the Sony pads' MAC). `None` only for an invalid
    /// profile; a device that couldn't be created comes back disconnected.
    pub fn create(
        id: impl Into<String>,
        display_name: impl Into<String>,
        profile_json: &str,
        index: u32,
    ) -> Option<Self> {
        let mut dev = Self::unbuilt(id.into(), display_name.into(), Profile::from_json(profile_json).ok()?);
        if let Err(e) = super::access::ensure() {
            eprintln!("[uhid] {}: no access to {} ({e})", dev.id, super::access::UHID);
            return Some(dev);
        }
        // A Sony pad goes by the name its USB strings give it, which is what
        // hid-playstation and the games looking for one expect; our own nodes
        // by theirs.
        let name = if dev.profile.vid == 0x054C {
            format!(
                "Sony Interactive Entertainment {}",
                dev.profile.product_string.as_deref().unwrap_or("Wireless Controller")
            )
        } else {
            dev.display_name.clone()
        };
        let phys = format!("{}{}", flexinput_core::own_virtual::PHYS_PREFIX, dev.id);
        let node = match Node::create(&name, &phys, &dev.profile) {
            Ok(n) => Arc::new(n),
            Err(e) => {
                eprintln!("[uhid] {}: create failed: {e}", dev.id);
                return Some(dev);
            }
        };
        let answers = flexinput_hidmaestro::answers::feature_reports(dev.profile.vid, dev.profile.pid, index);
        let numbered = dev.profile.report.report_id != 0;
        let (n2, outputs, stop) = (Arc::clone(&node), Arc::clone(&dev.outputs), Arc::clone(&dev.stop));
        dev.reader = std::thread::Builder::new()
            .name("uhid".into())
            .spawn(move || serve(n2, answers, numbered, outputs, stop))
            .ok();
        dev.node = Some(node);
        eprintln!("[uhid] {} up as \"{name}\" ({:04X}:{:04X})", dev.id, dev.profile.vid, dev.profile.pid);
        Some(dev)
    }

    /// The device before its node exists: disconnected.
    fn unbuilt(id: String, display_name: String, profile: Profile) -> Self {
        UhidDevice {
            id,
            display_name,
            report_buf: vec![0u8; profile.input_report_size],
            profile,
            pins: PinState::new(),
            node: None,
            outputs: Arc::new(Mutex::new(Vec::new())),
            stop: Arc::new(AtomicBool::new(false)),
            reader: None,
            rumble: (0.0, 0.0),
            sony: SonyFeedback::default(),
        }
    }

    /// Send a raw, pre-built input report: `input_report_size` bytes of
    /// payload without the report id, as `HidMaestroDevice::write_raw_input`
    /// takes it. For the profiles the gamepad encoder doesn't describe.
    pub fn write_raw_input(&mut self, data: &[u8]) {
        let Some(node) = &self.node else { return };
        let id = self.profile.report.report_id;
        if id != 0 {
            let mut rep = Vec::with_capacity(data.len() + 1);
            rep.push(id);
            rep.extend_from_slice(data);
            node.input(&rep);
        } else {
            node.input(data);
        }
    }

    fn sink_pins_for(&self) -> &'static [SinkPin] {
        if self.profile.id.contains("dualsense-edge") {
            layouts::DUALSENSE_EDGE_SINK_PINS
        } else if self.profile.id.contains("dualsense") {
            layouts::DUALSENSE_SINK_PINS
        } else {
            layouts::DS4_SINK_PINS
        }
    }
}

impl crate::VirtualDevice for UhidDevice {
    fn id(&self) -> &str {
        &self.id
    }

    fn display_name(&self) -> &str {
        &self.display_name
    }

    fn sink_pins(&self) -> &'static [SinkPin] {
        self.sink_pins_for()
    }

    fn send(&mut self, pin: &str, value: Signal) {
        crate::apply_pin(&mut self.pins, pin, value);
    }

    fn flush(&mut self) {
        let Some(node) = &self.node else { return };
        let state = self.pins.state();
        // The encoder writes the report id at [0] when there is one, which is
        // the form UHID takes.
        encode_report_into(&self.profile, &state, &mut self.report_buf);
        node.input(&self.report_buf);
    }

    fn reset_outputs(&mut self) {
        self.pins.reset();
        self.flush();
    }

    fn is_connected(&self) -> bool {
        self.node.is_some()
    }

    fn source_pins(&self) -> &'static [SourcePin] {
        if self.profile.id.contains("dualsense") {
            layouts::DUALSENSE_SOURCE_PINS
        } else {
            layouts::DS4_SOURCE_PINS
        }
    }

    fn poll_outputs(&mut self) -> Vec<(&'static str, Signal)> {
        let frames = std::mem::take(&mut *self.outputs.lock().unwrap_or_else(|e| e.into_inner()));
        self.sony.poll(&self.profile, &mut self.rumble, frames.iter().map(Vec::as_slice))
    }

    fn inject_rumble_ping(&mut self, strong: f32, weak: f32) {
        // As HidMaestroDevice: the level stands until the next real frame.
        self.rumble = (strong, weak);
    }
}

impl Drop for UhidDevice {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(r) = self.reader.take() {
            let _ = r.join();
        }
        // The last Arc: Node's Drop destroys the device.
        self.node = None;
    }
}

/// What a [`Passthrough`] is created as. BlueZ's choices for a Bluetooth
/// pad: its own name, the adapter's address as `phys`, the pad's as `uniq`
/// (where hid-playstation reads a Bluetooth pad's MAC), its Device ID numbers.
///
/// ❗ Neither our `phys` prefix nor our version mark: this is a real pad, and
/// FlexInput reads it like one (see `flexinput_core::own_virtual`).
pub struct PassthroughSpec<'a> {
    pub name: &'a str,
    pub phys: &'a str,
    pub uniq: &'a str,
    pub bus: u16,
    pub vid: u16,
    pub pid: u16,
    pub version: u16,
    pub descriptor: &'a [u8],
}

/// A report type, from the kernel's number for it.
fn report_kind(rtype: u8) -> ReportKind {
    match rtype {
        UHID_FEATURE_REPORT => ReportKind::Feature,
        UHID_OUTPUT_REPORT => ReportKind::Output,
        _ => ReportKind::Input,
    }
}

/// A real device's HID interface served through UHID. Input reports go in
/// with [`input`](Self::input); what the host asks comes out of
/// [`next_request`](Self::next_request) for the caller to put to the device,
/// and each read or write is answered once the device has.
///
/// ⭐ A report read is answered by the DEVICE, not from a table, so drivers
/// get the pad's own calibration and firmware data. The kernel gives each
/// request five seconds before failing it.
pub struct Passthrough {
    node: Arc<Node>,
    requests: mpsc::Receiver<HostRequest>,
    stop: Arc<AtomicBool>,
    reader: Option<JoinHandle<()>>,
}

impl Passthrough {
    /// Create the device. Asks for `/dev/uhid` access the first time, as the
    /// virtual pads do.
    pub fn create(spec: &PassthroughSpec) -> Result<Self, String> {
        super::access::ensure().map_err(|e| format!("no access to {}: {e}", super::access::UHID))?;
        let node = Arc::new(Node::create_raw(spec).map_err(|e| format!("create failed: {e}"))?);
        let (tx, requests) = mpsc::channel();
        let stop = Arc::new(AtomicBool::new(false));
        let (n2, s2) = (Arc::clone(&node), Arc::clone(&stop));
        let reader = std::thread::Builder::new()
            .name("uhid-passthrough".into())
            .spawn(move || relay(n2, tx, s2))
            .ok();
        Ok(Passthrough { node, requests, stop, reader })
    }

    /// Send one input report, report id included.
    pub fn input(&self, report: &[u8]) {
        self.node.input(report);
    }

    /// The host's next request, if any is waiting.
    pub fn next_request(&self) -> Option<HostRequest> {
        self.requests.try_recv().ok()
    }

    /// Answer a [`HostRequest::GetReport`]: the report (its id first), or
    /// `None` for a device that refused or never answered.
    pub fn reply_get(&self, id: u32, report: Option<&[u8]>) {
        let data = report.unwrap_or_default();
        let len = data.len().min(UHID_DATA_MAX);
        let mut out = vec![0u8; 12 + len];
        out[0..4].copy_from_slice(&UHID_GET_REPORT_REPLY.to_ne_bytes());
        out[4..8].copy_from_slice(&id.to_ne_bytes());
        let err: u16 = if report.is_some() { 0 } else { libc::EIO as u16 };
        out[8..10].copy_from_slice(&err.to_ne_bytes());
        out[10..12].copy_from_slice(&(len as u16).to_ne_bytes());
        out[12..].copy_from_slice(&data[..len]);
        let _ = (&self.node.file).write(&out);
    }

    /// Answer a [`HostRequest::SetReport`].
    pub fn reply_set(&self, id: u32, ok: bool) {
        let mut out = [0u8; 10];
        out[0..4].copy_from_slice(&UHID_SET_REPORT_REPLY.to_ne_bytes());
        out[4..8].copy_from_slice(&id.to_ne_bytes());
        let err: u16 = if ok { 0 } else { libc::EIO as u16 };
        out[8..10].copy_from_slice(&err.to_ne_bytes());
        let _ = (&self.node.file).write(&out);
    }
}

impl Drop for Passthrough {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(r) = self.reader.take() {
            let _ = r.join();
        }
        // The reader's Arc is gone with it: this drop destroys the device.
    }
}

/// The passthrough's reader thread: every request the host makes, handed to
/// the owner. Polls with a timeout so `stop` ends it.
fn relay(node: Arc<Node>, tx: mpsc::Sender<HostRequest>, stop: Arc<AtomicBool>) {
    let fd = node.file.as_raw_fd();
    let mut ev = vec![0u8; EVENT_SIZE];
    while !stop.load(Ordering::Relaxed) {
        let mut pfd = libc::pollfd { fd, events: libc::POLLIN, revents: 0 };
        // SAFETY: one valid pollfd for the node's open fd.
        let ready = unsafe { libc::poll(&mut pfd, 1, 100) };
        if ready <= 0 {
            continue;
        }
        let n = match (&node.file).read(&mut ev) {
            Ok(n) if n >= 4 => n,
            Ok(_) => continue,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => break,
        };
        let request = match u32::from_ne_bytes([ev[0], ev[1], ev[2], ev[3]]) {
            UHID_GET_REPORT if n >= 10 => HostRequest::GetReport {
                id: u32::from_ne_bytes([ev[4], ev[5], ev[6], ev[7]]),
                number: ev[8],
                kind: report_kind(ev[9]),
            },
            UHID_SET_REPORT if n >= 12 => {
                let size = (u16::from_ne_bytes([ev[10], ev[11]]) as usize).min(UHID_DATA_MAX).min(n - 12);
                HostRequest::SetReport {
                    id: u32::from_ne_bytes([ev[4], ev[5], ev[6], ev[7]]),
                    kind: report_kind(ev[9]),
                    data: ev[12..12 + size].to_vec(),
                }
            }
            UHID_OUTPUT if n >= 4 + UHID_DATA_MAX + 3 => {
                let size = u16::from_ne_bytes([ev[4 + UHID_DATA_MAX], ev[5 + UHID_DATA_MAX]]) as usize;
                let data = &ev[4..4 + size.min(UHID_DATA_MAX)];
                if ev[6 + UHID_DATA_MAX] != UHID_OUTPUT_REPORT || data.is_empty() {
                    continue;
                }
                HostRequest::Output(data.to_vec())
            }
            // START / STOP / OPEN / CLOSE: nothing to do.
            _ => continue,
        };
        if tx.send(request).is_err() {
            break;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::VirtualDevice;

    #[test]
    fn report_kinds_map_from_the_kernels_numbers() {
        assert_eq!(report_kind(UHID_FEATURE_REPORT), ReportKind::Feature);
        assert_eq!(report_kind(UHID_OUTPUT_REPORT), ReportKind::Output);
        assert_eq!(report_kind(UHID_INPUT_REPORT), ReportKind::Input);
    }

    #[test]
    fn the_event_size_is_the_kernels() {
        // sizeof(struct uhid_event) on every Linux ABI: packed, create2 largest.
        assert_eq!(EVENT_SIZE, 4376);
    }

    /// Without a node: these test the pins and the decode, not the kernel.
    fn unbuilt(json: &str) -> UhidDevice {
        UhidDevice::unbuilt("virtual.hm.test".into(), "test".into(), Profile::from_json(json).unwrap())
    }

    #[test]
    fn sony_profiles_build_their_pin_sets_disconnected() {
        use flexinput_hidmaestro::profile::presets::{DUALSENSE_EDGE_JSON, DUALSENSE_JSON, DUALSHOCK_4_V2_JSON};
        let ds4 = unbuilt(DUALSHOCK_4_V2_JSON);
        assert!(!ds4.is_connected());
        assert!(std::ptr::eq(ds4.sink_pins(), layouts::DS4_SINK_PINS));
        assert!(std::ptr::eq(unbuilt(DUALSENSE_JSON).sink_pins(), layouts::DUALSENSE_SINK_PINS));
        assert!(std::ptr::eq(unbuilt(DUALSENSE_EDGE_JSON).sink_pins(), layouts::DUALSENSE_EDGE_SINK_PINS));
        assert!(std::ptr::eq(unbuilt(DUALSENSE_JSON).source_pins(), layouts::DUALSENSE_SOURCE_PINS));
    }

    #[test]
    fn output_reports_reach_the_sony_decoder() {
        let mut dev = unbuilt(flexinput_hidmaestro::profile::presets::DUALSHOCK_4_V2_JSON);
        // DS4 USB report 0x05 without its id: flags, then right@4 / left@5
        // (RID-inclusive), so 3 / 4 here.
        let mut frame = vec![0u8; 31];
        frame[0] = 0x01;
        frame[3] = 0x40;
        frame[4] = 0xFF;
        dev.outputs.lock().unwrap().push(frame);
        let pins = dev.poll_outputs();
        let get = |p: &str| pins.iter().find(|(id, _)| *id == p).map(|(_, s)| s.as_float());
        assert_eq!(get("rumble_strong"), Some(1.0));
        assert!((get("rumble_weak").unwrap() - 0x40 as f32 / 255.0).abs() < 1e-6);
    }
}
