//! Incoming MIDI → sparse, latched bus pins.
//!
//! MIDI is event-based but the engine samples once per I/O tick, and the tick's
//! signal map is rebuilt from scratch every time. Two consequences shape this
//! decoder:
//!
//! * **Sparse.** Only pins away from rest are emitted. A controller with every
//!   key released and every knob at zero emits nothing, instead of the
//!   thousands of pins 16 channels × 128 notes/CCs would otherwise be. Readers
//!   substitute [`MidiPin::rest_value`] for a missing MIDI pin.
//! * **Latched.** Events that start and finish between two polls must still be
//!   seen: a note on+off reports its gate (and velocity) for one poll, a knob
//!   that spiked and came back reports its peak for one poll, and pulses
//!   (Program Change, Start/Stop/Continue, SysEx) stay asserted for at least
//!   [`PULSE_MIN_POLLS`] polls and [`PULSE_MIN_DURATION`].
//!
//! Pure and clock-injected so the state machines are unit-testable.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use flexinput_core::midi::{self, Channel, MidiMessage, MidiPin, Transport};
use flexinput_core::Signal;

/// Minimum number of polls a pulse pin stays asserted.
pub const PULSE_MIN_POLLS: u32 = 2;
/// Minimum wall time a pulse pin stays asserted, so press-mode timing in the
/// engine sees it even at very high polling rates.
pub const PULSE_MIN_DURATION: Duration = Duration::from_millis(10);
/// Clock ticks older than this mean the clock stopped; BPM drops to rest.
pub const CLOCK_TIMEOUT: Duration = Duration::from_millis(500);

const CLOCK_PPQN: f64 = 24.0;
/// Smoothing for the clock-interval EMA (weight of the newest interval).
const CLOCK_EMA_ALPHA: f64 = 0.1;
/// One data increment/decrement step, in 14-bit units (one MSB step).
const DATA_STEP: i32 = 128;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SlotKind {
    Gate,
    Value,
    Pulse,
}

struct Slot {
    /// Cached bus id, formatted once when the pin first leaves rest.
    id: String,
    /// Legacy alias emitted alongside (`cc_<n>` / `pitch_bend`).
    alias: Option<String>,
    kind: SlotKind,
    /// Gate: 1.0 while on. Value: the current value.
    value: f32,
    /// Gate: went on at some point since the last poll.
    rose: bool,
    /// Value: the value farthest from `reported` seen since the last poll.
    extreme: Option<f32>,
    /// Value: what the last poll emitted.
    reported: f32,
    /// Pulse: an event arrived since the last poll.
    pending: bool,
    /// Pulse: when the current assertion started, and how many polls saw it.
    asserted_at: Option<Instant>,
    asserted_polls: u32,
    /// Value: keep publishing it even at 0. A held note's aftertouch (and its
    /// channel's pressure) is the note's live value once it has been sent, and
    /// easing it back to nothing must read as 0 — not vanish from the bus, which
    /// would drop the note back to the next thing in line (its velocity).
    held: bool,
}

impl Slot {
    fn new(pin: &MidiPin) -> Self {
        let kind = if pin.is_pulse() {
            SlotKind::Pulse
        } else if pin.is_gate() {
            SlotKind::Gate
        } else {
            SlotKind::Value
        };
        let alias = match pin {
            MidiPin::Cc { ch: Channel::Any, cc } => Some(midi::legacy_cc_id(*cc)),
            MidiPin::PitchBend { ch: Channel::Any } => Some(midi::LEGACY_PITCH_BEND.to_string()),
            _ => None,
        };
        Slot {
            id: pin.to_id(),
            alias,
            kind,
            value: 0.0,
            rose: false,
            extreme: None,
            reported: 0.0,
            pending: false,
            asserted_at: None,
            asserted_polls: 0,
            held: false,
        }
    }

    fn set_value(&mut self, v: f32) {
        match self.kind {
            SlotKind::Gate => {
                if v > 0.0 && self.value <= 0.0 {
                    self.rose = true;
                }
                self.value = v;
            }
            SlotKind::Value => {
                let dist = (v - self.reported).abs();
                if self.extreme.is_none_or(|e| dist > (e - self.reported).abs()) {
                    self.extreme = Some(v);
                }
                self.value = v;
            }
            SlotKind::Pulse => self.pending = true,
        }
    }

