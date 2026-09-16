//! Feedback-loop protection between MIDI Out and MIDI In ports.
//!
//! A MIDI In and Out on the same port (a loopMIDI port, a DAW routing our
//! output back to us, Windows MIDI Services loopback endpoints) hands every
//! message FlexInput sends straight back to FlexInput. Three layers keep that
//! from running away:
//!
//! 1. **Echo cancelling.** Every sent message is fingerprinted. An input that
//!    exactly matches an unconsumed fingerprint within [`ECHO_WINDOW`], on a
//!    port PAIRED with the sender, is dropped before the decoder sees it.
//!    * Pairs are **seeded** from port names (same normalised name — loopMIDI,
//!      and most hardware whose in/out share a name), so the very first echo
//!      is already cancelled.
//!    * Pairs are **confirmed by observation**: [`CONFIRM_HITS`] exact echoes
//!      from an unpaired In port confirm it, which catches crossed loopbacks
//!      and DAW thru routes whose names don't match.
//!    * Pairs are **demoted** after [`DEMOTE_AFTER_SENDS`] sends without a
//!      single echo (plain hardware that doesn't echo), so a real press that
//!      happens to repeat what we just sent is never swallowed.
//! 2. **Encoder de-duplication** (see `encode.rs`) — a value or gate that comes
//!    back unchanged produces no new send, so loops that return what was sent
//!    die out by themselves even before a pair is confirmed.
//! 3. **Loop breaker.** What survives both is a loop that TRANSFORMS discrete
//!    events on the way round (e.g. PC 1 → PC 2 here, PC 2 → PC 1 in a DAW),
//!    which ping-pongs at the tick rate. When an Out port sends at least
//!    [`BREAKER_EVENTS`] discrete events (notes, program changes, transport,
//!    SysEx) in each of two consecutive [`BREAKER_WINDOW`]s, nearly all of them
//!    right after MIDI input arrived, it is muted until the user resets it.
//!    Continuous values are not counted: a performer sweeping a controller
//!    through a MIDI→MIDI mapping is legitimate, and cannot run away anyway.
//!
//! All state is session-scoped and clock-injected for tests.

use std::collections::{HashMap, VecDeque};
use std::time::{Duration, Instant};

use flexinput_core::midi::MidiMessage;

/// How long after a send an identical input still counts as its echo.
pub const ECHO_WINDOW: Duration = Duration::from_millis(50);
/// Unmatched echoes from an unpaired In port that confirm the pair.
pub const CONFIRM_HITS: u32 = 3;
/// Sends without any echo that demote a seeded/confirmed pair.
pub const DEMOTE_AFTER_SENDS: u32 = 32;
/// Breaker measurement window.
pub const BREAKER_WINDOW: Duration = Duration::from_secs(1);
/// Discrete events per window that make a window "hot".
pub const BREAKER_EVENTS: u32 = 100;
/// A send is "reactive" when MIDI input arrived at most this long before it.
pub const REACT_WINDOW: Duration = Duration::from_millis(20);
/// Fingerprints kept per Out port (older ones fall out regardless of age).
const FINGERPRINT_CAP: usize = 512;

/// How an Out→In port pair is currently treated.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PairStatus {
    /// Names match; echoes are cancelled until proven otherwise.
    Seeded,
    /// Echoes were observed; they are cancelled.
    Confirmed,
    /// No echoes despite many sends; nothing is cancelled.
    Demoted,
}

/// One pair, for status display.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PairInfo {
    pub out_id: String,
    pub in_id: String,
    pub status: PairStatus,
}

#[derive(Default)]
struct Pair {
    /// `None` = unpaired candidate collecting evidence.
    status: Option<PairStatus>,
    hits: u32,
    sends_since_echo: u32,
}

impl Pair {
    fn cancels(&self) -> bool {
        matches!(self.status, Some(PairStatus::Seeded | PairStatus::Confirmed))
    }
}

struct Fingerprint {
    msg: MidiMessage,
    at: Instant,
    consumed: bool,
}

struct Breaker {
    window_start: Instant,
    events: u32,
    reactive: u32,
    prev_hot: bool,
    tripped: bool,
}

