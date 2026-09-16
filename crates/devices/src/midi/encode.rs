//! Bus pins → outgoing MIDI messages, one encoder per MIDI Out port.
//!
//! The engine hands over the full set of pins a sink wants on this port every
//! tick; the encoder turns LEVELS into MIDI's EVENTS:
//!
//! * note gates send NoteOn on the rising edge (velocity from the matching
//!   `midi:vel` pin, else [`DEFAULT_VELOCITY`]) and NoteOff on the falling edge;
//! * value pins send only when their QUANTISED value changes, so float noise
//!   below one MIDI step never floods the port;
//! * pulses (Program Change, transport, SysEx) send on the rising edge;
//! * a pin that disappears from the set is released: a sounding note gets its
//!   NoteOff and a value away from rest is sent back to rest. The same path
//!   runs for bypass (an empty set), so FlexInput never leaves a stuck note.
//!
//! Every pin starts out assumed at rest, so loading a patch whose mappings
//! idle at zero sends nothing.

use std::collections::HashMap;

use flexinput_core::midi::{self, Channel, MidiMessage, MidiPin, Transport};
use flexinput_core::Signal;

/// NoteOn velocity when no `midi:vel` pin accompanies a note gate.
pub const DEFAULT_VELOCITY: u8 = 100;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Sent {
    /// Note gate / pulse level last seen.
    Level(bool),
    /// Quantised value last sent (7- or 14-bit, or raw bend).
    Value(u16),
}

#[derive(Default)]
pub struct OutPortEncoder {
    sent: HashMap<MidiPin, Sent>,
}

impl OutPortEncoder {
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether any note is currently sounding (NoteOn sent, no NoteOff yet).
    pub fn has_sounding_notes(&self) -> bool {
        self.sent.iter().any(|(p, s)| matches!(p, MidiPin::Note { .. }) && *s == Sent::Level(true))
    }

    /// Encode one tick's desired pins. Pins that can't be written to a port
    /// (unknown ids, "any channel", derived state) are ignored.
    pub fn apply(&mut self, desired: &[(String, Signal)]) -> Vec<MidiMessage> {
        let mut velocities: HashMap<(u8, u8), f32> = HashMap::new();
        let mut pins: Vec<(MidiPin, Signal)> = Vec::with_capacity(desired.len());
        for (id, sig) in desired {
            let Some(pin) = output_pin(id) else { continue };
            if let MidiPin::Velocity { ch: Channel::Ch(ch), note } = pin {
                velocities.insert((ch, note), sig.as_float());
                continue;
            }
            if pin.is_output_capable() {
                pins.push((pin, *sig));
            }
        }
        // Deterministic order within a tick regardless of HashMap iteration.
        pins.sort_by(|a, b| a.0.cmp(&b.0));

        let mut out = Vec::new();
        let mut present: Vec<MidiPin> = Vec::with_capacity(pins.len());
        for (pin, sig) in pins {
            let prev = self.sent.get(&pin).copied();
            if let Some(next) = encode_pin(&pin, sig, prev, &velocities, &mut out) {
                self.sent.insert(pin.clone(), next);
            }
            present.push(pin);
        }

        let mut dropped: Vec<MidiPin> = self.sent.keys()
            .filter(|p| !present.contains(p))
            .cloned()
            .collect();
        dropped.sort();
        for pin in dropped {
            if let Some(prev) = self.sent.remove(&pin) {
                release_pin(&pin, prev, &mut out);
            }
        }
        out
    }
}

/// Resolve an id a MIDI Out sink may carry: the bus grammar, or the legacy
/// `cc_<n>` pins of pre-AutoMap MIDI Out nodes (always sent on channel 1).
fn output_pin(id: &str) -> Option<MidiPin> {
    if let Some(pin) = midi::parse_pin(id) {
        return Some(pin);
    }
    match midi::legacy_pin(id)? {
        MidiPin::Cc { cc, .. } => Some(MidiPin::Cc { ch: Channel::Ch(0), cc }),
        _ => None,
    }
}

fn rest_sent(pin: &MidiPin) -> Sent {
    match pin {
        MidiPin::PitchBend { .. } => Sent::Value(midi::bend_to_14bit(0.0)),
        MidiPin::Note { .. } | MidiPin::ProgramChange { .. } | MidiPin::Transport(_) | MidiPin::SysEx(_) => {
            Sent::Level(false)
        }
        _ => Sent::Value(0),
    }
}