    /// Value to emit this poll (`None` = at rest), advancing latches.
    fn poll(&mut self, now: Instant) -> Option<Signal> {
        match self.kind {
            SlotKind::Gate => {
                let on = self.value > 0.0 || self.rose;
                self.rose = false;
                on.then_some(Signal::Bool(true))
            }
            SlotKind::Value => {
                let out = self.extreme.take().unwrap_or(self.value);
                self.reported = out;
                (out != 0.0 || self.held).then_some(Signal::Float(out))
            }
            SlotKind::Pulse => {
                if std::mem::take(&mut self.pending) {
                    self.asserted_at = Some(now);
                    self.asserted_polls = 0;
                }
                let Some(at) = self.asserted_at else { return None };
                let active = self.asserted_polls < PULSE_MIN_POLLS
                    || now.saturating_duration_since(at) < PULSE_MIN_DURATION;
                if active {
                    self.asserted_polls += 1;
                    Some(Signal::Bool(true))
                } else {
                    self.asserted_at = None;
                    None
                }
            }
        }
    }

    fn at_rest(&self) -> bool {
        match self.kind {
            SlotKind::Gate => self.value <= 0.0 && !self.rose,
            SlotKind::Value => {
                !self.held && self.value == 0.0 && self.extreme.is_none() && self.reported == 0.0
            }
            SlotKind::Pulse => !self.pending && self.asserted_at.is_none(),
        }
    }
}

/// The two halves of a 14-bit controller (MSB 0..=31, LSB MSB+32).
#[derive(Clone, Copy, Default)]
struct Cc14Halves {
    msb: u8,
    lsb: u8,
    msb_seen: bool,
    lsb_seen: bool,
}

impl Cc14Halves {
    /// Both halves have been used: this controller really is 14-bit, rather
    /// than a plain knob that happens to sit on 0..=63.
    fn paired(&self) -> bool {
        self.msb_seen && self.lsb_seen
    }

    fn unit(&self) -> f32 {
        (((self.msb as u16) << 7) | self.lsb as u16) as f32 / 16383.0
    }
}

/// NRPN/RPN parameter selection for one channel.
#[derive(Clone, Copy, Default)]
struct ParamSelect {
    nrpn_msb: u8,
    nrpn_lsb: u8,
    rpn_msb: u8,
    rpn_lsb: u8,
    /// Which parameter data entry currently addresses.
    active: Option<(bool /* nrpn */, u16)>,
}

/// Decoder + pin store for one MIDI In port.
pub struct InPortDecoder {
    slots: HashMap<MidiPin, Slot>,
    running_status: Option<u8>,
    /// Channels holding each note, one bit per channel (drives the `*` gate).
    note_channels: [u16; 128],
    /// 14-bit CC halves per channel for controllers 0..=31.
    cc14: [[Cc14Halves; 32]; 16],
    params: [ParamSelect; 16],
    /// Current 14-bit data per (nrpn, channel, param).
    param_values: HashMap<(bool, u8, u16), u16>,
    /// Timestamp (µs) of the last clock tick, smoothed interval, arrival time.
    clock_last_us: Option<u64>,
    clock_interval_us: Option<f64>,
    clock_seen_at: Option<Instant>,
    /// The pin the most recent learnable message addressed (concrete channel),
    /// for the MIDI In node's Learn.
    last_pin: Option<MidiPin>,
    /// Messages since the last `take_event_count` (Active Sensing excluded).
    event_count: u32,
}

impl Default for InPortDecoder {
    fn default() -> Self {
        Self::new()
    }
}

impl InPortDecoder {
    pub fn new() -> Self {
        Self {
            slots: HashMap::new(),
            running_status: None,
            note_channels: [0; 128],
            cc14: [[Cc14Halves::default(); 32]; 16],
            params: [ParamSelect::default(); 16],
            param_values: HashMap::new(),
            clock_last_us: None,
            clock_interval_us: None,
            clock_seen_at: None,
            last_pin: None,
            event_count: 0,
        }
    }

    fn set(&mut self, pin: MidiPin, v: f32) {
        // A value going to rest on a pin that was never away from rest needs
        // no slot — that's what keeps the store sparse.
        if let Some(slot) = self.slots.get_mut(&pin) {
            slot.set_value(v);
            return;
        }
        if v == 0.0 && !pin.is_pulse() {
            return;
        }
        let mut slot = Slot::new(&pin);
        slot.set_value(v);
        self.slots.insert(pin, slot);
    }

    /// Set a channel pin and its "any channel" twin.
    fn set_both(&mut self, pin: MidiPin, v: f32) {
        self.set(pin.with_channel(Channel::Any), v);
        self.set(pin, v);
    }

    /// Feed one raw message. `timestamp_us` is the driver timestamp (used for
    /// clock tempo); `now` is the arrival time.
    pub fn feed(&mut self, bytes: &[u8], timestamp_us: u64, now: Instant) {
        let Some(msg) = MidiMessage::parse(bytes, &mut self.running_status) else { return };
        if msg != MidiMessage::ActiveSensing {
            self.event_count = self.event_count.saturating_add(1);
        }
        self.apply(msg, timestamp_us, now);
    }

