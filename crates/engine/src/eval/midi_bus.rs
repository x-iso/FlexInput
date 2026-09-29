//! MIDI pins on the AutoMap bus.
//!
//! MIDI pins (`midi:…`, see `flexinput_core::midi`) are dynamic: there are far
//! too many to list in `automap::ALL_PINS`, and a MIDI In port only publishes
//! the ones away from rest. So wherever the bus falls back to a RAW device for
//! canonical pins, a MIDI device's live pins are copied in as well
//! ([`fill_raw_midi`]); collector-to-collector pass-through already copies
//! every entry, MIDI included.
//!
//! **Provenance.** A MIDI Out sink must not echo raw MIDI straight back out —
//! with In and Out on the same port that is an instant feedback loop. So the
//! sink forwards a `midi:` pin only when something PRODUCED it (a mapping card,
//! a Collector input) or when the sink's Thru toggle is on. Producers mark
//! their pins with [`mark_produced`]; the marker is an ordinary bus entry, so
//! every pass-through that copies a collector's entries carries it along.

use super::*;

use flexinput_core::midi;

/// `collector_sigs` pin prefix marking a `midi:` pin as produced on that key.
pub(crate) const MIDI_PRODUCED_PREFIX: &str = "__midi_out__:";

/// A physical MIDI input port id.
pub(crate) fn is_midi_device(dev_id: &str) -> bool {
    dev_id.starts_with("midi_in:")
}

/// Copy `dev_id`'s live MIDI bus pins into `key`, never overwriting an entry
/// the key already carries (an upstream producer's value wins over raw). A
/// no-op for anything but a MIDI input port. Legacy `cc_<n>` / `pitch_bend`
/// aliases stay off the bus.
pub(crate) fn fill_raw_midi(
    dev_sigs: &HashMap<(String, String), Signal>,
    dev_id: &str,
    key: &str,
    collector_sigs: &mut HashMap<(String, String), Signal>,
) {
    if !is_midi_device(dev_id) {
        return;
    }
    for ((d, pin), &sig) in dev_sigs.iter() {
        if d != dev_id || !midi::is_midi_pin(pin) {
            continue;
        }
        collector_sigs.entry((key.to_string(), pin.clone())).or_insert(sig);
    }
}

/// Mark a `midi:` pin on `key` as produced (see the module docs). No-op for
/// other pins.
pub(crate) fn mark_produced(key: &str, pin: &str, collector_sigs: &mut HashMap<(String, String), Signal>) {
    if midi::is_midi_pin(pin) {
        collector_sigs.insert((key.to_string(), format!("{MIDI_PRODUCED_PREFIX}{pin}")), Signal::Bool(true));
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
    collector_sigs: &HashMap<(String, String), Signal>,
    dev_sigs: &HashMap<(String, String), Signal>,
    upstream: &mut HashMap<String, Signal>,
) {
    if !collector_id.is_empty() {
        for ((k, pin), &sig) in collector_sigs.iter() {
            if k == collector_id && midi::is_midi_pin(pin) {
                upstream.insert(pin.clone(), sig);
            }
        }
    }
    if !dev_id.is_empty() {
        for ((d, pin), &sig) in dev_sigs.iter() {
            if d == dev_id && midi::is_midi_pin(pin) {
                upstream.entry(pin.clone()).or_insert(sig);
            }
        }
    }
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
    dev_sigs: &HashMap<(String, String), Signal>,
    collector_sigs: &HashMap<(String, String), Signal>,
) -> Vec<(String, Signal)> {
    let mut out: HashMap<String, Signal> = HashMap::new();
    if src_is_collector {
        for ((k, pin), &sig) in collector_sigs.iter() {
            if k != src_key || !midi::is_midi_pin(pin) {
                continue;
            }
            let produced = collector_sigs
                .contains_key(&(src_key.to_string(), format!("{MIDI_PRODUCED_PREFIX}{pin}")));
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
