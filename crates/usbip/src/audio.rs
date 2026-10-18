//! The isochronous engine for one composite device: paces both audio
//! directions at the endpoints' 1 ms service interval, hands the speaker /
//! haptic PCM a game streams to a consumer, and sources microphone PCM from
//! the consumer's feed.
//!
//! Pacing: a dedicated thread waits on a high-resolution timer for the next
//! URB's due time, with a short spin to land it. A stream cursor advances one
//! service interval per isochronous packet, so an URB of N packets completes
//! N ms after the one before it — this device is the audio clock, the way a
//! real adaptive-sink pad is. The host's own multi-URB lead is the jitter
//! buffer, and submit-time jitter is absorbed because due times come from the
//! cursor, not from arrival. With nothing pending the thread parks.
//!
//! Wire rules for the completions (usbip-win2 `wsk_receive.cpp`): OUT replies
//! carry descriptors only; IN replies carry the data compacted (no padding
//! between packets), then descriptors whose offsets echo the submit's, with
//! actual_length the sum of the per-packet actual lengths.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crate::descriptors::{AudioControl, DescriptorSet};
use crate::pacer::Pacer;
use crate::protocol::IsoPacket;

const SERVICE_INTERVAL: Duration = Duration::from_millis(1);
/// A cursor further behind now than this re-anchors (stream start, or the
/// host fell behind).
const REANCHOR: Duration = Duration::from_millis(50);
/// An inferred stream (see [`State::inferred_open`]) closes this long after
/// its traffic stops.
const STOP_AFTER: Duration = Duration::from_millis(200);
const INFER_CHECK: Duration = Duration::from_millis(50);
/// How early the timer wakes before a due time; the rest is spun.
const SPIN_LEAD: Duration = Duration::from_micros(200);
/// Microphone lead the ring holds, in service intervals.
const MIC_RING_MS: usize = 256;

/// A pending isochronous URB, parked until its due time.
#[derive(Debug, Clone)]
pub struct PendingIso {
    pub seqnum: u32,
    pub is_in: bool,
    pub transfer_buffer_length: i32,
    pub packets: Vec<IsoPacket>,
    /// OUT only: the PCM the host sent.
    pub out_payload: Option<Vec<u8>>,
    /// The streaming interface the endpoint belongs to, and the alternate
    /// setting that declares it.
    pub interface: u8,
    pub alt_setting: u8,
    due: Instant,
    /// The virtual frame counter (ms since the engine started) at completion.
    pub frame_at_completion: i32,
}

impl PendingIso {
    pub fn new(seqnum: u32, is_in: bool, transfer_buffer_length: i32, packets: Vec<IsoPacket>,
               out_payload: Option<Vec<u8>>, interface: u8, alt_setting: u8) -> Self {
        PendingIso {
            seqnum, is_in, transfer_buffer_length, packets, out_payload, interface, alt_setting,
            due: Instant::now(),
            frame_at_completion: 0,
        }
    }
}

/// Called on the pacing thread with each completed URB: for IN, the compacted
/// microphone data and the per-packet actual length.
pub(crate) type Complete = Box<dyn Fn(&PendingIso, Option<&[u8]>, u32) + Send + Sync>;

/// Receives each window of speaker/haptic PCM, interleaved, as the host sent it.
pub type PcmSink = Box<dyn FnMut(&[u8]) + Send>;

struct State {
    pending: Vec<PendingIso>,
    out_cursor: Option<Instant>,
    in_cursor: Option<Instant>,
    /// Microphone FIFO: one continuous interleaved stream. Bytes only ever
    /// leave or get refused on a frame boundary — a lost partial frame would
    /// shift every later sample by a byte, permanently.
    mic: VecDeque<u8>,
    mic_capacity: usize,
    mic_bytes_per_ms: usize,
    mic_frame: usize,
    mic_dropped: u64,
    /// UAC1 feature-unit state, keyed by (unit, selector, channel).
    controls: HashMap<(u8, u8, u8), i16>,
    ranges: HashMap<(u8, u8, u8), (i16, i16, i16)>,
    known_units: HashSet<u8>,
    alt: HashMap<u8, u8>,
    /// Interfaces the host has sent a SET_INTERFACE for: from then on only
    /// SET_INTERFACE decides their stream state.
    explicit_alt: HashSet<u8>,
    /// Streams opened by traffic alone. usbip-win2 0.9.8.1's host controller
    /// driver recognises the root-hub filter's SELECT_INTERFACE marker only
    /// with bmRequestType 0x80, and every older filter sends 0x00; with an old
    /// filter beside a new driver, SET_INTERFACE never arrives while the audio
    /// still flows. So an interface that never got one is open while its
    /// endpoint carries traffic, and closes [`STOP_AFTER`] after it stops.
    inferred_open: HashSet<u8>,
    last_iso: HashMap<u8, Instant>,
}

