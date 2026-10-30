//! One emulated composite USB device behind the server: the control-transfer
//! state machine, the endpoint dispatch, the parked URBs, and the bridges to
//! FlexInput — input reports in, output reports and speaker/haptic PCM out,
//! microphone PCM in.
//!
//! Port of HIDMaestro's `UsbipEmulatedDevice` (MIT) for the Sony composites:
//! FlexInput feeds it in-process, so it has no shared-memory bridge, and it
//! drops the Switch 2 and Valve personas' paths.
//!
//! A passthrough device (`crate::passthrough`) is the same machine with the
//! host's report requests handed out instead of answered here: each waits,
//! parked by its sequence number, for the real device's answer.

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{mpsc, Arc, Mutex};

use crate::audio::{AudioEngine, PcmSink, PendingIso};
use crate::descriptors::{DescriptorSet, TransferType};
use crate::feature::sony_feature;
use crate::passthrough::{HostRequest, ReportKind};
use crate::protocol::{
    write_iso_descriptor, write_ret_submit, write_ret_unlink, Command, IsoPacket, ECONNRESET, EPIPE,
    HEADER_SIZE, ISO_DESCRIPTOR_SIZE, NON_ISOCH,
};

/// Where replies go: the connection that imported the device.
pub(crate) trait Wire: Send + Sync {
    fn send(&self, data: &[u8]) -> std::io::Result<()>;
    /// End the connection; its reader then releases the device.
    fn close(&self) {}
}

/// Input reports kept while no interrupt-IN read is parked: the newest only.
/// FlexInput submits every I/O tick, faster than the host polls, so a deeper
/// queue would answer each read with a report from several ticks ago (HIDMaestro
/// keeps 8, for an SDK whose callers submit at their own pace). A real pad
/// likewise reports its state as of the poll.
const MAX_FRAME_QUEUE: usize = 1;
/// The same for a passthrough: a real pad's reports, each one an event —
/// a press and its release can arrive within one poll, and the newest alone
/// would lose the press.
const MAX_PASSTHROUGH_QUEUE: usize = 16;
/// Output reports kept for the consumer to collect.
const MAX_OUTPUTS: usize = 64;

/// What a host wrote to the device.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputKind {
    /// An output report: interrupt OUT, or SET_REPORT(Output).
    Output,
    /// SET_REPORT(Feature).
    Feature,
}

/// One report a host wrote, its report id split off.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutputFrame {
    pub kind: OutputKind,
    pub report_id: u8,
    pub data: Vec<u8>,
}

/// The claimed connection, shared with the audio engine's completion path.
#[derive(Default)]
struct WireSlot {
    wire: Mutex<Option<Arc<dyn Wire>>>,
}

impl WireSlot {
    fn send(&self, data: &[u8]) {
        let wire = self.wire.lock().unwrap().clone();
        if let Some(w) = wire {
            // A socket torn down mid-reply is the reader's to notice.
            let _ = w.send(data);
        }
    }

    fn ret_submit(&self, seqnum: u32, status: i32, actual: usize, payload: Option<&[u8]>) {
        let p = payload.unwrap_or(&[]);
        let mut buf = vec![0u8; HEADER_SIZE + p.len()];
        write_ret_submit(&mut buf, seqnum, status, actual as i32, 0, NON_ISOCH, 0);
        buf[HEADER_SIZE..].copy_from_slice(p);
        self.send(&buf);
    }

    /// An isochronous RET_SUBMIT. IN: the compacted data, then descriptors
    /// echoing the submit's offsets with `per` bytes each. OUT: descriptors
    /// alone, actual_length echoing the transfer buffer length.
    fn ret_submit_iso(&self, p: &PendingIso, data: Option<&[u8]>, per: u32) {
        let n = p.packets.len();
        let data = if p.is_in { data.unwrap_or(&[]) } else { &[] };
        let mut buf = vec![0u8; HEADER_SIZE + data.len() + n * ISO_DESCRIPTOR_SIZE];
        let actual = if p.is_in { per as i32 * n as i32 } else { p.transfer_buffer_length };
        write_ret_submit(&mut buf, p.seqnum, 0, actual, p.frame_at_completion, n as i32, 0);
        buf[HEADER_SIZE..HEADER_SIZE + data.len()].copy_from_slice(data);
        let d0 = HEADER_SIZE + data.len();
        for (i, pk) in p.packets.iter().enumerate() {
            let at = d0 + i * ISO_DESCRIPTOR_SIZE;
            let actual = if p.is_in { per } else { pk.length };
            write_iso_descriptor(&mut buf[at..at + ISO_DESCRIPTOR_SIZE], pk.offset, pk.length, actual, 0);
        }
        self.send(&buf);
    }
}

