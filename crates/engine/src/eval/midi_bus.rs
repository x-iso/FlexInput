//! MIDI pins on the AutoMap bus.
//!
//! MIDI pins (`midi:…`, see `flexinput_core::midi`) are dynamic: there are far
//! too many to list in `automap::ALL_PINS`, and a MIDI In port only publishes
//! the ones away from rest. So wherever the bus falls back to a RAW device for
//! canonical pins, a MIDI device's live pins are copied in as well
//! ([`fill_raw_dynamic`]); collector-to-collector pass-through already copies
//! every entry, MIDI included. A raw pad's own ports (`extra:` pins, see
//! `flexinput_core::raw_pad`) ride the same way.
//!
//! **Provenance.** A MIDI Out sink must not echo raw MIDI straight back out —
//! with In and Out on the same port that is an instant feedback loop. So the
//! sink forwards a `midi:` pin only when something PRODUCED it (a mapping card,
//! a Collector input) or when the sink's Thru toggle is on. Producers mark
//! their pins with [`mark_produced`]; the marker is an ordinary bus entry, so
//! every pass-through that copies a collector's entries carries it along.

use super::*;

use flexinput_core::{midi, raw_pad};

/// `collector_sigs` pin prefix marking a `midi:` pin as produced on that key.
pub(crate) const MIDI_PRODUCED_PREFIX: &str = "__midi_out__:";

/// A physical MIDI input port id.
pub(crate) fn is_midi_device(dev_id: &str) -> bool {
    dev_id.starts_with("midi_in:")
}

/// Copy `dev_id`'s live dynamic bus pins into `key`, never overwriting an
/// entry the key already carries (an upstream producer's value wins over
/// raw): a MIDI input port's `midi:` pins, or the `extra:` pins of a raw pad's
/// elements kept as ports of their own (`flexinput_core::raw_pad`) — neither
/// fits in `automap::ALL_PINS`. A no-op for any other device. Legacy
/// `cc_<n>` / `pitch_bend` aliases and a raw pad's per-element `raw:` pins
/// stay off the bus.
pub(crate) fn fill_raw_dynamic(
    dev_sigs: &SigMap,
    dev_id: &str,
    key: &str,
    collector_sigs: &mut SigMap,
) {
    let wanted: fn(&str) -> bool = if is_midi_device(dev_id) {
        midi::is_midi_pin
    } else if raw_pad::is_raw_device_id(dev_id) {
        raw_pad::is_extra_pin
    } else {
        return;
    };
    for ((d, pin), &sig) in dev_sigs.iter() {
        if d != dev_id || !wanted(pin) {
            continue;
        }
        collector_sigs.set_pin_if_absent(key, pin, sig);
    }
}

/// A raw pad's `extra:` pins, for a mapping module wired straight to the pad
/// (with a collector upstream, `fill_upstream_extra` already finds them on
/// the bus). Adds each to `upstream` and `names` unless already there.
pub(crate) fn fill_upstream_raw_extra(
    dev_id: &str,
    dev_sigs: &SigMap,
    upstream: &mut PinSigs,
    names: &mut Vec<String>,
) {
    if !raw_pad::is_raw_device_id(dev_id) {
        return;
    }
    for ((d, pin), &sig) in dev_sigs.iter() {
        if d != dev_id || !raw_pad::is_extra_pin(pin) || upstream.contains_key(pin.as_str()) {
            continue;
        }
        upstream.insert(flexinput_core::pooled_string(pin), sig);
        names.push(pin.clone());
    }
}

/// Mark a `midi:` pin on `key` as produced (see the module docs). No-op for
/// other pins.
pub(crate) fn mark_produced(key: &str, pin: &str, collector_sigs: &mut SigMap) {
    if midi::is_midi_pin(pin) {
        collector_sigs.set_pin(&key, &format!("{MIDI_PRODUCED_PREFIX}{pin}"), Signal::Bool(true));
    }
}