struct Shared {
    state: Mutex<State>,
    pacer: Pacer,
    stop: AtomicBool,
    complete: Complete,
    sink: Mutex<Option<PcmSink>>,
    epoch: Instant,
}

pub(crate) struct AudioEngine {
    shared: Arc<Shared>,
    thread: Option<JoinHandle<()>>,
}

impl AudioEngine {
    pub(crate) fn new(desc: &DescriptorSet, complete: Complete) -> Self {
        let (mic_bytes_per_ms, mic_frame) = desc
            .input_stream()
            .map(|s| (s.bytes_per_ms(), s.frame_bytes().max(1)))
            .unwrap_or((0, 1));
        let mut controls = HashMap::new();
        let mut ranges = HashMap::new();
        let mut known_units = HashSet::new();
        for &AudioControl { unit_id, mute_cur, volume_min, volume_max, volume_res, volume_cur } in
            &desc.audio_controls
        {
            known_units.insert(unit_id);
            controls.insert((unit_id, 1, 0), mute_cur);
            controls.insert((unit_id, 2, 0), volume_cur);
            ranges.insert((unit_id, 2, 0), (volume_min, volume_max, volume_res));
        }
        let shared = Arc::new(Shared {
            state: Mutex::new(State {
                pending: Vec::new(),
                out_cursor: None,
                in_cursor: None,
                mic: VecDeque::new(),
                mic_capacity: mic_bytes_per_ms.max(1) * MIC_RING_MS,
                mic_bytes_per_ms,
                mic_frame,
                mic_dropped: 0,
                controls,
                ranges,
                known_units,
                alt: HashMap::new(),
                explicit_alt: HashSet::new(),
                inferred_open: HashSet::new(),
                last_iso: HashMap::new(),
            }),
            pacer: Pacer::new(),
            stop: AtomicBool::new(false),
            complete,
            sink: Mutex::new(None),
            epoch: Instant::now(),
        });
        let pump = Arc::clone(&shared);
        let thread = std::thread::Builder::new()
            .name("usbip-audio".into())
            .spawn(move || pump_loop(&pump))
            .ok();
        AudioEngine { shared, thread }
    }

    pub(crate) fn set_sink(&self, sink: Option<PcmSink>) {
        *self.shared.sink.lock().unwrap() = sink;
    }

    /// Queue an isochronous URB. Its due time comes from the direction's
    /// stream cursor, one service interval per packet.
    pub(crate) fn submit(&self, mut urb: PendingIso) {
        let now = Instant::now();
        {
            let mut st = self.shared.state.lock().unwrap();
            let cursor = if urb.is_in { &mut st.in_cursor } else { &mut st.out_cursor };
            let base = match *cursor {
                Some(c) if c + REANCHOR >= now => c,
                _ => now,
            };
            let due = base + SERVICE_INTERVAL * urb.packets.len() as u32;
            *cursor = Some(due);
            urb.due = due;
            let iface = urb.interface;
            if urb.alt_setting != 0 && !st.explicit_alt.contains(&iface) {
                st.last_iso.insert(iface, now);
                if st.alt.get(&iface).copied().unwrap_or(0) == 0 {
                    st.alt.insert(iface, urb.alt_setting);
                    st.inferred_open.insert(iface);
                }
            }
            st.pending.push(urb);
        }
        self.shared.pacer.wake();
    }