#[derive(Default)]
struct HidState {
    /// Input reports waiting for a read.
    frames: VecDeque<Vec<u8>>,
    /// Parked interrupt-IN reads, per endpoint address.
    parked: HashMap<u8, VecDeque<u32>>,
    /// The newest input report, for GET_REPORT(Input).
    last_input: Vec<u8>,
}

pub(crate) struct DeviceInner {
    pub(crate) desc: DescriptorSet,
    pub(crate) busid: String,
    pub(crate) devid: u32,
    /// Makes the pairing MAC unique per controller.
    index: u8,
    slot: Arc<WireSlot>,
    hid: Mutex<HidState>,
    configuration: AtomicU8,
    audio: AudioEngine,
    outputs: Mutex<VecDeque<OutputFrame>>,
    /// The interrupt-IN endpoint the input reports go out on.
    primary_in: u8,
    /// A passthrough's owner: report requests and output reports go here.
    forward: Option<mpsc::Sender<HostRequest>>,
    /// A passthrough's report requests waiting for the device, by sequence
    /// number: the length to answer with.
    deferred: Mutex<HashMap<u32, u16>>,
    frame_queue: usize,
}

impl DeviceInner {
    pub(crate) fn new(desc: DescriptorSet, busid: String, devid: u32, index: u8) -> Self {
        let slot = Arc::new(WireSlot::default());
        let primary_in = desc
            .endpoints
            .values()
            .find(|e| e.transfer_type() == TransferType::Interrupt && e.is_in() && e.interface == desc.hid_interface)
            .map_or(0, |e| e.address);
        let iso_slot = Arc::clone(&slot);
        let audio = AudioEngine::new(&desc, Box::new(move |p, data, per| iso_slot.ret_submit_iso(p, data, per)));
        DeviceInner {
            desc,
            busid,
            devid,
            index,
            slot,
            hid: Mutex::new(HidState::default()),
            configuration: AtomicU8::new(0),
            audio,
            outputs: Mutex::new(VecDeque::new()),
            primary_in,
            forward: None,
            deferred: Mutex::new(HashMap::new()),
            frame_queue: MAX_FRAME_QUEUE,
        }
    }

    /// A passthrough: what the host asks or writes goes to `forward`, and
    /// report requests wait for [`complete`](Self::complete).
    pub(crate) fn passthrough(desc: DescriptorSet, busid: String, devid: u32, forward: mpsc::Sender<HostRequest>) -> Self {
        DeviceInner {
            forward: Some(forward),
            frame_queue: MAX_PASSTHROUGH_QUEUE,
            ..Self::new(desc, busid, devid, 0)
        }
    }

    /// Answer a request a passthrough handed out: `Some` with a read's report
    /// (`[]` for a write), `None` to stall it. A request the host has since
    /// cancelled, or one that died with its connection, is dropped.
    pub(crate) fn complete(&self, seqnum: u32, result: Option<&[u8]>) {
        let Some(len) = self.deferred.lock().unwrap().remove(&seqnum) else { return };
        match result {
            Some(data) if !data.is_empty() => self.reply(seqnum, data, len),
            Some(_) => self.ok(seqnum, len as usize),
            None => self.stall(seqnum),
        }
    }

    /// Claim the device for one connection. A second import while one holds
    /// it is refused.
    pub(crate) fn claim(&self, wire: Arc<dyn Wire>) -> bool {
        let mut w = self.slot.wire.lock().unwrap();
        if w.is_some() {
            return false;
        }
        *w = Some(wire);
        true
    }

    /// Release the device if `wire` holds it.
    pub(crate) fn release(&self, wire: &Arc<dyn Wire>) {
        {
            let mut w = self.slot.wire.lock().unwrap();
            match w.as_ref() {
                Some(cur) if Arc::ptr_eq(cur, wire) => *w = None,
                _ => return,
            }
        }
        self.reset_state();
    }

    pub(crate) fn is_claimed(&self) -> bool {
        self.slot.wire.lock().unwrap().is_some()
    }