    fn apply(&mut self, msg: MidiMessage, timestamp_us: u64, now: Instant) {
        if let Some(pin) = self.learnable_pin(&msg) {
            self.last_pin = Some(pin);
        }
        match msg {
            MidiMessage::NoteOn { ch, note, vel } => {
                self.note_channels[note as usize] |= 1 << ch;
                let c = Channel::Ch(ch);
                self.set_both(MidiPin::Note { ch: c, note }, 1.0);
                self.set_both(MidiPin::Velocity { ch: c, note }, vel as f32 / 127.0);
            }
            MidiMessage::NoteOff { ch, note, .. } => self.note_off(ch, note),
            MidiMessage::PolyAftertouch { ch, note, value } => {
                let pin = MidiPin::PolyAftertouch { ch: Channel::Ch(ch), note };
                self.set_both(pin.clone(), value as f32 / 127.0);
                // Only for a note that is sounding: aftertouch is part of it.
                if self.note_channels[note as usize] & (1 << ch) != 0 {
                    self.hold(&pin, true);
                    self.hold(&pin.with_channel(Channel::Any), true);
                }
            }
            MidiMessage::ControlChange { ch, cc, value } => self.control_change(ch, cc, value),
            MidiMessage::ProgramChange { ch, program } => {
                self.set_both(MidiPin::ProgramChange { ch: Channel::Ch(ch), program }, 1.0);
            }
            MidiMessage::ChannelPressure { ch, value } => {
                let pin = MidiPin::ChannelPressure { ch: Channel::Ch(ch) };
                self.set_both(pin.clone(), value as f32 / 127.0);
                // The channel's pressure is its sounding notes' pressure.
                if self.channel_sounding(ch) {
                    self.hold(&pin, true);
                    self.hold(&pin.with_channel(Channel::Any), true);
                }
            }
            MidiMessage::PitchBend { ch, value } => {
                self.set_both(MidiPin::PitchBend { ch: Channel::Ch(ch) }, midi::bend_from_14bit(value));
            }
            MidiMessage::SysEx(bytes) => {
                if midi::is_valid_sysex(&bytes) {
                    self.set(MidiPin::SysEx(bytes), 1.0);
                }
            }
            MidiMessage::TimingClock => self.clock_tick(timestamp_us, now),
            MidiMessage::Start | MidiMessage::Continue => {
                let t = if msg == MidiMessage::Start { Transport::Start } else { Transport::Continue };
                self.set(MidiPin::Transport(t), 1.0);
                self.set(MidiPin::Playing, 1.0);
            }
            MidiMessage::Stop => {
                self.set(MidiPin::Transport(Transport::Stop), 1.0);
                self.set(MidiPin::Playing, 0.0);
            }
            MidiMessage::Reset => self.reset(),
            MidiMessage::ActiveSensing | MidiMessage::Other(_) => {}
        }
    }

    /// What Learn should pick up from `msg`, judged BEFORE the message updates
    /// the decoder state (so a CC LSB can check whether its MSB was seen).
    /// Parameter-number selection CCs and note-offs aren't learnable on their
    /// own; the data entry that follows a selection is.
    fn learnable_pin(&self, msg: &MidiMessage) -> Option<MidiPin> {
        Some(match *msg {
            MidiMessage::NoteOn { ch, note, .. } => MidiPin::Note { ch: Channel::Ch(ch), note },
            // Aftertouch is part of its note, so learning it learns the note —
            // whose live value it then is.
            MidiMessage::PolyAftertouch { ch, note, .. } => MidiPin::Note { ch: Channel::Ch(ch), note },
            MidiMessage::ControlChange { ch, cc, .. } => {
                let c = Channel::Ch(ch);
                match cc {
                    // Data entry addresses the selected parameter, if any.
                    6 | 38 | 96 | 97 if self.params[ch as usize].active.is_some() => {
                        match self.params[ch as usize].active {
                            Some((true, param)) => MidiPin::Nrpn { ch: c, param },
                            Some((false, param)) => MidiPin::Rpn { ch: c, param },
                            None => unreachable!(),
                        }
                    }
                    98..=101 => return None,
                    // An LSB whose MSB this port has sent is a 14-bit pair;
                    // otherwise it's a plain 7-bit controller.
                    32..=63 if self.cc14[ch as usize][(cc - 32) as usize].msb_seen => {
                        MidiPin::Cc14 { ch: c, cc: cc - 32 }
                    }
                    _ => MidiPin::Cc { ch: c, cc },
                }
            }
            MidiMessage::ProgramChange { ch, program } => MidiPin::ProgramChange { ch: Channel::Ch(ch), program },
            MidiMessage::ChannelPressure { ch, .. } => MidiPin::ChannelPressure { ch: Channel::Ch(ch) },
            MidiMessage::PitchBend { ch, .. } => MidiPin::PitchBend { ch: Channel::Ch(ch) },
            MidiMessage::SysEx(ref bytes) if midi::is_valid_sysex(bytes) => MidiPin::SysEx(bytes.clone()),
            MidiMessage::Start => MidiPin::Transport(Transport::Start),
            MidiMessage::Stop => MidiPin::Transport(Transport::Stop),
            MidiMessage::Continue => MidiPin::Transport(Transport::Continue),
            _ => return None,
        })
    }