/// Add the MIDI pins an upstream bus carries to a mapping module's upstream
/// snapshot. The collector's value wins over the raw device's, matching how the
/// canonical pins are snapshotted.
///
/// MIDI pins are dynamic, so a mapping module takes whatever the bus actually
/// carries rather than walking a list. Pins at rest are absent; a reader that
/// finds nothing reads rest, which is what `Signal::as_bool` / `as_float`
/// already do with a missing pin.
pub(crate) fn fill_upstream_midi(
    collector_id: &str,
    dev_id: &str,
    collector_sigs: &SigMap,
    dev_sigs: &SigMap,
    upstream: &mut PinSigs,
) {
    if !collector_id.is_empty() {
        for ((k, pin), &sig) in collector_sigs.iter() {
            if k == collector_id && midi::is_midi_pin(pin) {
                upstream.insert(flexinput_core::pooled_string(pin), sig);
            }
        }
    }
    if !dev_id.is_empty() {
        for ((d, pin), &sig) in dev_sigs.iter() {
            if d == dev_id && midi::is_midi_pin(pin) {
                upstream.entry(flexinput_core::pooled_string(pin)).or_insert(sig);
            }
        }
    }
}

/// What a mapping card is doing to one of its MIDI output pins this tick.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum CardMidi {
    /// Released: a note lifts, a value goes to the card's off level.
    Off,
    /// Held: a note sounds at the card's velocity, a value sits at its on level.
    On,
    /// An analog card's live value — a controller's position, a bend's (signed)
    /// deflection, or for a note its velocity (sounding while above zero).
    Value(f32),
}

/// Publish one MIDI output of a mapping card, the way the Remapper publishes
/// its own: a note as a gate with its velocity riding the twin pin, a value at
/// the card's on / off level (`midi_card_levels`), a program change or
/// transport as a gate — and every pin MARKED produced, so a MIDI Out sends it
/// with its Thru toggle off. Touch Zones, Lean and the Virtual Menu all publish
/// their MIDI through here, so a card means the same thing in each.
pub(crate) fn publish_card_midi(
    key: &str,
    pin_id: &str,
    state: CardMidi,
    levels: (f32, f32, f32),
    collector_sigs: &mut SigMap,
) {
    let Some(pin) = midi::parse_pin(pin_id) else { return };
    let (vel, on, off) = levels;
    let put = |id: String, sig: Signal, collector_sigs: &mut SigMap| {
        collector_sigs.set_pin(&key, &id, sig);
        mark_produced(key, &id, collector_sigs);
    };
    if let midi::MidiPin::Note { ch, note } = pin {
        let (gate, v) = match state {
            CardMidi::Off => (false, 0.0),
            CardMidi::On => (true, vel),
            // Never 0 while sounding: a note-on at velocity 0 is a note-off.
            CardMidi::Value(v) => (v > 0.0, v.clamp(1.0 / 127.0, 1.0)),
        };
        put(pin_id.to_string(), Signal::Bool(gate), collector_sigs);
        if gate {
            put(midi::MidiPin::Velocity { ch, note }.to_id(), Signal::Float(v), collector_sigs);
        }
        // Played by a value, the value keeps going after the strike as the
        // note's aftertouch — a note is a gate and a value.
        if let CardMidi::Value(live) = state {
            let at = if gate { live.clamp(0.0, 1.0) } else { 0.0 };
            put(midi::MidiPin::PolyAftertouch { ch, note }.to_id(), Signal::Float(at), collector_sigs);
        }
        return;
    }
    let sig = if pin.is_continuous() {
        let bend = matches!(pin, midi::MidiPin::PitchBend { .. });
        Signal::Float(match state {
            CardMidi::Off => off,
            CardMidi::On => on,
            CardMidi::Value(v) if bend => v.clamp(-1.0, 1.0),
            CardMidi::Value(v) => v.clamp(0.0, 1.0),
        })
    } else {
        Signal::Bool(match state {
            CardMidi::Off => false,
            CardMidi::On => true,
            CardMidi::Value(v) => v > 0.0,
        })
    };
    put(pin_id.to_string(), sig, collector_sigs);
}