    /// Close the claimed connection, if any.
    pub(crate) fn disconnect(&self) {
        let wire = self.slot.wire.lock().unwrap().clone();
        if let Some(w) = wire {
            w.close();
        }
    }

    fn reset_state(&self) {
        self.configuration.store(0, Ordering::Release);
        self.reset_alt_settings();
        self.audio.clear();
        let mut h = self.hid.lock().unwrap();
        h.parked.clear();
        h.frames.clear();
        self.deferred.lock().unwrap().clear();
    }

    fn reset_alt_settings(&self) {
        for ep in self.desc.endpoints.values() {
            if ep.transfer_type() == TransferType::Isochronous {
                self.audio.reset_alt(ep.interface);
            }
        }
    }

    fn stall(&self, seqnum: u32) {
        self.slot.ret_submit(seqnum, -EPIPE, 0, None);
    }

    fn reply(&self, seqnum: u32, data: &[u8], w_length: u16) {
        let n = data.len().min(w_length as usize);
        self.slot.ret_submit(seqnum, 0, n, Some(&data[..n]));
    }

    fn ok(&self, seqnum: u32, actual: usize) {
        self.slot.ret_submit(seqnum, 0, actual, None);
    }

    fn push_output(&self, kind: OutputKind, report_id: u8, data: &[u8]) {
        let mut q = self.outputs.lock().unwrap();
        if q.len() >= MAX_OUTPUTS {
            q.pop_front();
        }
        q.push_back(OutputFrame { kind, report_id, data: data.to_vec() });
    }

    // ── From FlexInput ──────────────────────────────────────────────────

    /// A built input report (report id first): answers the oldest parked read
    /// on the input endpoint, or waits for the next one.
    pub(crate) fn submit_input(&self, report: &[u8]) {
        // Held across the send, so two reports can't reach the wire out of order.
        let mut h = self.hid.lock().unwrap();
        h.last_input.clear();
        h.last_input.extend_from_slice(report);
        let seq = h.parked.get_mut(&self.primary_in).and_then(VecDeque::pop_front);
        match seq {
            Some(seq) => self.slot.ret_submit(seq, 0, report.len(), Some(report)),
            None => {
                if h.frames.len() >= self.frame_queue {
                    h.frames.pop_front();
                }
                h.frames.push_back(report.to_vec());
            }
        }
    }

    pub(crate) fn take_outputs(&self) -> Vec<OutputFrame> {
        self.outputs.lock().unwrap().drain(..).collect()
    }

    pub(crate) fn submit_mic(&self, pcm: &[u8]) -> usize {
        self.audio.submit_mic(pcm)
    }

    pub(crate) fn set_pcm_sink(&self, sink: Option<PcmSink>) {
        self.audio.set_sink(sink);
    }

    /// Whether the host has the speaker/haptics stream open.
    pub(crate) fn output_stream_open(&self) -> bool {
        self.desc.output_stream().is_some_and(|s| self.audio.alt(s.interface) == s.alt_setting)
    }

    // ── From the connection's reader ────────────────────────────────────

    pub(crate) fn handle_submit(&self, cmd: &Command, payload: Option<Vec<u8>>, iso: Vec<IsoPacket>) {
        if cmd.ep == 0 {
            self.handle_control(cmd, payload.as_deref().unwrap_or(&[]));
            return;
        }
        let addr = cmd.ep as u8 | if cmd.is_in() { 0x80 } else { 0 };
        let Some(ep) = self.desc.endpoints.get(&addr).copied() else {
            self.stall(cmd.seqnum);
            return;
        };
        match ep.transfer_type() {
            TransferType::Isochronous => {
                self.audio.submit(PendingIso::new(
                    cmd.seqnum,
                    cmd.is_in(),
                    cmd.transfer_buffer_length,
                    iso,
                    payload,
                    ep.interface,
                    ep.alt_setting,
                ));
            }
            TransferType::Interrupt if cmd.is_in() => {
                // Only the HID interface's endpoint has reports.
                let mut h = self.hid.lock().unwrap();
                let frame = if addr == self.primary_in { h.frames.pop_front() } else { None };
                match frame {
                    Some(f) => self.slot.ret_submit(cmd.seqnum, 0, f.len(), Some(&f)),
                    None => h.parked.entry(addr).or_default().push_back(cmd.seqnum),
                }
            }
            TransferType::Interrupt => {
                // A whole output report, report id first.
                let p = payload.unwrap_or_default();
                let n = p.len();
                if let Some(f) = &self.forward {
                    let _ = f.send(HostRequest::Output(p));
                } else {
                    let (rid, data) = p.split_first().map_or((0, &[][..]), |(r, d)| (*r, d));
                    self.push_output(OutputKind::Output, rid, data);
                }
                self.ok(cmd.seqnum, n);
            }
            _ => self.stall(cmd.seqnum),
        }
    }