impl Breaker {
    fn new(now: Instant) -> Self {
        Self { window_start: now, events: 0, reactive: 0, prev_hot: false, tripped: false }
    }

    fn hot(&self) -> bool {
        self.events >= BREAKER_EVENTS && self.reactive * 10 >= self.events * 9
    }
}

#[derive(Default)]
pub struct LoopGuard {
    in_names: HashMap<String, String>,
    out_names: HashMap<String, String>,
    sent: HashMap<String, VecDeque<Fingerprint>>,
    /// Keyed by (out id, in id).
    pairs: HashMap<(String, String), Pair>,
    breakers: HashMap<String, Breaker>,
    last_input_at: Option<Instant>,
    new_trips: Vec<String>,
}

/// Port name reduced to what an In and Out of the same device share:
/// case-folded, trimmed, with Windows' `MIDIIN2 (…)` / `MIDIOUT2 (…)`
/// multi-port wrappers removed.
pub fn normalise_port_name(name: &str) -> String {
    let n = name.trim();
    let lower = n.to_ascii_lowercase();
    for prefix in ["midiin", "midiout"] {
        if let Some(rest) = lower.strip_prefix(prefix) {
            let rest = rest.trim_start_matches(|c: char| c.is_ascii_digit()).trim_start();
            if let Some(inner) = rest.strip_prefix('(').and_then(|r| r.strip_suffix(')')) {
                return inner.trim().to_string();
            }
        }
    }
    lower
}

/// Events that can ping-pong forever through a transforming loop.
fn is_discrete(msg: &MidiMessage) -> bool {
    matches!(
        msg,
        MidiMessage::NoteOn { .. } | MidiMessage::NoteOff { .. } | MidiMessage::ProgramChange { .. }
            | MidiMessage::Start | MidiMessage::Stop | MidiMessage::Continue | MidiMessage::SysEx(_)
    )
}

impl LoopGuard {
    pub fn new() -> Self {
        Self::default()
    }

    /// An In port was opened.
    pub fn register_in(&mut self, in_id: &str, name: &str) {
        self.in_names.insert(in_id.to_string(), normalise_port_name(name));
        let outs: Vec<String> = self.out_names.keys().cloned().collect();
        for out_id in outs {
            self.seed(&out_id, in_id);
        }
    }

    /// An Out port was opened.
    pub fn register_out(&mut self, out_id: &str, name: &str, now: Instant) {
        self.out_names.insert(out_id.to_string(), normalise_port_name(name));
        self.breakers.entry(out_id.to_string()).or_insert_with(|| Breaker::new(now));
        let ins: Vec<String> = self.in_names.keys().cloned().collect();
        for in_id in ins {
            self.seed(out_id, &in_id);
        }
    }

    fn seed(&mut self, out_id: &str, in_id: &str) {
        if self.out_names.get(out_id) != self.in_names.get(in_id) {
            return;
        }
        let pair = self.pairs.entry((out_id.to_string(), in_id.to_string())).or_default();
        if pair.status.is_none() {
            pair.status = Some(PairStatus::Seeded);
        }
    }

    /// A port was closed. Its fingerprints and pairs go with it; a muted Out
    /// port stays muted until [`Self::reset_breaker`] (reopening a runaway
    /// route must not silently restart the loop).
    pub fn unregister(&mut self, id: &str) {
        self.in_names.remove(id);
        self.out_names.remove(id);
        self.sent.remove(id);
        self.pairs.retain(|(o, i), _| o != id && i != id);
        if self.breakers.get(id).is_some_and(|b| !b.tripped) {
            self.breakers.remove(id);
        }
    }