    /// Remove a pending URB (CMD_UNLINK). True if it was still queued.
    pub(crate) fn unlink(&self, seqnum: u32) -> bool {
        let mut st = self.shared.state.lock().unwrap();
        match st.pending.iter().position(|p| p.seqnum == seqnum) {
            Some(i) => {
                st.pending.remove(i);
                true
            }
            None => false,
        }
    }

    /// Drop every pending URB without replying (the connection is going).
    pub(crate) fn clear(&self) {
        self.shared.state.lock().unwrap().pending.clear();
    }

    /// The host's SET_INTERFACE.
    pub(crate) fn set_alt(&self, interface: u8, alt: u8) {
        let mut st = self.shared.state.lock().unwrap();
        st.explicit_alt.insert(interface);
        apply_alt(&mut st, interface, alt);
    }

    /// A reset (SET_CONFIGURATION, port reset) parks the stream, and says
    /// nothing about whether the transport forwards SET_INTERFACE.
    pub(crate) fn reset_alt(&self, interface: u8) {
        apply_alt(&mut self.shared.state.lock().unwrap(), interface, 0);
    }

    pub(crate) fn alt(&self, interface: u8) -> u8 {
        self.shared.state.lock().unwrap().alt.get(&interface).copied().unwrap_or(0)
    }

    /// Append interleaved microphone PCM. Returns the bytes accepted: all of
    /// them, or — when the ring is full — as many as land it on a frame
    /// boundary.
    pub(crate) fn submit_mic(&self, pcm: &[u8]) -> usize {
        let mut st = self.shared.state.lock().unwrap();
        let used = st.mic.len();
        let free = st.mic_capacity.saturating_sub(used);
        let mut n = pcm.len();
        if n > free {
            n = free.saturating_sub((used + free) % st.mic_frame);
        }
        st.mic.extend(&pcm[..n]);
        st.mic_dropped += (pcm.len() - n) as u64;
        n
    }

    /// GET_CUR / MIN / MAX / RES on a feature unit: the little-endian value,
    /// or `None` to stall (an unknown unit or selector, as the real pad).
    pub(crate) fn uac_get(&self, request: u8, unit: u8, selector: u8, channel: u8, w_length: u16) -> Option<Vec<u8>> {
        let st = self.shared.state.lock().unwrap();
        if !st.known_units.contains(&unit) {
            return None;
        }
        let key = (unit, selector, channel);
        let value = match request {
            0x81 => *st.controls.get(&key)?,
            0x82 => st.ranges.get(&key)?.0,
            0x83 => st.ranges.get(&key)?.1,
            0x84 => st.ranges.get(&key)?.2,
            _ => return None,
        };
        // Mute is a 1-byte control, volume 2: serve what the host asked for.
        Some(if w_length <= 1 { vec![value as u8] } else { value.to_le_bytes().to_vec() })
    }

    /// SET_CUR on a feature unit. False stalls.
    pub(crate) fn uac_set(&self, unit: u8, selector: u8, channel: u8, data: &[u8]) -> bool {
        let mut st = self.shared.state.lock().unwrap();
        if !st.known_units.contains(&unit) {
            return false;
        }
        let value = match data {
            [lo, hi, ..] => i16::from_le_bytes([*lo, *hi]),
            [b] => *b as i16,
            [] => 0,
        };
        match st.controls.get_mut(&(unit, selector, channel)) {
            Some(v) => {
                *v = value;
                true
            }
            None => false,
        }
    }
}

impl Drop for AudioEngine {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::Release);
        self.shared.pacer.wake();
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

fn apply_alt(st: &mut State, interface: u8, alt: u8) {
    st.alt.insert(interface, alt);
    st.inferred_open.remove(&interface);
    if alt == 0 {
        // Parking a stream resets its cursor, so the next start re-anchors
        // instead of completing a burst at once.
        st.out_cursor = None;
        st.in_cursor = None;
    }
}

fn close_idle_inferred(st: &mut State, now: Instant) {
    let idle: Vec<u8> = st
        .inferred_open
        .iter()
        .copied()
        .filter(|i| st.last_iso.get(i).is_none_or(|&t| now.duration_since(t) > STOP_AFTER))
        .collect();
    for i in idle {
        st.inferred_open.remove(&i);
        st.alt.insert(i, 0);
    }
}