    /// Keep (or stop keeping) a value pin on the bus at 0 — see `Slot::held`.
    fn hold(&mut self, pin: &MidiPin, on: bool) {
        if let Some(slot) = self.slots.get_mut(pin) {
            slot.held = on;
        }
    }

    /// Is any note sounding on channel `ch`?
    fn channel_sounding(&self, ch: u8) -> bool {
        self.note_channels.iter().any(|m| m & (1 << ch) != 0)
    }

    fn note_off(&mut self, ch: u8, note: u8) {
        let mask = &mut self.note_channels[note as usize];
        *mask &= !(1 << ch);
        let any_left = *mask != 0;
        let c = Channel::Ch(ch);
        self.set(MidiPin::Note { ch: c, note }, 0.0);
        self.set(MidiPin::Velocity { ch: c, note }, 0.0);
        self.set(MidiPin::PolyAftertouch { ch: c, note }, 0.0);
        self.hold(&MidiPin::PolyAftertouch { ch: c, note }, false);
        if !any_left {
            self.set(MidiPin::Note { ch: Channel::Any, note }, 0.0);
            self.set(MidiPin::Velocity { ch: Channel::Any, note }, 0.0);
            self.set(MidiPin::PolyAftertouch { ch: Channel::Any, note }, 0.0);
            self.hold(&MidiPin::PolyAftertouch { ch: Channel::Any, note }, false);
        }
        // The channel's pressure lets go with its last note.
        if !self.channel_sounding(ch) {
            self.hold(&MidiPin::ChannelPressure { ch: c }, false);
        }
        if (0..16u8).all(|c| !self.channel_sounding(c)) {
            self.hold(&MidiPin::ChannelPressure { ch: Channel::Any }, false);
        }
    }

    fn control_change(&mut self, ch: u8, cc: u8, value: u8) {
        let c = Channel::Ch(ch);
        self.set_both(MidiPin::Cc { ch: c, cc }, value as f32 / 127.0);

        let chi = ch as usize;
        match cc {
            // 14-bit controller MSB: per the spec a new MSB zeroes the LSB.
            0..=31 => {
                let e = &mut self.cc14[chi][cc as usize];
                e.msb = value;
                e.lsb = 0;
                e.msb_seen = true;
                if e.paired() {
                    let v = e.unit();
                    self.set_both(MidiPin::Cc14 { ch: c, cc }, v);
                }
            }
            // 14-bit controller LSB. The cc14 pin only appears once BOTH halves
            // have been seen, so a plain 7-bit controller on 0..=63 doesn't
            // grow a 14-bit twin.
            32..=63 => {
                let msb_cc = cc - 32;
                let e = &mut self.cc14[chi][msb_cc as usize];
                e.lsb = value;
                e.lsb_seen = true;
                if e.paired() {
                    let v = e.unit();
                    self.set_both(MidiPin::Cc14 { ch: c, cc: msb_cc }, v);
                }
            }
            _ => {}
        }

        let sel = &mut self.params[chi];
        match cc {
            99 => { sel.nrpn_msb = value; sel.active = Some((true, param(sel.nrpn_msb, sel.nrpn_lsb))); }
            98 => { sel.nrpn_lsb = value; sel.active = Some((true, param(sel.nrpn_msb, sel.nrpn_lsb))); }
            101 => {
                sel.rpn_msb = value;
                sel.active = rpn_selection(sel.rpn_msb, sel.rpn_lsb);
            }
            100 => {
                sel.rpn_lsb = value;
                sel.active = rpn_selection(sel.rpn_msb, sel.rpn_lsb);
            }
            6 | 38 | 96 | 97 => {
                if let Some((nrpn, p)) = sel.active {
                    let key = (nrpn, ch, p);
                    let cur = self.param_values.get(&key).copied().unwrap_or(0) as i32;
                    let next = match cc {
                        6 => (value as i32) << 7,
                        38 => (cur & !0x7F) | value as i32,
                        96 => cur + DATA_STEP,
                        _ => cur - DATA_STEP,
                    }
                    .clamp(0, 16383);
                    self.param_values.insert(key, next as u16);
                    let pin = if nrpn { MidiPin::Nrpn { ch: c, param: p } } else { MidiPin::Rpn { ch: c, param: p } };
                    self.set_both(pin, next as f32 / 16383.0);
                }
            }
            // All Sound Off / All Notes Off release every note on the channel.
            120 | 123 => {
                for note in 0..128u8 {
                    if self.note_channels[note as usize] & (1 << ch) != 0 {
                        self.note_off(ch, note);
                    }
                }
            }
            _ => {}
        }
    }