/// Encode `pin` at `sig` given what was last sent; returns the new sent state.
fn encode_pin(
    pin: &MidiPin,
    sig: Signal,
    prev: Option<Sent>,
    velocities: &HashMap<(u8, u8), f32>,
    out: &mut Vec<MidiMessage>,
) -> Option<Sent> {
    let prev = prev.unwrap_or_else(|| rest_sent(pin));
    let ch_of = |c: &Channel| c.index().unwrap_or(0);
    match pin {
        MidiPin::Note { ch, note } => {
            let on = sig.as_bool();
            let ch = ch_of(ch);
            match (prev, on) {
                (Sent::Level(false), true) => {
                    let vel = velocities.get(&(ch, *note))
                        .map(|v| midi::unit_to_7bit(*v).max(1))
                        .unwrap_or(DEFAULT_VELOCITY);
                    out.push(MidiMessage::NoteOn { ch, note: *note, vel });
                }
                (Sent::Level(true), false) => out.push(MidiMessage::NoteOff { ch, note: *note, vel: 0 }),
                _ => {}
            }
            Some(Sent::Level(on))
        }
        MidiPin::ProgramChange { .. } | MidiPin::Transport(_) | MidiPin::SysEx(_) => {
            let on = sig.as_bool();
            if on && prev == Sent::Level(false) {
                out.push(match pin {
                    MidiPin::ProgramChange { ch, program } => {
                        MidiMessage::ProgramChange { ch: ch_of(ch), program: *program }
                    }
                    MidiPin::Transport(Transport::Start) => MidiMessage::Start,
                    MidiPin::Transport(Transport::Stop) => MidiMessage::Stop,
                    MidiPin::Transport(Transport::Continue) => MidiMessage::Continue,
                    MidiPin::SysEx(bytes) => MidiMessage::SysEx(bytes.clone()),
                    _ => unreachable!(),
                });
            }
            Some(Sent::Level(on))
        }
        _ => {
            let q = quantise(pin, sig);
            if prev != Sent::Value(q) {
                value_messages(pin, q, out);
            }
            Some(Sent::Value(q))
        }
    }
}

fn release_pin(pin: &MidiPin, prev: Sent, out: &mut Vec<MidiMessage>) {
    match (pin, prev) {
        (MidiPin::Note { ch, note }, Sent::Level(true)) => {
            out.push(MidiMessage::NoteOff { ch: ch.index().unwrap_or(0), note: *note, vel: 0 });
        }
        (_, Sent::Value(v)) => {
            if let Sent::Value(rest) = rest_sent(pin) {
                if v != rest {
                    value_messages(pin, rest, out);
                }
            }
        }
        _ => {}
    }
}

fn quantise(pin: &MidiPin, sig: Signal) -> u16 {
    let v = sig.as_float();
    match pin {
        MidiPin::PitchBend { .. } => midi::bend_to_14bit(v),
        MidiPin::Cc14 { .. } | MidiPin::Nrpn { .. } | MidiPin::Rpn { .. } => midi::unit_to_14bit(v),
        _ => midi::unit_to_7bit(v) as u16,
    }
}