fn pump_loop(sh: &Shared) {
    let mut next_infer_check = Instant::now();
    let mut done: Vec<PendingIso> = Vec::new();
    while !sh.stop.load(Ordering::Acquire) {
        let now = Instant::now();
        let mut next_due: Option<Instant> = None;
        {
            let mut st = sh.state.lock().unwrap();
            if now >= next_infer_check {
                next_infer_check = now + INFER_CHECK;
                close_idle_inferred(&mut st, now);
            }
            // Submit order is completion order.
            let mut i = 0;
            while i < st.pending.len() {
                if st.pending[i].due <= now {
                    let mut p = st.pending.remove(i);
                    p.frame_at_completion = p.due.duration_since(sh.epoch).as_millis() as i32;
                    done.push(p);
                } else {
                    let due = st.pending[i].due;
                    next_due = Some(next_due.map_or(due, |n| n.min(due)));
                    i += 1;
                }
            }
        }
        for p in done.drain(..) {
            complete(sh, p);
        }
        match next_due {
            None => sh.pacer.wait(Duration::from_millis(250)),
            Some(due) => {
                let left = due.saturating_duration_since(Instant::now());
                if left > SPIN_LEAD {
                    sh.pacer.wait(left - SPIN_LEAD);
                } else {
                    while Instant::now() < due && !sh.stop.load(Ordering::Relaxed) {
                        std::hint::spin_loop();
                    }
                }
            }
        }
    }
}