    pub(crate) fn handle_unlink(&self, seqnum: u32, victim: u32) {
        let mut removed = self.audio.unlink(victim);
        if !removed {
            removed = self.deferred.lock().unwrap().remove(&victim).is_some();
        }
        if !removed {
            let mut h = self.hid.lock().unwrap();
            for q in h.parked.values_mut() {
                if let Some(i) = q.iter().position(|&s| s == victim) {
                    q.remove(i);
                    removed = true;
                    break;
                }
            }
        }
        let mut buf = [0u8; HEADER_SIZE];
        write_ret_unlink(&mut buf, seqnum, if removed { -ECONNRESET } else { 0 });
        self.slot.send(&buf);
    }

    fn handle_control(&self, cmd: &Command, out: &[u8]) {
        let s = cmd.setup;
        let (request_type, request) = (s[0], s[1]);
        let value = u16::from_le_bytes([s[2], s[3]]);
        let index = u16::from_le_bytes([s[4], s[5]]);
        let length = u16::from_le_bytes([s[6], s[7]]);
        let kind = (request_type >> 5) & 0x03; // 0 standard, 1 class, 2 vendor
        let recipient = request_type & 0x1F; // 0 device, 1 interface, 2 endpoint
        let to_host = request_type & 0x80 != 0;
        let seq = cmd.seqnum;

        // A hub port reset (SET_FEATURE(PORT_RESET), USB_RT_PORT): back to
        // the unconfigured state.
        if request_type == 0x23 && request == 0x03 {
            self.reset_state();
            self.ok(seq, 0);
            return;
        }

        match (kind, recipient) {
            (0, _) => match request {
                0x06 => {
                    // GET_DESCRIPTOR
                    let (ty, idx) = ((value >> 8) as u8, value as u8);
                    let d = if recipient == 1 {
                        self.desc.get_hid_descriptor(ty)
                    } else {
                        self.desc.get_descriptor(ty, idx)
                    };
                    match d {
                        Some(d) => self.reply(seq, &d, length),
                        None => self.stall(seq),
                    }
                }
                0x00 if to_host => {
                    // GET_STATUS: the device is self-powered; the rest zero.
                    let status: u16 = if recipient == 0 { 1 } else { 0 };
                    self.reply(seq, &status.to_le_bytes(), length);
                }
                0x09 => {
                    // SET_CONFIGURATION
                    self.configuration.store(value as u8, Ordering::Release);
                    self.reset_alt_settings();
                    self.ok(seq, 0);
                }
                0x08 => self.reply(seq, &[self.configuration.load(Ordering::Acquire)], length),
                0x0B => {
                    // SET_INTERFACE
                    self.audio.set_alt(index as u8, value as u8);
                    self.ok(seq, 0);
                }
                0x0A => self.reply(seq, &[self.audio.alt(index as u8)], length),
                // CLEAR_FEATURE, SET_FEATURE, SET_ADDRESS (UDE handles that itself).
                0x01 | 0x03 | 0x05 => self.ok(seq, 0),
                _ => self.stall(seq),
            },
            (1, 1) => {
                let iface = index as u8;
                if iface == self.desc.hid_interface {
                    self.handle_hid_class(seq, request, value, length, to_host, out);
                    return;
                }
                // A UAC1 feature-unit request: the entity is wIndex's high byte.
                let (unit, selector, channel) = ((index >> 8) as u8, (value >> 8) as u8, value as u8);
                if to_host {
                    match self.audio.uac_get(request, unit, selector, channel, length) {
                        Some(d) => self.reply(seq, &d, length),
                        None => self.stall(seq),
                    }
                } else if request == 0x01 && self.audio.uac_set(unit, selector, channel, out) {
                    self.ok(seq, out.len());
                } else {
                    self.stall(seq);
                }
            }
            (1, 2) => {
                // Endpoint SAMPLING_FREQ. The high-speed blob advertises no
                // endpoint controls, but a host may set the one rate anyway.
                if value >> 8 == 0x01 {
                    if to_host {
                        self.reply(seq, &[0x80, 0xBB, 0x00], length); // 48000
                    } else {
                        self.ok(seq, out.len());
                    }
                } else {
                    self.stall(seq);
                }
            }
            // Vendor requests, Microsoft OS descriptors: the Sony pads stall them.
            _ => self.stall(seq),
        }
    }