    fn clock_tick(&mut self, timestamp_us: u64, now: Instant) {
        if let Some(last) = self.clock_last_us {
            let dt = timestamp_us.saturating_sub(last) as f64;
            if dt > 0.0 {
                let ema = match self.clock_interval_us {
                    Some(prev) => prev + CLOCK_EMA_ALPHA * (dt - prev),
                    None => dt,
                };
                self.clock_interval_us = Some(ema);
                let bpm = 60_000_000.0 / (ema * CLOCK_PPQN);
                self.set(MidiPin::Bpm, bpm as f32);
            }
        }
        self.clock_last_us = Some(timestamp_us);
        self.clock_seen_at = Some(now);
    }

    fn reset(&mut self) {
        for note in 0..128u8 {
            let mask = self.note_channels[note as usize];
            for ch in 0..16u8 {
                if mask & (1 << ch) != 0 {
                    self.note_off(ch, note);
                }
            }
        }
        self.set(MidiPin::Playing, 0.0);
        self.clock_last_us = None;
        self.clock_interval_us = None;
        self.clock_seen_at = None;
        self.set(MidiPin::Bpm, 0.0);
    }

    /// Emit every pin away from rest, advancing latches and dropping slots that
    /// have returned to rest.
    pub fn poll(&mut self, now: Instant) -> Vec<(String, Signal)> {
        if self.clock_seen_at.is_some_and(|t| now.saturating_duration_since(t) > CLOCK_TIMEOUT) {
            self.clock_last_us = None;
            self.clock_interval_us = None;
            self.clock_seen_at = None;
            self.set(MidiPin::Bpm, 0.0);
        }
        let mut out = Vec::new();
        for slot in self.slots.values_mut() {
            if let Some(sig) = slot.poll(now) {
                if let Some(alias) = &slot.alias {
                    out.push((alias.clone(), sig));
                }
                out.push((slot.id.clone(), sig));
            }
        }
        self.slots.retain(|_, s| !s.at_rest());
        out
    }

    /// The pin the last learnable message addressed, cleared on read.
    pub fn take_last_pin(&mut self) -> Option<MidiPin> {
        self.last_pin.take()
    }

    pub fn take_event_count(&mut self) -> u32 {
        std::mem::take(&mut self.event_count)
    }
}

fn param(msb: u8, lsb: u8) -> u16 {
    ((msb as u16) << 7) | lsb as u16
}