    /// Record an input on `in_id`. Returns `true` when it is an echo of our own
    /// output and must be dropped.
    pub fn on_receive(&mut self, in_id: &str, bytes: &[u8], now: Instant) -> bool {
        let Some(msg) = MidiMessage::parse(bytes, &mut None) else { return false };
        if msg == MidiMessage::ActiveSensing {
            return false;
        }
        self.last_input_at = Some(now);

        // Find the matching fingerprint, preferring an Out port already paired
        // with this input.
        let mut paired_match: Option<(String, usize)> = None;
        let mut other_match: Option<(String, usize)> = None;
        for (out_id, ring) in &mut self.sent {
            while ring.front().is_some_and(|f| now.saturating_duration_since(f.at) > ECHO_WINDOW) {
                ring.pop_front();
            }
            let Some(idx) = ring.iter().position(|f| !f.consumed && f.msg == msg) else { continue };
            let cancels = self.pairs.get(&(out_id.clone(), in_id.to_string())).is_some_and(Pair::cancels);
            if cancels {
                paired_match = Some((out_id.clone(), idx));
                break;
            }
            if other_match.is_none() {
                other_match = Some((out_id.clone(), idx));
            }
        }

        let Some((out_id, idx)) = paired_match.or(other_match) else { return false };
        if let Some(ring) = self.sent.get_mut(&out_id) {
            ring[idx].consumed = true;
        }
        let pair = self.pairs.entry((out_id, in_id.to_string())).or_default();
        pair.hits += 1;
        pair.sends_since_echo = 0;
        if !pair.cancels() && pair.hits >= CONFIRM_HITS {
            pair.status = Some(PairStatus::Confirmed);
        }
        pair.cancels()
    }

    /// Record a message about to be sent on `out_id`. Returns `false` when the
    /// port is muted by the loop breaker (don't send it).
    pub fn on_send(&mut self, out_id: &str, msg: &MidiMessage, now: Instant) -> bool {
        let breaker = self.breakers.entry(out_id.to_string()).or_insert_with(|| Breaker::new(now));
        if breaker.tripped {
            return false;
        }
        if now.saturating_duration_since(breaker.window_start) >= BREAKER_WINDOW {
            breaker.prev_hot = breaker.hot();
            breaker.window_start = now;
            breaker.events = 0;
            breaker.reactive = 0;
        }
        if is_discrete(msg) {
            breaker.events += 1;
            if self.last_input_at.is_some_and(|t| now.saturating_duration_since(t) <= REACT_WINDOW) {
                breaker.reactive += 1;
            }
            if breaker.prev_hot && breaker.hot() {
                breaker.tripped = true;
                self.new_trips.push(out_id.to_string());
                return false;
            }
        }

        let ring = self.sent.entry(out_id.to_string()).or_default();
        if ring.len() >= FINGERPRINT_CAP {
            ring.pop_front();
        }
        ring.push_back(Fingerprint { msg: msg.clone(), at: now, consumed: false });

        for ((o, _), pair) in self.pairs.iter_mut() {
            if o != out_id {
                continue;
            }
            pair.sends_since_echo += 1;
            if pair.sends_since_echo >= DEMOTE_AFTER_SENDS {
                if pair.cancels() {
                    pair.status = Some(PairStatus::Demoted);
                }
                pair.hits = 0;
            }
        }
        true
    }

    pub fn is_muted(&self, out_id: &str) -> bool {
        self.breakers.get(out_id).is_some_and(|b| b.tripped)
    }

    /// Unmute a port the breaker tripped.
    pub fn reset_breaker(&mut self, out_id: &str, now: Instant) {
        self.breakers.insert(out_id.to_string(), Breaker::new(now));
    }

    /// Out ports muted since the last call.
    pub fn take_trips(&mut self) -> Vec<String> {
        std::mem::take(&mut self.new_trips)
    }