/// The MIDI levels of the first card in `cards` that sends `pin_id` — what a
/// released pin goes back to when no card holds it this tick.
pub(crate) fn card_levels_for(cards: &[serde_json::Value], pin_id: &str) -> (f32, f32, f32) {
    cards
        .iter()
        .find(|c| {
            c.get("out")
                .and_then(|v| v.as_array())
                .is_some_and(|a| a.iter().any(|p| p.as_str() == Some(pin_id)))
        })
        .map(midi_card_levels)
        .unwrap_or_else(|| midi_card_levels(&serde_json::Value::Null))
}

/// Rest value for a MIDI pin id, for publishing a claimed pin as "off".
pub(crate) fn midi_rest(pin: &str) -> Option<Signal> {
    midi::parse_pin(pin).map(|p| p.rest_value())
}

/// MIDI pins a MIDI Out sink should send from its AutoMap source this tick:
/// produced pins from the source collector key, or — with Thru on — every MIDI
/// pin on the bus, raw device pins included.
pub(crate) fn midi_out_pins(
    src_key: &str,
    src_is_collector: bool,
    fallback_dev: Option<&str>,
    thru: bool,
    dev_sigs: &SigMap,
    collector_sigs: &SigMap,
) -> Vec<(String, Signal)> {
    let mut out: PinSigs = PinSigs::default();
    if src_is_collector {
        for ((k, pin), &sig) in collector_sigs.iter() {
            if k != src_key || !midi::is_midi_pin(pin) {
                continue;
            }
            let produced = collector_sigs
                .has_pin(&src_key, &format!("{MIDI_PRODUCED_PREFIX}{pin}"));
            if produced || thru {
                out.insert(pin.clone(), sig);
            }
        }
    }
    if thru {
        let raw = if src_is_collector { fallback_dev } else { Some(src_key) };
        if let Some(dev) = raw.filter(|d| is_midi_device(d)) {
            for ((d, pin), &sig) in dev_sigs.iter() {
                if d == dev && midi::is_midi_pin(pin) {
                    out.entry(pin.clone()).or_insert(sig);
                }
            }
        }
    }
    out.into_iter().collect()
}

#[cfg(test)]
mod raw_extra_tests {
    use super::*;

    fn sigs(entries: &[(&str, &str, Signal)]) -> SigMap {
        entries.iter().map(|(d, p, s)| ((d.to_string(), p.to_string()), *s)).collect()
    }

    #[test]
    fn a_raw_pads_own_ports_ride_the_bus_and_its_raw_pins_do_not() {
        let dev = sigs(&[
            ("sdl:raw:i0", "extra:b9", Signal::Bool(true)),
            ("sdl:raw:i0", "raw:b9", Signal::Bool(true)),
            ("sdl:raw:i0", "btn_south", Signal::Bool(false)),
            ("sdl:raw:i1", "extra:b2", Signal::Bool(true)),
        ]);
        let mut bus = SigMap::default();
        fill_raw_dynamic(&dev, "sdl:raw:i0", "fork:1:0", &mut bus);
        assert_eq!(bus.len(), 1, "{bus:?}");
        assert_eq!(bus.get_pin("fork:1:0", "extra:b9"), Some(&Signal::Bool(true)));

        let mut upstream = PinSigs::default();
        let mut names = Vec::new();
        fill_upstream_raw_extra("sdl:raw:i0", &dev, &mut upstream, &mut names);
        assert_eq!(names, ["extra:b9"]);

        // Any other device carries none.
        let mut bus = SigMap::default();
        fill_raw_dynamic(&sigs(&[("gilrs:generic:0", "extra:b1", Signal::Bool(true))]),
            "gilrs:generic:0", "k", &mut bus);
        assert!(bus.is_empty());
    }
}