/// RPN 127/127 is the "null" parameter: data entry stops addressing anything.
fn rpn_selection(msb: u8, lsb: u8) -> Option<(bool, u16)> {
    (!(msb == 127 && lsb == 127)).then(|| (false, param(msb, lsb)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn map(v: Vec<(String, Signal)>) -> HashMap<String, Signal> {
        v.into_iter().collect()
    }

    struct Rig {
        dec: InPortDecoder,
        t0: Instant,
        us: u64,
    }

    impl Rig {
        fn new() -> Self {
            Self { dec: InPortDecoder::new(), t0: Instant::now(), us: 0 }
        }
        fn at(&self, ms: u64) -> Instant {
            self.t0 + Duration::from_millis(ms)
        }
        fn feed(&mut self, bytes: &[u8], ms: u64) {
            self.us = ms * 1000;
            let now = self.at(ms);
            self.dec.feed(bytes, self.us, now);
        }
        fn poll(&mut self, ms: u64) -> HashMap<String, Signal> {
            let now = self.at(ms);
            map(self.dec.poll(now))
        }
    }

    /// Aftertouch a sounding note has sent is its live value: easing it back to
    /// nothing reads 0 rather than vanishing, until the note itself ends. The
    /// channel's pressure does the same until the channel's last note ends.
    #[test]
    fn pressure_on_a_held_note_stays_on_the_bus_at_zero_until_it_ends() {
        let mut r = Rig::new();
        r.feed(&[0x92, 60, 100], 1); // note on, ch 3
        r.feed(&[0xA2, 60, 50], 2); // poly aftertouch
        r.feed(&[0xD2, 40], 3); // channel pressure
        r.poll(4);
        r.feed(&[0xA2, 60, 0], 5);
        r.feed(&[0xD2, 0], 6);
        let p = r.poll(7);
        assert_eq!(p.get("midi:pat:3:60"), Some(&Signal::Float(0.0)), "{p:?}");
        assert_eq!(p.get("midi:pat:*:60"), Some(&Signal::Float(0.0)));
        assert_eq!(p.get("midi:cp:3"), Some(&Signal::Float(0.0)));
        r.feed(&[0x82, 60, 0], 8); // note off
        r.poll(9);
        let p = r.poll(10);
        assert!(!p.contains_key("midi:pat:3:60"), "gone with the note: {p:?}");
        assert!(!p.contains_key("midi:cp:3"), "and the channel's last note");
    }

    /// With no note sounding, pressure is an ordinary value: at 0 it is at rest.
    #[test]
    fn pressure_without_a_note_rests_at_zero() {
        let mut r = Rig::new();
        r.feed(&[0xD0, 40], 1);
        r.poll(2);
        r.feed(&[0xD0, 0], 3);
        r.poll(4);
        assert!(!r.poll(5).contains_key("midi:cp:1"));
    }

    #[test]
    fn learning_aftertouch_learns_its_note() {
        let mut r = Rig::new();
        r.feed(&[0x90, 60, 100], 1);
        r.dec.take_last_pin();
        r.feed(&[0xA0, 60, 70], 2);
        assert_eq!(r.dec.take_last_pin(), Some(MidiPin::Note { ch: Channel::Ch(0), note: 60 }));
    }

    #[test]
    fn nothing_is_emitted_at_rest() {
        let mut r = Rig::new();
        assert!(r.poll(0).is_empty());
        r.feed(&[0xB0, 7, 0], 1);
        assert!(r.poll(2).is_empty(), "a CC arriving at 0 is still at rest");
    }

    #[test]
    fn a_held_note_emits_gate_and_velocity_on_its_channel_and_any() {
        let mut r = Rig::new();
        r.feed(&[0x91, 60, 127], 1);
        let p = r.poll(2);
        assert_eq!(p.get("midi:note:2:60"), Some(&Signal::Bool(true)));
        assert_eq!(p.get("midi:note:*:60"), Some(&Signal::Bool(true)));
        assert_eq!(p.get("midi:vel:2:60"), Some(&Signal::Float(1.0)));
        r.feed(&[0x81, 60, 0], 3);
        let p = r.poll(4);
        assert!(p.is_empty(), "released note leaves nothing behind: {p:?}");
    }

    #[test]
    fn a_note_tapped_between_polls_is_seen_for_exactly_one_poll() {
        let mut r = Rig::new();
        r.feed(&[0x90, 36, 100], 1);
        r.feed(&[0x80, 36, 0], 1);
        let p = r.poll(2);
        assert_eq!(p.get("midi:note:1:36"), Some(&Signal::Bool(true)));
        assert!(p.get("midi:vel:1:36").is_some_and(|v| v.as_float() > 0.7));
        assert!(r.poll(3).is_empty());
    }

    #[test]
    fn any_channel_gate_holds_until_the_last_channel_releases() {
        let mut r = Rig::new();
        r.feed(&[0x90, 60, 90], 1);
        r.feed(&[0x91, 60, 90], 1);
        r.poll(2);
        r.feed(&[0x80, 60, 0], 3);
        let p = r.poll(4);
        assert_eq!(p.get("midi:note:*:60"), Some(&Signal::Bool(true)));
        assert_eq!(p.get("midi:note:1:60"), None);
        r.feed(&[0x81, 60, 0], 5);
        assert!(r.poll(6).get("midi:note:*:60").is_none());
    }

    #[test]
    fn a_cc_spike_that_returns_reports_its_peak_once() {
        let mut r = Rig::new();
        r.feed(&[0xB0, 20, 127], 1);
        r.feed(&[0xB0, 20, 0], 1);
        let p = r.poll(2);
        assert_eq!(p.get("midi:cc:1:20"), Some(&Signal::Float(1.0)));
        assert!(r.poll(3).is_empty());
    }

    #[test]
    fn a_resting_knob_keeps_being_emitted() {
        let mut r = Rig::new();
        r.feed(&[0xB2, 74, 64], 1);
        for t in 2..6 {
            let p = r.poll(t);
            assert!(p.get("midi:cc:3:74").is_some_and(|v| (v.as_float() - 64.0 / 127.0).abs() < 1e-6));
        }
    }

    #[test]
    fn legacy_aliases_follow_the_any_channel_pins() {
        let mut r = Rig::new();
        r.feed(&[0xB5, 7, 127], 1);
        r.feed(&[0xE3, 0x00, 0x60], 1);
        let p = r.poll(2);
        assert_eq!(p.get("cc_7"), Some(&Signal::Float(1.0)));
        assert!(p.get("pitch_bend").is_some_and(|v| v.as_float() > 0.0));
    }

    #[test]
    fn learn_picks_the_most_specific_pin_of_the_last_message() {
        let mut r = Rig::new();
        r.feed(&[0x92, 64, 90], 1);
        assert_eq!(r.dec.take_last_pin(), Some(MidiPin::Note { ch: Channel::Ch(2), note: 64 }));
        assert_eq!(r.dec.take_last_pin(), None, "cleared on read");
        r.feed(&[0x92, 64, 0], 2);
        assert_eq!(r.dec.take_last_pin(), None, "a note-off isn't learnable");
        // 14-bit CC: MSB then LSB learns the pair.
        r.feed(&[0xB0, 7, 10], 3);
        r.feed(&[0xB0, 39, 10], 3);
        assert_eq!(r.dec.take_last_pin(), Some(MidiPin::Cc14 { ch: Channel::Ch(0), cc: 7 }));
        // NRPN: selection alone is nothing, data entry learns the parameter.
        r.feed(&[0xB0, 99, 0], 4);
        r.feed(&[0xB0, 98, 5], 4);
        assert_eq!(r.dec.take_last_pin(), None);
        r.feed(&[0xB0, 6, 1], 4);
        assert_eq!(r.dec.take_last_pin(), Some(MidiPin::Nrpn { ch: Channel::Ch(0), param: 5 }));
        r.feed(&[0xB0, 38, 1], 4);
        assert_eq!(r.dec.take_last_pin(), Some(MidiPin::Nrpn { ch: Channel::Ch(0), param: 5 }),
            "data entry LSB addresses the parameter, not a 14-bit CC 6");
        // A lone knob on CC 40 (no CC 8 ever) is a plain CC.
        r.feed(&[0xB1, 40, 64], 5);
        assert_eq!(r.dec.take_last_pin(), Some(MidiPin::Cc { ch: Channel::Ch(1), cc: 40 }));
        assert!(!r.poll(5).keys().any(|k| k.starts_with("midi:cc14:2:")), "no 14-bit twin for a lone LSB");
        r.feed(&[0xFA], 5);
        assert_eq!(r.dec.take_last_pin(), Some(MidiPin::Transport(Transport::Start)));
        r.feed(&[0xF8], 6);
        assert_eq!(r.dec.take_last_pin(), None, "clock isn't learnable");
    }

    #[test]
    fn pulses_last_the_minimum_polls_and_duration() {
        let mut r = Rig::new();
        r.feed(&[0xC0, 5], 0);
        // Many fast polls inside 10 ms: all asserted.
        for t in 1..=9 {
            assert_eq!(r.poll(t).get("midi:pc:1:5"), Some(&Signal::Bool(true)), "t={t}");
        }
        // Past the duration and min polls: released.
        assert!(r.poll(20).get("midi:pc:1:5").is_none());
        assert!(r.dec.slots.is_empty(), "released pulse leaves no slot");

        // Slow polls: still at least PULSE_MIN_POLLS.
        r.feed(&[0xFA], 100);
        assert!(r.poll(200).contains_key("midi:rt:start"));
        assert!(r.poll(300).contains_key("midi:rt:start"));
        assert!(!r.poll(400).contains_key("midi:rt:start"));
    }

    #[test]
    fn transport_drives_playing_state() {
        let mut r = Rig::new();
        r.feed(&[0xFA], 1);
        assert_eq!(r.poll(2).get("midi:rt:playing"), Some(&Signal::Bool(true)));
        r.feed(&[0xFC], 3);
        let p = r.poll(4);
        assert!(p.contains_key("midi:rt:stop"));
        assert!(!p.contains_key("midi:rt:playing"));
    }

    #[test]
    fn fourteen_bit_cc_appears_only_after_an_lsb() {
        let mut r = Rig::new();
        r.feed(&[0xB0, 1, 64], 1);
        assert!(!r.poll(2).contains_key("midi:cc14:1:1"));
        r.feed(&[0xB0, 33, 127], 3);
        let v = r.poll(4)["midi:cc14:1:1"].as_float();
        assert!((v - ((64u16 << 7 | 127) as f32 / 16383.0)).abs() < 1e-6);
        // A new MSB zeroes the LSB.
        r.feed(&[0xB0, 1, 10], 5);
        let v = r.poll(6)["midi:cc14:1:1"].as_float();
        assert!((v - ((10u16 << 7) as f32 / 16383.0)).abs() < 1e-6);
    }

    #[test]
    fn nrpn_and_rpn_data_entry_address_the_selected_parameter() {
        let mut r = Rig::new();
        // NRPN 0x01/0x02 = param 130, data 0x40/0x10.
        for msg in [[0xB0, 99, 1], [0xB0, 98, 2], [0xB0, 6, 0x40], [0xB0, 38, 0x10]] {
            r.feed(&msg, 1);
        }
        let p = r.poll(2);
        let v = p["midi:nrpn:1:130"].as_float();
        assert!((v - ((0x40u16 << 7 | 0x10) as f32 / 16383.0)).abs() < 1e-6);
        // Increment by one MSB step.
        r.feed(&[0xB0, 96, 0], 3);
        let v = r.poll(4)["midi:nrpn:1:130"].as_float();
        assert!((v - (((0x41u16) << 7 | 0x10) as f32 / 16383.0)).abs() < 1e-6);

        // RPN 0 (bend range) then null RPN: data entry stops addressing it.
        for msg in [[0xB1, 101, 0], [0xB1, 100, 0], [0xB1, 6, 12]] {
            r.feed(&msg, 5);
        }
        assert!(r.poll(6).contains_key("midi:rpn:2:0"));
        for msg in [[0xB1, 101, 127], [0xB1, 100, 127], [0xB1, 6, 2]] {
            r.feed(&msg, 7);
        }
        let v = r.poll(8)["midi:rpn:2:0"].as_float();
        assert!((v - ((12u16 << 7) as f32 / 16383.0)).abs() < 1e-6, "null RPN must not overwrite");
    }

    #[test]
    fn all_notes_off_releases_the_channel() {
        let mut r = Rig::new();
        r.feed(&[0x90, 60, 100], 1);
        r.feed(&[0x90, 64, 100], 1);
        r.feed(&[0x91, 67, 100], 1);
        r.poll(2);
        r.feed(&[0xB0, 123, 0], 3);
        let p = r.poll(4);
        assert!(!p.contains_key("midi:note:1:60") && !p.contains_key("midi:note:1:64"));
        assert!(p.contains_key("midi:note:2:67"));
    }

    #[test]
    fn clock_ticks_give_bpm_until_the_clock_stops() {
        let mut r = Rig::new();
        // 120 BPM = 24 ticks per 500 ms → one tick every 20.833 ms. Use µs.
        let tick_us = 20_833u64;
        for i in 0..48u64 {
            let us = i * tick_us;
            r.dec.feed(&[0xF8], us, r.t0 + Duration::from_micros(us));
        }
        let now_ms = 48 * tick_us / 1000;
        let bpm = r.poll(now_ms)["midi:rt:bpm"].as_float();
        assert!((bpm - 120.0).abs() < 0.5, "bpm {bpm}");
        assert!(!r.poll(now_ms + 600).contains_key("midi:rt:bpm"), "clock timeout drops bpm");
    }

    #[test]
    fn sysex_becomes_an_exact_match_pulse_and_bulk_dumps_are_ignored() {
        let mut r = Rig::new();
        r.feed(&[0xF0, 0x43, 0x10, 0xF7], 1);
        assert!(r.poll(2).contains_key("midi:sx:F04310F7"));
        let mut dump = vec![0xF0];
        dump.extend(std::iter::repeat_n(0x01, 200));
        dump.push(0xF7);
        r.feed(&dump, 50);
        assert!(r.poll(51).keys().all(|k| !k.starts_with("midi:sx:F001")));
    }

    #[test]
    fn active_sensing_is_neither_counted_nor_mapped() {
        let mut r = Rig::new();
        r.feed(&[0xFE], 1);
        assert!(r.poll(2).is_empty());
        assert_eq!(r.dec.take_event_count(), 0);
        r.feed(&[0x90, 1, 1], 3);
        assert_eq!(r.dec.take_event_count(), 1);
    }

    #[test]
    fn system_reset_releases_everything() {
        let mut r = Rig::new();
        r.feed(&[0x90, 60, 100], 1);
        r.feed(&[0xFA], 1);
        r.poll(2);
        r.feed(&[0xFF], 30);
        let p = r.poll(31);
        assert!(!p.keys().any(|k| k.starts_with("midi:note") || k == "midi:rt:playing"), "{p:?}");
    }
}