    fn handle_hid_class(&self, seq: u32, request: u8, value: u16, length: u16, to_host: bool, out: &[u8]) {
        let (report_type, report_id) = ((value >> 8) as u8, value as u8);
        // A passthrough's reads and writes go to the real device.
        if let Some(f) = &self.forward {
            let kind = match report_type {
                0x01 => ReportKind::Input,
                0x02 => ReportKind::Output,
                _ => ReportKind::Feature,
            };
            let request = match request {
                0x01 if to_host => Some((length, HostRequest::GetReport { id: seq, kind, number: report_id })),
                0x09 if !to_host => {
                    Some((out.len() as u16, HostRequest::SetReport { id: seq, kind, data: out.to_vec() }))
                }
                _ => None,
            };
            if let Some((len, req)) = request {
                self.deferred.lock().unwrap().insert(seq, len);
                if f.send(req).is_err() {
                    self.complete(seq, None);
                }
                return;
            }
        }
        match request {
            0x01 if to_host => match report_type {
                // GET_REPORT(Input): the newest report.
                0x01 => {
                    let last = self.hid.lock().unwrap().last_input.clone();
                    self.reply(seq, &last, length);
                }
                // GET_REPORT(Feature): the Sony interrogation.
                0x03 => {
                    let stub = (self.desc.vid == 0x054C)
                        .then(|| sony_feature(self.desc.pid, report_id, length, self.index))
                        .flatten();
                    match stub {
                        Some(d) => self.reply(seq, &d, length),
                        None => self.stall(seq),
                    }
                }
                _ => self.stall(seq),
            },
            0x09 if !to_host => {
                // SET_REPORT: report id first when the payload carries one.
                let (rid, data) = out.split_first().map_or((report_id, &[][..]), |(r, d)| (*r, d));
                let kind = if report_type == 0x03 { OutputKind::Feature } else { OutputKind::Output };
                self.push_output(kind, rid, data);
                self.ok(seq, out.len());
            }
            0x0A if !to_host => self.ok(seq, 0), // SET_IDLE
            0x02 if to_host => self.reply(seq, &[0], length), // GET_IDLE
            // GET/SET_PROTOCOL: the pad isn't a boot device and stalls them.
            _ => self.stall(seq),
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::profiles::{DUALSENSE_COMPOSITE_JSON, DUALSENSE_EDGE_COMPOSITE_JSON};
    use crate::protocol::{read_iso_descriptor, CMD_SUBMIT, RET_SUBMIT, RET_UNLINK};
    use std::sync::mpsc;
    use std::time::Duration;

    /// Captures every reply as one message.
    pub(crate) struct Capture(pub Mutex<mpsc::Sender<Vec<u8>>>);

    impl Wire for Capture {
        fn send(&self, data: &[u8]) -> std::io::Result<()> {
            let _ = self.0.lock().unwrap().send(data.to_vec());
            Ok(())
        }
    }

    pub(crate) fn device(json: &str) -> (Arc<DeviceInner>, Arc<dyn Wire>, mpsc::Receiver<Vec<u8>>) {
        let desc = DescriptorSet::from_profile_json(json, "FX00000000000A").unwrap();
        let dev = Arc::new(DeviceInner::new(desc, "1-1".into(), 0x1_0001, 3));
        let (tx, rx) = mpsc::channel();
        let wire: Arc<dyn Wire> = Arc::new(Capture(Mutex::new(tx)));
        assert!(dev.claim(Arc::clone(&wire)));
        (dev, wire, rx)
    }

    pub(crate) fn submit(seqnum: u32, ep: u32, is_in: bool, len: i32, setup: [u8; 8]) -> Command {
        Command {
            command: CMD_SUBMIT,
            seqnum,
            devid: 0x1_0001,
            direction: is_in as u32,
            ep,
            transfer_buffer_length: len,
            number_of_packets: NON_ISOCH,
            setup,
            ..Default::default()
        }
    }

    /// (command, seqnum, status, actual_length, payload after the header).
    pub(crate) fn parse_reply(r: &[u8]) -> (u32, u32, i32, i32, Vec<u8>) {
        let be = |at: usize| u32::from_be_bytes([r[at], r[at + 1], r[at + 2], r[at + 3]]);
        (be(0), be(4), be(20) as i32, be(24) as i32, r[HEADER_SIZE..].to_vec())
    }

    fn recv(rx: &mpsc::Receiver<Vec<u8>>) -> (u32, u32, i32, i32, Vec<u8>) {
        parse_reply(&rx.recv_timeout(Duration::from_secs(1)).expect("a reply"))
    }

    fn control(dev: &DeviceInner, seq: u32, setup: [u8; 8], out: Option<Vec<u8>>) {
        let is_in = setup[0] & 0x80 != 0;
        let len = u16::from_le_bytes([setup[6], setup[7]]) as i32;
        dev.handle_submit(&submit(seq, 0, is_in, len, setup), out, Vec::new());
    }

    #[test]
    fn get_descriptor_cuts_to_wlength() {
        let (dev, _w, rx) = device(DUALSENSE_COMPOSITE_JSON);
        control(&dev, 1, [0x80, 0x06, 0x00, 0x02, 0, 0, 9, 0], None);
        let (cmd, seq, status, actual, data) = recv(&rx);
        assert_eq!((cmd, seq, status, actual), (RET_SUBMIT, 1, 0, 9));
        assert_eq!(&data, &dev.desc.configuration[..9]);
        // The HID report descriptor, asked of the HID interface.
        control(&dev, 2, [0x81, 0x06, 0x00, 0x22, 3, 0, 0xFF, 0x01], None);
        assert_eq!(recv(&rx).3, 273);
    }

    #[test]
    fn unknown_requests_stall() {
        let (dev, _w, rx) = device(DUALSENSE_COMPOSITE_JSON);
        control(&dev, 1, [0xC0, 0x01, 0, 0, 4, 0, 16, 0], None); // a vendor request
        assert_eq!(recv(&rx).2, -EPIPE);
        control(&dev, 2, [0xA1, 0x03, 0, 0, 3, 0, 1, 0], None); // GET_PROTOCOL
        assert_eq!(recv(&rx).2, -EPIPE);
    }

    // A read parks until FlexInput has a report; a report waits for a read.
    #[test]
    fn interrupt_in_parks_until_input_and_input_waits_for_a_read() {
        let (dev, _w, rx) = device(DUALSENSE_COMPOSITE_JSON);
        dev.handle_submit(&submit(10, 4, true, 64, [0; 8]), None, Vec::new());
        assert!(rx.recv_timeout(Duration::from_millis(30)).is_err(), "parked");
        let report = [0x01u8; 64];
        dev.submit_input(&report);
        let (_, seq, status, actual, data) = recv(&rx);
        assert_eq!((seq, status, actual), (10, 0, 64));
        assert_eq!(data, report.to_vec());

        dev.submit_input(&[0x01, 0xAA]);
        dev.submit_input(&[0x01, 0xBB]);
        dev.handle_submit(&submit(11, 4, true, 64, [0; 8]), None, Vec::new());
        assert_eq!(recv(&rx).4, vec![0x01, 0xBB], "the newest report answers the next read");
        dev.handle_submit(&submit(12, 4, true, 64, [0; 8]), None, Vec::new());
        assert!(rx.recv_timeout(Duration::from_millis(30)).is_err(), "nothing older is left to send");
    }

    #[test]
    fn interrupt_out_reaches_take_outputs_with_its_id_split_off() {
        let (dev, _w, rx) = device(DUALSENSE_COMPOSITE_JSON);
        dev.handle_submit(&submit(5, 3, false, 4, [0; 8]), Some(vec![0x02, 0xFF, 0x00, 0x7F]), Vec::new());
        let (_, seq, status, actual, data) = recv(&rx);
        assert_eq!((seq, status, actual), (5, 0, 4));
        assert!(data.is_empty(), "OUT replies carry no payload");
        assert_eq!(dev.take_outputs(), vec![OutputFrame {
            kind: OutputKind::Output,
            report_id: 0x02,
            data: vec![0xFF, 0x00, 0x7F],
        }]);
        assert!(dev.take_outputs().is_empty());
    }

    #[test]
    fn sony_interrogation_is_answered() {
        let (dev, _w, rx) = device(DUALSENSE_EDGE_COMPOSITE_JSON);
        control(&dev, 1, [0xA1, 0x01, 0x20, 0x03, 3, 0, 64, 0], None); // firmware info
        let (_, _, status, actual, data) = recv(&rx);
        assert_eq!((status, actual), (0, 64));
        assert_eq!(&data[22..24], &[0x44, 0x00], "the Edge's firmware line");
        control(&dev, 2, [0xA1, 0x01, 0x09, 0x03, 3, 0, 20, 0], None); // pairing
        assert_eq!(recv(&rx).4[6], 3, "MAC carries the controller index");
        control(&dev, 3, [0xA1, 0x01, 0x05, 0x03, 3, 0, 41, 0], None); // calibration
        assert_eq!(recv(&rx).3, 41);
    }

    #[test]
    fn unlink_answers_econnreset_for_a_parked_read_and_zero_after() {
        let (dev, _w, rx) = device(DUALSENSE_COMPOSITE_JSON);
        dev.handle_submit(&submit(20, 4, true, 64, [0; 8]), None, Vec::new());
        dev.handle_unlink(21, 20);
        let (cmd, seq, status, _, _) = recv(&rx);
        assert_eq!((cmd, seq, status), (RET_UNLINK, 21, -ECONNRESET));
        dev.handle_unlink(22, 20);
        assert_eq!(recv(&rx).2, 0, "already gone");
        dev.submit_input(&[1]);
        assert!(rx.recv_timeout(Duration::from_millis(30)).is_err(), "the unlinked read isn't answered");
    }

    // The speaker/haptics stream: SET_INTERFACE opens it, an OUT URB's PCM
    // reaches the sink and the reply carries only descriptors.
    #[test]
    fn iso_out_feeds_the_sink_and_replies_with_descriptors() {
        let (dev, _w, rx) = device(DUALSENSE_COMPOSITE_JSON);
        let (tx, pcm) = mpsc::channel();
        dev.set_pcm_sink(Some(Box::new(move |b: &[u8]| { let _ = tx.send(b.to_vec()); })));
        control(&dev, 1, [0x01, 0x0B, 1, 0, 1, 0, 0, 0], None); // SET_INTERFACE(1, 1)
        recv(&rx);
        assert!(dev.output_stream_open());
        let packets = vec![IsoPacket { offset: 0, length: 384 }, IsoPacket { offset: 392, length: 384 }];
        let mut cmd = submit(30, 1, false, 784, [0; 8]);
        cmd.number_of_packets = 2;
        dev.handle_submit(&cmd, Some(vec![9u8; 784]), packets);
        let reply = rx.recv_timeout(Duration::from_secs(1)).unwrap();
        let (_, seq, status, actual, rest) = parse_reply(&reply);
        assert_eq!((seq, status, actual), (30, 0, 784));
        assert_eq!(rest.len(), 2 * ISO_DESCRIPTOR_SIZE, "descriptors only");
        assert_eq!(read_iso_descriptor(&rest[16..]), IsoPacket { offset: 392, length: 384 });
        assert_eq!(pcm.recv_timeout(Duration::from_secs(1)).unwrap().len(), 784);
    }

    #[test]
    fn port_reset_unconfigures_and_drops_parked_reads() {
        let (dev, _w, rx) = device(DUALSENSE_COMPOSITE_JSON);
        control(&dev, 1, [0x00, 0x09, 1, 0, 0, 0, 0, 0], None); // SET_CONFIGURATION(1)
        recv(&rx);
        dev.handle_submit(&submit(2, 4, true, 64, [0; 8]), None, Vec::new());
        control(&dev, 3, [0x23, 0x03, 4, 0, 1, 0, 0, 0], None); // PORT_RESET
        recv(&rx);
        control(&dev, 4, [0x80, 0x08, 0, 0, 0, 0, 1, 0], None); // GET_CONFIGURATION
        assert_eq!(recv(&rx).4, vec![0]);
        dev.submit_input(&[1]);
        assert!(rx.recv_timeout(Duration::from_millis(30)).is_err());
    }

    #[test]
    fn a_second_claim_is_refused_until_released() {
        let (dev, wire, _rx) = device(DUALSENSE_COMPOSITE_JSON);
        let (tx, _rx2) = mpsc::channel();
        let other: Arc<dyn Wire> = Arc::new(Capture(Mutex::new(tx)));
        assert!(!dev.claim(Arc::clone(&other)));
        dev.release(&other); // not the holder: no effect
        assert!(dev.is_claimed());
        dev.release(&wire);
        assert!(dev.claim(other));
    }
}