fn value_messages(pin: &MidiPin, q: u16, out: &mut Vec<MidiMessage>) {
    let cc = |ch: u8, cc: u8, value: u16| MidiMessage::ControlChange { ch, cc, value: (value & 0x7F) as u8 };
    match pin {
        MidiPin::Cc { ch, cc: n } => out.push(cc(ch.index().unwrap_or(0), *n, q)),
        MidiPin::Cc14 { ch, cc: n } => {
            let ch = ch.index().unwrap_or(0);
            out.push(cc(ch, *n, q >> 7));
            out.push(cc(ch, *n + 32, q));
        }
        MidiPin::Nrpn { ch, param } | MidiPin::Rpn { ch, param } => {
            let ch = ch.index().unwrap_or(0);
            let (msb_cc, lsb_cc) = if matches!(pin, MidiPin::Nrpn { .. }) { (99, 98) } else { (101, 100) };
            out.push(cc(ch, msb_cc, *param >> 7));
            out.push(cc(ch, lsb_cc, *param));
            out.push(cc(ch, 6, q >> 7));
            out.push(cc(ch, 38, q));
        }
        MidiPin::PitchBend { ch } => out.push(MidiMessage::PitchBend { ch: ch.index().unwrap_or(0), value: q }),
        MidiPin::ChannelPressure { ch } => {
            out.push(MidiMessage::ChannelPressure { ch: ch.index().unwrap_or(0), value: q as u8 });
        }
        MidiPin::PolyAftertouch { ch, note } => {
            out.push(MidiMessage::PolyAftertouch { ch: ch.index().unwrap_or(0), note: *note, value: q as u8 });
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(pins: &[(&str, Signal)]) -> Vec<(String, Signal)> {
        pins.iter().map(|(p, s)| (p.to_string(), *s)).collect()
    }

    #[test]
    fn note_gate_edges_send_on_and_off_with_velocity() {
        let mut e = OutPortEncoder::new();
        let on = e.apply(&d(&[("midi:note:2:60", Signal::Bool(true)), ("midi:vel:2:60", Signal::Float(0.5))]));
        assert_eq!(on, vec![MidiMessage::NoteOn { ch: 1, note: 60, vel: 64 }]);
        assert!(e.has_sounding_notes());
        // Held: nothing more.
        assert!(e.apply(&d(&[("midi:note:2:60", Signal::Bool(true))])).is_empty());
        let off = e.apply(&d(&[("midi:note:2:60", Signal::Bool(false))]));
        assert_eq!(off, vec![MidiMessage::NoteOff { ch: 1, note: 60, vel: 0 }]);
        assert!(!e.has_sounding_notes());
    }

    #[test]
    fn a_note_without_a_velocity_pin_uses_the_default() {
        let mut e = OutPortEncoder::new();
        let on = e.apply(&d(&[("midi:note:1:36", Signal::Bool(true))]));
        assert_eq!(on, vec![MidiMessage::NoteOn { ch: 0, note: 36, vel: DEFAULT_VELOCITY }]);
    }

    #[test]
    fn a_dropped_or_bypassed_note_is_released() {
        let mut e = OutPortEncoder::new();
        e.apply(&d(&[("midi:note:1:40", Signal::Bool(true)), ("midi:cc:1:7", Signal::Float(1.0))]));
        let released = e.apply(&[]);
        assert_eq!(released, vec![
            MidiMessage::NoteOff { ch: 0, note: 40, vel: 0 },
            MidiMessage::ControlChange { ch: 0, cc: 7, value: 0 },
        ]);
        assert!(e.apply(&[]).is_empty(), "release happens once");
    }

    #[test]
    fn values_send_only_when_the_quantised_value_changes() {
        let mut e = OutPortEncoder::new();
        assert!(e.apply(&d(&[("midi:cc:1:1", Signal::Float(0.0))])).is_empty(), "rest is assumed");
        assert_eq!(e.apply(&d(&[("midi:cc:1:1", Signal::Float(0.5))])).len(), 1);
        assert!(e.apply(&d(&[("midi:cc:1:1", Signal::Float(0.501))])).is_empty(), "sub-step noise");
        assert_eq!(e.apply(&d(&[("midi:cc:1:1", Signal::Float(0.6))])).len(), 1);
    }

    #[test]
    fn pitch_bend_rests_at_centre() {
        let mut e = OutPortEncoder::new();
        assert!(e.apply(&d(&[("midi:pb:1", Signal::Float(0.0))])).is_empty());
        let m = e.apply(&d(&[("midi:pb:1", Signal::Float(1.0))]));
        assert_eq!(m, vec![MidiMessage::PitchBend { ch: 0, value: 16383 }]);
        assert_eq!(e.apply(&[]), vec![MidiMessage::PitchBend { ch: 0, value: 8192 }]);
    }

    #[test]
    fn fourteen_bit_and_parameter_numbers_send_their_sequences() {
        let mut e = OutPortEncoder::new();
        let m = e.apply(&d(&[("midi:cc14:1:1", Signal::Float(1.0))]));
        assert_eq!(m, vec![
            MidiMessage::ControlChange { ch: 0, cc: 1, value: 127 },
            MidiMessage::ControlChange { ch: 0, cc: 33, value: 127 },
        ]);
        let m = e.apply(&d(&[("midi:nrpn:3:130", Signal::Float(1.0))]));
        assert_eq!(m[..4], [
            MidiMessage::ControlChange { ch: 2, cc: 99, value: 1 },
            MidiMessage::ControlChange { ch: 2, cc: 98, value: 2 },
            MidiMessage::ControlChange { ch: 2, cc: 6, value: 127 },
            MidiMessage::ControlChange { ch: 2, cc: 38, value: 127 },
        ]);
        let m = e.apply(&d(&[("midi:nrpn:3:130", Signal::Float(1.0)), ("midi:rpn:1:0", Signal::Float(0.5))]));
        assert_eq!(m[0], MidiMessage::ControlChange { ch: 0, cc: 101, value: 0 });
    }

    #[test]
    fn pulses_fire_once_per_rising_edge() {
        let mut e = OutPortEncoder::new();
        let pins = |on: bool| d(&[
            ("midi:pc:1:9", Signal::Bool(on)),
            ("midi:rt:start", Signal::Bool(on)),
            ("midi:sx:F07EF7", Signal::Bool(on)),
        ]);
        assert_eq!(e.apply(&pins(true)).len(), 3);
        assert!(e.apply(&pins(true)).is_empty());
        assert!(e.apply(&pins(false)).is_empty());
        assert_eq!(e.apply(&pins(true)).len(), 3);
    }

    #[test]
    fn unwritable_pins_are_ignored_and_legacy_cc_goes_to_channel_one() {
        let mut e = OutPortEncoder::new();
        let m = e.apply(&d(&[
            ("midi:note:*:60", Signal::Bool(true)),
            ("midi:rt:bpm", Signal::Float(120.0)),
            ("btn_south", Signal::Bool(true)),
            ("cc_7", Signal::Float(1.0)),
        ]));
        assert_eq!(m, vec![MidiMessage::ControlChange { ch: 0, cc: 7, value: 127 }]);
    }
}