    /// Every pair currently known (seeded, confirmed or demoted), sorted.
    pub fn pairs(&self) -> Vec<PairInfo> {
        let mut v: Vec<PairInfo> = self.pairs.iter()
            .filter_map(|((o, i), p)| p.status.map(|status| PairInfo {
                out_id: o.clone(),
                in_id: i.clone(),
                status,
            }))
            .collect();
        v.sort_by(|a, b| (&a.out_id, &a.in_id).cmp(&(&b.out_id, &b.in_id)));
        v
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOTE: MidiMessage = MidiMessage::NoteOn { ch: 0, note: 60, vel: 100 };

    struct Rig {
        g: LoopGuard,
        t0: Instant,
    }

    impl Rig {
        fn new() -> Self {
            Self { g: LoopGuard::new(), t0: Instant::now() }
        }
        fn at(&self, ms: u64) -> Instant {
            self.t0 + Duration::from_millis(ms)
        }
        fn send(&mut self, out: &str, msg: &MidiMessage, ms: u64) -> bool {
            let now = self.at(ms);
            self.g.on_send(out, msg, now)
        }
        fn recv(&mut self, inp: &str, msg: &MidiMessage, ms: u64) -> bool {
            let now = self.at(ms);
            self.g.on_receive(inp, &msg.to_bytes(), now)
        }
        fn status(&self, out: &str, inp: &str) -> Option<PairStatus> {
            self.g.pairs().into_iter().find(|p| p.out_id == out && p.in_id == inp).map(|p| p.status)
        }
    }

    #[test]
    fn port_names_normalise_across_windows_wrappers() {
        assert_eq!(normalise_port_name("MIDIIN2 (Launchpad Pro)"), "launchpad pro");
        assert_eq!(normalise_port_name("MIDIOUT2 (Launchpad Pro)"), "launchpad pro");
        assert_eq!(normalise_port_name(" loopMIDI Port "), "loopmidi port");
        assert_ne!(normalise_port_name("Port A"), normalise_port_name("Port B"));
    }

    #[test]
    fn a_same_named_pair_cancels_the_very_first_echo() {
        let mut r = Rig::new();
        r.g.register_in("midi_in:0", "loopMIDI Port");
        r.g.register_out("midi_out:0", "loopMIDI Port", r.t0);
        assert_eq!(r.status("midi_out:0", "midi_in:0"), Some(PairStatus::Seeded));
        assert!(r.send("midi_out:0", &NOTE, 0));
        assert!(r.recv("midi_in:0", &NOTE, 1), "echo dropped");
        assert!(!r.recv("midi_in:0", &NOTE, 2), "fingerprint consumed: a second one is real input");
    }

    #[test]
    fn note_off_matches_its_note_on_zero_velocity_spelling() {
        let mut r = Rig::new();
        r.g.register_in("midi_in:0", "P");
        r.g.register_out("midi_out:0", "P", r.t0);
        r.send("midi_out:0", &MidiMessage::NoteOff { ch: 0, note: 60, vel: 0 }, 0);
        let now = r.at(1);
        assert!(r.g.on_receive("midi_in:0", &[0x90, 60, 0], now));
    }

    #[test]
    fn echoes_outside_the_window_or_with_different_bytes_pass() {
        let mut r = Rig::new();
        r.g.register_in("midi_in:0", "P");
        r.g.register_out("midi_out:0", "P", r.t0);
        r.send("midi_out:0", &NOTE, 0);
        assert!(!r.recv("midi_in:0", &MidiMessage::NoteOn { ch: 0, note: 61, vel: 100 }, 1));
        assert!(!r.recv("midi_in:0", &NOTE, 60), "older than the echo window");
    }

    #[test]
    fn an_unpaired_loop_is_confirmed_by_repeated_echoes() {
        let mut r = Rig::new();
        r.g.register_in("midi_in:1", "Loopback (B)");
        r.g.register_out("midi_out:0", "Loopback (A)", r.t0);
        assert_eq!(r.status("midi_out:0", "midi_in:1"), None);
        for i in 0..CONFIRM_HITS as u64 - 1 {
            r.send("midi_out:0", &NOTE, i * 10);
            assert!(!r.recv("midi_in:1", &NOTE, i * 10 + 1), "not yet confirmed: passes");
        }
        let t = CONFIRM_HITS as u64 * 10;
        r.send("midi_out:0", &NOTE, t);
        assert!(r.recv("midi_in:1", &NOTE, t + 1), "confirming echo is dropped");
        assert_eq!(r.status("midi_out:0", "midi_in:1"), Some(PairStatus::Confirmed));
    }

    #[test]
    fn a_pair_that_never_echoes_is_demoted_so_real_input_passes() {
        let mut r = Rig::new();
        r.g.register_in("midi_in:0", "nanoKONTROL2");
        r.g.register_out("midi_out:0", "nanoKONTROL2", r.t0);
        for i in 0..DEMOTE_AFTER_SENDS as u64 {
            r.send("midi_out:0", &MidiMessage::ControlChange { ch: 0, cc: 1, value: i as u8 }, i);
        }
        assert_eq!(r.status("midi_out:0", "midi_in:0"), Some(PairStatus::Demoted));
        // The user presses what we just sent: must not be swallowed.
        r.send("midi_out:0", &NOTE, 100);
        assert!(!r.recv("midi_in:0", &NOTE, 101));
    }

    #[test]
    fn echoes_keep_a_pair_alive() {
        let mut r = Rig::new();
        r.g.register_in("midi_in:0", "P");
        r.g.register_out("midi_out:0", "P", r.t0);
        for i in 0..(DEMOTE_AFTER_SENDS as u64 * 3) {
            let m = MidiMessage::ControlChange { ch: 0, cc: 1, value: (i % 128) as u8 };
            r.send("midi_out:0", &m, i * 2);
            assert!(r.recv("midi_in:0", &m, i * 2 + 1));
        }
        assert_eq!(r.status("midi_out:0", "midi_in:0"), Some(PairStatus::Seeded));
    }

    #[test]
    fn a_transforming_discrete_loop_trips_the_breaker() {
        let mut r = Rig::new();
        r.g.register_out("midi_out:0", "Out", r.t0);
        // Ping-pong: every send is 1 ms after an input, 300 events/s.
        let mut t = 0u64;
        let mut sent_ok = true;
        while t < 3000 && sent_ok {
            r.recv("midi_in:5", &MidiMessage::ProgramChange { ch: 0, program: 2 }, t);
            sent_ok = r.send("midi_out:0", &MidiMessage::ProgramChange { ch: 0, program: 1 }, t + 1);
            t += 3;
        }
        assert!(!sent_ok, "breaker must trip");
        assert!(t <= 2100, "trips within about two windows, t={t}");
        assert!(r.g.is_muted("midi_out:0"));
        assert_eq!(r.g.take_trips(), vec!["midi_out:0".to_string()]);
        assert!(!r.send("midi_out:0", &NOTE, t + 10), "stays muted");
        r.g.reset_breaker("midi_out:0", r.at(t + 20));
        assert!(r.send("midi_out:0", &NOTE, t + 30));
    }

    #[test]
    fn fast_output_not_driven_by_midi_input_never_trips() {
        let mut r = Rig::new();
        r.g.register_out("midi_out:0", "Out", r.t0);
        // A gamepad turbo → note at 500 events/s for 5 s, no MIDI input at all.
        for i in 0..2500u64 {
            let m = if i % 2 == 0 { NOTE } else { MidiMessage::NoteOff { ch: 0, note: 60, vel: 0 } };
            assert!(r.send("midi_out:0", &m, i * 2), "tripped at {i}");
        }
    }

    #[test]
    fn continuous_streams_never_trip_even_when_input_driven() {
        let mut r = Rig::new();
        r.g.register_out("midi_out:0", "Out", r.t0);
        for i in 0..5000u64 {
            let m = MidiMessage::ControlChange { ch: 0, cc: 7, value: (i % 128) as u8 };
            r.recv("midi_in:0", &MidiMessage::ChannelPressure { ch: 0, value: (i % 128) as u8 }, i);
            assert!(r.send("midi_out:0", &m, i), "tripped at {i}");
        }
    }

    #[test]
    fn closing_a_port_forgets_its_pairs_but_not_a_trip() {
        let mut r = Rig::new();
        r.g.register_in("midi_in:0", "P");
        r.g.register_out("midi_out:0", "P", r.t0);
        r.g.unregister("midi_in:0");
        assert!(r.g.pairs().is_empty());
        r.g.breakers.get_mut("midi_out:0").unwrap().tripped = true;
        r.g.unregister("midi_out:0");
        assert!(r.g.is_muted("midi_out:0"));
    }
}