fn complete(sh: &Shared, p: PendingIso) {
    if p.is_in {
        // Each packet gets one service interval of microphone PCM, compacted;
        // an underrun fills silence so usbaudio is never starved.
        let (data, per) = {
            let mut st = sh.state.lock().unwrap();
            let mut per = p.packets.first().map_or(st.mic_bytes_per_ms, |q| {
                st.mic_bytes_per_ms.min(q.length as usize)
            });
            if per == 0 {
                per = st.mic_bytes_per_ms;
            }
            let mut data = vec![0u8; per * p.packets.len()];
            let mut take = st.mic.len().min(data.len());
            take -= take % st.mic_frame;
            for (d, s) in data.iter_mut().zip(st.mic.drain(..take)) {
                *d = s;
            }
            (data, per)
        };
        (sh.complete)(&p, Some(&data), per as u32);
    } else {
        if let Some(pcm) = p.out_payload.as_deref().filter(|b| !b.is_empty()) {
            if let Some(sink) = sh.sink.lock().unwrap().as_mut() {
                sink(pcm);
            }
        }
        (sh.complete)(&p, None, 0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::profiles::DUALSENSE_COMPOSITE_JSON;
    use std::sync::mpsc;

    /// (seqnum, when, IN data, per-packet actual) per completion.
    type Completions = mpsc::Receiver<(u32, Instant, Option<Vec<u8>>, u32)>;

    fn engine() -> (AudioEngine, Completions) {
        let desc = DescriptorSet::from_profile_json(DUALSENSE_COMPOSITE_JSON, "FX").unwrap();
        let (tx, rx) = mpsc::channel();
        let tx = Mutex::new(tx);
        let e = AudioEngine::new(&desc, Box::new(move |p, data, per| {
            let _ = tx.lock().unwrap().send((p.seqnum, Instant::now(), data.map(<[u8]>::to_vec), per));
        }));
        (e, rx)
    }

    fn packets(n: usize, len: u32) -> Vec<IsoPacket> {
        (0..n).map(|i| IsoPacket { offset: i as u32 * len, length: len }).collect()
    }

    // Back-to-back URBs complete one service interval per packet apart, paced
    // by the cursor rather than by when they arrived.
    #[test]
    fn out_urbs_complete_a_millisecond_per_packet() {
        let (e, rx) = engine();
        let start = Instant::now();
        for s in 1..=4 {
            e.submit(PendingIso::new(s, false, 4 * 392, packets(4, 392), Some(vec![0; 4 * 384]), 1, 1));
        }
        let mut last = start;
        for s in 1..=4u32 {
            let (seq, at, data, _) = rx.recv_timeout(Duration::from_secs(1)).unwrap();
            assert_eq!(seq, s, "submit order");
            assert!(data.is_none());
            last = at;
        }
        let took = last.duration_since(start);
        assert!(took >= Duration::from_millis(15), "16 packets take ~16 ms, took {took:?}");
        assert!(took < Duration::from_millis(80), "paced, not starved: {took:?}");
    }

    #[test]
    fn the_sink_gets_the_hosts_pcm() {
        let (e, rx) = engine();
        let (tx, got) = mpsc::channel();
        e.set_sink(Some(Box::new(move |pcm: &[u8]| { let _ = tx.send(pcm.to_vec()); })));
        e.submit(PendingIso::new(1, false, 8, packets(1, 8), Some((0..8).collect()), 1, 1));
        rx.recv_timeout(Duration::from_secs(1)).unwrap();
        assert_eq!(got.recv_timeout(Duration::from_secs(1)).unwrap(), (0..8).collect::<Vec<u8>>());
    }

    // An IN URB carries one interval of microphone PCM per packet, whole
    // frames only, and silence past what was fed.
    #[test]
    fn mic_urbs_carry_whole_frames_then_silence() {
        let (e, rx) = engine();
        assert_eq!(e.submit_mic(&[7u8; 10]), 10);
        e.submit(PendingIso::new(1, true, 2 * 196, packets(2, 196), None, 2, 1));
        let (_, _, data, per) = rx.recv_timeout(Duration::from_secs(1)).unwrap();
        let data = data.unwrap();
        assert_eq!(per, 192, "one millisecond of 48 kHz stereo 16-bit");
        assert_eq!(data.len(), 384);
        assert_eq!(&data[..8], &[7u8; 8], "two whole 4-byte frames");
        assert!(data[8..].iter().all(|&b| b == 0));
    }

    #[test]
    fn a_full_mic_ring_refuses_on_a_frame_boundary() {
        let (e, _rx) = engine();
        let cap = 192 * MIC_RING_MS;
        assert_eq!(e.submit_mic(&vec![1u8; cap - 4]), cap - 4);
        assert_eq!(e.submit_mic(&[2u8; 10]), 4, "one whole frame fits; the rest is refused");
        assert_eq!(e.submit_mic(&[3u8; 4]), 0, "full");
    }

    #[test]
    fn unlink_removes_a_queued_urb_only() {
        let (e, rx) = engine();
        e.submit(PendingIso::new(1, false, 0, packets(30, 392), None, 1, 1));
        assert!(e.unlink(1));
        assert!(!e.unlink(1));
        assert!(rx.recv_timeout(Duration::from_millis(60)).is_err(), "an unlinked URB never completes");
    }

    #[test]
    fn uac_controls_serve_the_pads_ranges() {
        let (e, _rx) = engine();
        // Speaker volume, unit 2: min -25600, max 0, res 256, cur -25600.
        assert_eq!(e.uac_get(0x82, 2, 2, 0, 2), Some((-25600i16).to_le_bytes().to_vec()));
        assert_eq!(e.uac_get(0x84, 2, 2, 0, 2), Some(256i16.to_le_bytes().to_vec()));
        assert!(e.uac_set(2, 2, 0, &(-1024i16).to_le_bytes()));
        assert_eq!(e.uac_get(0x81, 2, 2, 0, 2), Some((-1024i16).to_le_bytes().to_vec()));
        assert_eq!(e.uac_get(0x81, 2, 1, 0, 1), Some(vec![0]), "mute, one byte");
        assert!(e.uac_get(0x81, 9, 2, 0, 2).is_none(), "unknown unit stalls");
        assert!(!e.uac_set(2, 2, 1, &[0, 0]), "per-channel volume isn't a control here");
    }

    #[test]
    fn traffic_opens_a_stream_set_interface_never_sent() {
        let (e, _rx) = engine();
        assert_eq!(e.alt(1), 0);
        e.submit(PendingIso::new(1, false, 0, packets(1, 392), None, 1, 1));
        assert_eq!(e.alt(1), 1, "opened by its traffic");
        e.set_alt(1, 0);
        e.submit(PendingIso::new(2, false, 0, packets(1, 392), None, 1, 1));
        assert_eq!(e.alt(1), 0, "after a SET_INTERFACE only SET_INTERFACE decides");
    }
}
